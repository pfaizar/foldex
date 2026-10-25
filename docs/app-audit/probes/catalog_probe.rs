use foldex_index::{catalog::NativeIndexBuilder, Cancellation, IndexRecord, QuerySpec};
use std::{fs, path::PathBuf, time::Instant};

fn main() {
    let root = std::env::temp_dir().join(format!("foldex-audit-catalog-{}", std::process::id()));
    assert!(!root.exists());
    let scope = root.join("scope");
    let cache = root.join("cache");
    fs::create_dir_all(&scope).unwrap();
    fs::create_dir_all(&cache).unwrap();
    let cancel = Cancellation::new();
    let mut builder = NativeIndexBuilder::create(&cache, &scope, &cancel).unwrap();
    let count = 100_000;
    for first in (0..count).step_by(512) {
        let records: Vec<_> = (first..(first+512).min(count)).map(|i| {
            IndexRecord::new(PathBuf::from(format!("folder{:04}/report-{i:08}.txt", i%1024)), false, i as u64, 1_700_000_000+i as i64).unwrap()
        }).collect();
        builder.ingest_batch(&records, &cancel).unwrap();
    }
    let mut index = builder.finish(true).unwrap();
    for text in ["", "report", "rpt"] {
        let spec = QuerySpec { text:text.into(), include_total:false, ..QuerySpec::default() };
        for (label, offset) in [("first",0),("within_head",768),("cross_head",1024),("deep_warm",50_000)] {
            let started = Instant::now();
            let page = index.query(&spec, offset,256,&cancel).unwrap();
            let timing = index.last_query_timing();
            let elapsed = started.elapsed().as_secs_f64()*1000.0;
            println!("{{\"probe\":\"catalog_paging\",\"records\":{count},\"query\":\"{text}\",\"page\":\"{label}\",\"offset\":{offset},\"rows\":{},\"elapsed_ms\":{elapsed:.3},\"head_ms\":{:.3},\"row_decode_ms\":{:.3}}}",page.rows.len(),timing.head.as_secs_f64()*1000.0,timing.rows.as_secs_f64()*1000.0);
        }
    }
    drop(index);
    assert_eq!(root.canonicalize().unwrap().parent(),Some(std::env::temp_dir().canonicalize().unwrap().as_path()));
    fs::remove_dir_all(root).unwrap();
}
