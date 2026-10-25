//! Launcher preferences live in Options. Recording consumes only the explorer's
//! own event stream; it never installs an input hook or temporarily owns OS keys.
use super::*;
use crate::launcher::{Launcher, Settings, native};
#[derive(Default)]
pub(super) struct LauncherOptions {
    pub(super) open: bool,
    recording: bool,
    super_down: bool,
    modifiers: Modifiers,
    candidate: Option<String>,
    preview: String,
    roots: String,
    error: Option<String>,
    message: Option<String>,
    awaiting: Option<String>,
    apply_requested: bool,
    editor: Option<OptionId>,
    anchors: [Option<Rect>; 2],
    seen: [u64; 2],
    roots_error: Option<String>,
    roots_message: Option<String>,
    history_cleared: bool,
    pending: bool,
    focus_editor: bool,
    review_focus: bool,
}
#[cfg(test)]
mod tests {
    use super::*;
    fn key(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    #[test]
    fn launcher_recorder_previews_real_chord_consumes_text_and_enter_requests_apply() {
        let mut options = LauncherOptions {
            open: true,
            recording: true,
            ..Default::default()
        };
        let mut input = egui::RawInput {
            focused: true,
            events: vec![
                key(Key::K, Modifiers::CTRL | Modifiers::SHIFT),
                egui::Event::Text("K".into()),
                egui::Event::Copy,
            ],
            ..Default::default()
        };
        options.capture(&mut input);
        assert_eq!(options.candidate.as_deref(), Some("Ctrl+Shift+K"));
        assert!(input.events.is_empty());
        assert!(options.recording);
        assert!(!options.apply_requested);
        input.events.push(key(Key::Enter, Modifiers::NONE));
        options.capture(&mut input);
        assert!(options.apply_requested);
        assert!(input.events.is_empty());
    }
    #[test]
    fn launcher_recorder_escape_and_focus_loss_cancel_without_applying_or_leaking_keys() {
        let mut options = LauncherOptions {
            open: true,
            recording: true,
            candidate: Some("Ctrl+Alt+K".into()),
            ..Default::default()
        };
        let mut input = egui::RawInput {
            focused: true,
            events: vec![key(Key::Escape, Modifiers::NONE)],
            ..Default::default()
        };
        options.capture(&mut input);
        assert!(!options.recording);
        assert!(options.candidate.is_none());
        assert!(!options.apply_requested);
        assert!(input.events.is_empty());
        options.recording = true;
        options.candidate = Some("Ctrl+Alt+K".into());
        input.focused = false;
        options.capture(&mut input);
        assert!(!options.recording);
        assert!(options.candidate.is_none());
        assert!(!options.apply_requested);
    }
    #[test]
    fn launcher_recorder_rejects_plain_keys_ignores_repeat_and_tracks_windows_modifier() {
        let mut options = LauncherOptions {
            open: true,
            recording: true,
            ..Default::default()
        };
        let mut input = egui::RawInput {
            focused: true,
            events: vec![key(Key::K, Modifiers::NONE)],
            ..Default::default()
        };
        options.capture(&mut input);
        assert!(options.candidate.is_none());
        assert!(options.error.is_some());
        input.events = vec![
            key(Key::SuperLeft, Modifiers::NONE),
            key(Key::K, Modifiers::NONE),
        ];
        options.capture(&mut input);
        assert_eq!(options.candidate.as_deref(), Some("Win+K"));
        input.events = vec![egui::Event::Key {
            key: Key::A,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::CTRL,
        }];
        options.capture(&mut input);
        assert_eq!(options.candidate.as_deref(), Some("Win+K"));
        assert!(input.events.is_empty());
    }
    #[test]
    fn launcher_recorder_closed_options_leave_normal_input_untouched() {
        let mut popup = OptionsPopup::default();
        let mut input = egui::RawInput {
            focused: true,
            events: vec![key(Key::A, Modifiers::CTRL), egui::Event::Text("a".into())],
            ..Default::default()
        };
        popup.capture_launcher_shortcut(&mut input);
        assert_eq!(input.events.len(), 2);
        popup.open_launcher_section(&Settings::default());
        assert!(popup.active);
        assert_eq!(popup.rows.len(), 5);
        assert!(
            popup
                .rows
                .iter()
                .all(|(index, _)| is_launcher_option(OPTIONS[*index].id))
        );
        popup.begin_launcher_shortcut_recording();
        popup.capture_launcher_shortcut(&mut input);
        assert_eq!(
            popup.launcher_options_diagnostics().2.as_deref(),
            Some("Ctrl+A")
        );
        assert!(input.events.is_empty());
    }
    #[test]
    fn launcher_recorder_modifier_preview_uses_real_events_across_frames() {
        let mut options = LauncherOptions {
            open: true,
            recording: true,
            ..Default::default()
        };
        let mut input = egui::RawInput {
            events: vec![egui::Event::ModifiersChanged(
                Modifiers::CTRL | Modifiers::ALT,
            )],
            ..Default::default()
        };
        options.capture(&mut input);
        assert_eq!(options.preview, "Ctrl+Alt");
        assert!(options.candidate.is_none());
        input.events.clear();
        options.capture(&mut input);
        assert_eq!(options.preview, "Ctrl+Alt");
        input
            .events
            .push(key(Key::K, Modifiers::CTRL | Modifiers::ALT));
        options.capture(&mut input);
        assert_eq!(options.candidate.as_deref(), Some("Ctrl+Alt+K"));
        input
            .events
            .push(egui::Event::ModifiersChanged(Modifiers::NONE));
        options.capture(&mut input);
        assert!(options.preview.is_empty());
        assert_eq!(options.candidate.as_deref(), Some("Ctrl+Alt+K"));
    }

    #[test]
    fn launcher_recorder_tab_reviews_controls_without_replacing_the_candidate() {
        let mut options = LauncherOptions {
            open: true,
            recording: true,
            candidate: Some("Ctrl+Alt+K".into()),
            ..Default::default()
        };
        let mut input = egui::RawInput {
            focused: true,
            events: vec![
                key(Key::Tab, Modifiers::NONE),
                key(Key::Tab, Modifiers::SHIFT),
            ],
            ..Default::default()
        };
        options.capture(&mut input);
        assert_eq!(input.events.len(), 2);
        assert_eq!(options.candidate.as_deref(), Some("Ctrl+Alt+K"));
        assert!(options.error.is_none());
        options.review_focus = true;
        input.events = vec![
            key(Key::Enter, Modifiers::NONE),
            key(Key::Space, Modifiers::NONE),
            egui::Event::Text(" ".into()),
        ];
        options.capture(&mut input);
        assert_eq!(input.events.len(), 2);
        assert!(!options.apply_requested);
        assert_eq!(options.candidate.as_deref(), Some("Ctrl+Alt+K"));
    }

    #[test]
    fn launcher_recorder_consumes_ime_and_clipboard_aliases_of_captured_chords() {
        let mut options = LauncherOptions {
            open: true,
            recording: true,
            ..Default::default()
        };
        let mut input = egui::RawInput {
            focused: true,
            events: vec![
                key(Key::V, Modifiers::CTRL),
                egui::Event::Paste("private".into()),
                egui::Event::Ime(egui::ImeEvent::Commit("text".into())),
            ],
            ..Default::default()
        };
        options.capture(&mut input);
        assert!(input.events.is_empty());
        assert_eq!(options.candidate.as_deref(), Some("Ctrl+V"));
    }
}
fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::ShiftLeft
            | Key::ShiftRight
            | Key::SuperLeft
            | Key::SuperRight
    )
}
fn modifiers_text(modifiers: Modifiers, super_down: bool) -> String {
    let mut names = Vec::with_capacity(4);
    if modifiers.ctrl {
        names.push("Ctrl");
    }
    if modifiers.alt {
        names.push("Alt");
    }
    if modifiers.shift {
        names.push("Shift");
    }
    if super_down || modifiers.mac_cmd {
        names.push("Win");
    }
    names.join("+")
}
fn recorded_chord(key: Key, modifiers: Modifiers, super_down: bool) -> Option<String> {
    let prefix = modifiers_text(modifiers, super_down);
    if prefix.is_empty() || is_modifier(key) {
        return None;
    }
    let name = match key {
        Key::Minus => "Minus",
        Key::Equals | Key::Plus => "Equals",
        Key::Comma => "Comma",
        Key::Period => "Period",
        Key::Slash => "Slash",
        Key::Backslash => "Backslash",
        Key::Semicolon => "Semicolon",
        Key::Quote => "Quote",
        Key::Backtick => "Backtick",
        Key::OpenBracket => "OpenBracket",
        Key::CloseBracket => "CloseBracket",
        _ => key.name(),
    };
    let chord = format!("{prefix}+{name}");
    native::parse_hotkey(&chord).map(|_| chord)
}
impl LauncherOptions {
    pub(super) fn is_recording(&self) -> bool {
        self.recording
    }

    pub(super) fn editor_open(&self) -> bool {
        self.editor.is_some()
    }
    pub(super) fn close_editor(&mut self) {
        self.cancel_recording();
        self.editor = None;
    }
    pub(super) fn status(&self, option: OptionId) -> Option<(&str, bool)> {
        match option {
            OptionId::LauncherShortcut => self
                .error
                .as_deref()
                .map(|s| (s, true))
                .or_else(|| self.pending.then_some(("Registering shortcut…", false)))
                .or_else(|| self.message.as_deref().map(|s| (s, false))),
            OptionId::LauncherFolders => self
                .roots_error
                .as_deref()
                .map(|s| (s, true))
                .or_else(|| self.roots_message.as_deref().map(|s| (s, false))),
            OptionId::LauncherHistory if self.history_cleared => {
                Some(("Text history cleared.", false))
            }
            _ => None,
        }
    }
    fn cancel_recording(&mut self) {
        self.recording = false;
        self.super_down = false;
        self.modifiers = Modifiers::NONE;
        self.candidate = None;
        self.preview.clear();
        self.apply_requested = false;
        self.review_focus = false;
        if self.editor == Some(OptionId::LauncherShortcut) {
            self.editor = None;
        }
    }
    fn capture(&mut self, input: &mut egui::RawInput) {
        if !self.recording {
            return;
        }
        if !input.focused {
            self.cancel_recording();
            return;
        }
        self.preview = modifiers_text(self.modifiers, self.super_down);
        for event in input.events.iter().take(64) {
            if let egui::Event::ModifiersChanged(modifiers) = event {
                self.modifiers = *modifiers;
                self.preview = modifiers_text(*modifiers, self.super_down);
                continue;
            }
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
            } = event
            else {
                continue;
            };
            self.modifiers = *modifiers;
            if matches!(key, Key::SuperLeft | Key::SuperRight) {
                self.super_down = *pressed;
            }
            self.preview = modifiers_text(*modifiers, self.super_down);
            if !pressed || *repeat || is_modifier(*key) {
                continue;
            }
            if *key == Key::Escape {
                self.cancel_recording();
                break;
            }
            // Tab belongs to the recorder's review controls. Enter/Space on a
            // focused button are also ordinary activation after a chord exists.
            // Modified Tab remains recordable as a real shortcut.
            if *key == Key::Tab
                && !modifiers.ctrl
                && !modifiers.alt
                && !modifiers.command
                && !modifiers.mac_cmd
                && !self.super_down
            {
                self.review_focus = true;
                continue;
            }
            if self.review_focus && modifiers.is_none() && matches!(key, Key::Enter | Key::Space) {
                continue;
            }
            if *key == Key::Enter && modifiers.is_none() && self.candidate.is_some() {
                self.apply_requested = true;
                break;
            }
            if let Some(chord) =
                recorded_chord(physical_key.unwrap_or(*key), *modifiers, self.super_down)
            {
                self.preview.clone_from(&chord);
                self.candidate = Some(chord);
                self.error = None;
            } else {
                self.candidate = None;
                self.error = Some("Include Ctrl, Alt, Shift or Win and one supported key.".into());
            }
        }
        // Text and semantic clipboard aliases from a recorded chord must not
        // leak into the Options search box, folder editor, or explorer commands.
        input.events.retain(|event| {
            if matches!(event, egui::Event::Key { key: Key::Tab, modifiers, .. }
                if !modifiers.ctrl && !modifiers.alt && !modifiers.command && !modifiers.mac_cmd)
            {
                return true;
            }
            if self.review_focus
                && matches!(event, egui::Event::Key {
                key: Key::Enter | Key::Space, modifiers, .. } if modifiers.is_none())
            {
                return true;
            }
            !matches!(
                event,
                egui::Event::Key { .. }
                    | egui::Event::Text(_)
                    | egui::Event::Paste(_)
                    | egui::Event::Copy
                    | egui::Event::Cut
                    | egui::Event::Ime(_)
            )
        });
    }
    fn apply(&mut self, launcher: &mut Launcher) {
        let Some(candidate) = self.candidate.clone() else {
            return;
        };
        match launcher.apply_launcher_shortcut(&candidate) {
            Ok(()) => {
                self.recording = false;
                self.super_down = false;
                self.editor = None;
                self.awaiting = Some(candidate);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        self.apply_requested = false;
    }
    fn settle(&mut self, launcher: &mut Launcher) {
        launcher.settle_launcher_shortcut();
        let feedback = launcher.native.hotkey_feedback();
        self.pending = feedback.pending;
        if let Some(wanted) = &self.awaiting {
            if feedback.requested == *wanted && !feedback.pending {
                self.error = feedback.error;
                if self.error.is_none() {
                    self.message = Some("Shortcut saved".into());
                    self.candidate = None;
                }
                self.awaiting = None;
            }
        }
    }
}
impl OptionsPopup {
    pub(in crate::app) fn close(&mut self) {
        self.closing = true;
        self.launcher_options.cancel_recording();
    }
    pub(in crate::app) fn open_launcher_section(&mut self, settings: &Settings) {
        self.open(0);
        self.launcher_options = LauncherOptions::from_settings(settings);
        self.query = "launcher".into();
        self.refilter();
        self.selected = Some(OptionId::LauncherShortcut);
    }
    pub(in crate::app) fn is_recording_launcher_shortcut(&self) -> bool {
        self.launcher_options.open && self.launcher_options.recording
    }
    pub(in crate::app) fn capture_launcher_shortcut(&mut self, input: &mut egui::RawInput) {
        if self.launcher_options.open {
            self.launcher_options.capture(input);
        }
    }
    /// Native harnesses feed this recorder through the app's RawInput only.
    pub(in crate::app) fn begin_launcher_shortcut_recording(&mut self) {
        if self.launcher_options.open {
            self.launcher_options.cancel_recording();
            self.launcher_options.recording = true;
            self.launcher_options.editor = Some(OptionId::LauncherShortcut);
            self.launcher_options.focus_editor = true;
            self.selected = Some(OptionId::LauncherShortcut);
            self.focus = false;
            self.launcher_options.error = None;
            self.launcher_options.message = None;
        }
    }
    pub(in crate::app) fn launcher_options_diagnostics(
        &self,
    ) -> (bool, bool, Option<String>, Option<String>) {
        (
            self.launcher_options.open
                && self.active
                && self
                    .rows
                    .iter()
                    .filter(|(index, _)| is_launcher_option(OPTIONS[*index].id))
                    .count()
                    == 5,
            self.launcher_options.recording,
            self.launcher_options.candidate.clone(),
            self.launcher_options.error.clone(),
        )
    }
    /// The existing global binding is delivered by WM_HOTKEY instead of egui.
    /// While recording, use its known chord as the preview and keep Options open.
    pub(in crate::app) fn record_active_launcher_shortcut(&mut self, shortcut: &str) {
        if self.is_recording_launcher_shortcut() {
            self.launcher_options.candidate = Some(shortcut.to_owned());
            self.launcher_options.preview = shortcut.to_owned();
            self.launcher_options.error = None;
        }
    }
    pub(in crate::app) fn ui_with_launcher(
        &mut self,
        ctx: &egui::Context,
        theme: &mut Theme,
        launcher: &mut Launcher,
    ) -> OptionsResponse {
        // prepare_open can make the surface active before live services are
        // supplied. Initialize preferences on their first render as well.
        if !self.active || !self.launcher_options.open {
            self.launcher_options = LauncherOptions::from_settings(&launcher.settings);
        }
        self.launcher_options.settle(launcher);
        if self.launcher_options.error.is_none() {
            // Shortcut registration feedback belongs to the shortcut row. A
            // provider/clipboard/tray failure must not masquerade as a chord
            // conflict or escape into an unrelated explorer error surface.
            self.launcher_options.error = launcher.launcher_shortcut_error();
        }
        if self.launcher_options.apply_requested {
            self.launcher_options.apply(launcher);
        }
        self.ui_impl(ctx, theme, Some(launcher))
    }
    pub(in crate::app) fn ui_with_services(
        &mut self,
        ctx: &egui::Context,
        theme: &mut Theme,
        launcher: &mut Launcher,
        sidebar: &mut crate::sidebar::Preferences,
    ) -> OptionsResponse {
        self.sidebar_preferences = sidebar.clone();
        let result = self.ui_with_launcher(ctx, theme, launcher);
        if result.sidebar_changed {
            *sidebar = self.sidebar_preferences.clone();
        }
        result
    }
    pub(super) fn launcher_control(
        &mut self,
        ui: &mut Ui,
        theme: &Theme,
        rect: Rect,
        option: OptionId,
        activate: bool,
        opacity: f32,
        launcher: &mut Launcher,
    ) {
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(("option-control", option))
                .max_rect(rect),
        );
        child.multiply_opacity(opacity);
        let ui = &mut child;
        ui.spacing_mut().interact_size.y = theme.metrics().control;
        if matches!(
            option,
            OptionId::LauncherBackground | OptionId::LauncherClipboard
        ) {
            let mut value = if option == OptionId::LauncherBackground {
                launcher.settings.background
            } else {
                launcher.settings.clipboard_history
            };
            let response = foldex_ui::components::checkbox(ui, theme, &mut value, "");
            self.control_focus(ui, option, &response);
            if activate {
                value = !value;
            }
            if response.changed() || activate {
                let (background, clipboard) = if option == OptionId::LauncherBackground {
                    (value, launcher.settings.clipboard_history)
                } else {
                    (launcher.settings.background, value)
                };
                launcher.set_launcher_flags(background, clipboard);
                self.selected = Some(option);
            }
            return;
        }
        let label = match option {
            OptionId::LauncherShortcut => {
                if launcher.settings.hotkey.is_empty() {
                    "Disabled"
                } else {
                    &launcher.settings.hotkey
                }
            }
            OptionId::LauncherFolders => "Configure…",
            OptionId::LauncherHistory => "Clear history",
            _ => return,
        };
        let response = foldex_ui::components::text_button_at(
            ui,
            rect,
            ui.id().with("launcher-setting"),
            theme,
            label,
            false,
        );
        self.control_focus(ui, option, &response);
        if matches!(
            option,
            OptionId::LauncherShortcut | OptionId::LauncherFolders
        ) {
            self.launcher_options.anchors[usize::from(option == OptionId::LauncherFolders)] =
                Some(rect);
            self.launcher_options.seen[usize::from(option == OptionId::LauncherFolders)] =
                ui.ctx().cumulative_frame_nr();
        }
        if response.clicked() || activate {
            self.selected = Some(option);
            self.child_return_focus = Some(response.id);
            match option {
                OptionId::LauncherShortcut => {
                    self.begin_launcher_shortcut_recording();
                    if let Some(focused) = ui.memory(|memory| memory.focused()) {
                        ui.memory_mut(|memory| memory.surrender_focus(focused));
                    }
                }
                OptionId::LauncherFolders => {
                    self.launcher_options.close_editor();
                    self.launcher_options.editor = Some(option);
                    self.launcher_options.focus_editor = true;
                    self.launcher_options.roots = roots_text(&launcher.settings);
                    self.launcher_options.roots_error = None;
                    self.focus = false;
                }
                OptionId::LauncherHistory => {
                    self.child_return_focus = None;
                    launcher.clear_text_history();
                    self.launcher_options.history_cleared = true;
                }
                _ => {}
            }
            ui.ctx().request_repaint();
        }
        response.on_hover_text(match option {
            OptionId::LauncherShortcut => "Configure the launcher shortcut",
            OptionId::LauncherFolders => "Choose launcher file search folders",
            _ => "Clear text history stored in memory",
        });
    }
    pub(super) fn launcher_editor_ui(
        &mut self,
        ctx: &egui::Context,
        theme: &Theme,
        parent: Id,
        body: Rect,
        launcher: &mut Launcher,
    ) {
        let Some(editor) = self.launcher_options.editor else {
            return;
        };
        let Some(anchor) =
            self.launcher_options.anchors[usize::from(editor == OptionId::LauncherFolders)]
        else {
            return;
        };
        if self.closing
            || !body.contains(anchor.center())
            || self.launcher_options.seen[usize::from(editor == OptionId::LauncherFolders)]
                != ctx.cumulative_frame_nr()
        {
            self.launcher_options.close_editor();
            return;
        }
        let scale = theme.metrics().scale;
        let width = (if editor == OptionId::LauncherShortcut {
            328.0
        } else {
            440.0
        }) * scale;
        let width = width.min((ctx.content_rect().width() - 32.0 * scale).max(1.0));
        let mut open = true;
        let mut done = false;
        egui::Popup::new(parent.with("launcher-editor"), ctx.clone(), anchor,
            egui::LayerId::new(egui::Order::Foreground, parent))
            .open_bool(&mut open)
            .width(width)
            .gap(4.0 * scale)
            .frame(egui::Frame::new().shadow(egui::epaint::Shadow { offset: [0, 4], blur: 18, spread: 0, color: Color32::from_black_alpha(85) })
                .fill(Theme::color(theme.surface))
                .stroke(Stroke::new(1.0, theme.line()))
                .corner_radius(theme.radius)
                .inner_margin(10.0 * scale))
            .show(|ui| {
                ui.set_width((width - 20.0 * scale).max(1.0));
                ui.spacing_mut().item_spacing = vec2(6.0, 8.0) * scale;
                if editor == OptionId::LauncherShortcut {
                    self.launcher_options.review_focus = false;
                    ui.label(egui::RichText::new("Launcher shortcut").font(theme.font()));
                    let preview = self.launcher_options.candidate.as_deref()
                        .map(|key| if key.is_empty() { "Disabled" } else { key })
                        .unwrap_or_else(|| if self.launcher_options.preview.is_empty() {
                            "Press a shortcut…"
                        } else { &self.launcher_options.preview });
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), theme.metrics().control + 8.0 * scale), Sense::hover());
                    let recorder = foldex_ui::components::PopupFocus::surface(ui, rect,
                        parent.with("launcher-recorder"), "Record a launcher shortcut");
                    if self.launcher_options.focus_editor && foldex_ui::components::can_focus(ui) {
                        recorder.request_focus();
                        self.launcher_options.focus_editor = ui.is_sizing_pass() || !recorder.has_focus();
                    }
                    ui.painter().rect_filled(rect, theme.radius, Theme::color(theme.chrome));
                    ui.painter().rect_stroke(rect, theme.radius,
                        Stroke::new(1.0, theme.accent()), StrokeKind::Inside);
                    foldex_ui::components::elided_label(ui.painter(),
                        rect.shrink2(vec2(8.0 * scale, 0.0)), theme, preview, theme.fg(), false);
                    ui.horizontal(|ui| {
                        let clear = foldex_ui::components::text_button(ui, theme, "Clear", false);
                        self.launcher_options.review_focus |= clear.has_focus();
                        if clear.clicked() {
                            self.launcher_options.candidate = Some(String::new());
                            self.launcher_options.error = None;
                        }
                        let cancel = foldex_ui::components::text_button(ui, theme, "Cancel", false);
                        self.launcher_options.review_focus |= cancel.has_focus();
                        if cancel.clicked() { done = true; }
                        let ready = self.launcher_options.candidate.is_some()
                            && self.launcher_options.awaiting.is_none();
                        ui.add_enabled_ui(ready, |ui| {
                            let apply = foldex_ui::components::text_button(ui, theme, "Apply", true);
                            self.launcher_options.review_focus |= apply.has_focus();
                            if apply.clicked() {
                                self.launcher_options.apply(launcher);
                            }
                        });
                    });
                    let (hint, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), 24.0 * scale), Sense::hover());
                    foldex_ui::components::popup_hint(ui.painter(), hint, theme,
                        "Enter Apply     Esc Cancel", "Enter Apply     Esc Cancel");
                } else {
                    ui.label(egui::RichText::new("Launcher search folders").font(theme.font()));
                    ui.add(egui::Label::new(egui::RichText::new(
                        "Up to two absolute paths, separated by a semicolon. Leave empty for the default folders.")
                        .font(theme.font()).color(theme.dim())).wrap());
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), theme.metrics().control), Sense::hover());
                    let edit = foldex_ui::components::input_box(ui, rect, parent.with("launcher-roots"), theme,
                        &mut self.launcher_options.roots, "Use default search folders", 4096);
                    if self.launcher_options.focus_editor && foldex_ui::components::can_focus(ui) {
                        edit.response.request_focus();
                        self.launcher_options.focus_editor = ui.is_sizing_pass() || !edit.response.has_focus();
                    }
                    let submit = edit.response.has_focus() && !self.ime_input
                        && crate::commands::take_key(ctx, Modifiers::NONE, Key::Enter);
                    if let Some(error) = &self.launcher_options.roots_error {
                        ui.add(egui::Label::new(egui::RichText::new(error)
                            .color(Theme::color(theme.danger))).wrap());
                    }
                    ui.horizontal(|ui| {
                        if foldex_ui::components::text_button(ui, theme, "Cancel", false).clicked() { done = true; }
                        if foldex_ui::components::text_button(ui, theme, "Apply", true).clicked() || submit {
                            match launcher.apply_launcher_roots(&self.launcher_options.roots) {
                                Ok(()) => {
                                    self.launcher_options.roots_error = None;
                                    self.launcher_options.roots_message = Some("Search folders saved.".into());
                                    done = true;
                                }
                                Err(error) => self.launcher_options.roots_error = Some(error),
                            }
                        }
                    });
                }
            });
        if !open || done {
            self.launcher_options.close_editor();
            self.return_from_child(ctx);
            ctx.request_repaint();
        }
    }
}
impl LauncherOptions {
    fn from_settings(settings: &Settings) -> Self {
        Self {
            open: true,
            roots: roots_text(settings),
            ..Default::default()
        }
    }
}
fn roots_text(settings: &Settings) -> String {
    settings
        .file_roots
        .iter()
        .map(|path| crate::pane::friendly_path(path))
        .collect::<Vec<_>>()
        .join("; ")
}
pub(super) fn is_launcher_option(option: OptionId) -> bool {
    matches!(
        option,
        OptionId::LauncherShortcut
            | OptionId::LauncherBackground
            | OptionId::LauncherClipboard
            | OptionId::LauncherFolders
            | OptionId::LauncherHistory
    )
}
