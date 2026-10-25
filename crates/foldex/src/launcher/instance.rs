//! One background owner per portable installation and user. Folder activations
//! reach its bounded inbox before another GPU/window/search runtime is created.
use std::{
    io,
    path::{Path, PathBuf},
};

pub const PACKET: usize = 0x46494c31;
pub const MAX_UNITS: usize = 16_384;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Activation {
    StayInTray,
    Explorer,
    Launcher,
    Settings,
    Path(PathBuf),
}

pub fn identity(directory: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    directory.as_os_str().to_ascii_lowercase().hash(&mut hash);
    std::env::var_os("USERPROFILE").hash(&mut hash);
    format!("Foldex.Background.{:016x}", hash.finish())
}

#[cfg(windows)]
pub fn encode(activation: &Activation) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut units = vec![match activation {
        Activation::StayInTray => 0,
        Activation::Explorer => 1,
        Activation::Launcher => 2,
        Activation::Settings => 4,
        Activation::Path(_) => 3,
    }];
    if let Activation::Path(path) = activation {
        units.extend(path.as_os_str().encode_wide());
    }
    if units.len() > MAX_UNITS || units[1..].contains(&0) {
        return Err(io::Error::other(
            "The folder path is too long or contains a null character",
        ));
    }
    Ok(units)
}

#[cfg(windows)]
pub fn decode(units: &[u16]) -> Option<Activation> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    if units.is_empty() || units.len() > MAX_UNITS || units[1..].contains(&0) {
        return None;
    }
    match units[0] {
        0 if units.len() == 1 => Some(Activation::StayInTray),
        1 if units.len() == 1 => Some(Activation::Explorer),
        2 if units.len() == 1 => Some(Activation::Launcher),
        4 if units.len() == 1 => Some(Activation::Settings),
        3 if units.len() > 1 => Some(Activation::Path(PathBuf::from(OsString::from_wide(
            &units[1..],
        )))),
        _ => None,
    }
}

pub enum Claim {
    Primary(Guard),
    Forwarded,
}
#[cfg(windows)]
pub struct Guard {
    mutex: windows_sys::Win32::Foundation::HANDLE,
    owned: bool,
}
#[cfg(not(windows))]
pub struct Guard;

#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        unsafe {
            if self.owned {
                windows_sys::Win32::System::Threading::ReleaseMutex(self.mutex);
            }
            windows_sys::Win32::Foundation::CloseHandle(self.mutex);
        }
    }
}

#[cfg(windows)]
pub fn claim(channel: &str, activation: &Activation) -> io::Result<Claim> {
    use std::{
        ptr,
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::*,
        System::{DataExchange::COPYDATASTRUCT, Threading::*},
        UI::WindowsAndMessaging::*,
    };
    let name: Vec<_> = format!("Local\\{channel}")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mutex = unsafe { CreateMutexW(ptr::null(), 1, name.as_ptr()) };
    if mutex.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut guard = Guard {
        mutex,
        owned: unsafe { GetLastError() } != ERROR_ALREADY_EXISTS,
    };
    if guard.owned {
        return Ok(Claim::Primary(guard));
    }
    let class: Vec<_> = channel.encode_utf16().chain(Some(0)).collect();
    let start = Instant::now();
    let window = loop {
        // Keep the mutex handle until handoff completes. If the previous owner
        // exits while its message window is starting or closing, this launch
        // becomes the new primary instead of failing against a dead process.
        let wait = unsafe { WaitForSingleObject(guard.mutex, 0) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            guard.owned = true;
            return Ok(Claim::Primary(guard));
        }
        if wait != WAIT_TIMEOUT {
            return Err(io::Error::other(
                "Unable to inspect the running Foldex instance",
            ));
        }
        let window = unsafe { FindWindowW(class.as_ptr(), ptr::null()) };
        if !window.is_null() {
            break window;
        }
        if start.elapsed() >= Duration::from_secs(5) {
            return Err(io::Error::other(
                "The running Foldex instance is still starting. Try again shortly.",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let units = encode(activation)?;
    let mut owner = 0;
    unsafe {
        GetWindowThreadProcessId(window, &mut owner);
        AllowSetForegroundWindow(owner);
    }
    let packet = COPYDATASTRUCT {
        dwData: PACKET,
        cbData: (units.len() * 2) as u32,
        lpData: units.as_ptr().cast_mut().cast(),
    };
    let mut result = 0;
    // Bounded synchronous handoff; the receiver copies the packet before return.
    let sent = unsafe {
        SendMessageTimeoutW(
            window,
            WM_COPYDATA,
            0,
            (&packet as *const COPYDATASTRUCT) as isize,
            SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
            1500,
            &mut result,
        )
    };
    if sent == 0 || result != 1 {
        let wait = unsafe { WaitForSingleObject(guard.mutex, 0) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            guard.owned = true;
            return Ok(Claim::Primary(guard));
        }
        return Err(io::Error::other(
            "The running Foldex instance could not accept this folder yet",
        ));
    }
    Ok(Claim::Forwarded)
}
#[cfg(not(windows))]
pub fn claim(_: &str, _: &Activation) -> io::Result<Claim> {
    Ok(Claim::Primary(Guard))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn relaunch_recovers_when_previous_owner_exits_before_handoff() {
        use std::{sync::mpsc, time::Duration};
        let channel = format!(
            "Foldex.Test.Reclaim.{}.{:?}",
            std::process::id(),
            std::time::SystemTime::now()
        );
        let (ready_tx, ready_rx) = mpsc::channel();
        let owner_channel = channel.clone();
        let owner = std::thread::spawn(move || {
            let Claim::Primary(guard) = claim(&owner_channel, &Activation::StayInTray).unwrap()
            else {
                panic!("isolated test must own its unique mutex");
            };
            ready_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(60));
            drop(guard);
        });
        ready_rx.recv().unwrap();
        // There is deliberately no HWND, tray icon, global hotkey or clipboard
        // listener. Only this uniquely named test mutex can be reclaimed.
        let recovered = claim(&channel, &Activation::Launcher).unwrap();
        owner.join().unwrap();
        assert!(matches!(recovered, Claim::Primary(_)));
    }

    #[test]
    fn activation_packets_preserve_raw_paths_and_reject_bad_sizes() {
        use std::{
            ffi::OsString,
            os::windows::ffi::{OsStrExt, OsStringExt},
        };
        for path in [
            PathBuf::from("C:\\"),
            PathBuf::from("\\\\?\\C:\\Program Space Ω"),
            PathBuf::from(OsString::from_wide(&[67, 58, 92, 0xd800])),
        ] {
            let Some(Activation::Path(returned)) =
                decode(&encode(&Activation::Path(path.clone())).unwrap())
            else {
                panic!("path packet");
            };
            assert_eq!(
                returned.as_os_str().encode_wide().collect::<Vec<_>>(),
                path.as_os_str().encode_wide().collect::<Vec<_>>()
            );
        }
        for action in [
            Activation::StayInTray,
            Activation::Explorer,
            Activation::Launcher,
            Activation::Settings,
        ] {
            assert_eq!(decode(&encode(&action).unwrap()), Some(action));
        }
        for units in [
            vec![],
            vec![5],
            vec![0, 1],
            vec![3],
            vec![3, 0],
            vec![3; MAX_UNITS + 1],
        ] {
            assert!(decode(&units).is_none());
        }
    }
}
