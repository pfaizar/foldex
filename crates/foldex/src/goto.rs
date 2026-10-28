//! Floating path completion; all directory access uses the background browser.
use crate::commands::{
    SurfaceFocus, result_choice_row, result_input, result_row, result_surface, take_key,
};
use egui::{Id, Key, Rect, Sense, Stroke, StrokeKind, UiBuilder, pos2, vec2};
use foldex_core::{Browser, Engine, Event, Query, Snapshot};
use foldex_ui::{Icon, Theme};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
mod anchored;

struct AnchorMenu {
    anchor: Rect,
    trigger: Id,
    children: bool,
    trail: Vec<(PathBuf, Id)>,
    trail_at: usize,
    visual_origin: Option<egui::Pos2>,
    current: Option<PathBuf>,
    current_lower: String,
    follow_current: bool,
    pending_current: bool,
    revision: u64,
    width_key: Option<(u64, u64, usize, u32, u32, usize)>,
    width: f32,
    visual_width: f32,
    view_ready: bool,
    child_depth: usize,
}

#[derive(Clone)]
pub struct Location {
    pub label: String,
    pub path: PathBuf,
    pub icon: Icon,
}
pub struct Destination {
    pub path: PathBuf,
    pub keep_open: bool,
    pub new_tab: bool,
    pub is_file: bool,
}
pub struct GoTo {
    pub target: u64,
    browser: Browser,
    input: String,
    common: Vec<Location>,
    common_rows: Vec<usize>,
    directory: Option<PathBuf>,
    snapshot: Option<Arc<Snapshot>>,
    pending: Option<Arc<Snapshot>>,
    retired: Vec<Arc<Snapshot>>,
    indices: Arc<[u32]>,
    query: Query,
    navigation: u64,
    filtering: u64,
    cursor: Option<usize>,
    scroll_to: Option<usize>,
    loading: bool,
    error: Option<String>,
    focus: bool,
    select_all: bool,
    closing: bool,
    height: f32,
    opened_frame: Option<u64>,
    history: Vec<String>,
    history_at: usize,
    motion: foldex_ui::motion::LayoutMotion,
    menu: Option<AnchorMenu>,
    focus_scope: SurfaceFocus,
    view_ready: bool,
    tab_exit: Option<bool>,
}
impl GoTo {
    pub fn new(
        engine: &Engine,
        target: u64,
        initial: Option<PathBuf>,
        common: Vec<Location>,
    ) -> Self {
        let input = initial.map_or_else(String::new, |p| with_separator(&p));
        let mut this = Self {
            target,
            browser: engine.browser(),
            input,
            common,
            common_rows: vec![],
            directory: None,
            snapshot: None,
            pending: None,
            retired: vec![],
            indices: Arc::from([]),
            query: Query::default(),
            navigation: 0,
            filtering: 0,
            cursor: None,
            scroll_to: None,
            loading: false,
            error: None,
            focus: true,
            select_all: true,
            closing: false,
            height: 0.0,
            opened_frame: None,
            history: vec![],
            history_at: 0,
            motion: foldex_ui::motion::LayoutMotion::new(128),
            menu: None,
            focus_scope: Default::default(),
            view_ready: false,
            tab_exit: None,
        };
        this.query.show_hidden = true;
        this.refresh_input(true);
        this
    }
    pub fn close(&mut self) {
        self.closing = true;
        self.browser.set_active(false);
    }
    pub fn set_return_focus(&mut self, id: Id) {
        self.focus_scope.set_return_focus(id);
    }
    /// Consume a breadcrumb-menu Tab dismissal for the root focus router.
    pub fn take_tab_exit(&mut self) -> Option<bool> {
        self.tab_exit.take()
    }
    pub fn finished(&self) -> bool {
        self.closing && self.height <= 0.25
    }
    pub(crate) fn input_text(&self) -> &str {
        &self.input
    }
    fn retire(&mut self, s: Arc<Snapshot>) {
        if let Err(s) = self.browser.retire(s) {
            self.retired.push(s);
        }
    }
    fn refilter(&mut self) {
        self.view_ready = false;
        if let Some(s) = self.pending.as_ref().or(self.snapshot.as_ref()).cloned() {
            if let Some(menu) = &mut self.menu {
                menu.view_ready = false;
            }
            self.filtering = self.browser.filter(s, self.query.clone());
        }
    }
    fn refresh_input(&mut self, remember: bool) {
        self.cursor = None;
        self.scroll_to = Some(0);
        self.error = None;
        if self.menu.is_some() {
            self.filter_menu();
            return;
        }
        if remember && self.history.last() != Some(&self.input) {
            self.history.truncate(self.history_at + 1);
            self.history.push(self.input.clone());
            if self.history.len() > 64 {
                self.history.remove(0);
            }
            self.history_at = self.history.len().saturating_sub(1);
        }
        let text = self.input.trim().trim_matches('"');
        let path = expand_path(text);
        if path.is_absolute() {
            let trailing = text.ends_with(['\\', '/']);
            let (base, leaf) = if trailing || (path.parent().is_none()) {
                (path.clone(), String::new())
            } else {
                (
                    path.parent().unwrap_or(&path).to_path_buf(),
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                )
            };
            self.query.text = leaf;
            if self.directory.as_ref() != Some(&base) {
                self.view_ready = false;
                self.directory = Some(base.clone());
                self.navigation = self.browser.navigate(base, false);
                self.loading = true;
                if let Some(s) = self.snapshot.take() {
                    self.retire(s);
                }
                if let Some(s) = self.pending.take() {
                    self.retire(s);
                }
                self.indices = Arc::from([]);
            } else {
                self.refilter();
            }
        } else {
            let matcher = foldex_match::Matcher::new(text);
            if self.directory.is_some() {
                self.browser.cancel_navigation();
            }
            self.directory = None;
            self.browser.set_active(false);
            if let Some(s) = self.snapshot.take() {
                self.retire(s);
            }
            if let Some(s) = self.pending.take() {
                self.retire(s);
            }
            self.indices = Arc::from([]);
            let mut rows: Vec<_> = self
                .common
                .iter()
                .enumerate()
                .filter_map(|(i, p)| {
                    matcher
                        .score(&p.label)
                        .or_else(|| matcher.score(&p.path.to_string_lossy()))
                        .map(|score| (i, score))
                })
                .collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1));
            self.common_rows = rows.into_iter().map(|(i, _)| i).collect();
            self.loading = false;
            self.view_ready = true;
        }
    }
    fn poll(&mut self) {
        self.browser
            .set_active(self.directory.is_some() && !self.closing);
        while let Some(s) = self.retired.pop() {
            if let Err(s) = self.browser.retire(s) {
                self.retired.push(s);
                break;
            }
        }
        for _ in 0..16 {
            let Some(event) = self.browser.try_recv() else {
                break;
            };
            match event {
                Event::Directory {
                    generation,
                    snapshot,
                    done,
                    ..
                } if generation == self.navigation => {
                    self.loading = !done;
                    if let Some(old) = self.pending.replace(snapshot) {
                        self.retire(old);
                    }
                    self.refilter();
                }
                Event::View {
                    generation,
                    snapshot,
                    indices,
                    ..
                } if generation == self.filtering => {
                    if self
                        .pending
                        .as_ref()
                        .or(self.snapshot.as_ref())
                        .is_some_and(|s| s.id == snapshot.id)
                    {
                        if let Some(old) = self.snapshot.replace(snapshot) {
                            self.retire(old);
                        }
                        if let Some(old) = self.pending.take() {
                            self.retire(old);
                        }
                        self.indices = indices;
                        self.view_ready = true;
                        if self.cursor.is_some_and(|row| row >= self.indices.len()) {
                            self.cursor = None;
                        }
                        self.error = None;
                        if let Some(menu) = &mut self.menu {
                            menu.view_ready = true;
                            if menu.follow_current {
                                menu.pending_current = true;
                            } else if self.cursor.is_none() && !self.indices.is_empty() {
                                self.cursor = Some(0);
                            }
                        }
                    }
                }
                Event::Error {
                    generation,
                    message,
                    ..
                } if generation == self.navigation => {
                    self.loading = false;
                    self.error = Some(crate::friendly::present(
                        "Couldn't open that folder",
                        &message,
                    ));
                    self.view_ready = true;
                    if let Some(menu) = &mut self.menu {
                        menu.view_ready = true;
                    }
                }
                _ => {}
            }
        }
    }
    fn len(&self) -> usize {
        if !self.view_ready {
            0
        } else if self.directory.is_some() {
            self.indices.len()
        } else {
            self.common_rows.len()
        }
    }
    fn destination(&self, row: usize) -> Option<(PathBuf, bool)> {
        if !self.view_ready {
            return None;
        }
        if self.directory.is_some() {
            let s = self.snapshot.as_ref()?;
            let e = s.entries.get(*self.indices.get(row)? as usize)?;
            Some((e.path(s), e.is_dir))
        } else {
            let p = self.common.get(*self.common_rows.get(row)?)?;
            Some((p.path.clone(), true))
        }
    }
    fn move_cursor(&mut self, step: isize) {
        let len = self.len();
        if len == 0 {
            self.cursor = None;
            return;
        }
        let at =
            self.cursor
                .map(|row| row as isize)
                .unwrap_or(if step > 0 { -1 } else { len as isize });
        let row = (at + step).clamp(0, len as isize - 1) as usize;
        self.cursor = Some(row);
        self.scroll_to = Some(row);
    }
    pub fn ui(&mut self, ctx: &egui::Context, t: &Theme) -> Option<Destination> {
        if self.menu.is_some() {
            return self.anchored_ui(ctx, t);
        }
        let m = t.metrics();
        let scale = m.scale;
        self.opened_frame.get_or_insert(ctx.cumulative_frame_nr());
        self.poll();
        let screen = ctx.content_rect();
        let width = (screen.width() - 40.0 * scale).max(1.0).min(720.0 * scale);
        let input_id = Id::new("goto-input");
        let list_id = Id::new("goto-results-focus");
        let show_history = width >= 460.0 * scale;
        let mut members = vec![input_id];
        if show_history && self.history_at > 0 {
            members.push(Id::new(("goto-action", 0usize)));
        }
        if show_history && self.history_at + 1 < self.history.len() {
            members.push(Id::new(("goto-action", 1usize)));
        }
        if self.directory.is_some() && Path::new(self.input.trim()).parent().is_some() {
            members.push(Id::new(("goto-action", 2usize)));
        }
        members.extend([Id::new(("goto-action", 3usize)), list_id]);
        self.focus_scope
            .update_animated(ctx, input_id, &members, self.closing, self.finished());
        if !self.closing {
            self.focus_scope.redirect_text(ctx, list_id, input_id);
            if self.focus_scope.tab(ctx, &members) {
                self.focus = false;
            }
        }
        let row_height = foldex_ui::components::menu_row_height(t);
        let max_body = (screen.height() - 32.0 - 97.0 * scale).clamp(row_height, 390.0 * scale);
        let empty_text = (self.len() == 0).then(|| {
            self.error.clone().unwrap_or_else(|| {
                if !self.view_ready && !self.loading {
                    "Filtering paths…"
                } else if self.loading {
                    "Opening folder…"
                } else {
                    "No matching paths"
                }
                .into()
            })
        });
        let empty_height = empty_text.as_ref().map(|text| {
            foldex_ui::components::message_galley(
                ctx,
                t,
                text,
                width - 36.0 * scale,
                max_body - 8.0 * scale,
                t.dim(),
            )
            .size()
            .y + 8.0 * scale
        });
        let visible_rows = (max_body / row_height).floor().max(1.0) as usize;
        let body_height = empty_height
            .unwrap_or(self.len().max(1).min(visible_rows) as f32 * row_height)
            .clamp(row_height, max_body);
        let target_height = if self.closing {
            0.0
        } else {
            body_height + 67.0 * scale
        };
        let moving = foldex_ui::motion::popup_gains(ctx, t.duration()).step_pixels(
            &mut self.height,
            target_height,
            foldex_ui::motion::MotionProfile::Standard,
        );
        if moving {
            ctx.request_repaint();
        }
        if self.finished() {
            self.focus_scope.update(ctx, input_id, &members, false);
            return None;
        }
        self.motion.set_pixels_per_point(ctx.pixels_per_point());
        self.motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ctx, t.duration()));
        let top = (screen.top() + 32.0 + 47.0 * scale)
            .min(screen.bottom() - target_height.min(screen.height() - 30.0) - 15.0)
            .max(screen.top() + 15.0);
        let outer = Rect::from_min_size(
            pos2(screen.center().x - width / 2.0, top),
            vec2(width, self.height),
        );
        let mut changed = false;
        let mut picked = None;
        let accepts_keys = !self.closing && self.focus_scope.accepts_keys(ctx, &members);
        let (mut navigate_history, mut parent) = if !accepts_keys {
            (None, false)
        } else {
            {
                let previous = take_key(ctx, egui::Modifiers::ALT, Key::ArrowLeft);
                let next = take_key(ctx, egui::Modifiers::ALT, Key::ArrowRight);
                let history = if previous && self.history_at > 0 {
                    Some(self.history_at - 1)
                } else if next && self.history_at + 1 < self.history.len() {
                    Some(self.history_at + 1)
                } else {
                    None
                };
                (history, take_key(ctx, egui::Modifiers::ALT, Key::ArrowUp))
            }
        };
        let mut activate = None;
        let mut expand = false;
        if accepts_keys {
            let results_focus = ctx.memory(|memory| memory.has_focus(list_id));
            if results_focus || ctx.memory(|memory| memory.has_focus(input_id)) {
                for (key, step) in [
                    (Key::ArrowDown, 1),
                    (Key::ArrowUp, -1),
                    (Key::PageDown, visible_rows as isize),
                    (Key::PageUp, -(visible_rows as isize)),
                ] {
                    if take_key(ctx, egui::Modifiers::NONE, key) {
                        self.move_cursor(step);
                    }
                }
                if (results_focus || self.input.is_empty())
                    && take_key(ctx, egui::Modifiers::NONE, Key::Home)
                {
                    self.move_cursor(-(self.len() as isize));
                }
                if (results_focus || self.input.is_empty())
                    && take_key(ctx, egui::Modifiers::NONE, Key::End)
                {
                    self.move_cursor(self.len() as isize);
                }
                if take_key(ctx, egui::Modifiers::CTRL, Key::Enter) {
                    activate = Some(true);
                } else if take_key(ctx, egui::Modifiers::NONE, Key::Enter)
                    || (results_focus && take_key(ctx, egui::Modifiers::NONE, Key::Space))
                {
                    activate = Some(false);
                }
                expand = results_focus && take_key(ctx, egui::Modifiers::NONE, Key::ArrowRight);
            }
            if take_key(ctx, egui::Modifiers::NONE, Key::Escape) {
                self.close();
            }
        }
        foldex_ui::components::popup_surface(
            ctx,
            egui::Area::new(Id::new("go-to-popup"))
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
                    t.radius.saturating_add(5),
                    Theme::color(t.surface),
                );
                ui.painter().rect_stroke(
                    outer,
                    t.radius.saturating_add(5),
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                let layout =
                    foldex_ui::components::PopupLayout::with_body_height(outer, t, body_height);
                let header = layout.header;
                foldex_ui::components::popup_input_icon(
                    ui,
                    header,
                    t,
                    if self.directory.is_some() {
                        Icon::Folder
                    } else {
                        Icon::Search
                    },
                );
                let action_count = if show_history { 4 } else { 2 };
                let input_rect = foldex_ui::components::popup_input_rect(header, action_count);
                ui.scope_builder(UiBuilder::new().max_rect(input_rect), |ui| {
                    let acquire_focus = self.focus && !self.closing;
                    if acquire_focus {
                        if !ui.memory(|memory| memory.has_focus(input_id)) {
                            ui.memory_mut(|m| m.request_focus(input_id));
                        }
                        if self.select_all {
                            let mut state =
                                egui::text_edit::TextEditState::load(ui.ctx(), input_id)
                                    .unwrap_or_default();
                            state
                                .cursor
                                .set_char_range(Some(egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(0),
                                    egui::text::CCursor::new(self.input.chars().count()),
                                )));
                            state.store(ui.ctx(), input_id);
                        }
                    }
                    let out = foldex_ui::components::text_input_at(
                        ui,
                        input_rect,
                        input_id,
                        t,
                        &mut self.input,
                        "Go to a folder…",
                        4096,
                    );
                    if acquire_focus {
                        self.focus = ui.is_sizing_pass() || !out.response.has_focus();
                        if !ui.is_sizing_pass() || out.response.changed() {
                            self.select_all = false;
                        }
                    }
                    changed = out.response.changed();
                    result_input(ctx, input_id, list_id, !self.closing);
                });
                for (index, (glyph, hint)) in [
                    (Icon::Back, "Previous path · Alt+Left"),
                    (Icon::Forward, "Next path · Alt+Right"),
                    (Icon::Up, "Parent folder · Alt+Up"),
                    (Icon::Close, "Close · Escape"),
                ]
                .into_iter()
                .enumerate()
                {
                    if !show_history && index < 2 {
                        continue;
                    }
                    let slot = if show_history { index } else { index - 2 };
                    let rect = foldex_ui::components::popup_action_rect(header, slot, action_count);
                    let enabled = match index {
                        0 => self.history_at > 0,
                        1 => self.history_at + 1 < self.history.len(),
                        2 => {
                            self.directory.is_some()
                                && Path::new(self.input.trim()).parent().is_some()
                        }
                        _ => true,
                    };
                    let response = foldex_ui::components::icon_button_at_enabled(
                        ui,
                        rect,
                        Id::new(("goto-action", index)),
                        t,
                        glyph,
                        false,
                        enabled,
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled() && enabled,
                            hint,
                        )
                    });
                    if response
                        .on_hover_text(hint)
                        .on_disabled_hover_text(hint)
                        .clicked()
                    {
                        match index {
                            0 if self.history_at > 0 => {
                                navigate_history = Some(self.history_at - 1)
                            }
                            1 if self.history_at + 1 < self.history.len() => {
                                navigate_history = Some(self.history_at + 1)
                            }
                            2 => parent = true,
                            3 => self.closing = true,
                            _ => {}
                        }
                    }
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    Stroke::new(1.0, t.line()),
                );
                let body = layout.body;
                let body_height = body.height();
                result_surface(ui, body.intersect(outer), list_id, "Matching paths");
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(body)
                        .accessibility_parent(list_id),
                    |ui| {
                        ui.set_clip_rect(body.intersect(outer));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let scroll = foldex_ui::components::menu_scroll_area_for_content(
                            ui,
                            t,
                            "go-to-results",
                            body_height,
                            self.len() as f32 * row_height,
                        );
                        scroll.show_rows(ui, row_height, self.len(), |ui, range| {
                            if body_height >= row_height
                                && let Some(row) = self.scroll_to.take()
                            {
                                foldex_ui::components::reveal_menu_row(
                                    ui,
                                    t,
                                    row,
                                    range.start,
                                    row_height,
                                );
                            }
                            for row in range {
                                let (label, glyph, id) = if self.directory.is_some() {
                                    let Some(s) = self.snapshot.as_ref() else {
                                        continue;
                                    };
                                    let Some(&idx) = self.indices.get(row) else {
                                        continue;
                                    };
                                    let Some(e) = s.entries.get(idx as usize) else {
                                        continue;
                                    };
                                    (
                                        &e.name,
                                        foldex_ui::file_icon(&e.name, e.is_dir),
                                        Id::new(("goto-entry", &s.path, &e.raw_name)),
                                    )
                                } else {
                                    let index = self.common_rows[row];
                                    let p = &self.common[index];
                                    (&p.label, p.icon, Id::new(("goto-location", index, &p.path)))
                                };
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
                                let r = placement.rect.translate(origin);
                                let mut row_ui =
                                    ui.new_child(UiBuilder::new().id_salt(id).max_rect(r));
                                row_ui.set_clip_rect(ui.clip_rect());
                                row_ui.multiply_opacity(placement.opacity);
                                row_ui.painter().rect_filled(r, 0, Theme::color(t.surface));
                                let response = result_choice_row(
                                    &row_ui,
                                    r,
                                    id.with("hit"),
                                    t,
                                    label,
                                    glyph,
                                    self.cursor == Some(row),
                                    None,
                                );
                                result_row(
                                    &row_ui,
                                    &response,
                                    list_id,
                                    input_id,
                                    label,
                                    self.cursor == Some(row),
                                    true,
                                );
                                foldex_ui::components::focus_ring(
                                    &row_ui,
                                    r,
                                    list_id.with(row),
                                    t,
                                    self.cursor == Some(row)
                                        && ctx.memory(|memory| memory.has_focus(list_id)),
                                );
                                if response.clicked() {
                                    picked = Some(row);
                                }
                            }
                        });
                        if let Some(text) = &empty_text {
                            foldex_ui::components::message_at(
                                ui,
                                body,
                                Id::new("goto-message"),
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
                let footer = layout.footer;
                ui.painter().line_segment(
                    [footer.left_top(), footer.right_top()],
                    Stroke::new(1.0, t.line()),
                );
                let hint = if width >= 600.0 {
                    "↑ ↓  navigate     Enter  use     Right  expand in results     Ctrl+Enter  open in panel     Esc  dismiss"
                } else if width >= 400.0 {
                    "↑ ↓  navigate     Enter  use     Right  expand in results     Esc  dismiss"
                } else {
                    "Enter  use     Esc  dismiss"
                };
                foldex_ui::components::popup_hint(
                    ui.painter(),
                    footer,
                    t,
                    hint,
                    "Enter  use     Esc  close",
                );
            },
        );
        if self.motion.is_moving() {
            ctx.request_repaint();
        }
        if changed {
            self.refresh_input(false);
            activate = None;
            expand = false;
            picked = None;
        }
        if let Some(at) = navigate_history {
            self.history_at = at;
            self.input = self.history[at].clone();
            self.refresh_input(false);
            self.focus = true;
        }
        if parent {
            let path = expand_path(self.input.trim());
            if let Some(p) = path.parent() {
                self.input = with_separator(p);
                self.refresh_input(true);
                self.focus = true;
            }
        }
        if !self.closing {
            if expand {
                if let Some((path, is_dir)) = self.destination(self.cursor.unwrap_or(0)) {
                    self.input = if is_dir {
                        with_separator(&path)
                    } else {
                        path.to_string_lossy().into_owned()
                    };
                    self.refresh_input(true);
                    self.focus = true;
                }
            } else if activate.is_some() || picked.is_some() {
                let path = if let Some(row) = picked.or(self.cursor) {
                    self.destination(row).map(|v| v.0)
                } else if self.directory.is_some() && self.view_ready {
                    Some(expand_path(self.input.trim()))
                } else {
                    self.destination(0).map(|v| v.0)
                };
                if let Some(path) = path {
                    let keep_open = activate.unwrap_or_else(|| ctx.input(|i| i.modifiers.ctrl));
                    if !keep_open {
                        self.close();
                    }
                    self.focus_scope
                        .update(ctx, input_id, &members, !self.closing);
                    return Some(Destination {
                        path,
                        keep_open,
                        new_tab: false,
                        is_file: false,
                    });
                }
            }
            if ctx.cumulative_frame_nr() > self.opened_frame.unwrap_or_default()
                && ctx.input(|i| {
                    i.pointer.any_pressed()
                        && i.pointer.interact_pos().is_some_and(|p| !outer.contains(p))
                })
            {
                self.close();
            }
        }
        self.focus_scope
            .update_animated(ctx, input_id, &members, self.closing, self.finished());
        None
    }
}
fn with_separator(path: &Path) -> String {
    let mut s = path.to_string_lossy().into_owned();
    if crate::navigation::special_route(path).is_some() {
        return s;
    }
    if !s.ends_with(['\\', '/']) {
        s.push('\\');
    }
    s
}
fn expand_path(text: &str) -> PathBuf {
    let text = text.trim().trim_matches('"');
    if text == "~" || text.starts_with("~\\") || text.starts_with("~/") {
        if let Some(p) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(p).join(text[1..].trim_start_matches(['\\', '/']));
        }
    }
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        output.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        if let Some(end) = tail.find('%') {
            if let Some(value) = std::env::var_os(&tail[..end]) {
                output.push_str(&value.to_string_lossy());
            } else {
                output.push('%');
                output.push_str(&tail[..=end]);
            }
            rest = &tail[end + 1..];
        } else {
            output.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    output.push_str(rest);
    PathBuf::from(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locations() -> Vec<Location> {
        (0..24)
            .map(|index| Location {
                label: format!("Folder {index:02}"),
                path: PathBuf::from(format!("C:\\KeyboardFixture\\{index:02}")),
                icon: Icon::Folder,
            })
            .collect()
    }
    fn key(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    fn keyboard_frame(
        ctx: &egui::Context,
        goto: &mut GoTo,
        frame: u32,
        events: Vec<egui::Event>,
    ) -> Option<Destination> {
        let mut destination = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 700.0))),
                events,
                ..Default::default()
            },
            |ui| {
                foldex_ui::components::PopupFocus::surface(
                    ui,
                    Rect::from_min_size(pos2(10.0, 10.0), vec2(100.0, 24.0)),
                    Id::new("goto-invoker"),
                    "Address",
                );
                if let Some(result) = goto.ui(
                    ui.ctx(),
                    &Theme {
                        reduced_motion: true,
                        ..Default::default()
                    },
                ) {
                    destination = Some(result);
                }
            },
        );
        output.textures_delta.clear();
        destination
    }

    #[test]
    fn goto_tab_traverses_controls_and_results_instead_of_completing_a_path() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let ctx = egui::Context::default();
        let mut goto = GoTo::new(&engine, 7, None, locations());
        goto.set_return_focus(Id::new("goto-invoker"));
        keyboard_frame(&ctx, &mut goto, 0, vec![]);
        assert!(
            keyboard_frame(
                &ctx,
                &mut goto,
                1,
                vec![key(Key::Tab, egui::Modifiers::NONE)]
            )
            .is_none()
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new(("goto-action", 3usize)))
        );
        assert_eq!(goto.input, "");
        keyboard_frame(
            &ctx,
            &mut goto,
            2,
            vec![key(Key::Tab, egui::Modifiers::NONE)],
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("goto-results-focus"))
        );
        keyboard_frame(
            &ctx,
            &mut goto,
            3,
            vec![key(Key::End, egui::Modifiers::NONE)],
        );
        assert_eq!(goto.cursor, Some(23));
        keyboard_frame(
            &ctx,
            &mut goto,
            4,
            vec![key(Key::PageUp, egui::Modifiers::NONE)],
        );
        let page =
            (390.0 / foldex_ui::components::menu_row_height(&Theme::default())).floor() as usize;
        assert_eq!(goto.cursor, Some(23usize.saturating_sub(page)));
        assert!(
            keyboard_frame(
                &ctx,
                &mut goto,
                5,
                vec![key(Key::Escape, egui::Modifiers::NONE)]
            )
            .is_none()
        );
        keyboard_frame(&ctx, &mut goto, 6, vec![]);
        assert!(goto.finished());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("goto-invoker"))
        );
    }

    #[test]
    fn goto_editing_home_and_pending_results_cannot_activate_old_rows() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let ctx = egui::Context::default();
        let mut goto = GoTo::new(&engine, 7, None, locations());
        keyboard_frame(&ctx, &mut goto, 0, vec![egui::Event::Text("Folder".into())]);
        assert_eq!(goto.input, "Folder");
        keyboard_frame(
            &ctx,
            &mut goto,
            1,
            vec![key(Key::Home, egui::Modifiers::NONE)],
        );
        assert_eq!(goto.cursor, None);
        goto.view_ready = false;
        goto.cursor = Some(12);
        assert!(
            keyboard_frame(
                &ctx,
                &mut goto,
                2,
                vec![key(Key::Enter, egui::Modifiers::NONE)]
            )
            .is_none()
        );
        assert!(!goto.closing);
        assert_eq!(goto.len(), 0);
        assert!(goto.destination(12).is_none());
    }

    #[test]
    fn anchored_menu_arrows_focus_results_and_escape_or_tab_returns_to_trigger() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let ctx = egui::Context::default();
        let trigger = Id::new("goto-invoker");
        let anchor = Rect::from_min_size(pos2(10.0, 10.0), vec2(100.0, 24.0));
        let mut goto = GoTo::ancestor_menu(&engine, 7, locations(), anchor, trigger);
        keyboard_frame(&ctx, &mut goto, 0, vec![]);
        keyboard_frame(
            &ctx,
            &mut goto,
            1,
            vec![key(Key::ArrowDown, egui::Modifiers::NONE)],
        );
        assert_eq!(goto.cursor, Some(1));
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("breadcrumb-results-focus"))
        );
        keyboard_frame(
            &ctx,
            &mut goto,
            2,
            vec![key(Key::End, egui::Modifiers::NONE)],
        );
        assert_eq!(goto.cursor, Some(23));
        assert!(
            keyboard_frame(
                &ctx,
                &mut goto,
                3,
                vec![key(Key::Escape, egui::Modifiers::NONE)]
            )
            .is_none()
        );
        assert!(goto.closing);
        keyboard_frame(&ctx, &mut goto, 4, vec![]);
        assert!(goto.finished());
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger));
        let mut goto = GoTo::ancestor_menu(&engine, 7, locations(), anchor, trigger);
        keyboard_frame(&ctx, &mut goto, 5, vec![]);
        assert!(
            keyboard_frame(
                &ctx,
                &mut goto,
                6,
                vec![key(Key::Tab, egui::Modifiers::SHIFT)]
            )
            .is_none()
        );
        assert!(goto.closing);
    }
}
