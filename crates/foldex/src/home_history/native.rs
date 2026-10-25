//! Read existing Recent shortcuts without Resolve, recursive search, UI or writes.
use super::*;

#[derive(Default)]
pub(super) struct Validator {
    #[cfg(windows)]
    drives: [Option<bool>; 26],
}

impl Validator {
    #[cfg(not(windows))]
    pub(super) fn can_validate(&mut self, path: &Path) -> bool {
        path.is_absolute()
    }

    #[cfg(windows)]
    pub(super) fn can_validate(&mut self, path: &Path) -> bool {
        use std::os::windows::ffi::OsStrExt;
        // One drive-type sample per letter in a validation/import batch. No
        // network, device or removable path is automatically probed for metadata.
        let mut units = [0; 7];
        let mut length = 0;
        for (slot, unit) in units.iter_mut().zip(path.as_os_str().encode_wide()) {
            *slot = unit;
            length += 1;
        }
        let Some(root) = local_drive_root(&units[..length]) else {
            return false;
        };
        let index = (root[0] as u8).to_ascii_uppercase() as usize - b'A' as usize;
        *self.drives[index].get_or_insert_with(|| {
            matches!(
                unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) },
                3 | 6
            )
        })
    }
}

#[cfg(not(windows))]
pub(super) fn recent(_cancelled: &impl Fn() -> bool, _validator: &mut Validator) -> Vec<Entry> {
    vec![]
}

#[cfg(any(windows, test))]
fn local_drive_root(units: &[u16]) -> Option<[u16; 4]> {
    let units = units.strip_prefix(&[92, 92, 63, 92]).unwrap_or(units);
    if units.len() < 3
        || units[1] != b':' as u16
        || !matches!(units[2], 47 | 92)
        || !((b'A' as u16..=b'Z' as u16).contains(&units[0])
            || (b'a' as u16..=b'z' as u16).contains(&units[0]))
    {
        return None;
    }
    Some([units[0], b':' as u16, b'\\' as u16, 0])
}

#[cfg(windows)]
pub(super) fn recent(cancelled: &impl Fn() -> bool, validator: &mut Validator) -> Vec<Entry> {
    use std::{
        ffi::{OsString, c_void},
        os::windows::ffi::{OsStrExt, OsStringExt},
        ptr,
    };
    use windows_sys::{
        Win32::{
            System::Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{FOLDERID_Recent, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath},
        },
        core::GUID,
    };
    type Query = unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32;
    #[repr(C)]
    struct Unknown {
        query: Query,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }
    #[repr(C)]
    struct LinkTable {
        unknown: Unknown,
        get_path: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> i32,
    }
    #[repr(C)]
    struct PersistTable {
        unknown: Unknown,
        get_class_id: usize,
        is_dirty: usize,
        load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> i32,
    }
    struct Com(*mut c_void);
    impl Drop for Com {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let table = *self.0.cast::<*const Unknown>();
                    ((*table).release)(self.0);
                }
            }
        }
    }
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    const LINK_CLASS: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);
    const LINK_IID: GUID = GUID::from_u128(0x000214f9_0000_0000_c000_000000000046);
    const PERSIST_IID: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
    if cancelled() {
        return vec![];
    }
    if unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) } < 0 {
        return vec![];
    }
    let _apartment = Apartment;
    let mut folder = ptr::null_mut();
    if unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_Recent,
            KF_FLAG_DONT_VERIFY as u32,
            ptr::null_mut(),
            &mut folder,
        )
    } < 0
        || folder.is_null()
    {
        return vec![];
    }
    let mut length = 0;
    unsafe {
        while length < 32768 && *folder.add(length) != 0 {
            length += 1;
        }
    }
    let root = (length < 32768).then(|| unsafe {
        PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(
            folder, length,
        )))
    });
    unsafe {
        CoTaskMemFree(folder.cast());
    }
    let Some(root) = root.filter(|path| validator.can_validate(path)) else {
        return vec![];
    };
    let Ok(directory) = std::fs::read_dir(root) else {
        return vec![];
    };
    let mut links = Vec::new();
    // One known directory, at most 1024 entries examined and 256 links loaded.
    for file in directory.take(1024) {
        if cancelled() {
            break;
        }
        let Ok(file) = file else {
            continue;
        };
        if !Path::new(&file.file_name())
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
        {
            continue;
        }
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
        links.push((modified, file.path()));
    }
    links.sort_by(|a, b| b.0.cmp(&a.0));
    let mut entries = Vec::new();
    for (last_used, link_path) in links.into_iter().take(256) {
        if cancelled() {
            break;
        }
        let mut pointer = ptr::null_mut();
        if unsafe {
            CoCreateInstance(
                &LINK_CLASS,
                ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &LINK_IID,
                &mut pointer,
            )
        } < 0
            || pointer.is_null()
        {
            continue;
        }
        let link = Com(pointer);
        let link_table = unsafe { *link.0.cast::<*const LinkTable>() };
        let mut persist = ptr::null_mut();
        if unsafe { ((*link_table).unknown.query)(link.0, &PERSIST_IID, &mut persist) } < 0
            || persist.is_null()
        {
            continue;
        }
        let persist = Com(persist);
        let persist_table = unsafe { *persist.0.cast::<*const PersistTable>() };
        let path: Vec<_> = link_path.as_os_str().encode_wide().chain([0]).collect();
        // STGM_READ | STGM_SHARE_DENY_NONE. No Save or Resolve call is made.
        if unsafe { ((*persist_table).load)(persist.0, path.as_ptr(), 0x40) } < 0 {
            continue;
        }
        let mut target = [0u16; 32768];
        // SLGP_RAWPATH reads stored data without searching for moved targets.
        if unsafe {
            ((*link_table).get_path)(
                link.0,
                target.as_mut_ptr(),
                target.len() as i32,
                ptr::null_mut(),
                4,
            )
        } != 0
        {
            continue;
        }
        let length = target
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(target.len());
        if length == 0 || length == target.len() {
            continue;
        }
        let path = PathBuf::from(OsString::from_wide(&target[..length]));
        if validator.can_validate(&path) {
            entries.push(Entry {
                path,
                access: Access::WindowsRecent,
                last_used,
                uses: 0,
            });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn drive_eligibility_is_reused_for_case_and_verbatim_spellings_in_one_batch() {
        let mut validator = Validator::default();
        validator.drives[2] = Some(true);
        validator.drives[3] = Some(false);
        // Seeded decisions avoid querying real drives in this unit test.
        assert!(validator.can_validate(Path::new("C:/saved/file.txt")));
        assert!(validator.can_validate(Path::new(r"c:\saved\another.txt")));
        assert!(validator.can_validate(Path::new(r"\\?\C:\saved\long.txt")));
        assert!(!validator.can_validate(Path::new("D:/saved/file.txt")));
        assert!(!validator.can_validate(Path::new(r"\\server\share\file.txt")));
        assert!(!validator.can_validate(Path::new(r"\\.\PhysicalDrive0")));
        assert!(recent(&|| true, &mut validator).is_empty());
    }

    #[test]
    fn automatic_home_checks_accept_verbatim_drives_but_not_unc_or_device_namespaces() {
        let root = |path: &str| local_drive_root(&path.encode_utf16().collect::<Vec<_>>());
        assert_eq!(root("C:/folder/file.txt"), Some([67, 58, 92, 0]));
        assert_eq!(root(r"\\?\C:\folder\file.txt"), Some([67, 58, 92, 0]));
        assert_eq!(
            root(&format!(r"\\?\D:\{}\file.txt", "long".repeat(100))),
            Some([68, 58, 92, 0])
        );
        assert_eq!(root(r"\\server\share\file.txt"), None);
        assert_eq!(root(r"\\?\UNC\server\share\file.txt"), None);
        assert_eq!(root(r"\\.\PhysicalDrive0"), None);
        assert_eq!(root(r"\\?\GLOBALROOT\Device\HarddiskVolume1"), None);
        assert_eq!(root("C:relative"), None);
    }
}
