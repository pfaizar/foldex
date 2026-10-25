use super::*;
use std::{
    sync::{atomic::AtomicUsize, mpsc},
    time::{Duration, Instant},
};

fn path(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}
fn retry(mut call: impl FnMut() -> Result<(), String>) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match call() {
            Ok(()) => return,
            Err(error) if error.contains("mailbox busy") && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("{error}"),
        }
    }
}
fn receive(menu: &ShellMenu) -> Event {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(event) = menu.try_recv() {
            return event;
        }
        assert!(Instant::now() < deadline, "shell event deadline");
        thread::sleep(Duration::from_millis(1));
    }
}
fn join(mut menu: ShellMenu) {
    menu.shutdown();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !menu.is_worker_finished() {
        assert!(Instant::now() < deadline, "shell shutdown deadline");
        thread::sleep(Duration::from_millis(1));
    }
    menu.worker.take().unwrap().join().unwrap();
}

struct Fake {
    opened: u64,
    calls: Arc<AtomicUsize>,
    start: Option<mpsc::Sender<()>>,
    gate: Option<mpsc::Receiver<()>>,
}
impl Backend for Fake {
    fn open(&mut self, open: &Open) -> Result<Snapshot, String> {
        self.opened = open.stamp.request;
        if let Some(start) = self.start.take() {
            start.send(()).unwrap();
        }
        if let Some(gate) = self.gate.take() {
            gate.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        Ok(Snapshot {
            items: vec![ShellItem {
                id: 1,
                label: format!("Request {}", self.opened),
                enabled: true,
                ..Default::default()
            }],
            truncated: false,
        })
    }
    fn expand(&mut self, _: u32) -> Result<Snapshot, String> {
        self.open(&Open {
            stamp: Stamp {
                request: self.opened,
                serial: 0,
            },
            owner: 0,
            paths: vec![],
            background: false,
            options: MenuOptions::default(),
        })
    }
    fn invoke(&mut self, _: u32) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn close(&mut self) {}
}
fn fake(
    gated: bool,
) -> (
    ShellMenu,
    Arc<AtomicUsize>,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
) {
    let calls = Arc::new(AtomicUsize::new(0));
    let worker_calls = Arc::clone(&calls);
    let (start_tx, start_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    let menu = ShellMenu::spawn(Arc::new(|| {}), move || Fake {
        opened: 0,
        calls: worker_calls,
        start: gated.then_some(start_tx),
        gate: gated.then_some(gate_rx),
    })
    .unwrap();
    (menu, calls, start_rx, gate_tx)
}

#[test]
fn labels_preserve_escaped_ampersands_and_separate_shortcut_chips() {
    assert_eq!(
        split_label("Save && &Close\tCtrl+Shift+S"),
        ("Save & Close".into(), Some("Ctrl+Shift+S".into()))
    );
    assert_eq!(split_label("&Open"), ("Open".into(), None));
}
#[test]
fn opening_or_search_expansion_never_invokes_a_command() {
    let (menu, calls, _, _) = fake(false);
    retry(|| menu.open(1, 0, vec![path("fixture.txt")], false));
    assert!(matches!(receive(&menu), Event::Ready { request: 1, .. }));
    retry(|| menu.expand(1, 1));
    assert!(matches!(receive(&menu), Event::Updated { request: 1, .. }));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    join(menu);
}
#[test]
fn superseding_open_discards_slow_old_enumeration_and_rejects_old_invocation() {
    let (menu, calls, started, release) = fake(true);
    retry(|| menu.open(10, 0, vec![path("old.txt")], false));
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    retry(|| menu.open(11, 0, vec![path("new.txt")], false));
    assert!(menu.invoke(10, 1).is_err());
    release.send(()).unwrap();
    assert!(matches!(receive(&menu), Event::Ready { request: 11, .. }));
    assert!(menu.try_recv().is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    join(menu);
}
#[test]
fn committed_user_invocation_survives_close_and_runs_once() {
    let (menu, calls, _, _) = fake(false);
    retry(|| menu.open(1, 0, vec![path("chosen.txt")], false));
    receive(&menu);
    retry(|| menu.invoke(1, 1));
    retry(|| menu.close(1));
    assert!(menu.invoke(1, 1).is_err());
    assert!(matches!(
        receive(&menu),
        Event::Invoked {
            request: 1,
            item_id: 1
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    join(menu);
}
#[test]
fn invalid_paths_and_selection_sizes_do_not_enter_native_queue() {
    let (menu, calls, _, _) = fake(false);
    for paths in [
        vec![],
        vec![PathBuf::from("relative")],
        vec![path("bad\0.txt")],
        vec![path("x"); MAX_PATHS + 1],
    ] {
        assert!(menu.open(1, 0, paths, false).is_err());
    }
    assert!(
        menu.open(1, 0, vec![path("one"), path("two")], true)
            .is_err()
    );
    {
        let _guard = menu.shared.state.lock().unwrap();
        assert!(
            menu.open(1, 0, vec![path("locked")], false)
                .unwrap_err()
                .contains("mailbox busy")
        );
        assert!(menu.try_recv().is_none());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    join(menu);
}
#[test]
fn event_mailbox_coalesces_snapshots_and_stays_bounded() {
    let mut state = State::default();
    let stamp = Stamp {
        request: 1,
        serial: 1,
    };
    state.desired = Some(stamp);
    for _ in 0..100 {
        state.publish(
            stamp,
            Event::Updated {
                request: 1,
                items: vec![],
                truncated: false,
            },
            false,
        );
    }
    assert_eq!(state.events.len(), 1);
    for id in 0..100 {
        state.publish(
            stamp,
            Event::Invoked {
                request: 1,
                item_id: id,
            },
            true,
        );
    }
    assert!(state.events.len() <= MAX_EVENTS);
}
#[test]
fn idle_sta_shutdown_signals_without_a_polling_timer() {
    for _ in 0..16 {
        let (menu, _, _, _) = fake(false);
        join(menu);
    }
}

#[test]
fn slow_extension_does_not_hold_submission_or_properties_lane() {
    let (menu, calls, started, release) = fake(true);
    retry(|| menu.open(1, 0, vec![path("slow.txt")], false));
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    let start = Instant::now();
    let properties = Properties::new(Arc::new(|| {}));
    properties.spawn(|| Ok(())).unwrap();
    while properties.try_recv().is_none() {
        assert!(start.elapsed() < Duration::from_secs(1));
        thread::sleep(Duration::from_millis(1));
    }
    let mut times = Vec::new();
    for request in 2..=4097 {
        let submit = Instant::now();
        retry(|| menu.open(request, 0, vec![path("next.txt")], false));
        assert!(menu.try_recv().is_none());
        retry(|| menu.close(request));
        times.push(submit.elapsed().as_secs_f64() * 1000.0);
    }
    // Bounded latest-open storage, even with thousands of opens while a native
    // call is stuck. The UI never takes the native worker's call stack/lock.
    assert!(menu.shared.state.lock().unwrap().pending_open.is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    times.sort_by(f64::total_cmp);
    eprintln!(
        "blocked shell: 4096 submissions p95={:.4}ms max={:.4}ms total={:.2}ms",
        times[3891],
        times[4095],
        start.elapsed().as_secs_f64() * 1000.0
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    release.send(()).unwrap();
    join(menu);
}

#[cfg(windows)]
#[test]
fn native_generated_selection_and_background_enumerate_without_invoking() {
    let _guard = super::windows::NATIVE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let menu = ShellMenu::new(Arc::new(|| {})).unwrap();
    retry(|| menu.open(1, 0, vec![fixture.file.clone()], false));
    let selected = receive(&menu);
    match selected {
        Event::Ready { items, .. } => {
            assert!(!items.is_empty());
            fn inspect(items: &[ShellItem], depth: usize, count: &mut usize, text: &mut usize) {
                assert!(depth < MAX_DEPTH);
                for item in items {
                    *count += 1;
                    *text += item.label.len()
                        + item.help.len()
                        + item.canonical_verb.as_ref().map_or(0, String::len)
                        + item.shortcut.as_ref().map_or(0, String::len);
                    if !item.children.is_empty() {
                        inspect(&item.children, depth + 1, count, text);
                    }
                }
            }
            let (mut count, mut text) = (0, 0);
            inspect(&items, 0, &mut count, &mut text);
            assert!(count <= MAX_ITEMS && text <= MAX_TEXT_BYTES);
            assert!(
                items.iter().any(|item| item.canonical_verb.is_some()),
                "native command metadata"
            );
            if let Some(submenu) = items.iter().find(|item| item.has_submenu && item.enabled) {
                retry(|| menu.expand(1, submenu.id));
                assert!(matches!(receive(&menu), Event::Updated { request: 1, .. }));
            }
        }
        other => panic!("selection enumeration failed: {other:?}"),
    }
    retry(|| menu.open(2, 0, vec![fixture.root.clone()], true));
    let background = receive(&menu);
    join(menu);
    match background {
        Event::Ready { items, .. } => assert!(!items.is_empty(), "background menu"),
        other => panic!("background enumeration failed: {other:?}"),
    }
    drop(fixture);
}

#[cfg(windows)]
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    file: PathBuf,
}
#[cfg(windows)]
impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/shell-test-fixtures");
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = base.join(format!("owned-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("Generated Menu Notes.txt");
        std::fs::write(&file, "Generated menu fixture; never invoke its verbs.\n").unwrap();
        Self { base, root, file }
    }
}
#[cfg(windows)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let root = self.root.canonicalize().unwrap();
        assert!(root.starts_with(&self.base) && root != self.base);
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("owned-")
        );
        std::fs::remove_file(&self.file).unwrap();
        std::fs::remove_dir(&root).unwrap();
    }
}

#[cfg(windows)]
use std::path::Path;
