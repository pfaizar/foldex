use super::*;
use std::fs;

pub(crate) struct Fixture(pub(super) PathBuf);
impl Fixture {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "foldex-live-search-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("scope/child")).unwrap();
        Self(path)
    }
    pub(crate) fn root(&self) -> PathBuf {
        self.0.join("scope")
    }
    pub(crate) fn cache(&self) -> PathBuf {
        self.0.join("cache")
    }
    fn file(&self, path: &str, bytes: &[u8]) {
        fs::write(self.root().join(path), bytes).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(
            self.0.canonicalize().unwrap().parent(),
            Some(std::env::temp_dir().canonicalize().unwrap().as_path())
        );
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn query() -> Query {
    Query {
        text: "report".into(),
        kind: foldex_core::KindFilter::Files,
        ..Default::default()
    }
}
fn window(
    client: &SearchClient,
    id: u64,
    mut accepts: impl FnMut(&SearchWindow) -> bool,
) -> Arc<SearchWindow> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        while let Some(event) = client.try_recv() {
            match event {
                SearchEvent::Window(value) if value.request == id && accepts(&value) => {
                    return value;
                }
                SearchEvent::Failed { message, .. } => panic!("{message}"),
                _ => {}
            }
        }
        assert!(
            Instant::now() < until,
            "live search did not publish the expected revision"
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn complete(client: &SearchClient, id: u64, total: usize) -> Arc<SearchWindow> {
    window(client, id, |value| {
        value.total == Some(total) && value.status.complete && !value.status.from_cache
    })
}

#[cfg(windows)]
#[test]
fn shared_catalog_keeps_each_subscribers_requested_root_spelling() {
    let fixture = Fixture::new();
    fixture.file("report.txt", b"a");
    let alias = fixture.root().with_file_name("SCOPE");
    // A case-sensitive Windows directory must not be assumed to have this alias.
    if !alias.exists() {
        return;
    }
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let first = hub.client();
    let second = hub.client();
    let one = complete(&first, first.request(fixture.root(), query(), 0, false), 1);
    let two = complete(&second, second.request(alias.clone(), query(), 0, false), 1);
    assert_eq!(one.status.generation, two.status.generation);
    assert_eq!(two.snapshot.path, alias);
    assert_eq!(two.snapshot.entries[0].raw_name, "report.txt");
    hub.shutdown();
}

#[test]
fn directory_and_recursive_subscribers_use_distinct_catalogs_for_the_same_root() {
    let fixture = Fixture::new();
    fixture.file("report-root.txt", b"a");
    fixture.file("child/report-nested.txt", b"b");
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let directory = hub.client();
    let recursive = hub.client();
    let direct = directory.request_directory_as_of(fixture.root(), query(), 0, false, None);
    let deep = recursive.request(fixture.root(), query(), 0, false);
    let one = complete(&directory, direct, 1);
    let two = complete(&recursive, deep, 2);
    assert_ne!(one.status.database_path, two.status.database_path);
    assert_ne!(one.status.generation, two.status.generation);
    assert_eq!(
        one.snapshot.entries[0].relative_path.as_deref(),
        Some(Path::new("report-root.txt"))
    );
    let forged = directory.request_directory_as_of(
        fixture.root(),
        query(),
        0,
        false,
        Some(two.status.clone()),
    );
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(SearchEvent::Failed { request, .. }) = directory.try_recv() {
            assert_eq!(request, forged);
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    hub.shutdown();
}

#[test]
fn four_visible_scopes_and_a_launcher_finish_without_hiding_or_resubmitting() {
    let fixture = Fixture::new();
    let hub = SearchHub::unstarted_for_test();
    let mut panes = Vec::new();
    for index in 0..4 {
        let root = fixture.root().join(format!("pane{index}"));
        fs::create_dir(&root).unwrap();
        fs::write(root.join(format!("report{index}.txt")), b"a").unwrap();
        if index == 3 {
            for item in 0..2048 {
                fs::write(root.join(format!("other{item}.bin")), []).unwrap();
            }
        }
        let client = hub.client();
        let id = client.request(root, query(), 0, false);
        panes.push((client, id));
    }
    let launcher = hub.client();
    let launcher_id =
        launcher.request_launcher(fixture.root().join("pane0"), "report".into(), true);
    let shared = hub.shared.clone();
    let cache = fixture.cache();
    let started = Instant::now();
    let worker = thread::spawn(move || run(shared, cache));
    for (client, id) in &panes {
        complete(client, *id, 1);
    }
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(SearchEvent::Launcher {
            request,
            rows,
            complete: true,
            ..
        }) = launcher.try_recv()
        {
            assert_eq!(request, launcher_id);
            assert_eq!(rows.len(), 1);
            break;
        }
        assert!(
            Instant::now() < until,
            "launcher starved behind completed panes"
        );
        thread::sleep(Duration::from_millis(1));
    }
    println!(
        "five_visible_subscriptions_complete_ms={:.3}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    hub.shutdown();
    worker.join().unwrap();
}

#[test]
fn selected_generation_keeps_its_lease_while_other_visible_scopes_run() {
    let fixture = Fixture::new();
    fixture.file("report.txt", b"a");
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let selected = hub.client();
    let id = selected.request(fixture.root(), query(), 0, false);
    let original = complete(&selected, id, 1);
    let pinned = selected.request_as_of(
        fixture.root(),
        query(),
        0,
        false,
        Some(original.status.clone()),
    );
    complete(&selected, pinned, 1);
    let others = (0..4)
        .map(|index| {
            let root = fixture.0.join(format!("other{index}"));
            fs::create_dir(&root).unwrap();
            fs::write(root.join("report.txt"), b"b").unwrap();
            let client = hub.client();
            let id = client.request(root, query(), 0, false);
            (client, id)
        })
        .collect::<Vec<_>>();
    for (client, id) in &others {
        complete(client, *id, 1);
    }
    let selection = selected.resolve_selection(query(), original.status.clone(), vec![(0, 0)]);
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(SearchEvent::Selection {
            selection: id,
            result,
        }) = selected.try_recv()
        {
            assert_eq!(id, selection);
            assert_eq!(result.unwrap(), vec![fixture.root().join("report.txt")]);
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    hub.shutdown();
}

#[test]
fn open_search_observes_nested_create_rename_resize_and_delete_without_refresh() {
    let fixture = Fixture::new();
    fixture.file("report1.txt", b"a");
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let client = hub.client();
    let id = client.request(fixture.root(), query(), 0, false);
    complete(&client, id, 1);
    let started = Instant::now();
    fixture.file("child/report2.txt", b"bb");
    let added = complete(&client, id, 2);
    let create_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(added.snapshot.entries.iter().any(|entry| {
        entry
            .relative_path
            .as_ref()
            .is_some_and(|path| path == Path::new("child/report2.txt"))
    }));
    fs::rename(
        fixture.root().join("child/report2.txt"),
        fixture.root().join("child/other.bin"),
    )
    .unwrap();
    complete(&client, id, 1);
    let original_time = fs::metadata(fixture.root().join("report1.txt"))
        .unwrap()
        .modified()
        .unwrap();
    fixture.file("report1.txt", b"updated payload");
    fs::OpenOptions::new()
        .write(true)
        .open(fixture.root().join("report1.txt"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    window(&client, id, |value| {
        value.status.complete
            && !value.status.from_cache
            && value
                .snapshot
                .entries
                .iter()
                .any(|entry| entry.name == "report1.txt" && entry.size == 15)
    });
    fs::remove_file(fixture.root().join("report1.txt")).unwrap();
    complete(&client, id, 0);
    println!(
        "IMPROVEMENT {}",
        serde_json::json!({"stage":2,"external_create_observed_ms":create_ms,"nested_create_rename_resize_delete":true})
    );
    hub.shutdown();
}

#[test]
fn unchanged_scope_reactivation_reuses_the_same_generation() {
    let fixture = Fixture::new();
    fixture.file("report.txt", b"a");
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let client = hub.client();
    let id = client.request(fixture.root(), query(), 0, false);
    let first = complete(&client, id, 1);
    for _ in 0..4 {
        client.set_active(false);
        client.set_active(true);
        let reused = complete(&client, id, 1);
        assert_eq!(reused.status.generation, first.status.generation);
    }
    let cache = Index::cache_dir_for(&fixture.cache(), &fixture.root()).unwrap();
    let generations = fs::read_dir(cache)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "sqlite3")
        })
        .count();
    assert_eq!(generations, 1);
    println!(
        "IMPROVEMENT {}",
        serde_json::json!({"stage":2,"reactivations":4,"catalog_generations":generations,"additional_full_scans":0})
    );
    hub.shutdown();
}

#[test]
fn explorer_and_launcher_subscribers_share_one_catalog_and_cancel_independently() {
    let fixture = Fixture::new();
    fixture.file("report1.txt", b"a");
    fixture.file("report2.txt", b"b");
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let first = hub.client();
    let second = hub.client();
    let a = first.request(fixture.root(), query(), 0, false);
    let initial = complete(&first, a, 2);
    let b = second.request(
        fixture.root(),
        Query {
            kind: foldex_core::KindFilter::Files,
            ..Default::default()
        },
        0,
        false,
    );
    let shared = complete(&second, b, 2);
    assert_eq!(shared.status.generation, initial.status.generation);
    first.cancel();
    fixture.file("report3.txt", b"c");
    let changed = complete(&second, b, 3);
    assert_ne!(changed.status.generation, initial.status.generation);
    assert!(first.try_recv().is_none());
    hub.shutdown();
}

#[test]
fn selected_generation_remains_immutable_while_live_results_change() {
    let fixture = Fixture::new();
    fixture.file("report1.txt", b"a");
    fixture.file("report2.txt", b"b");
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let selected = hub.client();
    let live = hub.client();
    let id = selected.request(fixture.root(), query(), 0, false);
    let original = complete(&selected, id, 2);
    let pinned = selected.request_as_of(
        fixture.root(),
        query(),
        0,
        false,
        Some(original.status.clone()),
    );
    window(&selected, pinned, |value| {
        value.total == Some(2) && value.status.generation == original.status.generation
    });
    fixture.file("report0.txt", b"new");
    fs::remove_file(fixture.root().join("report2.txt")).unwrap();
    let live_id = live.request(fixture.root(), query(), 0, false);
    window(&live, live_id, |value| {
        value.total == Some(2)
            && value.status.complete
            && !value.status.from_cache
            && value.status.generation != original.status.generation
    });
    let selection = selected.resolve_selection(query(), original.status.clone(), vec![(0, 0)]);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(SearchEvent::Selection {
            selection: id,
            result,
        }) = selected.try_recv()
        {
            assert_eq!(id, selection);
            assert_eq!(result.unwrap(), vec![fixture.root().join("report1.txt")]);
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    hub.shutdown();
}

#[test]
fn indexing_its_own_cache_does_not_trigger_a_refresh_loop() {
    let fixture = Fixture::new();
    fixture.file("report1.txt", b"a");
    let cache = fixture.root().join("cache");
    let hub = SearchHub::new(cache.clone(), Arc::new(|| {})).unwrap();
    let client = hub.client();
    let id = client.request(fixture.root(), query(), 0, false);
    let first = complete(&client, id, 1);
    fs::write(cache.join("report-cache.txt"), b"excluded").unwrap();
    thread::sleep(Duration::from_millis(300));
    let again = client.request(fixture.root(), query(), 0, false);
    let reused = complete(&client, again, 1);
    assert_eq!(reused.status.generation, first.status.generation);
    fixture.file("child/report2.txt", b"b");
    complete(&client, again, 2);
    hub.shutdown();
}

#[test]
fn replaced_root_is_reattached_and_a_new_scope_cannot_pin_another_scopes_rows() {
    let fixture = Fixture::new();
    fixture.file("report-old.txt", b"a");
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let client = hub.client();
    let id = client.request(fixture.root(), query(), 0, false);
    let old = complete(&client, id, 1);
    fs::remove_dir_all(fixture.root()).unwrap();
    fs::create_dir(fixture.root()).unwrap();
    fixture.file("report-new.txt", b"b");
    window(&client, id, |value| {
        value.status.complete
            && !value.status.from_cache
            && value
                .snapshot
                .entries
                .iter()
                .any(|entry| entry.name == "report-new.txt")
    });
    let other = fixture.0.join("other");
    fs::create_dir(&other).unwrap();
    let pinned = client.request_as_of(other, query(), 0, false, Some(old.status.clone()));
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(SearchEvent::Failed { request, .. }) = client.try_recv() {
            assert_eq!(request, pinned);
            break;
        }
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(2));
    }
    hub.shutdown();
}

#[cfg(unix)]
#[test]
fn watched_scopes_do_not_follow_symbolic_links_or_reindex_file_reads() {
    let fixture = Fixture::new();
    fixture.file("report.txt", b"a");
    let outside = fixture.0.join("outside");
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.root().join("link")).unwrap();
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let client = hub.client();
    let id = client.request(fixture.root(), query(), 0, false);
    let original = complete(&client, id, 1);
    fs::write(outside.join("report-outside.txt"), b"excluded").unwrap();
    fs::read(fixture.root().join("report.txt")).unwrap();
    thread::sleep(Duration::from_millis(250));
    let id = client.request(fixture.root(), query(), 0, false);
    let unchanged = complete(&client, id, 1);
    assert_eq!(original.status.generation, unchanged.status.generation);
    hub.shutdown();
}
