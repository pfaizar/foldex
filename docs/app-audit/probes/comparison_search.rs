
#[cfg(test)]
mod app_comparison_probes {
    use super::*;
    use std::fs;
    use foldex_index::IndexRecord;
    fn fixture(label: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("foldex-comparison-{label}-{}", std::process::id()));
        assert!(!root.exists());
        fs::create_dir_all(root.join("scope")).unwrap(); fs::create_dir_all(root.join("cache")).unwrap();
        (root.clone(), root.join("scope"), root.join("cache"))
    }
    #[test]
    fn app_comparison_equal_windows() {
        let (root, scope, base) = fixture("windows");
        let cancel = Cancellation::new();
        let runs: usize = std::env::var("FOLDEX_COMPARISON_RUNS").unwrap().parse().unwrap();
        for run in 0..runs {
            let cache_base = base.join(format!("run{run}"));
            let cache = Index::cache_dir_for(&cache_base, &scope).unwrap();
            let mut builder = IndexBuilder::create(&cache, &scope, &cancel).unwrap();
            for first in (0..100_000).step_by(512) {
                let records = (first..(first + 512).min(100_000)).map(|i| IndexRecord::new(
                    format!("folder{:04}/report-{i:08}.txt", i % 1024).into(), false, i as u64, 1_700_000_000 + i as i64).unwrap()).collect::<Vec<_>>();
                builder.ingest_batch(&records, &cancel).unwrap();
            }
            let index = builder.finish(true).unwrap(); let database = index.status().database_path.clone(); drop(index);
            for text in ["", "report", "rpt"] {
                let hub = SearchHub::unstarted_for_test(); let client = hub.client();
                let query = Query { text: text.into(), ..Default::default() };
                client.request(scope.clone(), query.clone(), 0, false);
                let mut context = Context::new(&client.state, client.state.input().unwrap(), &cache_base).unwrap();
                context.backend = Some(Backend::Complete(NativeIndex::open_database(&database).unwrap()));
                for (phase, start) in [("first", 0), ("nearby_crossing", 640), ("cross_head", 1024), ("arbitrary_seek", 50_000), ("warm_deep", 50_512)] {
                    client.request(scope.clone(), query.clone(), start, false);
                    context.replace_request(client.state.input().unwrap());
                    let started = Instant::now(); context.fetch_window(&cancel).unwrap();
                    let elapsed = started.elapsed();
                    let rows = context.window.as_ref().unwrap().entries.len(); assert_eq!(rows, WINDOW_ROWS);
                    println!("COMPARISON {}", serde_json::json!({"probe":"complete_window", "run":run, "query":text, "phase":phase, "offset":start, "rows":rows, "records":100000, "elapsed_ms":elapsed.as_secs_f64()*1000.0}));
                }
                drop(context); hub.shutdown();
            }
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn app_comparison_large_directory_and_refresh() {
        let (root, scope, base) = fixture("directory");
        let count = 150_000;
        let suffix = "x".repeat(110); let filename = |i| format!("{i:06}_{suffix}.txt");
        for i in 0..count { fs::write(scope.join(filename(i)), []).unwrap(); }
        let started = Instant::now(); let snapshot = foldex_core::enumerate_directory(&scope).unwrap();
        let elapsed = started.elapsed(); let mut present = vec![false; count];
        for entry in snapshot.entries.iter() { present[entry.name[..6].parse::<usize>().unwrap()] = true; }
        let omitted = present.iter().position(|p| !p).expect("fixture must exercise the model limit");
        let needle = filename(omitted);
        let filtered = foldex_core::filter_snapshot(snapshot.clone(), &Query { text: needle.clone(), match_mode: foldex_core::MatchMode::Substring, ..Default::default() });
        println!("COMPARISON {}", serde_json::json!({"probe":"bounded_directory", "created":count, "filename_bytes":needle.len(), "rows":snapshot.entries.len(), "complete":snapshot.complete, "model_bytes":snapshot.managed_bytes, "limit":format!("{:?}",snapshot.limit), "filter_matches":filtered.indices.len(), "elapsed_ms":elapsed.as_secs_f64()*1000.0}));
        let cancel = Cancellation::new();
        /* AFTER_DIRECTORY */
        /* BEFORE_REFRESH */
        drop(filtered); drop(snapshot); fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn app_comparison_query_lane() {
        let (root, scope, base) = fixture("query-lane"); let cancel = Cancellation::new();
        let runs: usize = std::env::var("FOLDEX_COMPARISON_RUNS").unwrap().parse().unwrap();
        for run in 0..runs {
            let cache_base = base.join(format!("run{run}")); let cache = Index::cache_dir_for(&cache_base, &scope).unwrap();
            let mut builder = IndexBuilder::create(&cache, &scope, &cancel).unwrap();
            for first in (0..100_000).step_by(512) {
                let records = (first..(first + 512).min(100_000)).map(|i| IndexRecord::new(format!("folder{:04}/report-{i:08}.txt", i % 1024).into(), false, i as u64, 1_700_000_000 + i as i64).unwrap()).collect::<Vec<_>>();
                builder.ingest_batch(&records, &cancel).unwrap();
            }
            let index = builder.finish(true).unwrap(); let database = index.status().database_path.clone(); drop(index);
            for text in ["", "report", "rpt"] {
                let hub = SearchHub::unstarted_for_test(); let client = hub.client();
                /* POOL_SETUP */
                let query = Query { text: text.into(), ..Default::default() };
                client.request(scope.clone(), query.clone(), 0, false);
                let mut context = Context::new(&client.state, client.state.input().unwrap(), &cache_base).unwrap();
                context.backend = Some(Backend::Complete(NativeIndex::open_database(&database).unwrap()));
                for (phase, start) in [("first", 0), ("nearby_crossing", 640), ("cross_head", 1024), ("arbitrary_seek", 50_000), ("warm_deep", 50_512)] {
                    client.request(scope.clone(), query.clone(), start, false); context.replace_request(client.state.input().unwrap());
                    let started = Instant::now();
                    /* STEP_CALL */;
                    let dispatch = started.elapsed();
                    while context.query_dirty {
                        /* STEP_CALL */;
                        assert!(started.elapsed() < Duration::from_secs(30));
                        if context.query_dirty { thread::sleep(Duration::from_millis(1)); }
                    }
                    let elapsed = started.elapsed(); let rows = context.window.as_ref().unwrap().entries.len(); assert_eq!(rows, WINDOW_ROWS);
                    println!("COMPARISON {}", serde_json::json!({"probe":"query_lane_window", "run":run, "query":text, "phase":phase, "offset":start, "rows":rows, "records":100000, "elapsed_ms":elapsed.as_secs_f64()*1000.0, "first_coordinator_turn_ms":dispatch.as_secs_f64()*1000.0}));
                }
                drop(context); /* POOL_DROP */ hub.shutdown();
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

}
