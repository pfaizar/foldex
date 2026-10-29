use super::*;
use crate::navigation::{Route, SearchLocation};

pub(super) struct SearchEditor {
    origin: u64,
    scope: String,
    location: SearchLocation,
    min: String,
    max: String,
    days: String,
    after: String,
    before: String,
    exclusions: String,
    error: Option<String>,
    focused: bool,
}
impl SearchEditor {
    fn new(pane: &Pane) -> Self {
        let location = match pane.route() {
            Route::Search(location) => location.clone(),
            _ => SearchLocation {
                scope: pane.path.clone(),
                query: pane.query.clone(),
                directory_only: !pane.expanded_search(),
            },
        };
        let c = &location.query.conditions;
        Self {
            after: c
                .modified_after
                .map(foldex_query::format_utc_date)
                .unwrap_or_default(),
            before: c
                .modified_before
                .map(foldex_query::format_utc_date)
                .unwrap_or_default(),
            origin: pane.id,
            scope: location.scope.to_string_lossy().into_owned(),
            min: c.min_bytes.map(|v| v.to_string()).unwrap_or_default(),
            max: c.max_bytes.map(|v| v.to_string()).unwrap_or_default(),
            days: c
                .modified_within_days
                .map(|v| v.to_string())
                .unwrap_or_default(),
            exclusions: c
                .excluded_subtrees
                .iter()
                .map(|v| v.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" | "),
            location,
            error: None,
            focused: false,
        }
    }
    fn reset(&mut self) {
        self.location.query.text.clear();
        self.location.query.show_hidden = true;
        self.location.query.conditions = Default::default();
        for value in [
            &mut self.min,
            &mut self.max,
            &mut self.days,
            &mut self.after,
            &mut self.before,
            &mut self.exclusions,
        ] {
            value.clear();
        }
        self.error = None;
    }
    fn build(&self) -> Result<SearchLocation, String> {
        let mut location = self.location.clone();
        if self.scope != self.location.scope.to_string_lossy() {
            location.scope = PathBuf::from(&self.scope);
        }
        let c = &mut location.query.conditions;
        c.min_bytes = foldex_query::parse_bytes(&self.min)?;
        c.max_bytes = foldex_query::parse_bytes(&self.max)?;
        if self.after
            != c.modified_after
                .map(foldex_query::format_utc_date)
                .unwrap_or_default()
        {
            c.modified_after = foldex_query::parse_utc_date(&self.after, false)?;
        }
        if self.before
            != c.modified_before
                .map(foldex_query::format_utc_date)
                .unwrap_or_default()
        {
            c.modified_before = foldex_query::parse_utc_date(&self.before, true)?;
        }
        c.modified_within_days = if self.days.trim().is_empty() {
            None
        } else {
            Some(
                self.days
                    .trim()
                    .parse::<u16>()
                    .map_err(|_| "Days must be a whole number from 0 to 65535")?,
            )
        };
        c.excluded_subtrees = self
            .exclusions
            .split(['\n', '|'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        c.excluded_subtrees.sort();
        c.excluded_subtrees.dedup();
        location.validate()?;
        Ok(location)
    }
}
impl Foldex {
    pub(super) fn open_search_editor(&mut self) {
        if !self.pane().is_recycle_bin() {
            self.search_editor = Some(SearchEditor::new(self.pane()));
        }
    }
    #[cfg(test)]
    pub(super) fn catalog_search_error(&mut self) {
        if let Some(editor) = &mut self.search_editor {
            editor.max = "1 KB".into();
            editor.min = "2 MiB".into();
            editor.error = editor.build().err();
        }
    }
    pub(super) fn searches_ui(&mut self, ctx: &egui::Context) {
        use foldex_ui::dialog::{
            Action, Dialog, Footer, Form, Kind, ROW_GAP, Size, Tone, callout, checkbox, field,
            field_sized, half, inline_label, unit,
        };
        let Some(mut editor) = self.search_editor.take() else {
            return;
        };
        let t = self.theme.clone();
        let s = t.metrics().scale;
        let mut open = true;
        let closing = foldex_ui::components::dialog_is_closing(ctx, Id::new("search-conditions"));
        let mut apply = !closing && crate::commands::take_key(ctx, Modifiers::NONE, Key::Enter);
        let area_id = Id::new("search-conditions");
        let footer = Footer::new(vec![
            Action::new(area_id.with("reset"), "Reset", Kind::Ghost),
            Action::new(area_id.with("cancel"), "Cancel", Kind::Secondary),
            Action::new(area_id.with("apply"), "Search", Kind::Primary),
        ]);
        let shown = Dialog::new(area_id, "Search conditions", Size::Large.width(&t))
            .subtitle("Narrow results in this folder by name, size and date.")
            .footer(footer)
            .show(ctx, &t, &mut open, |ui| {
                ui.spacing_mut().item_spacing.y = ROW_GAP * s;
                let form = Form::new(
                    ui,
                    &t,
                    &[
                        "Folder",
                        "Exclude",
                        "Name",
                        "Path contains",
                        "Size",
                        "Modified",
                    ],
                );
                let field_id = |index: usize| area_id.with(("field", index));
                form.row(ui, &t, "Folder", |ui| {
                    field(ui, &t, field_id(0), &mut editor.scope, "", 32768);
                });
                form.row(ui, &t, "", |ui| {
                    ui.spacing_mut().item_spacing.x = 24.0 * s;
                    let mut subfolders = !editor.location.directory_only;
                    if checkbox(ui, &t, &mut subfolders, "Include subfolders").changed() {
                        editor.location.directory_only = !subfolders;
                    }
                    checkbox(
                        ui,
                        &t,
                        &mut editor.location.query.show_hidden,
                        "Include hidden items",
                    );
                });
                form.row(ui, &t, "Exclude", |ui| {
                    field(
                        ui,
                        &t,
                        field_id(10),
                        &mut editor.exclusions,
                        "Folders to skip, separated by |",
                        16384,
                    );
                });
                let name = form.row(ui, &t, "Name", |ui| {
                    field(
                        ui,
                        &t,
                        field_id(1),
                        &mut editor.location.query.text,
                        "Part of the file name",
                        1024,
                    )
                });
                if !editor.focused {
                    editor.focused = true;
                    name.response.request_focus();
                }
                form.row(ui, &t, "Path contains", |ui| {
                    field(
                        ui,
                        &t,
                        field_id(3),
                        &mut editor.location.query.conditions.path_contains,
                        "",
                        4096,
                    );
                });
                form.row(ui, &t, "Size", |ui| {
                    let width = half(ui, &t);
                    field_sized(
                        ui,
                        &t,
                        field_id(4),
                        &mut editor.min,
                        "At least, e.g. 2 MiB",
                        32,
                        width,
                    );
                    inline_label(ui, &t, "to");
                    field(
                        ui,
                        &t,
                        field_id(5),
                        &mut editor.max,
                        "At most, e.g. 1 GB",
                        32,
                    );
                });
                let date = 116.0 * s;
                form.row(ui, &t, "Modified", |ui| {
                    inline_label(ui, &t, "In the last");
                    field_sized(ui, &t, field_id(6), &mut editor.days, "", 5, 64.0 * s);
                    unit(ui, &t, "days");
                });
                form.row(ui, &t, "", |ui| {
                    inline_label(ui, &t, "Between");
                    field_sized(
                        ui,
                        &t,
                        field_id(7),
                        &mut editor.after,
                        "YYYY-MM-DD",
                        10,
                        date,
                    );
                    inline_label(ui, &t, "and");
                    field_sized(
                        ui,
                        &t,
                        field_id(8),
                        &mut editor.before,
                        "YYYY-MM-DD",
                        10,
                        date,
                    );
                    unit(ui, &t, "UTC");
                });
                if let Some(error) = &editor.error {
                    ui.add_space(6.0 * s);
                    callout(ui, &t, Tone::Danger, error);
                }
            });
        if let Some(index) = shown.clicked {
            match index {
                0 => editor.reset(),
                1 => foldex_ui::components::close_dialog(ctx, area_id),
                _ => apply = true,
            }
        }
        if apply {
            let result = editor.build().and_then(|location| {
                if !self.tabs.iter().any(|tab| {
                    tab.left.id == editor.origin
                        || tab
                            .right
                            .as_ref()
                            .is_some_and(|pane| pane.id == editor.origin)
                }) {
                    return Err("The originating pane was closed; reopen the search editor".into());
                }
                if let Some(pane) = self
                    .tabs
                    .iter_mut()
                    .flat_map(|tab| std::iter::once(&mut tab.left).chain(tab.right.iter_mut()))
                    .find(|pane| pane.id == editor.origin)
                {
                    pane.navigate_route(Route::Search(location), true);
                }
                Ok(())
            });
            match result {
                Ok(()) => foldex_ui::components::close_dialog(ctx, area_id),
                Err(error) => editor.error = Some(error),
            }
        }
        if open {
            self.search_editor = Some(editor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collection_editor_parses_bounds_and_sessions_merge_collections() {
        let (mut app, ctx, root) = crate::app::tests::fixture_app();
        app.open_search_editor();
        assert!(app.popup_active());
        let mut editor = app.search_editor.take().unwrap();
        editor.location.scope = root.clone();
        editor.scope = root.to_string_lossy().into_owned();
        editor.min = "2 MiB".into();
        editor.max = "3 MB".into();
        editor.days = "7".into();
        editor.exclusions = "build\nbuild\nnode_modules".into();
        let location = editor.build().unwrap();
        assert_eq!(location.query.conditions.min_bytes, Some(2_097_152));
        assert_eq!(location.query.conditions.excluded_subtrees.len(), 2);
        app.saved_searches.push(crate::navigation::SavedSearch {
            name: "Reports".into(),
            location: location.clone(),
        });
        let session = serde_json::to_value(app.session_snapshot()).unwrap();
        let shared = crate::persistence::shared::preferences(&session);
        assert_eq!(shared["saved_searches"][0]["name"], "Reports");
        let restored: Session = serde_json::from_value(session).unwrap();
        app.apply_shared_preferences(restored);
        assert_eq!(app.saved_searches[0].location, location);
        editor.max = "1 KB".into();
        assert!(editor.build().is_err());
        editor.reset();
        assert!(editor.build().is_ok());
        app.search_editor = Some(editor);
        ctx.run_ui(egui::RawInput::default(), |ui| app.searches_ui(ui.ctx()))
            .drop_without_applying_deltas();
        assert!(app.search_editor.is_some());
        app.action(Action::Search(location));
        assert!(matches!(app.pane().route(), Route::Search(_)));
        app.action(Action::RemoveSearch("Reports".into()));
        assert!(app.saved_searches.is_empty());
        app.search_editor = None;
        app.pane_mut().navigate_home(true);
        app.open_search_editor();
        let editor = app.search_editor.as_ref().unwrap();
        assert!(!editor.scope.is_empty());
    }
}
