use super::*;
use std::rc::Rc;
const PDOPS_CANCELLED: i32 = 3;

#[derive(Debug, PartialEq)]
enum Call {
    Start(usize, u32),
    Stop,
    Operation(i32),
    Mode(u32),
    Progress([u64; 6]),
    Locations([usize; 3]),
    Reset,
    Pause,
    Resume,
    Status,
}

#[repr(C)]
struct Fake {
    vtable: &'static DialogVtable,
    references: Cell<u32>,
    calls: Rc<RefCell<Vec<Call>>>,
    destroyed: Rc<Cell<bool>>,
}

fn fake(this: *mut c_void) -> &'static Fake {
    unsafe { &*this.cast::<Fake>() }
}
unsafe extern "system" fn fake_query(
    _: *mut c_void,
    _: *const GUID,
    _: *mut *mut c_void,
) -> HRESULT {
    E_NOINTERFACE
}
unsafe extern "system" fn fake_add_ref(this: *mut c_void) -> u32 {
    let value = fake(this);
    value.references.set(value.references.get() + 1);
    value.references.get()
}
unsafe extern "system" fn fake_release(this: *mut c_void) -> u32 {
    let value = fake(this);
    let remaining = value.references.get() - 1;
    value.references.set(remaining);
    if remaining == 0 {
        value.destroyed.set(true);
        unsafe { drop(Box::from_raw(this.cast::<Fake>())) };
    }
    remaining
}
unsafe extern "system" fn fake_start(this: *mut c_void, owner: HWND, flags: u32) -> HRESULT {
    fake(this)
        .calls
        .borrow_mut()
        .push(Call::Start(owner as usize, flags));
    0
}
unsafe extern "system" fn fake_stop(this: *mut c_void) -> HRESULT {
    fake(this).calls.borrow_mut().push(Call::Stop);
    0
}
unsafe extern "system" fn fake_operation(this: *mut c_void, action: i32) -> HRESULT {
    fake(this).calls.borrow_mut().push(Call::Operation(action));
    0
}
unsafe extern "system" fn fake_mode(this: *mut c_void, mode: u32) -> HRESULT {
    fake(this).calls.borrow_mut().push(Call::Mode(mode));
    0
}
unsafe extern "system" fn fake_progress(
    this: *mut c_void,
    a: u64,
    b: u64,
    c: u64,
    d: u64,
    e: u64,
    f: u64,
) -> HRESULT {
    fake(this)
        .calls
        .borrow_mut()
        .push(Call::Progress([a, b, c, d, e, f]));
    0
}
unsafe extern "system" fn fake_locations(
    this: *mut c_void,
    a: *mut c_void,
    b: *mut c_void,
    c: *mut c_void,
) -> HRESULT {
    fake(this)
        .calls
        .borrow_mut()
        .push(Call::Locations([a as usize, b as usize, c as usize]));
    0
}
unsafe extern "system" fn fake_reset(this: *mut c_void) -> HRESULT {
    fake(this).calls.borrow_mut().push(Call::Reset);
    0
}
unsafe extern "system" fn fake_pause(this: *mut c_void) -> HRESULT {
    fake(this).calls.borrow_mut().push(Call::Pause);
    0
}
unsafe extern "system" fn fake_resume(this: *mut c_void) -> HRESULT {
    fake(this).calls.borrow_mut().push(Call::Resume);
    0
}
unsafe extern "system" fn fake_milliseconds(
    _: *mut c_void,
    elapsed: *mut u64,
    remaining: *mut u64,
) -> HRESULT {
    unsafe {
        *elapsed = 17;
        *remaining = 23;
    }
    0
}
unsafe extern "system" fn fake_status(this: *mut c_void, output: *mut i32) -> HRESULT {
    fake(this).calls.borrow_mut().push(Call::Status);
    unsafe {
        *output = PDOPS_CANCELLED;
    }
    0
}
static FAKE_VTABLE: DialogVtable = DialogVtable {
    unknown: UnknownVtable {
        query: fake_query,
        add_ref: fake_add_ref,
        release: fake_release,
    },
    start: fake_start,
    stop: fake_stop,
    operation: fake_operation,
    mode: fake_mode,
    progress: fake_progress,
    locations: fake_locations,
    reset: fake_reset,
    pause: fake_pause,
    resume: fake_resume,
    milliseconds: fake_milliseconds,
    status: fake_status,
};

struct Harness {
    adapter: DelayedProgressDialog,
    calls: Rc<RefCell<Vec<Call>>>,
    destroyed: Rc<Cell<bool>>,
}
impl Harness {
    fn new() -> Self {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let destroyed = Rc::new(Cell::new(false));
        let delegate = Com(Box::into_raw(Box::new(Fake {
            vtable: &FAKE_VTABLE,
            references: Cell::new(1),
            calls: calls.clone(),
            destroyed: destroyed.clone(),
        }))
        .cast());
        Self {
            adapter: DelayedProgressDialog::with_delegate(delegate),
            calls,
            destroyed,
        }
    }
    fn pointer(&self) -> *mut c_void {
        self.adapter.as_raw()
    }
    fn begin(&self) {
        assert_eq!(unsafe { start(self.pointer(), 123usize as HWND, 128) }, 0);
    }
    fn age(&self) {
        let dialog = unsafe { dialog(self.pointer()) };
        let (_, owner, flags) = dialog.started.get().unwrap();
        dialog.started.set(Some((
            Instant::now() - DELAY - Duration::from_millis(1),
            owner,
            flags,
        )));
    }
    fn update(&self, current: u64, total: u64) {
        assert_eq!(
            unsafe { progress(self.pointer(), current, total, current, total, 0, 1) },
            0
        );
    }
}

#[test]
fn short_operations_never_start_a_window_even_at_one_hundred_percent() {
    let h = Harness::new();
    h.begin();
    h.update(0, 100);
    h.update(100, 100);
    assert_eq!(unsafe { stop(h.pointer()) }, 0);
    assert!(h.calls.borrow().is_empty());
}

#[test]
fn a_slow_operation_whose_first_update_is_complete_never_flashes() {
    let h = Harness::new();
    h.begin();
    h.age();
    h.update(100, 100);
    let mut state = 0;
    assert_eq!(unsafe { status(h.pointer(), &mut state) }, 0);
    assert_eq!(state, PDOPS_RUNNING);
    assert!(h.calls.borrow().is_empty());
}

#[test]
fn long_operations_replay_native_state_and_keep_cancellation_and_progress() {
    let h = Harness::new();
    h.begin();
    unsafe {
        operation(h.pointer(), 2);
        mode(h.pointer(), 1);
    }
    h.update(3, 100);
    h.age();
    let large = 1u64 << 40;
    assert_eq!(
        unsafe { progress(h.pointer(), 10, 100, large, large * 2, 4, 10) },
        0
    );
    let calls = h.calls.borrow();
    assert_eq!(
        &calls[..4],
        &[
            Call::Start(123, 128),
            Call::Operation(2),
            Call::Mode(1),
            Call::Locations([0; 3])
        ]
    );
    assert!(calls.contains(&Call::Progress([10, 100, large, large * 2, 4, 10])));
    drop(calls);
    let mut state = 0;
    assert_eq!(unsafe { status(h.pointer(), &mut state) }, 0);
    assert_eq!(state, PDOPS_CANCELLED);
    h.update(100, 100);
    unsafe {
        stop(h.pointer());
        stop(h.pointer());
    }
    assert_eq!(
        h.calls
            .borrow()
            .iter()
            .filter(|call| **call == Call::Stop)
            .count(),
        1
    );
}

#[test]
fn status_polling_can_reveal_indeterminate_work_and_stop_prevents_late_reveal() {
    let h = Harness::new();
    h.begin();
    h.age();
    let mut state = 0;
    unsafe {
        status(h.pointer(), &mut state);
    }
    assert!(h.calls.borrow().contains(&Call::Start(123, 128)));
    let h = Harness::new();
    h.begin();
    h.age();
    unsafe {
        stop(h.pointer());
        status(h.pointer(), &mut state);
    }
    h.update(1, 100);
    assert!(h.calls.borrow().is_empty());
}

#[test]
fn locations_are_retained_until_replaced_or_the_adapter_is_released() {
    let h = Harness::new();
    let item = Harness::new();
    let item_pointer = unsafe { dialog(item.pointer()) }.delegate.0;
    unsafe {
        locations(h.pointer(), item_pointer, ptr::null_mut(), item_pointer);
    }
    assert_eq!(fake(item_pointer).references.get(), 3);
    h.begin();
    h.age();
    h.update(1, 100);
    assert!(h.calls.borrow().contains(&Call::Locations([
        item_pointer as usize,
        0,
        item_pointer as usize
    ])));
    assert_eq!(fake(item_pointer).references.get(), 3);
    unsafe {
        locations(
            h.pointer(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        );
    }
    assert_eq!(fake(item_pointer).references.get(), 1);
}

#[test]
fn query_interface_retains_identity_and_drop_closes_the_native_window() {
    let h = Harness::new();
    h.begin();
    h.age();
    h.update(1, 100);
    let mut pointer = ptr::null_mut();
    assert_eq!(unsafe { query(h.pointer(), &IID_UNKNOWN, &mut pointer) }, 0);
    assert_eq!(pointer, h.pointer());
    let calls = h.calls.clone();
    let destroyed = h.destroyed.clone();
    drop(h);
    assert!(!destroyed.get());
    assert_eq!(unsafe { release(pointer) }, 0);
    assert!(destroyed.get());
    assert_eq!(calls.borrow().last(), Some(&Call::Stop));
}

#[test]
fn time_spent_paused_does_not_trigger_a_progress_window_on_resume() {
    let h = Harness::new();
    h.begin();
    h.age();
    let dialog = unsafe { dialog(h.pointer()) };
    let (started, _, _) = dialog.started.get().unwrap();
    dialog.presentation_pause.set(Some(started));
    let mut state = 0;
    unsafe {
        status(h.pointer(), &mut state);
        resume(h.pointer());
        status(h.pointer(), &mut state);
    }
    assert!(h.calls.borrow().is_empty());
    assert_eq!(state, PDOPS_RUNNING);
}

#[test]
fn hidden_timer_queries_and_invalid_outputs_do_not_show_progress() {
    let h = Harness::new();
    h.begin();
    let mut elapsed = 1;
    let mut remaining = 1;
    unsafe {
        pause(h.pointer());
        milliseconds(h.pointer(), &mut elapsed, &mut remaining);
        resume(h.pointer());
        reset(h.pointer());
    }
    assert_eq!(remaining, 0);
    assert_eq!(
        unsafe { milliseconds(h.pointer(), ptr::null_mut(), &mut remaining) },
        E_POINTER
    );
    assert_eq!(unsafe { status(h.pointer(), ptr::null_mut()) }, E_POINTER);
    let mut pointer = h.pointer();
    assert_eq!(
        unsafe { query(h.pointer(), ptr::null(), &mut pointer) },
        E_POINTER
    );
    assert!(pointer.is_null());
    assert!(h.calls.borrow().is_empty());
}
