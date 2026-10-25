use foldex_index::*;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
static NEXT: AtomicU64 = AtomicU64::new(1);
static BASE: OnceLock<PathBuf> = OnceLock::new();
struct Fixture {
    path: PathBuf,
    base: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let base = BASE
            .get_or_init(|| {
                let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-fixtures");
                fs::create_dir_all(&base).unwrap();
                base
            })
            .clone();
        let path = base.join(format!(
            "owned-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("scope")).unwrap();
        Self { path, base }
    }
    fn scope(&self) -> PathBuf {
        self.path.join("scope")
    }
    fn cache(&self) -> PathBuf {
        self.path.join("cache")
    }
    fn builder(&self) -> IndexBuilder {
        IndexBuilder::create(&self.cache(), &self.scope(), &Cancellation::new()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let (Ok(path), Ok(base)) = (self.path.canonicalize(), self.base.canonicalize()) {
            if path.starts_with(&base) && path != base {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
}
fn record(name: &str, is_dir: bool, size: u64) -> IndexRecord {
    IndexRecord::new(PathBuf::from(name), is_dir, size, size as i64).unwrap()
}

#[test]
fn kind_visibility_and_exact_counts_agree_between_legacy_and_native_search() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let records = vec![
        record("report-folder", true, 0),
        record("report.txt", false, 10),
        record("other.txt", false, 20),
    ];
    let mut legacy = fixture.builder();
    #[cfg(feature = "catalog-experiment")]
    let mut native = catalog::NativeIndexBuilder::create(
        &fixture.cache().join("native"),
        &fixture.scope(),
        &cancel,
    )
    .unwrap();
    for record in &records {
        legacy.ingest(record, &cancel).unwrap();
    }
    #[cfg(feature = "catalog-experiment")]
    native.ingest_batch(&records, &cancel).unwrap();
    let mut legacy = legacy.finish(true).unwrap();
    #[cfg(feature = "catalog-experiment")]
    let mut native = native.finish(true).unwrap();
    for (kind, expected) in [
        (KindFilter::All, (1, 1)),
        (KindFilter::Files, (0, 1)),
        (KindFilter::Folders, (1, 0)),
        (KindFilter::None, (0, 0)),
    ] {
        let spec = QuerySpec {
            text: "report".into(),
            kind,
            ..Default::default()
        };
        let check_page = |page: QueryPage| {
            assert_eq!(page.kind_counts, Some(expected));
            assert_eq!(page.total, Some(expected.0 + expected.1));
            assert_eq!(page.rows.len() as u64, expected.0 + expected.1);
            assert!(!page.has_more);
        };
        check_page(legacy.query(&spec, 0, 256, &cancel).unwrap());
        #[cfg(feature = "catalog-experiment")]
        check_page(native.query(&spec, 0, 256, &cancel).unwrap());
    }
}

#[test]
fn multi_type_masks_match_legacy_and_native_counts_and_pages() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let records = vec![
        record("folder", true, 0),
        record("image.PNG", false, 10),
        record("notes.txt", false, 20),
        record("future.new", false, 30),
        record("Readme", false, 40),
    ];
    let mut legacy = fixture.builder();
    #[cfg(feature = "catalog-experiment")]
    let mut native = catalog::NativeIndexBuilder::create(
        &fixture.cache().join("native-types"),
        &fixture.scope(),
        &cancel,
    )
    .unwrap();
    for entry in &records {
        legacy.ingest(entry, &cancel).unwrap();
    }
    #[cfg(feature = "catalog-experiment")]
    native.ingest_batch(&records, &cancel).unwrap();
    let mut legacy = legacy.finish(true).unwrap();
    #[cfg(feature = "catalog-experiment")]
    let mut native = native.finish(true).unwrap();
    for (filter, expected, names) in [
        (
            TypeFilter {
                include_folders: false,
                exclude: false,
                extensions: vec!["png".into(), "txt".into()],
            },
            2,
            vec!["image.PNG", "notes.txt"],
        ),
        (
            TypeFilter {
                include_folders: true,
                exclude: true,
                extensions: vec!["png".into(), "txt".into()],
            },
            3,
            vec!["folder", "future.new", "Readme"],
        ),
        (TypeFilter::default(), 0, vec![]),
        (
            TypeFilter {
                include_folders: false,
                exclude: false,
                extensions: vec![String::new()],
            },
            1,
            vec!["Readme"],
        ),
    ] {
        let spec = QuerySpec {
            show_hidden: true,
            type_filter: Some(filter),
            ..Default::default()
        };
        let left = legacy.query(&spec, 0, 256, &cancel).unwrap();
        assert_eq!(left.total, Some(expected));
        assert_eq!(
            left.rows
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            names
        );
        let mut excluded = spec.clone();
        excluded.kind = KindFilter::None;
        assert!(
            legacy
                .query(&excluded, 0, 256, &cancel)
                .unwrap()
                .rows
                .is_empty()
        );
        #[cfg(feature = "catalog-experiment")]
        {
            let right = native.query(&spec, 0, 256, &cancel).unwrap();
            assert_eq!(right.total, left.total);
            assert_eq!(left.kind_counts, right.kind_counts);
            assert_eq!(
                left.rows.iter().map(|row| &row.name).collect::<Vec<_>>(),
                right.rows.iter().map(|row| &row.name).collect::<Vec<_>>()
            );
            assert!(
                native
                    .query(&excluded, 0, 256, &cancel)
                    .unwrap()
                    .rows
                    .is_empty()
            );
        }
    }
}

fn check_type_filter_cache_isolation(
    mut query: impl FnMut(&QuerySpec, Option<&PageCursor>, usize) -> Result<QueryPage>,
) {
    let filters = [
        (
            None,
            vec![
                "Report_folder",
                "Report1.txt",
                "Report2.txt",
                "Report3.rs",
                "Report4.md",
                "Report5",
            ],
        ),
        (
            Some(TypeFilter {
                extensions: vec!["txt".into()],
                ..Default::default()
            }),
            vec!["Report1.txt", "Report2.txt"],
        ),
        (
            Some(TypeFilter {
                include_folders: true,
                extensions: vec!["txt".into()],
                ..Default::default()
            }),
            vec!["Report_folder", "Report1.txt", "Report2.txt"],
        ),
        (
            Some(TypeFilter {
                extensions: vec!["rs".into(), "md".into()],
                ..Default::default()
            }),
            vec!["Report3.rs", "Report4.md"],
        ),
        (
            Some(TypeFilter {
                exclude: true,
                extensions: vec!["txt".into()],
                ..Default::default()
            }),
            vec!["Report3.rs", "Report4.md", "Report5"],
        ),
        (Some(TypeFilter::default()), vec![]),
        (
            Some(TypeFilter {
                include_folders: true,
                ..Default::default()
            }),
            vec!["Report_folder"],
        ),
        (
            None,
            vec![
                "Report_folder",
                "Report1.txt",
                "Report2.txt",
                "Report3.rs",
                "Report4.md",
                "Report5",
            ],
        ),
    ];
    for (mode, text) in [(MatchMode::Fuzzy, "rpt"), (MatchMode::Substring, "report")] {
        let base = QuerySpec {
            text: text.into(),
            match_mode: mode,
            show_hidden: true,
            ..Default::default()
        };
        let first = query(&base, None, 1).unwrap();
        let old_cursor = first.next.unwrap();
        for (filter, names) in &filters {
            let spec = QuerySpec {
                type_filter: filter.clone(),
                ..base.clone()
            };
            if filter.is_some() {
                assert!(matches!(
                    query(&spec, Some(&old_cursor), 2),
                    Err(Error::InvalidCursor)
                ));
            }
            let mut actual = Vec::new();
            let mut cursor = None;
            loop {
                let page = query(&spec, cursor.as_ref(), 2).unwrap();
                assert_eq!(page.total, Some(names.len() as u64), "{spec:?}");
                let folders = u64::from(names.contains(&"Report_folder"));
                assert_eq!(
                    page.kind_counts,
                    Some((folders, names.len() as u64 - folders))
                );
                actual.extend(page.rows.into_iter().map(|row| row.name));
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
                assert!(actual.len() < names.len());
            }
            actual.sort();
            let mut expected = names.clone();
            expected.sort();
            assert_eq!(actual, expected, "{spec:?}");
            assert_eq!(
                query(&spec, None, 0).unwrap().total,
                Some(names.len() as u64)
            );
        }
    }
}

#[test]
fn type_filtered_fuzzy_and_substring_pages_isolate_caches_and_cursors() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut records: Vec<_> = [
        "Report1.txt",
        "Report2.txt",
        "Report3.rs",
        "Report4.md",
        "Report5",
        "Other.txt",
    ]
    .iter()
    .map(|name| record(name, false, 1))
    .collect();
    records.push(record("Report_folder", true, 0));
    let mut legacy = fixture.builder();
    legacy.ingest_batch(&records, &cancel).unwrap();
    let mut legacy = legacy.finish(true).unwrap();
    check_type_filter_cache_isolation(|spec, cursor, size| {
        legacy.query_after(spec, cursor, size, &cancel)
    });
    #[cfg(feature = "catalog-experiment")]
    {
        let mut native = catalog::NativeIndexBuilder::create(
            &fixture.cache().join("native-filter-cache"),
            &fixture.scope(),
            &cancel,
        )
        .unwrap();
        native.ingest_batch(&records, &cancel).unwrap();
        let mut native = native.finish(true).unwrap();
        check_type_filter_cache_isolation(|spec, cursor, size| {
            native.query_after(spec, cursor, size, &cancel)
        });
    }
}

#[cfg(feature = "diagnostics")]
#[test]
fn candidate_planning_reuses_probes_and_replans_after_publication() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder = fixture.builder();
    builder
        .ingest_batch(
            &[
                record("report1.txt", false, 1),
                record("report2.txt", false, 2),
                record("report3.txt", false, 3),
            ],
            &cancel,
        )
        .unwrap();
    let boundary = builder.checkpoint().unwrap().status;
    let mut index = Index::open_database(&boundary.database_path).unwrap();
    let spec = QuerySpec {
        text: "report".into(),
        match_mode: MatchMode::Substring,
        show_hidden: true,
        include_total: false,
        ..Default::default()
    };
    let first = index.query(&spec, 0, 1, &cancel).unwrap();
    let initial = index.last_query_diagnostics();
    assert!(initial.candidate_probes > 0);
    assert_eq!(initial.candidate_cache_hits, 0);
    let page = index
        .query_after(&spec, first.next.as_ref(), 1, &cancel)
        .unwrap();
    assert_eq!(page.rows[0].name, "report2.txt");
    let cached = index.last_query_diagnostics();
    assert_eq!(cached.candidate_probes, 0);
    assert_eq!(cached.candidate_cache_hits, 1);
    let counted = QuerySpec {
        include_total: true,
        ..spec.clone()
    };
    assert_eq!(index.query(&counted, 0, 0, &cancel).unwrap().total, Some(3));
    assert_eq!(index.last_query_diagnostics().candidate_probes, 0);
    builder
        .ingest(&record("report4.txt", false, 4), &cancel)
        .unwrap();
    builder.checkpoint().unwrap();
    assert_eq!(index.query(&counted, 0, 0, &cancel).unwrap().total, Some(4));
    assert!(index.last_query_diagnostics().candidate_probes > 0);
    // An older pinned result still uses its original complete prefix.
    assert_eq!(
        index
            .query_as_of(&counted, &boundary, 0, 0, &cancel)
            .unwrap()
            .total,
        Some(3)
    );
    index.benchmark_set_query_cache_enabled(false);
    index.query(&spec, 0, 1, &cancel).unwrap();
    index.query(&spec, 0, 1, &cancel).unwrap();
    assert!(index.last_query_diagnostics().candidate_probes > 0);
    assert_eq!(index.last_query_diagnostics().candidate_cache_hits, 0);
}

#[cfg(feature = "catalog-experiment")]
#[test]
fn native_sparse_row_cache_misses_match_ordered_decoder_across_queries() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder =
        catalog::NativeIndexBuilder::create(&fixture.cache(), &fixture.scope(), &cancel).unwrap();
    for start in [1, 2, 3] {
        let records = [start, start + 3, start + 6]
            .into_iter()
            .map(|at| {
                record(
                    &format!("Report{at}.{}", ["txt", "cfg", "log"][(at - 1) % 3]),
                    false,
                    at as u64,
                )
            })
            .collect::<Vec<_>>();
        builder.ingest_batch(&records, &cancel).unwrap();
        builder.checkpoint().unwrap();
    }
    let mut index = builder.finish(true).unwrap();
    let mut oracle = catalog::NativeIndex::open_database(&index.status().database_path).unwrap();
    oracle.benchmark_set_grouped_payload(false);
    let filtered = QuerySpec {
        text: "rpt".into(),
        type_filter: Some(TypeFilter {
            extensions: vec!["txt".into()],
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(index.query(&filtered, 0, 9, &cancel).unwrap().rows.len(), 3);
    for spec in [
        QuerySpec {
            type_filter: None,
            ..filtered
        },
        QuerySpec {
            text: "r".into(),
            descending: true,
            ..Default::default()
        },
    ] {
        let expected = oracle.query(&spec, 0, 9, &cancel).unwrap();
        let actual = index.query(&spec, 0, 9, &cancel).unwrap();
        assert_eq!(
            actual
                .rows
                .iter()
                .map(|row| (row.id, &row.path))
                .collect::<Vec<_>>(),
            expected
                .rows
                .iter()
                .map(|row| (row.id, &row.path))
                .collect::<Vec<_>>()
        );
        assert_eq!(actual.total, expected.total);
        assert!(index.last_query_timing().cache_hits >= 3);
        assert_eq!(actual.rows.len(), 9);
    }
    assert_eq!(index.last_query_timing().cache_misses, 0);
}

#[cfg(feature = "catalog-experiment")]
#[test]
fn catalog_incremental_ranges_cross_shard_boundaries_without_omissions() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut data = catalog::Catalog::create(&fixture.path.join("range-data")).unwrap();
    for start in [1, 4, 7, 10, 13] {
        let records = (start..start + 3)
            .map(|at| record(&format!("item{at:02}.txt"), false, at))
            .collect::<Vec<_>>();
        data.append_batch(&records, &cancel).unwrap();
        data.checkpoint().unwrap();
    }
    assert_eq!(data.shards(), 5);
    for (lower, upper) in [
        (0, 0),
        (0, 1),
        (0, 15),
        (2, 4),
        (3, 6),
        (4, 4),
        (6, 13),
        (11, 15),
        (15, 15),
    ] {
        let page = data
            .search(
                &catalog::CatalogQuery {
                    lower_id: lower,
                    upper_id: Some(upper),
                    workers: 8,
                    ..Default::default()
                },
                &cancel,
            )
            .unwrap();
        assert_eq!(page.files, upper - lower);
        assert_eq!(
            page.hits.iter().map(|hit| hit.id).collect::<Vec<_>>(),
            (lower + 1..=upper).collect::<Vec<_>>()
        );
    }
}

#[test]
fn fuzzy_default_keeps_gaps_order_repetition_and_unicode() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    for name in [
        "Design",
        "Downloads",
        "Documents",
        "Readme",
        "東京の本.txt",
        "bookkeeper.txt",
    ] {
        b.ingest(&record(name, false, 0), &c).unwrap();
    }
    let mut index = b.finish(true).unwrap();
    for (text, expected) in [
        ("dsg", vec!["Design"]),
        ("dgs", vec![]),
        ("東本", vec!["東京の本.txt"]),
        ("bkkp", vec!["bookkeeper.txt"]),
        ("ooo", vec![]),
    ] {
        let page = index
            .query(
                &QuerySpec {
                    text: text.into(),
                    ..QuerySpec::default()
                },
                0,
                256,
                &c,
            )
            .unwrap();
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(page.used_character_index && !page.used_trigram);
    }
    let exact = index
        .query(
            &QuerySpec {
                text: "dsg".into(),
                match_mode: MatchMode::Substring,
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert_eq!(exact.total, Some(0));
}

#[test]
fn fuzzy_source_folding_and_score_pages_match_shared_matcher() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let names = [
        "Design",
        "Downloads",
        "Documents",
        "Readme",
        "dsg",
        "de_sign",
        "Ω.txt",
        "ω.txt",
        "İ.txt",
        "i.txt",
    ];
    let mut b = f.builder();
    for name in names {
        b.ingest(&record(name, false, 0), &c).unwrap();
    }
    let mut index = b.finish(true).unwrap();
    for (text, expected) in [
        ("Ω", "Ω.txt"),
        ("ω", "ω.txt"),
        ("ı", "İ.txt"),
        ("i", "i.txt"),
    ] {
        let page = index
            .query(
                &QuerySpec {
                    text: text.into(),
                    ..QuerySpec::default()
                },
                0,
                256,
                &c,
            )
            .unwrap();
        if text != "i" {
            assert_eq!(page.total, Some(1));
        }
        assert!(page.rows.iter().any(|row| row.name == expected));
        assert!(!page.rows.iter().any(|row| row.name
            == if expected == "Ω.txt" {
                "ω.txt"
            } else if expected == "ω.txt" {
                "Ω.txt"
            } else if expected == "İ.txt" {
                "i.txt"
            } else {
                "İ.txt"
            }));
    }
    let matcher = foldex_match::Matcher::new("d");
    let mut expected: Vec<_> = names
        .iter()
        .filter_map(|name| matcher.score(name).map(|score| (*name, score)))
        .collect();
    expected.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
            .then_with(|| a.0.cmp(b.0))
    });
    let spec = QuerySpec {
        text: "d".into(),
        ..QuerySpec::default()
    };
    let mut cursor = None;
    let mut actual = Vec::new();
    loop {
        let page = index.query_after(&spec, cursor.as_ref(), 2, &c).unwrap();
        actual.extend(page.rows.iter().map(|row| row.name.clone()));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        actual,
        expected
            .iter()
            .map(|(name, _)| name.to_string())
            .collect::<Vec<_>>()
    );
}

#[test]
fn owned_cleanup_retains_pointer_targets_and_rejects_unrelated_files() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut first = f.builder();
    first.ingest(&record("old.txt", false, 1), &c).unwrap();
    let first = first.finish(true).unwrap();
    let old_path = first.status().database_path.clone();
    drop(first);
    let mut second = f.builder();
    second.ingest(&record("current.txt", false, 1), &c).unwrap();
    let second = second.finish(true).unwrap();
    let current_path = second.status().database_path.clone();
    drop(second);
    let building = f.builder();
    let building_path = building.status().database_path.clone();
    drop(building);
    let fake = f.cache().join("index-unrelated.sqlite3");
    fs::write(&fake, b"private user data").unwrap();
    let report = Index::cleanup_owned(&f.cache(), &f.scope()).unwrap();
    assert_eq!(report.removed_generations, 1);
    assert_eq!(report.retained_unverified, 1);
    assert!(!old_path.exists());
    assert!(current_path.exists() && building_path.exists() && fake.exists());
    assert_eq!(fs::read(fake).unwrap(), b"private user data");
}

#[test]
fn mixed_name_sql_ranking_matches_the_real_matcher_across_memo_classes() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    let prefixes = [
        "DesignReview",
        "projectReport",
        "README",
        "runtime_cache",
        "東京_写真",
        "Данные_Файл",
        "a_b_a_b",
        "invoice FINAL",
        "Q3Summary",
        "Ωω_İABC",
    ];
    let names: Vec<_> = (0..2000)
        .map(|i| {
            (
                format!("{}_{i:06}.txt", prefixes[i % prefixes.len()]),
                i % 17 == 0,
            )
        })
        .collect();
    for chunk in names.chunks(256) {
        b.ingest_batch(
            &chunk
                .iter()
                .map(|(name, directory)| record(name, *directory, 0))
                .collect::<Vec<_>>(),
            &c,
        )
        .unwrap();
    }
    let mut index = b.finish(true).unwrap();
    for text in [
        "rpt", "dsg", "rm", "ab", "東写", "дф", "Ω", "ı", "3s", "inF",
    ] {
        let matcher = foldex_match::Matcher::new(text);
        let mut expected: Vec<_> = names
            .iter()
            .filter_map(|(name, directory)| {
                matcher
                    .score(name)
                    .map(|score| (name.as_str(), *directory, score))
            })
            .collect();
        expected.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
                .then_with(|| a.0.cmp(b.0))
        });
        let page = index
            .query(
                &QuerySpec {
                    text: text.into(),
                    ..QuerySpec::default()
                },
                0,
                256,
                &c,
            )
            .unwrap();
        assert_eq!(page.total, Some(expected.len() as u64), "query {text}");
        assert_eq!(
            page.rows
                .iter()
                .map(|row| row.name.as_str())
                .collect::<Vec<_>>(),
            expected
                .iter()
                .take(256)
                .map(|value| value.0)
                .collect::<Vec<_>>(),
            "query {text}"
        );
    }
}

#[test]
fn cleanup_reclaims_owned_obsolete_normalization_without_opening_it_for_search() {
    let f = Fixture::new();
    let old = f.builder().finish(true).unwrap();
    let obsolete = old.status().database_path.clone();
    drop(old);
    let connection = rusqlite::Connection::open(&obsolete).unwrap();
    connection
        .execute(
            "UPDATE manifest SET normalization='previous-owned-normalization'",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(Index::open_scope(&f.cache(), &f.scope()).is_err());
    drop(f.builder().finish(true).unwrap());
    assert_eq!(
        Index::cleanup_owned(&f.cache(), &f.scope())
            .unwrap()
            .removed_generations,
        1
    );
    assert!(!obsolete.exists());
}

#[test]
fn cleanup_preserves_explicit_active_generation_then_reclaims_it_after_close() {
    let f = Fixture::new();
    let first = f.builder().finish(true).unwrap();
    let active = first.status().database_path.clone();
    drop(first);
    drop(f.builder().finish(true).unwrap());
    drop(f.builder().finish(true).unwrap());
    let report =
        Index::cleanup_owned_retaining(&f.cache(), &f.scope(), std::slice::from_ref(&active))
            .unwrap();
    assert_eq!(report.removed_generations, 1);
    assert!(active.exists());
    assert_eq!(
        Index::cleanup_owned(&f.cache(), &f.scope())
            .unwrap()
            .removed_generations,
        1
    );
    assert!(!active.exists());
    let outside = f.path.join("unrelated.sqlite3");
    fs::write(&outside, b"unrelated user content").unwrap();
    assert!(Index::cleanup_owned_retaining(&f.cache(), &f.scope(), &[outside.clone()]).is_err());
    assert_eq!(fs::read(outside).unwrap(), b"unrelated user content");
}

#[test]
fn common_cache_ancestor_is_excluded_for_all_scope_generations() {
    let f = Fixture::new();
    let common = f.scope().join(".indexes");
    fs::create_dir_all(common.join("another-scope")).unwrap();
    fs::write(common.join("another-scope/private.db"), b"cache").unwrap();
    fs::write(f.scope().join("visible.txt"), b"user").unwrap();
    let cache = common.join("this-scope");
    let c = Cancellation::new();
    let mut b = IndexBuilder::create_with_options(
        &cache,
        &f.scope(),
        &BuildOptions {
            excluded_root: Some(common.clone()),
            ..BuildOptions::default()
        },
        &c,
    )
    .unwrap();
    loop {
        if b.scan_step(2, &c, &|| {}).unwrap().done {
            break;
        }
    }
    let mut index = b.finish(true).unwrap();
    let page = index
        .query(
            &QuerySpec {
                show_hidden: true,
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert!(page.rows.iter().any(|row| row.name == "visible.txt"));
    assert!(page.rows.iter().all(
        |row| !row.relative_path.starts_with(".indexes/another-scope") && row.name != "private.db"
    ));
    assert_eq!(page.status.excluded_cache, 1);
}

#[test]
fn dense_trigram_pages_match_literal_ordered_scans_for_all_sorts() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    for start in (0..4500).step_by(256) {
        let records: Vec<_> = (start..(start + 256).min(4500))
            .map(|index| {
                record(
                    &format!("report{index}.txt"),
                    index % 100 == 0,
                    index as u64 * 17 % 1009,
                )
            })
            .collect();
        b.ingest_batch(&records, &c).unwrap();
        b.checkpoint().unwrap();
    }
    let mut index = b.finish(true).unwrap();
    for sort in [
        SortColumn::Name,
        SortColumn::Size,
        SortColumn::Modified,
        SortColumn::Kind,
    ] {
        for descending in [false, true] {
            let query = QuerySpec {
                text: "report".into(),
                match_mode: MatchMode::Substring,
                sort,
                descending,
                include_total: false,
                ..QuerySpec::default()
            };
            let indexed = index.query(&query, 0, 256, &c).unwrap();
            let scan = index
                .query(
                    &QuerySpec {
                        text: "r".into(),
                        ..query.clone()
                    },
                    0,
                    256,
                    &c,
                )
                .unwrap();
            assert!(indexed.used_trigram && !scan.used_trigram);
            assert_eq!(
                indexed.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
                scan.rows.iter().map(|row| row.id).collect::<Vec<_>>()
            );
            assert!(indexed.rows.len() <= 256);
        }
    }
}

#[test]
fn unicode_literal_substrings_and_verified_short_fallback() {
    let f = Fixture::new();
    let mut b = f.builder();
    let c = Cancellation::new();
    b.ingest_batch(
        &[
            record("ÄPFEL.txt", false, 1),
            record("東京.rs", false, 2),
            record("a\"b%.txt", false, 3),
            record("İ_test.txt", false, 4),
            record("straße.txt", false, 5),
        ],
        &c,
    )
    .unwrap();
    let mut index = b.finish(true).unwrap();
    assert!(index.metrics().unwrap().fts5);
    for (text, expected, trigram) in [
        ("äpf", 1, true),
        ("東京", 1, false),
        ("a\"b%", 1, true),
        ("i\u{307}", 1, false),
        ("STRASSE", 0, true),
        ("Straße", 1, true),
    ] {
        let p = index
            .query(
                &QuerySpec {
                    text: text.into(),
                    match_mode: MatchMode::Substring,
                    ..QuerySpec::default()
                },
                0,
                256,
                &c,
            )
            .unwrap();
        assert_eq!(p.total, Some(expected), "query {text}");
        assert_eq!(p.used_trigram, trigram);
    }
}

#[test]
fn natural_order_folder_groups_and_unsigned_sizes() {
    let f = Fixture::new();
    let mut b = f.builder();
    let c = Cancellation::new();
    b.ingest_batch(
        &[
            record("file10", false, 10),
            record("file02", false, 2),
            record("file2", false, 2),
            record("folder10", true, 0),
            record("folder2", true, 0),
            record("huge", false, u64::MAX),
        ],
        &c,
    )
    .unwrap();
    let mut index = b.finish(true).unwrap();
    let p = index.query(&QuerySpec::default(), 0, 256, &c).unwrap();
    assert_eq!(
        p.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["folder2", "folder10", "file2", "file02", "file10", "huge"]
    );
    let p = index
        .query(
            &QuerySpec {
                sort: SortColumn::Size,
                descending: true,
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert!(p.rows[0].is_dir && p.rows[1].is_dir);
    assert_eq!(p.rows[2].size, u64::MAX);
}

#[test]
fn keyset_pages_have_no_duplicates_and_invalidate_on_revision() {
    let f = Fixture::new();
    let mut b = f.builder();
    let c = Cancellation::new();
    b.ingest_batch(
        &(0..300)
            .map(|i| record(&format!("file{i}"), i % 7 == 0, i))
            .collect::<Vec<_>>(),
        &c,
    )
    .unwrap();
    let spec = QuerySpec {
        include_total: false,
        ..QuerySpec::default()
    };
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let p = b.query_after(&spec, cursor.as_ref(), 37, &c).unwrap();
        assert!(p.rows.len() <= 37);
        all.extend(p.rows.iter().map(|r| r.id));
        cursor = p.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(all.len(), 300);
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 300);
    let first = b.query(&spec, 0, 25, &c).unwrap();
    let cursor = first.next.unwrap();
    b.ingest(&record("new", false, 0), &c).unwrap();
    assert!(matches!(
        b.query_after(&spec, Some(&cursor), 25, &c),
        Err(Error::InvalidCursor)
    ));
}

#[test]
fn pinned_prefix_keeps_global_rows_and_keyset_pages_stable_during_ingestion() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    b.ingest_batch(
        &[record("file2", false, 2), record("file10", false, 10)],
        &c,
    )
    .unwrap();
    let spec = QuerySpec::default();
    let first = b.query(&spec, 0, 1, &c).unwrap();
    let boundary = first.status;
    let cursor = b
        .query_as_of(&spec, &boundary, 0, 1, &c)
        .unwrap()
        .next
        .unwrap();
    b.ingest_batch(&[record("file1", false, 1), record("folder", true, 0)], &c)
        .unwrap();
    let pinned = b.query_as_of(&spec, &boundary, 0, 256, &c).unwrap();
    assert_eq!(pinned.total, Some(2));
    assert_eq!(pinned.status.revision, boundary.revision);
    assert_eq!(
        pinned
            .rows
            .iter()
            .map(|row| row.name.as_str())
            .collect::<Vec<_>>(),
        ["file2", "file10"]
    );
    let next = b
        .query_after_as_of(&spec, &boundary, Some(&cursor), 256, &c)
        .unwrap();
    assert_eq!(next.rows.len(), 1);
    assert_eq!(next.rows[0].name, "file10");
    let live = b.query(&spec, 0, 256, &c).unwrap();
    assert_eq!(live.total, Some(4));
    assert_eq!(live.rows[0].name, "folder");
    let mut unrelated = boundary;
    unrelated.generation.push_str("-other");
    assert!(matches!(
        b.query_as_of(&spec, &unrelated, 0, 1, &c),
        Err(Error::InvalidCursor)
    ));
}

#[test]
fn partial_checkpoint_reopens_and_cancelled_rebuild_preserves_finished_generation() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    b.ingest(&record("old", false, 1), &c).unwrap();
    let old = b.finish(true).unwrap();
    let generation = old.status().generation.clone();
    drop(old);
    let mut b = f.builder();
    b.ingest(&record("partial", false, 2), &c).unwrap();
    b.checkpoint().unwrap();
    let mut progressive = Index::open_database(&b.status().database_path).unwrap();
    let p = progressive
        .query(&QuerySpec::default(), 0, 256, &c)
        .unwrap();
    assert_eq!(p.total, Some(1));
    assert!(!p.status.complete);
    drop(progressive);
    let partial = b.finish_cancelled().unwrap();
    assert_eq!(partial.status().state, BuildState::Cancelled);
    drop(partial);
    let cached = Index::open_scope(&f.cache(), &f.scope()).unwrap();
    assert_eq!(cached.status().generation, generation);
    assert!(cached.status().from_cache);
}

#[test]
fn cancelled_query_is_reusable_and_hidden_kind_filters_work() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    let mut hidden = record("hidden", false, 1);
    hidden.hidden = true;
    b.ingest_batch(
        &[hidden, record("folder", true, 0), record("file", false, 2)],
        &c,
    )
    .unwrap();
    let mut index = b.finish(true).unwrap();
    let cancelled = Cancellation::new();
    cancelled.cancel();
    assert!(matches!(
        index.query(&QuerySpec::default(), 0, 256, &cancelled),
        Err(Error::Cancelled)
    ));
    let p = index
        .query(
            &QuerySpec {
                kind: KindFilter::Files,
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert_eq!(p.total, Some(1));
    let p = index
        .query(
            &QuerySpec {
                show_hidden: true,
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert_eq!(p.total, Some(3));
}

#[test]
fn exact_non_unicode_paths_round_trip() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    #[cfg(windows)]
    let raw = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[b'x' as u16, 0xd800, b'y' as u16])
    };
    #[cfg(unix)]
    let raw = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![b'x', 0xff, b'y'])
    };
    b.ingest(
        &IndexRecord::new(PathBuf::from(&raw), false, 0, 0).unwrap(),
        &c,
    )
    .unwrap();
    let mut index = b.finish(true).unwrap();
    let p = index.query(&QuerySpec::default(), 0, 256, &c).unwrap();
    assert_eq!(p.rows[0].raw_name, raw);
    assert_eq!(p.rows[0].path, f.scope().join(raw));
}

#[test]
fn filesystem_scan_is_progressive_and_excludes_directory_links() {
    let f = Fixture::new();
    let c = Cancellation::new();
    fs::create_dir(f.scope().join("nested")).unwrap();
    fs::write(f.scope().join("nested/inside.txt"), b"fixture").unwrap();
    let outside = f.path.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("must-not-visit.txt"), b"fixture").unwrap();
    #[cfg(windows)]
    let link = std::os::windows::fs::symlink_dir(&outside, f.scope().join("linked"));
    #[cfg(unix)]
    let link = std::os::unix::fs::symlink(&outside, f.scope().join("linked"));
    let mut b = f.builder();
    let mut steps = 0;
    loop {
        let p = b.scan_step(1, &c, &|| {}).unwrap();
        steps += 1;
        if p.done {
            break;
        }
    }
    assert!(steps > 1);
    let mut index = b.finish(true).unwrap();
    let p = index
        .query(
            &QuerySpec {
                text: "inside".into(),
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert_eq!(p.total, Some(1));
    let p = index
        .query(
            &QuerySpec {
                text: "must-not-visit".into(),
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert_eq!(p.total, Some(0));
    if link.is_ok() {
        assert_eq!(index.status().excluded_links, 1);
    } else {
        eprintln!("directory-link fixture unavailable: {link:?}");
    }
}

#[test]
fn cancelled_walk_is_explicitly_partial() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    c.cancel();
    let p = b.scan_step(5, &c, &|| {}).unwrap();
    assert!(p.done);
    assert_eq!(p.status.state, BuildState::Cancelled);
    let index = b.finish(false).unwrap();
    assert!(!index.status().complete);
}

#[test]
#[ignore = "explicit streaming stress fixture; run in release mode"]
fn exceeds_old_count_and_memory_caps_without_a_ram_record_vector() {
    let f = Fixture::new();
    let c = Cancellation::new();
    let mut b = f.builder();
    let count = 300_001;
    for begin in (0..count).step_by(256) {
        let batch = (begin..(begin + 256).min(count))
            .map(|i| {
                record(
                    &format!(
                        "group{:04}/record{i:09}_long_descriptive_東京_name.txt",
                        i % 1000
                    ),
                    false,
                    i as u64,
                )
            })
            .collect::<Vec<_>>();
        b.ingest_batch(&batch, &c).unwrap();
        b.checkpoint().unwrap();
    }
    let mut index = b.finish(true).unwrap();
    assert_eq!(index.status().entries, count as u64);
    assert!(index.status().complete);
    let p = index
        .query(
            &QuerySpec {
                text: "record000300000".into(),
                ..QuerySpec::default()
            },
            0,
            256,
            &c,
        )
        .unwrap();
    assert_eq!(p.total, Some(1));
    assert!(index.metrics().unwrap().page_cache_used <= 4 * 1024 * 1024);
}
