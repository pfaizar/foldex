//! Installed and portable copies have deliberately separate storage lifetimes.
use std::{
    io,
    path::{Path, PathBuf},
};

pub fn managed(directory: &Path) -> bool {
    directory.join("foldex-installed.ini").is_file()
}

pub fn state_directory(directory: &Path) -> io::Result<PathBuf> {
    if !managed(directory) {
        return Ok(directory.to_owned());
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            io::Error::other("Windows has not provided a local application data directory")
        })?;
    let state = local.join("Foldex").join("data");
    std::fs::create_dir_all(&state)?;
    Ok(state)
}

#[cfg(windows)]
pub struct RunningGuard(windows_sys::Win32::Foundation::HANDLE);
#[cfg(not(windows))]
pub struct RunningGuard;

/// Every browser process, including auxiliary windows and tray owners, keeps
/// the same per-installation mutex until exit. New launches wait out Setup.
#[cfg(windows)]
pub fn claim(directory: &Path) -> io::Result<RunningGuard> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, GetLastError},
        System::Threading::{
            CreateMutexW, OpenMutexW, ReleaseMutex, SYNCHRONIZATION_SYNCHRONIZE,
            WaitForSingleObject,
        },
    };
    let setup: Vec<u16> = "Global\\Foldex.Setup".encode_utf16().chain([0]).collect();
    let start = std::time::Instant::now();
    loop {
        let mutex = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, setup.as_ptr()) };
        if mutex.is_null() && unsafe { GetLastError() } == ERROR_FILE_NOT_FOUND {
            break;
        }
        if !mutex.is_null() {
            unsafe {
                CloseHandle(mutex);
            }
        }
        if start.elapsed() > std::time::Duration::from_secs(60) {
            return Err(io::Error::other(
                "Foldex Setup is still running. Finish or close it before opening Foldex.",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let transaction_name: Vec<u16> = "Global\\Foldex.InstallTransaction"
        .encode_utf16()
        .chain([0])
        .collect();
    let transaction = unsafe { CreateMutexW(std::ptr::null(), 0, transaction_name.as_ptr()) };
    if transaction.is_null() {
        return Err(io::Error::last_os_error());
    }
    let wait = unsafe { WaitForSingleObject(transaction, 60000) };
    if wait != 0 && wait != 0x80 {
        unsafe {
            CloseHandle(transaction);
        }
        return Err(io::Error::other(
            "Foldex Setup is changing files. Finish or close it before opening Foldex.",
        ));
    }
    let name: Vec<u16> = foldex_update::running_mutex(directory)
        .encode_utf16()
        .chain([0])
        .collect();
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    let error = io::Error::last_os_error();
    unsafe {
        ReleaseMutex(transaction);
        CloseHandle(transaction);
    }
    if mutex.is_null() {
        return Err(error);
    }
    Ok(RunningGuard(mutex))
}
#[cfg(not(windows))]
pub fn claim(_: &Path) -> io::Result<RunningGuard> {
    Ok(RunningGuard)
}
#[cfg(windows)]
impl Drop for RunningGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

pub fn show_error(message: &str) {
    #[cfg(windows)]
    {
        let message: Vec<u16> = message.encode_utf16().chain([0]).collect();
        let title: Vec<u16> = "Foldex".encode_utf16().chain([0]).collect();
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                message.as_ptr(),
                title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                    | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!("Foldex: {message}");
}

#[cfg(windows)]
pub fn ensure_only_process() -> Result<(), String> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::{
            ProcessStatus::K32EnumProcesses,
            Threading::{
                OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
    };
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut processes = vec![0_u32; 4096];
    let mut bytes = 0;
    if unsafe {
        K32EnumProcesses(
            processes.as_mut_ptr(),
            (processes.len() * 4) as u32,
            &mut bytes,
        )
    } == 0
        || bytes as usize >= processes.len() * 4
    {
        return Err("Could not check for other Foldex windows. Close them and try again.".into());
    }
    for pid in processes
        .into_iter()
        .take(bytes as usize / 4)
        .filter(|pid| *pid != std::process::id() && *pid != 0)
    {
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            continue;
        }
        let mut path = vec![0; 32768];
        let mut length = path.len() as u32;
        let success =
            unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) };
        unsafe {
            CloseHandle(process);
        }
        if success != 0 {
            let other = PathBuf::from(std::ffi::OsString::from_wide(&path[..length as usize]));
            if other.as_os_str().eq_ignore_ascii_case(current.as_os_str()) {
                return Err("Close the other Foldex windows before installing this update.".into());
            }
        }
    }
    Ok(())
}
#[cfg(not(windows))]
pub fn ensure_only_process() -> Result<(), String> {
    Ok(())
}
