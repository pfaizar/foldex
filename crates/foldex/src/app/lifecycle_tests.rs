use super::*;
use crate::launcher::{instance::Activation, native};
use eframe::App;
use std::sync::atomic::Ordering;

fn background_fixture() -> (Foldex, egui::Context, PathBuf) {
    let (mut app, ctx, root) = super::tests::fixture_app();
    // The fixture's Native runtime stays isolated: these flags model successful
    // registration without creating a tray icon, hotkey or clipboard listener.
    app.background_allowed = true;
    app.launcher.settings.background = true;
    app.launcher
        .native
        .state
        .tray_ready
        .store(true, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(4);
    while app.pane().loading {
        app.poll(&ctx);
        assert!(Instant::now() < deadline, "owned fixture did not resolve");
        std::thread::sleep(Duration::from_millis(2));
    }
    (app, ctx, root)
}

fn hidden_logic(app: &mut Foldex, ctx: &egui::Context) -> egui::LogicOutput {
    window_state_logic(app, ctx, Some(true), Some(true))
}

fn window_state_logic(
    app: &mut Foldex,
    ctx: &egui::Context,
    minimized: Option<bool>,
    occluded: Option<bool>,
) -> egui::LogicOutput {
    let mut input = egui::RawInput::default();
    let root = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    root.minimized = minimized;
    root.occluded = occluded;
    input.focused = false;
    ctx.run_logic(&input, |ctx| {
        app.logic(ctx, &mut eframe::Frame::_new_kittest());
    })
}

#[test]
fn open_explorer_services_launcher_while_minimized_or_occluded() {
    for (minimized, occluded) in [
        (Some(true), Some(false)),
        (Some(true), None),
        (Some(false), Some(true)),
    ] {
        let (mut app, ctx, _) = background_fixture();
        assert!(
            app.explorer_visible,
            "this scenario must not hide the explorer"
        );
        let path = app.pane().path.clone();
        let frame_count = app.frame_count;
        app.launcher
            .native
            .state
            .events
            .fetch_or(native::TOGGLE, Ordering::Release);
        let output = window_state_logic(&mut app, &ctx, minimized, occluded);
        assert!(
            app.launcher_open,
            "window state {minimized:?}/{occluded:?} lost activation"
        );
        assert!(app.run_ui_when_hidden());
        assert!(
            app.explorer_visible,
            "opening the launcher must preserve explorer visibility"
        );
        assert_eq!(
            app.frame_count, frame_count,
            "App::ui was not called by this regression"
        );
        assert_eq!(app.pane().path, path);
        assert!(app.close_requested.is_none());
        assert!(
            output
                .viewport_commands
                .values()
                .flatten()
                .all(|command| !matches!(
                    command,
                    egui::ViewportCommand::Visible(_)
                        | egui::ViewportCommand::Minimized(false)
                        | egui::ViewportCommand::Close
                ))
        );
        app.close_launcher(false);
    }
}

#[test]
fn final_tab_keeps_root_session_and_hidden_launcher_activations_alive() {
    let (mut app, ctx, _) = background_fixture();
    let path = app.pane().path.clone();
    let group = app.desk.focused;
    app.close_tab(app.active);
    assert!(!app.explorer_visible);
    assert!(app.close_requested.is_none());
    assert!(!app.allow_close);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.desk.focused, group);
    assert_eq!(app.pane().path, path);

    for _ in 0..4 {
        app.launcher
            .native
            .state
            .events
            .fetch_or(native::TOGGLE, Ordering::Release);
        let output = hidden_logic(&mut app, &ctx);
        assert!(
            app.launcher_open,
            "hidden logic must consume the tray activation"
        );
        assert!(
            app.run_ui_when_hidden(),
            "the cold immediate child needs a UI pass"
        );
        assert!(!app.explorer_visible);
        assert!(
            output
                .viewport_commands
                .values()
                .flatten()
                .all(|command| { !matches!(command, egui::ViewportCommand::Close) })
        );
        app.close_launcher(false);
        assert!(!app.run_ui_when_hidden());
        assert!(!app.explorer_visible);
        assert!(app.close_requested.is_none());
    }

    app.launcher
        .native
        .state
        .activations
        .lock()
        .unwrap()
        .push_back(Activation::Explorer);
    app.launcher
        .native
        .state
        .events
        .fetch_or(native::ACTIVATE, Ordering::Release);
    let output = hidden_logic(&mut app, &ctx);
    assert!(app.explorer_visible);
    assert_eq!(app.pane().path, path);
    assert_eq!(app.tabs.len(), 1);
    let commands = &output.viewport_commands[&egui::ViewportId::ROOT];
    assert!(commands.contains(&egui::ViewportCommand::Visible(true)));
    assert!(commands.contains(&egui::ViewportCommand::Minimized(false)));
    assert!(!commands.contains(&egui::ViewportCommand::Close));
}

#[test]
fn final_workspace_exits_when_background_is_disabled_or_unavailable() {
    for unavailable in 0..3 {
        let (mut app, _, _) = background_fixture();
        match unavailable {
            0 => app.launcher.settings.background = false,
            1 => app.background_allowed = false,
            _ => app
                .launcher
                .native
                .state
                .tray_ready
                .store(false, Ordering::Release),
        }
        app.close_tab(app.active);
        assert!(app.close_requested.is_some());
        assert!(app.explorer_visible);
        assert!(!app.allow_close, "normal close must await the atomic save");
        assert_eq!(app.tabs.len(), 1);
    }
}

#[test]
fn explicit_quit_saves_and_exits_from_hidden_logic() {
    let (mut app, ctx, root) = background_fixture();
    app.close_tab(app.active);
    app.launcher
        .native
        .state
        .events
        .fetch_or(native::EXIT, Ordering::Release);
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let output = hidden_logic(&mut app, &ctx);
        assert!(
            app.close_requested.is_some(),
            "Quit must bypass close-to-tray"
        );
        if output
            .viewport_commands
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|commands| commands.contains(&egui::ViewportCommand::Close))
        {
            assert!(app.allow_close);
            let saved: Session =
                serde_json::from_slice(&std::fs::read(root.join("session.json")).unwrap()).unwrap();
            assert_eq!(saved.tabs.len(), 1);
            assert!(saved.launcher.background);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "hidden Quit did not receive its save acknowledgement"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn stale_workspace_close_cannot_shut_down_the_only_live_workspace() {
    let (mut app, _, _) = background_fixture();
    app.close_workspace(u64::MAX);
    assert!(app.explorer_visible);
    assert!(app.close_requested.is_none());
    assert!(!app.allow_close);
}
