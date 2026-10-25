#![cfg(feature = "catalog-experiment")]
use foldex_index::{
    BuildOptions, Cancellation, IndexRecord, QuerySpec, SortColumn, catalog::NativeIndexBuilder,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "foldex-continuation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("scope")).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn directory_continuation_enumerates_direct_children_and_retains_dates() {
    let fixture = Fixture::new();
    let scope = fixture.0.join("scope");
    fs::create_dir_all(scope.join("child/deep")).unwrap();
    fs::write(scope.join("root.txt"), b"root").unwrap();
    fs::write(scope.join("child/deep/hidden.txt"), b"nested").unwrap();
    let cancel = Cancellation::new();
    let mut builder = NativeIndexBuilder::create_with_options(
        &fixture.0.join("cache"),
        &scope,
        &BuildOptions {
            excluded_subtrees: Vec::new(),
            directory_only: true,
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    while !builder.scan_step(128, &cancel, &|| {}).unwrap().done {}
    let mut index = builder.finish(true).unwrap();
    assert_eq!(index.status().entries, 2);
    let page = index.query(&QuerySpec::default(), 0, 256, &cancel).unwrap();
    assert!(
        page.rows
            .iter()
            .all(|row| row.relative_path.components().count() == 1)
    );
    let file = page.rows.iter().find(|row| row.name == "root.txt").unwrap();
    assert_eq!(file.size, 4);
    assert!(file.accessed_secs.is_some());
    #[cfg(windows)]
    assert!(file.created_secs.is_some());
}
#[test]
fn creation_and_access_date_order_agree_across_head_cursor_and_deep_seek() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder =
        NativeIndexBuilder::create(&fixture.0.join("cache"), &fixture.0.join("scope"), &cancel)
            .unwrap();
    for first in (0..1500).step_by(256) {
        let records = (first..(first + 256).min(1500))
            .map(|i| {
                let mut row =
                    IndexRecord::new(format!("file{i:04}.txt").into(), false, i as u64, 100)
                        .unwrap();
                row.created_secs = (i % 7 != 0).then_some(i as i64);
                row.accessed_secs = (i % 11 != 0).then_some(1500 - i as i64);
                row
            })
            .collect::<Vec<_>>();
        builder.ingest_batch(&records, &cancel).unwrap();
    }
    let mut index = builder.finish(true).unwrap();
    for sort in [SortColumn::Created, SortColumn::Accessed] {
        for descending in [false, true] {
            let spec = QuerySpec {
                sort,
                descending,
                ..Default::default()
            };
            let mut page = index.query(&spec, 0, 128, &cancel).unwrap();
            let mut rows = Vec::new();
            loop {
                assert!(
                    !page.has_more || page.next.is_some(),
                    "nonfinal pages need a cursor"
                );
                assert!(rows.len() < 1600, "cursor must advance monotonically");
                rows.extend(page.rows);
                if !page.has_more {
                    break;
                }
                page = index
                    .query_after(&spec, page.next.as_ref(), 128, &cancel)
                    .unwrap();
            }
            assert_eq!(rows.len(), 1500);
            assert_eq!(
                rows.iter()
                    .map(|row| row.id)
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                1500
            );
            let dates = rows
                .iter()
                .map(|row| {
                    if sort == SortColumn::Created {
                        row.created_secs
                    } else {
                        row.accessed_secs
                    }
                })
                .collect::<Vec<_>>();
            let known = dates.partition_point(Option::is_some);
            assert!(dates[known..].iter().all(Option::is_none));
            assert!(dates[..known].windows(2).all(|pair| if descending {
                pair[0] >= pair[1]
            } else {
                pair[0] <= pair[1]
            }));
            let deep = index.query(&spec, 1040, 128, &cancel).unwrap();
            assert_eq!(
                deep.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
                rows[1040..1168]
                    .iter()
                    .map(|row| row.id)
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn different_subscriber_queries_reuse_independent_heads_with_a_combined_budget() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder =
        NativeIndexBuilder::create(&fixture.0.join("cache"), &fixture.0.join("scope"), &cancel)
            .unwrap();
    let rows = (0..256)
        .map(|i| {
            IndexRecord::new(format!("report{i:04}.txt").into(), false, i as u64, 100).unwrap()
        })
        .collect::<Vec<_>>();
    builder.ingest_batch(&rows, &cancel).unwrap();
    let mut index = builder.finish(true).unwrap();
    let first = QuerySpec {
        text: "report1".into(),
        ..Default::default()
    };
    let second = QuerySpec {
        text: "report2".into(),
        ..Default::default()
    };
    let expected = index
        .query(&first, 0, 32, &cancel)
        .unwrap()
        .rows
        .iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    assert!(!index.last_query_timing().head_reused);
    index.query(&second, 0, 32, &cancel).unwrap();
    let returned = index.query(&first, 0, 32, &cancel).unwrap();
    assert!(index.last_query_timing().head_reused);
    assert_eq!(
        returned.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        expected
    );
    for query in 0..20 {
        index
            .query(
                &QuerySpec {
                    text: format!("report{query}"),
                    ..Default::default()
                },
                0,
                32,
                &cancel,
            )
            .unwrap();
        let metrics = index.metrics().unwrap();
        assert!(metrics.rank_cache_used <= metrics.rank_cache_budget);
    }
    index.release_query_cache();
    assert_eq!(index.retained_query_bytes(), 0);
}

fn filesystem_index(fixture: &Fixture, directory_only: bool) -> foldex_index::catalog::NativeIndex {
    let cancel = Cancellation::new();
    let mut builder = NativeIndexBuilder::create_with_options(
        &fixture.0.join("cache"),
        &fixture.0.join("scope"),
        &BuildOptions {
            directory_only,
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !builder.scan_step(256, &cancel, &|| {}).unwrap().done {
        assert!(std::time::Instant::now() < until);
    }
    builder.finish(true).unwrap()
}
#[test]
fn immutable_file_reconciliation_reads_one_changed_entry_and_preserves_old_rows() {
    let fixture = Fixture::new();
    let scope = fixture.0.join("scope");
    fs::write(scope.join("report.txt"), b"a").unwrap();
    fs::write(scope.join("kept.txt"), b"b").unwrap();
    let cancel = Cancellation::new();
    let mut old = filesystem_index(&fixture, false);
    let original = old.query(&QuerySpec::default(), 0, 256, &cancel).unwrap();
    let before = fs::metadata(scope.join("report.txt"))
        .unwrap()
        .modified()
        .unwrap();
    fs::write(scope.join("report.txt"), b"updated").unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(scope.join("report.txt"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(before))
        .unwrap();
    let updated = foldex_index::catalog::NativeIndex::reconcile(
        &old.status().database_path,
        &["report.txt".into()],
        &BuildOptions::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(updated.metrics.metadata_reads, 1);
    assert_eq!(updated.metrics.enumerated_entries, 0);
    assert_eq!(updated.metrics.retired_rows, 1);
    assert!(updated.metrics.reused_shards > 0 || updated.metrics.copied_bytes > 0);
    let mut new = updated.index;
    assert_ne!(new.status().generation, old.status().generation);
    let rows = new.query(&QuerySpec::default(), 0, 256, &cancel).unwrap();
    assert_eq!(new.status().entries, 3);
    assert_eq!(new.status().files, 2);
    assert_eq!(
        rows.rows
            .iter()
            .find(|row| row.name == "report.txt")
            .unwrap()
            .size,
        7
    );
    let unchanged = old.query(&QuerySpec::default(), 0, 256, &cancel).unwrap();
    assert_eq!(
        unchanged
            .rows
            .iter()
            .find(|row| row.name == "report.txt")
            .unwrap()
            .size,
        1
    );
    assert_eq!(
        unchanged.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
        original.rows.iter().map(|row| row.id).collect::<Vec<_>>()
    );
    let reopened = foldex_index::catalog::NativeIndex::open(&fixture.0.join("cache")).unwrap();
    assert_eq!(reopened.status().generation, new.status().generation);
}
#[test]
fn subtree_rename_and_removal_reconcile_descendants_and_parent_metadata() {
    let fixture = Fixture::new();
    let scope = fixture.0.join("scope");
    fs::create_dir_all(scope.join("old/sub")).unwrap();
    fs::write(scope.join("old/sub/deep.txt"), b"a").unwrap();
    fs::write(scope.join("kept.txt"), b"b").unwrap();
    let cancel = Cancellation::new();
    let old = filesystem_index(&fixture, false);
    fs::rename(scope.join("old"), scope.join("new")).unwrap();
    fs::remove_file(scope.join("kept.txt")).unwrap();
    fs::write(scope.join("new/sub/added.txt"), b"cc").unwrap();
    let updated = foldex_index::catalog::NativeIndex::reconcile(
        &old.status().database_path,
        &[
            "old".into(),
            "new".into(),
            "new/sub/added.txt".into(),
            "kept.txt".into(),
        ],
        &BuildOptions::default(),
        &cancel,
    )
    .unwrap();
    assert_eq!(updated.metrics.metadata_reads, 3);
    assert_eq!(updated.metrics.enumerated_entries, 3);
    let mut renamed = updated.index;
    let page = renamed
        .query(&QuerySpec::default(), 0, 256, &cancel)
        .unwrap();
    assert_eq!(page.rows.len(), 4);
    assert!(
        page.rows
            .iter()
            .all(|row| row.relative_path.starts_with("new"))
    );
    fs::remove_dir_all(scope.join("new/sub")).unwrap();
    let mut removed = foldex_index::catalog::NativeIndex::reconcile(
        &renamed.status().database_path,
        &["new/sub".into()],
        &BuildOptions::default(),
        &cancel,
    )
    .unwrap()
    .index;
    let page = removed
        .query(&QuerySpec::default(), 0, 256, &cancel)
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].relative_path, PathBuf::from("new"));
    assert_eq!(removed.status().files, 0);
    assert_eq!(removed.status().folders, 1);
}
#[test]
fn shallow_reconciliation_rejects_a_different_traversal_policy_and_never_descends() {
    let fixture = Fixture::new();
    let scope = fixture.0.join("scope");
    fs::create_dir_all(scope.join("child")).unwrap();
    fs::write(scope.join("child/deep.txt"), b"a").unwrap();
    let cancel = Cancellation::new();
    let old = filesystem_index(&fixture, true);
    assert!(old.status().directory_only);
    assert!(matches!(
        foldex_index::catalog::NativeIndex::reconcile(
            &old.status().database_path,
            &["child".into()],
            &BuildOptions::default(),
            &cancel
        ),
        Err(foldex_index::Error::WrongMode)
    ));
    let result = foldex_index::catalog::NativeIndex::reconcile(
        &old.status().database_path,
        &["child".into()],
        &BuildOptions {
            excluded_subtrees: Vec::new(),
            directory_only: true,
            ..Default::default()
        },
        &cancel,
    )
    .unwrap();
    assert_eq!(result.metrics.enumerated_entries, 0);
    assert!(result.index.status().directory_only);
    assert_eq!(result.index.status().files, 0);
    assert_eq!(result.index.status().folders, 1);
    cancel.cancel();
    assert!(matches!(
        foldex_index::catalog::NativeIndex::reconcile(
            &old.status().database_path,
            &["child".into()],
            &BuildOptions {
                excluded_subtrees: Vec::new(),
                directory_only: true,
                ..Default::default()
            },
            &cancel
        ),
        Err(foldex_index::Error::Cancelled)
    ));
}

#[test]
fn sequential_continuation_is_cached_within_the_existing_head_budget() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder =
        NativeIndexBuilder::create(&fixture.0.join("cache"), &fixture.0.join("scope"), &cancel)
            .unwrap();
    for first in (0..5000).step_by(256) {
        let records = (first..(first + 256).min(5000))
            .map(|i| IndexRecord::new(format!("report{i:05}.txt").into(), false, i, 100).unwrap())
            .collect::<Vec<_>>();
        builder.ingest_batch(&records, &cancel).unwrap();
    }
    let mut index = builder.finish(true).unwrap();
    let spec = QuerySpec::default();
    let mut page = index.query(&spec, 0, 256, &cancel).unwrap();
    while page.next_offset < 1025 {
        page = index
            .query_after(&spec, page.next.as_ref(), 256, &cancel)
            .unwrap();
    }
    let page = index
        .query_after(&spec, page.next.as_ref(), 256, &cancel)
        .unwrap();
    assert_eq!(page.rows[0].name, "report01025.txt");
    assert!(index.cached_window(&spec, 5000, 1281, 768));
    let next = index.query(&spec, 1281, 256, &cancel).unwrap();
    assert_eq!(next.rows[0].name, "report01281.txt");
    assert_eq!(next.rows.len(), 256);
    let metrics = index.metrics().unwrap();
    assert!(metrics.rank_cache_used <= metrics.rank_cache_budget);
    index.release_query_cache();
    assert!(!index.cached_window(&spec, 5000, 1281, 768));
    let resumed = index
        .query_after(&spec, next.next.as_ref(), 256, &cancel)
        .unwrap();
    assert_eq!(resumed.rows[0].name, "report01537.txt");
}
