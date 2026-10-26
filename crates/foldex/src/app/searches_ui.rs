use super::*;
use crate::navigation::{Route, SearchLocation};

const WIDTH: f32 = 560.0;
const HEADER: f32 = 38.0;
const ROW: f32 = 36.0;
const FOOTER: f32 = 44.0;
const ROWS: usize = 11;

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
    pub(super) fn searches_ui(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.search_editor.take() else {
            return;
        };
        let t = self.theme.clone();
        let m = t.metrics();
        let scale = m.scale;
        let mut open = true;
        let mut apply = false;
        if crate::commands::take_key(ctx, Modifiers::NONE, Key::Escape) {
            open = false;
        }
        if crate::commands::take_key(ctx, Modifiers::NONE, Key::Enter) {
            apply = true;
        }
        let screen = ctx.content_rect();
        let width = (WIDTH * scale).min((screen.width() - 30.0 * scale).max(1.0));
        let (header_h, row_h, footer_h) = (HEADER * scale, ROW * scale, FOOTER * scale);
        let top = (screen.top() + 32.0 + 47.0 * scale)
            .min(screen.bottom() - (header_h + footer_h + row_h).min(screen.height()) - 12.0)
            .max(screen.top());
        let available = (screen.bottom() - top - 12.0).max(1.0);
        let height = (header_h + row_h * ROWS as f32 + 8.0 * scale + footer_h).min(available);
        let outer = Rect::from_min_size(
            pos2(screen.center().x - width * 0.5, top),
            vec2(width, height),
        );
        let area_id = Id::new("search-conditions");
        foldex_ui::components::popup_surface(
            ctx,
            egui::Area::new(area_id)
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            true,
            |ui| {
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(screen);
                ui.painter().add(
                    egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 18,
                        spread: 0,
                        color: egui::Color32::from_black_alpha(85),
                    }
                    .as_shape(outer, t.radius),
                );
                ui.painter()
                    .rect_filled(outer, t.radius, Theme::color(t.surface));
                ui.painter().rect_stroke(
                    outer,
                    t.radius,
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                ui.set_clip_rect(outer.intersect(screen));
                let header = Rect::from_min_size(outer.min, vec2(outer.width(), header_h));
                ui.painter().text(
                    pos2(header.left() + 14.0 * scale, header.center().y),
                    Align2::LEFT_CENTER,
                    "Search conditions",
                    t.font(),
                    t.fg(),
                );
                if foldex_ui::components::icon_button_at(
                    ui,
                    foldex_ui::components::popup_action_rect(header, 0, 1),
                    area_id.with("close"),
                    &t,
                    Icon::Close,
                    false,
                )
                .on_hover_text("Close · Escape")
                .clicked()
                {
                    open = false;
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    Stroke::new(1.0, t.line()),
                );
                let body = Rect::from_min_max(
                    pos2(outer.left() + 4.0, header.bottom() + 4.0 * scale),
                    pos2(outer.right() - 4.0, outer.bottom() - footer_h),
                );
                ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
                    ui.set_clip_rect(body.intersect(outer));
                    ui.spacing_mut().item_spacing.y = 0.0;
                    egui::ScrollArea::vertical()
                        .id_salt("search-conditions-rows")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for index in 0..ROWS {
                                let rect = ui
                                    .allocate_exact_size(
                                        vec2(ui.available_width(), row_h),
                                        Sense::hover(),
                                    )
                                    .0;
                                let control_width = (rect.width() * 0.56).max(80.0);
                                let control = Rect::from_min_max(
                                    pos2(
                                        rect.right() - control_width - 12.0 * scale,
                                        rect.center().y - m.control * 0.5,
                                    ),
                                    pos2(
                                        rect.right() - 12.0 * scale,
                                        rect.center().y + m.control * 0.5,
                                    ),
                                );
                                let label_rect = Rect::from_min_max(
                                    pos2(rect.left() + 12.0 * scale, rect.top()),
                                    pos2(control.left() - 8.0 * scale, rect.bottom()),
                                );
                                let id = area_id.with(("field", index));
                                let c = &mut editor.location.query.conditions;
                                let (label, text, hint, limit): (
                                    &str,
                                    Option<&mut String>,
                                    &str,
                                    usize,
                                ) = match index {
                                    0 => ("Folder", Some(&mut editor.scope), "", 32768),
                                    1 => (
                                        "Filename",
                                        Some(&mut editor.location.query.text),
                                        "",
                                        1024,
                                    ),
                                    2 => ("Direct children only", None, "", 0),
                                    3 => ("Path contains", Some(&mut c.path_contains), "", 4096),
                                    4 => ("Minimum size", Some(&mut editor.min), "e.g. 2 MiB", 32),
                                    5 => ("Maximum size", Some(&mut editor.max), "e.g. 1 GB", 32),
                                    6 => ("Modified in last days", Some(&mut editor.days), "", 5),
                                    7 => (
                                        "Modified after (UTC)",
                                        Some(&mut editor.after),
                                        "YYYY-MM-DD",
                                        10,
                                    ),
                                    8 => (
                                        "Modified before (UTC)",
                                        Some(&mut editor.before),
                                        "YYYY-MM-DD",
                                        10,
                                    ),
                                    9 => ("Include hidden items", None, "", 0),
                                    _ => (
                                        "Exclude folders",
                                        Some(&mut editor.exclusions),
                                        "folder | folder",
                                        16384,
                                    ),
                                };
                                foldex_ui::components::elided_label(
                                    ui.painter(),
                                    label_rect,
                                    &t,
                                    label,
                                    t.fg(),
                                    false,
                                );
                                if let Some(value) = text {
                                    if index == 1 && !editor.focused {
                                        editor.focused = true;
                                        ui.memory_mut(|memory| memory.request_focus(id));
                                    }
                                    foldex_ui::components::input_box(
                                        ui, control, id, &t, value, hint, limit,
                                    );
                                } else {
                                    let slot = Rect::from_min_size(
                                        pos2(control.right() - m.control, control.top()),
                                        vec2(m.control, m.control),
                                    );
                                    let mut child =
                                        ui.new_child(UiBuilder::new().id_salt(id).max_rect(slot));
                                    let value = if index == 2 {
                                        &mut editor.location.directory_only
                                    } else {
                                        &mut editor.location.query.show_hidden
                                    };
                                    foldex_ui::components::checkbox(&mut child, &t, value, "");
                                }
                            }
                        });
                });
                let footer =
                    Rect::from_min_max(pos2(outer.left(), outer.bottom() - footer_h), outer.max);
                ui.painter().line_segment(
                    [footer.left_top(), footer.right_top()],
                    Stroke::new(1.0, t.line()),
                );
                let button = vec2(80.0 * scale, m.control);
                let apply_rect = Rect::from_min_size(
                    pos2(
                        footer.right() - 12.0 * scale - button.x,
                        footer.center().y - button.y * 0.5,
                    ),
                    button,
                );
                let reset_rect = apply_rect.translate(vec2(-(button.x + 8.0 * scale), 0.0));
                if foldex_ui::components::text_button_at(
                    ui,
                    apply_rect,
                    area_id.with("apply"),
                    &t,
                    "Apply",
                    true,
                )
                .clicked()
                {
                    apply = true;
                }
                if foldex_ui::components::text_button_at(
                    ui,
                    reset_rect,
                    area_id.with("reset"),
                    &t,
                    "Reset",
                    false,
                )
                .clicked()
                {
                    editor.reset();
                }
                if let Some(error) = &editor.error {
                    foldex_ui::components::elided_label(
                        ui.painter(),
                        Rect::from_min_max(
                            pos2(footer.left() + 14.0 * scale, footer.top()),
                            pos2(reset_rect.left() - 8.0 * scale, footer.bottom()),
                        ),
                        &t,
                        error,
                        Theme::color(t.danger),
                        false,
                    );
                }
            },
        );
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
