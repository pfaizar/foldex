//! Only run by the private-window-station driver. Never touches WinSta0's
//! clipboard/windows, and never sends keyboard/mouse input or invokes app verbs.
use super::*;
use std::{ptr, sync::atomic::AtomicU32};
use windows_sys::Win32::{
    Foundation::*,
    System::{DataExchange::*, Memory::*, StationsAndDesktops::*, Threading::GetCurrentThreadId},
    UI::WindowsAndMessaging::*,
};

fn station_name() -> String {
    let mut name = [0u16; 256];
    let mut needed = 0;
    assert_ne!(
        unsafe {
            GetUserObjectInformationW(
                GetProcessWindowStation(),
                UOI_NAME as i32,
                name.as_mut_ptr().cast(),
                (name.len() * 2) as u32,
                &mut needed,
            )
        },
        0
    );
    String::from_utf16_lossy(&name[..name.iter().position(|unit| *unit == 0).unwrap()])
}

fn add_flag(owner: HWND, name: &str, value: u32) {
    assert_ne!(unsafe { OpenClipboard(owner) }, 0);
    let name: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, 4) };
    assert!(!memory.is_null());
    let pointer = unsafe { GlobalLock(memory) } as *mut u32;
    assert!(!pointer.is_null());
    unsafe {
        pointer.write_unaligned(value);
        GlobalUnlock(memory);
        assert!(!SetClipboardData(format, memory).is_null());
        CloseClipboard();
    }
}

unsafe extern "system" fn close_property_sheet(hwnd: HWND, parameter: LPARAM) -> i32 {
    let mut process = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut process);
    }
    if process == std::process::id() {
        let mut class = [0u16; 32];
        let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        if String::from_utf16_lossy(&class[..length.max(0) as usize]) == "#32770" {
            let count = unsafe { &*(parameter as *const AtomicU32) };
            count.fetch_add(1, Ordering::Release);
            unsafe {
                PostMessageW(hwnd, WM_CLOSE, 0, 0);
            }
        }
    }
    1
}

#[test]
#[ignore = "requires run-native-services.ps1 private window station"]
fn native_isolated_services() {
    let station = station_name();
    assert!(
        station.starts_with("FoldexProbe_"),
        "Refusing clipboard/UI checks on {station}"
    );
    let output =
        PathBuf::from(std::env::var_os("FOLDEX_NATIVE_PROBE_OUTPUT").expect("probe output"));
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    assert!(
        output
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(&root)
    );
    assert!(!output.exists());
    let class: Vec<_> = "STATIC".encode_utf16().chain(Some(0)).collect();
    let owner = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null(),
        )
    };
    assert!(!owner.is_null());
    let (signal, _) = mpsc::sync_channel(1);
    let native = native::Native::new(
        false,
        true,
        "Ctrl+Alt+Space".into(),
        Arc::new(|| {}),
        signal,
        false,
        Some(format!("Foldex.Probe.Listener.{}", std::process::id())),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while native.state.window() == 0
        || native.state.events.load(Ordering::Acquire) & native::STATUS == 0
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let text = "Foldex isolated text Ω 😀\nsecond line";
    let start = Instant::now();
    providers::write_clipboard(text, owner as isize).unwrap();
    assert_eq!(providers::clipboard_text().as_deref(), Some(text));
    let copy_ms = start.elapsed().as_secs_f64() * 1000.0;
    while native.state.clipboard_version.load(Ordering::Acquire) == 0 {
        assert!(Instant::now() < deadline, "Clipboard notification");
        std::thread::sleep(Duration::from_millis(5));
    }
    for (name, value) in [
        ("CanIncludeInClipboardHistory", 0),
        ("ExcludeClipboardContentFromMonitorProcessing", 1),
        ("Clipboard Viewer Ignore", 1),
    ] {
        providers::write_clipboard(text, owner as isize).unwrap();
        add_flag(owner, name, value);
        assert!(providers::clipboard_text().is_none(), "respects {name}");
    }
    providers::write_clipboard(text, owner as isize).unwrap();
    assert!(providers::write_clipboard(&"x".repeat(40_000), owner as isize).is_err());
    assert_eq!(
        providers::clipboard_text().as_deref(),
        Some(text),
        "oversized write is non-destructive"
    );
    drop(native);
    unsafe {
        DestroyWindow(owner);
    }

    let base = root.join("target/native-service-fixtures");
    std::fs::create_dir_all(&base).unwrap();
    let path = base.join(format!("owned-native-{}.txt", std::process::id()));
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .unwrap()
        .write_all(b"Generated native Properties fixture\n")
        .unwrap();
    let desktop = unsafe { GetThreadDesktop(GetCurrentThreadId()) } as isize;
    let sheets = Arc::new(AtomicU32::new(0));
    let count = sheets.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let closer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            unsafe {
                EnumDesktopWindows(
                    desktop as _,
                    Some(close_property_sheet),
                    Arc::as_ptr(&count) as isize,
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let properties = foldex_shell::Properties::new(Arc::new(|| {}));
    let start = Instant::now();
    properties.show(vec![path.clone()]).unwrap();
    let result = loop {
        if let Some(result) = properties.try_recv() {
            break result;
        }
        assert!(
            start.elapsed() < Duration::from_secs(12),
            "Native Properties deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    stop.store(true, Ordering::Release);
    closer.join().unwrap();
    result.unwrap();
    assert!(
        sheets.load(Ordering::Acquire) > 0,
        "Actual native property sheet appeared"
    );
    let properties_ms = start.elapsed().as_secs_f64() * 1000.0;
    std::fs::remove_file(path).unwrap();
    std::fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
        "private_window_station": station, "interactive_clipboard_touched": false,
        "clipboard_roundtrip_ms": copy_ms, "clipboard_notifications": true, "exclusion_formats_checked": 3,
        "oversize_write_preserved_clipboard": true, "native_property_sheets": sheets.load(Ordering::Acquire),
        "properties_open_and_close_ms": properties_ms, "passed": true,
    })).unwrap()).unwrap();
}
