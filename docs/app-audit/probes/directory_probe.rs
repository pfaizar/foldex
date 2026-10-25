use foldex_core::{enumerate_directory, filter_snapshot, Query};
use std::{fs, path::PathBuf, time::Instant};

fn main() {
    let count = 150_000;
    let root = std::env::temp_dir().join(format!("foldex-audit-directory-{}", std::process::id()));
    assert!(!root.exists());
    fs::create_dir(&root).unwrap();
    let suffix = "x".repeat(110);
    let filename = |i| format!("{i:06}_{suffix}.txt");
    for i in 0..count {
        fs::write(root.join(filename(i)), []).unwrap();
    }
    let started = Instant::now();
    let snapshot = enumerate_directory(&root).unwrap();
    let elapsed = started.elapsed();
    let mut present = vec![false; count];
    for entry in snapshot.entries.iter() {
        present[entry.name[..6].parse::<usize>().unwrap()] = true;
    }
    let omitted = present.iter().position(|p| !p).unwrap();
    let needle = filename(omitted);
    let filtered = filter_snapshot(snapshot.clone(), &Query {
        text:needle.clone(), match_mode:foldex_match::MatchMode::Substring, ..Query::default()
    });
    println!("{{\"probe\":\"large_flat_directory\",\"created\":{count},\"filename_bytes\":{},\"enumerated\":{},\"complete\":{},\"limit\":\"{:?}\",\"logical_model_bytes\":{},\"enumerate_ms\":{:.3},\"omitted_file_exists\":{},\"filter_matches_for_omitted_file\":{}}}", needle.len(),snapshot.entries.len(),snapshot.complete,snapshot.limit,snapshot.managed_bytes,elapsed.as_secs_f64()*1000.0,root.join(needle).is_file(),filtered.indices.len());
    assert!(!snapshot.complete && snapshot.entries.len() < count);
    assert_eq!(filtered.indices.len(), 0);
    drop(filtered);
    drop(snapshot);
    let canonical = root.canonicalize().unwrap();
    assert_eq!(canonical.parent(), Some(std::env::temp_dir().canonicalize().unwrap().as_path()));
    fs::remove_dir_all(root).unwrap();
    let _ = PathBuf::new();
}
