//! Opt-in native Shell conflict observation. This is not compiled in production.
//! The parent owns the subprocess hard timeout and its offscreen desktop.

use super::*;
use std::{
    fs::{self, OpenOptions},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    System::{
        StationsAndDesktops::{
            EnumDesktopWindows, GetProcessWindowStation, GetThreadDesktop,
            GetUserObjectInformationW, SetThreadDesktop, UOI_NAME,
        },
        Threading::GetCurrentThreadId,
    },
    UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, EnumChildWindows, GW_OWNER, GetClassNameW, GetDlgCtrlID,
        GetWindow, IsWindowVisible, PostMessageW, SMTO_ABORTIFHUNG, SMTO_BLOCK, SMTO_ERRORONEXIT,
        SW_SHOWNOACTIVATE, SendMessageTimeoutW, ShowWindow, WM_CLOSE, WM_GETTEXT,
        WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    },
};

const MAX_TOP_WINDOWS: usize = 16;
const MAX_CONTROLS: usize = 96;
const OBSERVATION_SECONDS: u64 = 20;

#[derive(Debug, serde::Serialize)]
struct Control {
    hwnd: usize,
    id: i32,
    class: String,
    text: String,
    visible: bool,
}

#[derive(Debug, serde::Serialize)]
struct Window {
    top: Control,
    fixture_owned: bool,
    owner_chain: Vec<usize>,
    controls: Vec<Control>,
}

#[derive(Debug, Default, serde::Serialize)]
struct Observation {
    process_id: u32,
    fixture_owner: usize,
    station: String,
    desktop: String,
    windows: Vec<Window>,
    /// Read-only same-PID diagnostics, including windows outside the owner tree.
    all_process_windows_at_deadline: Vec<Window>,
    conflict_index: Option<usize>,
    conflict_classification: Option<String>,
    cancel_ownership_kind: Option<String>,
    cancel_posts: usize,
    elapsed_ms: u128,
    timed_out: bool,
    setup_error: Option<String>,
}

fn object_name(handle: *mut c_void) -> io::Result<String> {
    let mut name = [0u16; 256];
    let mut needed = 0;
    if unsafe {
        GetUserObjectInformationW(
            handle,
            UOI_NAME,
            name.as_mut_ptr().cast(),
            std::mem::size_of_val(&name) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf16_lossy(
        &name[..name
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(name.len())],
    ))
}

fn owned_offscreen_desktop() -> (isize, String, String) {
    assert_eq!(
        std::env::var("FOLDEX_NATIVE_DIALOG_PROBE").as_deref(),
        Ok("1"),
        "Requires explicit FOLDEX_NATIVE_DIALOG_PROBE=1 in the parent's isolated driver"
    );
    let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) };
    let station = unsafe { GetProcessWindowStation() };
    let desktop_name = object_name(desktop).unwrap();
    let station_name = object_name(station).unwrap();
    assert!(
        desktop_name.starts_with("FoldexProbe_") || station_name.starts_with("FoldexProbe_"),
        "Refusing native dialog observation outside a parent-owned FoldexProbe_ desktop/station"
    );
    (desktop as isize, station_name, desktop_name)
}

struct Owner(HWND);
impl Owner {
    fn new() -> Self {
        let class: Vec<_> = "STATIC\0".encode_utf16().collect();
        let title: Vec<_> = "Foldex generated collision fixture owner\0"
            .encode_utf16()
            .collect();
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                0,
                0,
                640,
                480,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        assert!(
            !hwnd.is_null(),
            "Create isolated fixture owner: {}",
            io::Error::last_os_error()
        );
        // This is already a guarded, parent-owned non-input desktop. Making
        // its owner visible lets native modal UI establish normal ownership
        // without activating or switching the user's desktop.
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        Self(hwnd)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

fn own_process(hwnd: HWND) -> bool {
    let mut process = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
    process == unsafe { GetCurrentProcessId() }
}

fn owner_chain(hwnd: HWND, root: HWND) -> Option<Vec<usize>> {
    if hwnd == root || !own_process(hwnd) {
        return None;
    }
    let mut current = hwnd;
    let mut chain = Vec::with_capacity(8);
    // Owner traversal is capped; no HWND-next polling or unbounded graph walk.
    for _ in 0..16 {
        current = unsafe { GetWindow(current, GW_OWNER) };
        if current.is_null() || !own_process(current) {
            return None;
        }
        if chain.contains(&(current as usize)) {
            return None;
        }
        chain.push(current as usize);
        if current == root {
            return Some(chain);
        }
    }
    None
}

fn control(hwnd: HWND, deadline: Instant) -> Control {
    let mut class = [0u16; 256];
    let length =
        unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) }.max(0) as usize;
    let class = String::from_utf16_lossy(&class[..length.min(class.len())]);
    let mut text = [0u16; 1024];
    let mut copied = 0;
    // GetWindowText on our own process can block indefinitely; use its native
    // system message with an explicit cross-thread timeout instead.
    // Both enumeration callbacks check their deadline before coming here, and
    // this second check also covers time consumed by child enumeration. Never
    // start a fresh 30 ms send after the shared deadline has already elapsed.
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .as_millis()
        .min(30) as u32;
    let got = if timeout == 0 {
        0
    } else {
        unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_GETTEXT,
                text.len(),
                text.as_mut_ptr() as isize,
                SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
                timeout,
                &mut copied,
            )
        }
    };
    let length = if got == 0 {
        0
    } else {
        copied.min(text.len() - 1)
    };
    Control {
        hwnd: hwnd as usize,
        id: unsafe { GetDlgCtrlID(hwnd) },
        class,
        text: String::from_utf16_lossy(&text[..length]),
        visible: unsafe { IsWindowVisible(hwnd) } != 0,
    }
}

struct ChildEnumeration<'a> {
    controls: &'a mut Vec<Control>,
    process: u32,
    deadline: Instant,
}
unsafe extern "system" fn child_callback(hwnd: HWND, parameter: isize) -> i32 {
    let state = unsafe { &mut *(parameter as *mut ChildEnumeration<'_>) };
    if state.controls.len() >= MAX_CONTROLS || Instant::now() >= state.deadline {
        return 0;
    }
    let mut process = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
    if process == state.process {
        state.controls.push(control(hwnd, state.deadline));
    }
    1
}

struct TopEnumeration {
    owner: HWND,
    include_unowned: bool,
    windows: Vec<Window>,
    deadline: Instant,
}
unsafe extern "system" fn top_callback(hwnd: HWND, parameter: isize) -> i32 {
    let state = unsafe { &mut *(parameter as *mut TopEnumeration) };
    if state.windows.len() >= MAX_TOP_WINDOWS || Instant::now() >= state.deadline {
        return 0;
    }
    if !own_process(hwnd) {
        return 1;
    }
    let owned = owner_chain(hwnd, state.owner);
    if owned.is_none() && !state.include_unowned {
        return 1;
    }
    let fixture_owned = owned.is_some();
    let chain = owned.unwrap_or_else(|| {
        let mut chain = Vec::new();
        let mut current = hwnd;
        for _ in 0..16 {
            current = unsafe { GetWindow(current, GW_OWNER) };
            if current.is_null() || chain.contains(&(current as usize)) {
                break;
            }
            chain.push(current as usize);
            if !own_process(current) {
                break;
            }
        }
        chain
    });
    let mut controls = Vec::new();
    let mut children = ChildEnumeration {
        controls: &mut controls,
        process: unsafe { GetCurrentProcessId() },
        deadline: state.deadline,
    };
    unsafe {
        EnumChildWindows(
            hwnd,
            Some(child_callback),
            (&mut children as *mut ChildEnumeration<'_>) as isize,
        )
    };
    state.windows.push(Window {
        top: control(hwnd, state.deadline),
        fixture_owned,
        owner_chain: chain,
        controls,
    });
    1
}

fn desktop_windows(
    desktop: isize,
    owner: HWND,
    include_unowned: bool,
    deadline: Instant,
) -> Vec<Window> {
    let mut enumeration = TopEnumeration {
        owner,
        include_unowned,
        windows: Vec::new(),
        deadline,
    };
    unsafe {
        // Explicitly enumerate the owned offscreen desktop, not an implicit
        // default/input surface selected by a convenience enumeration API.
        EnumDesktopWindows(
            desktop as _,
            Some(top_callback),
            (&mut enumeration as *mut TopEnumeration) as isize,
        )
    };
    enumeration.windows
}

fn conflict_window(window: &Window) -> bool {
    let mut text = window.top.text.to_lowercase();
    for control in &window.controls {
        text.push('\n');
        text.push_str(&control.text.to_lowercase());
    }
    // Never mistake a generic progress window containing a filename for the
    // collision decision. Unknown/localized UI fails with recorded evidence;
    // it does not become a synthetic pass.
    window.top.visible && conflict_text(&text)
}

fn conflict_text(text: &str) -> bool {
    let text = text.to_lowercase();
    text.contains("replace or skip")
        || text.contains("confirm file replace")
        || (text.contains("already exists") && (text.contains("replace") || text.contains("skip")))
}

fn authorized_conflict(window: &Window) -> bool {
    conflict_window(window)
        && (window.fixture_owned
            || (window.top.class == "OperationStatusWindow" && conflict_text(&window.top.text)))
}

fn post_cancel(
    hwnd: HWND,
    owner: HWND,
    desktop_name: &str,
    station_name: &str,
    done: &AtomicBool,
    deadline: Instant,
) -> Option<&'static str> {
    // The process is this one-fixture sandbox child. Shell may create its
    // OperationStatusWindow without a GW_OWNER despite SetOwnerWindow. Permit
    // that observed native class/title only on the exact guarded desktop while
    // the one fixture operation is active. No external/default-desktop target.
    if done.load(Ordering::Acquire) || hwnd == owner || !own_process(hwnd) {
        return None;
    }
    if !(desktop_name.starts_with("FoldexProbe_") || station_name.starts_with("FoldexProbe_")) {
        return None;
    }
    let mut process = 0;
    let thread = unsafe { GetWindowThreadProcessId(hwnd, &mut process) };
    if process != unsafe { GetCurrentProcessId() } {
        return None;
    }
    let target_desktop = unsafe { GetThreadDesktop(thread) };
    if object_name(target_desktop).ok().as_deref() != Some(desktop_name)
        || object_name(unsafe { GetProcessWindowStation() })
            .ok()
            .as_deref()
            != Some(station_name)
    {
        return None;
    }
    // Re-read native class/title/visibility, not the earlier observation.
    let current = control(hwnd, deadline);
    if !current.visible || !conflict_text(&current.text) {
        return None;
    }
    let kind = if owner_chain(hwnd, owner).is_some() {
        "fixture_owner_chain"
    } else if current.class == "OperationStatusWindow" {
        "same_process_guarded_desktop_native_conflict"
    } else {
        return None;
    };
    if done.load(Ordering::Acquire) || !own_process(hwnd) {
        return None;
    }
    (unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) } != 0).then_some(kind)
}

fn observe(
    desktop: isize,
    owner: isize,
    done: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    station: String,
    desktop_name: String,
) -> Observation {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(OBSERVATION_SECONDS);
    let mut report = Observation {
        process_id: unsafe { GetCurrentProcessId() },
        fixture_owner: owner as usize,
        station,
        desktop: desktop_name,
        ..Observation::default()
    };
    if unsafe { SetThreadDesktop(desktop as _) } == 0 {
        report.setup_error = Some(format!(
            "Set observer desktop: {}",
            io::Error::last_os_error()
        ));
        record_observer(&report);
        stop.store(true, Ordering::Release);
        return report;
    }
    let owner = owner as HWND;
    let mut last_cancel = None;
    while !done.load(Ordering::Acquire) && Instant::now() < deadline {
        let windows = desktop_windows(desktop, owner, true, deadline);
        if report.conflict_index.is_none() {
            if let Some(index) = windows.iter().position(authorized_conflict) {
                report.conflict_index = Some(index);
                report.conflict_classification = Some("observed_native_conflict_text".into());
                report.windows = windows;
            } else if !windows.is_empty() {
                report.windows = windows;
            }
        }
        if let Some(index) = report.conflict_index {
            let hwnd = report.windows[index].top.hwnd as HWND;
            if report.cancel_posts < 5
                && last_cancel
                    .is_none_or(|last: Instant| last.elapsed() >= Duration::from_millis(150))
            {
                if let Some(kind) = post_cancel(
                    hwnd,
                    owner,
                    &report.desktop,
                    &report.station,
                    &done,
                    deadline,
                ) {
                    report.cancel_posts += 1;
                    report.cancel_ownership_kind = Some(kind.into());
                }
                last_cancel = Some(Instant::now());
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
    if !done.load(Ordering::Acquire) {
        report.timed_out = true;
        report.all_process_windows_at_deadline = desktop_windows(
            desktop,
            owner,
            true,
            Instant::now() + Duration::from_secs(1),
        );
        report.elapsed_ms = started.elapsed().as_millis();
        // Persist from the observer BEFORE requesting cleanup or waiting for
        // PerformOperations. The parent's hard process timeout must not erase
        // the only evidence when Shell never returns.
        record_observer(&report);
        stop.store(true, Ordering::Release);
        // On unknown UI/timeout, only revalidated fixture/native-conflict
        // windows on this exact same-PID guarded desktop may be dismissed.
        // The parent still enforces a hard subprocess timeout if Shell hangs.
        for window in desktop_windows(
            desktop,
            owner,
            true,
            Instant::now() + Duration::from_secs(1),
        ) {
            if let Some(kind) = post_cancel(
                window.top.hwnd as HWND,
                owner,
                &report.desktop,
                &report.station,
                &done,
                Instant::now() + Duration::from_millis(40),
            ) {
                report.cancel_posts += 1;
                report.cancel_ownership_kind = Some(kind.into());
            }
        }
    }
    report.elapsed_ms = started.elapsed().as_millis();
    report
}

fn record_observer(report: &Observation) {
    let evidence = serde_json::json!({ "scenario": "cross_folder_collision_native_cancel",
        "phase": "observer_before_operation_return", "observation": report,
        "desktop_input_sent": false, "clipboard_accessed": false });
    if let Err(error) = record_to(&evidence, Some("observer")) {
        eprintln!("FOLDEX_NATIVE_COLLISION_OBSERVER_EVIDENCE_ERROR={error}");
    }
}

fn record(evidence: &serde_json::Value) {
    record_to(evidence, None).expect("Write final owned-fixture evidence");
}

fn record_to(evidence: &serde_json::Value, suffix: Option<&str>) -> io::Result<()> {
    println!(
        "FOLDEX_NATIVE_COLLISION_EVIDENCE={}",
        serde_json::to_string(evidence).map_err(io::Error::other)?
    );
    let Some(path) = std::env::var_os("FOLDEX_NATIVE_PROBE_OUTPUT") else {
        return Ok(());
    };
    let mut path = PathBuf::from(path);
    if let Some(suffix) = suffix {
        let stem = path
            .file_stem()
            .ok_or_else(|| invalid("Evidence file name is missing"))?
            .to_string_lossy();
        path.set_file_name(format!("{stem}.{suffix}.json"));
    }
    let results = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))?;
    if !path.is_absolute() {
        return Err(invalid("Evidence path must be absolute"));
    }
    let parent = fs::canonicalize(
        path.parent()
            .ok_or_else(|| invalid("Evidence parent is missing"))?,
    )?;
    if !parent.starts_with(&results) {
        return Err(invalid("Evidence must stay within workspace results"));
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut file, evidence).map_err(io::Error::other)?;
    file.sync_all()
}

pub(super) fn cross_folder_collision() {
    let (desktop, station_name, desktop_name) = owned_offscreen_desktop();
    let fixture = super::native_probe_tests::Fixture::new("native-collision");
    let source_folder = fixture.root.join("source");
    let target_folder = fixture.root.join("destination");
    fs::create_dir(&source_folder).unwrap();
    fs::create_dir(&target_folder).unwrap();
    let source = source_folder.join("foldex-generated-collision.txt");
    let target = target_folder.join("foldex-generated-collision.txt");
    fs::write(&source, b"SOURCE fixture bytes must survive native Cancel").unwrap();
    fs::write(
        &target,
        b"DESTINATION fixture bytes must survive native Cancel",
    )
    .unwrap();

    let _apartment = Apartment::new().unwrap();
    let owner = Owner::new();
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let observer_done = done.clone();
    let observer_stop = stop.clone();
    let owner_value = owner.0 as isize;
    let observer = thread::Builder::new()
        .name("foldex-fixture-dialog-observer".into())
        .spawn(move || {
            observe(
                desktop,
                owner_value,
                observer_done,
                observer_stop,
                station_name,
                desktop_name,
            )
        })
        .unwrap();
    let result = transfer_report(
        &[source.clone()],
        &target_folder,
        &[true],
        owner_value,
        stop,
    );
    done.store(true, Ordering::Release);
    let observed = observer.join().expect("bounded observer thread");
    let source_preserved = fs::read(&source).ok().as_deref()
        == Some(b"SOURCE fixture bytes must survive native Cancel".as_slice());
    let target_preserved = fs::read(&target).ok().as_deref()
        == Some(b"DESTINATION fixture bytes must survive native Cancel".as_slice());
    let (aborted, native_result, completion, operation_error) = match &result {
        Ok(report) => (
            report.aborted,
            Some(report.result),
            report.message("Moved").ok(),
            None,
        ),
        Err(error) => (false, None, None, Some(error.to_string())),
    };
    record(
        &serde_json::json!({ "scenario": "cross_folder_collision_native_cancel", "observation": &observed,
        "any_operations_aborted": aborted, "native_hresult": native_result, "completion": completion,
        "operation_error": operation_error, "source_original_bytes_preserved": source_preserved,
        "destination_original_bytes_preserved": target_preserved, "desktop_input_sent": false,
        "clipboard_accessed": false, "fixture_root": &fixture.root }),
    );
    assert!(
        observed.setup_error.is_none(),
        "Observer setup failed: {observed:?}"
    );
    assert!(
        observed.conflict_index.is_some(),
        "No identifiable native collision decision was observed: {observed:?}"
    );
    assert!(
        observed.cancel_posts > 0,
        "No native owned-dialog Cancel/WM_CLOSE was posted"
    );
    assert!(
        !observed.timed_out,
        "Native conflict Cancel did not complete before the observer timeout"
    );
    assert!(
        source_preserved && target_preserved,
        "Native Cancel must preserve both original payloads"
    );
    let report = result.expect("Shell must return a native cancellation report");
    assert!(
        report.aborted || is_cancel(report.result),
        "Native operation did not report stopped/canceled: {report:?}"
    );
    assert!(
        !report.full_success(),
        "Canceled collision must not report a completed move"
    );
}
