//! Native ABI definitions are checked against SDK 10.0.26100.0 ShObjIdl_core.h
//! and the locally pinned windows 0.62.2 metadata. Uncalled vtable slots retain
//! their exact pointer-sized positions. No caller-owned PIDL is dereferenced.
//!
//! Metadata/restore contracts:
//! https://devblogs.microsoft.com/oldnewthing/20110830-00/?p=9773/
//! https://devblogs.microsoft.com/oldnewthing/20110901-00/?p=9753/

use super::{
    Action, CANCELLED, Item, MAX_ID_BYTES, MAX_ITEMS, MAX_SNAPSHOT_BYTES, check_cancelled, failure,
    unix_seconds, validate_pidl,
};
use std::{
    collections::HashSet,
    ffi::{OsString, c_void},
    mem,
    os::windows::ffi::OsStringExt,
    path::{Component, Path, PathBuf},
    ptr,
    sync::atomic::{AtomicBool, AtomicI32, Ordering},
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::{FILETIME, HWND, LPARAM, PROPERTYKEY},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
            Ole::{OleInitialize, OleUninitialize},
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
            Variant::{
                PSTF_UTC, VARIANT, VariantClear, VariantToFileTime, VariantToStringAlloc,
                VariantToUInt64,
            },
        },
        UI::{
            Shell::{
                CMF_NORMAL, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX,
                Common::{ITEMIDLIST, STRRET},
                FOF_WANTNUKEWARNING, FOLDERID_RecycleBinFolder, FileOperation, ILCombine,
                ILGetSize, SEE_MASK_NOASYNC, SEE_MASK_UNICODE, SHBindToObject, SHCONTF_FOLDERS,
                SHCONTF_INCLUDEHIDDEN, SHCONTF_INCLUDESUPERHIDDEN, SHCONTF_NONFOLDERS,
                SHCreateShellItemArray, SHCreateThreadRef, SHERB_NOCONFIRMATION,
                SHEmptyRecycleBinW, SHGDN_INFOLDER, SHGetKnownFolderIDList, SHGetNameFromIDList,
                SHGetThreadRef, SHSetThreadRef, SIGDN_FILESYSPATH, StrRetToStrW,
            },
            WindowsAndMessaging::{
                CreatePopupMenu, DestroyMenu, DispatchMessageW, EnumThreadWindows, EnumWindows,
                FindWindowExW, GA_ROOT, GetAncestor, GetClassNameW, GetWindowThreadProcessId,
                HMENU, IsDialogMessageW, IsWindow, IsWindowVisible, MSG, MWMO_INPUTAVAILABLE,
                MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, SW_SHOWNORMAL,
                TranslateMessage,
            },
        },
    },
    core::{GUID, HRESULT},
};

const IID_FOLDER2: GUID = GUID::from_u128(0x93f2f68c_1d1b_11d3_a30e_00c04f79abd1);
const IID_CONTEXT: GUID = GUID::from_u128(0x000214e4_0000_0000_c000_000000000046);
const IID_FILE_OPERATION: GUID = GUID::from_u128(0x947aab5f_0a5c_4c13_b4d6_4bf7836fc9f8);
const DISPLACED: GUID = GUID::from_u128(0x9b174b33_40ff_11d2_a27e_00c04fc30871);
const STORAGE: GUID = GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac);
const ORIGINAL_LOCATION: PROPERTYKEY = PROPERTYKEY {
    fmtid: DISPLACED,
    pid: 2,
};
const DATE_DELETED: PROPERTYKEY = PROPERTYKEY {
    fmtid: DISPLACED,
    pid: 3,
};
const SIZE: PROPERTYKEY = PROPERTYKEY {
    fmtid: STORAGE,
    pid: 12,
};
const FILE_ATTRIBUTES: PROPERTYKEY = PROPERTYKEY {
    fmtid: STORAGE,
    pid: 13,
};
const FILE_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x41cf5ae0_f75a_4806_bd87_59c7d9248eb9),
    pid: 100,
};
const SFGAO_FOLDER: u32 = 0x2000_0000;
const SFGAO_FILESYSTEM: u32 = 0x4000_0000;
const SFGAO_STREAM: u32 = 0x0040_0000;
const FILE_ATTRIBUTE_DIRECTORY: u64 = 0x10;
const MAX_STRING_UNITS: usize = 32 * 1024;
const FIRST_COMMAND: u32 = 1;
const LAST_COMMAND: u32 = 0x7fff;

pub(super) fn enumerate(cancel: &AtomicBool) -> Result<Vec<Item>, String> {
    let _apartment = Apartment::new()?;
    check_cancelled(cancel)?;
    let bin = Bin::open()?;
    let mut result = Vec::new();
    let mut budget = Budget::default();
    bin.visit(cancel, |child, absolute, id| {
        let item = bin.item(&child, &absolute, id, cancel)?;
        budget.add(item_bytes(&item))?;
        result.push(item);
        Ok(true)
    })?;
    check_cancelled(cancel)?;
    Ok(result)
}

pub(super) fn selection_action(
    ids: &[Vec<u8>],
    owner: isize,
    action: Action,
) -> Result<(), String> {
    let _apartment = Apartment::new()?;
    let owner = owner_window(owner)?;
    let bin = Bin::open()?;
    let selection = bin.resolve(ids)?;
    match action {
        Action::Properties => invoke_verb(&bin, &selection, owner, true),
        Action::Restore => {
            invoke_verb(&bin, &selection, owner, false)?;
            // IContextMenu has no GetAnyOperationsAborted equivalent. Check the
            // namespace afterwards so a successful HRESULT cannot falsely claim
            // every selected item was restored after a cancelled conflict UI.
            ensure_removed(ids, "restore")
        }
        Action::Remove => {
            delete_items(&bin, &selection, owner)?;
            ensure_removed(ids, "permanent deletion")
        }
    }
}

pub(super) fn empty(owner: isize, confirmed: bool) -> Result<(), String> {
    let _apartment = Apartment::new()?;
    let owner = owner_window(owner)?;
    let flags = if confirmed { SHERB_NOCONFIRMATION } else { 0 };
    // NULL root means all of this user's drive bins. Never infer drive roots
    // from display metadata. Progress and error UI are intentionally enabled.
    let hr = unsafe { SHEmptyRecycleBinW(owner, ptr::null(), flags) };
    check(hr, "empty")
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, String> {
        // OleInitialize selects STA, initializes OLE used by native Shell UI,
        // and fails with RPC_E_CHANGED_MODE on an already-initialized MTA.
        let hr = unsafe { OleInitialize(ptr::null()) };
        check(hr, "STA/OLE initialization")?;
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        // S_FALSE (already initialized STA) must also be balanced exactly once.
        unsafe { OleUninitialize() };
    }
}

#[repr(C)]
struct UnknownVtable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}
#[repr(C)]
struct FolderVtable {
    unknown: UnknownVtable,
    parse_display_name: usize,
    enum_objects: unsafe extern "system" fn(*mut c_void, HWND, u32, *mut *mut c_void) -> HRESULT,
    bind_to_object: usize,
    bind_to_storage: usize,
    compare_ids: usize,
    create_view_object: usize,
    get_attributes_of:
        unsafe extern "system" fn(*mut c_void, u32, *const *const ITEMIDLIST, *mut u32) -> HRESULT,
    get_ui_object_of: unsafe extern "system" fn(
        *mut c_void,
        HWND,
        u32,
        *const *const ITEMIDLIST,
        *const GUID,
        *const u32,
        *mut *mut c_void,
    ) -> HRESULT,
    get_display_name_of:
        unsafe extern "system" fn(*mut c_void, *const ITEMIDLIST, u32, *mut STRRET) -> HRESULT,
    set_name_of: usize,
}
#[repr(C)]
struct Folder2Vtable {
    folder: FolderVtable,
    get_default_search_guid: usize,
    enum_searches: usize,
    get_default_column: usize,
    get_default_column_state: usize,
    get_details_ex: unsafe extern "system" fn(
        *mut c_void,
        *const ITEMIDLIST,
        *const PROPERTYKEY,
        *mut VARIANT,
    ) -> HRESULT,
    get_details_of: usize,
    map_column_to_scid: usize,
}
#[repr(C)]
struct EnumVtable {
    unknown: UnknownVtable,
    next: unsafe extern "system" fn(*mut c_void, u32, *mut *mut ITEMIDLIST, *mut u32) -> HRESULT,
    skip: usize,
    reset: usize,
    clone: usize,
}
#[repr(C)]
struct ContextVtable {
    unknown: UnknownVtable,
    query_menu: unsafe extern "system" fn(*mut c_void, HMENU, u32, u32, u32, u32) -> HRESULT,
    invoke: unsafe extern "system" fn(*mut c_void, *mut CMINVOKECOMMANDINFO) -> HRESULT,
    get_command_string: usize,
}
#[repr(C)]
struct FileOperationVtable {
    unknown: UnknownVtable,
    advise: usize,
    unadvise: usize,
    set_operation_flags: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    set_progress_message: usize,
    set_progress_dialog: usize,
    set_properties: usize,
    set_owner_window: unsafe extern "system" fn(*mut c_void, HWND) -> HRESULT,
    apply_properties_to_item: usize,
    apply_properties_to_items: usize,
    rename_item: usize,
    rename_items: usize,
    move_item: usize,
    move_items: usize,
    copy_item: usize,
    copy_items: usize,
    delete_item: usize,
    delete_items: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    new_item: usize,
    perform_operations: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    get_any_operations_aborted: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
}

struct Com(*mut c_void);
impl Com {
    fn received(hr: HRESULT, pointer: *mut c_void, operation: &str) -> Result<Self, String> {
        // Even a failed COM call can return an owned non-null interface.
        let object = if pointer.is_null() {
            None
        } else {
            Some(Self(pointer))
        };
        check(hr, operation)?;
        object.ok_or_else(|| format!("Recycle Bin {operation} returned no interface"))
    }
    fn vtable<T>(&self) -> &T {
        // Only used with the interface IID and its verified matching ABI.
        unsafe { &**self.0.cast::<*const T>() }
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { (self.vtable::<UnknownVtable>().release)(self.0) };
    }
}

// This type is constructed only from Shell-owned allocations, never Vec<u8>.
struct Pidl(*mut ITEMIDLIST);
impl Pidl {
    fn received(hr: HRESULT, pointer: *mut ITEMIDLIST, operation: &str) -> Result<Self, String> {
        let pidl = Self(pointer);
        check(hr, operation)?;
        if pidl.0.is_null() {
            Err(format!("Recycle Bin {operation} returned no identity"))
        } else {
            Ok(pidl)
        }
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        // ILGetSize is used only on an allocator-owned PIDL received from Shell.
        let size = unsafe { ILGetSize(self.0) } as usize;
        if !(4..=MAX_ID_BYTES).contains(&size) {
            return Err("Recycle Bin returned an oversized or empty native identity".into());
        }
        let bytes = unsafe { std::slice::from_raw_parts(self.0.cast::<u8>(), size) };
        validate_pidl(bytes)?;
        Ok(bytes.to_vec())
    }
}
impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(self.0.cast()) };
    }
}

struct TaskString(*mut u16);
impl TaskString {
    fn received(hr: HRESULT, pointer: *mut u16, operation: &str) -> Result<OsString, String> {
        let value = Self(pointer);
        check(hr, operation)?;
        if value.0.is_null() {
            return Err(format!("Recycle Bin {operation} returned no string"));
        }
        // Native APIs promise a NUL-terminated task-allocated string. Bound
        // retained metadata and reject pathological extension output.
        for length in 0..MAX_STRING_UNITS {
            if unsafe { *value.0.add(length) } == 0 {
                let units = unsafe { std::slice::from_raw_parts(value.0, length) };
                return Ok(OsString::from_wide(units));
            }
        }
        Err("Recycle Bin metadata string exceeds 32,767 UTF-16 units".into())
    }
}
impl Drop for TaskString {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(self.0.cast()) };
    }
}

#[derive(Default)]
struct Variant(VARIANT);
impl Variant {
    fn text(&self) -> Result<OsString, String> {
        let mut pointer = ptr::null_mut();
        let hr = unsafe { VariantToStringAlloc(&self.0, &mut pointer) };
        TaskString::received(hr, pointer, "read text metadata")
    }
    fn number(&self) -> Result<u64, String> {
        let mut value = 0;
        check(
            unsafe { VariantToUInt64(&self.0, &mut value) },
            "read numeric metadata",
        )?;
        Ok(value)
    }
    fn deleted_at(&self) -> Result<i64, String> {
        let mut time = FILETIME::default();
        check(
            unsafe { VariantToFileTime(&self.0, PSTF_UTC, &mut time) },
            "read deletion time",
        )?;
        Ok(unix_seconds(
            (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime),
        ))
    }
}
impl Drop for Variant {
    fn drop(&mut self) {
        unsafe { VariantClear(&mut self.0) };
    }
}

#[derive(Default)]
struct Budget {
    items: usize,
    bytes: usize,
}
impl Budget {
    fn add(&mut self, bytes: usize) -> Result<(), String> {
        if self.items >= MAX_ITEMS {
            return Err(
                "Recycle Bin contains more than 100,000 items; snapshot was not loaded".into(),
            );
        }
        self.items += 1;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= MAX_SNAPSHOT_BYTES)
            .ok_or("Recycle Bin snapshot exceeds 64 MiB; snapshot was not loaded")?;
        Ok(())
    }
}

struct Bin {
    folder: Com,
    root: Pidl,
}
impl Bin {
    fn open() -> Result<Self, String> {
        let mut pointer = ptr::null_mut();
        let hr = unsafe {
            SHGetKnownFolderIDList(&FOLDERID_RecycleBinFolder, 0, ptr::null_mut(), &mut pointer)
        };
        let root = Pidl::received(hr, pointer, "locate native namespace")?;
        root.bytes()?;
        let mut pointer = ptr::null_mut();
        let hr = unsafe {
            SHBindToObject(
                ptr::null_mut(),
                root.0,
                ptr::null_mut(),
                &IID_FOLDER2,
                &mut pointer,
            )
        };
        let folder = Com::received(hr, pointer, "bind native namespace")?;
        Ok(Self { folder, root })
    }

    fn visit(
        &self,
        cancel: &AtomicBool,
        mut visitor: impl FnMut(Pidl, Pidl, Vec<u8>) -> Result<bool, String>,
    ) -> Result<(), String> {
        check_cancelled(cancel)?;
        let mut pointer = ptr::null_mut();
        let hr = unsafe {
            (self.folder.vtable::<Folder2Vtable>().folder.enum_objects)(
                self.folder.0,
                ptr::null_mut(),
                (SHCONTF_FOLDERS
                    | SHCONTF_NONFOLDERS
                    | SHCONTF_INCLUDEHIDDEN
                    | SHCONTF_INCLUDESUPERHIDDEN) as u32,
                &mut pointer,
            )
        };
        // EnumObjects explicitly permits S_FALSE/NULL for an empty folder.
        if hr == 1 && pointer.is_null() {
            return Ok(());
        }
        let enumerator = Com::received(hr, pointer, "enumerate namespace")?;
        let mut budget = Budget::default();
        loop {
            check_cancelled(cancel)?;
            let mut pointer = ptr::null_mut();
            let mut fetched = 0;
            let hr = unsafe {
                (enumerator.vtable::<EnumVtable>().next)(
                    enumerator.0,
                    1,
                    &mut pointer,
                    &mut fetched,
                )
            };
            let child = Pidl(pointer);
            check(hr, "read native item")?;
            if hr == 1 && fetched == 0 {
                break;
            }
            if hr != 0 || fetched != 1 || child.0.is_null() {
                return Err("Recycle Bin enumerator returned an invalid item count".into());
            }
            // IShellFolder2/GetAttributesOf require a single-level child PIDL.
            if validate_pidl(&child.bytes()?)? != 1 {
                return Err("Recycle Bin returned a non-child native identity".into());
            }
            let absolute = Pidl(unsafe { ILCombine(self.root.0, child.0) });
            if absolute.0.is_null() {
                return Err("Cannot allocate Recycle Bin absolute identity".into());
            }
            let id = absolute.bytes()?;
            budget.add(id.len())?;
            check_cancelled(cancel)?;
            if !visitor(child, absolute, id)? {
                break;
            }
            if budget.items % 16 == 0 {
                pump_messages();
            }
        }
        check_cancelled(cancel)
    }

    fn detail(&self, child: &Pidl, key: &PROPERTYKEY) -> Result<Variant, String> {
        let mut value = Variant::default();
        check(
            unsafe {
                (self.folder.vtable::<Folder2Vtable>().get_details_ex)(
                    self.folder.0,
                    child.0,
                    key,
                    &mut value.0,
                )
            },
            "read item details",
        )?;
        Ok(value)
    }

    fn item(
        &self,
        child: &Pidl,
        absolute: &Pidl,
        id: Vec<u8>,
        cancel: &AtomicBool,
    ) -> Result<Item, String> {
        let name = self.display_name(child, SHGDN_INFOLDER)?;
        check_cancelled(cancel)?;
        let original_location = self
            .detail(child, &ORIGINAL_LOCATION)
            .and_then(|value| value.text())
            .ok();
        check_cancelled(cancel)?;
        // System.FileName includes the extension. Shell's display name can hide
        // it or localize it, so do not use a backing $R path to invent a name.
        let original_name = self
            .detail(child, &FILE_NAME)
            .and_then(|value| value.text())
            .ok()
            .filter(|value| is_leaf(Path::new(value)))
            .unwrap_or_else(|| name.clone());
        let original_path = original_location
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && is_leaf(Path::new(&original_name)))
            .map(|path| path.join(original_name))
            .unwrap_or_default();
        check_cancelled(cancel)?;
        let deleted_at = self
            .detail(child, &DATE_DELETED)
            .and_then(|value| value.deleted_at())
            .unwrap_or(0);
        check_cancelled(cancel)?;
        let size = self
            .detail(child, &SIZE)
            .and_then(|value| value.number())
            .unwrap_or(0);
        check_cancelled(cancel)?;
        let mut attributes = SFGAO_FOLDER | SFGAO_FILESYSTEM | SFGAO_STREAM;
        let child_pointer = child.0 as *const ITEMIDLIST;
        check(
            unsafe {
                (self
                    .folder
                    .vtable::<Folder2Vtable>()
                    .folder
                    .get_attributes_of)(
                    self.folder.0, 1, &child_pointer, &mut attributes
                )
            },
            "read item type",
        )?;
        let is_dir = self
            .detail(child, &FILE_ATTRIBUTES)
            .and_then(|value| value.number())
            .map(|attributes| attributes & FILE_ATTRIBUTE_DIRECTORY != 0)
            // A ZIP is a Shell folder and a stream, not a filesystem directory.
            .unwrap_or(attributes & SFGAO_FOLDER != 0 && attributes & SFGAO_STREAM == 0);
        check_cancelled(cancel)?;
        let file_system_path = if attributes & SFGAO_FILESYSTEM != 0 {
            let mut pointer = ptr::null_mut();
            let hr = unsafe { SHGetNameFromIDList(absolute.0, SIGDN_FILESYSPATH, &mut pointer) };
            TaskString::received(hr, pointer, "read backing path")
                .ok()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        } else {
            None
        };
        Ok(Item {
            id,
            name: name.to_string_lossy().into_owned(),
            original_path,
            deleted_at,
            size,
            is_dir,
            file_system_path,
        })
    }

    fn display_name(&self, child: &Pidl, flags: u32) -> Result<OsString, String> {
        let mut display = STRRET::default();
        check(
            unsafe {
                (self
                    .folder
                    .vtable::<Folder2Vtable>()
                    .folder
                    .get_display_name_of)(
                    self.folder.0, child.0, flags, &mut display
                )
            },
            "read display name",
        )?;
        let mut pointer = ptr::null_mut();
        // StrRetToStrW consumes/frees a STRRET_WSTR and normalizes all three
        // STRRET representations. The returned string uses CoTaskMemFree.
        let hr = unsafe { StrRetToStrW(&mut display, child.0, &mut pointer) };
        TaskString::received(hr, pointer, "convert display name")
    }

    fn resolve(&self, ids: &[Vec<u8>]) -> Result<Vec<Pidl>, String> {
        let mut wanted: HashSet<&[u8]> = ids.iter().map(Vec::as_slice).collect();
        let mut selected = Vec::with_capacity(ids.len());
        self.visit(&AtomicBool::new(false), |child, _, id| {
            // Compare exact native bytes. Merely validating PIDL framing or a
            // namespace prefix would still allow attacker-controlled payloads.
            if wanted.remove(id.as_slice()) {
                selected.push(child);
            }
            Ok(!wanted.is_empty())
        })?;
        if !wanted.is_empty() {
            return Err(format!(
                "Recycle Bin selection changed ({} items are no longer available); refresh and select again",
                wanted.len()
            ));
        }
        Ok(selected)
    }
}

fn ensure_removed(ids: &[Vec<u8>], operation: &str) -> Result<(), String> {
    let wanted: HashSet<&[u8]> = ids.iter().map(Vec::as_slice).collect();
    let mut remaining = 0usize;
    Bin::open()?.visit(&AtomicBool::new(false), |_, _, id| {
        if wanted.contains(id.as_slice()) {
            remaining += 1;
        }
        Ok(true)
    })?;
    if remaining == 0 {
        Ok(())
    } else {
        Err(format!(
            "Recycle Bin {operation} was cancelled or incomplete; {remaining} selected items remain. Refresh before trying again"
        ))
    }
}

fn invoke_verb(bin: &Bin, selection: &[Pidl], owner: HWND, properties: bool) -> Result<(), String> {
    let children: Vec<*const ITEMIDLIST> =
        selection.iter().map(|pidl| pidl.0.cast_const()).collect();
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        (bin.folder.vtable::<Folder2Vtable>().folder.get_ui_object_of)(
            bin.folder.0,
            owner,
            children.len() as u32,
            children.as_ptr(),
            &IID_CONTEXT,
            ptr::null(),
            &mut pointer,
        )
    };
    let context = Com::received(hr, pointer, "get native selection command")?;
    let menu = Menu(unsafe { CreatePopupMenu() });
    if menu.0.is_null() {
        return Err("Cannot allocate Recycle Bin command menu".into());
    }
    check(
        unsafe {
            (context.vtable::<ContextVtable>().query_menu)(
                context.0,
                menu.0,
                0,
                FIRST_COMMAND,
                LAST_COMMAND,
                CMF_NORMAL,
            )
        },
        "initialize native selection command",
    )?;
    let mut lifetime = InvocationLifetime::new()?;
    let (verb, wide) = if properties {
        (c"properties", "properties")
    } else {
        (c"undelete", "undelete")
    };
    let wide: Vec<u16> = wide.encode_utf16().chain(Some(0)).collect();
    let mut info = CMINVOKECOMMANDINFOEX {
        cbSize: mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: SEE_MASK_UNICODE | SEE_MASK_NOASYNC,
        hwnd: owner,
        lpVerb: verb.as_ptr().cast(),
        lpVerbW: wide.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // Only an explicitly named native Recycle Bin verb is invoked. There is no
    // ShellExecute, default/open verb, filesystem fallback, or app dispatcher.
    let hr = unsafe {
        (context.vtable::<ContextVtable>().invoke)(
            context.0,
            (&mut info as *mut CMINVOKECOMMANDINFOEX).cast(),
        )
    };
    lifetime.expect_sheet = properties && hr >= 0;
    // Release context-held references before draining children; retain OLE and
    // the native selection itself until all dialogs/child references finish.
    drop(context);
    drop(menu);
    drop(lifetime);
    check(
        hr,
        if properties {
            "show Properties"
        } else {
            "restore"
        },
    )
}

struct Menu(HMENU);
impl Drop for Menu {
    fn drop(&mut self) {
        unsafe { DestroyMenu(self.0) };
    }
}

fn delete_items(bin: &Bin, selection: &[Pidl], owner: HWND) -> Result<(), String> {
    let children: Vec<*const ITEMIDLIST> =
        selection.iter().map(|pidl| pidl.0.cast_const()).collect();
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        SHCreateShellItemArray(
            bin.root.0,
            bin.folder.0,
            children.len() as u32,
            children.as_ptr(),
            &mut pointer,
        )
    };
    let items = Com::received(hr, pointer, "create native deletion selection")?;
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        CoCreateInstance(
            &FileOperation,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_FILE_OPERATION,
            &mut pointer,
        )
    };
    let operation = Com::received(hr, pointer, "create permanent deletion operation")?;
    let vtable = operation.vtable::<FileOperationVtable>();
    check(
        unsafe { (vtable.set_owner_window)(operation.0, owner) },
        "set deletion owner",
    )?;
    // Explicitly replace IFileOperation's default ALLOWUNDO. Neither ALLOWUNDO
    // nor RECYCLEONDELETE is set. Keep confirmation, progress and errors enabled.
    check(
        unsafe { (vtable.set_operation_flags)(operation.0, FOF_WANTNUKEWARNING) },
        "set permanent deletion flags",
    )?;
    check(
        unsafe { (vtable.delete_items)(operation.0, items.0) },
        "queue permanent deletion",
    )?;
    let hr = unsafe { (vtable.perform_operations)(operation.0) };
    let mut aborted = 0;
    // Microsoft requires this check even when PerformOperations fails or returns
    // success; user cancellation may otherwise be mistaken for full completion.
    let abort_hr = unsafe { (vtable.get_any_operations_aborted)(operation.0, &mut aborted) };
    check(hr, "perform permanent deletion")?;
    check(abort_hr, "read permanent deletion completion")?;
    if aborted != 0 {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

fn check(hr: HRESULT, operation: &str) -> Result<(), String> {
    if hr < 0 {
        Err(failure(operation, hr))
    } else {
        Ok(())
    }
}

fn owner_window(owner: isize) -> Result<HWND, String> {
    let owner = owner as HWND;
    if !owner.is_null() {
        let mut process = 0;
        if unsafe { IsWindow(owner) } == 0
            || unsafe { GetWindowThreadProcessId(owner, &mut process) } == 0
            || process != unsafe { GetCurrentProcessId() }
        {
            return Err("Recycle Bin owner window is no longer a Foldex window".into());
        }
    }
    Ok(owner)
}

fn is_leaf(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn item_bytes(item: &Item) -> usize {
    mem::size_of::<Item>()
        .saturating_add(item.id.len())
        .saturating_add(item.name.len())
        .saturating_add(
            item.original_path
                .as_os_str()
                .as_encoded_bytes()
                .len()
                .saturating_mul(2),
        )
        .saturating_add(item.file_system_path.as_ref().map_or(0, |path| {
            path.as_os_str().as_encoded_bytes().len().saturating_mul(2)
        }))
}

// Shell UI can return before a property sheet or child operation releases its
// COM objects. Keep a registered Shell thread reference and pump this STA until
// those references and native sheets have finished. Never time out live COM.
struct InvocationLifetime {
    count: Box<AtomicI32>,
    registered_references: i32,
    reference: Option<Com>,
    previous: Option<Com>,
    previous_sheets: HashSet<usize>,
    expect_sheet: bool,
}
impl InvocationLifetime {
    fn new() -> Result<Self, String> {
        let mut pointer = ptr::null_mut();
        let hr = unsafe { SHGetThreadRef(&mut pointer) };
        let previous = Com::received(hr, pointer, "read previous thread reference").ok();
        let count = Box::new(AtomicI32::new(0));
        let mut pointer = ptr::null_mut();
        let hr = unsafe { SHCreateThreadRef(count.as_ptr(), &mut pointer) };
        let reference = Com::received(hr, pointer, "create Shell thread reference")?;
        check(
            unsafe { SHSetThreadRef(reference.0) },
            "register Shell thread reference",
        )?;
        let registered_references = count.load(Ordering::Acquire);
        Ok(Self {
            count,
            registered_references,
            reference: Some(reference),
            previous,
            previous_sheets: process_sheets(),
            expect_sheet: false,
        })
    }
}
impl Drop for InvocationLifetime {
    fn drop(&mut self) {
        let startup = Instant::now() + Duration::from_secs(2);
        let mut sheets = HashSet::new();
        let mut observed = false;
        loop {
            if self.expect_sheet {
                for window in process_sheets().difference(&self.previous_sheets) {
                    if sheets.len() < 32 {
                        sheets.insert(*window);
                        observed = true;
                    }
                }
                sheets.retain(|window| unsafe { IsWindow(*window as HWND) } != 0);
            }
            // Keep our registered reference available while pumping: another
            // Shell child can legitimately obtain it during a dialog callback.
            // Only references beyond our registration baseline are children.
            if self.count.load(Ordering::Acquire) <= self.registered_references
                && !thread_has_windows()
                && sheets.is_empty()
                && (!self.expect_sheet || observed || Instant::now() >= startup)
            {
                break;
            }
            pump_messages();
            unsafe {
                MsgWaitForMultipleObjectsEx(0, ptr::null(), 100, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
        }
        unsafe {
            SHSetThreadRef(
                self.previous
                    .as_ref()
                    .map_or(ptr::null_mut(), |value| value.0),
            )
        };
        drop(self.reference.take());
    }
}

fn pump_messages() {
    let mut message = MSG::default();
    for _ in 0..64 {
        if unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 {
            break;
        }
        let root = unsafe { GetAncestor(message.hwnd, GA_ROOT) };
        if root.is_null() || unsafe { IsDialogMessageW(root, &message) } == 0 {
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

fn thread_has_windows() -> bool {
    unsafe extern "system" fn visit(window: HWND, state: LPARAM) -> i32 {
        if unsafe { IsWindowVisible(window) } != 0 {
            unsafe { *(state as *mut bool) = true };
            return 0;
        }
        1
    }
    let mut found = false;
    unsafe {
        EnumThreadWindows(
            GetCurrentThreadId(),
            Some(visit),
            (&mut found as *mut bool) as LPARAM,
        )
    };
    found
}

fn process_sheets() -> HashSet<usize> {
    unsafe extern "system" fn visit(window: HWND, state: LPARAM) -> i32 {
        let result = unsafe { &mut *(state as *mut HashSet<usize>) };
        let mut process = 0;
        unsafe { GetWindowThreadProcessId(window, &mut process) };
        if process != unsafe { GetCurrentProcessId() } || unsafe { IsWindowVisible(window) } == 0 {
            return 1;
        }
        let mut class = [0u16; 16];
        let length = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        if length == 6
            && class[..6] == [35, 51, 50, 55, 55, 48]
            && !unsafe {
                FindWindowExW(window, ptr::null_mut(), w_tab_class().as_ptr(), ptr::null())
            }
            .is_null()
            && result.len() < 32
        {
            result.insert(window as usize);
        }
        1
    }
    let mut result = HashSet::new();
    unsafe { EnumWindows(Some(visit), (&mut result as *mut HashSet<usize>) as LPARAM) };
    result
}

fn w_tab_class() -> &'static [u16] {
    &[
        83, 121, 115, 84, 97, 98, 67, 111, 110, 116, 114, 111, 108, 51, 50, 0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_reject_extra_items_and_bytes_without_truncation() {
        let mut budget = Budget {
            items: MAX_ITEMS,
            bytes: 0,
        };
        assert!(budget.add(1).is_err());
        let mut budget = Budget {
            items: 0,
            bytes: MAX_SNAPSHOT_BYTES,
        };
        assert!(budget.add(1).is_err());
        assert!(Budget::default().add(usize::MAX).is_err());
    }

    #[test]
    fn original_filename_must_be_one_normal_component() {
        for name in ["file.txt", "資料.zip", "$Roriginal.txt"] {
            assert!(is_leaf(Path::new(name)));
        }
        for name in [
            "",
            ".",
            "..",
            "C:\\file.txt",
            "\\file.txt",
            "folder\\file.txt",
        ] {
            assert!(!is_leaf(Path::new(name)));
        }
    }

    /// Explicitly gated, read-only diagnostics. This never queries/invokes a
    /// context menu, file operation, restore, delete, or Empty Bin operation.
    #[test]
    #[ignore = "opt-in native read-only enumeration; set FOLDEX_NATIVE_RECYCLE_READONLY_PROBE=1"]
    fn native_readonly_metadata_probe() {
        assert_eq!(
            std::env::var("FOLDEX_NATIVE_RECYCLE_READONLY_PROBE").as_deref(),
            Ok("1")
        );
        let _apartment = Apartment::new().unwrap();
        let bin = Bin::open().unwrap();
        let mut items = Vec::new();
        let mut original_locations = 0;
        let mut deletion_times = 0;
        let mut sizes = 0;
        let mut full_names = 0;
        let mut backing_name_substitutions = 0;
        bin.visit(&AtomicBool::new(false), |child, absolute, id| {
            let item = bin.item(&child, &absolute, id, &AtomicBool::new(false))?;
            assert!(
                validate_pidl(&item.id).is_ok(),
                "invalid native identity framing"
            );
            assert!(!item.name.is_empty(), "native item has no display name");
            original_locations += usize::from(!item.original_path.as_os_str().is_empty());
            deletion_times += usize::from(
                bin.detail(&child, &DATE_DELETED)
                    .and_then(|value| value.deleted_at())
                    .is_ok(),
            );
            sizes += usize::from(
                bin.detail(&child, &SIZE)
                    .and_then(|value| value.number())
                    .is_ok(),
            );
            full_names += usize::from(
                bin.detail(&child, &FILE_NAME)
                    .and_then(|value| value.text())
                    .is_ok(),
            );
            if let Some(backing_name) = item
                .file_system_path
                .as_ref()
                .and_then(|path| path.file_name())
            {
                if backing_name != std::ffi::OsStr::new(&item.name)
                    && item.original_path.file_name() == Some(backing_name)
                {
                    backing_name_substitutions += 1;
                }
            }
            items.push(item);
            Ok(items.len() < 3)
        })
        .unwrap();
        let ids: Vec<_> = items.iter().map(|item| item.id.clone()).collect();
        if !ids.is_empty() {
            // A fresh enumeration must resolve the same native identities.
            // No incoming byte buffer is converted to a native pointer.
            let resolved = Bin::open().unwrap().resolve(&ids).unwrap();
            assert!(
                resolved.len() == ids.len(),
                "native identities did not remain stable"
            );
        }
        assert!(
            backing_name_substitutions == 0,
            "native filename metadata used a backing filename instead of the original name"
        );
        assert!(check_cancelled(&AtomicBool::new(true)).is_err());
        eprintln!(
            "Read-only Recycle Bin probe: sampled={}, stable_identities={}, original_paths={}, deletion_times={}, sizes={}, full_names={}; no operations invoked",
            items.len(),
            ids.len(),
            original_locations,
            deletion_times,
            sizes,
            full_names
        );
    }
}
