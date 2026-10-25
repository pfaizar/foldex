
#[cfg(test)]
mod app_audit_probes {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn app_audit_open_waits_behind_a_blocked_mutation() {
        let base = std::env::temp_dir().join(format!("foldex-audit-queue-{}", std::process::id()));
        assert!(!base.exists());
        fs::create_dir(&base).unwrap();
        let platform = Platform::new(Arc::new(|| {})).unwrap();
        let history = platform.history_handle();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let holder = thread::spawn(move || history.mutate("Audit gate", || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Mutation {result:Ok(String::new()), steps:vec![], changed:false}
        }));
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        platform.try_submit(Job::NewFolder {parent:base.clone(), name:"new-folder".into()}).unwrap();
        platform.try_submit(Job::Open(base.join("missing-file.txt"))).unwrap();
        let until = Instant::now() + Duration::from_millis(250);
        let mut events_while_blocked = 0;
        while Instant::now() < until {
            if platform.try_recv().is_some() {events_while_blocked += 1;}
            thread::sleep(Duration::from_millis(2));
        }
        release_tx.send(()).unwrap();
        holder.join().unwrap().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        let mut events_after_release = 0;
        while Instant::now() < until && events_after_release < 2 {
            if platform.try_recv().is_some() {events_after_release += 1;}
            thread::sleep(Duration::from_millis(2));
        }
        println!("AUDIT {}",serde_json::json!({"probe":"operation_queue", "simulation":"mutation history lock held for 250 ms", "events_while_mutation_blocked":events_while_blocked, "events_after_release":events_after_release}));
        assert_eq!(events_while_blocked,0);
        assert_eq!(events_after_release,2);
        drop(platform);
        fs::remove_dir_all(base).unwrap();
    }
}
