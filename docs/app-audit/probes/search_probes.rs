
#[cfg(test)]
mod app_audit_probes {
    use super::*;
    use std::fs;

    fn fixture(label: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("foldex-audit-{label}-{}", std::process::id()));
        assert!(!base.exists());
        let scope = base.join("scope");
        let cache = base.join("cache");
        fs::create_dir_all(&scope).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(scope.join("item0.txt"), []).unwrap();
        (base, scope, cache)
    }

    fn completed(client: &SearchClient, id: u64) -> Arc<SearchWindow> {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            while let Some(event) = client.try_recv() {
                match event {
                    SearchEvent::Window(w) if w.request == id && w.status.complete && !w.status.from_cache && w.total.is_some() => return w,
                    SearchEvent::Failed { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            assert!(Instant::now() < deadline, "audit request timed out");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn app_audit_completed_search_replays_stale_results_until_refresh() {
        let (base, scope, cache) = fixture("stale-search");
        let hub = SearchHub::new(cache, Arc::new(|| {})).unwrap();
        let client = hub.client();
        let initial = completed(&client, client.request(scope.clone(), Query::default(), 0, false));
        assert_eq!(initial.total, Some(1));
        fs::write(scope.join("item1.txt"), []).unwrap();
        let stale = completed(&client, client.request(scope.clone(), Query::default(), 0, false));
        let refreshed = completed(&client, client.request(scope, Query::default(), 0, true));
        println!("AUDIT {}", serde_json::json!({"probe":"search_freshness", "files_on_disk":2, "after_nonrefresh_request":stale.total, "after_refresh":refreshed.total, "stale_marked_complete":stale.status.complete, "stale_marked_from_cache":stale.status.from_cache, "same_generation":initial.status.generation == stale.status.generation}));
        assert_eq!(stale.total, Some(1));
        assert_eq!(refreshed.total, Some(2));
        hub.shutdown();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn app_audit_unchanged_scope_reactivation_builds_a_new_catalog() {
        let (base, scope, cache) = fixture("rescan");
        let hub = SearchHub::new(cache, Arc::new(|| {})).unwrap();
        let client = hub.client();
        let first = completed(&client, client.request(scope.clone(), Query::default(), 0, false));
        client.set_active(false);
        client.set_active(true);
        let second = completed(&client, client.state.sequence.load(Ordering::Acquire));
        println!("AUDIT {}", serde_json::json!({"probe":"unchanged_scope_rebuild", "first_entries":first.status.entries, "second_entries":second.status.entries, "new_generation":first.status.generation != second.status.generation}));
        assert_ne!(first.status.generation, second.status.generation);
        hub.shutdown();
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn app_audit_third_active_search_waits_after_other_two_finish() {
        let (base, scope, cache) = fixture("fairness");
        let mut hub = SearchHub::unstarted_for_test();
        let clients: Vec<_> = (0..3).map(|_| hub.client()).collect();
        let ids: Vec<_> = clients.iter().map(|c| c.request(scope.clone(), Query::default(), 0, false)).collect();
        let worker = hub.shared.clone();
        hub.worker = Some(thread::spawn(move || run(worker, cache)));
        completed(&clients[2], ids[2]);
        completed(&clients[1], ids[1]);
        thread::sleep(Duration::from_millis(250));
        let oldest_pending = clients[0].window_pending(ids[0]);
        let oldest_had_event = clients[0].try_recv().is_some();
        clients[2].set_active(false);
        let admitted = completed(&clients[0], ids[0]);
        println!("AUDIT {}", serde_json::json!({"probe":"three_active_searches", "oldest_pending_after_other_two_complete":oldest_pending, "oldest_received_any_event":oldest_had_event, "completes_after_newer_client_deactivation":admitted.total == Some(1)}));
        assert!(oldest_pending && !oldest_had_event);
        hub.shutdown();
        fs::remove_dir_all(base).unwrap();
    }
}
