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
