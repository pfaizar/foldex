//! Windows Folder Options is an explicit job, never a startup/settings probe.
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

pub(super) fn open() -> io::Result<()> {
    dispatch_with(|mut command| command.spawn().map(|_| ()))
}

fn dispatch_with(launch: impl FnOnce(Command) -> io::Result<()>) -> io::Result<()> {
    let system = system_directory()?;
    launch(command_for(&system))
}

fn command_for(system: &Path) -> Command {
    // Microsoft documents page 0 as General; the same dialog provides View and
    // Search. Fixed arguments bypass file associations, shell verbs and PATH.
    // https://learn.microsoft.com/windows/win32/shell/executing-control-panel-items
    let mut command = Command::new(system.join("rundll32.exe"));
    command
        .args(["shell32.dll,Options_RunDLL", "0"])
        .current_dir(system);
    // Both the executable directory and working directory are the system
    // directory, so the documented shell32 entry cannot resolve from a pane.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(windows)]
fn system_directory() -> io::Result<PathBuf> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    // Do not trust a pane path, PATH or environment overrides for an OS tool.
    let mut buffer = vec![0_u16; 260];
    loop {
        let length =
            unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < buffer.len() {
            let path = PathBuf::from(OsString::from_wide(&buffer[..length]));
            return if path.is_absolute() {
                Ok(path)
            } else {
                Err(io::Error::other(
                    "Windows returned an invalid system directory",
                ))
            };
        }
        if length > 32_768 {
            return Err(io::Error::other("Windows system directory is too long"));
        }
        // An insufficient buffer returns the required size including its NUL.
        buffer.resize(length, 0);
    }
}

#[cfg(not(windows))]
fn system_directory() -> io::Result<PathBuf> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Folder Options is available on Windows",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_folder_options_command(command: &Command, system: &Path) {
        assert_eq!(
            command.get_program(),
            system.join("rundll32.exe").as_os_str()
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                std::ffi::OsStr::new("shell32.dll,Options_RunDLL"),
                std::ffi::OsStr::new("0")
            ]
        );
        assert_eq!(command.get_current_dir(), Some(system));
    }

    #[test]
    fn folder_options_command_uses_only_the_documented_dialog_and_general_page() {
        // Command preparation never starts rundll32 or loads a shell handler.
        for name in ["system-directory", "system directory with spaces"] {
            let system = Path::new(name);
            assert_folder_options_command(&command_for(system), system);
        }
    }

    #[cfg(windows)]
    #[test]
    fn explicit_folder_options_dispatch_reaches_only_the_injected_launcher() {
        let mut calls = 0;
        dispatch_with(|command| {
            calls += 1;
            let system = system_directory()?;
            assert!(system.is_absolute());
            assert_folder_options_command(&command, &system);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls, 1);
    }

    #[cfg(windows)]
    #[test]
    fn folder_options_dispatch_propagates_launch_failure_without_a_fallback() {
        let error =
            dispatch_with(|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "blocked")))
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[cfg(not(windows))]
    #[test]
    fn unsupported_folder_options_never_calls_the_launcher() {
        let error = dispatch_with(|_| panic!("unsupported dialog was dispatched")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
    }
}
