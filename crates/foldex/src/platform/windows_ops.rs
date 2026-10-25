//! User file operations belong to the Windows Shell. This module deliberately
//! leaves its progress, collision, elevation and retry dialogs enabled.
//!
//! Everything COM-owned is created, used and released on the existing bounded
//! file worker. No interface pointer escapes that STA or survives a job.

use super::{MAX_SELECTION, check_stop, invalid, validate_name};
use crate::operation_history::{Item, Mutation, RecycleToken, Replay, Step, Target};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::c_void,
    io,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU32, AtomicUsize, Ordering},
    },
};
use windows_sys::{
    Win32::{
        Foundation::HWND,
        Storage::FileSystem::{GetVolumeNameForVolumeMountPointW, GetVolumePathNameW},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
            Ole::{OleInitialize, OleUninitialize},
            Threading::GetCurrentProcessId,
        },
        UI::{
            Shell::{
                COPYENGINE_E_USER_CANCELLED, COPYENGINE_S_ALREADY_DONE, COPYENGINE_S_PENDING,
                COPYENGINE_S_USER_IGNORED, FOF_ALLOWUNDO, FOF_NOCONFIRMMKDIR,
                FOF_RENAMEONCOLLISION, FOF_WANTNUKEWARNING, FOFX_ADDUNDORECORD,
                FOFX_PRESERVEFILEEXTENSIONS, FOFX_RECYCLEONDELETE, SHCreateItemFromParsingName,
                SICHINT_CANONICAL, SIGDN_FILESYSPATH, TSF_DELETE_RECYCLE_IF_POSSIBLE,
                TSF_OVERWRITE_EXIST,
            },
            WindowsAndMessaging::{
                DispatchMessageW, GetWindowThreadProcessId, IsWindow, MSG, PM_REMOVE, PeekMessageW,
                TranslateMessage, WM_QUIT,
            },
        },
    },
    core::GUID,
};

#[cfg(test)]
use windows_sys::Win32::UI::Shell::COPYENGINE_S_DONT_PROCESS_CHILDREN;

#[path = "windows_clipboard.rs"]
mod clipboard;
#[path = "windows_history.rs"]
mod history;
#[cfg(test)]
pub(super) use clipboard::{clipboard_encoding_for_test, paste};
pub(super) use clipboard::{paste_tracked, write_clipboard};
pub(crate) use history::replay_step;
#[cfg(test)]
#[path = "windows_operation_dialog_probe.rs"]
mod dialog_probe;
#[cfg(test)]
#[path = "windows_undo_probe.rs"]
mod undo_probe;

const CLSID_FILE_OPERATION: GUID = GUID::from_u128(0x3ad05575_8857_4850_9277_11b85bdb8e09);
const IID_FILE_OPERATION: GUID = GUID::from_u128(0x947aab5f_0a5c_4c13_b4d6_4bf7836fc9f8);
const IID_SHELL_ITEM: GUID = GUID::from_u128(0x43826d1e_e718_42ee_bc55_a1e261c37bfe);
const IID_UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_PROGRESS_SINK: GUID = GUID::from_u128(0x04b0f1a7_9490_44bc_96e1_4296a31252e2);
const E_ABORT: i32 = 0x80004004u32 as i32;
const E_NOINTERFACE: i32 = 0x80004002u32 as i32;
const E_POINTER: i32 = 0x80004003u32 as i32;
const ERROR_CANCELLED_HRESULT: i32 = 0x800704c7u32 as i32;
const MAX_PATH_BYTES: usize = 1024 * 1024;
const PENDING: u8 = 0;
const SUCCEEDED: u8 = 1;
const SKIPPED: u8 = 2;
const FAILED: u8 = 3;

struct Apartment;
impl Apartment {
    fn new() -> io::Result<Self> {
        // OleInitialize initializes an STA and is required by the OLE clipboard.
        // This never changes the eframe/UI thread's apartment.
        hresult(
            unsafe { OleInitialize(ptr::null()) },
            "Initialize Windows Shell",
        )?;
        pump_messages();
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        pump_messages();
        unsafe { OleUninitialize() };
    }
}

fn pump_messages() {
    // PerformOperations supplies the Shell's modal progress/message loop. Also
    // dispatch our STA's messages around calls and at progress callbacks. Bound
    // each drain so a posted-message flood cannot prevent shutdown checks.
    for _ in 0..64 {
        let mut message = MSG::default();
        if unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 {
            break;
        }
        if message.message == WM_QUIT {
            break;
        }
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn hresult(result: i32, action: &str) -> io::Result<()> {
    if result >= 0 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{action}: Windows HRESULT 0x{:08X}",
            result as u32
        )))
    }
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut units: Vec<_> = path.as_os_str().encode_wide().collect();
    if !path.is_absolute() || units.is_empty() || units.len() > 32_767 || units.contains(&0) {
        return Err(invalid(
            "Use an absolute Windows path without null characters",
        ));
    }
    // Canonicalized paths may carry the verbatim prefix; parsing a normal drive
    // or UNC path is supported more consistently by third-party Shell handlers.
    if units.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        units.drain(..8);
        units.splice(..0, [92, 92]);
    } else if units.starts_with(&[92, 92, 63, 92])
        && units.len() >= 7
        && matches!(units[4], 65..=90 | 97..=122)
        && units[5] == 58
    {
        units.drain(..4);
    }
    units.push(0);
    Ok(units)
}

fn validate_paths(paths: &[PathBuf]) -> io::Result<()> {
    if paths.is_empty() || paths.len() > MAX_SELECTION {
        return Err(invalid("Select between 1 and 4,096 items"));
    }
    let mut bytes = 0usize;
    let mut unique = HashSet::with_capacity(paths.len());
    for path in paths {
        let units = wide(path)?;
        bytes = bytes.saturating_add(units.len().saturating_mul(2));
        if bytes > MAX_PATH_BYTES {
            return Err(invalid("Selected paths exceed the 1 MiB operation budget"));
        }
        if path.file_name().is_none() {
            return Err(invalid(
                "A drive or share root cannot be copied, moved, renamed or recycled",
            ));
        }
        if !unique.insert(path) {
            return Err(invalid("The same source was selected more than once"));
        }
    }
    Ok(())
}

fn owner_window(owner: isize) -> HWND {
    let window = owner as HWND;
    if window.is_null() || unsafe { IsWindow(window) } == 0 {
        return ptr::null_mut();
    }
    let mut process = 0;
    unsafe { GetWindowThreadProcessId(window, &mut process) };
    if process == unsafe { GetCurrentProcessId() } {
        window
    } else {
        ptr::null_mut()
    }
}

#[repr(C)]
struct UnknownVtable {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

struct ComPointer(*mut c_void);
impl ComPointer {
    unsafe fn vtable<T>(&self) -> &T {
        unsafe { &**self.0.cast::<*const T>() }
    }
    fn checked(pointer: *mut c_void, description: &str) -> io::Result<Self> {
        if pointer.is_null() {
            Err(io::Error::other(format!(
                "Windows returned an empty {description}"
            )))
        } else {
            Ok(Self(pointer))
        }
    }
}
impl Clone for ComPointer {
    fn clone(&self) -> Self {
        unsafe { (self.vtable::<UnknownVtable>().add_ref)(self.0) };
        Self(self.0)
    }
}
impl Drop for ComPointer {
    fn drop(&mut self) {
        unsafe { (self.vtable::<UnknownVtable>().release)(self.0) };
    }
}

#[repr(C)]
struct ShellItemVtable {
    unknown: UnknownVtable,
    bind_to_handler: usize,
    get_parent: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    get_display_name: unsafe extern "system" fn(*mut c_void, i32, *mut *mut u16) -> i32,
    get_attributes: usize,
    compare: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, *mut i32) -> i32,
}

fn shell_item(path: &Path) -> io::Result<ComPointer> {
    let name = wide(path)?;
    let mut item = ptr::null_mut();
    hresult(
        unsafe {
            SHCreateItemFromParsingName(name.as_ptr(), ptr::null_mut(), &IID_SHELL_ITEM, &mut item)
        },
        &format!("Locate {}", path.display()),
    )?;
    ComPointer::checked(item, "Shell item")
}

fn parent_item(item: &ComPointer) -> io::Result<ComPointer> {
    let mut parent = ptr::null_mut();
    hresult(
        unsafe { (item.vtable::<ShellItemVtable>().get_parent)(item.0, &mut parent) },
        "Locate source folder",
    )?;
    ComPointer::checked(parent, "source folder")
}

fn same_item(left: *mut c_void, right: *mut c_void) -> bool {
    if left.is_null() || right.is_null() {
        return false;
    }
    if left == right {
        return true;
    }
    let vtable = unsafe { &**left.cast::<*const ShellItemVtable>() };
    let mut order = 1;
    (unsafe { (vtable.compare)(left, right, SICHINT_CANONICAL as u32, &mut order) }) == 0
        && order == 0
}

// The slots below follow the SDK's IFileOperation ABI, including every unused
// member. Do not compact this vtable when adding another operation.
#[repr(C)]
struct FileOperationVtable {
    unknown: UnknownVtable, // 0..2
    advise: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut u32) -> i32, // 3
    unadvise: unsafe extern "system" fn(*mut c_void, u32) -> i32, // 4
    set_flags: unsafe extern "system" fn(*mut c_void, u32) -> i32, // 5
    set_progress_message: usize, // 6
    set_progress_dialog: usize, // 7: left unset to preserve Windows' dialog/chart
    set_properties: usize,  // 8
    set_owner: unsafe extern "system" fn(*mut c_void, HWND) -> i32, // 9
    apply_properties: [usize; 2], // 10..11
    rename_item:
        unsafe extern "system" fn(*mut c_void, *mut c_void, *const u16, *mut c_void) -> i32, // 12
    rename_items: usize,    // 13
    move_item: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const u16,
        *mut c_void,
    ) -> i32, // 14
    move_items: usize,      // 15
    copy_item: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *mut c_void,
        *const u16,
        *mut c_void,
    ) -> i32, // 16
    copy_items: usize,      // 17
    delete_item: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32, // 18
    delete_items: usize,    // 19
    new_item: usize,        // 20
    perform: unsafe extern "system" fn(*mut c_void) -> i32, // 21
    any_aborted: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32, // 22
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Copy,
    Duplicate,
    Move,
    Rename,
    Recycle,
    PermanentDelete,
}

fn operation_flags(mode: Mode) -> u32 {
    if mode == Mode::PermanentDelete {
        // Foldex's explicit permanent-delete modal already confirmed this
        // request. Keep progress, error, elevation and retry UI enabled.
        return FOF_NOCONFIRMMKDIR | windows_sys::Win32::UI::Shell::FOF_NOCONFIRMATION;
    }
    let base = FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | FOFX_ADDUNDORECORD;
    match mode {
        Mode::Duplicate => base | FOF_RENAMEONCOLLISION | FOFX_PRESERVEFILEEXTENSIONS,
        Mode::Recycle => {
            base | FOFX_RECYCLEONDELETE
                | FOF_WANTNUKEWARNING
                | windows_sys::Win32::UI::Shell::FOF_NOCONFIRMATION
        }
        _ => base,
    }
}

#[cfg(test)]
thread_local! { static NO_SESSION_UNDO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// Generated native probes must not disturb Explorer's user-session history.
/// This is thread-scoped; the caller must invoke its Shell work inside it.
#[cfg(test)]
pub(crate) fn without_session_undo<T>(operation: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            NO_SESSION_UNDO.with(|value| value.set(self.0));
        }
    }
    let _restore = Restore(NO_SESSION_UNDO.with(|value| value.replace(true)));
    operation()
}

fn effective_operation_flags(mode: Mode, replay: bool) -> u32 {
    let mut flags = operation_flags(mode);
    #[cfg(test)]
    let suppress_record = replay || NO_SESSION_UNDO.with(|value| value.get());
    #[cfg(not(test))]
    let suppress_record = replay;
    if suppress_record {
        flags &= !(FOF_ALLOWUNDO | FOFX_ADDUNDORECORD);
    }
    if replay && !matches!(mode, Mode::Recycle | Mode::PermanentDelete) {
        // A late collision preserves a distinct actual output, never a replace.
        flags |= FOF_RENAMEONCOLLISION | FOFX_PRESERVEFILEEXTENSIONS;
    }
    flags
}

struct ProgressState {
    stop: Arc<AtomicBool>,
    outcomes: Box<[AtomicU8]>,
    errors: Box<[AtomicI32]>,
    skipped: AtomicBool,
    rejected_delete: AtomicBool,
    finish_result: AtomicI32,
    created: Mutex<Vec<Option<history::Created>>>,
    created_bytes: AtomicUsize,
    replay: bool,
    replaced: AtomicBool,
    replay_guard: Option<history::ReplayGuard>,
    guard_error: Mutex<Option<String>>,
    roots: Mutex<BTreeMap<history::SourceKey, Vec<(history::SourceKey, usize)>>>,
    capture_errors: Mutex<Vec<Option<String>>>,
}
impl ProgressState {
    fn new(count: usize, stop: Arc<AtomicBool>) -> Arc<Self> {
        Arc::new(Self {
            stop,
            outcomes: (0..count).map(|_| AtomicU8::new(PENDING)).collect(),
            errors: (0..count).map(|_| AtomicI32::new(0)).collect(),
            skipped: AtomicBool::new(false),
            rejected_delete: AtomicBool::new(false),
            finish_result: AtomicI32::new(0),
            created: Mutex::new((0..count).map(|_| None).collect()),
            created_bytes: AtomicUsize::new(0),
            replay: false,
            replaced: AtomicBool::new(false),
            replay_guard: None,
            guard_error: Mutex::new(None),
            roots: Mutex::new(BTreeMap::new()),
            capture_errors: Mutex::new((0..count).map(|_| None).collect()),
        })
    }
}

#[repr(C)]
struct ProgressSinkVtable {
    unknown: UnknownVtable,
    start: unsafe extern "system" fn(*mut c_void) -> i32,
    finish: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    pre_rename: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *const u16) -> i32,
    post_rename: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *mut c_void,
        *const u16,
        i32,
        *mut c_void,
    ) -> i32,
    pre_move:
        unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut c_void, *const u16) -> i32,
    post_move: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *mut c_void,
        *mut c_void,
        *const u16,
        i32,
        *mut c_void,
    ) -> i32,
    pre_copy:
        unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *mut c_void, *const u16) -> i32,
    post_copy: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *mut c_void,
        *mut c_void,
        *const u16,
        i32,
        *mut c_void,
    ) -> i32,
    pre_delete: unsafe extern "system" fn(*mut c_void, u32, *mut c_void) -> i32,
    post_delete: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, i32, *mut c_void) -> i32,
    pre_new: unsafe extern "system" fn(*mut c_void, u32, *mut c_void, *const u16) -> i32,
    post_new: unsafe extern "system" fn(
        *mut c_void,
        u32,
        *mut c_void,
        *const u16,
        *const u16,
        u32,
        i32,
        *mut c_void,
    ) -> i32,
    progress: unsafe extern "system" fn(*mut c_void, u32, u32) -> i32,
    reset: unsafe extern "system" fn(*mut c_void) -> i32,
    pause: unsafe extern "system" fn(*mut c_void) -> i32,
    resume: unsafe extern "system" fn(*mut c_void) -> i32,
}

#[repr(C)]
struct ProgressSink {
    vtable: &'static ProgressSinkVtable,
    references: AtomicU32,
    state: Arc<ProgressState>,
    root: Option<(usize, ComPointer)>,
    recycle: bool,
}

fn guid_equal(left: &GUID, right: &GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}
unsafe extern "system" fn sink_query(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> i32 {
    if output.is_null() || iid.is_null() {
        return E_POINTER;
    }
    unsafe {
        *output = ptr::null_mut();
    }
    if unsafe { guid_equal(&*iid, &IID_UNKNOWN) || guid_equal(&*iid, &IID_PROGRESS_SINK) } {
        unsafe {
            *output = this;
            sink_add_ref(this);
        }
        0
    } else {
        E_NOINTERFACE
    }
}
unsafe extern "system" fn sink_add_ref(this: *mut c_void) -> u32 {
    unsafe { &*this.cast::<ProgressSink>() }
        .references
        .fetch_add(1, Ordering::Relaxed)
        + 1
}
unsafe extern "system" fn sink_release(this: *mut c_void) -> u32 {
    let previous = unsafe { &*this.cast::<ProgressSink>() }
        .references
        .fetch_sub(1, Ordering::AcqRel);
    if previous == 1 {
        unsafe {
            drop(Box::from_raw(this.cast::<ProgressSink>()));
        }
    }
    previous - 1
}
fn sink_check(this: *mut c_void) -> i32 {
    let sink = unsafe { &*this.cast::<ProgressSink>() };
    pump_messages();
    if sink.state.stop.load(Ordering::Acquire) {
        E_ABORT
    } else {
        0
    }
}
unsafe extern "system" fn sink_start(this: *mut c_void) -> i32 {
    sink_check(this)
}
unsafe extern "system" fn sink_finish(this: *mut c_void, result: i32) -> i32 {
    let sink = unsafe { &*this.cast::<ProgressSink>() };
    if sink.root.is_none() {
        sink.state.finish_result.store(result, Ordering::Release);
    }
    0
}
unsafe extern "system" fn sink_pre_name(
    this: *mut c_void,
    flags: u32,
    item: *mut c_void,
    _: *const u16,
) -> i32 {
    if reject_replay_replacement(this, flags) {
        return E_ABORT;
    }
    if check_replay_guard(this, item) < 0 {
        return E_ABORT;
    }
    sink_check(this)
}
unsafe extern "system" fn sink_pre_transfer(
    this: *mut c_void,
    flags: u32,
    item: *mut c_void,
    _: *mut c_void,
    _: *const u16,
) -> i32 {
    if reject_replay_replacement(this, flags) {
        return E_ABORT;
    }
    if check_replay_guard(this, item) < 0 {
        return E_ABORT;
    }
    sink_check(this)
}
fn reject_replay_replacement(this: *mut c_void, flags: u32) -> bool {
    let sink = unsafe { &*this.cast::<ProgressSink>() };
    if flags & TSF_OVERWRITE_EXIST as u32 != 0 {
        sink.state.replaced.store(true, Ordering::Release);
        return sink.state.replay;
    }
    false
}
fn check_replay_guard(this: *mut c_void, item: *mut c_void) -> i32 {
    let sink = unsafe { &*this.cast::<ProgressSink>() };
    if root_index(sink, item).is_some()
        && let Some(guard) = &sink.state.replay_guard
        && let Err(error) = guard.validate()
    {
        *sink
            .state
            .guard_error
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(error.to_string());
        return E_ABORT;
    }
    0
}

fn root_index(sink: &ProgressSink, item: *mut c_void) -> Option<usize> {
    if let Ok(key) = history::source_key(item) {
        let roots = sink
            .state
            .roots
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(candidates) = roots.get(&key) {
            if candidates.len() == 1 {
                return Some(candidates[0].1);
            }
            // Case-sensitive Windows directories can contain distinct names
            // that are equal under ordinal ignore-case comparison. Never
            // assign one file's completion to a different selected source.
            let mut exact = candidates
                .iter()
                .filter(|(candidate, _)| candidate.same_spelling(&key));
            if let Some((_, index)) = exact.next()
                && exact.next().is_none()
            {
                return Some(*index);
            }
        }
        // A different known path is a child notification, even when a Shell
        // comparison happens to equate an alias/hard link with a root item.
        return None;
    }
    sink.root
        .as_ref()
        .and_then(|(index, root)| same_item(root.0, item).then_some(*index))
}

#[cfg(test)]
fn native_trace_slot() -> bool {
    thread_local! { static COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
    COUNT.with(|count| {
        let value = count.get();
        count.set(value.saturating_add(1));
        value < 256
    })
}

#[cfg(test)]
fn native_post_diagnostic(
    sink: &ProgressSink,
    item: *mut c_void,
    result: i32,
    created: *mut c_void,
    deletion: bool,
    index: Option<usize>,
) {
    if !native_trace_slot() {
        return;
    }
    let source = history::source_key(item)
        .map(|key| key.diagnostic())
        .unwrap_or_else(|error| error.to_string());
    eprintln!(
        "FOLDEX_NATIVE_POST root_sink={:?} matched={index:?} deletion={deletion} hr=0x{:08X} created={} source={source}",
        sink.root.as_ref().map(|(index, _)| index),
        result as u32,
        !created.is_null()
    );
}
unsafe extern "system" fn sink_pre_delete(this: *mut c_void, flags: u32, item: *mut c_void) -> i32 {
    let sink = unsafe { &*this.cast::<ProgressSink>() };
    if sink.recycle && flags & TSF_DELETE_RECYCLE_IF_POSSIBLE as u32 == 0 {
        sink.state.rejected_delete.store(true, Ordering::Release);
        // Recycle remains recycle-only, including on network/removable volumes.
        // Never silently turn this command into permanent deletion.
        return E_ABORT;
    }
    if check_replay_guard(this, item) < 0 {
        return E_ABORT;
    }
    sink_check(this)
}

fn record_item(
    this: *mut c_void,
    item: *mut c_void,
    result: i32,
    created: *mut c_void,
    deletion: bool,
) -> i32 {
    let sink = unsafe { &*this.cast::<ProgressSink>() };
    let ignored = matches!(
        result,
        COPYENGINE_S_USER_IGNORED | COPYENGINE_S_ALREADY_DONE
    );
    if ignored {
        sink.state.skipped.store(true, Ordering::Release);
    }
    let index = root_index(sink, item);
    #[cfg(test)]
    if std::env::var("FOLDEX_NATIVE_UNDO_PROBE").as_deref() == Ok("1") {
        native_post_diagnostic(sink, item, result, created, deletion, index);
    }
    if let Some(index) = index {
        let outcome = item_outcome(result, !created.is_null(), deletion);
        if outcome == SUCCEEDED && !created.is_null() {
            // Copy path/PIDL data while this apartment owns the callback's
            // borrowed IShellItem. Never retain or marshal a COM pointer.
            match history::capture_created(created, sink.recycle) {
                Ok(output) => {
                    let bytes = output.path.as_os_str().len().saturating_mul(2)
                        + output.recycle.as_ref().map_or(0, |token| token.pidl.len());
                    let mut outputs = sink
                        .state
                        .created
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if outputs[index].is_none()
                        && sink
                            .state
                            .created_bytes
                            .fetch_add(bytes, Ordering::AcqRel)
                            .saturating_add(bytes)
                            <= 4 * 1024 * 1024
                    {
                        outputs[index] = Some(output);
                        sink.state
                            .capture_errors
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())[index] = None;
                    }
                }
                Err(error) => {
                    let error: String = error.to_string().chars().take(256).collect();
                    #[cfg(test)]
                    if std::env::var("FOLDEX_NATIVE_UNDO_PROBE").as_deref() == Ok("1")
                        && native_trace_slot()
                    {
                        eprintln!("FOLDEX_NATIVE_CAPTURE_ERROR index={index} error={error}");
                    }
                    let mut errors = sink
                        .state
                        .capture_errors
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    if errors[index].is_none() {
                        errors[index] = Some(error);
                    }
                }
            }
        }
        sink.state.errors[index].store(result, Ordering::Release);
        sink.state.outcomes[index].store(outcome, Ordering::Release);
    }
    // Return S_OK after failures so Windows can complete its own retry/skip UI.
    // Returning the item's failure here would cancel unrelated queued items.
    0
}

fn item_outcome(result: i32, has_created_item: bool, deletion: bool) -> u8 {
    if matches!(
        result,
        COPYENGINE_S_USER_IGNORED | COPYENGINE_S_ALREADY_DONE | COPYENGINE_S_PENDING
    ) {
        SKIPPED
    } else if result < 0 {
        FAILED
    } else if deletion || has_created_item {
        // COPYENGINE_S_DONT_PROCESS_CHILDREN (0x00270008) is the completed
        // non-recursive/optimized result used by native move, rename and
        // delete. It suppresses further child processing; it is not Skip.
        // A transfer still requires its resulting IShellItem for confirmation.
        SUCCEEDED
    } else {
        // S_OK without a resulting item is not proof of a successful transfer.
        SKIPPED
    }
}
unsafe extern "system" fn sink_post_name(
    this: *mut c_void,
    _: u32,
    item: *mut c_void,
    _: *const u16,
    result: i32,
    created: *mut c_void,
) -> i32 {
    record_item(this, item, result, created, false)
}
unsafe extern "system" fn sink_post_transfer(
    this: *mut c_void,
    _: u32,
    item: *mut c_void,
    _: *mut c_void,
    _: *const u16,
    result: i32,
    created: *mut c_void,
) -> i32 {
    record_item(this, item, result, created, false)
}
unsafe extern "system" fn sink_post_delete(
    this: *mut c_void,
    _: u32,
    item: *mut c_void,
    result: i32,
    created: *mut c_void,
) -> i32 {
    record_item(this, item, result, created, true)
}
unsafe extern "system" fn sink_post_new(
    _: *mut c_void,
    _: u32,
    _: *mut c_void,
    _: *const u16,
    _: *const u16,
    _: u32,
    _: i32,
    _: *mut c_void,
) -> i32 {
    0
}
unsafe extern "system" fn sink_progress(this: *mut c_void, _: u32, _: u32) -> i32 {
    sink_check(this)
}

static PROGRESS_VTABLE: ProgressSinkVtable = ProgressSinkVtable {
    unknown: UnknownVtable {
        query_interface: sink_query,
        add_ref: sink_add_ref,
        release: sink_release,
    },
    start: sink_start,
    finish: sink_finish,
    pre_rename: sink_pre_name,
    post_rename: sink_post_name,
    pre_move: sink_pre_transfer,
    post_move: sink_post_transfer,
    pre_copy: sink_pre_transfer,
    post_copy: sink_post_transfer,
    pre_delete: sink_pre_delete,
    post_delete: sink_post_delete,
    pre_new: sink_pre_name,
    post_new: sink_post_new,
    progress: sink_progress,
    reset: sink_start,
    pause: sink_start,
    resume: sink_start,
};

fn progress_sink(
    state: Arc<ProgressState>,
    root: Option<(usize, ComPointer)>,
    recycle: bool,
) -> ComPointer {
    ComPointer(
        Box::into_raw(Box::new(ProgressSink {
            vtable: &PROGRESS_VTABLE,
            references: AtomicU32::new(1),
            state,
            root,
            recycle,
        }))
        .cast(),
    )
}

#[derive(Debug)]
struct Report {
    outcomes: Vec<u8>,
    aborted: bool,
    result: i32,
    skipped: bool,
    rejected_delete: bool,
    created: Vec<Option<history::Created>>,
    setup_error: Option<String>,
    replaced: bool,
    capture_errors: Vec<Option<String>>,
}
impl Report {
    fn full_success(&self) -> bool {
        self.setup_error.is_none()
            && !self.aborted
            && self.result >= 0
            && !self.skipped
            && !self.rejected_delete
            && self.outcomes.iter().all(|outcome| *outcome == SUCCEEDED)
    }
    fn message(&self, action: &str) -> io::Result<String> {
        if let Some(error) = &self.setup_error {
            return Err(io::Error::other(error.clone()));
        }
        let complete = self
            .outcomes
            .iter()
            .filter(|outcome| **outcome == SUCCEEDED)
            .count();
        let count = self.outcomes.len();
        if self.rejected_delete {
            return Err(io::Error::other(format!(
                "Recycled {complete}/{count} items. Windows could not recycle a remaining item; permanent deletion was refused"
            )));
        }
        if self.aborted || is_cancel(self.result) {
            return Ok(format!(
                "Canceled or stopped: {action} {complete}/{count} selected items. Completed changes were kept"
            ));
        }
        hresult(
            self.result,
            &format!("Windows {action} completed {complete}/{count} items"),
        )?;
        if self.full_success() {
            Ok(format!(
                "{action} {complete} item{}",
                if complete == 1 { "" } else { "s" }
            ))
        } else if self.outcomes.contains(&PENDING) {
            Ok(format!(
                "Windows {action}: confirmed {complete}/{count} selected items; the remaining final statuses could not be verified"
            ))
        } else {
            Ok(format!(
                "{action} {complete}/{count} selected items; remaining items were skipped or unchanged"
            ))
        }
    }
}

fn is_cancel(result: i32) -> bool {
    matches!(
        result,
        E_ABORT | ERROR_CANCELLED_HRESULT | COPYENGINE_E_USER_CANCELLED
    )
}

struct Advice<'a> {
    operation: &'a ComPointer,
    cookie: u32,
}
impl Drop for Advice<'_> {
    fn drop(&mut self) {
        unsafe {
            (self.operation.vtable::<FileOperationVtable>().unadvise)(
                self.operation.0,
                self.cookie,
            );
        }
    }
}

fn perform_batch(
    items: &[(usize, &ComPointer)],
    destination: Option<&ComPointer>,
    name: Option<&[u16]>,
    mode: Mode,
    owner: isize,
    state: &Arc<ProgressState>,
) -> io::Result<(i32, bool)> {
    check_stop(&state.stop)?;
    let mut pointer = ptr::null_mut();
    hresult(
        unsafe {
            CoCreateInstance(
                &CLSID_FILE_OPERATION,
                ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_FILE_OPERATION,
                &mut pointer,
            )
        },
        "Create Windows file operation",
    )?;
    let operation = ComPointer::checked(pointer, "file operation")?;
    let vtable = unsafe { operation.vtable::<FileOperationVtable>() };
    hresult(
        unsafe { (vtable.set_flags)(operation.0, effective_operation_flags(mode, state.replay)) },
        "Set Windows file operation options",
    )?;
    hresult(
        unsafe { (vtable.set_owner)(operation.0, owner_window(owner)) },
        "Set Foldex operation owner",
    )?;
    let global_sink = progress_sink(state.clone(), None, mode == Mode::Recycle);
    let mut cookie = 0;
    hresult(
        unsafe { (vtable.advise)(operation.0, global_sink.0, &mut cookie) },
        "Observe Windows file operation",
    )?;
    let _advice = Advice {
        operation: &operation,
        cookie,
    };
    let mut item_sinks = Vec::with_capacity(items.len());
    for (index, item) in items {
        check_stop(&state.stop)?;
        if let Ok(key) = history::source_key(item.0) {
            #[cfg(test)]
            if std::env::var("FOLDEX_NATIVE_UNDO_PROBE").as_deref() == Ok("1")
                && native_trace_slot()
            {
                eprintln!(
                    "FOLDEX_NATIVE_ROOT mode={mode:?} index={index} source={}",
                    key.diagnostic()
                );
            }
            state
                .roots
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .entry(key.clone())
                .or_default()
                .push((key, *index));
        }
        let sink = progress_sink(
            state.clone(),
            Some((*index, (*item).clone())),
            mode == Mode::Recycle,
        );
        let target = destination.map_or(ptr::null_mut(), |item| item.0);
        let result = unsafe {
            match mode {
                Mode::Copy | Mode::Duplicate => {
                    (vtable.copy_item)(operation.0, item.0, target, ptr::null(), sink.0)
                }
                Mode::Move => (vtable.move_item)(
                    operation.0,
                    item.0,
                    target,
                    name.map_or(ptr::null(), |name| name.as_ptr()),
                    sink.0,
                ),
                Mode::Rename => (vtable.rename_item)(
                    operation.0,
                    item.0,
                    name.expect("rename has a name").as_ptr(),
                    sink.0,
                ),
                Mode::Recycle | Mode::PermanentDelete => {
                    (vtable.delete_item)(operation.0, item.0, sink.0)
                }
            }
        };
        hresult(result, "Queue Windows file operation")?;
        item_sinks.push(sink);
    }
    pump_messages();
    let (performed, aborted) = perform_and_query(
        || unsafe { (vtable.perform)(operation.0) },
        |aborted| unsafe { (vtable.any_aborted)(operation.0, aborted) },
    )?;
    pump_messages();
    let finished = state.finish_result.load(Ordering::Acquire);
    let result = if performed < 0 {
        performed
    } else if finished < 0 {
        finished
    } else {
        performed
    };
    Ok((result, aborted))
}

fn perform_and_query(
    perform: impl FnOnce() -> i32,
    query_aborted: impl FnOnce(&mut i32) -> i32,
) -> io::Result<(i32, bool)> {
    let performed = perform();
    // Required even when PerformOperations failed, or returned S_OK after
    // Cancel. Keep both calls together so an early HRESULT return cannot skip
    // the cancellation query.
    let mut aborted = 0;
    hresult(
        query_aborted(&mut aborted),
        "Read Windows operation cancellation",
    )?;
    Ok((performed, aborted != 0))
}

fn report(state: &ProgressState, result: i32, aborted: bool) -> Report {
    let outcomes: Vec<_> = state
        .outcomes
        .iter()
        .map(|value| value.load(Ordering::Acquire))
        .collect();
    let result = if result >= 0 && !aborted {
        state
            .errors
            .iter()
            .map(|value| value.load(Ordering::Acquire))
            .find(|value| *value < 0)
            .unwrap_or(result)
    } else {
        result
    };
    Report {
        outcomes,
        result,
        aborted,
        skipped: state.skipped.load(Ordering::Acquire),
        rejected_delete: state.rejected_delete.load(Ordering::Acquire),
        created: state
            .created
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        setup_error: state
            .guard_error
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        replaced: state.replaced.load(Ordering::Acquire),
        capture_errors: state
            .capture_errors
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
    }
}

fn transfer_report(
    sources: &[PathBuf],
    destination: &Path,
    moves: &[bool],
    owner: isize,
    stop: Arc<AtomicBool>,
) -> io::Result<Report> {
    validate_paths(sources)?;
    if moves.len() != sources.len() {
        return Err(invalid("Every source needs a transfer intent"));
    }
    check_stop(&stop)?;
    let destination = shell_item(destination)?;
    let items: Vec<_> = sources
        .iter()
        .map(|path| shell_item(path))
        .collect::<io::Result<_>>()?;
    let mut groups: [Vec<(usize, &ComPointer)>; 3] = std::array::from_fn(|_| Vec::new());
    for (index, item) in items.iter().enumerate() {
        let group = if moves[index] {
            2
        } else if same_item(parent_item(item)?.0, destination.0) {
            0
        } else {
            1
        };
        groups[group].push((index, item));
    }
    let state = ProgressState::new(sources.len(), stop);
    let mut result = 0;
    let mut aborted = false;
    // SetOperationFlags is per operation, not per item. Separate duplicate-copy
    // requests so cross-folder collisions never inherit automatic rename.
    for (items, mode) in groups.iter().zip([Mode::Duplicate, Mode::Copy, Mode::Move]) {
        if items.is_empty() {
            continue;
        }
        match perform_batch(items, Some(&destination), None, mode, owner, &state) {
            Ok(status) => (result, aborted) = status,
            Err(error) => {
                let mut completed = report(&state, result, aborted);
                completed.setup_error = Some(error.to_string());
                return Ok(completed);
            }
        }
        if aborted || result < 0 || state.stop.load(Ordering::Acquire) {
            break;
        }
    }
    Ok(report(&state, result, aborted))
}

#[cfg(test)]
pub(super) fn transfer(
    sources: &[PathBuf],
    destination: &Path,
    cut: bool,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> io::Result<String> {
    transfer_tracked(sources, destination, cut, owner, stop).result
}

pub(super) fn transfer_tracked(
    sources: &[PathBuf],
    destination: &Path,
    cut: bool,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> Mutation {
    let attempt = || -> io::Result<Mutation> {
        validate_paths(sources)?;
        wide(destination)?;
        let _apartment = Apartment::new()?;
        let moves = vec![cut; sources.len()];
        let inputs = history::transfer_inputs(sources, destination, &moves);
        let report = transfer_report(sources, destination, &moves, owner, stop)?;
        Ok(history::mutation(
            report,
            inputs,
            if cut { "Moved" } else { "Copied" },
            false,
        ))
    };
    attempt().unwrap_or_else(Mutation::failed)
}

#[cfg(test)]
pub(super) fn rename(
    path: &Path,
    name: &str,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> io::Result<String> {
    rename_tracked(path, name, owner, stop).result
}

pub(super) fn rename_tracked(
    path: &Path,
    name: &str,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> Mutation {
    let attempt = || -> io::Result<Mutation> {
        validate_name(name)?;
        validate_paths(&[path.to_owned()])?;
        let _apartment = Apartment::new()?;
        let input = history::Input::new(path, path.with_file_name(name), true);
        let item = shell_item(path)?;
        let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let state = ProgressState::new(1, stop);
        let result = perform_batch(
            &[(0, &item)],
            None,
            Some(&name),
            Mode::Rename,
            owner,
            &state,
        );
        Ok(history::mutation(
            completed_report(&state, result),
            vec![input],
            "Renamed",
            false,
        ))
    };
    attempt().unwrap_or_else(Mutation::failed)
}

#[cfg(test)]
pub(super) fn recycle(
    paths: &[PathBuf],
    owner: isize,
    stop: Arc<AtomicBool>,
) -> io::Result<String> {
    delete_tracked(paths, false, owner, stop).result
}

pub(super) fn delete_tracked(
    paths: &[PathBuf],
    permanent: bool,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> Mutation {
    let attempt = || -> io::Result<Mutation> {
        validate_paths(paths)?;
        let _apartment = Apartment::new()?;
        let inputs = if permanent {
            vec![]
        } else {
            paths
                .iter()
                .map(|path| history::Input::new(path, path.clone(), true))
                .collect()
        };
        let items: Vec<_> = paths
            .iter()
            .map(|path| shell_item(path))
            .collect::<io::Result<_>>()?;
        let indexed: Vec<_> = items.iter().enumerate().collect();
        let state = ProgressState::new(paths.len(), stop);
        let mode = if permanent {
            Mode::PermanentDelete
        } else {
            Mode::Recycle
        };
        let result = perform_batch(&indexed, None, None, mode, owner, &state);
        Ok(history::mutation(
            completed_report(&state, result),
            inputs,
            if permanent { "Deleted" } else { "Recycled" },
            !permanent,
        ))
    };
    attempt().unwrap_or_else(Mutation::failed)
}

fn completed_report(state: &ProgressState, result: io::Result<(i32, bool)>) -> Report {
    match result {
        Ok((result, aborted)) => report(state, result, aborted),
        Err(error) => {
            let mut report = report(state, 0, false);
            report.setup_error = Some(error.to_string());
            report
        }
    }
}

fn volume_identity(path: &Path) -> Option<Vec<u16>> {
    let path = wide(path).ok()?;
    let mut mount = vec![0u16; 32_768];
    if unsafe { GetVolumePathNameW(path.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } == 0 {
        return None;
    }
    let mut volume = vec![0u16; 128];
    if unsafe {
        GetVolumeNameForVolumeMountPointW(mount.as_ptr(), volume.as_mut_ptr(), volume.len() as u32)
    } != 0
    {
        volume.truncate(volume.iter().position(|unit| *unit == 0)?);
        Some(volume)
    } else {
        // UNC paths have no volume GUID. The mount/share path remains a useful
        // conservative identity; unknown identities default to copy.
        mount.truncate(mount.iter().position(|unit| *unit == 0)?);
        for unit in &mut mount {
            if (65..=90).contains(unit) {
                *unit += 32;
            }
        }
        Some(mount)
    }
}

#[cfg(test)]
pub(super) fn drop_files(
    sources: &[PathBuf],
    destination: &Path,
    control: bool,
    shift: bool,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> io::Result<String> {
    drop_files_tracked(sources, destination, control, shift, owner, stop).result
}

pub(super) fn drop_files_tracked(
    sources: &[PathBuf],
    destination: &Path,
    control: bool,
    shift: bool,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> Mutation {
    let attempt = || -> io::Result<Mutation> {
        validate_paths(sources)?;
        drop_should_move(control, shift, false)?;
        let _apartment = Apartment::new()?;
        let target_volume = (!control && !shift)
            .then(|| volume_identity(destination))
            .flatten();
        let moves: Vec<_> = sources
            .iter()
            .map(|path| {
                let same_volume = target_volume
                    .as_ref()
                    .is_some_and(|target| volume_identity(path).as_ref() == Some(target));
                drop_should_move(control, shift, same_volume)
            })
            .collect::<io::Result<_>>()?;
        let action = if moves.iter().all(|value| *value) {
            "Moved"
        } else if moves.iter().all(|value| !*value) {
            "Copied"
        } else {
            "Transferred"
        };
        let inputs = history::transfer_inputs(sources, destination, &moves);
        let report = transfer_report(sources, destination, &moves, owner, stop)?;
        Ok(history::mutation(report, inputs, action, false))
    };
    attempt().unwrap_or_else(Mutation::failed)
}

fn drop_should_move(control: bool, shift: bool, same_volume: bool) -> io::Result<bool> {
    if control && shift {
        return Err(invalid(
            "Creating shortcuts with Ctrl+Shift drag is not supported",
        ));
    }
    Ok(if control {
        false
    } else if shift {
        true
    } else {
        same_volume
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use windows_sys::Win32::UI::Shell::{
        FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOFX_KEEPNEWERFILE,
    };

    #[repr(C)]
    struct NamedItem {
        vtable: &'static ShellItemVtable,
        references: AtomicU32,
        name: Vec<u16>,
    }
    fn named_item(name: &str) -> ComPointer {
        ComPointer(
            Box::into_raw(Box::new(NamedItem {
                vtable: &NAMED_ITEM_VTABLE,
                references: AtomicU32::new(1),
                name: name.encode_utf16().chain(Some(0)).collect(),
            }))
            .cast(),
        )
    }
    unsafe extern "system" fn named_query(
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
        if unsafe { guid_equal(&*iid, &IID_UNKNOWN) || guid_equal(&*iid, &IID_SHELL_ITEM) } {
            unsafe {
                *output = this;
                named_add_ref(this);
            }
            0
        } else {
            E_NOINTERFACE
        }
    }
    unsafe extern "system" fn named_add_ref(this: *mut c_void) -> u32 {
        unsafe { &*this.cast::<NamedItem>() }
            .references
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }
    unsafe extern "system" fn named_release(this: *mut c_void) -> u32 {
        let previous = unsafe { &*this.cast::<NamedItem>() }
            .references
            .fetch_sub(1, Ordering::AcqRel);
        if previous == 1 {
            unsafe {
                drop(Box::from_raw(this.cast::<NamedItem>()));
            }
        }
        previous - 1
    }
    unsafe extern "system" fn named_parent(_: *mut c_void, _: *mut *mut c_void) -> i32 {
        E_NOINTERFACE
    }
    unsafe extern "system" fn named_display(
        this: *mut c_void,
        _: i32,
        output: *mut *mut u16,
    ) -> i32 {
        if output.is_null() {
            return E_POINTER;
        }
        let item = unsafe { &*this.cast::<NamedItem>() };
        let buffer =
            unsafe { windows_sys::Win32::System::Com::CoTaskMemAlloc(item.name.len() * 2) }
                .cast::<u16>();
        if buffer.is_null() {
            return 0x8007000eu32 as i32;
        }
        unsafe {
            ptr::copy_nonoverlapping(item.name.as_ptr(), buffer, item.name.len());
            *output = buffer;
        }
        0
    }
    unsafe extern "system" fn missing_compare(
        _: *mut c_void,
        _: *mut c_void,
        _: u32,
        _: *mut i32,
    ) -> i32 {
        // Simulate an IShellItem whose backing source was moved or deleted.
        0x80070002u32 as i32
    }
    static NAMED_ITEM_VTABLE: ShellItemVtable = ShellItemVtable {
        unknown: UnknownVtable {
            query_interface: named_query,
            add_ref: named_add_ref,
            release: named_release,
        },
        bind_to_handler: 0,
        get_parent: named_parent,
        get_display_name: named_display,
        get_attributes: 0,
        compare: missing_compare,
    };

    fn register_named_root(state: &ProgressState, index: usize, item: &ComPointer) {
        let key = history::source_key(item.0).unwrap();
        state
            .roots
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default()
            .push((key, index));
    }

    #[test]
    fn global_post_events_match_the_retained_name_when_source_comparison_fails() {
        let state = ProgressState::new(1, Arc::new(AtomicBool::new(false)));
        let root = named_item("C:\\FoldexGeneratedCallbackFixture\\source.txt");
        let callback_source = named_item("C:\\FoldexGeneratedCallbackFixture\\source.txt");
        let created = named_item("C:\\FoldexGeneratedCallbackFixture\\copy.txt");
        register_named_root(&state, 0, &root);
        assert!(!same_item(root.0, callback_source.0));
        let global = progress_sink(state.clone(), None, false);
        assert_eq!(
            record_item(
                global.0,
                callback_source.0,
                COPYENGINE_S_DONT_PROCESS_CHILDREN,
                created.0,
                false
            ),
            0
        );
        assert_eq!(state.outcomes[0].load(Ordering::Acquire), SUCCEEDED);
        assert!(!state.skipped.load(Ordering::Acquire));
        assert_eq!(
            state.created.lock().unwrap()[0].as_ref().unwrap().path,
            PathBuf::from("C:\\FoldexGeneratedCallbackFixture\\copy.txt")
        );
        let bytes = state.created_bytes.load(Ordering::Acquire);
        let per_item = progress_sink(state.clone(), Some((0, root.clone())), false);
        record_item(per_item.0, callback_source.0, 0, created.0, false);
        assert_eq!(state.created_bytes.load(Ordering::Acquire), bytes);
    }

    #[test]
    fn per_item_child_post_events_cannot_mark_a_selected_directory_complete() {
        let state = ProgressState::new(1, Arc::new(AtomicBool::new(false)));
        let root = named_item("C:\\FoldexGeneratedCallbackFixture\\folder");
        let child = named_item("C:\\FoldexGeneratedCallbackFixture\\folder\\child.txt");
        let created_child = named_item("C:\\FoldexGeneratedCallbackFixture\\copy\\child.txt");
        register_named_root(&state, 0, &root);
        let sink = progress_sink(state.clone(), Some((0, root)), false);
        record_item(sink.0, child.0, 0, created_child.0, false);
        assert_eq!(state.outcomes[0].load(Ordering::Acquire), PENDING);
        assert!(state.created.lock().unwrap()[0].is_none());
    }

    #[test]
    fn case_distinct_selected_sources_keep_distinct_completion_indices() {
        let state = ProgressState::new(2, Arc::new(AtomicBool::new(false)));
        let lower = named_item("C:\\FoldexGeneratedCallbackFixture\\a.txt");
        let upper = named_item("C:\\FoldexGeneratedCallbackFixture\\A.txt");
        register_named_root(&state, 0, &lower);
        register_named_root(&state, 1, &upper);
        let sink = progress_sink(state, None, false);
        let sink = unsafe { &*sink.0.cast::<ProgressSink>() };
        assert_eq!(root_index(sink, lower.0), Some(0));
        assert_eq!(root_index(sink, upper.0), Some(1));
    }

    #[test]
    fn post_capture_errors_remain_available_without_rejecting_the_native_success() {
        let state = ProgressState::new(1, Arc::new(AtomicBool::new(false)));
        let root = named_item("C:\\FoldexGeneratedCallbackFixture\\source.txt");
        let unusable_created = named_item("relative-result.txt");
        register_named_root(&state, 0, &root);
        let sink = progress_sink(state.clone(), None, false);
        record_item(sink.0, root.0, 0, unusable_created.0, false);
        let report = report(&state, 0, false);
        assert_eq!(report.outcomes[0], SUCCEEDED);
        assert!(report.created[0].is_none());
        assert!(
            report.capture_errors[0]
                .as_ref()
                .unwrap()
                .contains("absolute Windows path")
        );
        assert!(report.message("Copied").is_ok());
    }

    #[test]
    fn interactive_shell_policy_keeps_native_progress_collision_and_error_ui() {
        let suppress = FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOFX_KEEPNEWERFILE;
        for mode in [
            Mode::Copy,
            Mode::Duplicate,
            Mode::Move,
            Mode::Rename,
            Mode::Recycle,
        ] {
            let suppress = if mode == Mode::Recycle {
                suppress & !FOF_NOCONFIRMATION
            } else {
                suppress
            };
            assert_eq!(operation_flags(mode) & suppress, 0, "{mode:?}");
            assert_ne!(operation_flags(mode) & FOFX_ADDUNDORECORD, 0);
        }
        assert_ne!(operation_flags(Mode::Duplicate) & FOF_RENAMEONCOLLISION, 0);
        assert_ne!(
            operation_flags(Mode::Duplicate) & FOFX_PRESERVEFILEEXTENSIONS,
            0
        );
        for mode in [Mode::Copy, Mode::Move, Mode::Rename, Mode::Recycle] {
            assert_eq!(operation_flags(mode) & FOF_RENAMEONCOLLISION, 0);
        }
        assert_ne!(operation_flags(Mode::Recycle) & FOFX_RECYCLEONDELETE, 0);
        assert_ne!(operation_flags(Mode::Recycle) & FOF_WANTNUKEWARNING, 0);
    }

    #[test]
    fn permanent_delete_and_history_replay_never_add_global_undo_records() {
        let flags = operation_flags(Mode::PermanentDelete);
        assert_eq!(
            flags
                & (FOF_ALLOWUNDO
                    | FOFX_ADDUNDORECORD
                    | FOFX_RECYCLEONDELETE
                    | FOF_SILENT
                    | FOF_NOERRORUI),
            0
        );
        assert_ne!(flags & FOF_NOCONFIRMATION, 0);
        let recycled = effective_operation_flags(Mode::Recycle, true);
        assert_ne!(recycled & FOFX_RECYCLEONDELETE, 0);
        assert_ne!(recycled & FOF_WANTNUKEWARNING, 0);
        assert_eq!(recycled & (FOF_ALLOWUNDO | FOFX_ADDUNDORECORD), 0);
        assert_ne!(
            effective_operation_flags(Mode::Move, true) & FOF_RENAMEONCOLLISION,
            0
        );
        without_session_undo(|| {
            assert_eq!(
                effective_operation_flags(Mode::Copy, false) & (FOF_ALLOWUNDO | FOFX_ADDUNDORECORD),
                0
            );
            assert_ne!(
                effective_operation_flags(Mode::Recycle, false) & FOFX_RECYCLEONDELETE,
                0
            );
        });
        assert_ne!(
            effective_operation_flags(Mode::Copy, false) & FOFX_ADDUNDORECORD,
            0
        );
    }

    #[test]
    fn replay_rejects_an_overwrite_flag_before_shell_mutation() {
        let mut state = ProgressState::new(1, Arc::new(AtomicBool::new(false)));
        Arc::get_mut(&mut state).unwrap().replay = true;
        let sink = progress_sink(state.clone(), None, false);
        assert_eq!(
            unsafe {
                sink_pre_transfer(
                    sink.0,
                    TSF_OVERWRITE_EXIST as u32,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            },
            E_ABORT
        );
        assert!(state.replaced.load(Ordering::Acquire));
    }

    #[test]
    fn drop_modifiers_override_volume_and_unknown_volume_copies() {
        for same_volume in [false, true] {
            assert!(!drop_should_move(true, false, same_volume).unwrap());
            assert!(drop_should_move(false, true, same_volume).unwrap());
            assert!(drop_should_move(true, true, same_volume).is_err());
        }
        assert!(!drop_should_move(false, false, false).unwrap());
        assert!(drop_should_move(false, false, true).unwrap());
    }

    #[test]
    fn cancellation_is_queried_after_both_success_and_failed_perform() {
        for performed in [0, E_ABORT, 0x80070005u32 as i32] {
            let calls = Cell::new(0);
            let result = perform_and_query(
                || {
                    calls.set(1);
                    performed
                },
                |aborted| {
                    assert_eq!(calls.get(), 1);
                    calls.set(2);
                    *aborted = 1;
                    0
                },
            )
            .unwrap();
            assert_eq!(calls.get(), 2);
            assert_eq!(result, (performed, true));
        }
    }

    #[test]
    fn skip_and_unconfirmed_positive_statuses_are_not_copy_success() {
        assert_eq!(item_outcome(0, true, false), SUCCEEDED);
        assert_eq!(item_outcome(0, false, false), SKIPPED);
        assert_eq!(item_outcome(0, false, true), SUCCEEDED);
        for status in [
            COPYENGINE_S_USER_IGNORED,
            COPYENGINE_S_ALREADY_DONE,
            COPYENGINE_S_PENDING,
        ] {
            assert_eq!(item_outcome(status, true, false), SKIPPED);
        }
        assert_eq!(item_outcome(E_ABORT, true, false), FAILED);
    }

    #[test]
    fn native_completed_subtree_status_requires_a_confirmed_transfer_or_deletion() {
        assert_eq!(
            item_outcome(COPYENGINE_S_DONT_PROCESS_CHILDREN, true, false),
            SUCCEEDED
        );
        assert_eq!(
            item_outcome(COPYENGINE_S_DONT_PROCESS_CHILDREN, true, true),
            SUCCEEDED
        );
        assert_eq!(
            item_outcome(COPYENGINE_S_DONT_PROCESS_CHILDREN, false, true),
            SUCCEEDED
        );
        assert_eq!(
            item_outcome(COPYENGINE_S_DONT_PROCESS_CHILDREN, false, false),
            SKIPPED
        );
        let state = ProgressState::new(1, Arc::new(AtomicBool::new(false)));
        let root = named_item("C:\\FoldexGeneratedCallbackFixture\\deleted.txt");
        register_named_root(&state, 0, &root);
        let sink = progress_sink(state.clone(), None, false);
        record_item(
            sink.0,
            root.0,
            COPYENGINE_S_DONT_PROCESS_CHILDREN,
            ptr::null_mut(),
            true,
        );
        assert!(!state.skipped.load(Ordering::Acquire));
        let mutation = history::mutation(report(&state, 0, false), vec![], "Deleted", false);
        assert!(mutation.changed);
        assert!(mutation.steps.is_empty());
        assert_eq!(mutation.result.unwrap(), "Deleted 1 item");
    }

    #[test]
    fn canceled_or_partial_results_never_claim_complete_success() {
        let complete = Report {
            outcomes: vec![SUCCEEDED, SUCCEEDED],
            aborted: false,
            result: 0,
            skipped: false,
            rejected_delete: false,
            created: vec![],
            setup_error: None,
            replaced: false,
            capture_errors: vec![],
        };
        assert!(complete.full_success());
        let canceled = Report {
            aborted: true,
            ..complete
        };
        assert!(!canceled.full_success());
        assert!(
            canceled
                .message("Moved")
                .unwrap()
                .contains("Canceled or stopped")
        );
        let partial = Report {
            outcomes: vec![SUCCEEDED, SKIPPED],
            aborted: false,
            ..canceled
        };
        assert!(!partial.full_success());
        assert!(partial.message("Moved").unwrap().contains("1/2"));
        let failure = Report {
            result: 0x80070005u32 as i32,
            ..partial
        };
        assert!(failure.message("Moved").is_err());
    }

    #[test]
    fn native_progress_sink_checks_shutdown_and_refuses_permanent_delete() {
        let stop = Arc::new(AtomicBool::new(false));
        let state = ProgressState::new(1, stop.clone());
        let sink = progress_sink(state.clone(), None, true);
        assert_eq!(
            unsafe {
                sink_pre_delete(
                    sink.0,
                    TSF_DELETE_RECYCLE_IF_POSSIBLE as u32,
                    ptr::null_mut(),
                )
            },
            0
        );
        assert_eq!(
            unsafe { sink_pre_delete(sink.0, 0, ptr::null_mut()) },
            E_ABORT
        );
        assert!(state.rejected_delete.load(Ordering::Acquire));
        stop.store(true, Ordering::Release);
        assert_eq!(
            unsafe { sink_pre_transfer(sink.0, 0, ptr::null_mut(), ptr::null_mut(), ptr::null()) },
            E_ABORT
        );
    }

    #[test]
    fn raw_com_vtables_keep_sdk_slot_offsets() {
        let slot = std::mem::size_of::<usize>();
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, set_flags),
            5 * slot
        );
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, set_owner),
            9 * slot
        );
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, rename_item),
            12 * slot
        );
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, move_item),
            14 * slot
        );
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, copy_item),
            16 * slot
        );
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, delete_item),
            18 * slot
        );
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, perform),
            21 * slot
        );
        assert_eq!(
            std::mem::offset_of!(FileOperationVtable, any_aborted),
            22 * slot
        );
        assert_eq!(std::mem::size_of::<FileOperationVtable>(), 23 * slot);
        assert_eq!(
            std::mem::offset_of!(ProgressSinkVtable, progress),
            15 * slot
        );
        assert_eq!(std::mem::size_of::<ProgressSinkVtable>(), 19 * slot);
    }
}

#[cfg(test)]
mod native_probe_tests {
    use super::*;
    use std::{fs, sync::atomic::AtomicU64};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);

    pub(super) struct Fixture {
        pub(super) root: PathBuf,
        base: PathBuf,
    }
    impl Fixture {
        pub(super) fn new(name: &str) -> Self {
            assert_eq!(
                std::env::var("FOLDEX_NATIVE_OPS_PROBE").as_deref(),
                Ok("1"),
                "This ignored probe requires FOLDEX_NATIVE_OPS_PROBE=1 and generated fixtures only"
            );
            let base = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/native-file-operation-fixtures");
            fs::create_dir_all(&base).unwrap();
            let base = fs::canonicalize(base).unwrap();
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = base.join(format!("{name}-{}-{sequence}", std::process::id()));
            fs::create_dir(&root).unwrap();
            Self { root, base }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Resolve and verify the exact fixture boundary before recursion.
            let Ok(root) = fs::canonicalize(&self.root) else {
                return;
            };
            if root != self.base && root.starts_with(&self.base) {
                let _ = fs::remove_dir_all(root);
            }
        }
    }

    pub(super) fn private_clipboard_required() {
        use windows_sys::Win32::System::StationsAndDesktops::{
            GetProcessWindowStation, GetUserObjectInformationW, UOI_NAME,
        };
        assert_eq!(
            std::env::var("FOLDEX_NATIVE_CLIPBOARD_PROBE").as_deref(),
            Ok("1"),
            "Clipboard probes require explicit FOLDEX_NATIVE_CLIPBOARD_PROBE=1"
        );
        let station = unsafe { GetProcessWindowStation() };
        let mut name = [0u16; 256];
        let mut required = 0;
        assert_ne!(
            unsafe {
                GetUserObjectInformationW(
                    station,
                    UOI_NAME,
                    name.as_mut_ptr().cast(),
                    std::mem::size_of_val(&name) as u32,
                    &mut required,
                )
            },
            0
        );
        let name = String::from_utf16_lossy(
            &name[..name
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(name.len())],
        );
        assert!(
            !name.eq_ignore_ascii_case("WinSta0"),
            "Refusing to touch the user's interactive clipboard"
        );
    }

    #[test]
    #[ignore = "Opt-in Shell mutation probe; generated workspace fixtures only"]
    fn native_fixture_same_folder_copy_uses_shell_duplicate_names() {
        let fixture = Fixture::new("duplicate");
        let file = fixture.root.join("résumé source.txt");
        let folder = fixture.root.join("folder source");
        fs::create_dir(&folder).unwrap();
        fs::write(&file, b"original payload").unwrap();
        fs::write(folder.join("nested.txt"), b"nested payload").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        transfer(
            &[file.clone(), folder.clone()],
            &fixture.root,
            false,
            0,
            stop.clone(),
        )
        .unwrap();
        transfer(&[file.clone()], &fixture.root, false, 0, stop).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"original payload");
        let entries: Vec<_> = fs::read_dir(&fixture.root)
            .unwrap()
            .map(|item| item.unwrap().path())
            .collect();
        assert_eq!(entries.len(), 5);
        let copies: Vec<_> = entries
            .iter()
            .filter(|path| path.is_file() && **path != file)
            .collect();
        assert_eq!(copies.len(), 2);
        for copy in copies {
            assert_eq!(copy.extension().unwrap(), "txt");
            assert_eq!(fs::read(copy).unwrap(), b"original payload");
        }
        let folder_copy = entries
            .iter()
            .find(|path| path.is_dir() && **path != folder)
            .unwrap();
        assert_eq!(
            fs::read(folder_copy.join("nested.txt")).unwrap(),
            b"nested payload"
        );
    }

    #[test]
    #[ignore = "Opt-in Shell mutation probe; generated workspace fixtures only"]
    fn native_fixture_move_and_case_rename_preserve_content() {
        let fixture = Fixture::new("move-rename");
        let source = fixture.root.join("source.txt");
        let destination = fixture.root.join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"move payload").unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        transfer(&[source.clone()], &destination, true, 0, stop.clone()).unwrap();
        assert!(!source.exists());
        let moved = destination.join("source.txt");
        rename(&moved, "SOURCE.txt", 0, stop).unwrap();
        assert_eq!(
            fs::read(destination.join("SOURCE.txt")).unwrap(),
            b"move payload"
        );
        assert_eq!(
            fs::read_dir(&destination)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .file_name(),
            "SOURCE.txt"
        );
    }

    #[test]
    #[ignore = "Opt-in Shell mutation probe; generated workspace fixtures only"]
    fn native_fixture_precanceled_operation_preserves_source_and_destination() {
        let fixture = Fixture::new("canceled");
        let source = fixture.root.join("source.txt");
        let destination = fixture.root.join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"keep source").unwrap();
        let error = transfer(
            &[source.clone()],
            &destination,
            true,
            0,
            Arc::new(AtomicBool::new(true)),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(fs::read(source).unwrap(), b"keep source");
        assert_eq!(fs::read_dir(destination).unwrap().count(), 0);
    }

    #[test]
    #[ignore = "Strictly opt-in native conflict dialog probe; owned offscreen desktop and generated fixtures only"]
    fn native_fixture_cross_folder_collision_shows_native_dialog_and_cancels() {
        super::dialog_probe::cross_folder_collision();
    }
}
