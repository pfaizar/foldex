use super::*;
use serde_json::json;
use std::time::Instant;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "foldex-session-regression-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn preferences(&self) -> PathBuf {
        self.0.join("foldex-preferences.json")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let root = self.0.canonicalize().unwrap();
        assert_eq!(
            root.parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        fs::remove_dir_all(root).unwrap();
    }
}
fn wait(writer: &Persistence, id: u64) {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some((revision, result)) = writer.poll() {
            result.unwrap();
            if revision == id {
                return;
            }
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn coalesced_workspace_saves_are_atomic_and_latest_wins() {
    let fixture = Fixture::new();
    let writer = Persistence::new(Arc::new(|| {})).unwrap();
    let path = fixture.0.join("workspace.json");
    let mut revision = 0;
    for n in 0..40 {
        loop {
            match writer.save(path.clone(), format!("{{\"revision\":{n}}}").into_bytes()) {
                Ok(id) => {
                    revision = id;
                    break;
                }
                Err(SubmitError::Busy) => thread::yield_now(),
                Err(error) => panic!("{error}"),
            }
        }
    }
    wait(&writer, revision);
    assert_eq!(fs::read_to_string(path).unwrap(), "{\"revision\":39}");
}

#[test]
fn two_window_saves_preserve_preferences_and_independent_workspaces_in_both_orders() {
    for reverse in [false, true] {
        let fixture = Fixture::new();
        let base = json!({"bookmarks":["a"],"hotkeys":[{"command":"Open","sequence":[1]}],"launcher":{"background":true},"workspace":{"active":0}});
        let paths = [
            window_path(&fixture.0, false),
            window_path(&fixture.0, true),
        ];
        let mut first = base.clone();
        first["bookmarks"] = json!(["a", "b"]);
        first["hotkeys"][0]["sequence"] = json!([2]);
        first["workspace"]["active"] = json!(1);
        let mut second = base.clone();
        second["bookmarks"] = json!(["a", "c"]);
        second["launcher"]["background"] = json!(false);
        second["workspace"]["active"] = json!(2);
        let inputs = [first, second];
        for index in if reverse { [1, 0] } else { [0, 1] } {
            let request = Request {
                revision: 1,
                path: paths[index].clone(),
                bytes: serde_json::to_vec(&inputs[index]).unwrap(),
                shared: Some((fixture.preferences(), Arc::new(shared::preferences(&base)))),
            };
            save_request(&request, |_| {}).unwrap();
        }
        let merged = shared::current(&fixture.preferences()).unwrap().unwrap();
        let bookmarks = merged["bookmarks"].as_array().unwrap();
        assert_eq!(bookmarks.len(), 3);
        for value in ["a", "b", "c"] {
            assert!(bookmarks.contains(&json!(value)));
        }
        assert_eq!(merged["hotkeys"][0]["sequence"], json!([2]));
        assert_eq!(merged["launcher"]["background"], false);
        for (index, path) in paths.iter().enumerate() {
            let stored: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            assert_eq!(stored["workspace"]["active"], index + 1);
            assert!(stored.get("bookmarks").is_none());
        }
    }
}

#[test]
fn unchanged_shared_save_preserves_file_and_revision_and_retry_is_idempotent() {
    let fixture = Fixture::new();
    let base = json!({"bookmarks":["a"]});
    let local = json!({"bookmarks":["a","b"]});
    let first = shared::save(&fixture.preferences(), &base, &local).unwrap();
    let bytes = fs::read(fixture.preferences()).unwrap();
    let modified = fs::metadata(fixture.preferences())
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(
        shared::save(&fixture.preferences(), &base, &local).unwrap(),
        first
    );
    assert_eq!(fs::read(fixture.preferences()).unwrap(), bytes);
    assert_eq!(
        fs::metadata(fixture.preferences())
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
}

#[test]
fn corrupt_or_future_preferences_are_preserved_on_load_and_save() {
    for bytes in [
        b"broken".as_slice(),
        br#"{"version":99,"revision":1,"preferences":{}}"#,
    ] {
        let fixture = Fixture::new();
        fs::write(fixture.preferences(), bytes).unwrap();
        let mut legacy = json!({"bookmarks":["a"]});
        let original = legacy.clone();
        assert!(shared::load(&fixture.preferences(), &mut legacy).is_err());
        assert_eq!(legacy, original);
        assert!(shared::save(&fixture.preferences(), &json!({}), &json!({})).is_err());
        assert_eq!(fs::read(fixture.preferences()).unwrap(), bytes);
    }
}

#[test]
fn loading_migrates_legacy_preferences_and_merges_only_shared_fields() {
    let fixture = Fixture::new();
    let base = json!({"bookmarks":["a"],"workspace":{"active":7}});
    shared::save(
        &fixture.preferences(),
        &json!({}),
        &shared::preferences(&base),
    )
    .unwrap();
    let mut auxiliary = json!({"bookmarks":[],"workspace":{"active":2}});
    shared::load(&fixture.preferences(), &mut auxiliary).unwrap();
    assert_eq!(auxiliary["bookmarks"], json!(["a"]));
    assert_eq!(auxiliary["workspace"]["active"], 2);
}

#[test]
fn remote_refresh_preserves_local_unsaved_edits_without_reapplying_history() {
    let fixture = Fixture::new();
    let base = json!({"bookmarks":["a"],"launcher":{"background":true}});
    let mut writer =
        Persistence::new_session(Arc::new(|| {}), fixture.preferences(), &base).unwrap();
    let remote = json!({"bookmarks":["a","b"],"launcher":{"background":true}});
    *writer.mailbox.preferences.lock().unwrap() = Some(Ok(shared::preferences(&remote)));
    let local =
        json!({"bookmarks":["a","c"],"launcher":{"background":false},"workspace":{"active":3}});
    let merged = writer.poll_preferences(&local).unwrap().unwrap();
    assert_eq!(merged["bookmarks"], json!(["a", "b", "c"]));
    assert_eq!(merged["launcher"]["background"], false);
    assert_eq!(merged["workspace"]["active"], 3);
}

#[test]
fn valid_saved_layouts_fit_after_history_compaction_and_have_recoverable_state() {
    use crate::{
        app::Session,
        navigation::{Navigation, Route},
        workspace::{NamedWorkspace, PaneState, TabState, WorkspaceState},
    };
    let fixture = Fixture::new();
    let mut workspace = WorkspaceState::default();
    for tab in 0..4 {
        let mut nav = Navigation::new(Route::Home);
        for n in 0..64 {
            nav.visit(Route::Folder(PathBuf::from(format!(
                "C:/Users/Auditor/Documents/projects/{}/tab-{tab}/folder-{n:02}",
                "x".repeat(120)
            ))));
        }
        let route = nav.current().clone();
        let Route::Folder(path) = route.clone() else {
            unreachable!()
        };
        workspace.tabs.push(TabState {
            left: PaneState {
                path,
                view: Default::default(),
                route: Some(route),
                navigation: Some(nav),
            },
            right: None,
            vertical: false,
            split: 0.5,
        });
    }
    let mut session = Session::default();
    session.workspace = Some(workspace.clone());
    for n in 0..3 {
        session.saved_layouts.push(NamedWorkspace {
            name: format!("Layout {n}"),
            state: workspace.clone(),
        });
    }
    let before = serde_json::to_vec_pretty(&session).unwrap().len();
    session.compact_layouts();
    let bytes = serde_json::to_vec(&session).unwrap();
    let after = bytes.len();
    assert!(before > 256 * 1024);
    assert!(after < 256 * 1024);
    let path = fixture.0.join("workspace.json");
    save_request(
        &Request {
            revision: 1,
            path: path.clone(),
            bytes,
            shared: Some((fixture.preferences(), Arc::new(json!({})))),
        },
        |_| {},
    )
    .unwrap();
    let mut stored: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    shared::load(&fixture.preferences(), &mut stored).unwrap();
    let restored: Session = serde_json::from_value(stored).unwrap();
    assert_eq!(restored.workspace.unwrap().tabs.len(), 4);
    assert_eq!(restored.saved_layouts.len(), 3);
    assert!(restored.saved_layouts.iter().all(|layout| {
        layout
            .state
            .tabs
            .iter()
            .all(|tab| tab.left.navigation.is_none())
    }));
    println!(
        "IMPROVEMENT {}",
        json!({"stage":1,"baseline_pretty_bytes":before,"compact_bytes":after,"workspace_bytes":fs::metadata(path).unwrap().len(),"shared_bytes":fs::metadata(fixture.preferences()).unwrap().len()})
    );
}

#[test]
fn permanent_size_errors_are_distinct_from_transient_capacity() {
    let writer = Persistence::new(Arc::new(|| {})).unwrap();
    let error = writer
        .save(PathBuf::from("unused"), vec![0; MAX_SESSION_BYTES + 1])
        .unwrap_err();
    assert!(matches!(error, SubmitError::TooLarge));
    assert!(!error.retryable());
    assert!(SubmitError::Busy.retryable());
}

#[test]
fn simultaneous_writers_do_not_lose_updates_or_rewrite_valid_json() {
    let fixture = Fixture::new();
    let path = fixture.preferences();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for name in ["b", "c"] {
        let path = path.clone();
        let barrier = barrier.clone();
        workers.push(thread::spawn(move || {
            barrier.wait();
            shared::save(
                &path,
                &json!({"bookmarks":["a"]}),
                &json!({"bookmarks":["a",name]}),
            )
            .unwrap()
        }));
    }
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    let result = shared::current(&path).unwrap().unwrap();
    assert_eq!(result["bookmarks"].as_array().unwrap().len(), 3);
}

#[test]
fn failed_workspace_write_acknowledges_preferences_and_preserves_last_good_workspace() {
    let fixture = Fixture::new();
    let blocker = fixture.0.join("not-a-directory");
    fs::write(&blocker, b"unchanged").unwrap();
    let base = json!({"bookmarks":["a"]});
    let desired = json!({"bookmarks":["a","b"],"workspace":{}});
    let request = Request {
        revision: 1,
        path: blocker.join("workspace.json"),
        bytes: serde_json::to_vec(&desired).unwrap(),
        shared: Some((fixture.preferences(), Arc::new(base))),
    };
    let published = std::cell::RefCell::new(None);
    assert!(save_request(&request, |value| *published.borrow_mut() = Some(value)).is_err());
    assert_eq!(
        published.borrow().as_ref().unwrap()["bookmarks"],
        json!(["a", "b"])
    );
    assert_eq!(fs::read(blocker).unwrap(), b"unchanged");
    assert!(save_request(&request, |_| {}).is_err());
    assert_eq!(
        shared::current(&fixture.preferences()).unwrap().unwrap()["bookmarks"],
        json!(["a", "b"])
    );
}

#[test]
fn concurrent_collection_overflow_is_visible_and_preserves_existing_preferences() {
    let fixture = Fixture::new();
    let base = json!({"bookmarks":(0..31).map(|n|n.to_string()).collect::<Vec<_>>()});
    let mut first = base.clone();
    first["bookmarks"]
        .as_array_mut()
        .unwrap()
        .push(json!("first"));
    let mut second = base.clone();
    second["bookmarks"]
        .as_array_mut()
        .unwrap()
        .push(json!("second"));
    shared::save(&fixture.preferences(), &base, &first).unwrap();
    let before = fs::read(fixture.preferences()).unwrap();
    assert!(shared::save(&fixture.preferences(), &base, &second).is_err());
    assert_eq!(fs::read(fixture.preferences()).unwrap(), before);
}
