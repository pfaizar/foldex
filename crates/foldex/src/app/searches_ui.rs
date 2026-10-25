use super::*;
use crate::navigation::{Route, SearchLocation};

pub(super) struct SearchEditor {
    origin: u64,
    location: SearchLocation,
    min: String,
    max: String,
    days: String,
    exclusions: String,
    name: String,
    error: Option<String>,
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
            origin: pane.id,
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
                .join("\n"),
            location,
            name: String::new(),
            error: None,
        }
    }
    fn build(&self) -> Result<SearchLocation, String> {
        let mut location = self.location.clone();
        let c = &mut location.query.conditions;
        c.min_bytes = foldex_query::parse_bytes(&self.min)?;
        c.max_bytes = foldex_query::parse_bytes(&self.max)?;
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
            .lines()
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
        if !self.pane().is_virtual() {
            self.search_editor = Some(SearchEditor::new(self.pane()));
        }
    }
    pub(super) fn searches_ui(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.search_editor.take() else {
            return;
        };
        let mut open = true;
        let mut action = None;
        egui::Window::new("Search conditions").id(Id::new("search-conditions"))
            .open(&mut open).resizable(true).default_width(440.0).show(ctx, |ui| {
            ui.label(format!("In {}", editor.location.scope.display()));
            ui.label("Filename"); ui.text_edit_singleline(&mut editor.location.query.text);
            ui.checkbox(&mut editor.location.directory_only, "Direct children only");
            let c = &mut editor.location.query.conditions;
            ui.label("Relative path contains"); ui.text_edit_singleline(&mut c.path_contains);
            ui.horizontal(|ui| { ui.label("Minimum size"); ui.text_edit_singleline(&mut editor.min); });
            ui.horizontal(|ui| { ui.label("Maximum size"); ui.text_edit_singleline(&mut editor.max); });
            ui.label("Size units: B, KB, MB, GB, KiB, MiB, GiB. Size conditions include files.");
            ui.horizontal(|ui| { ui.label("Modified in the last days"); ui.text_edit_singleline(&mut editor.days); });
            ui.horizontal(|ui| {
                ui.label("Modified after / before (UTC Unix seconds)");
                for value in [&mut c.modified_after, &mut c.modified_before] {
                    let mut enabled = value.is_some();
                    if ui.checkbox(&mut enabled, "").changed() { *value = enabled.then_some(0); }
                    if let Some(value) = value { ui.add(egui::DragValue::new(value)); }
                }
            });
            ui.checkbox(&mut editor.location.query.show_hidden, "Include hidden items");
            ui.label("Exclude subtrees (one raw relative path per line)");
            ui.add(egui::TextEdit::multiline(&mut editor.exclusions).desired_rows(3));
            ui.label("Excluded folders and their descendants are omitted from traversal. Other conditions use cached metadata.");
            ui.separator();
            ui.horizontal(|ui| { ui.label("Collection name"); ui.text_edit_singleline(&mut editor.name); });
            ui.horizontal(|ui| {
                if ui.button("Apply search").clicked() { action = Some(false); }
                if ui.button("Save collection and apply").clicked() { action = Some(true); }
            });
            if let Some(error) = &editor.error { ui.colored_label(egui::Color32::LIGHT_RED, error); }
        });
        if let Some(save) = action {
            let result = editor.build().and_then(|location| {
                if save {
                    let name = editor.name.trim();
                    if name.is_empty() || name.len() > 96 {
                        return Err("Choose a collection name of 1–96 bytes".into());
                    }
                    if let Some(old) = self.saved_searches.iter_mut().find(|s| s.name == name) {
                        old.location = location.clone();
                    } else {
                        if self.saved_searches.len() >= 32 {
                            return Err(
                                "Remove a collection before saving another (limit 32)".into()
                            );
                        }
                        self.saved_searches.push(crate::navigation::SavedSearch {
                            name: name.into(),
                            location: location.clone(),
                        });
                    }
                    self.session_dirty = true;
                    self.save_due = Some(Instant::now());
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
                Ok(()) => open = false,
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
        app.search_editor = Some(editor);
        ctx.run_ui(egui::RawInput::default(), |ui| app.searches_ui(ui.ctx()))
            .drop_without_applying_deltas();
        assert!(app.search_editor.is_some());
        app.action(Action::Search(location));
        assert!(matches!(app.pane().route(), Route::Search(_)));
        app.action(Action::RemoveSearch("Reports".into()));
        assert!(app.saved_searches.is_empty());
    }
}
