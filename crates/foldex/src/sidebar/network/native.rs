//! Native WNet calls are isolated from Foldex's file/asset workers. WNet exposes
//! no per-call deadline: the disposable helper is killed on timeout or collapse,
//! rather than retaining an uninterruptible provider call in the app process.
//!
//! Windows contracts used here:
//! - https://learn.microsoft.com/windows/win32/api/winnetwk/nf-winnetwk-wnetopenenumw
//! - https://learn.microsoft.com/windows/win32/api/winnetwk/nf-winnetwk-wnetenumresourcew
//! - https://learn.microsoft.com/windows/win32/api/winnetwk/ns-winnetwk-connectdlgstructw
//! - https://learn.microsoft.com/windows/win32/api/shlobj_core/nf-shlobj_core-shgetknownfolderidlist
//! - https://learn.microsoft.com/windows/win32/api/shellapi/ns-shellapi-shellexecuteinfow
use super::*;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    mem,
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle, process::CommandExt},
    process::{Child, Command, Stdio},
    ptr,
    time::{SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_CANCELLED, ERROR_EXTENDED_ERROR, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS,
        ERROR_NO_NETWORK, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    NetworkManagement::WNet::*,
    Storage::FileSystem::{
        FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    },
    System::{
        Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode},
        Threading::{CREATE_NO_WINDOW, CreateEventW, SetEvent, WaitForMultipleObjects},
    },
    UI::{
        Shell::{
            FOLDERID_ComputerFolder, FOLDERID_NetworkFolder, ILFree, KF_FLAG_DONT_VERIFY,
            SEE_MASK_FLAG_NO_UI, SEE_MASK_IDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
            SHGetKnownFolderIDList, ShellExecuteExW,
        },
        WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow, SW_SHOWNORMAL},
    },
};

const DISCOVERY_TIMEOUT_MS: u32 = 8_000;
const MAX_ENUM_BUFFER: usize = 128 * 1024;
const MAX_ENUM_BATCHES: usize = 16;

pub(super) fn cancel_event() -> io::Result<usize> {
    // Unnamed, manual-reset and non-inheritable. The worker holds an Arc until
    // the process wait is finished, so the waited handle cannot close early.
    let event = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
    if event.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(event as usize)
    }
}
pub(super) fn signal_cancel(event: usize) {
    unsafe {
        SetEvent(event as HANDLE);
    }
}
pub(super) fn close_cancel(event: usize) {
    unsafe {
        CloseHandle(event as HANDLE);
    }
}

struct Helper(Child);
impl Drop for Helper {
    fn drop(&mut self) {
        // Child::drop does not terminate it. This also covers read/wait errors
        // and cancellation racing the spawn. No join ever happens on the UI.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn reply_file() -> io::Result<File> {
    // A delete-on-close file avoids a pipe-reader thread and pipe-capacity
    // deadlock while waiting for a provider. No path is passed to the helper.
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..4 {
        let path = std::env::temp_dir().join(format!(
            "foldex-network-{}-{stamp}-{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE)
            .open(path)
        {
            Ok(file) => return Ok(file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not create network reply mailbox",
    ))
}

pub(super) fn discover(cancel: &Cancellation) -> Result<Snapshot, String> {
    let operation = || -> Result<Snapshot, String> {
        if cancel.is_cancelled() {
            return Err("Network request cancelled".into());
        }
        let mut output = reply_file().map_err(|error| format!("Network mailbox: {error}"))?;
        let mut helper = Helper(
            Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
                .arg(HELPER_ARGUMENT)
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::from(
                    output.try_clone().map_err(|error| error.to_string())?,
                ))
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| format!("Could not start network discovery: {error}"))?,
        );
        let handles = [helper.0.as_raw_handle() as HANDLE, cancel.event as HANDLE];
        let waited =
            unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, DISCOVERY_TIMEOUT_MS) };
        if waited != WAIT_OBJECT_0 {
            let _ = helper.0.kill();
            let _ = helper.0.wait();
        }
        if cancel.is_cancelled() {
            return Err("Network request cancelled".into());
        }
        if waited != WAIT_OBJECT_0 && waited != WAIT_TIMEOUT {
            return Err(format!(
                "Could not wait for network discovery: {}",
                io::Error::last_os_error()
            ));
        }
        let failed = waited == WAIT_OBJECT_0
            && !helper
                .0
                .wait()
                .map_err(|error| format!("Network helper: {error}"))?
                .success();
        output
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        output
            .take(MAX_REPLY_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        let decoded = decode_reply(&bytes);
        if waited == WAIT_TIMEOUT {
            return decoded.map(|mut snapshot| {
                snapshot.warning = Some("A network provider did not respond within 8 seconds. Showing the connections already returned. Retry or browse Windows Network.".into());
                snapshot
            }).map_err(|_| "Network discovery timed out after 8 seconds. Retry or browse Windows Network. Local folders remain available.".into());
        }
        if failed {
            return decoded.map(|mut snapshot| {
                snapshot.warning = Some("Network discovery stopped before finishing. Showing cached results; retry or browse Windows Network.".into());
                snapshot
            }).map_err(|_| "Network discovery stopped before finishing. Retry or browse Windows Network.".into());
        }
        decoded
    };
    operation()
}

struct Enumeration(HANDLE);
impl Drop for Enumeration {
    fn drop(&mut self) {
        unsafe {
            WNetCloseEnum(self.0);
        }
    }
}

/// Provider strings must belong to the enumeration buffer. Validate pointer,
/// alignment, terminator and length before copying; keep all allocations bounded.
fn resource_string(pointer: *const u16, buffer: &[usize]) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let start = buffer.as_ptr() as usize;
    let end = start.checked_add(mem::size_of_val(buffer))?;
    let address = pointer as usize;
    if address < start || address >= end || address % mem::align_of::<u16>() != 0 {
        return None;
    }
    let available = ((end - address) / mem::size_of::<u16>()).min(MAX_NAME_UNITS + 1);
    let text = unsafe { std::slice::from_raw_parts(pointer, available) };
    let length = text.iter().position(|unit| *unit == 0)?;
    String::from_utf16(&text[..length]).ok()
}

fn enumerate(scope: u32, snapshot: &mut Snapshot) -> Result<(), String> {
    let mut handle = ptr::null_mut();
    let result = unsafe { WNetOpenEnumW(scope, RESOURCETYPE_DISK, 0, ptr::null(), &mut handle) };
    if result != 0 {
        return Err(network_error(result));
    }
    let enumeration = Enumeration(handle);
    let mut buffer = vec![0usize; (16 * 1024) / mem::size_of::<usize>()];
    for _ in 0..MAX_ENUM_BATCHES {
        let mut count = u32::MAX;
        let mut length = mem::size_of_val(buffer.as_slice()) as u32;
        let result = unsafe {
            WNetEnumResourceW(
                enumeration.0,
                &mut count,
                buffer.as_mut_ptr().cast(),
                &mut length,
            )
        };
        if result == ERROR_NO_MORE_ITEMS {
            return Ok(());
        }
        if result == ERROR_MORE_DATA {
            let requested = length as usize;
            if requested <= mem::size_of_val(buffer.as_slice()) || requested > MAX_ENUM_BUFFER {
                snapshot.truncated = true;
                return Ok(());
            }
            buffer.resize(requested.div_ceil(mem::size_of::<usize>()), 0);
            continue;
        }
        if result != 0 {
            return Err(network_error(result));
        }
        if count == 0 {
            return Ok(());
        }
        if count as usize > mem::size_of_val(buffer.as_slice()) / mem::size_of::<NETRESOURCEW>() {
            return Err("A network provider returned an invalid connection list".into());
        }
        let resources = unsafe {
            std::slice::from_raw_parts(buffer.as_ptr().cast::<NETRESOURCEW>(), count as usize)
        };
        for resource in resources {
            if resource.dwType != RESOURCETYPE_DISK {
                continue;
            }
            let Some(remote) = resource_string(resource.lpRemoteName, &buffer) else {
                continue;
            };
            let local = resource_string(resource.lpLocalName, &buffer);
            if let Some(resource) =
                Resource::from_names(&remote, local.as_deref(), scope == RESOURCE_CONNECTED)
            {
                snapshot.insert(resource);
            }
        }
        if snapshot.resources.len() >= MAX_RESOURCES {
            snapshot.truncated = true;
            return Ok(());
        }
    }
    snapshot.truncated = true;
    Ok(())
}

pub(super) fn helper_main() {
    unsafe {
        SetThreadErrorMode(SEM_FAILCRITICALERRORS, ptr::null_mut());
    }
    let mut snapshot = Snapshot::default();
    let mut errors = Vec::new();
    let mut succeeded = false;
    let mut output = io::stdout().lock();
    // Connected first. Neither scope walks providers/servers or opens shares.
    // A second complete line replaces the first if remembered enumeration ends.
    for scope in [RESOURCE_CONNECTED, RESOURCE_REMEMBERED] {
        match enumerate(scope, &mut snapshot) {
            Ok(()) => succeeded = true,
            Err(error) => errors.push(error),
        }
        snapshot.warning = (!errors.is_empty()).then(|| errors.join("\n"));
        let result = if succeeded || !snapshot.resources.is_empty() {
            Ok(snapshot.clone().normalize())
        } else {
            Err(errors.join("\n"))
        };
        if serde_json::to_writer(&mut output, &result).is_err()
            || output.write_all(b"\n").is_err()
            || output.flush().is_err()
        {
            return;
        }
    }
}

fn network_error(code: u32) -> String {
    if code == ERROR_NO_NETWORK {
        return "Windows reports that the network is unavailable. Check the connection and retry."
            .into();
    }
    if code == ERROR_EXTENDED_ERROR {
        let mut provider_code = 0;
        let mut message = [0u16; 512];
        let mut provider = [0u16; 256];
        if unsafe {
            WNetGetLastErrorW(
                &mut provider_code,
                message.as_mut_ptr(),
                message.len() as u32,
                provider.as_mut_ptr(),
                provider.len() as u32,
            )
        } == 0
        {
            let length = message
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(message.len());
            if length > 0 {
                return format!(
                    "{} (network provider code {provider_code})",
                    String::from_utf16_lossy(&message[..length])
                );
            }
        }
    }
    format!(
        "Windows network error {code}: {}",
        io::Error::from_raw_os_error(code as i32)
    )
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, String> {
        let result = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if result < 0 {
            Err(format!(
                "Windows network controls could not initialize (0x{:08X})",
                result as u32
            ))
        } else {
            Ok(Self)
        }
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub(super) fn action(action: UserAction, owner: isize) -> Result<bool, String> {
    let _apartment = Apartment::new()?;
    let owner = owner as windows_sys::Win32::Foundation::HWND;
    let mut process = 0;
    if owner.is_null() || unsafe { IsWindow(owner) } == 0 {
        return Err("The Foldex window is no longer available".into());
    }
    unsafe {
        GetWindowThreadProcessId(owner, &mut process);
    }
    if process != std::process::id() {
        return Err("The Foldex window is no longer available".into());
    }
    if action == UserAction::Connect {
        let mut resource = NETRESOURCEW {
            dwType: RESOURCETYPE_DISK,
            ..Default::default()
        };
        let mut dialog = CONNECTDLGSTRUCTW {
            cbStructure: mem::size_of::<CONNECTDLGSTRUCTW>() as u32,
            hwndOwner: owner,
            lpConnRes: &mut resource,
            dwFlags: CONNDLG_USE_MRU,
            ..Default::default()
        };
        let result = unsafe { WNetConnectionDialog1W(&mut dialog) };
        return match result {
            0 => Ok(true),
            u32::MAX | ERROR_CANCELLED => Ok(false),
            code => Err(network_error(code)),
        };
    }
    // Fixed Known Folder identities, never a user-controlled command or URL.
    // Virtual Network/This PC folders cannot go to the filesystem browser.
    let folder = match action {
        UserAction::Browse => FOLDERID_NetworkFolder,
        UserAction::Manage => FOLDERID_ComputerFolder,
        _ => return Ok(false),
    };
    let mut identifier = ptr::null_mut();
    let result = unsafe {
        SHGetKnownFolderIDList(
            &folder,
            KF_FLAG_DONT_VERIFY as u32,
            ptr::null_mut(),
            &mut identifier,
        )
    };
    if result < 0 || identifier.is_null() {
        if !identifier.is_null() {
            unsafe {
                ILFree(identifier);
            }
        }
        return Err(format!(
            "Windows could not locate its network controls (0x{:08X}). You can still enter a UNC share in the address bar.",
            result as u32
        ));
    }
    let verb: Vec<_> = "open".encode_utf16().chain([0]).collect();
    let mut execute = SHELLEXECUTEINFOW {
        cbSize: mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_IDLIST | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        hwnd: owner,
        lpVerb: verb.as_ptr(),
        lpIDList: identifier.cast(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    let success = unsafe { ShellExecuteExW(&mut execute) } != 0;
    let error = (!success).then(io::Error::last_os_error);
    unsafe {
        ILFree(identifier);
    }
    if success {
        Ok(false)
    } else {
        Err(format!(
            "Windows could not open network controls: {}. You can still enter a UNC share in the address bar.",
            error.unwrap()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_strings_are_copied_only_inside_the_bounded_aligned_enum_buffer() {
        let mut buffer = vec![0usize; 32];
        let pointer = buffer.as_mut_ptr().cast::<u16>();
        for (index, unit) in "mock share".encode_utf16().chain([0]).enumerate() {
            unsafe {
                pointer.add(index).write(unit);
            }
        }
        assert_eq!(
            resource_string(pointer, &buffer).as_deref(),
            Some("mock share")
        );
        assert_eq!(resource_string(ptr::null(), &buffer), None);
        assert_eq!(
            resource_string((pointer as usize + 1) as *const u16, &buffer),
            None
        );
        assert_eq!(
            resource_string(buffer.as_ptr().wrapping_add(buffer.len()).cast(), &buffer),
            None
        );
        let unterminated = vec![usize::MAX; MAX_NAME_UNITS];
        assert_eq!(
            resource_string(unterminated.as_ptr().cast(), &unterminated),
            None
        );
    }
}
