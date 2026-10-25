//! Launcher behavior and search preferences. Keyboard shortcuts live in Hotkeys.
use super::*;
use crate::launcher::{Launcher, Settings};

#[derive(Default)]
pub(super) struct LauncherOptions {
    pub(super) open: bool,
    roots: String,
    editor: Option<OptionId>,
    anchor: Option<Rect>,
    seen: u64,
    roots_error: Option<String>,
    roots_message: Option<String>,
    history_cleared: bool,
    focus_editor: bool,
}

impl LauncherOptions {
    pub(super) fn editor_open(&self) -> bool {
        self.editor.is_some()
    }
    pub(super) fn close_editor(&mut self) {
        self.editor = None;
    }
    pub(super) fn status(&self, option: OptionId) -> Option<(&str, bool)> {
        match option {
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
}

impl OptionsPopup {
    pub(in crate::app) fn close(&mut self) {
        self.closing = true;
        self.launcher_options.close_editor();
    }
    pub(in crate::app) fn open_launcher_section(&mut self, settings: &Settings) {
        self.open(0);
        self.launcher_options = LauncherOptions::from_settings(settings);
        self.query = "launcher".into();
        self.refilter();
        self.selected = Some(OptionId::LauncherBackground);
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
        if option == OptionId::LauncherFolders {
            self.launcher_options.anchor = Some(rect);
            self.launcher_options.seen = ui.ctx().cumulative_frame_nr();
        }
        if response.clicked() || activate {
            self.selected = Some(option);
            self.child_return_focus = Some(response.id);
            match option {
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
        if !self.launcher_options.editor_open() {
            return;
        }
        let Some(anchor) = self.launcher_options.anchor else {
            return;
        };
        if self.closing
            || !body.contains(anchor.center())
            || self.launcher_options.seen != ctx.cumulative_frame_nr()
        {
            self.launcher_options.close_editor();
            return;
        }
        let scale = theme.metrics().scale;
        let width = 440.0 * scale;
        let width = width.min((ctx.content_rect().width() - 32.0 * scale).max(1.0));
        let mut open = true;
        let mut done = false;
        egui::Popup::new(
            parent.with("launcher-editor"),
            ctx.clone(),
            anchor,
            egui::LayerId::new(egui::Order::Foreground, parent),
        )
        .open_bool(&mut open)
        .width(width)
        .gap(4.0 * scale)
        .frame(
            egui::Frame::new()
                .shadow(egui::epaint::Shadow {
                    offset: [0, 4],
                    blur: 18,
                    spread: 0,
                    color: Color32::from_black_alpha(85),
                })
                .fill(Theme::color(theme.surface))
                .stroke(Stroke::new(1.0, theme.line()))
                .corner_radius(theme.radius)
                .inner_margin(10.0 * scale),
        )
        .show(|ui| {
            ui.set_width((width - 20.0 * scale).max(1.0));
            ui.spacing_mut().item_spacing = vec2(6.0, 8.0) * scale;
            ui.label(egui::RichText::new("Launcher search folders").font(theme.font()));
            ui.add(
                egui::Label::new(
                    egui::RichText::new("Up to two absolute paths, separated by a semicolon. Leave empty for the default folders.")
                        .font(theme.font())
                        .color(theme.dim()),
                ).wrap(),
            );
            let (rect, _) = ui.allocate_exact_size(
                vec2(ui.available_width(), theme.metrics().control), Sense::hover(),
            );
            let edit = foldex_ui::components::input_box(
                ui, rect, parent.with("launcher-roots"), theme,
                &mut self.launcher_options.roots, "Use default search folders", 4096,
            );
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
                if foldex_ui::components::text_button(ui, theme, "Cancel", false).clicked() {
                    done = true;
                }
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
        OptionId::LauncherBackground
            | OptionId::LauncherClipboard
            | OptionId::LauncherFolders
            | OptionId::LauncherHistory
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_options_contain_behavior_and_folders_without_shortcut_configuration() {
        let mut popup = OptionsPopup::default();
        popup.open_launcher_section(&Settings::default());
        let titles: Vec<_> = popup
            .rows
            .iter()
            .map(|(index, _)| OPTIONS[*index].title)
            .collect();
        assert_eq!(titles.len(), 4);
        assert!(
            titles
                .iter()
                .all(|title| !title.to_ascii_lowercase().contains("shortcut"))
        );
        assert!(
            popup
                .rows
                .iter()
                .all(|(index, _)| is_launcher_option(OPTIONS[*index].id))
        );
        assert_eq!(popup.selected, Some(OptionId::LauncherBackground));
    }
}
