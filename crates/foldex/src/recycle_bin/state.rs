//! Whether the current user's Recycle Bin holds anything, for its Places icon.
//!
//! A sleeping thread looks for one entry per fixed drive, and only after a bin
//! folder changed or a drive appeared. The frame loop reads a single atomic.
use std::sync::{
    OnceLock,
    atomic::{AtomicU8, Ordering},
};

const UNKNOWN: u8 = 0;
const EMPTY: u8 = 1;
const FULL: u8 = 2;
static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);
static WATCH: OnceLock<()> = OnceLock::new();

/// An unknown bin keeps the empty icon the place has always used.
pub(crate) fn holds_items() -> bool {
    STATE.load(Ordering::Relaxed) == FULL
}

/// Record a fresh answer, from the watcher or a complete listing of the bin.
/// Returns whether the icon changes.
pub(crate) fn observe(empty: bool) -> bool {
    let state = if empty { EMPTY } else { FULL };
    STATE.swap(state, Ordering::Relaxed) != state
}

/// Start following the bin once; later calls cost one atomic load.
pub(crate) fn watch(ctx: &egui::Context) {
    WATCH.get_or_init(|| {
        #[cfg(windows)]
        {
            let ctx = ctx.clone();
            let _ = std::thread::Builder::new()
                .name("foldex-recycle-state".into())
                .spawn(move || {
                    native::run(|empty| {
                        if observe(empty) {
                            ctx.request_repaint();
                        }
                    })
                });
        }
        #[cfg(not(windows))]
        let _ = ctx;
    });
}

#[cfg(windows)]
mod native {
    use std::{os::windows::ffi::OsStrExt, path::Path, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT},
        Security::{
            GetSidIdentifierAuthority, GetSidSubAuthority, GetSidSubAuthorityCount,
            GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{
            FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME, FindClose,
            FindCloseChangeNotification, FindExInfoBasic, FindExSearchNameMatch,
            FindFirstChangeNotificationW, FindFirstFileExW, FindNextFileW, GetDriveTypeW,
            GetFileAttributesW, GetLogicalDrives, INVALID_FILE_ATTRIBUTES, WIN32_FIND_DATAW,
        },
        System::Threading::{GetCurrentProcess, OpenProcessToken, Sleep, WaitForMultipleObjects},
    };

    /// `GetDriveTypeW` for a local disk, the only kind with a Recycle Bin.
    const DRIVE_FIXED: u32 = 3;

    /// New drives are noticed within this time. Nothing is read from disk
    /// unless the set of drives changed, so idle disks may keep sleeping.
    const DRIVE_CHECK_MS: u32 = 60_000;
    /// Bounds a probe through a bin of orphaned contents.
    const MAX_ORPHANS: usize = 4096;
    /// One probe follows a burst, such as emptying a large bin.
    const SETTLE_MS: u32 = 250;

    pub(super) fn run(report: impl Fn(bool)) {
        let Some(sid) = user_sid() else {
            return;
        };
        let mut drives = 0;
        let mut changed = true;
        loop {
            let present = fixed_drives();
            changed |= present != drives;
            drives = present;
            // Arm before probing, so a change during the probe still wakes us.
            let watches = Watches::arm(drives, &sid);
            if changed {
                report(!holds_items(drives, &sid));
            }
            changed = watches.wait();
            drop(watches);
            if changed {
                unsafe { Sleep(SETTLE_MS) };
            }
        }
    }

    /// The shell lists an item only when both its `$R` content and `$I`
    /// record exist; bins often keep orphans of either half. The first
    /// complete pair settles the answer.
    fn holds_items(drives: u32, sid: &str) -> bool {
        letters(drives).any(|letter| {
            let bin = format!("{letter}:\\$Recycle.Bin\\{sid}\\");
            let pattern = wide(&format!("{bin}$R*"));
            let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
            let handle = unsafe {
                FindFirstFileExW(
                    pattern.as_ptr(),
                    FindExInfoBasic,
                    (&raw mut data).cast(),
                    FindExSearchNameMatch,
                    ptr::null(),
                    0,
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut found = false;
            for _ in 0..MAX_ORPHANS {
                let length = data.cFileName.iter().position(|unit| *unit == 0);
                let name = &data.cFileName[..length.unwrap_or(data.cFileName.len())];
                if let Some(suffix) = name.get(2..) {
                    let record: Vec<u16> = bin
                        .encode_utf16()
                        .chain("$I".encode_utf16())
                        .chain(suffix.iter().copied())
                        .chain(Some(0))
                        .collect();
                    if unsafe { GetFileAttributesW(record.as_ptr()) } != INVALID_FILE_ATTRIBUTES {
                        found = true;
                        break;
                    }
                }
                if unsafe { FindNextFileW(handle, &mut data) } == 0 {
                    break;
                }
            }
            unsafe { FindClose(handle) };
            found
        })
    }

    struct Watches(Vec<HANDLE>);
    impl Watches {
        fn arm(drives: u32, sid: &str) -> Self {
            let handles = letters(drives)
                .filter_map(|letter| {
                    let root = format!("{letter}:\\$Recycle.Bin");
                    let user = format!("{root}\\{sid}");
                    // Until this drive's first deletion only the shared root exists.
                    let (path, subtree) = if Path::new(&user).is_dir() {
                        (user, false)
                    } else {
                        (root, true)
                    };
                    let path = wide(&path);
                    let handle = unsafe {
                        FindFirstChangeNotificationW(
                            path.as_ptr(),
                            i32::from(subtree),
                            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
                        )
                    };
                    (handle != INVALID_HANDLE_VALUE).then_some(handle)
                })
                .take(64)
                .collect();
            Self(handles)
        }
        /// Whether a bin changed, rather than the drive check coming due.
        fn wait(&self) -> bool {
            if self.0.is_empty() {
                unsafe { Sleep(DRIVE_CHECK_MS) };
                return false;
            }
            let result = unsafe {
                WaitForMultipleObjects(self.0.len() as u32, self.0.as_ptr(), 0, DRIVE_CHECK_MS)
            };
            result != WAIT_TIMEOUT && result < WAIT_OBJECT_0 + self.0.len() as u32
        }
    }
    impl Drop for Watches {
        fn drop(&mut self) {
            for handle in &self.0 {
                unsafe { FindCloseChangeNotification(*handle) };
            }
        }
    }

    fn fixed_drives() -> u32 {
        let mask = unsafe { GetLogicalDrives() };
        (0..26)
            .filter(|bit| mask & (1 << bit) != 0)
            .filter(|bit| {
                let root = wide(&format!("{}:\\", char::from(b'A' + *bit as u8)));
                unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED }
            })
            .fold(0, |fixed, bit| fixed | 1 << bit)
    }

    fn letters(drives: u32) -> impl Iterator<Item = char> {
        (0..26)
            .filter(move |bit| drives & (1 << bit) != 0)
            .map(|bit| char::from(b'A' + bit as u8))
    }

    /// The bin folder of each drive is named after the user's SID string.
    fn user_sid() -> Option<String> {
        let mut token: HANDLE = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return None;
        }
        let mut buffer = [0_u64; 64];
        let mut length = 0;
        let read = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(&buffer) as u32,
                &mut length,
            )
        };
        unsafe { CloseHandle(token) };
        if read == 0 {
            return None;
        }
        let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        let authority = unsafe { (*GetSidIdentifierAuthority(sid)).Value };
        let authority = authority
            .iter()
            .fold(0_u64, |value, byte| value << 8 | u64::from(*byte));
        let mut text = format!("S-1-{authority}");
        for index in 0..unsafe { *GetSidSubAuthorityCount(sid) } {
            text.push_str(&format!("-{}", unsafe {
                *GetSidSubAuthority(sid, u32::from(index))
            }));
        }
        Some(text)
    }

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(Some(0))
            .collect()
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn the_user_sid_has_the_bin_folder_format() {
            let sid = super::user_sid().unwrap();
            assert!(sid.starts_with("S-1-5-"), "{sid}");
            assert!(
                sid.split('-')
                    .skip(2)
                    .all(|part| part.parse::<u64>().is_ok())
            );
        }

        #[test]
        fn the_probe_agrees_with_the_shell_count() {
            use windows_sys::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
            let mut info = SHQUERYRBINFO {
                cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32,
                ..Default::default()
            };
            assert!(unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) } >= 0);
            let sid = super::user_sid().unwrap();
            assert_eq!(
                super::holds_items(super::fixed_drives(), &sid),
                info.i64NumItems > 0
            );
        }
    }
}
