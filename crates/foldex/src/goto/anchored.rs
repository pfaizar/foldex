use super::*;
use egui::Modifiers;
use foldex_ui::components;

impl AnchorMenu {
    fn new(
        anchor: Rect,
        trigger: Id,
        children: bool,
        current: Option<PathBuf>,
        trail: Vec<(PathBuf, Id)>,
    ) -> Self {
        let current_lower = current
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let trail_at = trail.iter().position(|(_, id)| *id == trigger).unwrap_or(0);
        Self {
            anchor,
            trigger,
            children,
            trail,
            trail_at,
            visual_origin: None,
            current,
            current_lower,
            follow_current: children,
            pending_current: children,
            revision: 1,
            width_key: None,
            width: 220.0,
            visual_width: 0.0,
            view_ready: !children,
            child_depth: 0,
        }
    }
}

impl GoTo {
    pub fn folder_menu(
        engine: &Engine,
        target: u64,
        path: PathBuf,
        current: Option<PathBuf>,
        anchor: Rect,
        trigger: Id,
        trail: Vec<(PathBuf, Id)>,
    ) -> Self {
        let mut popup = Self::new(engine, target, Some(path), Vec::new());
        popup.input.clear();
        popup.select_all = false;
        popup.menu = Some(AnchorMenu::new(anchor, trigger, true, current, trail));
        popup.set_return_focus(trigger);
        popup
    }
    pub fn ancestor_menu(
        engine: &Engine,
        target: u64,
        locations: Vec<Location>,
        anchor: Rect,
        trigger: Id,
    ) -> Self {
        let mut popup = Self::new(engine, target, None, locations);
        popup.select_all = false;
        popup.menu = Some(AnchorMenu::new(anchor, trigger, false, None, Vec::new()));
        popup.set_return_focus(trigger);
        popup.cursor = (!popup.common_rows.is_empty()).then_some(0);
        popup
    }
    pub fn active_breadcrumb(&self, target: u64) -> Option<Id> {
        (target == self.target && !self.closing)
            .then(|| self.menu.as_ref().map(|menu| menu.trigger))
            .flatten()
    }
    pub fn ancestor_count(&self) -> Option<usize> {
        (!self.closing && self.menu.as_ref().is_some_and(|menu| !menu.children))
            .then_some(self.common.len())
    }
    pub fn update_ancestors(&mut self, locations: Vec<Location>) {
        if locations.is_empty() {
            self.close();
            return;
        }
        let selected = self
            .cursor
            .and_then(|row| self.destination(row))
            .map(|(path, _)| path);
        self.common = locations;
        self.filter_menu();
        if let Some(path) = selected
            && let Some(row) = self
                .common_rows
                .iter()
                .position(|index| self.common[*index].path == path)
        {
            self.cursor = Some(row);
            self.scroll_to = Some(row);
        }
    }
    pub fn update_anchor(&mut self, anchor: Option<Rect>) {
        if let Some(menu) = &mut self.menu {
            if let Some(anchor) = anchor {
                menu.anchor = anchor;
            } else if self.opened_frame.is_some() && !self.closing {
                self.close();
            }
        }
    }
    pub fn release_focus(&mut self, ctx: &egui::Context) {
        let members = if self.menu.is_some() {
            vec![
                Id::new("breadcrumb-filter"),
                Id::new("breadcrumb-close"),
                Id::new("breadcrumb-results-focus"),
            ]
        } else {
            vec![
                Id::new("goto-input"),
                Id::new("goto-results-focus"),
                Id::new(("goto-action", 0usize)),
                Id::new(("goto-action", 1usize)),
                Id::new(("goto-action", 2usize)),
                Id::new(("goto-action", 3usize)),
            ]
        };
        self.focus_scope.update(ctx, members[0], &members, false);
    }

    pub(super) fn filter_menu(&mut self) {
        let children = {
            let menu = self.menu.as_mut().unwrap();
            menu.follow_current = false;
            menu.pending_current = false;
            menu.revision = menu.revision.wrapping_add(1);
            menu.children
        };
        if children {
            self.query.text = self.input.clone();
            self.refilter();
        } else {
            let matcher = foldex_match::Matcher::new(&self.input);
            let mut rows: Vec<_> = self
                .common
                .iter()
                .enumerate()
                .filter_map(|(row, location)| {
                    matcher
                        .score(&location.label)
                        .or_else(|| matcher.score(&location.path.to_string_lossy()))
                        .map(|score| (row, score))
                })
                .collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            self.common_rows = rows.into_iter().map(|(row, _)| row).collect();
            self.cursor = (!self.common_rows.is_empty()).then_some(0);
            self.view_ready = true;
            self.menu.as_mut().unwrap().view_ready = true;
        }
    }

    fn shift_menu(&mut self, direction: isize) -> bool {
        let Some(menu) = &mut self.menu else {
            return false;
        };
        if !menu.children {
            return false;
        }
        let Some(next) = menu
            .trail_at
            .checked_add_signed(direction)
            .filter(|next| *next < menu.trail.len())
        else {
            return false;
        };
        let (path, trigger) = menu.trail[next].clone();
        menu.trail_at = next;
        menu.child_depth = 0;
        menu.trigger = trigger;
        self.focus_scope.set_return_focus(trigger);
        menu.current = menu.trail.get(next + 1).map(|(path, _)| path.clone());
        menu.current_lower = menu
            .current
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        menu.follow_current = true;
        menu.pending_current = true;
        menu.view_ready = false;
        menu.revision = menu.revision.wrapping_add(1);
        menu.width_key = None;
        self.input.clear();
        self.query.text.clear();
        self.cursor = None;
        self.scroll_to = Some(0);
        self.error = None;
        self.directory = Some(path.clone());
        self.navigation = self.browser.navigate(path, false);
        self.loading = true;
        self.view_ready = false;
        if let Some(snapshot) = self.snapshot.take() {
            self.retire(snapshot);
        }
        if let Some(snapshot) = self.pending.take() {
            self.retire(snapshot);
        }
        self.indices = Arc::from([]);
        true
    }

    fn enter_menu_folder(&mut self, row: usize) -> bool {
        let Some((path, true)) = self.destination(row) else {
            return false;
        };
        let menu = self.menu.as_ref().unwrap();
        if !menu.children || menu.child_depth >= 32 {
            return false;
        }
        self.navigate_menu_folder(path, None, menu.child_depth + 1);
        true
    }

    fn leave_menu_folder(&mut self) -> bool {
        if self.menu.as_ref().is_none_or(|menu| menu.child_depth == 0) {
            return false;
        }
        let Some(current) = self.directory.clone() else {
            return false;
        };
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            return false;
        };
        let depth = self.menu.as_ref().unwrap().child_depth - 1;
        self.navigate_menu_folder(parent, Some(current), depth);
        true
    }

    fn navigate_menu_folder(&mut self, path: PathBuf, current: Option<PathBuf>, depth: usize) {
        let menu = self.menu.as_mut().unwrap();
        menu.current_lower = current
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        menu.current = current;
        menu.child_depth = depth;
        menu.follow_current = true;
        menu.pending_current = true;
        menu.view_ready = false;
        menu.revision = menu.revision.wrapping_add(1);
        self.input.clear();
        self.query.text.clear();
        self.cursor = None;
        self.scroll_to = Some(0);
        self.error = None;
        self.directory = Some(path.clone());
        self.navigation = self.browser.navigate(path, false);
        self.loading = true;
        self.view_ready = false;
        if let Some(snapshot) = self.snapshot.take() {
            self.retire(snapshot);
        }
        if let Some(snapshot) = self.pending.take() {
            self.retire(snapshot);
        }
        self.indices = Arc::from([]);
    }

    fn menu_row_data(&self, row: usize) -> Option<(&str, Icon, Id)> {
        if self.directory.is_some() {
            let snapshot = self.snapshot.as_ref()?;
            let entry = snapshot.entries.get(*self.indices.get(row)? as usize)?;
            Some((
                &entry.name,
                foldex_ui::file_icon(&entry.name, entry.is_dir),
                Id::new(("breadcrumb-entry", &snapshot.path, &entry.raw_name)),
            ))
        } else {
            let location = self.common.get(*self.common_rows.get(row)?)?;
            Some((
                &location.label,
                location.icon,
                Id::new(("breadcrumb-ancestor", &location.path)),
            ))
        }
    }

    fn select_current_folder(&mut self) {
        let Some(menu) = &mut self.menu else {
            return;
        };
        if !menu.pending_current {
            return;
        }
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        menu.pending_current = false;
        let Some(name) = menu.current.as_ref().and_then(|path| path.file_name()) else {
            self.cursor = (!self.indices.is_empty()).then_some(0);
            return;
        };
        // Empty queries retain folders-first natural name order. Locate the active
        // branch in logarithmic time, including directories with many children.
        if let Ok(row) = self.indices.binary_search_by(|index| {
            let entry = &snapshot.entries[*index as usize];
            (!entry.is_dir)
                .cmp(&false)
                .then_with(|| foldex_core::natural_cmp(entry.lowercase(), &menu.current_lower))
                .then_with(|| entry.raw_name.as_os_str().cmp(name))
        }) {
            self.cursor = Some(row);
            self.scroll_to = Some(row);
        }
    }

    fn is_current_folder(&self, row: usize) -> bool {
        self.menu
            .as_ref()
            .and_then(|menu| menu.current.as_ref())
            .and_then(|path| path.file_name())
            .zip(self.snapshot.as_ref().and_then(|snapshot| {
                self.indices
                    .get(row)
                    .and_then(|index| snapshot.entries.get(*index as usize))
            }))
            .is_some_and(|(name, entry)| entry.raw_name.as_os_str() == name)
    }

    pub(super) fn anchored_ui(&mut self, ctx: &egui::Context, t: &Theme) -> Option<Destination> {
        self.opened_frame.get_or_insert(ctx.cumulative_frame_nr());
        let input_id = Id::new("breadcrumb-filter");
        let list_id = Id::new("breadcrumb-results-focus");
        let members = [input_id, Id::new("breadcrumb-close"), list_id];
        self.focus_scope
            .update_animated(ctx, input_id, &members, self.closing, self.finished());
        if !self.closing {
            self.focus_scope.redirect_text(ctx, list_id, input_id);
        }
        // This searchable menu is one temporary surface. Tab leaves it rather
        // than walking every row (including virtualized rows) as separate stops.
        if !self.closing && self.focus_scope.accepts_keys(ctx, &members) {
            let backwards = take_key(ctx, Modifiers::SHIFT, Key::Tab);
            if backwards || take_key(ctx, Modifiers::NONE, Key::Tab) {
                self.focus = false;
                self.tab_exit = Some(backwards);
                self.close();
                self.release_focus(ctx);
                ctx.memory_mut(|memory| {
                    memory.move_focus(if backwards {
                        egui::FocusDirection::Previous
                    } else {
                        egui::FocusDirection::Next
                    })
                });
            }
            let left = take_key(ctx, Modifiers::ALT, Key::ArrowLeft);
            let right = take_key(ctx, Modifiers::ALT, Key::ArrowRight);
            if (left || right) && self.shift_menu(if left { -1 } else { 1 }) {
                ctx.request_repaint();
            }
        }
        self.poll();
        self.select_current_folder();
        let m = t.metrics();
        let scale = m.scale;
        let screen = ctx.content_rect();
        let key = (
            self.menu.as_ref().unwrap().revision,
            self.snapshot.as_ref().map_or(0, |snapshot| snapshot.id),
            self.len(),
            t.font_size.to_bits(),
            ctx.pixels_per_point().to_bits(),
            foldex_ui::fonts::revision(ctx),
        );
        if self.menu.as_ref().unwrap().width_key != Some(key) {
            let longest = (0..self.len().min(128))
                .filter_map(|row| self.menu_row_data(row))
                .map(|(label, _, _)| {
                    foldex_ui::fonts::request_text(ctx, label);
                    ctx.fonts_mut(|fonts| fonts.layout_no_wrap(label.into(), t.font(), t.fg()))
                        .size()
                        .x
                })
                .fold(0.0, f32::max);
            let menu = self.menu.as_mut().unwrap();
            let measured = (longest + 54.0 * scale).clamp(220.0 * scale, 460.0 * scale);
            let new_style = menu
                .width_key
                .is_none_or(|old| (old.3, old.4, old.5) != (key.3, key.4, key.5));
            menu.width = if new_style {
                measured
            } else {
                menu.width.max(measured)
            };
            menu.width_key = Some(key);
        }
        let menu = self.menu.as_mut().unwrap();
        let width_target = menu.width.min((screen.width() - 12.0 * scale).max(1.0));
        if menu.visual_width == 0.0 {
            menu.visual_width = width_target;
        }
        if foldex_ui::motion::popup_gains(ctx, t.duration()).step_pixels(
            &mut menu.visual_width,
            width_target,
            foldex_ui::motion::MotionProfile::Standard,
        ) {
            ctx.request_repaint();
        }
        let width = menu.visual_width;
        let anchor = menu.anchor;
        let children = menu.children;
        let row_height = components::menu_row_height(t);
        let header_height = m.toolbar;
        let safe_top = screen.top() + 36.0;
        let below = (anchor.bottom() + m.gap).max(safe_top);
        let above = (anchor.top() - m.gap).min(screen.bottom() - m.inset);
        let below_room = (screen.bottom() - m.inset - below).max(1.0);
        let above_room = (above - safe_top).max(1.0);
        let empty_text = (self.len() == 0).then(|| {
            self.error.clone().unwrap_or_else(|| {
                if !self.view_ready && !self.loading {
                    "Filtering items…"
                } else if self.loading {
                    "Opening folder…"
                } else if self.input.is_empty() {
                    "This folder is empty"
                } else {
                    "No matching items"
                }
                .into()
            })
        });
        let empty_height = empty_text.as_ref().map(|text| {
            components::message_galley(
                ctx,
                t,
                text,
                width - 32.0 * scale,
                row_height * 3.0,
                t.dim(),
            )
            .size()
            .y + 8.0 * scale
        });
        let desired = header_height
            + 8.0 * scale
            + empty_height
                .unwrap_or(self.len().max(1).min(12) as f32 * row_height)
                .max(row_height);
        let open_below = desired <= below_room || below_room >= above_room;
        let room = if open_below { below_room } else { above_room };
        let rows = ((room - header_height - 8.0 * scale) / row_height)
            .floor()
            .max(1.0) as usize;
        let target = (header_height
            + 8.0 * scale
            + empty_height
                .unwrap_or(self.len().max(1).min(rows).min(12) as f32 * row_height)
                .max(row_height))
        .min(room);
        if foldex_ui::motion::popup_gains(ctx, t.duration()).step_pixels(
            &mut self.height,
            if self.closing { 0.0 } else { target },
            foldex_ui::motion::MotionProfile::Standard,
        ) {
            ctx.request_repaint();
        }
        if self.finished() {
            self.focus_scope.update(ctx, input_id, &members, false);
            return None;
        }
        let left = anchor.left().clamp(
            screen.left() + m.inset,
            (screen.right() - width - m.inset).max(screen.left() + m.inset),
        );
        let top = if open_below {
            below
        } else {
            above - self.height
        };
        let menu = self.menu.as_mut().unwrap();
        let origin = menu.visual_origin.get_or_insert(pos2(left, top));
        let gains = foldex_ui::motion::layout_gains(ctx, t.duration());
        let moving_x = gains.step_pixels(
            &mut origin.x,
            left,
            foldex_ui::motion::MotionProfile::Standard,
        );
        let moving_y = gains.step_pixels(
            &mut origin.y,
            top,
            foldex_ui::motion::MotionProfile::Standard,
        );
        origin.x = origin.x.clamp(
            screen.left() + m.inset,
            (screen.right() - width - m.inset).max(screen.left() + m.inset),
        );
        origin.y = origin.y.clamp(
            safe_top,
            (screen.bottom() - self.height - m.inset).max(safe_top),
        );
        if moving_x || moving_y {
            ctx.request_repaint();
        }
        let outer = Rect::from_min_size(*origin, vec2(width, self.height));
        self.motion.set_pixels_per_point(ctx.pixels_per_point());
        self.motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ctx, t.duration()));
        let mut activate = None;
        if !self.closing && self.focus_scope.accepts_keys(ctx, &members) {
            if take_key(ctx, Modifiers::NONE, Key::Escape) {
                if !self.leave_menu_folder() {
                    self.close();
                }
            }
            let results_focus = ctx.memory(|memory| memory.has_focus(list_id));
            if results_focus || ctx.memory(|memory| memory.has_focus(input_id)) {
                let down = take_key(ctx, Modifiers::NONE, Key::ArrowDown);
                let up = take_key(ctx, Modifiers::NONE, Key::ArrowUp);
                let page_down = take_key(ctx, Modifiers::NONE, Key::PageDown);
                let page_up = take_key(ctx, Modifiers::NONE, Key::PageUp);
                let home = (results_focus || self.input.is_empty())
                    && take_key(ctx, Modifiers::NONE, Key::Home);
                let end = (results_focus || self.input.is_empty())
                    && take_key(ctx, Modifiers::NONE, Key::End);
                if self.len() > 0 && (down || up || home || end || page_down || page_up) {
                    let row = if home {
                        0
                    } else if end {
                        self.len() - 1
                    } else if page_down {
                        self.cursor
                            .unwrap_or(0)
                            .saturating_add(rows.min(12).max(1))
                            .min(self.len() - 1)
                    } else if page_up {
                        self.cursor.unwrap_or(0).saturating_sub(rows.min(12).max(1))
                    } else if down {
                        self.cursor.map_or(0, |row| (row + 1).min(self.len() - 1))
                    } else {
                        self.cursor.unwrap_or(1).saturating_sub(1)
                    };
                    self.cursor = Some(row);
                    self.scroll_to = Some(row);
                    let menu = self.menu.as_mut().unwrap();
                    menu.follow_current = false;
                    menu.pending_current = false;
                    ctx.request_repaint();
                }
                if down || up || page_down || page_up || home || end {
                    self.focus = false;
                    ctx.memory_mut(|memory| memory.request_focus(list_id));
                }
                if results_focus && take_key(ctx, Modifiers::NONE, Key::ArrowRight) {
                    if let Some(row) = self.cursor {
                        self.enter_menu_folder(row);
                    }
                }
                if results_focus && take_key(ctx, Modifiers::NONE, Key::ArrowLeft) {
                    if !self.leave_menu_folder() {
                        self.close();
                    }
                }
                if take_key(ctx, Modifiers::CTRL, Key::Enter) {
                    activate = Some(true);
                } else if take_key(ctx, Modifiers::NONE, Key::Enter)
                    || (results_focus && take_key(ctx, Modifiers::NONE, Key::Space))
                {
                    activate = Some(false);
                }
            }
        }
        let mut changed = false;
        let mut picked = None;
        components::popup_surface(
            ctx,
            egui::Area::new(Id::new("breadcrumb-menu"))
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            !self.closing,
            |ui| {
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(outer.intersect(screen));
                ui.painter().rect_filled(
                    outer,
                    t.radius.saturating_add(2),
                    Theme::color(t.surface),
                );
                ui.painter().rect_stroke(
                    outer,
                    t.radius.saturating_add(2),
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                let header = Rect::from_min_size(outer.min, vec2(width, header_height));
                let close = Rect::from_center_size(
                    pos2(
                        header.right() - m.inset - m.control * 0.5,
                        header.center().y,
                    ),
                    vec2(m.control, m.control),
                );
                let field = Rect::from_min_max(
                    header.min + vec2(m.inset, 0.0),
                    pos2(close.left() - m.gap, header.bottom()),
                );
                if self.focus
                    && !self.closing
                    && components::can_focus(ui)
                    && !ui.memory(|memory| memory.has_focus(input_id))
                {
                    ui.memory_mut(|memory| memory.request_focus(input_id));
                }
                let input = components::search_input(
                    ui,
                    field,
                    input_id,
                    t,
                    &mut self.input,
                    if children {
                        "Filter items…"
                    } else {
                        "Filter ancestors…"
                    },
                    1024,
                );
                if !ui.is_sizing_pass() && input.response.has_focus() {
                    self.focus = false;
                }
                changed = input.response.changed();
                result_input(ctx, input_id, list_id, !self.closing);
                if components::icon_button_at(
                    ui,
                    close,
                    Id::new("breadcrumb-close"),
                    t,
                    Icon::Close,
                    false,
                )
                .on_hover_text("Close · Escape")
                .clicked()
                {
                    self.close();
                }
                ui.painter().hline(
                    header.x_range(),
                    header.bottom(),
                    Stroke::new(1.0, t.line()),
                );
                let body = Rect::from_min_max(
                    pos2(outer.left() + 4.0 * scale, header.bottom() + 4.0 * scale),
                    pos2(
                        outer.right() - 4.0 * scale,
                        (outer.top() + target - 4.0 * scale).max(header.bottom() + 4.0 * scale),
                    ),
                );
                result_surface(
                    ui,
                    body.intersect(outer),
                    list_id,
                    if children {
                        "Folder items"
                    } else {
                        "Ancestors"
                    },
                );
                if body.height() > 0.0 {
                    ui.scope_builder(
                        UiBuilder::new()
                            .max_rect(body)
                            .accessibility_parent(list_id),
                        |ui| {
                            ui.set_clip_rect(body.intersect(outer));
                            ui.spacing_mut().item_spacing.y = 0.0;
                            let scroll = components::menu_scroll_area_for_content(
                                ui,
                                t,
                                "breadcrumb-results",
                                body.height(),
                                self.len() as f32 * row_height,
                            );
                            scroll.show_rows(ui, row_height, self.len(), |ui, range| {
                                if body.height() >= row_height
                                    && let Some(row) = self.scroll_to.take()
                                {
                                    components::reveal_menu_row(
                                        ui,
                                        t,
                                        row,
                                        range.start,
                                        row_height,
                                    );
                                }
                                for row in range {
                                    let Some((label, glyph, id)) = self.menu_row_data(row) else {
                                        continue;
                                    };
                                    let label = label.to_owned();
                                    let current = self.is_current_folder(row);
                                    let (target, _) = ui.allocate_exact_size(
                                        vec2(ui.available_width(), row_height),
                                        Sense::hover(),
                                    );
                                    let origin =
                                        target.min.to_vec2() - vec2(0.0, row as f32 * row_height);
                                    let placement = self.motion.place_in_view(
                                        id,
                                        Rect::from_min_size(
                                            pos2(0.0, row as f32 * row_height),
                                            target.size(),
                                        ),
                                        true,
                                        ui.clip_rect().translate(-origin),
                                    );
                                    let rect = placement.rect.translate(origin);
                                    ui.painter().rect_filled(rect, 0, Theme::color(t.surface));
                                    let mut row_ui =
                                        ui.new_child(UiBuilder::new().id_salt(id).max_rect(rect));
                                    row_ui.set_clip_rect(ui.clip_rect());
                                    row_ui.multiply_opacity(placement.opacity);
                                    let response = result_choice_row(
                                        &row_ui,
                                        rect,
                                        id.with("hit"),
                                        t,
                                        &label,
                                        glyph,
                                        self.cursor == Some(row),
                                        current.then_some(Icon::Back),
                                    );
                                    result_row(
                                        &row_ui,
                                        &response,
                                        list_id,
                                        input_id,
                                        &label,
                                        self.cursor == Some(row),
                                        true,
                                    );
                                    components::focus_ring(
                                        &row_ui,
                                        rect,
                                        list_id.with(row),
                                        t,
                                        self.cursor == Some(row)
                                            && ctx.memory(|memory| memory.has_focus(list_id)),
                                    );
                                    if response.clicked() || response.middle_clicked() {
                                        picked = Some((
                                            row,
                                            response.middle_clicked()
                                                || ctx.input(|input| input.modifiers.ctrl),
                                        ));
                                    }
                                    if !components::tooltips_blocked(ctx) {
                                        response.on_hover_ui(|ui| {
                                            if let Some((path, _)) = self.destination(row) {
                                                ui.label(path.to_string_lossy());
                                            }
                                        });
                                    }
                                }
                            });
                            if let Some(text) = &empty_text {
                                components::message_at(
                                    ui,
                                    body,
                                    Id::new("breadcrumb-message"),
                                    t,
                                    text,
                                    if self.error.is_some() {
                                        Theme::color(t.danger)
                                    } else {
                                        t.dim()
                                    },
                                );
                            }
                        },
                    );
                }
            },
        );
        if self.motion.is_moving() {
            ctx.request_repaint();
        }
        if changed {
            self.refresh_input(false);
            activate = None;
            picked = None;
        }
        if activate.is_some() {
            let menu = self.menu.as_mut().unwrap();
            if !menu.view_ready {
                activate = None;
            }
        }
        if !self.closing
            && let Some((row, new_tab)) =
                picked.or_else(|| activate.map(|new_tab| (self.cursor.unwrap_or(0), new_tab)))
            && let Some((path, is_dir)) = self.destination(row)
        {
            self.close();
            self.focus_scope.update(ctx, input_id, &members, false);
            return Some(Destination {
                path,
                keep_open: false,
                new_tab,
                is_file: !is_dir,
            });
        }
        if !self.closing
            && ctx.cumulative_frame_nr() > self.opened_frame.unwrap_or_default()
            && ctx.input(|input| {
                input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|point| !outer.contains(point))
            })
        {
            self.close();
            ctx.request_repaint();
        }
        self.focus_scope
            .update_animated(ctx, input_id, &members, self.closing, self.finished());
        None
    }
}
