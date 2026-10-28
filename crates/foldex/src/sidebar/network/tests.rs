use super::*;
use std::sync::atomic::AtomicUsize;

fn pending(
    data: &mut Data,
) -> (
    mpsc::SyncSender<Result<Snapshot, String>>,
    Arc<Cancellation>,
) {
    let (sender, result) = mpsc::sync_channel(1);
    let cancel = Arc::new(Cancellation::new().unwrap());
    data.flight = Some(Flight {
        cancel: cancel.clone(),
        result,
    });
    (sender, cancel)
}

#[test]
fn construction_persisted_expansion_and_idle_service_never_start_discovery_or_wake() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let worker_wakes = wakes.clone();
    let mut data = Data::new(Arc::new(move || {
        worker_wakes.fetch_add(1, Ordering::Relaxed);
    }));
    for active in [true, false, true] {
        for _ in 0..100 {
            data.service(active);
        }
    }
    assert!(data.flight.is_none() && data.action.is_none());
    assert!(!data.loading() && !data.loaded && !data.requested);
    assert_eq!(wakes.load(Ordering::Relaxed), 0);
}

#[test]
fn only_complete_unc_shares_become_navigation_targets() {
    for invalid in [
        "",
        "provider",
        "Z:",
        "Z:\\",
        r"\server\share",
        r"\\server",
        r"\\server\",
        r"\\?\UNC\server\share",
        r"\\.\device\share",
        r"\\server\..",
        r"\\.\share",
        r"\\server\share\..",
        r"\\server/share",
        "\\\\server\\share\0hidden",
    ] {
        assert!(
            Resource::from_names(invalid, None, true).is_none(),
            "{invalid:?}"
        );
    }
    assert!(
        Resource::from_names(
            &format!(r"\\server\{}", "x".repeat(MAX_NAME_UNITS)),
            None,
            false
        )
        .is_none()
    );
    let share = Resource::from_names(r"\\server\Shared Files\", Some("Z:"), true).unwrap();
    assert_eq!(share.path, PathBuf::from(r"\\server\Shared Files"));
    assert_eq!(share.label, "Shared Files on server (Z:)");
    assert!(share.connected);
    assert_eq!(
        Resource::from_names(r"\\server\share", Some("bad\nlocal"), false)
            .unwrap()
            .local,
        None
    );
}

#[test]
fn connected_and_remembered_entries_merge_without_probing_and_keep_connected_state() {
    let mut snapshot = Snapshot::default();
    snapshot.insert(Resource::from_names(r"\\NAS\Projects", None, true).unwrap());
    snapshot.insert(Resource::from_names(r"\\nas\projects", Some("P:"), false).unwrap());
    snapshot.insert(Resource::from_names(r"\\backup\Archive", Some("B:"), false).unwrap());
    let snapshot = snapshot.normalize();
    assert_eq!(snapshot.resources.len(), 2);
    assert!(snapshot.resources[0].connected);
    assert_eq!(snapshot.resources[0].local.as_deref(), Some("P:"));
    assert!(!snapshot.resources[1].connected);
    for index in 0..MAX_RESOURCES * 2 {
        let mut bounded = snapshot.clone();
        for item in 0..index {
            bounded.insert(
                Resource::from_names(&format!(r"\\server\share{item}"), None, false).unwrap(),
            );
        }
        assert!(bounded.resources.len() <= MAX_RESOURCES);
        assert_eq!(bounded.truncated, index + 2 > MAX_RESOURCES);
    }
}

#[test]
fn hiding_cancels_one_inflight_helper_and_discards_its_late_result() {
    let mut data = Data::new(Arc::new(|| {}));
    data.benchmark_snapshot(&[(r"\\old\share", Some("Z:"), true)], None);
    let previous = data.resources().to_vec();
    let revision = data.revision();
    let (sender, cancel) = pending(&mut data);
    data.service(false);
    assert!(cancel.is_cancelled());
    assert!(!data.loading());
    let mut replacement = Snapshot::default();
    replacement.insert(Resource::from_names(r"\\late\share", None, true).unwrap());
    sender.send(Ok(replacement)).unwrap();
    data.service(true);
    assert_eq!(data.resources(), previous);
    assert_eq!(data.revision(), revision);
    assert!(data.flight.is_none());
    assert!(!data.requested && !data.has_error());
}

#[test]
fn refresh_clicks_coalesce_and_drop_cancels_without_waiting() {
    let mut data = Data::new(Arc::new(|| {}));
    let (_sender, cancel) = pending(&mut data);
    for _ in 0..100 {
        data.perform(UserAction::Refresh);
        data.service(true);
    }
    assert!(!data.requested);
    assert!(!cancel.is_cancelled());
    data.service(false);
    data.perform(UserAction::Refresh);
    assert!(data.requested);
    assert!(Arc::ptr_eq(&cancel, &data.flight.as_ref().unwrap().cancel));
    drop(data);
    assert!(cancel.is_cancelled());
}

#[test]
fn offline_error_keeps_cached_rows_and_never_automatically_retries() {
    let mut data = Data::new(Arc::new(|| {}));
    data.benchmark_snapshot(&[(r"\\offline\archive", Some("A:"), false)], None);
    let (sender, _) = pending(&mut data);
    sender
        .send(Err("Windows reports that the network is unavailable".into()))
        .unwrap();
    data.service(true);
    assert_eq!(data.resources().len(), 1);
    assert!(!data.resources()[0].connected);
    assert!(data.has_error() && data.status().contains("Retry"));
    assert!(data.detail().contains("network"));
    for _ in 0..100 {
        data.service(true);
    }
    assert!(data.flight.is_none() && data.action.is_none() && !data.requested);
}

#[test]
fn native_dialog_cancellation_is_quiet_and_success_refreshes_only_a_visible_section() {
    for (active, refresh) in [(true, false), (false, true)] {
        let mut data = Data::new(Arc::new(|| {}));
        let (sender, receiver) = mpsc::sync_channel(1);
        data.action = Some(receiver);
        sender.send(Ok(refresh)).unwrap();
        data.service(active);
        assert!(data.action.is_none() && data.flight.is_none());
        assert!(!data.has_error() && !data.requested);
    }
}

#[test]
fn helper_output_keeps_the_last_complete_snapshot_and_rejects_oversized_data() {
    let snapshot = Snapshot {
        resources: vec![Resource::from_names(r"\\server\share", None, true).unwrap()],
        ..Default::default()
    };
    let mut bytes = serde_json::to_vec(&Ok::<_, String>(&snapshot)).unwrap();
    bytes.extend_from_slice(b"\n{\"Ok\":");
    assert_eq!(decode_reply(&bytes).unwrap(), snapshot);
    assert!(decode_reply(&vec![b' '; MAX_REPLY_BYTES + 1]).is_err());
    assert!(decode_reply(b"unrelated output").is_err());
    assert!(!run_helper_if_requested(&[
        HELPER_ARGUMENT.into(),
        "--home".into()
    ]));
    assert!(!run_helper_if_requested(&["--benchmark-keyboard".into()]));
}

#[test]
fn capture_fixture_uses_only_valid_cached_unc_paths_and_no_native_actions() {
    let mut data = Data::new(Arc::new(|| panic!("fixture must not wake a worker")));
    data.benchmark_snapshot(
        &[
            (r"\\design\Projects", Some("P:"), true),
            (r"\\backup\Archive", Some("B:"), false),
            (r"\\device", None, true),
        ],
        Some("Offline fixture: provider unavailable"),
    );
    data.service(true);
    assert_eq!(data.resources().len(), 2);
    assert!(data.resources()[0].connected && !data.resources()[1].connected);
    assert!(data.loaded && data.has_error());
    assert!(data.flight.is_none() && data.action.is_none() && !data.requested);
}
