//! Delayed native progress for caller-owned IFileOperation instances.
//!
//! SetProgressDialog is the documented extension point: only progress is delayed,
//! while the Shell continues to own conflicts, elevation, errors and cancellation.
//! No window is started for short operations or a terminal 100% update. All COM
//! objects stay on the operation's STA; there is no timer thread or late callback.
//!
//! ABI: Microsoft Win32 metadata, ShObjIdl_core.h, IOperationsProgressDialog.
//! UX: https://learn.microsoft.com/windows/win32/uxguide/progress-bars

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    ptr,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};
use windows_sys::{
    Win32::Foundation::HWND,
    core::{GUID, HRESULT},
};

const DELAY: Duration = Duration::from_secs(1);
const IID_UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_DIALOG: GUID = GUID::from_u128(0x0c9fb851_e5c9_43eb_a370_f0677b13874c);
const E_NOINTERFACE: HRESULT = 0x80004002u32 as i32;
const E_POINTER: HRESULT = 0x80004003u32 as i32;
const PDOPS_RUNNING: i32 = 1;
const PDOPS_STOPPED: i32 = 4;

#[repr(C)]
struct UnknownVtable {
    query: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct DialogVtable {
    unknown: UnknownVtable,
    start: unsafe extern "system" fn(*mut c_void, HWND, u32) -> HRESULT,
    stop: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    operation: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    mode: unsafe extern "system" fn(*mut c_void, u32) -> HRESULT,
    progress: unsafe extern "system" fn(*mut c_void, u64, u64, u64, u64, u64, u64) -> HRESULT,
    locations:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    reset: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    pause: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    resume: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    milliseconds: unsafe extern "system" fn(*mut c_void, *mut u64, *mut u64) -> HRESULT,
    status: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
}

struct Com(*mut c_void);
impl Com {
    fn vtable<T>(&self) -> &T {
        unsafe { &**self.0.cast::<*const T>() }
    }

    unsafe fn retain(pointer: *mut c_void) -> Option<Self> {
        if pointer.is_null() {
            return None;
        }
        let result = Self(pointer);
        unsafe { (result.vtable::<UnknownVtable>().add_ref)(pointer) };
        Some(result)
    }
}
impl Clone for Com {
    fn clone(&self) -> Self {
        unsafe { Self::retain(self.0).unwrap() }
    }
}
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { (self.vtable::<UnknownVtable>().release)(self.0) };
    }
}

/// An STA-owned IOperationsProgressDialog adapter. Attach `as_raw()` with
/// IFileOperation::SetProgressDialog before performing the operation.
pub struct DelayedProgressDialog(Com);
impl DelayedProgressDialog {
    #[cfg(windows)]
    pub fn new() -> Result<Self, HRESULT> {
        use windows_sys::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
        let mut pointer = ptr::null_mut();
        let result = unsafe {
            CoCreateInstance(
                &GUID::from_u128(0xf8383852_fcd3_11d1_a6b9_006097df5bd4),
                ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_DIALOG,
                &mut pointer,
            )
        };
        let delegate = (!pointer.is_null()).then(|| Com(pointer));
        if result < 0 {
            return Err(result);
        }
        Ok(Self::with_delegate(delegate.ok_or(E_POINTER)?))
    }

    fn with_delegate(delegate: Com) -> Self {
        Self(Com(Box::into_raw(Box::new(Dialog {
            vtable: &VTABLE,
            references: AtomicU32::new(1),
            delegate,
            started: Cell::new(None),
            presentation_pause: Cell::new(None),
            visible: Cell::new(false),
            operation: Cell::new(None),
            mode: Cell::new(None),
            progress: Cell::new([0; 6]),
            locations: RefCell::new([None, None, None]),
            timer: Cell::new((Instant::now(), Duration::ZERO, false)),
        }))
        .cast()))
    }

    /// Borrowed COM interface; keep this value alive until SetProgressDialog
    /// has retained it. Do not use the pointer from a different apartment.
    pub fn as_raw(&self) -> *mut c_void {
        self.0.0
    }
}

#[repr(C)]
struct Dialog {
    vtable: &'static DialogVtable,
    references: AtomicU32,
    delegate: Com,
    started: Cell<Option<(Instant, HWND, u32)>>,
    presentation_pause: Cell<Option<Instant>>,
    visible: Cell<bool>,
    operation: Cell<Option<i32>>,
    mode: Cell<Option<u32>>,
    progress: Cell<[u64; 6]>,
    locations: RefCell<[Option<Com>; 3]>,
    timer: Cell<(Instant, Duration, bool)>,
}
impl Dialog {
    fn show_if_needed(&self) -> HRESULT {
        let Some((started, owner, flags)) = self.started.get() else {
            return 0;
        };
        let counts = self.progress.get();
        let complete = counts
            .chunks_exact(2)
            .find(|pair| pair[1] > 0)
            .is_some_and(|pair| pair[0] >= pair[1]);
        if self.visible.get()
            || self.presentation_pause.get().is_some()
            || started.elapsed() < DELAY
            || complete
        {
            return 0;
        }
        let vtable = self.delegate.vtable::<DialogVtable>();
        let pointer = self.delegate.0;
        self.visible.set(true);
        let result = unsafe { (vtable.start)(pointer, owner, flags) };
        if result < 0 {
            self.visible.set(false);
            return result;
        }
        // Take owned snapshots before COM calls, which may pump messages. No
        // RefCell borrow or mutable reference is held across a native call.
        let retained_locations = self.locations.borrow().clone();
        let locations = retained_locations
            .each_ref()
            .map(|item| item.as_ref().map_or(ptr::null_mut(), |item| item.0));
        unsafe {
            if let Some(action) = self.operation.get() {
                (vtable.operation)(pointer, action);
            }
            if let Some(mode) = self.mode.get() {
                (vtable.mode)(pointer, mode);
            }
            (vtable.locations)(pointer, locations[0], locations[1], locations[2]);
            (vtable.progress)(
                pointer, counts[0], counts[1], counts[2], counts[3], counts[4], counts[5],
            );
            if self.timer.get().2 {
                (vtable.pause)(pointer);
            }
        }
        0
    }

    fn elapsed(&self) -> Duration {
        let (epoch, accumulated, paused) = self.timer.get();
        accumulated
            + if paused {
                Duration::ZERO
            } else {
                epoch.elapsed()
            }
    }

    fn stop(&self) -> HRESULT {
        self.started.set(None);
        self.presentation_pause.set(None);
        if self.visible.replace(false) {
            unsafe { (self.delegate.vtable::<DialogVtable>().stop)(self.delegate.0) }
        } else {
            0
        }
    }
}
impl Drop for Dialog {
    fn drop(&mut self) {
        self.stop();
    }
}

unsafe fn dialog<'a>(this: *mut c_void) -> &'a Dialog {
    unsafe { &*this.cast::<Dialog>() }
}

unsafe extern "system" fn query(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
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
    if same(&IID_UNKNOWN) || same(&IID_DIALOG) {
        unsafe {
            *output = this;
            add_ref(this)
        };
        0
    } else {
        E_NOINTERFACE
    }
}
unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    unsafe { dialog(this) }
        .references
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let previous = unsafe { dialog(this) }
        .references
        .fetch_sub(1, Ordering::AcqRel);
    if previous == 1 {
        unsafe { drop(Box::from_raw(this.cast::<Dialog>())) };
    }
    previous - 1
}
unsafe extern "system" fn start(this: *mut c_void, owner: HWND, flags: u32) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    dialog.stop();
    dialog.started.set(Some((Instant::now(), owner, flags)));
    dialog.presentation_pause.set(None);
    dialog.progress.set([0; 6]);
    dialog.timer.set((Instant::now(), Duration::ZERO, false));
    0
}
unsafe extern "system" fn stop(this: *mut c_void) -> HRESULT {
    unsafe { dialog(this) }.stop()
}
unsafe extern "system" fn operation(this: *mut c_void, action: i32) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    dialog.operation.set(Some(action));
    if dialog.visible.get() {
        unsafe { (dialog.delegate.vtable::<DialogVtable>().operation)(dialog.delegate.0, action) }
    } else {
        0
    }
}
unsafe extern "system" fn mode(this: *mut c_void, mode: u32) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    dialog.mode.set(Some(mode));
    if dialog.visible.get() {
        unsafe { (dialog.delegate.vtable::<DialogVtable>().mode)(dialog.delegate.0, mode) }
    } else {
        0
    }
}
unsafe extern "system" fn progress(
    this: *mut c_void,
    a: u64,
    b: u64,
    c: u64,
    d: u64,
    e: u64,
    f: u64,
) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    dialog.progress.set([a, b, c, d, e, f]);
    let result = dialog.show_if_needed();
    if result < 0 {
        return result;
    }
    if dialog.visible.get() {
        unsafe {
            (dialog.delegate.vtable::<DialogVtable>().progress)(dialog.delegate.0, a, b, c, d, e, f)
        }
    } else {
        0
    }
}
unsafe extern "system" fn locations(
    this: *mut c_void,
    source: *mut c_void,
    target: *mut c_void,
    item: *mut c_void,
) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    let retained = [source, target, item].map(|pointer| unsafe { Com::retain(pointer) });
    let previous = dialog.locations.replace(retained);
    drop(previous);
    if dialog.visible.get() {
        unsafe {
            (dialog.delegate.vtable::<DialogVtable>().locations)(
                dialog.delegate.0,
                source,
                target,
                item,
            )
        }
    } else {
        0
    }
}
unsafe extern "system" fn reset(this: *mut c_void) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    dialog.timer.set((Instant::now(), Duration::ZERO, false));
    if dialog.visible.get() {
        unsafe { (dialog.delegate.vtable::<DialogVtable>().reset)(dialog.delegate.0) }
    } else {
        0
    }
}
unsafe extern "system" fn pause(this: *mut c_void) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    if dialog.presentation_pause.get().is_none() {
        dialog.presentation_pause.set(Some(Instant::now()));
    }
    dialog.timer.set((Instant::now(), dialog.elapsed(), true));
    if dialog.visible.get() {
        unsafe { (dialog.delegate.vtable::<DialogVtable>().pause)(dialog.delegate.0) }
    } else {
        0
    }
}
unsafe extern "system" fn resume(this: *mut c_void) -> HRESULT {
    let dialog = unsafe { dialog(this) };
    if let Some(paused) = dialog.presentation_pause.take()
        && let Some((started, owner, flags)) = dialog.started.get()
    {
        dialog
            .started
            .set(Some((started + paused.elapsed(), owner, flags)));
    }
    dialog.timer.set((Instant::now(), dialog.elapsed(), false));
    if dialog.visible.get() {
        unsafe { (dialog.delegate.vtable::<DialogVtable>().resume)(dialog.delegate.0) }
    } else {
        0
    }
}
unsafe extern "system" fn milliseconds(
    this: *mut c_void,
    elapsed: *mut u64,
    remaining: *mut u64,
) -> HRESULT {
    if elapsed.is_null() || remaining.is_null() {
        return E_POINTER;
    }
    let dialog = unsafe { dialog(this) };
    if dialog.visible.get() {
        unsafe {
            (dialog.delegate.vtable::<DialogVtable>().milliseconds)(
                dialog.delegate.0,
                elapsed,
                remaining,
            )
        }
    } else {
        unsafe {
            *elapsed = dialog.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            *remaining = 0;
        }
        0
    }
}
unsafe extern "system" fn status(this: *mut c_void, output: *mut i32) -> HRESULT {
    if output.is_null() {
        return E_POINTER;
    }
    let dialog = unsafe { dialog(this) };
    let result = dialog.show_if_needed();
    if result < 0 {
        return result;
    }
    if dialog.visible.get() {
        unsafe { (dialog.delegate.vtable::<DialogVtable>().status)(dialog.delegate.0, output) }
    } else {
        unsafe {
            *output = if dialog.started.get().is_some() {
                PDOPS_RUNNING
            } else {
                PDOPS_STOPPED
            };
        }
        0
    }
}

static VTABLE: DialogVtable = DialogVtable {
    unknown: UnknownVtable {
        query,
        add_ref,
        release,
    },
    start,
    stop,
    operation,
    mode,
    progress,
    locations,
    reset,
    pause,
    resume,
    milliseconds,
    status,
};

#[cfg(test)]
mod tests;
