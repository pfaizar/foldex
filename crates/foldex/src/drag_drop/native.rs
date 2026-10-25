//! The Shell owns native data objects and external drop targets. Foldex-to-
//! Foldex transfers enter the existing worker/history lane after the OLE drag
//! ends, so they retain delayed progress, cancellation and local undo.

use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::c_void,
    io,
    os::windows::ffi::OsStrExt,
    ptr,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
};
use windows_sys::{
    Win32::{
        Foundation::{HWND, POINT, POINTL},
        Graphics::Gdi::ScreenToClient,
        System::{
            Com::CoTaskMemFree,
            Ole::{
                DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE, OleInitialize,
                OleUninitialize, RegisterDragDrop, RevokeDragDrop,
            },
        },
        UI::Shell::{
            BHID_DataObject, BHID_SFUIObject, Common::ITEMIDLIST, SHCreateItemFromParsingName,
            SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName,
        },
    },
    core::GUID,
};

const IID_UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_TARGET: GUID = GUID::from_u128(0x00000122_0000_0000_c000_000000000046);
const IID_DATA: GUID = GUID::from_u128(0x0000010e_0000_0000_c000_000000000046);
const IID_ITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
const E_POINTER: i32 = 0x80004003u32 as i32;
const E_NOINTERFACE: i32 = 0x80004002u32 as i32;

#[repr(C)]
struct UnknownVtable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}
#[repr(C)]
struct BindVtable {
    unknown: UnknownVtable,
    bind: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const GUID,
        *const GUID,
        *mut *mut c_void,
    ) -> i32,
}
#[repr(C)]
struct TargetVtable {
    unknown: UnknownVtable,
    enter: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> i32,
    over: unsafe extern "system" fn(*mut c_void, u32, POINTL, *mut u32) -> i32,
    leave: unsafe extern "system" fn(*mut c_void) -> i32,
    drop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> i32,
}
struct Com(*mut c_void);
impl Com {
    fn vtable<T>(&self) -> &T {
        unsafe { &**self.0.cast::<*const T>() }
    }
    fn received(result: i32, pointer: *mut c_void) -> io::Result<Self> {
        let value = (!pointer.is_null()).then(|| Self(pointer));
        check(result)?;
        value.ok_or_else(|| io::Error::other("Windows returned an empty drag interface"))
    }
    unsafe fn retain(pointer: *mut c_void) -> Self {
        let value = Self(pointer);
        unsafe { (value.vtable::<UnknownVtable>().add_ref)(pointer) };
        value
    }
    fn bind(&self, handler: &GUID, iid: &GUID) -> io::Result<Self> {
        let mut output = ptr::null_mut();
        let result = unsafe {
            (self.vtable::<BindVtable>().bind)(self.0, ptr::null_mut(), handler, iid, &mut output)
        };
        Self::received(result, output)
    }
    fn identity(&self) -> io::Result<Self> {
        let mut output = ptr::null_mut();
        let result =
            unsafe { (self.vtable::<UnknownVtable>().query)(self.0, &IID_UNKNOWN, &mut output) };
        Self::received(result, output)
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { (self.vtable::<UnknownVtable>().release)(self.0) };
    }
}
fn check(result: i32) -> io::Result<()> {
    if result < 0 {
        Err(io::Error::other(format!(
            "Windows file drag: HRESULT 0x{:08X}",
            result as u32
        )))
    } else {
        Ok(())
    }
}
fn wide(path: &std::path::Path) -> io::Result<Vec<u16>> {
    let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
    if !path.is_absolute() || name.len() > 32767 || name.contains(&0) {
        return Err(io::Error::other("Drag paths must be absolute and NUL-free"));
    }
    name.push(0);
    Ok(name)
}
fn folder_target(path: &std::path::Path) -> io::Result<Com> {
    let name = wide(path)?;
    let mut pointer = ptr::null_mut();
    let result = unsafe {
        SHCreateItemFromParsingName(name.as_ptr(), ptr::null_mut(), &IID_ITEM, &mut pointer)
    };
    Com::received(result, pointer)?.bind(&BHID_SFUIObject, &IID_TARGET)
}
struct Pidl(*mut ITEMIDLIST);
impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(self.0.cast()) };
    }
}
fn data_object(paths: &[PathBuf]) -> io::Result<Com> {
    if paths.is_empty() || paths.len() > 4096 {
        return Err(io::Error::other("Drag between 1 and 4,096 items"));
    }
    let mut ids = Vec::with_capacity(paths.len());
    let mut bytes = 0usize;
    for path in paths {
        let name = wide(path)?;
        bytes = bytes.saturating_add(name.len() * 2);
        if bytes > 1024 * 1024 {
            return Err(io::Error::other("Drag paths exceed 1 MiB"));
        }
        let mut pointer = ptr::null_mut();
        let result = unsafe {
            SHParseDisplayName(
                name.as_ptr(),
                ptr::null_mut(),
                &mut pointer,
                0,
                ptr::null_mut(),
            )
        };
        let id = Pidl(pointer);
        check(result)?;
        if id.0.is_null() {
            return Err(io::Error::other("Windows returned no drag identity"));
        }
        ids.push(id);
    }
    let pointers: Vec<_> = ids.iter().map(|id| id.0.cast_const()).collect();
    let mut pointer = ptr::null_mut();
    let result = unsafe {
        SHCreateShellItemArrayFromIDLists(pointers.len() as u32, pointers.as_ptr(), &mut pointer)
    };
    Com::received(result, pointer)?.bind(&BHID_DataObject, &IID_DATA)
}

#[derive(Default)]
struct Session {
    data: Option<Com>,
    target: Option<(PathBuf, Com)>,
    allowed: u32,
}
struct Shared {
    owner: HWND,
    ctx: Context,
    session: RefCell<Session>,
    own_source: RefCell<Option<(Com, Vec<PathBuf>)>>,
    drops: RefCell<VecDeque<DropRequest>>,
    changed: Cell<bool>,
}
impl Shared {
    fn path_at(&self, point: POINTL) -> Option<PathBuf> {
        let mut client = POINT {
            x: point.x,
            y: point.y,
        };
        if unsafe { ScreenToClient(self.owner, &mut client) } == 0 {
            return None;
        }
        let scale = self.ctx.pixels_per_point();
        destination(
            &self.ctx,
            Pos2::new(client.x as f32 / scale, client.y as f32 / scale),
        )
    }
    fn route(&self, session: &mut Session, keys: u32, point: POINTL, effect: &mut u32) -> i32 {
        let path = self.path_at(point);
        if session.target.as_ref().map(|(path, _)| path) != path.as_ref() {
            if let Some((_, target)) = session.target.take() {
                unsafe { (target.vtable::<TargetVtable>().leave)(target.0) };
            }
            if let (Some(path), Some(data)) = (path, session.data.as_ref()) {
                if let Ok(target) = folder_target(&path) {
                    *effect = session.allowed;
                    let result = unsafe {
                        (target.vtable::<TargetVtable>().enter)(
                            target.0, data.0, keys, point, effect,
                        )
                    };
                    if result >= 0 {
                        session.target = Some((path, target));
                    } else {
                        *effect = DROPEFFECT_NONE;
                    }
                    return result;
                }
            }
        }
        if let Some((_, target)) = &session.target {
            *effect = session.allowed;
            unsafe { (target.vtable::<TargetVtable>().over)(target.0, keys, point, effect) }
        } else {
            *effect = DROPEFFECT_NONE;
            0
        }
    }
    fn leave(&self) {
        let session = self.session.take();
        if let Some((_, target)) = session.target {
            unsafe { (target.vtable::<TargetVtable>().leave)(target.0) };
        }
        self.ctx.request_repaint();
    }
}

#[repr(C)]
struct Target {
    vtable: &'static TargetVtable,
    references: AtomicU32,
    shared: Rc<Shared>,
}
unsafe fn target<'a>(this: *mut c_void) -> &'a Target {
    unsafe { &*this.cast::<Target>() }
}
unsafe extern "system" fn query(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() {
        return E_POINTER;
    }
    unsafe { *output = ptr::null_mut() };
    if iid.is_null() {
        return E_POINTER;
    }
    let iid = unsafe { &*iid };
    let same = |other: &GUID| {
        iid.data1 == other.data1
            && iid.data2 == other.data2
            && iid.data3 == other.data3
            && iid.data4 == other.data4
    };
    if same(&IID_UNKNOWN) || same(&IID_TARGET) {
        unsafe {
            *output = this;
            add_ref(this);
        }
        0
    } else {
        E_NOINTERFACE
    }
}
unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    unsafe { target(this) }
        .references
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let previous = unsafe { target(this) }
        .references
        .fetch_sub(1, Ordering::AcqRel);
    if previous == 1 {
        unsafe { drop(Box::from_raw(this.cast::<Target>())) };
    }
    previous - 1
}
unsafe extern "system" fn enter(
    this: *mut c_void,
    data: *mut c_void,
    keys: u32,
    point: POINTL,
    effect: *mut u32,
) -> i32 {
    if effect.is_null() || data.is_null() {
        return E_POINTER;
    }
    let shared = &unsafe { target(this) }.shared;
    shared.leave();
    let mut session = Session {
        data: Some(unsafe { Com::retain(data) }),
        allowed: unsafe { *effect },
        ..Default::default()
    };
    let result = shared.route(&mut session, keys, point, unsafe { &mut *effect });
    shared.session.replace(session);
    result
}
unsafe extern "system" fn over(
    this: *mut c_void,
    keys: u32,
    point: POINTL,
    effect: *mut u32,
) -> i32 {
    if effect.is_null() {
        return E_POINTER;
    }
    let shared = &unsafe { target(this) }.shared;
    let mut session = shared.session.take();
    let result = shared.route(&mut session, keys, point, unsafe { &mut *effect });
    shared.session.replace(session);
    result
}
unsafe extern "system" fn leave(this: *mut c_void) -> i32 {
    unsafe { target(this) }.shared.leave();
    0
}
unsafe extern "system" fn drop_files(
    this: *mut c_void,
    data: *mut c_void,
    keys: u32,
    point: POINTL,
    effect: *mut u32,
) -> i32 {
    if effect.is_null() || data.is_null() {
        return E_POINTER;
    }
    let shared = &unsafe { target(this) }.shared;
    let mut session = shared.session.take();
    let mut chosen = 0;
    let result = shared.route(&mut session, keys, point, &mut chosen);
    chosen &= DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
    unsafe {
        *effect = DROPEFFECT_NONE;
    }
    if result < 0 || chosen == 0 {
        if let Some((_, target)) = session.target.take() {
            unsafe { (target.vtable::<TargetVtable>().leave)(target.0) };
        }
        return result;
    }
    if let Some((destination, target)) = session.target.take() {
        let own = shared.own_source.borrow();
        let paths = own.as_ref().and_then(|(identity, paths)| {
            let current = session.data.as_ref()?.identity().ok()?;
            (current.0 == identity.0).then(|| paths.clone())
        });
        drop(own);
        if let Some(sources) = paths.filter(|_| chosen & DROPEFFECT_LINK == 0) {
            // This source belongs to Foldex. Return NONE to OLE so its source
            // never deletes originals; the tracked worker owns the whole move.
            if shared.drops.borrow().len() < 8 {
                shared.drops.borrow_mut().push_back(DropRequest {
                    sources,
                    destination,
                    cut: chosen & DROPEFFECT_MOVE != 0,
                });
            }
            unsafe { (target.vtable::<TargetVtable>().leave)(target.0) };
            shared.ctx.request_repaint();
            return 0;
        }
        unsafe {
            *effect = chosen;
        }
        let result =
            unsafe { (target.vtable::<TargetVtable>().drop)(target.0, data, keys, point, effect) };
        shared.changed.set(result >= 0 && unsafe { *effect } != 0);
        shared.ctx.request_repaint();
        result
    } else {
        0
    }
}
static TARGET_VTABLE: TargetVtable = TargetVtable {
    unknown: UnknownVtable {
        query,
        add_ref,
        release,
    },
    enter,
    over,
    leave,
    drop: drop_files,
};

pub struct Native {
    shared: Rc<Shared>,
    target: Option<Com>,
    initialized: bool,
}
impl Native {
    pub fn new(owner: isize, ctx: &Context) -> io::Result<Self> {
        let shared = Rc::new(Shared {
            owner: owner as HWND,
            ctx: ctx.clone(),
            session: RefCell::new(Session::default()),
            own_source: RefCell::new(None),
            drops: RefCell::new(VecDeque::new()),
            changed: Cell::new(false),
        });
        let mut result = Self {
            shared,
            target: None,
            initialized: false,
        };
        if owner == 0 {
            return Ok(result);
        }
        check(unsafe { OleInitialize(ptr::null()) })?;
        result.initialized = true;
        let target = Com(Box::into_raw(Box::new(Target {
            vtable: &TARGET_VTABLE,
            references: AtomicU32::new(1),
            shared: result.shared.clone(),
        }))
        .cast());
        check(unsafe { RegisterDragDrop(owner as HWND, target.0) })?;
        result.target = Some(target);
        Ok(result)
    }
    pub fn start(&self, paths: &[PathBuf]) -> io::Result<()> {
        if !self.initialized {
            return Err(io::Error::other("No Windows drag window"));
        }
        let data = data_object(paths)?;
        let identity = data.identity()?;
        self.shared
            .own_source
            .replace(Some((identity, paths.to_vec())));
        let mut effect = 0;
        let result = unsafe {
            SHDoDragDrop(
                self.shared.owner,
                data.0,
                ptr::null_mut(),
                DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
                &mut effect,
            )
        };
        self.shared.own_source.replace(None);
        self.shared.leave();
        self.shared.changed.set(true);
        check(result)
    }
    pub fn take_drop(&self) -> Option<DropRequest> {
        self.shared.drops.borrow_mut().pop_front()
    }
    pub fn take_changed(&self) -> bool {
        self.shared.changed.replace(false)
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        if self.target.is_some() {
            unsafe { RevokeDragDrop(self.shared.owner) };
            self.shared.leave();
            self.target.take();
        }
        if self.initialized {
            unsafe { OleUninitialize() };
        }
    }
}
