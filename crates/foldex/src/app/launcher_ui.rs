use super::*;
use crate::launcher::{Builtin, Mode, ResultAction, instance::Activation, native};

impl Foldex {
    pub(super) fn open_launcher(&mut self, settings: bool) {
        if self.launcher_auxiliary {
            self.job(Job::Launcher { settings });
            return;
        }
        if settings {
            self.open_launcher_options();
            return;
        }
        if !self.launcher_open {
            self.launcher.set_discovery_paths(
                self.bookmarks.clone(),
                self.recent.iter().cloned().collect(),
            );
            let current = if self.pane().is_home() {
                PathBuf::new()
            } else {
                self.pane().path.clone()
            };
            self.launcher.open(current);
            self.launcher_open = true;
            self.launcher_positioned = false;
            self.launcher_focused = false;
            self.ui_context.data_mut(|data| {
                data.remove::<u8>(Id::new("launcher-placement-attempts"));
            });
        }
        crate::native_keys::set_file_mode(false);
        self.ui_context.request_repaint();
    }

    pub(super) fn open_launcher_options(&mut self) {
        self.close_launcher(false);
        let ctx = self.ui_context.clone();
        self.show_explorer(&ctx);
        self.options_popup
            .open_launcher_section(&self.launcher.settings);
        self.options_popup.prepare_open(&ctx);
        self.options = true;
        crate::native_keys::set_file_mode(false);
        ctx.request_repaint();
    }

    pub(super) fn close_launcher(&mut self, restore_focus: bool) {
        // This runs on the HWND's owner thread. Hide before cancellation/clear,
        // including hotkey/tray closes that happen before the child UI callback.
        if self.launcher_initialized || self.launcher_open {
            native::hide_launcher(&format!("Foldex Launcher · {}", std::process::id()));
            self.ui_context.send_viewport_cmd_to(
                egui::ViewportId::from_hash_of("foldex-focused-launcher"),
                egui::ViewportCommand::Visible(false),
            );
        }
        self.launcher_open = false;
        self.launcher_positioned = false;
        self.launcher_focused = false;
        self.launcher.close();
        if !self.explorer_visible
            && self.benchmark.is_none()
            && !self
                .launcher
                .native
                .state
                .tray_ready
                .load(std::sync::atomic::Ordering::Acquire)
        {
            self.show_explorer(&self.ui_context.clone());
        }
        if restore_focus && self.benchmark.is_none() {
            crate::launcher::Launcher::activate_window(self.launcher.previous());
        }
    }

    pub(super) fn show_explorer(&mut self, ctx: &egui::Context) {
        self.explorer_visible = true;
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Minimized(false),
        );
        if !self.launcher_open && self.benchmark.is_none() {
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        }
        ctx.request_repaint();
    }

    pub(super) fn hide_explorer(&mut self, ctx: &egui::Context) {
        self.explorer_visible = false;
        for tab in &mut self.tabs {
            tab.left.set_active(false);
            if let Some(pane) = &mut tab.right {
                pane.set_active(false);
            }
        }
        crate::native_keys::set_file_mode(false);
        ctx.send_viewport_cmd_to(
            egui::ViewportId::ROOT,
            egui::ViewportCommand::Visible(false),
        );
    }

    pub(super) fn request_explorer_close(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        if self.allow_close
            || self.close_failed
            || self
                .close_requested
                .is_some_and(|requested| requested.elapsed() > Duration::from_secs(2))
        {
            self.allow_close = true;
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
            return;
        }
        if self.close_requested.is_none()
            && self.background_allowed
            && self.launcher.settings.background
            && self
                .launcher
                .native
                .state
                .tray_ready
                .load(std::sync::atomic::Ordering::Acquire)
        {
            self.hide_explorer(ctx);
        } else {
            self.close_requested.get_or_insert(now);
        }
        self.session_dirty = true;
        self.save_due = Some(now);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    fn exit_background(&mut self, ctx: &egui::Context) {
        self.close_launcher(false);
        if self.close_failed {
            self.allow_close = true;
            ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
            return;
        }
        self.close_requested = Some(Instant::now());
        self.session_dirty = true;
        self.save_due = Some(Instant::now());
        ctx.request_repaint();
    }

    pub(super) fn service_launcher(&mut self, ctx: &egui::Context) {
        let events = self.launcher.native.take_events();
        if events & native::ACTIVATE != 0 {
            for activation in self.launcher.native.take_activations() {
                match activation {
                    Activation::StayInTray => {}
                    Activation::Explorer => {
                        self.close_launcher(false);
                        self.show_explorer(ctx);
                    }
                    Activation::Launcher => self.open_launcher(false),
                    Activation::Settings => self.open_launcher(true),
                    Activation::Path(path) => {
                        self.close_launcher(false);
                        self.show_explorer(ctx);
                        self.new_tab(path.clone());
                        self.action(Action::GoPath(path));
                    }
                }
            }
        }
        if events & native::TOGGLE != 0 {
            if self.options_popup.is_recording_launcher_shortcut() {
                self.options_popup
                    .record_active_launcher_shortcut(&self.launcher.settings.hotkey);
            } else if self.launcher_open {
                self.close_launcher(true);
            } else {
                self.open_launcher(false);
            }
        }
        if events & native::SETTINGS != 0 {
            self.open_launcher(true);
        }
        if events & native::EXPLORER != 0 {
            self.close_launcher(false);
            self.show_explorer(ctx);
        }
        if events & native::EXIT != 0 {
            self.exit_background(ctx);
        }
        self.launcher.settle_launcher_shortcut();
        if events & native::STATUS != 0 && self.launcher.launcher_settings_error().is_some() {
            // A failed tray/hotkey must never leave an unreachable hidden app.
            if !self.explorer_visible {
                self.show_explorer(ctx);
            }
            if !self.options {
                self.open_launcher_options();
            }
        }
        if self.launcher.poll() == Some(true) {
            self.close_launcher(true);
        }
        if self.launcher.settings_changed {
            self.launcher.settings_changed = false;
            self.session_dirty = true;
            self.save_due = Some(Instant::now());
            if !self.launcher.settings.background && !self.explorer_visible {
                self.show_explorer(ctx);
            }
        }
    }

    pub(super) fn launcher_viewport(&mut self, ctx: &egui::Context) {
        if !self.launcher_open && !self.launcher_initialized {
            return;
        }
        let title = format!("Foldex Launcher · {}", std::process::id());
        let compact = self.benchmark.as_ref().is_some_and(|run| {
            run.launcher.as_ref().is_some_and(|run| run.compact)
                || run.fixes.as_ref().is_some_and(|run| run.compact)
                || run.keyboard.as_ref().is_some_and(|run| run.compact)
        });
        let size = self.launcher.render_size(ctx, &self.theme, compact);
        let builder = egui::ViewportBuilder::default()
            .with_title(&title)
            .with_decorations(false)
            .with_taskbar(false)
            .with_resizable(false)
            .with_minimize_button(false)
            .with_maximize_button(false)
            .with_always_on_top()
            .with_inner_size(size)
            .with_visible(self.launcher_open && self.launcher_positioned)
            .with_active(false);
        if !self.launcher_open {
            // Keep the native window for fast reopening without coupling the
            // explorer's repaint loop to a hidden immediate child viewport.
            ctx.show_viewport_deferred(
                egui::ViewportId::from_hash_of("foldex-focused-launcher"),
                builder,
                // Native hide already ran before clear. Sending a visibility
                // command here would request another hidden child repaint.
                |_, _| {},
            );
            return;
        }
        let theme = self.theme.clone();
        let mut action = None;
        let mut lost_focus = false;
        let mut captured = None;
        let mut first_visible = false;
        let mut placement_failed = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("foldex-focused-launcher"),
            builder,
            |ui, _| {
                let child = ui.ctx().clone();
                if !self.launcher_open {
                    native::hide_launcher(&title);
                    child.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    return;
                }
                // Child viewports share fonts/textures with the root. Apply only
                // design-system style changes; never replace the font atlas.
                apply_launcher_theme(ui, &theme);
                captured = child.input(|input| {
                    input.events.iter().find_map(|event| match event {
                        egui::Event::Screenshot {
                            user_data, image, ..
                        } => user_data
                            .data
                            .as_ref()
                            .and_then(|value| value.downcast_ref::<String>())
                            .map(|label| (label.clone(), image.clone())),
                        _ => None,
                    })
                });
                if !self.launcher_positioned {
                    // The child is born hidden. Place the owned native window before
                    // its first visibility, including every secondary-monitor run.
                    native::hide_launcher(&title);
                    self.launcher_positioned = native::place_launcher(
                        &title,
                        self.launcher.previous(),
                        self.launcher_secondary,
                    );
                    first_visible = self.launcher_positioned;
                    if !self.launcher_positioned {
                        let attempts = child.data_mut(|data| {
                            let attempts = data.get_temp_mut_or_default::<u8>(Id::new(
                                "launcher-placement-attempts",
                            ));
                            *attempts = attempts.saturating_add(1);
                            *attempts
                        });
                        if attempts < 3 {
                            child.request_repaint_after(Duration::from_millis(16));
                        } else {
                            placement_failed = true;
                        }
                        child.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                        return;
                    }
                }
                let focused = child.input(|input| input.focused);
                if focused {
                    self.launcher_focused = true;
                }
                lost_focus = self.benchmark.is_none() && self.launcher_focused && !focused;
                let escape = self.launcher.take_dismiss_key(&child);
                if escape || child.input(|input| input.viewport().close_requested()) || lost_focus {
                    // The immediate renderer can present before this outer
                    // function handles `action`. Never return empty shapes
                    // while the native window is still visible.
                    native::hide_launcher(&title);
                    child.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    first_visible = false;
                    if !lost_focus {
                        action = Some(ResultAction::Builtin(Builtin::Dismiss));
                    }
                } else {
                    let begin = Instant::now();
                    if let Some(next_action) =
                        self.launcher.ui_with_assets(ui, &theme, &mut self.assets)
                    {
                        action = Some(next_action);
                    }
                    if matches!(
                        action,
                        Some(ResultAction::Builtin(Builtin::Dismiss | Builtin::Settings))
                    ) {
                        native::hide_launcher(&title);
                        child.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                        first_visible = false;
                    } else if first_visible {
                        // In the vendored immediate renderer visibility is
                        // applied after painting this complete content pass.
                        child.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        if self.benchmark.is_none() {
                            child.send_viewport_cmd(egui::ViewportCommand::Focus);
                        }
                    }
                    if let Some(run) = self
                        .benchmark
                        .as_mut()
                        .and_then(|run| run.launcher.as_mut())
                    {
                        if run.ui_ms.len() < 4096 {
                            run.ui_ms.push(begin.elapsed().as_secs_f64() * 1000.0);
                        }
                        if !run.capture_sent
                            && let Some(label) = run.capture
                        {
                            child.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                                egui::UserData::new(label.to_owned()),
                            ));
                            run.capture_sent = true;
                        }
                    }
                }
            },
        );
        self.launcher_initialized = true;
        if placement_failed {
            self.close_launcher(false);
            if self.benchmark.is_none() {
                self.show_explorer(ctx);
                self.toast("Windows could not position the launcher", true);
            }
            return;
        }
        if first_visible && self.benchmark.is_none() {
            native::focus_launcher(&title);
        }
        if self.launcher.settings_changed {
            ctx.request_repaint();
        }
        if let Some((label, image)) = captured {
            self.launcher_capture(&label, &image);
            self.fixes_capture(&label, &image);
        }
        if lost_focus && action.is_none() {
            self.close_launcher(false);
        }
        if let Some(action) = action {
            match action {
                ResultAction::Open(path) => {
                    self.close_launcher(false);
                    self.job(Job::Open(path));
                }
                ResultAction::Application(identifier) => {
                    self.close_launcher(false);
                    self.job(Job::Application(identifier));
                }
                ResultAction::Browse(path) => {
                    self.close_launcher(false);
                    self.show_explorer(ctx);
                    self.new_tab(path);
                }
                ResultAction::Reveal(path) => {
                    if let Some((folder, selection)) = launcher_reveal_location(&path) {
                        self.close_launcher(false);
                        self.show_explorer(ctx);
                        self.new_tab(folder);
                        if let Some(name) = selection {
                            let pane = self.pane_mut();
                            pane.pending_selection = Some(name);
                            pane.query.show_hidden = true;
                        }
                        crate::keyboard::request_id(
                            ctx,
                            crate::keyboard::listing_id(self.pane().id),
                        );
                    } else {
                        self.launcher.status = "That result has no folder location".into();
                    }
                }
                ResultAction::Window(target) => {
                    if crate::launcher::Launcher::activate_window(target) {
                        self.close_launcher(false);
                    } else {
                        self.launcher.status = "That window is no longer available".into();
                    }
                }
                ResultAction::Copy(text) => self.launcher.copy(text),
                ResultAction::Builtin(command) => match command {
                    Builtin::Explorer => {
                        self.close_launcher(false);
                        self.show_explorer(ctx);
                    }
                    Builtin::Settings => self.open_launcher_options(),
                    Builtin::Files => self.launcher.set_mode(Mode::Files),
                    Builtin::Apps => self.launcher.set_mode(Mode::Apps),
                    Builtin::Windows => self.launcher.set_mode(Mode::Windows),
                    Builtin::Clipboard => self.launcher.set_mode(Mode::Clipboard),
                    Builtin::Calculator => self.launcher.set_mode(Mode::Calculator),
                    Builtin::Exit => self.exit_background(ctx),
                    Builtin::Dismiss => self.close_launcher(true),
                },
            }
            ctx.request_repaint();
        }
    }
}

/// Folder hits reveal their entry in the parent just like file hits. Volume
/// and share roots have no selectable parent entry, so they open directly.
fn launcher_reveal_location(
    path: &std::path::Path,
) -> Option<(PathBuf, Option<std::ffi::OsString>)> {
    if !path.is_absolute() {
        return None;
    }
    Some(match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => (parent.to_path_buf(), Some(name.to_owned())),
        _ => (path.to_path_buf(), None),
    })
}

fn apply_launcher_theme(ui: &mut Ui, theme: &Theme) {
    let color = |rgb: [u8; 3]| u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 255]);
    let key = [
        color(theme.background),
        color(theme.sidebar),
        color(theme.chrome),
        color(theme.surface),
        color(theme.hover),
        color(theme.border),
        color(theme.text),
        color(theme.muted),
        color(theme.accent),
        color(theme.selection),
        color(theme.folder),
        color(theme.danger),
        theme.font_size.to_bits(),
        theme.inspector_font_size.to_bits(),
        theme.row_height.to_bits(),
        theme.sidebar_width.to_bits(),
        u32::from(theme.radius),
        theme.gap.to_bits(),
        theme.duration().to_bits(),
    ];
    let id = Id::new("launcher-applied-theme");
    if ui.ctx().data(|data| data.get_temp::<[u32; 19]>(id)) != Some(key) {
        theme.apply(ui.ctx());
        ui.ctx().data_mut(|data| data.insert_temp(id, key));
    }
    // The child Ui was created before the callback; refresh its style snapshot.
    ui.set_style(ui.ctx().global_style());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_reveal_uses_the_parent_and_native_name_for_both_file_and_folder_hits() {
        let parent = std::env::temp_dir().join("foldex-launcher-reveal-fixture");
        for name in ["report.txt", "Folder with spaces", ".hidden-target"] {
            let path = parent.join(name);
            assert_eq!(
                launcher_reveal_location(&path),
                Some((parent.clone(), Some(std::ffi::OsString::from(name))))
            );
        }
        assert!(launcher_reveal_location(std::path::Path::new("relative/file.txt")).is_none());
        assert!(launcher_reveal_location(std::path::Path::new("")).is_none());
    }

    #[test]
    fn launcher_reveal_roots_open_directly_and_trailing_folder_separators_keep_the_parent() {
        let path = std::env::temp_dir().join("foldex-launcher-reveal-fixture");
        let root = path.ancestors().last().unwrap();
        assert_eq!(
            launcher_reveal_location(root),
            Some((root.to_path_buf(), None))
        );
        let mut folder = path.clone().into_os_string();
        folder.push(std::path::MAIN_SEPARATOR.to_string());
        assert_eq!(
            launcher_reveal_location(std::path::Path::new(&folder)),
            Some((
                path.parent().unwrap().to_path_buf(),
                path.file_name().map(ToOwned::to_owned)
            ))
        );
    }

    #[cfg(windows)]
    #[test]
    fn launcher_reveal_keeps_unc_verbatim_and_non_unicode_paths_lossless() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        for (path, parent) in [
            (r"\\server\share\folder\file.txt", r"\\server\share\folder"),
            (r"\\?\C:\folder\file.txt", r"\\?\C:\folder"),
        ] {
            assert_eq!(
                launcher_reveal_location(std::path::Path::new(path)),
                Some((PathBuf::from(parent), Some("file.txt".into())))
            );
        }
        let root = PathBuf::from(r"\\server\share\");
        assert_eq!(launcher_reveal_location(&root), Some((root, None)));
        let name = std::ffi::OsString::from_wide(&[b'f' as u16, 0xd800, b'x' as u16]);
        let parent = PathBuf::from(r"C:\folder");
        let (_, selected) = launcher_reveal_location(&parent.join(&name)).unwrap();
        assert_eq!(
            selected.unwrap().encode_wide().collect::<Vec<_>>(),
            name.encode_wide().collect::<Vec<_>>()
        );
    }

    #[test]
    fn launcher_child_refreshes_current_theme_tokens_after_preset_changes() {
        let ctx = egui::Context::default();
        for theme in [Theme::default(), Theme::paper(), Theme::midnight()] {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
                apply_launcher_theme(ui, &theme);
                assert_eq!(ui.visuals().window_fill, Theme::color(theme.surface));
                assert_eq!(
                    ui.visuals().selection.bg_fill,
                    Theme::color(theme.selection)
                );
                assert_eq!(
                    ui.visuals().widgets.hovered.bg_fill,
                    Theme::color(theme.hover)
                );
                assert_eq!(ui.visuals().override_text_color, Some(theme.fg()));
            });
            output.textures_delta.clear(); // Headless test intentionally has no renderer.
        }
    }
}
