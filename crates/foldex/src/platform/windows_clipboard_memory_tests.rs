//! Real COM ABI/storage-medium tests without reading or writing the clipboard.
//! Shell format names are registered, but clipboard contents/ownership stay put.
//! Fixture paths are strings; no file is opened, created, copied or moved.

use super::*;
use std::sync::Mutex;

const IID_DATA_OBJECT: GUID = GUID::from_u128(0x0000010e_0000_0000_c000_000000000046);
const E_FAIL: i32 = 0x80004005u32 as i32;
const DV_E_FORMATETC: i32 = 0x80040064u32 as i32;

#[derive(Default)]
struct Counts {
    issued_media: AtomicU32,
    released_media: AtomicU32,
    accepted_sets: AtomicU32,
    released_objects: AtomicU32,
}

#[repr(C)]
struct MemoryDataObject {
    vtable: &'static DataObjectVtable,
    references: AtomicU32,
    formats: Mutex<Vec<(u16, Vec<u8>)>>,
    reject_optimized_ack: bool,
    counts: Arc<Counts>,
}

#[repr(C)]
struct MediumOwner {
    vtable: &'static UnknownVtable,
    references: AtomicU32,
    _memory: GlobalMemory,
    counts: Arc<Counts>,
}

unsafe extern "system" fn data_query(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    unsafe {
        *output = ptr::null_mut();
    }
    if unsafe { guid_equal(&*iid, &IID_UNKNOWN) || guid_equal(&*iid, &IID_DATA_OBJECT) } {
        unsafe {
            *output = this;
            data_add_ref(this);
        }
        0
    } else {
        E_NOINTERFACE
    }
}
unsafe extern "system" fn data_add_ref(this: *mut c_void) -> u32 {
    unsafe { &*this.cast::<MemoryDataObject>() }
        .references
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn data_release(this: *mut c_void) -> u32 {
    let previous = unsafe { &*this.cast::<MemoryDataObject>() }
        .references
        .fetch_sub(1, Ordering::AcqRel);
    if previous == 1 {
        let data = unsafe { Box::from_raw(this.cast::<MemoryDataObject>()) };
        data.counts.released_objects.fetch_add(1, Ordering::Relaxed);
        drop(data);
    }
    previous - 1
}

unsafe extern "system" fn owner_query(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if iid.is_null() || output.is_null() {
        return E_POINTER;
    }
    unsafe {
        *output = ptr::null_mut();
    }
    if unsafe { guid_equal(&*iid, &IID_UNKNOWN) } {
        unsafe {
            *output = this;
            owner_add_ref(this);
        }
        0
    } else {
        E_NOINTERFACE
    }
}
unsafe extern "system" fn owner_add_ref(this: *mut c_void) -> u32 {
    unsafe { &*this.cast::<MediumOwner>() }
        .references
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn owner_release(this: *mut c_void) -> u32 {
    let previous = unsafe { &*this.cast::<MediumOwner>() }
        .references
        .fetch_sub(1, Ordering::AcqRel);
    if previous == 1 {
        let owner = unsafe { Box::from_raw(this.cast::<MediumOwner>()) };
        owner.counts.released_media.fetch_add(1, Ordering::Relaxed);
        drop(owner); // releases the HGLOBAL through GlobalMemory
    }
    previous - 1
}

static MEDIUM_OWNER_VTABLE: UnknownVtable = UnknownVtable {
    query_interface: owner_query,
    add_ref: owner_add_ref,
    release: owner_release,
};

unsafe extern "system" fn data_get(
    this: *mut c_void,
    requested: *const FORMATETC,
    output: *mut STGMEDIUM,
) -> i32 {
    if requested.is_null() || output.is_null() {
        return E_POINTER;
    }
    let requested = unsafe { &*requested };
    if requested.tymed & TYMED_HGLOBAL as u32 == 0 {
        return DV_E_FORMATETC;
    }
    let data = unsafe { &*this.cast::<MemoryDataObject>() };
    let bytes = {
        let Ok(formats) = data.formats.lock() else {
            return E_FAIL;
        };
        let Some((_, bytes)) = formats
            .iter()
            .find(|(format, _)| *format == requested.cfFormat)
        else {
            return DV_E_FORMATETC;
        };
        bytes.clone()
    };
    let Ok(memory) = GlobalMemory::from_bytes(&bytes) else {
        return E_FAIL;
    };
    let handle = memory.0;
    let owner = Box::into_raw(Box::new(MediumOwner {
        vtable: &MEDIUM_OWNER_VTABLE,
        references: AtomicU32::new(1),
        _memory: memory,
        counts: data.counts.clone(),
    }));
    data.counts.issued_media.fetch_add(1, Ordering::Relaxed);
    unsafe {
        *output = STGMEDIUM {
            tymed: TYMED_HGLOBAL as u32,
            u: STGMEDIUM_0 { hGlobal: handle },
            pUnkForRelease: owner.cast(),
        };
    }
    0
}

unsafe extern "system" fn data_set(
    this: *mut c_void,
    requested: *const FORMATETC,
    medium: *const STGMEDIUM,
    release: i32,
) -> i32 {
    if requested.is_null() || medium.is_null() {
        return E_POINTER;
    }
    let data = unsafe { &*this.cast::<MemoryDataObject>() };
    let requested = unsafe { &*requested };
    let medium = unsafe { &*medium };
    if medium.tymed != TYMED_HGLOBAL as u32 {
        return DV_E_FORMATETC;
    }
    if data.reject_optimized_ack
        && registered(CFSTR_PERFORMEDDROPEFFECT).ok() == Some(requested.cfFormat as u32)
    {
        return E_FAIL; // rejected SetData retains caller ownership
    }
    let global = unsafe { medium.u.hGlobal };
    let size = unsafe { GlobalSize(global) };
    if size < 4 || size > MAX_PATH_BYTES {
        return E_FAIL;
    }
    let locked = unsafe { GlobalLock(global) };
    if locked.is_null() {
        return E_FAIL;
    }
    // This fixture accepts DWORD effect formats. Ignore allocation padding.
    let bytes = unsafe { std::slice::from_raw_parts(locked.cast::<u8>(), 4) }.to_vec();
    unsafe { GlobalUnlock(global) };
    {
        let Ok(mut formats) = data.formats.lock() else {
            return E_FAIL;
        };
        if let Some((_, current)) = formats
            .iter_mut()
            .find(|(format, _)| *format == requested.cfFormat)
        {
            *current = bytes;
        } else {
            if formats.len() == 8 {
                return E_FAIL;
            }
            formats.push((requested.cfFormat, bytes));
        }
    }
    data.counts.accepted_sets.fetch_add(1, Ordering::Relaxed);
    if release != 0 {
        let mut owned = *medium;
        unsafe { ReleaseStgMedium(&mut owned) }; // accepts exactly one medium
    }
    0
}

static MEMORY_DATA_VTABLE: DataObjectVtable = DataObjectVtable {
    unknown: UnknownVtable {
        query_interface: data_query,
        add_ref: data_add_ref,
        release: data_release,
    },
    get_data: data_get,
    get_data_here: 0,
    query_get_data: 0,
    get_canonical_format: 0,
    set_data: data_set,
    remaining: [0; 4],
};

fn fixture(paths: &[PathBuf], reject_optimized_ack: bool) -> (ComPointer, Arc<Counts>) {
    let counts = Arc::new(Counts::default());
    let preferred = registered(CFSTR_PREFERREDDROPEFFECT).unwrap() as u16;
    let data = MemoryDataObject {
        vtable: &MEMORY_DATA_VTABLE,
        references: AtomicU32::new(1),
        formats: Mutex::new(vec![
            (CF_HDROP as u16, encode_drop_files(paths).unwrap()),
            (preferred, DROPEFFECT_MOVE.to_le_bytes().to_vec()),
        ]),
        reject_optimized_ack,
        counts: counts.clone(),
    };
    (ComPointer(Box::into_raw(Box::new(data)).cast()), counts)
}

#[test]
fn memory_data_object_extracts_unicode_and_releases_each_storage_medium() {
    let paths = vec![
        PathBuf::from(r"C:\generated fixture\résumé.txt"),
        PathBuf::from(r"D:\generated fixture\folder"),
    ];
    let (data, counts) = fixture(&paths, false);
    let retained = data.clone();
    drop(data);
    assert_eq!(counts.released_objects.load(Ordering::Acquire), 0);
    assert_eq!(read_paths(&retained).unwrap(), paths);
    assert_eq!(
        read_effect(&retained, CFSTR_PREFERREDDROPEFFECT),
        Some(DROPEFFECT_MOVE)
    );
    assert_eq!(counts.issued_media.load(Ordering::Acquire), 2);
    assert_eq!(counts.released_media.load(Ordering::Acquire), 2);
    drop(retained);
    assert_eq!(counts.released_objects.load(Ordering::Acquire), 1);
}

#[test]
fn memory_data_object_feedback_reports_optimized_success_cancel_and_refusal() {
    let paths = [PathBuf::from(r"C:\generated fixture\source.txt")];
    for (success, refuse, expected_paste) in [
        (true, false, DROPEFFECT_MOVE),
        (false, false, DROPEFFECT_NONE),
        (true, true, DROPEFFECT_NONE),
    ] {
        let (data, counts) = fixture(&paths, refuse);
        feedback(&data, true, success);
        assert_eq!(
            read_effect(&data, CFSTR_PASTESUCCEEDED),
            Some(expected_paste)
        );
        assert_eq!(
            read_effect(&data, CFSTR_LOGICALPERFORMEDDROPEFFECT),
            Some(if success {
                DROPEFFECT_MOVE
            } else {
                DROPEFFECT_NONE
            })
        );
        if !refuse {
            assert_eq!(
                read_effect(&data, CFSTR_PERFORMEDDROPEFFECT),
                Some(DROPEFFECT_NONE)
            );
        }
        assert_eq!(
            counts.accepted_sets.load(Ordering::Acquire),
            if refuse { 2 } else { 3 }
        );
        assert_eq!(
            counts.issued_media.load(Ordering::Acquire),
            counts.released_media.load(Ordering::Acquire)
        );
        drop(data);
        assert_eq!(counts.released_objects.load(Ordering::Acquire), 1);
    }
}
