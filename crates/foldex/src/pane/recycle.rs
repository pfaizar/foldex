//! Presentation adapters for the shell-only Recycle Bin model.
use super::*;
use crate::recycle_bin::{Intent, Model, PreparedView, Target, ViewRequest};

impl Pane {
    pub fn is_recycle_bin(&self) -> bool {
        matches!(self.route(), crate::navigation::Route::RecycleBin)
    }
    pub fn is_virtual(&self) -> bool {
        !matches!(self.route(), crate::navigation::Route::Folder(_))
    }
    pub fn navigate_recycle_bin(&mut self, remember: bool) {
        if remember {
            self.navigation.visit(crate::navigation::Route::RecycleBin);
        }
        self.navigate_home(false);
        self.columns.enter_recycle();
        self.address = crate::navigation::RECYCLE_BIN_ADDRESS.into();
        self.recycle_refresh_requested = true;
        self.loading = true;
    }
    pub(crate) fn recycle_view_request(&mut self, model: Arc<Model>) -> Option<ViewRequest> {
        let key = (
            model.snapshot.id,
            self.navigation_epoch(),
            self.query_generation,
        );
        if !self.is_recycle_bin() || self.recycle_requested == Some(key) {
            return None;
        }
        self.recycle_requested = Some(key);
        Some(ViewRequest {
            pane: self.id,
            navigation: self.navigation_epoch(),
            generation: self.query_generation,
            model,
            query: self.query.clone(),
            sort: self.columns.recycle_sort(),
        })
    }
    pub(crate) fn recycle_view_retry(&mut self) {
        self.recycle_requested = None;
    }
    pub(crate) fn apply_recycle_view(&mut self, view: PreparedView) -> bool {
        let request = view.request;
        if !self.is_recycle_bin()
            || request.pane != self.id
            || request.navigation != self.navigation_epoch()
            || request.generation != self.query_generation
            || request.query != self.query
            || self.recycle_requested
                != Some((
                    request.model.snapshot.id,
                    request.navigation,
                    request.generation,
                ))
        {
            return false;
        }
        self.browser.cancel_filter();
        self.selected.clear();
        self.focused = None;
        self.anchor = None;
        self.last_selection = None;
        self.snapshot = Some(request.model.snapshot.clone());
        self.pending_snapshot = None;
        self.recycle_model = Some(request.model);
        self.indices = view.indices;
        self.kind_counts = Some(view.counts);
        self.rendered_query = request.query;
        self.match_paint = foldex_match::Matcher::new(&self.rendered_query.text)
            .with_mode(self.rendered_query.match_mode);
        self.query_time = view.elapsed;
        self.render_epoch = self.render_epoch.wrapping_add(1);
        self.row_cache.clear();
        self.name_cache.clear();
        self.loading = false;
        self.error = None;
        self.scroll_to = Some(0);
        true
    }
    pub(crate) fn recycle_target(&self, all: bool) -> Option<Target> {
        if !self.is_recycle_bin()
            || self.recycle_busy
            || self.loading
            || self.query != self.rendered_query
        {
            return None;
        }
        let model = self.recycle_model.as_ref()?.clone();
        if model.is_empty() {
            return None;
        }
        let target = if all {
            Target::All(model)
        } else {
            Target::Selection {
                model,
                order: self.indices.clone(),
                ranges: self.selected.intervals(),
            }
        };
        (target.count() > 0).then_some(target)
    }
    pub(super) fn recycle_selection(&self, command: SelectionCommand) -> Option<Action> {
        let intent = match command {
            SelectionCommand::Recycle | SelectionCommand::Delete(_) => Intent::DeleteSelected,
            SelectionCommand::Properties => Intent::PropertiesSelected,
            SelectionCommand::Context(position) => Intent::Context(position),
            _ => return None,
        };
        self.recycle_target(false)?;
        Some(Action::RecycleBin(intent))
    }
    pub(super) fn recycle_column_value(
        &self,
        column: columns::Column,
        index: u32,
    ) -> columns::CellValue {
        use columns::Column;
        let item = self
            .recycle_model
            .as_ref()
            .and_then(|model| model.items.get(index as usize));
        let text = item
            .map(|item| match column {
                Column::Name => item.name.clone(),
                Column::Type => {
                    if item.is_dir {
                        "File folder".into()
                    } else {
                        file_kind(&item.name)
                    }
                }
                Column::Size => foldex_ui::format_size(item.size),
                Column::DateDeleted => {
                    if item.deleted_at == 0 {
                        "—".into()
                    } else {
                        format_date(item.deleted_at)
                    }
                }
                Column::OriginalLocation => {
                    self.recycle_model.as_ref().unwrap().locations[index as usize].clone()
                }
                _ => "—".into(),
            })
            .unwrap_or_else(|| "—".into());
        columns::CellValue {
            tooltip: text.clone(),
            text,
            busy: false,
            partial: false,
        }
    }
    pub(super) fn recycle_controls(
        &self,
        ui: &mut Ui,
        t: &Theme,
        mut right: f32,
        y: f32,
        actions: &mut Vec<Action>,
    ) -> (f32, Rect, Rect) {
        let enabled = !self.recycle_busy
            && !self.loading
            && self.query == self.rendered_query
            && self
                .recycle_model
                .as_ref()
                .is_some_and(|model| !model.is_empty());
        let mut rects = [Rect::NOTHING; 2];
        for (index, label, intent) in [
            (1, "Restore All", Intent::RestoreAll),
            (0, "Empty Bin", Intent::Empty),
        ] {
            let width = ui
                .painter()
                .layout_no_wrap(label.to_owned(), t.font(), t.fg())
                .size()
                .x
                // text_button_at reserves eight scaled pixels on each side.
                // Leave a little extra room for pixel rounding in cached text.
                + 16.0 * t.metrics().scale
                + 2.0;
            let rect = Rect::from_center_size(
                pos2(right - width * 0.5, y),
                vec2(width, t.metrics().control),
            );
            let mut child = ui.new_child(
                UiBuilder::new()
                    .id_salt((self.id, "recycle-control", index))
                    .max_rect(rect),
            );
            if !enabled {
                child.disable();
            }
            let response = foldex_ui::components::text_button_at(
                &child,
                rect,
                Id::new((self.id, "recycle-control", index)),
                t,
                label,
                false,
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)
            });
            crate::keyboard::register(
                &child,
                crate::keyboard::Region::Toolbar(self.id),
                response.id,
                rect,
            );
            if response
                .on_hover_text(if index == 0 {
                    "Permanently delete all items in the Recycle Bin"
                } else {
                    "Restore every item to its original location"
                })
                .clicked()
            {
                actions.push(Action::RecycleBin(intent));
            }
            right = rect.left() - t.metrics().gap;
            rects[index] = rect;
        }
        (right, rects[0], rects[1])
    }
    pub(super) fn recycle_status_ui(
        &self,
        ui: &Ui,
        t: &Theme,
        body: Rect,
        actions: &mut Vec<Action>,
    ) {
        if let Some(error) = &self.error {
            let banner = Rect::from_min_size(
                body.min,
                vec2(body.width(), (t.metrics().control * 2.0).min(body.height())),
            );
            ui.painter()
                .rect_filled(banner, t.radius, Theme::color(t.surface));
            let retry = Rect::from_min_max(
                pos2(
                    (banner.right() - 68.0 * t.metrics().scale).max(banner.left()),
                    banner.top(),
                ),
                banner.max,
            );
            foldex_ui::components::elided_label(
                ui.painter(),
                Rect::from_min_max(banner.min, pos2(retry.left() - 4.0, banner.bottom())),
                t,
                error,
                Theme::color(t.danger),
                false,
            );
            if foldex_ui::components::text_button_at(
                ui,
                retry,
                Id::new((self.id, "recycle-retry")),
                t,
                "Retry",
                false,
            )
            .clicked()
            {
                actions.push(Action::RecycleBin(Intent::Retry));
            }
        }
    }
    pub(super) fn recycle_probe(
        &self,
        ctx: &egui::Context,
        address: Rect,
        body: Rect,
        footer: Rect,
        controls: (Rect, Rect),
    ) {
        if !ctx.data(|data| {
            data.get_temp::<bool>(Id::new("benchmark-probe-layout"))
                .unwrap_or(false)
        }) {
            return;
        }
        let mut rows = self
            .presented_entries
            .keys()
            .filter_map(|id| ctx.read_response(id.with("hit")))
            .map(|response| response.rect.intersect(body))
            .filter(|rect| rect.is_positive())
            .collect::<Vec<_>>();
        rows.sort_by(|a, b| {
            a.top()
                .total_cmp(&b.top())
                .then_with(|| a.left().total_cmp(&b.left()))
        });
        ctx.data_mut(|data| {
            data.insert_temp(
                Id::new((self.id, "recycle-probe")),
                crate::app::recycle_ui::RecycleProbe {
                    items: self.view_len(),
                    visible: self.visible_rows.clone(),
                    rows,
                    address,
                    body,
                    footer,
                    empty: controls.0,
                    restore_all: controls.1,
                },
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recycle_bin::Service;

    fn fixture() -> (Engine, Pane) {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let mut pane = Pane::dormant(19, &engine, "C:/inert-recycle-fallback".into());
        pane.navigate_recycle_bin(true);
        let items = [
            (1, "report.txt", false),
            (2, "report.txt", false),
            (3, "Folder", true),
        ]
        .into_iter()
        .map(|(id, name, is_dir)| foldex_shell::recycle::Item {
            id: vec![id],
            name: name.into(),
            original_path: PathBuf::from("C:/original").join(name),
            deleted_at: id as i64,
            size: id as u64 * 1024,
            is_dir,
            file_system_path: None,
        })
        .collect();
        let model = Model::new(items);
        let request = pane.recycle_view_request(model).unwrap();
        assert!(pane.apply_recycle_view(Service::prepare_inert(request)));
        (engine, pane)
    }

    #[test]
    fn virtual_bin_never_exposes_paths_or_schedules_a_folder_scan() {
        let (_engine, mut pane) = fixture();
        pane.set_active(true);
        pane.prepare_view(&egui::Context::default());
        pane.toggle_recursive();
        pane.up();
        pane.select_all();
        assert!(pane.is_recycle_bin() && pane.is_virtual());
        assert_eq!(pane.nav_generation, 0);
        assert!(!pane.recursive && !pane.scanning);
        assert!(pane.path_at(0).is_none());
        assert!(pane.primary_selected_path().is_none());
        assert!(pane.selected_paths().is_empty());
        for command in [
            SelectionCommand::Copy,
            SelectionCommand::Cut,
            SelectionCommand::CopyPaths,
            SelectionCommand::Rename,
        ] {
            assert!(pane.request_selection(command).is_none());
        }
        assert!(matches!(
            pane.request_selection(SelectionCommand::Properties),
            Some(Action::RecycleBin(Intent::PropertiesSelected))
        ));
        assert!(matches!(
            pane.request_selection(SelectionCommand::Delete(DeleteMode::Permanent)),
            Some(Action::RecycleBin(Intent::DeleteSelected))
        ));
        pane.refresh();
        assert!(pane.recycle_refresh_requested && pane.nav_generation == 0);
    }

    #[test]
    fn bin_filter_counts_and_columns_use_only_the_injected_metadata() {
        let (_engine, mut pane) = fixture();
        let configured = pane.stored_columns();
        let visible = configured
            .columns
            .iter()
            .filter(|setting| setting.visible)
            .map(|setting| setting.column)
            .collect::<Vec<_>>();
        assert_eq!(
            visible,
            [
                columns::Column::Name,
                columns::Column::OriginalLocation,
                columns::Column::DateDeleted,
                columns::Column::Type,
                columns::Column::Size
            ]
        );
        let model = pane.recycle_model.as_ref().unwrap().clone();
        pane.filter = "report".into();
        pane.refilter();
        let request = pane.recycle_view_request(model).unwrap();
        pane.apply_recycle_view(Service::prepare_inert(request));
        assert_eq!(pane.view_len(), 2);
        pane.select_all();
        assert_eq!(pane.benchmark_footer_counts(), (Some((0, 2)), (0, 2)));
        assert_eq!(pane.recycle_target(false).unwrap().count(), 2);
        assert_eq!(
            pane.recycle_column_value(columns::Column::OriginalLocation, 0)
                .text,
            "C:/original"
        );
        assert!(!pane.recycle_column_value(columns::Column::Size, 2).busy);
    }

    #[test]
    fn a_late_recycle_view_cannot_replace_a_new_route_or_query() {
        let (_engine, mut pane) = fixture();
        let model = pane.recycle_model.as_ref().unwrap().clone();
        pane.filter = "report".into();
        pane.refilter();
        let old = Service::prepare_inert(pane.recycle_view_request(model.clone()).unwrap());
        pane.filter = "Folder".into();
        pane.refilter();
        assert!(!pane.apply_recycle_view(old));
        let newer = Service::prepare_inert(pane.recycle_view_request(model).unwrap());
        pane.navigate_home(true);
        assert!(!pane.apply_recycle_view(newer));
        assert!(pane.snapshot.is_none());
    }

    #[test]
    fn recycle_toolbar_and_footer_remain_separate_at_narrow_and_large_font_sizes() {
        for (width, font) in [(320.0, 24.0), (360.0, 14.0), (760.0, 17.0), (1200.0, 24.0)] {
            let (_engine, mut pane) = fixture();
            let ctx = egui::Context::default();
            let mut theme = Theme::default();
            theme.font_size = font;
            theme.reduced_motion = true;
            theme.apply(&ctx);
            ctx.data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
            let mut bounds = Rect::NOTHING;
            for frame in 0..3 {
                let output = ctx.run_ui(
                    egui::RawInput {
                        time: Some(frame as f64 / 60.0),
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(width, 600.0))),
                        ..Default::default()
                    },
                    |ui| {
                        bounds = ui.max_rect();
                        let actions = pane.ui(ui, &theme, true, false, None);
                        assert!(actions.is_empty());
                    },
                );
                for label in ["Empty Bin", "Restore All"] {
                    let text = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text() == label => Some(text),
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("missing {label} at {width}/{font}"));
                    assert!(!text.galley.elided, "elided {label} at {width}/{font}");
                    let id = Id::new((
                        pane.id,
                        "recycle-control",
                        if label == "Empty Bin" { 0usize } else { 1usize },
                    ));
                    let button = ctx.read_response(id).unwrap().rect;
                    let inner = button.shrink2(vec2(8.0 * theme.metrics().scale, 0.0));
                    let text_rect = Rect::from_min_size(text.pos, text.galley.size());
                    assert!(
                        inner.expand(0.01).contains_rect(text_rect),
                        "clipped {label} at {width}/{font}"
                    );
                }
                output.drop_without_applying_deltas();
            }
            let probe = crate::app::recycle_ui::probe(&ctx, pane.id).unwrap();
            assert!(
                bounds.contains_rect(probe.empty),
                "empty {width}/{font}: {:?}",
                probe.empty
            );
            assert!(
                bounds.contains_rect(probe.restore_all),
                "restore {width}/{font}"
            );
            assert!(!probe.empty.intersect(probe.restore_all).is_positive());
            assert!(!probe.address.intersect(probe.empty).is_positive());
            assert!(!probe.address.intersect(probe.restore_all).is_positive());
            assert!(probe.address.bottom() <= probe.body.top());
            assert!(probe.body.bottom() <= probe.footer.top());
            assert!(
                probe
                    .footer
                    .contains_rect(pane.benchmark_footer_search_rect())
            );
            assert!(
                probe
                    .footer
                    .contains_rect(pane.benchmark_footer_view_rect())
            );
            assert!(!pane.benchmark_footer_expand_rect().is_positive());
            assert!(!probe.rows.is_empty());
        }
    }
}
