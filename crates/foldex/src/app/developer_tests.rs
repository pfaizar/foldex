use super::*;
use crate::app::tests::{fixture_app, key_event, pump_events};

#[test]
fn error_toasts_are_plain_by_default_and_keep_the_detail_for_developers() {
    crate::friendly::set_developer_mode(false);
    let (mut app, _, _) = fixture_app();
    app.toast_error(
        "Couldn't copy the selected items",
        "Access is denied. (os error 5)",
    );
    let toast = app.toast.as_ref().unwrap();
    assert!(toast.error);
    assert_eq!(
        toast.text,
        "Couldn't copy the selected items. You don't have permission to do that."
    );

    crate::friendly::set_developer_mode(true);
    app.toast_error(
        "Couldn't copy the selected items",
        "Access is denied. (os error 5)",
    );
    let text = &app.toast.as_ref().unwrap().text;
    assert!(text.starts_with("Couldn't copy the selected items. You don't have permission"));
    assert!(text.ends_with("Access is denied. (os error 5)"));
    crate::friendly::set_developer_mode(false);
}

#[test]
fn the_raw_detail_is_always_logged_even_with_developer_mode_off() {
    crate::friendly::set_developer_mode(false);
    let (mut app, _, _) = fixture_app();
    app.toast_error(
        "Couldn't move the selected items",
        "Windows HRESULT 0x80070020",
    );
    let log = std::fs::read_to_string(crate::friendly::log_path().unwrap()).unwrap();
    assert!(log.contains("HRESULT 0x80070020"), "{log}");
    assert!(!app.toast.as_ref().unwrap().text.contains("HRESULT"));
}

#[test]
fn a_silent_success_shows_no_empty_bubble() {
    let (mut app, _, _) = fixture_app();
    app.toast("");
    app.toast("   ");
    app.toast(crate::friendly::present_notice(""));
    assert!(app.toast.is_none());
    app.toast("Theme reloaded");
    app.toast("");
    assert_eq!(app.toast.as_ref().unwrap().text, "Theme reloaded");
}

#[test]
fn unknown_failures_never_show_internal_wording() {
    crate::friendly::set_developer_mode(false);
    let (mut app, _, _) = fixture_app();
    app.toast_error("", "inspector worker closed");
    assert_eq!(
        app.toast.as_ref().unwrap().text,
        "Something went wrong. Please try again."
    );
}

#[test]
fn performance_details_open_only_in_developer_mode() {
    let chord = || {
        vec![key_event(
            egui::Key::P,
            egui::Modifiers::CTRL | egui::Modifiers::ALT,
        )]
    };
    crate::friendly::set_developer_mode(false);
    let (mut app, ctx, _) = fixture_app();
    pump_events(&mut app, &ctx, 1, chord());
    pump_events(&mut app, &ctx, 2, vec![]);
    assert!(
        !app.show_stats,
        "the shortcut must do nothing without Developer Mode"
    );

    crate::friendly::set_developer_mode(true);
    pump_events(&mut app, &ctx, 3, chord());
    pump_events(&mut app, &ctx, 4, vec![]);
    assert!(app.show_stats);

    // Turning Developer Mode off closes the tool again.
    crate::friendly::set_developer_mode(false);
    pump_events(&mut app, &ctx, 5, vec![]);
    pump_events(&mut app, &ctx, 6, vec![]);
    assert!(!app.show_stats);
}

#[test]
fn developer_mode_is_saved_and_synced_between_windows() {
    crate::friendly::set_developer_mode(false);
    let (mut app, _, _) = fixture_app();
    assert!(!app.session_snapshot().developer_mode);
    crate::friendly::set_developer_mode(true);
    let session = app.session_snapshot();
    assert!(session.developer_mode);
    let value = serde_json::to_value(&session).unwrap();
    assert_eq!(value["developer_mode"], true);
    assert_eq!(
        crate::persistence::shared::preferences(&value)["developer_mode"],
        true,
        "other windows must receive the setting"
    );
    // An old session file without the field stays off.
    let old: Session = serde_json::from_value(serde_json::json!({"tabs": []})).unwrap();
    assert!(!old.developer_mode);
    // Another window turning it off reaches this one.
    let mut remote = app.session_snapshot();
    remote.developer_mode = false;
    app.apply_shared_preferences(remote);
    assert!(!crate::friendly::developer_mode());
}
