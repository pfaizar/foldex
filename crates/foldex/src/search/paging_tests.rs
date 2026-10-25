use super::*;
fn context(entries: usize) -> (live_tests::Fixture, SearchHub, SearchClient, Context) {
    let fixture = live_tests::Fixture::new();
    let hub = SearchHub::unstarted_for_test();
    let client = hub.client();
    let cancel = Cancellation::new();
    let cache = Index::cache_dir_for(&fixture.cache(), &fixture.root()).unwrap();
    let mut builder = IndexBuilder::create(&cache, &fixture.root(), &cancel).unwrap();
    for first in (0..entries).step_by(256) {
        let rows = (first..(first + 256).min(entries))
            .map(|i| {
                foldex_index::IndexRecord::new(
                    format!("report{i:08}.txt").into(),
                    false,
                    i as u64,
                    100,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        builder.ingest_batch(&rows, &cancel).unwrap();
    }
    let index = builder.finish(true).unwrap();
    client.request(fixture.root(), Query::default(), 0, false);
    let context = Context::with_backend(
        &client.state,
        client.state.input().unwrap(),
        cache,
        Backend::Complete(index),
    );
    (fixture, hub, client, context)
}
fn ordered_files(path: &Path) -> usize {
    let stem = path.file_stem().unwrap().to_string_lossy();
    let data = path.with_file_name(format!("{stem}.data"));
    std::fs::read_dir(data)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("result-"))
        .count()
}
#[test]
fn neighboring_windows_cross_the_cached_head_without_materializing_an_order() {
    let (fixture, hub, client, mut context) = context(4096);
    let cancel = Cancellation::new();
    context.fetch_window(&cancel).unwrap();
    client.request(fixture.root(), Query::default(), 1024, false);
    context.replace_request(client.state.input().unwrap());
    context.fetch_window(&cancel).unwrap();
    assert_eq!(context.window.as_ref().unwrap().entries.len(), WINDOW_ROWS);
    assert_eq!(
        context.window.as_ref().unwrap().entries[0].name,
        "report00001024.txt"
    );
    assert_eq!(
        ordered_files(&context.backend.as_ref().unwrap().database_path()),
        0
    );
    context.backend.as_mut().unwrap().release_query_cache();
    assert_eq!(context.backend.as_ref().unwrap().retained_query_bytes(), 0);
    client.request(fixture.root(), Query::default(), 1792, false);
    context.replace_request(client.state.input().unwrap());
    context.fetch_window(&cancel).unwrap();
    assert_eq!(
        context.window.as_ref().unwrap().entries[0].name,
        "report00001792.txt"
    );
    assert_eq!(
        ordered_files(&context.backend.as_ref().unwrap().database_path()),
        0
    );
    drop(context);
    hub.shutdown();
}
#[test]
fn a_queued_deep_seek_does_not_reserve_the_coordinator_and_full_queues_do_not_spin() {
    let (fixture, hub, client, mut context) = context(4096);
    let (sender, receiver) = mpsc::sync_channel(1);
    let pool = Pool {
        sender: Some(sender),
        stop: Arc::new(AtomicBool::new(false)),
        threads: Vec::new(),
    };
    client.request(fixture.root(), Query::default(), 3000, false);
    context.replace_request(client.state.input().unwrap());
    let cancel = Cancellation::new();
    assert!(
        context
            .step(&fixture.cache(), false, &cancel, &cancel, Some(&pool))
            .unwrap()
    );
    assert!(context.pending_page.is_some());
    assert!(
        !context
            .step(&fixture.cache(), false, &cancel, &cancel, Some(&pool))
            .unwrap()
    );
    let other = hub.client();
    other.request(fixture.root(), Query::default(), 0, false);
    let reader =
        NativeIndex::open_database(&context.backend.as_ref().unwrap().database_path()).unwrap();
    let mut other_context = Context::with_backend(
        &other.state,
        other.state.input().unwrap(),
        context.cache_dir.clone(),
        Backend::Complete(reader),
    );
    assert!(
        other_context
            .step(&fixture.cache(), false, &cancel, &cancel, Some(&pool))
            .unwrap()
    );
    assert!(matches!(other.try_recv(), Some(SearchEvent::Window(_))));
    other.request(fixture.root(), Query::default(), 2500, false);
    other_context.replace_request(other.state.input().unwrap());
    assert!(
        !other_context
            .step(&fixture.cache(), false, &cancel, &cancel, Some(&pool))
            .unwrap()
    );
    let queued = receiver.try_recv().unwrap();
    context.pending_page = None;
    assert!(queued.cancel.is_cancelled());
    assert!(!cancel.is_cancelled());
    drop(other_context);
    drop(context);
    drop(pool);
    hub.shutdown();
}
#[test]
fn real_seek_workers_finish_latest_requests_and_keep_selected_prefixes() {
    let (fixture, hub, client, mut context) = context(4096);
    let pool = Pool::new(hub.shared.clone()).unwrap();
    let cancel = Cancellation::new();
    let boundary = context.backend.as_ref().unwrap().status();
    client.request_as_of(
        fixture.root(),
        Query::default(),
        3000,
        false,
        Some(boundary.clone()),
    );
    context.replace_request(client.state.input().unwrap());
    context
        .step(&fixture.cache(), false, &cancel, &cancel, Some(&pool))
        .unwrap();
    let latest = client.request_as_of(
        fixture.root(),
        Query::default(),
        2000,
        false,
        Some(boundary.clone()),
    );
    context.replace_request(client.state.input().unwrap());
    let until = Instant::now() + Duration::from_secs(10);
    while context.query_dirty {
        context
            .step(&fixture.cache(), false, &cancel, &cancel, Some(&pool))
            .unwrap();
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(1));
    }
    let Some(SearchEvent::Window(window)) = client.try_recv() else {
        panic!("no latest seek window")
    };
    assert_eq!(window.request, latest);
    assert_eq!(window.start, 2000);
    assert_eq!(window.snapshot.entries[0].name, "report00002000.txt");
    assert_eq!(window.status.generation, boundary.generation);
    assert_eq!(window.total, Some(4096));
    drop(context);
    hub.shutdown();
    drop(pool);
}

#[test]
fn cached_seek_reader_observes_a_completed_revision_with_the_same_row_count() {
    let fixture = live_tests::Fixture::new();
    let hub = SearchHub::unstarted_for_test();
    let pool = Pool::with_workers(hub.shared.clone(), 1).unwrap();
    let cancel = Cancellation::new();
    let mut builder = IndexBuilder::create(&fixture.cache(), &fixture.root(), &cancel).unwrap();
    for first in (0..2048).step_by(256) {
        let rows = (first..first + 256)
            .map(|i| {
                foldex_index::IndexRecord::new(format!("file{i:08}.txt").into(), false, 0, 0)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        builder.ingest_batch(&rows, &cancel).unwrap();
    }
    let building = builder.checkpoint().unwrap().status;
    let wait = |work: Work| {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = work.take() {
                break result.unwrap();
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
    };
    let spec = QuerySpec {
        kind: KindFilter::Files,
        include_total: true,
        ..Default::default()
    };
    let first = wait(
        pool.submit(building.clone(), spec.clone(), 1500, None, None, &cancel)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(first.status.state, foldex_index::BuildState::Building);
    let index = builder.finish(true).unwrap();
    let finished = index.status().clone();
    assert_eq!(building.entries, finished.entries);
    assert!(finished.revision > building.revision);
    let second = wait(
        pool.submit(finished.clone(), spec.clone(), 1500, None, None, &cancel)
            .unwrap()
            .unwrap(),
    );
    assert!(second.status.complete);
    assert_eq!(second.status.state, foldex_index::BuildState::Complete);
    let data = index.status().database_path.with_file_name(format!(
        "{}.data",
        index
            .status()
            .database_path
            .file_stem()
            .unwrap()
            .to_string_lossy()
    ));
    let files = || {
        let mut names = std::fs::read_dir(&data)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().starts_with("result-"))
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let before = files();
    assert_eq!(before.len(), 2);
    let third = wait(
        pool.submit(finished, spec, 1400, None, second.kind_counts, &cancel)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(third.total, Some(2048));
    assert_eq!(third.kind_counts, Some((0, 2048)));
    assert_eq!(
        files(),
        before,
        "reused facet counts must preserve a completed ordered cache"
    );
    hub.shutdown();
    drop(pool);
    drop(index);
}
