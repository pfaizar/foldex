use super::*;
use foldex_index::{IndexBuilder as LegacyBuilder, IndexRecord};

#[test]
fn structured_conditions_agree_in_memory_native_and_legacy_with_deep_cursors() {
    let fixture = live_tests::Fixture::new();
    let cancel = Cancellation::new();
    let mut native =
        IndexBuilder::create(&fixture.cache().join("native"), &fixture.root(), &cancel).unwrap();
    let mut legacy =
        LegacyBuilder::create(&fixture.cache().join("legacy"), &fixture.root(), &cancel).unwrap();
    let mut entries = Vec::new();
    for first in (0..4000).step_by(128) {
        let mut records = (first..(first + 128).min(4000))
            .map(|i| {
                let parent = if i % 3 == 0 { "build" } else { "docs/sub" };
                IndexRecord::new(
                    format!("{parent}/report{i:04}.txt").into(),
                    false,
                    i,
                    1_700_000_000 + i as i64,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        for (offset, record) in records.iter_mut().enumerate() {
            record.availability = foldex_fs::capabilities::Availability::from_code(
                ((first as usize + offset) % 5) as u8,
            )
            .unwrap();
        }
        for record in &records {
            let mut entry = Entry::from_name(
                record.raw_name.clone(),
                false,
                record.size,
                record.modified_secs,
            );
            entry.availability = record.availability;
            entry.relative_path = Some(record.relative_path.clone());
            entries.push(entry);
        }
        native.ingest_batch(&records, &cancel).unwrap();
        legacy.ingest_batch(&records, &cancel).unwrap();
    }
    let mut native = native.finish(true).unwrap();
    let mut legacy = legacy.finish(true).unwrap();
    let snapshot = Arc::new(Snapshot::from_entries(fixture.root(), entries));
    for mode in [
        foldex_core::MatchMode::Substring,
        foldex_core::MatchMode::Fuzzy,
    ] {
        for sort in [
            foldex_core::SortColumn::Name,
            foldex_core::SortColumn::Size,
            foldex_core::SortColumn::Modified,
        ] {
            let query = Query {
                text: "report".into(),
                match_mode: mode,
                sort,
                conditions: foldex_query::Conditions {
                    path_contains: "DOCS".into(),
                    min_bytes: Some(100),
                    max_bytes: Some(3900),
                    modified_after: Some(1_700_000_200),
                    excluded_subtrees: vec!["build".into()],
                    ..Default::default()
                },
                ..Default::default()
            };
            let view = foldex_core::filter_snapshot(snapshot.clone(), &query);
            let expected = view
                .indices
                .iter()
                .map(|i| snapshot.entries[*i as usize].relative_path.clone().unwrap())
                .collect::<Vec<_>>();
            assert!(expected.len() > 1500);
            let spec = query_spec(&query, true);
            for offset in [0, 900, 1500] {
                let a = native.query(&spec, offset, 100, &cancel).unwrap();
                let b = legacy.query(&spec, offset, 100, &cancel).unwrap();
                assert_eq!(a.total, Some(expected.len() as u64));
                assert_eq!(b.total, a.total);
                for row in a.rows.iter().chain(b.rows.iter()) {
                    let number = row
                        .name
                        .trim_start_matches("report")
                        .trim_end_matches(".txt")
                        .parse::<usize>()
                        .unwrap();
                    assert_eq!(
                        row.availability,
                        foldex_fs::capabilities::Availability::from_code((number % 5) as u8)
                            .unwrap()
                    );
                }
                let end = expected.len().min(offset as usize + a.rows.len());
                assert_eq!(
                    a.rows
                        .iter()
                        .map(|r| r.relative_path.clone())
                        .collect::<Vec<_>>(),
                    expected[offset as usize..end]
                );
                assert_eq!(
                    b.rows
                        .iter()
                        .map(|r| r.relative_path.clone())
                        .collect::<Vec<_>>(),
                    expected[offset as usize..offset as usize + b.rows.len()]
                );
                if let Some(cursor) = a.next {
                    let page = native
                        .query_after(&spec, Some(&cursor), 50, &cancel)
                        .unwrap();
                    let start = cursor.next_offset() as usize;
                    assert_eq!(
                        page.rows
                            .iter()
                            .map(|r| r.relative_path.clone())
                            .collect::<Vec<_>>(),
                        expected[start..start + page.rows.len()]
                    );
                    let mut other = spec.clone();
                    other.conditions.min_bytes = Some(3000);
                    assert!(
                        native
                            .query_after(&other, Some(&cursor), 50, &cancel)
                            .is_err()
                    );
                }
            }
        }
    }
    let spec = QuerySpec {
        conditions: foldex_query::Conditions {
            path_contains: "report1001".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(native.query(&spec, 0, 100, &cancel).unwrap().total, Some(1));
    assert_eq!(legacy.query(&spec, 0, 100, &cancel).unwrap().total, Some(1));
}

#[test]
fn scoped_exclusions_prune_traversal_and_use_separate_live_generations() {
    let fixture = live_tests::Fixture::new();
    std::fs::create_dir_all(fixture.root().join("build/deep")).unwrap();
    std::fs::write(fixture.root().join("report.txt"), b"a").unwrap();
    std::fs::write(fixture.root().join("build/deep/report.txt"), b"b").unwrap();
    let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
    let all = hub.client();
    let pruned = hub.client();
    let query = Query {
        text: "report".into(),
        ..Default::default()
    };
    let one = all.request(fixture.root(), query.clone(), 0, false);
    let mut excluded = query.clone();
    excluded.conditions.excluded_subtrees = vec!["build".into()];
    let two = pruned.request(fixture.root(), excluded, 0, false);
    fn finished(client: &SearchClient, id: u64) -> Arc<SearchWindow> {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(SearchEvent::Window(window)) = client.try_recv() {
                if window.request == id && window.status.complete && !window.status.from_cache {
                    return window;
                }
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(2));
        }
    }
    let one = finished(&all, one);
    let two = finished(&pruned, two);
    assert_eq!(one.total, Some(2));
    assert_eq!(two.total, Some(1));
    assert_ne!(one.status.database_path, two.status.database_path);
    assert_eq!(two.status.entries, 2); // report.txt and the fixture's unrelated child folder; no build rows.
    assert_eq!(two.status.excluded_cache, 1);
    let three = pruned.request(fixture.root(), query, 0, false);
    assert_eq!(
        finished(&pruned, three).status.generation,
        one.status.generation
    );
    hub.shutdown();
}
