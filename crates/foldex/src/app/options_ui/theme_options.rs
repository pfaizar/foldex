//! Appearance rows use the same label/description and right-hand control layout
//! as the rest of Options. The only color editors remain the active-mode rows.
use super::*;
use foldex_ui::{ColorMode, ColorScheme};

#[derive(Default)]
pub(super) struct AppearanceOptions {
    pub library: crate::theme_store::Library,
    name: String,
    name_error: Option<String>,
    status_option: Option<OptionId>,
    saving: Option<ColorScheme>,
    system_theme: Option<egui::Theme>,
}
impl AppearanceOptions {
    pub(super) fn status(&self, option: OptionId) -> Option<(&str, bool)> {
        if option == OptionId::ThemeName
            && let Some(error) = &self.name_error
        {
            return Some((error, true));
        }
        if self.status_option == Some(option) {
            self.library
                .status
                .as_ref()
                .map(|(text, error)| (text.as_str(), *error))
        } else {
            None
        }
    }
}
impl OptionsPopup {
    pub(in crate::app) fn observe_system_theme(&mut self, theme: Option<egui::Theme>) {
        if theme.is_some() {
            self.appearance.system_theme = theme;
        }
    }
    pub(in crate::app) fn system_theme(&self, ctx: &egui::Context) -> Option<egui::Theme> {
        self.appearance.system_theme.or_else(|| ctx.system_theme())
    }
    pub(in crate::app) fn configure_themes(&mut self, path: std::path::PathBuf, owner: isize) {
        self.appearance.library.configure(path, owner);
    }
    pub(in crate::app) fn persist_theme(
        &mut self,
        ctx: &egui::Context,
        theme: &Theme,
        immediate: bool,
    ) {
        self.appearance.library.persist(ctx, theme, immediate);
    }
    pub(in crate::app) fn flush_theme_preferences(&mut self, ctx: &egui::Context) {
        self.appearance.library.flush(ctx);
    }
    pub(in crate::app) fn theme_preferences_saved(&self) -> bool {
        self.appearance.library.preferences_saved()
    }
    pub(in crate::app) fn theme_preferences_error(&self) -> Option<&str> {
        self.appearance.library.preferences_error()
    }
    pub(in crate::app) fn theme_palette_changed(&mut self, theme: &mut Theme) {
        if self.color_picker.is_open()
            && let Some(option) = self.color_option
            && let Some(value) = color_token(theme, option)
        {
            // The OS may switch mode while an editor is open. Rebind its HSV
            // and hex fields before another edit can touch the new palette.
            self.color_picker.open(*value);
        }
    }
    /// Polling is idempotent: each worker reply is consumed once, with no disk
    /// access or repeated writes. Safe from both visible UI and hidden logic.
    pub(in crate::app) fn service_themes(
        &mut self,
        ctx: &egui::Context,
        theme: &mut Theme,
    ) -> bool {
        let Some(selection) = self.appearance.library.poll(ctx) else {
            return false;
        };
        if selection.imported {
            theme.set_scheme(selection.scheme);
        } else {
            theme.remember_active_palette();
            // A save can finish after another choice or color edit. Preserve
            // that newer state while still adding the saved item to the menu.
            let source = self.appearance.saving.take();
            if !source.is_some_and(|source| {
                source.name == theme.scheme.name
                    && source.dark == theme.scheme.dark
                    && source.light == theme.scheme.light
            }) {
                return false;
            }
            let mut current = theme.scheme.clone();
            current.name = selection.scheme.name;
            theme.set_scheme(current);
        }
        self.appearance.name = theme.scheme.name.clone();
        true
    }
    pub(super) fn focus_theme_name(&mut self, theme: &Theme) {
        if self.appearance.name.is_empty() && theme.scheme.name != "Default" {
            self.appearance.name = theme.scheme.name.clone();
        }
        self.query.clear();
        self.refilter();
        self.selected = Some(OptionId::ThemeName);
        self.focus_control = Some(OptionId::ThemeName);
        self.scroll_to = true;
        self.focus = false;
    }
    fn save_named_theme(&mut self, ctx: &egui::Context, theme: &mut Theme) {
        if self.appearance.library.busy {
            return;
        }
        match theme.saved_scheme(&self.appearance.name) {
            Ok(scheme) => {
                theme.remember_active_palette();
                self.appearance.saving = Some(theme.scheme.clone());
                self.appearance.name_error = None;
                self.appearance.status_option = Some(OptionId::SaveTheme);
                self.appearance.library.start(ctx);
                self.appearance.library.save(scheme);
            }
            Err(error) => {
                self.appearance.name_error = Some(error);
                self.focus_theme_name(theme);
            }
        }
    }
    pub(super) fn theme_control(
        &mut self,
        ui: &mut Ui,
        theme: &mut Theme,
        paint: &Theme,
        rect: Rect,
        option: OptionId,
        activate: bool,
    ) -> bool {
        let response = match option {
            OptionId::ColorMode | OptionId::ColorScheme => {
                let label = if option == OptionId::ColorMode {
                    theme.color_mode.label()
                } else {
                    &theme.scheme.name
                };
                let response = foldex_ui::components::select_button(
                    ui,
                    paint,
                    label,
                    rect.width(),
                    self.scheme_menu.open && self.scheme_option == option,
                );
                response
                    .clone()
                    .on_hover_text(if option == OptionId::ColorMode {
                        format!(
                            "{} colors are active. Color controls edit this palette.",
                            theme.resolved_mode.label()
                        )
                    } else {
                        "Each scheme contains separate dark and light colors.".into()
                    });
                if response.clicked() || activate {
                    self.child_return_focus = Some(response.id);
                    if self.scheme_option != option {
                        self.scheme_menu = Default::default();
                    }
                    self.scheme_option = option;
                    self.scheme_menu.toggle(ui.ctx());
                    let selected = if option == OptionId::ColorMode {
                        ColorMode::ALL
                            .iter()
                            .position(|mode| *mode == theme.color_mode)
                            .unwrap_or(0)
                    } else {
                        self.scheme_choices(theme)
                            .iter()
                            .position(|scheme| scheme.name == theme.scheme.name)
                            .unwrap_or(0)
                    };
                    self.scheme_menu.select(selected);
                    response.request_focus();
                }
                if self.scheme_option == option {
                    self.scheme_anchor = response.rect;
                    self.scheme_seen = ui.ctx().cumulative_frame_nr();
                }
                response
            }
            OptionId::ThemeName => {
                let field_id = ui.id().with("theme-name");
                // Single-line TextEdit surrenders focus when it sees Enter.
                // Submit first so this inline action keeps its keyboard owner;
                // the asynchronous save reply must not reclaim focus later.
                let submit = foldex_ui::components::can_focus(ui)
                    && ui.memory(|memory| memory.focused() == Some(field_id))
                    && !self.ime_input
                    && crate::commands::take_key(ui.ctx(), Modifiers::NONE, Key::Enter);
                let input = foldex_ui::components::input_box(
                    ui,
                    rect,
                    field_id,
                    paint,
                    &mut self.appearance.name,
                    "Name your theme…",
                    64,
                );
                if input.response.changed() {
                    self.appearance.name_error = None;
                }
                if activate {
                    input.response.request_focus();
                }
                if submit {
                    self.save_named_theme(ui.ctx(), theme);
                }
                input.response.response
            }
            OptionId::SaveTheme | OptionId::LoadThemes | OptionId::ExportTheme => {
                let label = match option {
                    OptionId::SaveTheme => "Save Theme",
                    OptionId::LoadThemes => "Import…",
                    _ => "Export…",
                };
                let response = ui
                    .add_enabled_ui(!self.appearance.library.busy, |ui| {
                        foldex_ui::components::text_button_at(
                            ui,
                            rect,
                            ui.id().with("theme-action"),
                            paint,
                            label,
                            false,
                        )
                    })
                    .inner;
                if (response.clicked() || activate) && !self.appearance.library.busy {
                    match option {
                        OptionId::SaveTheme => self.save_named_theme(ui.ctx(), theme),
                        OptionId::LoadThemes => {
                            self.appearance.status_option = Some(option);
                            self.appearance.library.import();
                        }
                        _ => {
                            theme.remember_active_palette();
                            self.appearance.status_option = Some(option);
                            self.appearance.library.export(theme.scheme.clone());
                        }
                    }
                }
                response
            }
            _ => return false,
        };
        self.control_focus(ui, option, &response);
        // Name edits and library requests do not modify appearance. Menu
        // choices and successful worker replies produce the actual apply event.
        false
    }
    fn scheme_choices(&self, theme: &Theme) -> Vec<ColorScheme> {
        let mut choices = Vec::with_capacity(self.appearance.library.schemes.len() + 2);
        choices.push(ColorScheme::default());
        choices.extend(self.appearance.library.schemes.iter().cloned());
        if !choices
            .iter()
            .any(|scheme| scheme.name == theme.scheme.name)
        {
            choices.push(theme.scheme.clone());
        }
        choices
    }
    pub(super) fn theme_menu(
        &mut self,
        ctx: &egui::Context,
        theme: &mut Theme,
        paint: &Theme,
        area: Id,
    ) -> bool {
        if !self.scheme_menu.active() || self.scheme_option == OptionId::LauncherOpenFolder {
            return false;
        }
        let mode_menu = self.scheme_option == OptionId::ColorMode;
        let schemes = if mode_menu {
            Vec::new()
        } else {
            self.scheme_choices(theme)
        };
        let labels: Vec<&str> = if mode_menu {
            ColorMode::ALL.iter().map(|mode| mode.label()).collect()
        } else {
            schemes.iter().map(|scheme| scheme.name.as_str()).collect()
        };
        let items: Vec<_> = labels
            .iter()
            .enumerate()
            .map(|(index, label)| foldex_ui::components::MenuItem {
                id: index as u32,
                label,
                shortcut: "",
                glyph: Icon::Empty,
                checked: Some(if mode_menu {
                    ColorMode::ALL[index] == theme.color_mode
                } else {
                    schemes[index].name == theme.scheme.name
                }),
                enabled: true,
            })
            .collect();
        let Some(choice) = self.scheme_menu.show_width(
            ctx,
            area.with("scheme-menu"),
            paint,
            self.scheme_anchor,
            &items,
            self.scheme_anchor.width(),
        ) else {
            return false;
        };
        if mode_menu {
            if let Some(mode) = ColorMode::ALL.get(choice as usize) {
                theme.set_color_mode(*mode, ctx.system_theme());
            }
        } else if let Some(scheme) = schemes.get(choice as usize) {
            theme.set_scheme(scheme.clone());
            self.appearance.name = if scheme.name == "Default" {
                String::new()
            } else {
                scheme.name.clone()
            };
            self.appearance.name_error = None;
        }
        true
    }
}

pub(super) fn is_theme_option(option: OptionId) -> bool {
    matches!(
        option,
        OptionId::ColorMode
            | OptionId::ColorScheme
            | OptionId::ThemeName
            | OptionId::SaveTheme
            | OptionId::LoadThemes
            | OptionId::ExportTheme
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn appearance_frame(
        ctx: &egui::Context,
        popup: &mut OptionsPopup,
        theme: &mut Theme,
        frame: &mut u64,
        events: Vec<egui::Event>,
    ) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                time: Some(*frame as f64 / 60.0),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| {
                if popup.service_themes(ui.ctx(), theme) {
                    popup.theme_palette_changed(theme);
                    theme.apply(ui.ctx());
                    // The debounce itself has separate tests. Force the write
                    // here so this regression covers its acknowledgement too.
                    popup.persist_theme(ui.ctx(), theme, true);
                }
                popup.ui(ui.ctx(), theme);
            },
        );
        output.textures_delta.clear();
        *frame += 1;
    }

    #[test]
    fn enter_save_keeps_inline_focus_through_async_ack_and_keyboard_navigation() {
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        struct Fixture(std::path::PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(std::env::temp_dir().join(format!(
            "foldex-theme-focus-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        )));
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme {
            reduced_motion: true,
            ..Theme::default()
        };
        theme.set_color_mode(ColorMode::Dark, None);
        theme.apply(&ctx);
        popup.configure_themes(fixture.0.join("theme.toml"), 0);
        let mut frame = 0;
        appearance_frame(&ctx, &mut popup, &mut theme, &mut frame, vec![]);
        popup.focus = false;
        popup.focus_control = Some(OptionId::ThemeName);
        popup.selected = Some(OptionId::ThemeName);
        appearance_frame(&ctx, &mut popup, &mut theme, &mut frame, vec![]);
        appearance_frame(
            &ctx,
            &mut popup,
            &mut theme,
            &mut frame,
            vec![egui::Event::Text("Keyboard Theme".into())],
        );
        let field = popup.controls[&OptionId::ThemeName].id;
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(field));
        assert_eq!(popup.appearance.name, "Keyboard Theme");

        let loaded_by = Instant::now() + Duration::from_secs(5);
        while popup.appearance.library.busy {
            assert!(Instant::now() < loaded_by, "theme library did not load");
            appearance_frame(&ctx, &mut popup, &mut theme, &mut frame, vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        appearance_frame(
            &ctx,
            &mut popup,
            &mut theme,
            &mut frame,
            vec![key(Key::Enter, Modifiers::NONE)],
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(field),
            "submitting an inline name must not surrender its keyboard owner"
        );

        let saved_by = Instant::now() + Duration::from_secs(5);
        while theme.scheme.name != "Keyboard Theme" || !popup.theme_preferences_saved() {
            assert!(
                Instant::now() < saved_by,
                "named theme did not finish saving"
            );
            appearance_frame(&ctx, &mut popup, &mut theme, &mut frame, vec![]);
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(field),
                "library and preferences replies must preserve inline focus"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(fixture.0.join("themes/Keyboard Theme.toml").is_file());
        appearance_frame(
            &ctx,
            &mut popup,
            &mut theme,
            &mut frame,
            vec![key(Key::Tab, Modifiers::SHIFT)],
        );
        let scheme = popup.controls[&OptionId::ColorScheme].id;
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(scheme),
            "the saved editor must still yield to real Options Tab navigation"
        );
        for _ in 0..3 {
            appearance_frame(&ctx, &mut popup, &mut theme, &mut frame, vec![]);
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(scheme));
        }
    }

    #[test]
    fn default_stays_available_beside_migrated_and_saved_schemes() {
        let mut popup = OptionsPopup::default();
        popup
            .appearance
            .library
            .schemes
            .push(Theme::default().saved_scheme("Ocean").unwrap());
        let mut theme = Theme::midnight();
        theme.font_size = 17.0;
        theme.reduced_motion = true;
        let choices = popup.scheme_choices(&theme);
        assert_eq!(
            choices
                .iter()
                .map(|scheme| scheme.name.as_str())
                .collect::<Vec<_>>(),
            ["Default", "Ocean", "Midnight"]
        );
        theme.set_scheme(choices[0].clone());
        assert_eq!(theme.font_size, 17.0);
        assert!(theme.reduced_motion);
        assert_eq!(theme.background, Theme::default().background);
    }
    #[test]
    fn theme_name_is_an_inline_option_and_does_not_change_the_theme() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme::default();
        let before = theme.write().unwrap();
        popup.focus_theme_name(&theme);
        assert_eq!(popup.selected, Some(OptionId::ThemeName));
        assert_eq!(popup.focus_control, Some(OptionId::ThemeName));
        let paint = theme.clone();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(!popup.control(
                ui,
                &mut theme,
                &paint,
                Rect::from_min_size(pos2(0.0, 0.0), vec2(180.0, 28.0)),
                OptionId::ThemeName,
                false,
                1.0
            ));
        });
        output.textures_delta.clear();
        assert_eq!(theme.write().unwrap(), before);
    }
    #[test]
    fn raw_system_updates_survive_the_hidden_logic_contexts_stale_input() {
        let ctx = egui::Context::default();
        let mut popup = OptionsPopup::default();
        let mut theme = Theme::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                system_theme: Some(egui::Theme::Dark),
                ..Default::default()
            },
            |_| {},
        );
        output.textures_delta.clear();
        let input = egui::RawInput {
            system_theme: Some(egui::Theme::Light),
            ..Default::default()
        };
        // eframe calls raw_input_hook before Context::run_logic. run_logic
        // only updates window state, leaving Context::system_theme stale.
        popup.observe_system_theme(input.system_theme);
        let _ = ctx.run_logic(&input, |ctx| {
            assert_eq!(ctx.system_theme(), Some(egui::Theme::Dark));
            assert!(theme.resolve_system(popup.system_theme(ctx)));
        });
        assert_eq!(theme.resolved_mode, foldex_ui::ResolvedMode::Light);
        assert_eq!(theme.background, Theme::paper().background);
    }
}
