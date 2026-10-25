use super::{
    Backend, MAX_DEPTH, MAX_ITEMS, MAX_TEXT_BYTES, Open, ShellItem, Snapshot, split_label,
};
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    mem,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    sync::atomic::{AtomicI32, Ordering},
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        Globalization::{CP_ACP, MultiByteToWideChar},
        System::{
            Com::{
                COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
                DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
            },
            DataExchange::RegisterClipboardFormatW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Threading::{GetCurrentProcessId, GetCurrentThreadId},
        },
        UI::{
            Shell::{
                CMF_CANRENAME, CMF_EXTENDEDVERBS, CMF_ITEMMENU, CMINVOKECOMMANDINFO,
                CMINVOKECOMMANDINFOEX, Common::ITEMIDLIST, DROPFILES, GCS_HELPTEXTA, GCS_HELPTEXTW,
                GCS_VERBA, GCS_VERBW, ILClone, ILFindLastID, ILGetSize, ILIsEqual, ILRemoveLastID,
                SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SEE_MASK_UNICODE,
                SHBindToParent, SHCreateDataObject, SHCreateThreadRef, SHELLEXECUTEINFOW,
                SHGetDesktopFolder, SHMultiFileProperties, SHOP_FILEPATH, SHOP_VOLUMEGUID,
                SHObjectProperties, SHParseDisplayName, SHSetThreadRef, ShellExecuteExW,
            },
            WindowsAndMessaging::{
                CreatePopupMenu, DestroyMenu, DispatchMessageW, EnumThreadWindows, EnumWindows,
                FindWindowExW, GA_ROOT, GetAncestor, GetClassNameW, GetMenuItemCount,
                GetMenuItemInfoW, GetWindowThreadProcessId, HMENU, IsDialogMessageW, IsWindow,
                IsWindowVisible, MENUITEMINFOW, MFS_CHECKED, MFS_DEFAULT, MFS_DISABLED,
                MFT_OWNERDRAW, MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING,
                MIIM_SUBMENU, MSG, MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE,
                PeekMessageW, QS_ALLINPUT, SW_SHOWNORMAL, TranslateMessage, WM_INITMENUPOPUP,
            },
        },
    },
    core::{GUID, HRESULT},
};

const COMMAND_FIRST: u32 = 1;
const COMMAND_LAST: u32 = 0x7fff;
const MAX_STRING_UNITS: usize = 512;

#[path = "windows/builtin.rs"]
pub(super) mod builtin;

pub(super) struct Native {
    // Session must be destroyed before CoUninitialize (declaration drop order).
    session: Option<Session>,
    apartment: Result<Apartment, String>,
}

impl Native {
    pub(super) fn new() -> Self {
        Self {
            session: None,
            apartment: Apartment::new(),
        }
    }
}

impl Backend for Native {
    fn open(&mut self, open: &Open) -> Result<Snapshot, String> {
        self.apartment.as_ref().map_err(Clone::clone)?;
        let mut session = Session::new(open)?;
        let snapshot = session.snapshot()?;
        self.session = Some(session);
        Ok(snapshot)
    }
    fn expand(&mut self, item_id: u32) -> Result<Snapshot, String> {
        self.session
            .as_mut()
            .ok_or_else(super::stale)?
            .expand(item_id)
    }
    fn invoke(&mut self, item_id: u32) -> Result<(), String> {
        self.session
            .as_ref()
            .ok_or_else(super::stale)?
            .invoke(item_id)
    }
    fn close(&mut self) {
        self.session = None;
    }
    fn pump(&mut self) {
        let mut message = MSG::default();
        for _ in 0..32 {
            if unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 {
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if hr < 0 {
            Err(failure("COM initialization", hr))
        } else {
            Ok(Self)
        }
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

// Layouts verified against the pinned SDK 10.0.26100.0 ShObjIdl_core.h.
// Uncalled IShellFolder slots remain pointer-sized entries at their exact ABI
// positions; all called slots have the SDK's full parameter types.
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
    enum_objects: usize,
    bind_to_object: unsafe extern "system" fn(
        *mut c_void,
        *const ITEMIDLIST,
        *mut c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    bind_to_storage: usize,
    compare_ids: usize,
    create_view_object:
        unsafe extern "system" fn(*mut c_void, HWND, *const GUID, *mut *mut c_void) -> HRESULT,
    get_attributes_of: usize,
    get_ui_object_of: unsafe extern "system" fn(
        *mut c_void,
        HWND,
        u32,
        *const *const ITEMIDLIST,
        *const GUID,
        *mut u32,
        *mut *mut c_void,
    ) -> HRESULT,
    get_display_name_of: usize,
    set_name_of: usize,
}
#[repr(C)]
struct ContextVtable {
    unknown: UnknownVtable,
    query_menu: unsafe extern "system" fn(*mut c_void, HMENU, u32, u32, u32, u32) -> HRESULT,
    invoke: unsafe extern "system" fn(*mut c_void, *mut CMINVOKECOMMANDINFO) -> HRESULT,
    command_string:
        unsafe extern "system" fn(*mut c_void, usize, u32, *mut u32, *mut u8, u32) -> HRESULT,
}
#[repr(C)]
struct Context2Vtable {
    context: ContextVtable,
    handle_message: unsafe extern "system" fn(*mut c_void, u32, WPARAM, LPARAM) -> HRESULT,
}
#[repr(C)]
struct Context3Vtable {
    context2: Context2Vtable,
    handle_message2:
        unsafe extern "system" fn(*mut c_void, u32, WPARAM, LPARAM, *mut LRESULT) -> HRESULT,
}

const IID_DATA_OBJECT: GUID = GUID {
    data1: 0x0000010e,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};

#[repr(C)]
struct DataObjectVtable {
    unknown: UnknownVtable,
    get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
    get_data_here: usize,
    query_get_data: usize,
    canonical_format: usize,
    set_data:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, *const STGMEDIUM, i32) -> HRESULT,
}

fn properties_data(owner: HWND, paths: &[std::path::PathBuf]) -> Result<Com, String> {
    let pidls = paths
        .iter()
        .map(|path| Pidl::for_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    if !same_parent(&pidls)? {
        return absolute_selection_data(&pidls, paths);
    }
    // Ask the real parent folder for the selection's IDataObject. This is the
    // Shell contract for a set of siblings, and retains its standard formats
    // and property-page handlers without enumerating a context menu.
    selection_ui_object(owner, &pidls, &IID_DATA_OBJECT, "Properties data object")
        .or_else(|_| absolute_selection_data(&pidls, paths))
}

fn selection_ui_object(
    owner: HWND,
    pidls: &[Pidl],
    iid: &GUID,
    operation: &str,
) -> Result<Com, String> {
    let first = pidls.first().ok_or("Properties requires a selection")?;
    let mut parent_pointer = ptr::null_mut();
    let mut first_child = ptr::null_mut();
    let hr = unsafe { SHBindToParent(first.0, &IID_FOLDER, &mut parent_pointer, &mut first_child) };
    let folder = Com::received(hr, parent_pointer, "Properties parent folder")?;
    let children: Vec<*const ITEMIDLIST> = pidls
        .iter()
        .map(|pidl| unsafe { ILFindLastID(pidl.0) as *const _ })
        .collect();
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        (folder.vtable::<FolderVtable>().get_ui_object_of)(
            folder.0,
            owner,
            children.len() as u32,
            children.as_ptr(),
            iid,
            ptr::null_mut(),
            &mut pointer,
        )
    };
    Com::received(hr, pointer, operation)
}

fn absolute_selection_data(pidls: &[Pidl], paths: &[std::path::PathBuf]) -> Result<Com, String> {
    // CFSTR_SHELLIDLIST supports a Desktop parent and absolute item identifiers.
    // Build this format explicitly for search selections across parent folders;
    // GetUIObjectOf requires sibling CHILD identifiers instead.
    let header = (pidls.len() + 2) * mem::size_of::<u32>();
    let sizes: Vec<_> = pidls
        .iter()
        .map(|pidl| unsafe { ILGetSize(pidl.0) as usize })
        .collect();
    let size = sizes
        .iter()
        .try_fold(header + 2, |sum, size| sum.checked_add(*size))
        .filter(|size| *size <= 1024 * 1024)
        .ok_or("Properties selection identifiers exceed 1 MiB")?;
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        SHCreateDataObject(
            ptr::null(),
            0,
            ptr::null(),
            ptr::null_mut(),
            &IID_DATA_OBJECT,
            &mut pointer,
        )
    };
    let data = Com::received(hr, pointer, "Properties selection data")?;
    let format_name: Vec<_> = "Shell IDList Array".encode_utf16().chain(Some(0)).collect();
    let format = unsafe { RegisterClipboardFormatW(format_name.as_ptr()) };
    if format == 0 {
        return Err("Cannot register Shell selection format".into());
    }
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, size) };
    if memory.is_null() {
        return Err("Cannot allocate Properties selection".into());
    }
    let bytes = unsafe { GlobalLock(memory) } as *mut u8;
    if bytes.is_null() {
        unsafe { windows_sys::Win32::Foundation::GlobalFree(memory) };
        return Err("Cannot access Properties selection".into());
    }
    unsafe {
        ptr::write_bytes(bytes, 0, size);
        ptr::write_unaligned(bytes.cast::<u32>(), pidls.len() as u32);
        ptr::write_unaligned(bytes.add(4).cast::<u32>(), header as u32);
        let mut offset = header + 2;
        for (index, (pidl, size)) in pidls.iter().zip(sizes).enumerate() {
            ptr::write_unaligned(bytes.add((index + 2) * 4).cast::<u32>(), offset as u32);
            ptr::copy_nonoverlapping(pidl.0.cast::<u8>(), bytes.add(offset), size);
            offset += size;
        }
        GlobalUnlock(memory);
    }
    set_properties_format(&data, format as u16, memory)?;
    // Shell property-page extensions may also request filesystem paths. An
    // empty SHCreateDataObject cannot synthesize CF_HDROP from overridden CIDA.
    // Supply both representations of exactly the same selection.
    let names: Vec<u16> = paths
        .iter()
        .flat_map(|path| parsing_name(path))
        .chain(Some(0))
        .collect();
    let header = mem::size_of::<DROPFILES>();
    let size = names
        .len()
        .checked_mul(2)
        .and_then(|size| size.checked_add(header))
        .filter(|size| *size <= 1024 * 1024)
        .ok_or("Properties selection paths exceed 1 MiB")?;
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, size) };
    if memory.is_null() {
        return Err("Cannot allocate Properties paths".into());
    }
    let bytes = unsafe { GlobalLock(memory) }.cast::<u8>();
    if bytes.is_null() {
        unsafe { windows_sys::Win32::Foundation::GlobalFree(memory) };
        return Err("Cannot access Properties paths".into());
    }
    unsafe {
        ptr::write_unaligned(
            bytes.cast::<DROPFILES>(),
            DROPFILES {
                pFiles: header as u32,
                fWide: 1,
                ..Default::default()
            },
        );
        ptr::copy_nonoverlapping(
            names.as_ptr().cast::<u8>(),
            bytes.add(header),
            names.len() * 2,
        );
        GlobalUnlock(memory);
    }
    set_properties_format(&data, windows_sys::Win32::System::Ole::CF_HDROP, memory)?;
    Ok(data)
}

fn set_properties_format(
    data: &Com,
    format: u16,
    memory: windows_sys::Win32::Foundation::HGLOBAL,
) -> Result<(), String> {
    let format = FORMATETC {
        cfFormat: format,
        ptd: ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed: TYMED_HGLOBAL as u32,
    };
    let medium = STGMEDIUM {
        tymed: TYMED_HGLOBAL as u32,
        u: STGMEDIUM_0 { hGlobal: memory },
        pUnkForRelease: ptr::null_mut(),
    };
    let hr = unsafe { (data.vtable::<DataObjectVtable>().set_data)(data.0, &format, &medium, 1) };
    if hr < 0 {
        unsafe { windows_sys::Win32::Foundation::GlobalFree(memory) };
        return Err(failure("Properties selection format", hr));
    }
    // SetData(TRUE) owns the HGLOBAL. Format registration and this data object
    // do not open or modify the user's system clipboard.
    Ok(())
}

pub(super) fn properties(owner: isize, paths: &[std::path::PathBuf]) -> Result<(), String> {
    let _apartment = Apartment::new()?;
    let owner = owner as HWND;
    if !owner.is_null() && unsafe { IsWindow(owner) } == 0 {
        return Err("Properties owner window no longer exists".into());
    }
    // Some native handlers return before their property sheet closes. Register
    // a Shell thread reference and keep the apartment pumping until those
    // handlers release it; never dismantle COM immediately after activation.
    if let [path] = paths {
        let mut lifetime = PropertiesThread::new()?;
        let result = single_properties(owner, path);
        lifetime.activated = result.is_ok();
        return result;
    }
    let data = properties_data(owner, paths)?;
    let mut lifetime = PropertiesThread::new()?;
    // This native property sheet can load property-page extensions, but never
    // constructs a menu or waits for the retained context-menu STA.
    let hr = unsafe { SHMultiFileProperties(data.0, 0) };
    if hr >= 0 {
        lifetime.activated = true;
        return Ok(());
    }
    // A namespace can implement the canonical Properties command even if its
    // data object does not support the merged-sheet implementation. Invoke it
    // directly, with no QueryContextMenu or dependence on the More actions STA.
    let pidls = paths
        .iter()
        .map(|path| Pidl::for_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    if same_parent(&pidls)? {
        let result = selection_properties(owner, &pidls)
            .map_err(|fallback| format!("{}; {fallback}", failure("open merged Properties", hr)));
        lifetime.activated = result.is_ok();
        return result;
    }
    Err(failure("open merged Properties", hr))
}

fn single_properties(owner: HWND, path: &Path) -> Result<(), String> {
    let pidl = Pidl::for_path(path)?;
    let name = parsing_name(path);
    let verb: Vec<u16> = "properties".encode_utf16().chain(Some(0)).collect();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC | SEE_MASK_UNICODE | SEE_MASK_FLAG_NO_UI,
        hwnd: owner,
        lpVerb: verb.as_ptr(),
        lpIDList: pidl.0.cast(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    if unsafe { ShellExecuteExW(&mut info) } != 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    // SHObjectProperties is a Windows-provided fallback for files, directories
    // and drive roots. Drive roots use the dedicated volume object contract.
    if unsafe {
        SHObjectProperties(
            owner,
            properties_object_type(path),
            name.as_ptr(),
            ptr::null(),
        )
    } != 0
    {
        return Ok(());
    }
    Err(format!(
        "Cannot open Windows Properties for {}: {error}",
        path.display()
    ))
}

fn properties_object_type(path: &Path) -> u32 {
    use std::path::{Component, Prefix};
    let mut components = path.components();
    let volume = matches!(components.next(), Some(Component::Prefix(prefix))
        if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)));
    if volume
        && matches!(components.next(), Some(Component::RootDir))
        && components.next().is_none()
    {
        SHOP_VOLUMEGUID as u32
    } else {
        SHOP_FILEPATH as u32
    }
}

fn selection_properties(owner: HWND, pidls: &[Pidl]) -> Result<(), String> {
    let context = selection_ui_object(owner, pidls, &IID_CONTEXT, "Properties command")?;
    let verb: Vec<u16> = "properties".encode_utf16().chain(Some(0)).collect();
    let mut info = CMINVOKECOMMANDINFOEX {
        cbSize: mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        fMask: SEE_MASK_UNICODE | SEE_MASK_NOASYNC,
        hwnd: owner,
        lpVerb: c"properties".as_ptr().cast(),
        lpVerbW: verb.as_ptr(),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    let hr = unsafe {
        (context.vtable::<ContextVtable>().invoke)(
            context.0,
            (&mut info as *mut CMINVOKECOMMANDINFOEX).cast(),
        )
    };
    if hr < 0 {
        Err(failure("invoke Properties command", hr))
    } else {
        Ok(())
    }
}

struct PropertiesThread {
    count: Box<AtomicI32>,
    reference: Option<Com>,
    previous_sheets: HashSet<usize>,
    activated: bool,
}

impl PropertiesThread {
    fn new() -> Result<Self, String> {
        // Stable aligned storage: SHCreateThreadRef uses Windows interlocked
        // operations on this LONG, including releases on other Shell threads.
        let count = Box::new(AtomicI32::new(0));
        let mut pointer = ptr::null_mut();
        let hr = unsafe { SHCreateThreadRef(count.as_ptr(), &mut pointer) };
        let reference = Com::received(hr, pointer, "Properties thread reference")?;
        let hr = unsafe { SHSetThreadRef(reference.0) };
        if hr < 0 {
            return Err(failure("Properties thread registration", hr));
        }
        Ok(Self {
            count,
            reference: Some(reference),
            previous_sheets: properties_process_sheets(),
            activated: false,
        })
    }
}

impl Drop for PropertiesThread {
    fn drop(&mut self) {
        unsafe { SHSetThreadRef(ptr::null_mut()) };
        drop(self.reference.take());
        let startup = Instant::now() + Duration::from_secs(2);
        let mut tracked = HashSet::new();
        let mut observed = false;
        loop {
            if self.activated {
                // Shell can host a sheet on another thread without retaining
                // our thread reference. Keep the one-request lane occupied for
                // those sheets too, and wait for destruction after WM_CLOSE.
                for window in properties_process_sheets().difference(&self.previous_sheets) {
                    if tracked.len() < 32 {
                        tracked.insert(*window);
                        observed = true;
                    }
                }
                tracked.retain(|window| unsafe { IsWindow(*window as HWND) } != 0);
            }
            if self.count.load(Ordering::Acquire) == 0
                && !properties_thread_has_windows()
                && tracked.is_empty()
                && (!self.activated || observed || Instant::now() >= startup)
            {
                break;
            }
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
            // Shell reference releases need not post a message. A short wait
            // rechecks them while servicing STA and modeless-dialog messages.
            // This worker exists only while a Properties request is active.
            unsafe {
                MsgWaitForMultipleObjectsEx(0, ptr::null(), 250, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
            }
        }
    }
}

fn properties_process_sheets() -> HashSet<usize> {
    unsafe extern "system" fn visit(window: HWND, state: LPARAM) -> i32 {
        let windows = unsafe { &mut *(state as *mut HashSet<usize>) };
        let mut process = 0;
        unsafe { GetWindowThreadProcessId(window, &mut process) };
        if process != unsafe { GetCurrentProcessId() } || unsafe { IsWindowVisible(window) } == 0 {
            return 1;
        }
        let mut class = [0u16; 16];
        let length = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        if length != 6 || class[..6] != [35, 51, 50, 55, 55, 48] {
            return 1;
        }
        let tab: Vec<u16> = "SysTabControl32".encode_utf16().chain(Some(0)).collect();
        if !unsafe { FindWindowExW(window, ptr::null_mut(), tab.as_ptr(), ptr::null()) }.is_null()
            && windows.len() < 32
        {
            windows.insert(window as usize);
        }
        1
    }
    let mut windows = HashSet::new();
    unsafe { EnumWindows(Some(visit), (&mut windows as *mut HashSet<usize>) as LPARAM) };
    windows
}

fn properties_thread_has_windows() -> bool {
    unsafe extern "system" fn visible(window: HWND, state: LPARAM) -> i32 {
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
            Some(visible),
            (&mut found as *mut bool) as LPARAM,
        )
    };
    found
}

const IID_FOLDER: GUID = GUID {
    data1: 0x000214e6,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_CONTEXT: GUID = GUID {
    data1: 0x000214e4,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_CONTEXT2: GUID = GUID {
    data1: 0x000214f4,
    data2: 0,
    data3: 0,
    data4: [0xc0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_CONTEXT3: GUID = GUID {
    data1: 0xbcfce0a0,
    data2: 0xec17,
    data3: 0x11d0,
    data4: [0x8d, 0x10, 0, 0xa0, 0xc9, 0x0f, 0x27, 0x19],
};

struct Com(*mut c_void);
impl Com {
    fn received(hr: HRESULT, pointer: *mut c_void, operation: &str) -> Result<Self, String> {
        if pointer.is_null() {
            return Err(failure(operation, hr));
        }
        let object = Self(pointer);
        if hr < 0 {
            Err(failure(operation, hr))
        } else {
            Ok(object)
        }
    }
    fn vtable<T>(&self) -> &T {
        unsafe { &**(self.0.cast::<*const T>()) }
    }
    fn query(&self, iid: &GUID) -> Option<Self> {
        let mut pointer = ptr::null_mut();
        let hr = unsafe { (self.vtable::<UnknownVtable>().query)(self.0, iid, &mut pointer) };
        Self::received(hr, pointer, "context interface").ok()
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { (self.vtable::<UnknownVtable>().release)(self.0) };
    }
}

struct Pidl(*mut ITEMIDLIST);
impl Pidl {
    fn for_path(path: &Path) -> Result<Self, String> {
        let name = parsing_name(path);
        let mut pointer = ptr::null_mut();
        let hr = unsafe {
            SHParseDisplayName(
                name.as_ptr(),
                ptr::null_mut(),
                &mut pointer,
                0,
                ptr::null_mut(),
            )
        };
        let result = Self(pointer);
        if hr < 0 || pointer.is_null() {
            Err(failure("parse shell path", hr))
        } else {
            Ok(result)
        }
    }
}
impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(self.0.cast()) };
        }
    }
}

fn same_parent(pidls: &[Pidl]) -> Result<bool, String> {
    if pidls.len() < 2 {
        return Ok(true);
    }
    fn parent(pidl: &Pidl) -> Result<Pidl, String> {
        let result = Pidl(unsafe { ILClone(pidl.0) });
        if result.0.is_null() || unsafe { ILRemoveLastID(result.0) } == 0 {
            return Err("Cannot identify the selection's shell parent".into());
        }
        Ok(result)
    }
    let first = parent(&pidls[0])?;
    for pidl in &pidls[1..] {
        let next = parent(pidl)?;
        if unsafe { ILIsEqual(first.0, next.0) } == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Menu(HMENU);
impl Menu {
    fn new() -> Result<Self, String> {
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            Err(format!(
                "create native menu: {}",
                std::io::Error::last_os_error()
            ))
        } else {
            Ok(Self(menu))
        }
    }
}
impl Drop for Menu {
    fn drop(&mut self) {
        unsafe { DestroyMenu(self.0) };
    }
}

#[derive(Clone, Copy)]
struct Node {
    native_id: Option<u32>,
    submenu: HMENU,
    parent: HMENU,
    position: u32,
    depth: usize,
    enabled: bool,
}
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum Identity {
    Command(usize, u32),
    Submenu(usize),
    Position(usize, u32),
}

struct Session {
    menu: Menu,
    context3: Option<Com>,
    context2: Option<Com>,
    context: Com,
    _folder: Com,
    _pidls: Vec<Pidl>,
    owner: HWND,
    directory: Vec<u16>,
    command_end: u32,
    identities: HashMap<Identity, u32>,
    next_id: u32,
    nodes: HashMap<u32, Node>,
    expanded: HashSet<u32>,
}

impl Session {
    fn new(open: &Open) -> Result<Self, String> {
        let owner = open.owner as HWND;
        if !owner.is_null() && unsafe { IsWindow(owner) } == 0 {
            return Err("shell menu owner window no longer exists".into());
        }
        let mut desktop_pointer = ptr::null_mut();
        let hr = unsafe { SHGetDesktopFolder(&mut desktop_pointer) };
        let desktop = Com::received(hr, desktop_pointer, "desktop shell folder")?;
        let pidls: Vec<_> = open
            .paths
            .iter()
            .map(|path| Pidl::for_path(path))
            .collect::<Result<_, _>>()?;
        let (folder, context) = if open.background {
            let mut folder_pointer = ptr::null_mut();
            let hr = unsafe {
                (desktop.vtable::<FolderVtable>().bind_to_object)(
                    desktop.0,
                    pidls[0].0,
                    ptr::null_mut(),
                    &IID_FOLDER,
                    &mut folder_pointer,
                )
            };
            let folder = Com::received(hr, folder_pointer, "background shell folder")?;
            let mut context_pointer = ptr::null_mut();
            let hr = unsafe {
                (folder.vtable::<FolderVtable>().create_view_object)(
                    folder.0,
                    owner,
                    &IID_CONTEXT,
                    &mut context_pointer,
                )
            };
            let context = Com::received(hr, context_pointer, "background context menu")?;
            (folder, context)
        } else {
            // GetUIObjectOf requires one-level CHILD PIDLs from the actual
            // parent. Passing absolute PIDLs to Desktop can succeed while
            // returning a higher-level namespace menu instead of the file's menu.
            if !same_parent(&pidls)? {
                return Err("Windows app actions require items from the same folder".into());
            }
            let mut parent_pointer = ptr::null_mut();
            let mut first_child = ptr::null_mut();
            let hr = unsafe {
                SHBindToParent(
                    pidls[0].0,
                    &IID_FOLDER,
                    &mut parent_pointer,
                    &mut first_child,
                )
            };
            let folder = Com::received(hr, parent_pointer, "selection parent folder")?;
            let children: Vec<*const ITEMIDLIST> = pidls
                .iter()
                .map(|pidl| unsafe { ILFindLastID(pidl.0) as *const _ })
                .collect();
            let mut context_pointer = ptr::null_mut();
            let hr = unsafe {
                (folder.vtable::<FolderVtable>().get_ui_object_of)(
                    folder.0,
                    owner,
                    children.len() as u32,
                    children.as_ptr(),
                    &IID_CONTEXT,
                    ptr::null_mut(),
                    &mut context_pointer,
                )
            };
            let context = Com::received(hr, context_pointer, "selection context menu")?;
            (folder, context)
        };
        let context3 = context.query(&IID_CONTEXT3);
        let context2 = if context3.is_none() {
            context.query(&IID_CONTEXT2)
        } else {
            None
        };
        let menu = Menu::new()?;
        let flags = CMF_CANRENAME
            | if open.background { 0 } else { CMF_ITEMMENU }
            | if open.options.extended {
                CMF_EXTENDEDVERBS
            } else {
                0
            };
        let hr = unsafe {
            (context.vtable::<ContextVtable>().query_menu)(
                context.0,
                menu.0,
                0,
                COMMAND_FIRST,
                COMMAND_LAST,
                flags,
            )
        };
        if hr < 0 {
            return Err(failure("enumerate shell menu", hr));
        }
        let count = (hr as u32) & 0xffff;
        let directory = if open.background {
            open.paths[0].as_path()
        } else {
            open.paths[0].parent().unwrap_or(&open.paths[0])
        };
        Ok(Self {
            menu,
            context3,
            context2,
            context,
            _folder: folder,
            _pidls: pidls,
            owner,
            directory: parsing_name(directory),
            command_end: COMMAND_FIRST.saturating_add(count).min(COMMAND_LAST + 1),
            identities: HashMap::new(),
            next_id: 1,
            nodes: HashMap::new(),
            expanded: HashSet::new(),
        })
    }

    fn snapshot(&mut self) -> Result<Snapshot, String> {
        self.nodes.clear();
        let mut limits = Limits {
            remaining_text: MAX_TEXT_BYTES,
            count: 0,
            truncated: false,
            visited: HashSet::new(),
        };
        let items = self.read_level(self.menu.0, 0, &mut limits)?;
        Ok(Snapshot {
            items,
            truncated: limits.truncated,
        })
    }

    fn read_level(
        &mut self,
        menu: HMENU,
        depth: usize,
        limits: &mut Limits,
    ) -> Result<Vec<ShellItem>, String> {
        if depth >= MAX_DEPTH || !limits.visited.insert(menu as usize) {
            limits.truncated = true;
            return Ok(Vec::new());
        }
        let count = unsafe { GetMenuItemCount(menu) };
        if count < 0 {
            return Err("native submenu no longer exists".into());
        }
        if count as usize > MAX_ITEMS {
            limits.truncated = true;
        }
        let mut items = Vec::with_capacity((count as usize).min(MAX_ITEMS));
        for position in 0..(count as u32).min(MAX_ITEMS as u32) {
            if limits.count == MAX_ITEMS {
                limits.truncated = true;
                break;
            }
            let mut info = MENUITEMINFOW {
                cbSize: mem::size_of::<MENUITEMINFOW>() as u32,
                fMask: MIIM_FTYPE | MIIM_ID | MIIM_STATE | MIIM_STRING | MIIM_SUBMENU,
                ..Default::default()
            };
            if unsafe { GetMenuItemInfoW(menu, position, 1, &mut info) } == 0 {
                continue;
            }
            let declared_length = info.cch as usize;
            if declared_length > MAX_STRING_UNITS {
                limits.truncated = true;
            }
            let mut text = vec![0u16; declared_length.min(MAX_STRING_UNITS) + 1];
            info.dwTypeData = text.as_mut_ptr();
            info.cch = text.len() as u32;
            if unsafe { GetMenuItemInfoW(menu, position, 1, &mut info) } == 0 {
                continue;
            }
            let separator = info.fType & MFT_SEPARATOR != 0;
            let native_id =
                (info.wID >= COMMAND_FIRST && info.wID < self.command_end).then_some(info.wID);
            let identity = if !info.hSubMenu.is_null() {
                Identity::Submenu(info.hSubMenu as usize)
            } else if let Some(command) = native_id {
                Identity::Command(menu as usize, command)
            } else {
                Identity::Position(menu as usize, position)
            };
            let id = if let Some(id) = self.identities.get(&identity) {
                *id
            } else {
                if self.identities.len() >= MAX_ITEMS * 2 {
                    limits.truncated = true;
                    break;
                }
                let id = self.next_id;
                self.next_id += 1;
                self.identities.insert(identity, id);
                id
            };
            let raw = String::from_utf16_lossy(
                &text[..text
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(text.len())],
            );
            let (label, shortcut) = split_label(&raw);
            let mut label = limits.text(label);
            let shortcut = shortcut
                .map(|text| limits.text(text))
                .filter(|text| !text.is_empty());
            let canonical = if !separator && info.hSubMenu.is_null() && limits.remaining_text > 64 {
                native_id
                    .and_then(|command| self.command_string(command, GCS_VERBW, GCS_VERBA))
                    .map(|text| limits.text(text))
                    .filter(|text| !text.is_empty())
            } else {
                None
            };
            let help = if !separator
                && info.hSubMenu.is_null()
                && limits.remaining_text > MAX_STRING_UNITS
            {
                native_id
                    .and_then(|command| self.command_string(command, GCS_HELPTEXTW, GCS_HELPTEXTA))
                    .map(|text| limits.text(text))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            if label.is_empty() && !separator {
                if let Some(verb) = &canonical {
                    label = limits.text(verb.clone());
                }
            }
            let enabled = info.fState & MFS_DISABLED == 0
                && !separator
                && !label.is_empty()
                && (!info.hSubMenu.is_null() || native_id.is_some());
            limits.count += 1;
            self.nodes.insert(
                id,
                Node {
                    native_id,
                    submenu: info.hSubMenu,
                    parent: menu,
                    position,
                    depth,
                    enabled,
                },
            );
            let children = if info.hSubMenu.is_null() {
                Vec::new()
            } else {
                self.read_level(info.hSubMenu, depth + 1, limits)?
            };
            items.push(ShellItem {
                id,
                label,
                canonical_verb: canonical,
                help,
                enabled,
                separator,
                children,
                has_submenu: !info.hSubMenu.is_null(),
                shortcut,
                checked: info.fState & MFS_CHECKED != 0,
                default_item: info.fState & MFS_DEFAULT != 0,
                owner_draw: info.fType & MFT_OWNERDRAW != 0,
            });
        }
        Ok(items)
    }

    fn command_string(&self, command: u32, wide_type: u32, ansi_type: u32) -> Option<String> {
        let mut wide = [0u16; MAX_STRING_UNITS + 1];
        let hr = unsafe {
            (self.context.vtable::<ContextVtable>().command_string)(
                self.context.0,
                (command - COMMAND_FIRST) as usize,
                wide_type,
                ptr::null_mut(),
                wide.as_mut_ptr().cast(),
                MAX_STRING_UNITS as u32,
            )
        };
        let end = wide
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(MAX_STRING_UNITS);
        if hr >= 0 && end > 0 {
            return Some(String::from_utf16_lossy(&wide[..end]));
        }
        let mut ansi = [0u8; MAX_STRING_UNITS + 1];
        let hr = unsafe {
            (self.context.vtable::<ContextVtable>().command_string)(
                self.context.0,
                (command - COMMAND_FIRST) as usize,
                ansi_type,
                ptr::null_mut(),
                ansi.as_mut_ptr(),
                MAX_STRING_UNITS as u32,
            )
        };
        let end = ansi
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(MAX_STRING_UNITS);
        if hr < 0 || end == 0 {
            return None;
        }
        let written = unsafe {
            MultiByteToWideChar(
                CP_ACP,
                0,
                ansi.as_ptr(),
                end as i32,
                wide.as_mut_ptr(),
                MAX_STRING_UNITS as i32,
            )
        };
        (written > 0).then(|| String::from_utf16_lossy(&wide[..written as usize]))
    }

    fn expand(&mut self, item_id: u32) -> Result<Snapshot, String> {
        let node = self.nodes.get(&item_id).copied().ok_or_else(super::stale)?;
        if node.submenu.is_null() {
            return Err("item has no native submenu".into());
        }
        if node.depth + 1 >= MAX_DEPTH {
            return Err("native submenu depth limit reached".into());
        }
        if self.expanded.insert(item_id) {
            let mut result = 0isize;
            let handled = if let Some(context) = &self.context3 {
                let hr = unsafe {
                    (context.vtable::<Context3Vtable>().handle_message2)(
                        context.0,
                        WM_INITMENUPOPUP,
                        node.submenu as usize,
                        node.position as isize,
                        &mut result,
                    )
                };
                hr >= 0
            } else {
                false
            };
            if !handled {
                if let Some(context) = &self.context2 {
                    unsafe {
                        (context.vtable::<Context2Vtable>().handle_message)(
                            context.0,
                            WM_INITMENUPOPUP,
                            node.submenu as usize,
                            node.position as isize,
                        )
                    };
                } else if let Some(context) = &self.context3 {
                    unsafe {
                        (context.vtable::<Context3Vtable>().context2.handle_message)(
                            context.0,
                            WM_INITMENUPOPUP,
                            node.submenu as usize,
                            node.position as isize,
                        )
                    };
                }
            }
        }
        self.snapshot()
    }

    fn invoke(&self, item_id: u32) -> Result<(), String> {
        let node = self.nodes.get(&item_id).ok_or_else(super::stale)?;
        if !node.enabled || !node.submenu.is_null() {
            return Err("native item is disabled or requires submenu expansion".into());
        }
        // Re-read native enabled state; presentation may have preceded a clipboard
        // change or a dynamic extension update. Validate current menu identity too.
        let mut current = MENUITEMINFOW {
            cbSize: mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_ID | MIIM_STATE | MIIM_SUBMENU,
            ..Default::default()
        };
        if unsafe { GetMenuItemInfoW(node.parent, node.position, 1, &mut current) } == 0 {
            return Err(super::stale());
        }
        let command = node
            .native_id
            .ok_or_else(|| "native item has no invokable command identifier".to_owned())?;
        if current.wID != command
            || current.fState & MFS_DISABLED != 0
            || !current.hSubMenu.is_null()
        {
            return Err(super::stale());
        }
        let offset = (command - COMMAND_FIRST) as usize;
        let mut info = CMINVOKECOMMANDINFOEX {
            cbSize: mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: SEE_MASK_UNICODE | SEE_MASK_NOASYNC,
            hwnd: self.owner,
            lpVerb: offset as *const u8,
            lpVerbW: offset as *const u16,
            lpDirectoryW: self.directory.as_ptr(),
            nShow: SW_SHOWNORMAL,
            ..Default::default()
        };
        let hr = unsafe {
            (self.context.vtable::<ContextVtable>().invoke)(
                self.context.0,
                (&mut info as *mut CMINVOKECOMMANDINFOEX).cast(),
            )
        };
        if hr < 0 {
            Err(failure("invoke shell command", hr))
        } else {
            Ok(())
        }
    }
}

struct Limits {
    remaining_text: usize,
    count: usize,
    truncated: bool,
    visited: HashSet<usize>,
}
impl Limits {
    fn text(&mut self, mut text: String) -> String {
        if text.len() > self.remaining_text {
            let mut end = self.remaining_text;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            self.truncated = true;
        }
        self.remaining_text -= text.len();
        text
    }
}

fn parsing_name(path: &Path) -> Vec<u16> {
    // Rust accepts mixed separators in Windows PathBuf; SHParseDisplayName
    // rejects these even for real files. Normalize only the Shell input copy.
    let raw: Vec<_> = path
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == b'/' as u16 {
                b'\\' as u16
            } else {
                unit
            }
        })
        .collect();
    let mut name = if raw.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        let mut normalized = vec![92, 92];
        normalized.extend_from_slice(&raw[8..]);
        normalized
    } else if raw.starts_with(&[92, 92, 63, 92]) && raw.len() >= 7 && raw[5] == 58 {
        raw[4..].to_vec()
    } else {
        raw
    };
    name.push(0);
    name
}
fn failure(operation: &str, hr: HRESULT) -> String {
    format!("{operation} failed (HRESULT 0x{:08x})", hr as u32)
}

#[cfg(test)]
pub(super) static NATIVE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
#[test]
fn properties_selection_builds_data_without_a_context_menu_or_clipboard_write() {
    let _guard = NATIVE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _apartment = Apartment::new().unwrap();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // Different parents exercise an absolute multi-selection, not just the
    // usual common-parent case. No property sheet is opened in this check.
    let paths = vec![root.join("Cargo.toml"), root.join("src/lib.rs")];
    let data = properties_data(ptr::null_mut(), &paths).unwrap();
    let format_name: Vec<_> = "Shell IDList Array".encode_utf16().chain(Some(0)).collect();
    let format = FORMATETC {
        cfFormat: unsafe { RegisterClipboardFormatW(format_name.as_ptr()) } as u16,
        ptd: ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed: TYMED_HGLOBAL as u32,
    };
    let mut medium = STGMEDIUM::default();
    assert!(
        unsafe { (data.vtable::<DataObjectVtable>().get_data)(data.0, &format, &mut medium) } >= 0
    );
    let memory = unsafe { medium.u.hGlobal };
    let bytes = unsafe { GlobalLock(memory) } as *const u8;
    assert!(!bytes.is_null());
    let count = unsafe { ptr::read_unaligned(bytes.cast::<u32>()) };
    let mut actual = Vec::new();
    for index in 0..count {
        let offset =
            unsafe { ptr::read_unaligned(bytes.add((index as usize + 2) * 4).cast::<u32>()) };
        let mut name = vec![0u16; 32768];
        let ok = unsafe {
            windows_sys::Win32::UI::Shell::SHGetPathFromIDListEx(
                bytes.add(offset as usize).cast(),
                name.as_mut_ptr(),
                name.len() as u32,
                0,
            )
        };
        actual.push((ok != 0).then(|| {
            let end = name.iter().position(|unit| *unit == 0).unwrap();
            std::path::PathBuf::from(String::from_utf16(&name[..end]).unwrap())
                .canonicalize()
                .unwrap()
        }));
    }
    unsafe {
        GlobalUnlock(memory);
        windows_sys::Win32::System::Ole::ReleaseStgMedium(&mut medium);
    }
    assert_eq!(count, 2);
    assert_eq!(
        actual,
        paths
            .iter()
            .map(|path| Some(path.canonicalize().unwrap()))
            .collect::<Vec<_>>()
    );
    properties_tests::assert_drop_paths(&data, &paths);
}

#[cfg(test)]
mod properties_tests {
    use super::*;
    use std::{
        path::PathBuf,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    struct Fixture {
        root: PathBuf,
        file: PathBuf,
        unicode: PathBuf,
        folder: PathBuf,
        other: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "foldex-properties-fixture-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            let folder = root.join("Folder – 資料");
            std::fs::create_dir_all(&folder).unwrap();
            let file = root.join("foldex-properties-fixture.txt");
            let unicode = root.join("résumé – 資料.txt");
            let other = folder.join("other.txt");
            for (path, bytes) in [(&file, 23), (&unicode, 41), (&other, 89)] {
                // Unique sizes and sums distinguish an actual selection from
                // a stale sheet or an accidental first-item-only fallback.
                std::fs::write(path, vec![b'x'; bytes]).unwrap();
            }
            Self {
                root,
                file,
                unicode,
                folder,
                other,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this uniquely generated test directory is ever removed.
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    pub(super) fn assert_drop_paths(data: &Com, paths: &[PathBuf]) {
        let format = FORMATETC {
            cfFormat: windows_sys::Win32::System::Ole::CF_HDROP,
            ptd: ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT,
            lindex: -1,
            tymed: TYMED_HGLOBAL as u32,
        };
        let mut medium = STGMEDIUM::default();
        assert!(
            unsafe { (data.vtable::<DataObjectVtable>().get_data)(data.0, &format, &mut medium) }
                >= 0
        );
        let memory = unsafe { medium.u.hGlobal };
        let count = unsafe {
            windows_sys::Win32::UI::Shell::DragQueryFileW(memory, u32::MAX, ptr::null_mut(), 0)
        };
        let mut actual = Vec::new();
        for index in 0..count {
            let length = unsafe {
                windows_sys::Win32::UI::Shell::DragQueryFileW(memory, index, ptr::null_mut(), 0)
            };
            let mut name = vec![0u16; length as usize + 1];
            assert_eq!(
                unsafe {
                    windows_sys::Win32::UI::Shell::DragQueryFileW(
                        memory,
                        index,
                        name.as_mut_ptr(),
                        name.len() as u32,
                    )
                },
                length
            );
            actual.push(
                PathBuf::from(String::from_utf16(&name[..length as usize]).unwrap())
                    .canonicalize()
                    .unwrap(),
            );
        }
        unsafe { windows_sys::Win32::System::Ole::ReleaseStgMedium(&mut medium) };
        assert_eq!(
            actual,
            paths
                .iter()
                .map(|path| path.canonicalize().unwrap())
                .collect::<Vec<_>>()
        );
    }

    fn assert_id_paths(data: &Com, paths: &[PathBuf]) {
        let name: Vec<u16> = "Shell IDList Array".encode_utf16().chain(Some(0)).collect();
        let format = FORMATETC {
            cfFormat: unsafe { RegisterClipboardFormatW(name.as_ptr()) } as u16,
            ptd: ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT,
            lindex: -1,
            tymed: TYMED_HGLOBAL as u32,
        };
        let mut medium = STGMEDIUM::default();
        assert!(
            unsafe { (data.vtable::<DataObjectVtable>().get_data)(data.0, &format, &mut medium) }
                >= 0
        );
        let memory = unsafe { medium.u.hGlobal };
        let bytes = unsafe { GlobalLock(memory) }.cast::<u8>();
        assert!(!bytes.is_null());
        let count = unsafe { ptr::read_unaligned(bytes.cast::<u32>()) };
        let parent = unsafe { ptr::read_unaligned(bytes.add(4).cast::<u32>()) } as usize;
        let mut actual = Vec::new();
        for index in 0..count as usize {
            let offset =
                unsafe { ptr::read_unaligned(bytes.add((index + 2) * 4).cast::<u32>()) } as usize;
            let absolute = Pidl(unsafe {
                windows_sys::Win32::UI::Shell::ILCombine(
                    bytes.add(parent).cast(),
                    bytes.add(offset).cast(),
                )
            });
            assert!(!absolute.0.is_null());
            let mut name = vec![0u16; 32768];
            assert_ne!(
                unsafe {
                    windows_sys::Win32::UI::Shell::SHGetPathFromIDListEx(
                        absolute.0,
                        name.as_mut_ptr(),
                        name.len() as u32,
                        0,
                    )
                },
                0
            );
            let length = name.iter().position(|unit| *unit == 0).unwrap();
            actual.push(
                PathBuf::from(String::from_utf16(&name[..length]).unwrap())
                    .canonicalize()
                    .unwrap(),
            );
        }
        unsafe {
            GlobalUnlock(memory);
            windows_sys::Win32::System::Ole::ReleaseStgMedium(&mut medium);
        }
        assert_eq!(
            actual,
            paths
                .iter()
                .map(|path| path.canonicalize().unwrap())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn properties_sibling_and_mixed_parent_data_preserves_files_folders_and_unicode() {
        let _guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _apartment = Apartment::new().unwrap();
        let fixture = Fixture::new();
        for paths in [
            vec![fixture.file.clone()],
            vec![fixture.unicode.clone()],
            vec![fixture.folder.clone()],
            vec![
                fixture.file.clone(),
                fixture.unicode.clone(),
                fixture.folder.clone(),
            ],
            vec![fixture.unicode.clone(), fixture.other.clone()],
        ] {
            let data = properties_data(ptr::null_mut(), &paths).unwrap();
            assert_id_paths(&data, &paths);
            assert_drop_paths(&data, &paths);
        }
    }

    #[test]
    fn properties_drive_roots_use_volume_fallback_and_children_use_filepath() {
        for path in [r"C:\", "D:/", r"\\?\C:\"] {
            assert_eq!(
                properties_object_type(Path::new(path)),
                SHOP_VOLUMEGUID as u32,
                "{path}"
            );
        }
        for path in [r"C:\folder", r"C:\résumé – 資料.txt", r"\\server\share\"] {
            assert_eq!(
                properties_object_type(Path::new(path)),
                SHOP_FILEPATH as u32,
                "{path}"
            );
        }
    }

    #[test]
    fn properties_rejects_stale_owner_before_activating_a_handler() {
        let _guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = Fixture::new();
        let error = properties(-1, &[fixture.file.clone()]).unwrap_err();
        assert!(error.contains("owner window no longer exists"));
    }

    #[test]
    fn properties_thread_reference_is_registered_and_cleared() {
        let _guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _apartment = Apartment::new().unwrap();
        let lifetime = PropertiesThread::new().unwrap();
        let mut pointer = ptr::null_mut();
        let hr = unsafe { windows_sys::Win32::UI::Shell::SHGetThreadRef(&mut pointer) };
        let reference = Com::received(hr, pointer, "retrieve Properties thread reference").unwrap();
        assert!(lifetime.count.load(Ordering::Acquire) >= 2);
        drop(reference);
        drop(lifetime);
        let mut pointer = ptr::null_mut();
        assert!(unsafe { windows_sys::Win32::UI::Shell::SHGetThreadRef(&mut pointer) } < 0);
        assert!(pointer.is_null());
    }

    /// Parent-only native verification: isolated generated .txt/folder targets,
    /// actual Shell sheet activation, then WM_CLOSE on this test process's
    /// native property sheets. Never changes properties or invokes fixture files.
    #[test]
    #[ignore = "opens real Windows Properties; set FOLDEX_VERIFY_NATIVE_PROPERTIES=1 and run alone"]
    fn opt_in_properties_fixture_shows_real_native_sheets() {
        assert_eq!(
            std::env::var("FOLDEX_VERIFY_NATIVE_PROPERTIES").as_deref(),
            Ok("1")
        );
        let _guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let fixture = Fixture::new();
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        let properties = crate::Properties::new(std::sync::Arc::new(move || {
            let _ = wake_tx.send(());
        }));
        for (case, paths, bytes) in [
            ("file", vec![fixture.file.clone()], 23),
            ("unicode", vec![fixture.unicode.clone()], 41),
            ("folder", vec![fixture.folder.clone()], 89),
            (
                "siblings",
                vec![fixture.file.clone(), fixture.unicode.clone()],
                64,
            ),
            (
                "mixed parents",
                vec![fixture.unicode.clone(), fixture.other.clone()],
                130,
            ),
        ] {
            assert!(
                properties_process_sheets().is_empty(),
                "previous sheet survived before {case}"
            );
            let (ready_tx, ready_rx) = std::sync::mpsc::channel();
            let (seen_tx, seen_rx) = std::sync::mpsc::channel();
            let (close_tx, close_rx) = std::sync::mpsc::channel();
            let expected = paths.clone();
            let observer = std::thread::spawn(move || {
                observe_and_close_fixture_sheet(expected, bytes, ready_tx, seen_tx, close_rx)
            });
            ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
            properties.show_with_owner(0, paths).unwrap();
            let seen = seen_rx.recv_timeout(Duration::from_secs(12));
            // Exercise the actual Properties service while a verified native
            // sheet is held open, including sheets hosted on another thread.
            let rejected = properties.spawn(|| Ok(())).is_err();
            let _ = close_tx.send(());
            let observed = observer.join().unwrap();
            assert!(
                seen.is_ok(),
                "{case}: no correct selection appeared; {observed:?}"
            );
            assert!(
                rejected,
                "{case}: Properties lane was released while its sheet was still open"
            );
            wake_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("Properties lane did not finish after sheet destruction");
            assert_eq!(properties.try_recv(), Some(Ok(())), "{case}");
            println!(
                "{case}: {}; bytes={bytes}; busy_while_open=true; destroyed=true",
                observed.unwrap()
            );
        }
    }

    /// Parent-only, read-only SystemDrive verification on its owned offscreen
    /// desktop. Reads metadata/controls and closes the verified sheet only.
    #[test]
    #[ignore = "opens drive Properties; use owned offscreen driver and FOLDEX_VERIFY_NATIVE_DRIVE_PROPERTIES=1"]
    fn opt_in_system_drive_properties_shows_real_native_sheet() {
        use windows_sys::Win32::{
            Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW},
            UI::Shell::{SHFILEINFOW, SHGFI_DISPLAYNAME, SHGetFileInfoW},
        };
        assert_eq!(
            std::env::var("FOLDEX_VERIFY_NATIVE_DRIVE_PROPERTIES").as_deref(),
            Ok("1")
        );
        assert_owned_probe_desktop();
        let _guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(
            properties_process_sheets().is_empty(),
            "probe process already has a native property sheet"
        );
        let drive = std::env::var("SystemDrive").expect("SystemDrive is required");
        assert!(
            drive.len() == 2
                && drive.as_bytes()[0].is_ascii_alphabetic()
                && drive.as_bytes()[1] == b':',
            "SystemDrive must be a drive letter, got {drive:?}"
        );
        let root = PathBuf::from(format!("{drive}\\"));
        let name = parsing_name(&root);
        let mut capacity = 0u64;
        assert_ne!(
            unsafe {
                GetDiskFreeSpaceExW(
                    name.as_ptr(),
                    ptr::null_mut(),
                    &mut capacity,
                    ptr::null_mut(),
                )
            },
            0
        );
        assert!(capacity > 0);
        let mut filesystem = [0u16; 64];
        assert_ne!(
            unsafe {
                GetVolumeInformationW(
                    name.as_ptr(),
                    ptr::null_mut(),
                    0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    filesystem.as_mut_ptr(),
                    filesystem.len() as u32,
                )
            },
            0
        );
        let filesystem = String::from_utf16_lossy(
            &filesystem[..filesystem.iter().position(|unit| *unit == 0).unwrap()],
        );
        assert!(!filesystem.is_empty());
        let mut file_info = SHFILEINFOW::default();
        let _apartment = Apartment::new().unwrap();
        assert_ne!(
            unsafe {
                SHGetFileInfoW(
                    name.as_ptr(),
                    0,
                    &mut file_info,
                    mem::size_of::<SHFILEINFOW>() as u32,
                    SHGFI_DISPLAYNAME,
                )
            },
            0
        );
        let display = String::from_utf16_lossy(
            &file_info.szDisplayName[..file_info
                .szDisplayName
                .iter()
                .position(|unit| *unit == 0)
                .unwrap()],
        );
        assert!(
            display.contains(&drive),
            "Shell display name does not identify SystemDrive: {display}"
        );
        println!(
            "drive root={}; display={display}; filesystem={filesystem}; capacity={capacity}",
            root.display()
        );
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        let properties = crate::Properties::new(std::sync::Arc::new(move || {
            let _ = wake_tx.send(());
        }));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (seen_tx, seen_rx) = std::sync::mpsc::channel();
        let (close_tx, close_rx) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            observe_and_close_drive_sheet(
                display, filesystem, capacity, ready_tx, seen_tx, close_rx,
            )
        });
        ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        properties.show_with_owner(0, vec![root]).unwrap();
        let seen = seen_rx.recv_timeout(Duration::from_secs(12));
        let rejected = properties.spawn(|| Ok(())).is_err();
        let _ = close_tx.send(());
        let observed = observer.join().unwrap();
        assert!(
            seen.is_ok(),
            "SystemDrive sheet was not verified: {observed:?}"
        );
        assert!(
            rejected,
            "drive sheet was open but the Properties lane was released"
        );
        wake_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("drive Properties did not finish after destruction");
        assert_eq!(properties.try_recv(), Some(Ok(())));
        println!(
            "drive: {}; busy_while_open=true; destroyed=true; settings_changed=false",
            observed.unwrap()
        );
    }

    fn assert_owned_probe_desktop() {
        use windows_sys::Win32::System::StationsAndDesktops::{
            GetProcessWindowStation, GetThreadDesktop, GetUserObjectInformationW, UOI_IO, UOI_NAME,
        };
        fn object_name(handle: windows_sys::Win32::Foundation::HANDLE) -> String {
            let mut name = [0u16; 256];
            assert_ne!(
                unsafe {
                    GetUserObjectInformationW(
                        handle,
                        UOI_NAME,
                        name.as_mut_ptr().cast(),
                        mem::size_of_val(&name) as u32,
                        ptr::null_mut(),
                    )
                },
                0
            );
            String::from_utf16_lossy(&name[..name.iter().position(|unit| *unit == 0).unwrap()])
        }
        let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
        assert!(!desktop.is_null());
        let desktop_name = object_name(desktop);
        let station_name = object_name(unsafe { GetProcessWindowStation() });
        assert!(
            desktop_name.starts_with("FoldexProbe_")
                || (desktop_name == "Probe" && station_name.starts_with("FoldexProbe_")),
            "drive verification requires the parent's owned probe desktop: {station_name}\\{desktop_name}"
        );
        let mut receives_input = 0i32;
        assert_ne!(
            unsafe {
                GetUserObjectInformationW(
                    desktop,
                    UOI_IO,
                    (&mut receives_input as *mut i32).cast(),
                    mem::size_of_val(&receives_input) as u32,
                    ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(
            receives_input, 0,
            "probe desktop must not receive user input"
        );
        println!("owned desktop={station_name}\\{desktop_name}; receives_input=false");
    }

    fn observe_and_close_drive_sheet(
        display: String,
        filesystem: String,
        capacity: u64,
        ready: std::sync::mpsc::Sender<()>,
        seen: std::sync::mpsc::Sender<()>,
        close: std::sync::mpsc::Receiver<()>,
    ) -> Result<String, String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumChildWindows, PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CLOSE,
            WM_GETTEXT,
        };
        fn text(window: HWND) -> String {
            let mut buffer = [0u16; 2048];
            let mut length = 0;
            if unsafe {
                SendMessageTimeoutW(
                    window,
                    WM_GETTEXT,
                    buffer.len(),
                    buffer.as_mut_ptr() as LPARAM,
                    SMTO_ABORTIFHUNG,
                    100,
                    &mut length,
                )
            } == 0
            {
                return String::new();
            }
            String::from_utf16_lossy(&buffer[..length.min(buffer.len() - 1)])
        }
        unsafe extern "system" fn child(window: HWND, state: LPARAM) -> i32 {
            let strings = unsafe { &mut *(state as *mut Vec<String>) };
            if strings.len() >= 96 {
                return 0;
            }
            if unsafe { IsWindowVisible(window) } != 0 {
                let value = text(window);
                if !value.is_empty() {
                    strings.push(value);
                }
            }
            1
        }
        let baseline = properties_process_sheets();
        let _ = ready.send(());
        let deadline = Instant::now() + Duration::from_secs(10);
        let capacity = capacity.to_string();
        let mut last = String::new();
        loop {
            for window in properties_process_sheets().difference(&baseline) {
                let window = *window as HWND;
                let caption = text(window);
                let mut controls: Vec<String> = Vec::new();
                unsafe {
                    EnumChildWindows(
                        window,
                        Some(child),
                        (&mut controls as *mut Vec<String>) as LPARAM,
                    )
                };
                last = format!("{caption}; controls={controls:?}");
                // File/folder sheets cannot satisfy the exact Shell drive
                // caption, native filesystem value and full capacity together.
                let correct_caption = caption.starts_with(&display);
                let correct_filesystem = controls
                    .iter()
                    .any(|value| value.trim().eq_ignore_ascii_case(&filesystem));
                let correct_capacity = controls.iter().any(|value| {
                    value
                        .chars()
                        .filter(char::is_ascii_digit)
                        .collect::<String>()
                        .contains(&capacity)
                });
                if !correct_caption || !correct_filesystem || !correct_capacity {
                    continue;
                }
                let _ = seen.send(());
                std::thread::sleep(Duration::from_millis(300));
                let _ = close.recv_timeout(Duration::from_secs(3));
                unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
                let deadline = Instant::now() + Duration::from_secs(3);
                while unsafe { IsWindow(window) } != 0 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if unsafe { IsWindow(window) } != 0 {
                    return Err(format!("verified drive sheet did not close: {last}"));
                }
                return Ok(last);
            }
            if Instant::now() >= deadline {
                // Unverified windows are deliberately left untouched. The
                // isolated test process/parent timeout owns failure cleanup.
                return Err(format!(
                    "no fresh sheet matched drive caption, filesystem and exact capacity: {last}"
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn observe_and_close_fixture_sheet(
        paths: Vec<PathBuf>,
        bytes: i64,
        ready: std::sync::mpsc::Sender<()>,
        seen: std::sync::mpsc::Sender<()>,
        close: std::sync::mpsc::Receiver<()>,
    ) -> Result<String, String> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumChildWindows, PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_CLOSE,
            WM_GETTEXT,
        };
        fn text(window: HWND) -> String {
            let mut buffer = [0u16; 2048];
            let mut length = 0;
            if unsafe {
                SendMessageTimeoutW(
                    window,
                    WM_GETTEXT,
                    buffer.len(),
                    buffer.as_mut_ptr() as LPARAM,
                    SMTO_ABORTIFHUNG,
                    100,
                    &mut length,
                )
            } == 0
            {
                return String::new();
            }
            String::from_utf16_lossy(&buffer[..length.min(buffer.len() - 1)])
        }
        unsafe extern "system" fn child(window: HWND, state: LPARAM) -> i32 {
            let strings = unsafe { &mut *(state as *mut Vec<String>) };
            if strings.len() >= 96 {
                return 0;
            }
            if unsafe { IsWindowVisible(window) } != 0 {
                let value = text(window);
                if !value.is_empty() {
                    strings.push(value);
                }
            }
            1
        }
        fn compact(value: &str) -> String {
            value
                .chars()
                .filter(|c| !c.is_whitespace() && *c != '\u{200e}' && *c != '\u{200f}')
                .collect()
        }
        let mut formatted = [0u16; 64];
        assert!(
            !unsafe {
                windows_sys::Win32::UI::Shell::StrFormatByteSizeW(
                    bytes,
                    formatted.as_mut_ptr(),
                    formatted.len() as u32,
                )
            }
            .is_null()
        );
        let size = compact(&String::from_utf16_lossy(
            &formatted[..formatted.iter().position(|unit| *unit == 0).unwrap()],
        ));
        let single_name = (paths.len() == 1)
            .then(|| paths[0].file_stem().unwrap().to_string_lossy().into_owned());
        let parent = paths[0].parent().unwrap();
        let common_parent = paths
            .iter()
            .all(|path| path.parent() == Some(parent))
            .then(|| parent.to_string_lossy().replace('/', "\\").to_lowercase());
        let deadline = Instant::now() + Duration::from_secs(10);
        let baseline = properties_process_sheets();
        let _ = ready.send(());
        let mut last = String::new();
        loop {
            for window in properties_process_sheets().difference(&baseline) {
                let window = *window as HWND;
                let title = text(window);
                let mut controls: Vec<String> = Vec::new();
                unsafe {
                    EnumChildWindows(
                        window,
                        Some(child),
                        (&mut controls as *mut Vec<String>) as LPARAM,
                    )
                };
                last = format!("{title}; controls={controls:?}");
                let name_matches = single_name.as_ref().is_none_or(|name| title.contains(name));
                let location_matches = common_parent.as_ref().is_none_or(|parent| {
                    controls
                        .iter()
                        .any(|value| value.replace('/', "\\").to_lowercase().contains(parent))
                });
                let size_matches = controls.iter().any(|value| compact(value).contains(&size));
                if !name_matches || !location_matches || !size_matches {
                    continue;
                }
                let _ = seen.send(());
                // Let the lifetime worker observe the sheet across its 250 ms
                // wait before closing; it must remain busy throughout.
                std::thread::sleep(Duration::from_millis(300));
                let _ = close.recv_timeout(Duration::from_secs(3));
                unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
                let destroyed = Instant::now() + Duration::from_secs(3);
                while unsafe { IsWindow(window) } != 0 && Instant::now() < destroyed {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if unsafe { IsWindow(window) } != 0 {
                    return Err(format!("verified sheet did not close: {last}"));
                }
                return Ok(last);
            }
            if Instant::now() >= deadline {
                // Only fresh property windows from this isolated test process.
                for window in properties_process_sheets().difference(&baseline) {
                    unsafe { PostMessageW(*window as HWND, WM_CLOSE, 0, 0) };
                }
                return Err(format!(
                    "no fresh sheet matched name, location and total size ({bytes} bytes): {last}"
                ));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
