use super::*;

#[test]
fn permanent_save_failure_stops_retries_and_keeps_the_window_recoverable() {
    let (mut app, ctx, _) = super::tests::fixture_app();
    app.saved_layouts.push(crate::workspace::NamedWorkspace {
        name: "x".repeat(crate::persistence::MAX_SESSION_BYTES + 1),
        state: Default::default(),
    });
    app.service_session(&ctx);
    app.save_due = Some(Instant::now());
    app.close_requested = Some(Instant::now());
    app.service_session(&ctx);
    assert!(app.session_dirty && app.close_failed && app.save_due.is_none());
    let toast_until = app.toast.as_ref().unwrap().until;
    for _ in 0..8 {
        app.service_session(&ctx);
        assert!(app.save_due.is_none());
        assert_eq!(app.toast.as_ref().unwrap().until, toast_until);
    }
    app.request_explorer_close(&ctx);
    assert!(app.allow_close);
}

#[test]
fn another_windows_preferences_merge_around_unsaved_edits_and_preserve_navigation() {
    let (mut app, ctx, root) = super::tests::fixture_app();
    let base = serde_json::to_value(app.session_snapshot()).unwrap();
    let writer = crate::persistence::Persistence::new_session(
        Arc::new(|| {}),
        root.join("preferences.json"),
        &base,
    )
    .unwrap();
    let mut remote = base.clone();
    remote["bookmarks"] = serde_json::json!([root.join("remote")]);
    writer
        .save(
            root.join("other-window.json"),
            serde_json::to_vec(&remote).unwrap(),
        )
        .unwrap();
    app.bookmarks.push(root.join("local"));
    let route = app.pane().route().clone();
    let navigation = app.pane().navigation.clone();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.persistence.preferences_pending() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    app.service_session(&ctx);
    assert!(app.bookmarks.contains(&root.join("remote")));
    assert!(app.bookmarks.contains(&root.join("local")));
    assert_eq!(app.pane().route(), &route);
    assert_eq!(app.pane().navigation, navigation);
}

#[cfg(windows)]
#[test]
fn shared_hotkey_refresh_keeps_the_window_attached_shortcut_recorder() {
    let (mut app, _, _) = super::tests::fixture_app();
    // An invalid owner never holds focus, so an attached recorder refuses to
    // start. A detached one would silently record without the keyboard hook.
    app.hotkeys.attach_native_capture(-1);
    let mut session: Session =
        serde_json::from_value(serde_json::to_value(app.session_snapshot()).unwrap()).unwrap();
    session.hotkeys = Some(vec![crate::hotkeys::Binding::single(
        crate::commands::Command::Refresh,
        egui::Key::F9,
        egui::Modifiers::CTRL,
    )]);
    app.apply_shared_preferences(session);
    assert!(
        app.hotkeys
            .bindings()
            .iter()
            .any(|binding| binding.command == crate::commands::Command::Refresh)
    );
    app.hotkeys.record(crate::commands::Command::Refresh);
    assert!(!app.hotkeys.is_recording());
}
