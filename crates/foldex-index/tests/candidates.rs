#![cfg(feature = "catalog-experiment")]

use foldex_index::{Cancellation, Error, IndexRecord, catalog::NativeIndexBuilder};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/candidate-fixtures");
        fs::create_dir_all(&base).unwrap();
        let path = base.join(format!(
            "owned-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::create_dir(path.join("scope")).unwrap();
        Self(path)
    }
    fn builder(&self) -> NativeIndexBuilder {
        NativeIndexBuilder::create(
            &self.0.join("cache"),
            &self.0.join("scope"),
            &Cancellation::new(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/candidate-fixtures");
        if let (Ok(path), Ok(base)) = (self.0.canonicalize(), base.canonicalize()) {
            if path.starts_with(&base) && path != base {
                let _ = fs::remove_dir_all(path);
            }
        }
    }
}

#[test]
fn opt_in_name_ranking_visits_each_prefix_once_and_keeps_only_64_locations() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder = fixture.builder();
    // The ingestion API deliberately caps each call at 512 records/1 MiB.
    // Cross multiple search batches without bypassing that production limit.
    for first in (0..8210u64).step_by(512) {
        let records: Vec<_> = (first..(first + 512).min(8210))
            .map(|at| {
                IndexRecord::new(PathBuf::from(format!("item{at:05}.txt")), false, at, 0).unwrap()
            })
            .collect();
        builder.ingest_batch(&records, &cancel).unwrap();
    }
    let index = builder.finish(true).unwrap();
    let mut calls = 0;
    let first = index
        .candidate_batch(0, &cancel, &mut |name, _, _, _| {
            calls += 1;
            Some(name[4..9].parse::<i32>().unwrap())
        })
        .unwrap();
    assert_eq!(first.scanned_to, 8192);
    assert_eq!(calls, 8192);
    assert_eq!(first.hits.len(), 64);
    let next = index
        .candidate_batch(first.scanned_to, &cancel, &mut |name, _, _, _| {
            calls += 1;
            Some(name[4..9].parse::<i32>().unwrap())
        })
        .unwrap();
    assert_eq!(next.scanned_to, 8210);
    assert_eq!(calls, 8210);
    assert_eq!(next.hits.len(), 18);
    let mut best = first.hits;
    best.extend(next.hits);
    best.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(a.id.cmp(&b.id)));
    best.truncate(64);
    let rows = index.candidate_rows(&best, &cancel).unwrap();
    assert_eq!(rows.len(), 64);
    assert_eq!(rows[0].name, "item08209.txt");
    assert_eq!(rows[63].name, "item08146.txt");
    let empty = index
        .candidate_batch(8210, &cancel, &mut |_, _, _, _| {
            panic!("An already visited prefix must not be rescored")
        })
        .unwrap();
    assert!(empty.hits.is_empty());
}

#[test]
fn opt_in_candidate_cancellation_and_invalid_prefix_do_not_publish_partial_hits() {
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder = fixture.builder();
    let records: Vec<_> = (0..260)
        .map(|at| IndexRecord::new(PathBuf::from(format!("item{at}")), false, 0, 0).unwrap())
        .collect();
    builder.ingest_batch(&records, &cancel).unwrap();
    let index = builder.finish(true).unwrap();
    let mut calls = 0;
    let result = index.candidate_batch(0, &cancel, &mut |_, _, _, _| {
        calls += 1;
        if calls == 4 {
            cancel.cancel();
        }
        Some(10)
    });
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(calls <= 128);
    let result = index.candidate_batch(261, &Cancellation::new(), &mut |_, _, _, _| {
        panic!("Invalid prefix")
    });
    assert!(matches!(result, Err(Error::InvalidCursor)));
}

#[cfg(windows)]
#[test]
fn opt_in_name_display_preserves_raw_windows_path_identity_when_decoding_winners() {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    let fixture = Fixture::new();
    let cancel = Cancellation::new();
    let mut builder = fixture.builder();
    let raw = OsString::from_wide(&[b'n' as u16, 0xd800, b'.' as u16, b't' as u16]);
    let record = IndexRecord::new(PathBuf::from(&raw), false, 0, 0).unwrap();
    builder.ingest_batch(&[record], &cancel).unwrap();
    let index = builder.finish(true).unwrap();
    let batch = index
        .candidate_batch(0, &cancel, &mut |name, _, _, _| {
            assert!(name.starts_with('n'));
            Some(1)
        })
        .unwrap();
    let rows = index.candidate_rows(&batch.hits, &cancel).unwrap();
    assert_eq!(rows[0].raw_name, raw);
    assert_eq!(rows[0].path, index.status().scope.join(&raw));
}
