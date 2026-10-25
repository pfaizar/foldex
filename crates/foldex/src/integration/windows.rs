use super::{MAX_BACKUP, Registry, Value};
use std::{ffi::OsStr, os::windows::ffi::OsStrExt, ptr};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_PATH_NOT_FOUND, HANDLE,
        WAIT_ABANDONED, WAIT_OBJECT_0,
    },
    System::{
        Registry::*,
        Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
    },
    UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_FLUSHNOWAIT, SHCNF_IDLIST, SHChangeNotify},
};

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain([0]).collect()
}
fn failure(code: u32) -> String {
    format!(
        "Windows Explorer integration: {}",
        std::io::Error::from_raw_os_error(code as i32)
    )
}
struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

#[derive(Default)]
pub(super) struct UserRegistry {
    /// Tests map all operations into a separate owned subtree, never live classes.
    pub(super) prefix: String,
}
impl UserRegistry {
    fn path(&self, key: &str) -> Vec<u16> {
        wide(&if self.prefix.is_empty() {
            key.to_owned()
        } else {
            format!(r"{}\{key}", self.prefix)
        })
    }
    fn open(&self, key: &str, access: u32) -> Result<Option<Key>, String> {
        let mut handle = ptr::null_mut();
        let code = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                self.path(key).as_ptr(),
                0,
                access,
                &mut handle,
            )
        };
        match code {
            0 => Ok(Some(Key(handle))),
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => Ok(None),
            code => Err(failure(code)),
        }
    }
}
impl Registry for UserRegistry {
    fn read(&self, key: &str, name: &str) -> Result<Option<Value>, String> {
        let Some(key) = self.open(key, KEY_QUERY_VALUE)? else {
            return Ok(None);
        };
        let name = wide(name);
        let mut bytes = Vec::new();
        for _ in 0..3 {
            let mut length = bytes.len() as u32;
            let mut kind = 0;
            let code = unsafe {
                RegQueryValueExW(
                    key.0,
                    name.as_ptr(),
                    ptr::null(),
                    &mut kind,
                    if bytes.is_empty() {
                        ptr::null_mut()
                    } else {
                        bytes.as_mut_ptr()
                    },
                    &mut length,
                )
            };
            if code == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            if code != 0 && code != ERROR_MORE_DATA {
                return Err(failure(code));
            }
            if length as usize > MAX_BACKUP {
                return Err("Explorer registration value exceeds its size limit.".into());
            }
            if code == 0 && length as usize <= bytes.len() {
                bytes.truncate(length as usize);
                return Ok(Some(Value { kind, bytes }));
            }
            bytes.resize(length as usize, 0);
        }
        Err("Explorer registration changed while it was being read. Try again.".into())
    }
    fn exists(&self, key: &str) -> Result<bool, String> {
        Ok(self.open(key, KEY_READ)?.is_some())
    }
    fn write(&mut self, key: &str, name: &str, value: &Value) -> Result<(), String> {
        let mut handle = ptr::null_mut();
        let code = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                self.path(key).as_ptr(),
                0,
                ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                ptr::null(),
                &mut handle,
                ptr::null_mut(),
            )
        };
        if code != 0 {
            return Err(failure(code));
        }
        let handle = Key(handle);
        let code = unsafe {
            RegSetValueExW(
                handle.0,
                wide(name).as_ptr(),
                0,
                value.kind,
                value.bytes.as_ptr(),
                value.bytes.len() as u32,
            )
        };
        if code == 0 {
            Ok(())
        } else {
            Err(failure(code))
        }
    }
    fn remove(&mut self, key: &str, name: &str) -> Result<(), String> {
        let Some(key) = self.open(key, KEY_SET_VALUE)? else {
            return Ok(());
        };
        let code = unsafe { RegDeleteValueW(key.0, wide(name).as_ptr()) };
        if code == 0 || code == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(failure(code))
        }
    }
    fn remove_empty_key(&mut self, key: &str) -> Result<(), String> {
        let Some(handle) = self.open(key, KEY_READ)? else {
            return Ok(());
        };
        let mut children = 0;
        let mut values = 0;
        let code = unsafe {
            RegQueryInfoKeyW(
                handle.0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
                &mut children,
                ptr::null_mut(),
                ptr::null_mut(),
                &mut values,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if code != 0 {
            return Err(failure(code));
        }
        drop(handle);
        if children != 0 || values != 0 {
            return Ok(());
        }
        let code = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, self.path(key).as_ptr()) };
        if code == 0 || code == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            Err(failure(code))
        }
    }
}

pub(super) struct RegistrationLock(HANDLE);
impl RegistrationLock {
    pub(super) fn acquire() -> Result<Self, String> {
        let handle = unsafe {
            CreateMutexW(
                ptr::null(),
                0,
                wide(r"Local\Foldex.ExplorerIntegration.v1").as_ptr(),
            )
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().to_string());
        }
        match unsafe { WaitForSingleObject(handle, 5_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self(handle)),
            _ => {
                unsafe {
                    CloseHandle(handle);
                }
                Err("Explorer integration is busy. Try again.".into())
            }
        }
    }
}
impl Drop for RegistrationLock {
    fn drop(&mut self) {
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}
pub(super) fn notify() {
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST | SHCNF_FLUSHNOWAIT,
            ptr::null(),
            ptr::null(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::integration::{BACKUP_KEY, Status, WIN_E, apply, inspect, plan};
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };
    struct PrivateHive {
        registry: UserRegistry,
        files: PathBuf,
        file_base: PathBuf,
    }
    impl PrivateHive {
        fn new() -> Self {
            let suffix = format!(
                "owned-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let prefix = format!(r"Software\Foldex\IntegrationTests\{suffix}");
            let file_base =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/integration-fixtures");
            let files = file_base.join(suffix);
            std::fs::create_dir_all(&files).unwrap();
            Self {
                registry: UserRegistry { prefix },
                files,
                file_base,
            }
        }
    }
    impl Drop for PrivateHive {
        fn drop(&mut self) {
            // The test never writes live Software\Classes. Only this freshly
            // owned, uniquely named subtree and workspace fixture are removed.
            assert!(
                self.registry
                    .prefix
                    .starts_with(r"Software\Foldex\IntegrationTests\owned-")
            );
            unsafe {
                RegDeleteTreeW(HKEY_CURRENT_USER, wide(&self.registry.prefix).as_ptr());
            }
            let files = self.files.canonicalize().unwrap();
            let base = self.file_base.canonicalize().unwrap();
            assert!(files.starts_with(&base) && files != base);
            std::fs::remove_dir_all(files).unwrap();
        }
    }
    fn live_values() -> Vec<Option<Value>> {
        let r = UserRegistry::default();
        plan(std::path::Path::new("Foldex.exe"))
            .iter()
            .map(|change| r.read(&change.key, &change.name).unwrap())
            .collect()
    }
    #[test]
    fn background_status_query_does_not_change_live_associations() {
        use std::{
            sync::Arc,
            time::{Duration, Instant},
        };
        let before = live_values();
        let worker = crate::platform::Platform::new(Arc::new(|| {})).unwrap();
        worker
            .try_submit(crate::platform::Job::ExplorerIntegration {
                id: 47,
                enabled: None,
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(crate::platform::Event::ExplorerIntegration { id, result }) =
                worker.try_recv()
            {
                assert_eq!(id, 47);
                assert!(result.is_ok(), "{result:?}");
                break;
            }
            assert!(Instant::now() < deadline, "integration response timed out");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(live_values(), before);
    }

    #[test]
    fn isolated_registry_round_trip_preserves_the_existing_handler() {
        let before_live = live_values();
        let mut hive = PrivateHive::new();
        let r = &mut hive.registry;
        let folder = r"Software\Classes\Directory\shell";
        r.write(folder, "", &Value::text("Previous.Manager"))
            .unwrap();
        let delegate = Value::text("{11111111-1111-1111-1111-111111111111}");
        r.write(WIN_E, "DelegateExecute", &delegate).unwrap();
        let exe = std::env::current_exe().unwrap();
        assert!(apply(r, &exe, true).unwrap().enabled);
        assert!(inspect(r).unwrap().enabled);
        assert_eq!(apply(r, &exe, false).unwrap(), Status::default());
        assert_eq!(
            r.read(folder, "").unwrap(),
            Some(Value::text("Previous.Manager"))
        );
        assert_eq!(r.read(WIN_E, "DelegateExecute").unwrap(), Some(delegate));
        assert!(r.read(BACKUP_KEY, "RestoreV1").unwrap().is_none());
        assert_eq!(
            live_values(),
            before_live,
            "test changed a live association"
        );
    }
    #[test]
    #[ignore = "Build the shell_arg_probe example first; run this focused check explicitly."]
    fn isolated_shell_launch_preserves_drive_roots_and_unicode_arguments() {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::{
            System::{
                Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
                Threading::GetExitCodeProcess,
            },
            UI::{
                Shell::{
                    SEE_MASK_CLASSKEY, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
                    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
                },
                WindowsAndMessaging::SW_HIDE,
            },
        };
        let initialized = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        assert!(initialized >= 0);
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _apartment = Apartment;
        let before_live = live_values();
        let mut hive = PrivateHive::new();
        let probe = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/release/examples/shell_arg_probe.exe");
        assert!(
            probe.exists(),
            "build --release -p foldex --example shell_arg_probe first"
        );
        let program_dir = hive.files.join("Program Space Ω");
        std::fs::create_dir(&program_dir).unwrap();
        let program = program_dir.join("Foldex probe.exe");
        std::fs::copy(probe, &program).unwrap();
        let program = program.canonicalize().unwrap();
        let folder = hive.files.join("Folder Space 東京");
        std::fs::create_dir(&folder).unwrap();
        apply(&mut hive.registry, &program, true).unwrap();
        for (class, target) in [("Drive", PathBuf::from(r"C:\")), ("Directory", folder)] {
            let key = hive
                .registry
                .open(&format!(r"Software\Classes\{class}"), KEY_READ)
                .unwrap()
                .unwrap();
            let file: Vec<u16> = crate::integration::shell_path(&target.canonicalize().unwrap())
                .encode_wide()
                .chain([0])
                .collect();
            let verb = wide("Foldex.Open"); // Unique verb: an ignored class key fails without opening Explorer.
            let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
            info.cbSize = std::mem::size_of_val(&info) as u32;
            info.fMask = SEE_MASK_CLASSKEY
                | SEE_MASK_NOCLOSEPROCESS
                | SEE_MASK_FLAG_NO_UI
                | SEE_MASK_NOASYNC;
            info.hkeyClass = key.0;
            info.lpFile = file.as_ptr();
            info.lpVerb = verb.as_ptr();
            info.nShow = SW_HIDE;
            assert_ne!(
                unsafe { ShellExecuteExW(&mut info) },
                0,
                "class={class}, program={program:?}, command={:?}, shell_error={}, {}",
                hive.registry
                    .read(
                        &format!(r"Software\Classes\{class}\shell\Foldex.Open\command"),
                        ""
                    )
                    .unwrap(),
                info.hInstApp as isize,
                std::io::Error::last_os_error()
            );
            assert!(!info.hProcess.is_null());
            let wait = unsafe { WaitForSingleObject(info.hProcess, 5_000) };
            let mut code = 1;
            unsafe {
                GetExitCodeProcess(info.hProcess, &mut code);
                CloseHandle(info.hProcess);
            }
            assert_eq!(wait, WAIT_OBJECT_0);
            assert_eq!(code, 0);
            let output = program.with_file_name("received-arguments.json");
            let raw: Vec<Vec<u16>> =
                serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
            std::fs::remove_file(output).unwrap();
            assert_eq!(raw.len(), 2);
            assert_eq!(String::from_utf16(&raw[0]).unwrap(), "--shell-open");
            let received =
                crate::integration::folder_argument(&std::ffi::OsString::from_wide(&raw[1]));
            assert_eq!(
                received.canonicalize().unwrap(),
                target.canonicalize().unwrap()
            );
        }
        apply(&mut hive.registry, &program, false).unwrap();
        assert_eq!(live_values(), before_live);
    }
}
