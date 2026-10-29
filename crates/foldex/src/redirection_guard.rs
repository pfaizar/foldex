//! Redirection Guard (Windows 11 24H2+) makes a process refuse to follow symlinks
//! and junctions a non-administrator created, and every process it starts
//! inherits it. Inno Setup turns it on for itself, so the Foldex its updater
//! restarts carries it and hands it to every editor, console and terminal it
//! opens: nvm's `C:\nvm4w\nodejs`, scoop's `current` and pnpm's `node_modules`
//! junctions stop resolving there ("'node' is not recognized", "The path cannot
//! be traversed because it contains an untrusted mount point"). A process cannot
//! turn the policy off, so Foldex restarts itself once as a child of the desktop
//! shell, the parent a Start-menu launch has, and the new process starts without it.

use std::{
    ffi::c_void,
    mem::{size_of, zeroed},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::{null, null_mut},
};

use windows_sys::Win32::{
    Foundation::{FALSE, HANDLE},
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::{
        Environment::GetCommandLineW,
        Threading::{
            CREATE_NEW_CONSOLE, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess, GetProcessMitigationPolicy,
            GetStartupInfoW, InitializeProcThreadAttributeList, OpenProcess, OpenProcessToken,
            PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, PROCESS_CREATE_PROCESS, PROCESS_CREATION_FLAGS,
            PROCESS_INFORMATION, PROCESS_QUERY_INFORMATION, ProcessRedirectionTrustPolicy,
            STARTF_USESHOWWINDOW, STARTUPINFOEXW, STARTUPINFOW, UpdateProcThreadAttribute,
        },
    },
    UI::WindowsAndMessaging::{
        ASFW_ANY, AllowSetForegroundWindow, GetShellWindow, GetWindowThreadProcessId,
    },
};

/// Set on the restarted process so a restart that still inherits the policy cannot loop.
const RESTARTED: &str = "FOLDEX_REDIRECTION_GUARD_RESTART";

/// `true` when this process restarted itself without the policy and should exit
/// now. Call before Foldex claims its installation or single-instance mutexes,
/// while the process has no other threads.
pub fn restart_without_inherited_guard() -> bool {
    if std::env::var_os(RESTARTED).is_some() {
        unsafe { std::env::remove_var(RESTARTED) };
        return false;
    }
    // An elevated Foldex would lose its rights under the (unelevated) desktop shell.
    if !enforced(unsafe { GetCurrentProcess() }) || elevated() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    // The command line exactly as this process received it.
    let command_line = unsafe {
        let line = GetCommandLineW();
        let len = (0..).take_while(|&i| *line.add(i) != 0).count();
        std::slice::from_raw_parts(line, len).to_vec()
    };
    unsafe { std::env::set_var(RESTARTED, "1") };
    // The new process is not our child as far as focus rules go: let it take the foreground.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    let restarted = spawn_under_shell(&exe, command_line, 0);
    unsafe { std::env::remove_var(RESTARTED) };
    restarted.is_some()
}

fn elevated() -> bool {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        // Unknown: staying put is the safe choice.
        return true;
    }
    let _token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut written = 0u32;
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut written,
        )
    };
    ok == 0 || elevation.TokenIsElevated != 0
}

/// Whether `process` (opened with query rights) enforces Redirection Guard.
fn enforced(process: HANDLE) -> bool {
    // PROCESS_MITIGATION_REDIRECTION_TRUST_POLICY: bit 0 is EnforceRedirectionTrust.
    let mut flags = 0u32;
    let ok = unsafe {
        GetProcessMitigationPolicy(
            process,
            ProcessRedirectionTrustPolicy,
            (&raw mut flags).cast(),
            size_of::<u32>(),
        )
    };
    ok != 0 && flags & 1 != 0
}

struct Spawned {
    #[cfg_attr(not(test), allow(dead_code))]
    process: OwnedHandle,
    _thread: OwnedHandle,
}

/// Starts `exe` with the desktop shell as its parent, so it inherits the
/// shell's mitigation policies instead of ours. Keeps how this launch asked to
/// show its window (minimized, maximized, ...).
fn spawn_under_shell(
    exe: &Path,
    mut command_line: Vec<u16>,
    flags: PROCESS_CREATION_FLAGS,
) -> Option<Spawned> {
    let mut shell_pid = 0u32;
    unsafe { GetWindowThreadProcessId(GetShellWindow(), &mut shell_pid) };
    if shell_pid == 0 {
        return None;
    }
    let shell = unsafe {
        OpenProcess(
            PROCESS_CREATE_PROCESS | PROCESS_QUERY_INFORMATION,
            FALSE,
            shell_pid,
        )
    };
    if shell.is_null() {
        return None;
    }
    let shell = unsafe { OwnedHandle::from_raw_handle(shell) };
    let shell_handle: HANDLE = shell.as_raw_handle();
    if enforced(shell_handle) {
        return None;
    }

    let exe: Vec<u16> = exe.as_os_str().encode_wide().chain([0]).collect();
    // CreateProcessW may write into the command line buffer.
    command_line.push(0);
    let mut size = 0usize;
    // The first call only queries the required size.
    unsafe { InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size) };
    // usize elements keep the buffer pointer-aligned.
    let mut list = vec![0usize; size.div_ceil(size_of::<usize>()).max(1)];
    let list_ptr: *mut c_void = list.as_mut_ptr().cast();
    if unsafe { InitializeProcThreadAttributeList(list_ptr, 1, 0, &mut size) } == 0 {
        return None;
    }
    struct Attributes(*mut c_void);
    impl Drop for Attributes {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }
    let _attributes = Attributes(list_ptr);
    // `shell` stays open until CreateProcessW below has returned.
    if unsafe {
        UpdateProcThreadAttribute(
            list_ptr,
            0,
            PROC_THREAD_ATTRIBUTE_PARENT_PROCESS as usize,
            (&raw const shell_handle).cast(),
            size_of::<HANDLE>(),
            null_mut(),
            null(),
        )
    } == 0
    {
        return None;
    }

    let mut ours: STARTUPINFOW = unsafe { zeroed() };
    unsafe { GetStartupInfoW(&mut ours) };
    let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = ours.dwFlags & STARTF_USESHOWWINDOW;
    startup.StartupInfo.wShowWindow = ours.wShowWindow;
    startup.lpAttributeList = list_ptr;

    let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
    let ok = unsafe {
        CreateProcessW(
            exe.as_ptr(),
            command_line.as_mut_ptr(),
            null(),
            null(),
            FALSE,
            // A console program (a debug build) under the shell cannot reuse our
            // console and fails to load without its own; GUI programs ignore it.
            CREATE_NEW_CONSOLE | EXTENDED_STARTUPINFO_PRESENT | flags,
            null(),
            null(),
            &startup.StartupInfo,
            &mut info,
        )
    };
    if ok == 0 {
        return None;
    }
    Some(unsafe {
        Spawned {
            process: OwnedHandle::from_raw_handle(info.hProcess),
            _thread: OwnedHandle::from_raw_handle(info.hThread),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, SetProcessMitigationPolicy, TerminateProcess,
    };

    const INSIDE: &str = "FOLDEX_REDIRECTION_GUARD_TEST";

    #[test]
    fn a_normal_process_does_not_restart() {
        if enforced(unsafe { GetCurrentProcess() }) {
            eprintln!("SKIP: this test process runs with Redirection Guard");
            return;
        }
        assert!(!restart_without_inherited_guard());
    }

    /// Turning the policy on cannot be undone, so it runs in its own copy of
    /// the test binary.
    #[test]
    fn a_guarded_process_starts_clean_processes_under_the_shell() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "redirection_guard::tests::inside_a_guarded_process",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env(INSIDE, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    #[test]
    #[ignore = "run by a_guarded_process_starts_clean_processes_under_the_shell"]
    fn inside_a_guarded_process() {
        if std::env::var_os(INSIDE).is_none() {
            return;
        }
        let policy = 1u32; // EnforceRedirectionTrust
        let turned_on = unsafe {
            SetProcessMitigationPolicy(
                ProcessRedirectionTrustPolicy,
                (&raw const policy).cast(),
                size_of::<u32>(),
            )
        } != 0;
        if !turned_on || unsafe { GetShellWindow() }.is_null() {
            eprintln!("SKIP: no Redirection Guard or no desktop shell here");
            return;
        }
        assert!(enforced(unsafe { GetCurrentProcess() }));
        let exe = std::env::current_exe().unwrap();
        let line: Vec<u16> = format!("\"{}\" --list", exe.display())
            .encode_utf16()
            .collect();
        let child = spawn_under_shell(&exe, line, CREATE_SUSPENDED)
            .expect("the desktop shell can parent a new process");
        let clean = !enforced(child.process.as_raw_handle());
        unsafe { TerminateProcess(child.process.as_raw_handle(), 0) };
        assert!(clean, "the child inherited Redirection Guard");
    }
}
