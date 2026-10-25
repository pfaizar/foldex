//! Read the Windows Apps folder, including packaged applications without .lnk
//! files. COM objects stay on the provider thread; bounded immutable PIDLs cross
//! the channel. Apps-folder names are not reliable launchable filesystem paths.
pub const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;

/// Stable usefulness bias for cold discovery, without collecting launch history
/// or fabricating installed applications. Negative entries remain searchable.
pub(super) fn default_priority(title: &str) -> i32 {
    let title = title.to_lowercase();
    if [
        "uninstall",
        "updater",
        "update service",
        "crash",
        "debugger",
        "diagnostic",
        "redistributable",
        "sdk",
        "runtime",
        "help viewer",
        "release notes",
    ]
    .iter()
    .any(|word| title.contains(*word))
    {
        return -600;
    }
    if [
        "file explorer",
        "windows explorer",
        "windows terminal",
        "visual studio code",
        "notepad",
        "calculator",
    ]
    .iter()
    .any(|name| title == *name)
    {
        return 400;
    }
    if [
        "chrome",
        "firefox",
        "edge",
        "brave",
        "opera",
        "word",
        "excel",
        "powerpoint",
        "outlook",
        "onenote",
        "paint",
        "photos",
        "7-zip",
        "code",
        "terminal",
        "settings",
    ]
    .iter()
    .any(|word| {
        title
            .split(|c: char| !c.is_alphanumeric() && c != '-')
            .any(|part| part == *word)
    }) {
        return 300;
    }
    if [
        "discord", "slack", "teams", "spotify", "vlc", "obs", "figma", "adobe", "acrobat",
        "blender", "zoom",
    ]
    .iter()
    .any(|word| {
        title
            .split(|c: char| !c.is_alphanumeric())
            .any(|part| part == *word)
    }) {
        return 200;
    }
    0
}
pub fn valid_target(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || bytes.len() > 64 * 1024 {
        return false;
    }
    let mut at = 0;
    while at + 2 <= bytes.len() {
        let size = u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
        if size == 0 {
            return at + 2 == bytes.len();
        }
        if size < 2 || size > bytes.len() - at {
            return false;
        }
        at += size;
    }
    false
}
#[cfg(windows)]
pub fn installed(current: &dyn Fn() -> bool) -> Option<Vec<(String, std::sync::Arc<[u8]>)>> {
    use std::{ffi::c_void, ptr};
    use windows_sys::{
        Win32::{System::Com::*, UI::Shell::*},
        core::{GUID, HRESULT},
    };
    // Called slots match SDK 10.0.26100.0 ShObjIdl_core.h (IShellItem and
    // IEnumShellItems). Uncalled slots retain their pointer-sized ABI positions.
    #[repr(C)]
    struct Unknown {
        query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
        add: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
    }
    #[repr(C)]
    struct Item {
        unknown: Unknown,
        bind: unsafe extern "system" fn(
            *mut c_void,
            *mut c_void,
            *const GUID,
            *const GUID,
            *mut *mut c_void,
        ) -> HRESULT,
        parent: usize,
        name: unsafe extern "system" fn(*mut c_void, SIGDN, *mut *mut u16) -> HRESULT,
        attributes: usize,
        compare: usize,
    }
    #[repr(C)]
    struct Enumeration {
        unknown: Unknown,
        next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> HRESULT,
        skip: usize,
        reset: usize,
        clone: usize,
    }
    struct Object(*mut c_void);
    impl Drop for Object {
        fn drop(&mut self) {
            let table = unsafe { &**(self.0 as *const *const Unknown) };
            unsafe {
                (table.release)(self.0);
            }
        }
    }
    impl Object {
        fn name(&self, kind: SIGDN) -> Option<String> {
            let table = unsafe { &**(self.0 as *const *const Item) };
            let mut text = ptr::null_mut();
            if unsafe { (table.name)(self.0, kind, &mut text) } < 0 || text.is_null() {
                return None;
            }
            let mut length = 0;
            while length < 16_384 && unsafe { *text.add(length) } != 0 {
                length += 1;
            }
            let value = (length < 16_384).then(|| {
                String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
            });
            unsafe {
                CoTaskMemFree(text.cast());
            }
            value
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
    if !current() {
        return None;
    }
    if unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) } < 0 {
        return Some(vec![]);
    }
    let _apartment = Apartment;
    const ITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
    const ENUM: GUID = GUID::from_u128(0x70629033_e363_4a28_a567_0db78006e6d7);
    let mut folder = ptr::null_mut();
    if unsafe { SHGetKnownFolderItem(&FOLDERID_AppsFolder, 0, ptr::null_mut(), &ITEM, &mut folder) }
        < 0
        || folder.is_null()
    {
        return Some(vec![]);
    }
    let folder = Object(folder);
    if !current() {
        return None;
    }
    let table = unsafe { &**(folder.0 as *const *const Item) };
    let mut enumeration = ptr::null_mut();
    if unsafe {
        (table.bind)(
            folder.0,
            ptr::null_mut(),
            &BHID_EnumItems,
            &ENUM,
            &mut enumeration,
        )
    } < 0
        || enumeration.is_null()
    {
        return Some(vec![]);
    }
    let enumeration = Object(enumeration);
    let table = unsafe { &**(enumeration.0 as *const *const Enumeration) };
    let mut apps = Vec::new();
    let mut retained = 0;
    for _ in 0..8192 {
        if !current() {
            return None;
        }
        let mut item = ptr::null_mut();
        let mut fetched = 0;
        if unsafe { (table.next)(enumeration.0, 1, &mut item, &mut fetched) } < 0
            || fetched != 1
            || item.is_null()
        {
            break;
        }
        let item = Object(item);
        if !current() {
            return None;
        }
        if let Some(title) = item.name(SIGDN_NORMALDISPLAY) {
            if !current() {
                return None;
            }
            if !title.is_empty() {
                let mut pidl = ptr::null_mut();
                if unsafe { SHGetIDListFromObject(item.0, &mut pidl) } < 0 || pidl.is_null() {
                    continue;
                }
                let size = unsafe { ILGetSize(pidl) } as usize;
                let identifier = (size <= 64 * 1024).then(|| {
                    unsafe { std::slice::from_raw_parts(pidl.cast::<u8>(), size) }.to_vec()
                });
                #[cfg(test)]
                if apps.len() < 16 {
                    let mut resolved = ptr::null_mut();
                    let hr = unsafe { SHCreateItemFromIDList(pidl, &ITEM, &mut resolved) };
                    assert!(
                        hr >= 0 && !resolved.is_null(),
                        "Native application identifier did not roundtrip: {hr:#x}"
                    );
                    drop(Object(resolved));
                }
                unsafe {
                    CoTaskMemFree(pidl.cast());
                }
                let Some(identifier) = identifier.filter(|bytes| valid_target(bytes)) else {
                    continue;
                };
                retained += title.len() + identifier.len();
                if retained > MAX_CATALOG_BYTES {
                    break;
                }
                apps.push((title, std::sync::Arc::from(identifier)));
            }
        }
    }
    current().then_some(apps)
}
#[cfg(not(windows))]
pub fn installed(current: &dyn Fn() -> bool) -> Option<Vec<(String, std::sync::Arc<[u8]>)>> {
    current().then(Vec::new)
}

#[cfg(test)]
mod tests {
    #[test]
    fn cancelled_app_catalog_does_not_initialize_native_enumeration() {
        assert!(super::installed(&|| false).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn cancelled_native_enumeration_discards_its_partial_catalog() {
        let calls = std::cell::Cell::new(0);
        let apps = super::installed(&|| {
            calls.set(calls.get() + 1);
            calls.get() < 5
        });
        assert!(apps.is_none());
        assert_eq!(calls.get(), 5);
    }

    #[cfg(windows)]
    #[test]
    fn installed_apps_enumerate_without_launching_anything() {
        let apps = super::installed(&|| true).unwrap();
        assert!(
            !apps.is_empty(),
            "Windows Apps folder should enumerate this system's installed applications"
        );
        assert!(apps.len() <= 8192);
        assert!(
            apps.iter()
                .all(|(name, identifier)| !name.is_empty() && super::valid_target(identifier))
        );
        assert!(
            apps.iter()
                .map(|(name, id)| name.len() + id.len())
                .sum::<usize>()
                <= super::MAX_CATALOG_BYTES
        );
        println!(
            "{} installed Apps-folder entries; no application launched",
            apps.len()
        );
    }
}
