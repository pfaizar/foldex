//! A native picker for tabs, types, history and saved layouts.
//! Choice identities are opaque and unique; equal labels/paths stay independent.
use crate::commands::{
    SurfaceFocus, control, highlight, result_input, result_row, result_surface, take_key,
};
use egui::{
    Align2, Color32, FontId, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, UiBuilder, pos2,
    vec2,
};
use foldex_match::{Matcher, bounded_prefix};
use foldex_ui::{
    Icon, Theme, icon,
    motion::{LayoutMotion, MotionGains, MotionProfile},
};
use std::{collections::HashMap, sync::Arc};

const MAX_RENDER_ROWS: usize = 64;

#[derive(Clone)]
pub struct Choice {
    pub id: u64,
    pub label: String,
    pub detail: String,
    pub icon: Icon,
}
struct MatchRow {
    index: usize,
    score: i32,
}
#[derive(Clone, Copy, PartialEq)]
struct TextKey {
    fonts: usize,
    width: u32,
    font: u32,
    ppp: u32,
    foreground: Color32,
    muted: Color32,
    accent: Color32,
}
struct CachedRow {
    key: TextKey,
    label: Arc<egui::Galley>,
    detail: Arc<egui::Galley>,
    hover: f32,
    selected: f32,
    seen: u64,
}

pub struct Picker {
    title: String,
    hint: String,
    choices: Vec<Choice>,
    rows: Vec<MatchRow>,
    query: String,
    matcher: Matcher,
    selected: Option<u64>,
    scroll_to: bool,
    focus: bool,
    closing: bool,
    height: f32,
    opacity: f32,
    opened_frame: Option<u64>,
    footer: String,
    motion: LayoutMotion,
    cache: HashMap<u64, CachedRow>,
    focus_scope: SurfaceFocus,
}
impl Picker {
    /// Data stays owned by this picker; only visible rows acquire text/motion caches.
    /// Callers should use the background browser for large filesystem searches.
    pub fn new(title: impl Into<String>, hint: impl Into<String>, choices: Vec<Choice>) -> Self {
        debug_assert_eq!(
            choices
                .iter()
                .map(|choice| choice.id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            choices.len(),
            "Picker Choice.id values must be unique"
        );
        let mut this = Self {
            title: title.into(),
            hint: hint.into(),
            rows: Vec::with_capacity(choices.len()),
            choices,
            query: String::new(),
            matcher: Matcher::new(""),
            selected: None,
            scroll_to: false,
            focus: true,
            closing: false,
            height: 0.0,
            opacity: 0.0,
            opened_frame: None,
            footer: String::new(),
            motion: LayoutMotion::new(MAX_RENDER_ROWS),
            cache: HashMap::with_capacity(MAX_RENDER_ROWS),
            focus_scope: Default::default(),
        };
        this.refilter();
        this
    }
    pub fn open(&mut self) {
        self.closing = false;
        self.focus = true;
        self.opened_frame = None;
        self.query.clear();
        self.refilter();
    }
    pub fn close(&mut self) {
        self.closing = true;
    }
    pub fn set_return_focus(&mut self, id: Id) {
        self.focus_scope.set_return_focus(id);
    }
    pub fn release_focus(&mut self, ctx: &egui::Context) {
        let area = Id::new(("choice-picker", &self.title));
        self.focus_scope.update(
            ctx,
            area.with("input"),
            &[
                area.with("input"),
                area.with("close"),
                area.with("results-focus"),
            ],
            false,
        );
    }
    pub fn is_open(&self) -> bool {
        !self.closing
    }
    pub fn finished(&self) -> bool {
        self.closing && self.height <= 0.25 && self.opacity <= 0.01
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.refilter();
    }
    pub fn selected(&self) -> Option<u64> {
        self.selected
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn replace_choices(&mut self, choices: Vec<Choice>) {
        debug_assert_eq!(
            choices
                .iter()
                .map(|choice| choice.id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            choices.len(),
            "Picker Choice.id values must be unique"
        );
        self.choices = choices;
        self.cache.clear();
        self.refilter();
    }
    fn refilter(&mut self) {
        self.matcher = Matcher::new(&self.query);
        self.rows.clear();
        self.cache.clear();
        for (index, choice) in self.choices.iter().enumerate() {
            let label = self.matcher.score(&choice.label);
            let detail = self.matcher.score(&choice.detail);
            if label.is_none() && detail.is_none() {
                continue;
            }
            self.rows.push(MatchRow {
                index,
                score: label.unwrap_or(0) + (detail.unwrap_or(0) as f32 * 0.75).round() as i32,
            });
        }
        if !self.query.is_empty() {
            self.rows.sort_by(|left, right| {
                right
                    .score
                    .cmp(&left.score)
                    .then_with(|| left.index.cmp(&right.index))
            });
        }
        if !self
            .rows
            .iter()
            .any(|row| Some(self.choices[row.index].id) == self.selected)
        {
            self.selected = self.rows.first().map(|row| self.choices[row.index].id);
        }
        self.footer = format!(
            "{} results     ↑ ↓  select     Enter  use     Esc  close",
            self.rows.len()
        );
        self.scroll_to = true;
    }
    fn move_selection(&mut self, step: isize) {
        if self.rows.is_empty() {
            return;
        }
        let current = self
            .rows
            .iter()
            .position(|row| self.selected == Some(self.choices[row.index].id));
        let next = current.map_or(
            if step > 0 {
                -1
            } else {
                self.rows.len() as isize
            },
            |index| index as isize,
        ) + step;
        let next = next.clamp(0, self.rows.len() as isize - 1) as usize;
        self.selected = Some(self.choices[self.rows[next].index].id);
        self.scroll_to = true;
    }
    /// Keep calling while closing; retire the picker once `finished()` is true.
    pub fn ui(&mut self, ctx: &egui::Context, t: &Theme) -> Option<u64> {
        let m = t.metrics();
        let scale = m.scale;
        self.opened_frame.get_or_insert(ctx.cumulative_frame_nr());
        let area_id = Id::new(("choice-picker", &self.title));
        let input_id = area_id.with("input");
        let list_id = area_id.with("results-focus");
        let members = [input_id, area_id.with("close"), list_id];
        self.focus_scope
            .update(ctx, input_id, &members, !self.closing);
        self.focus_scope.redirect_text(ctx, list_id, input_id);
        if self.focus_scope.tab(ctx, &members) {
            self.focus = false;
        }
        let mut execute = false;
        if !self.closing && self.focus_scope.accepts_keys(ctx, &members) {
            let results_focus = ctx.memory(|memory| memory.has_focus(list_id));
            let search_focus = ctx.memory(|memory| memory.has_focus(input_id));
            if results_focus || search_focus {
                let row_height = foldex_ui::components::menu_row_height(t);
                let page = ((ctx.content_rect().height() - 175.0 * scale)
                    .clamp(row_height, 390.0 * scale)
                    / row_height)
                    .floor()
                    .max(1.0) as isize;
                for (key, step) in [
                    (Key::ArrowDown, 1),
                    (Key::ArrowUp, -1),
                    (Key::PageDown, page),
                    (Key::PageUp, -page),
                ] {
                    if take_key(ctx, Modifiers::NONE, key) {
                        self.move_selection(step);
                    }
                }
                if (results_focus || self.query.is_empty())
                    && take_key(ctx, Modifiers::NONE, Key::Home)
                {
                    self.move_selection(-(self.rows.len() as isize));
                }
                if (results_focus || self.query.is_empty())
                    && take_key(ctx, Modifiers::NONE, Key::End)
                {
                    self.move_selection(self.rows.len() as isize);
                }
                execute = take_key(ctx, Modifiers::NONE, Key::Enter)
                    || (results_focus && take_key(ctx, Modifiers::NONE, Key::Space));
            }
            if take_key(ctx, Modifiers::NONE, Key::Escape) {
                self.close();
            }
        }
        let screen = ctx.content_rect();
        let width = (screen.width() - 40.0 * scale).max(1.0).min(720.0 * scale);
        let row_height = foldex_ui::components::menu_row_height(t);
        let body_height = (self.rows.len().max(1) as f32 * row_height)
            .min((screen.height() - 175.0 * scale).clamp(row_height, 390.0 * scale));
        let target = if self.closing {
            0.0
        } else {
            m.popup_header + body_height + 29.0 * scale
        };
        let gains = MotionGains::new(ctx.input(|input| input.stable_dt), t.duration());
        let mut moving = foldex_ui::motion::popup_gains(ctx, t.duration()).step_pixels(
            &mut self.height,
            target,
            MotionProfile::Standard,
        );
        moving |= gains.step_normalized(
            &mut self.opacity,
            if self.closing { 0.0 } else { 1.0 },
            MotionProfile::Standard,
        );
        if self.finished() {
            return None;
        }
        self.motion.set_pixels_per_point(ctx.pixels_per_point());
        self.motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ctx, t.duration()));
        let top = (screen.top() + 32.0 + 47.0 * scale)
            .min(screen.bottom() - target.min(screen.height() - 30.0) - 15.0)
            .max(screen.top() + 15.0);
        let outer = Rect::from_min_size(
            pos2(screen.center().x - width / 2.0, top),
            vec2(width, self.height),
        );
        let frame = ctx.cumulative_frame_nr();
        let mut changed = false;
        let mut picked = None;
        foldex_ui::components::popup_surface(
            ctx,
            egui::Area::new(area_id)
                .order(egui::Order::Foreground)
                .fixed_pos(outer.min)
                .fade_in(false),
            !self.closing,
            |ui| {
                ui.set_min_size(outer.size());
                ui.set_max_size(outer.size());
                ui.set_clip_rect(outer.intersect(screen));
                ui.set_opacity(self.opacity);
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
                foldex_ui::components::popup_input_icon(ui, header, t, Icon::Search);
                let title_width = (self.title.chars().count().min(24) as f32 * t.font_size * 0.5
                    + 10.0 * scale)
                    .min(width * 0.25);
                ui.painter()
                    .with_clip_rect(Rect::from_min_size(
                        pos2(header.left() + 34.0 * scale, header.top()),
                        vec2(title_width, m.popup_header),
                    ))
                    .text(
                        pos2(header.left() + 34.0 * scale, header.center().y),
                        Align2::LEFT_CENTER,
                        &self.title,
                        t.font(),
                        t.dim(),
                    );
                let mut input = foldex_ui::components::popup_input_rect(header, 1);
                input.min.x += title_width;
                ui.scope_builder(UiBuilder::new().max_rect(input), |ui| {
                    let acquire_focus = self.focus && !self.closing;
                    if acquire_focus && !ui.memory(|memory| memory.has_focus(input_id)) {
                        ui.memory_mut(|memory| memory.request_focus(input_id));
                    }
                    let response = foldex_ui::components::text_input_at(
                        ui,
                        input,
                        input_id,
                        t,
                        &mut self.query,
                        &self.hint,
                        1024,
                    );
                    if acquire_focus {
                        self.focus = ui.is_sizing_pass() || !response.response.has_focus();
                    }
                    changed = response.response.changed();
                    result_input(ctx, input_id, list_id, !self.closing);
                });
                if changed {
                    self.refilter();
                }
                let close = foldex_ui::components::popup_action_rect(header, 0, 1);
                if control(
                    ui,
                    t,
                    close,
                    area_id.with("close"),
                    Icon::Close,
                    false,
                    gains,
                    &mut moving,
                )
                .on_hover_text("Close · Escape")
                .clicked()
                {
                    self.close();
                }
                ui.painter().line_segment(
                    [header.left_bottom(), header.right_bottom()],
                    Stroke::new(1.0, t.line()),
                );
                let body = layout.body;
                let body_height = body.height();
                result_surface(ui, body.intersect(outer), list_id, &self.title);
                ui.scope_builder(
                    UiBuilder::new()
                        .max_rect(body)
                        .accessibility_parent(list_id),
                    |ui| {
                        ui.set_clip_rect(body.intersect(outer));
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let mut scroll = foldex_ui::components::menu_scroll_area_for_content(
                            ui,
                            t,
                            area_id.with("results"),
                            body_height,
                            self.rows.len() as f32 * row_height,
                        );
                        if self.scroll_to && body_height >= row_height {
                            let index = self
                                .rows
                                .iter()
                                .position(|row| self.selected == Some(self.choices[row.index].id))
                                .unwrap_or(0);
                            scroll = scroll.vertical_scroll_offset(
                                (index as f32 * row_height - body_height / 2.0 + row_height / 2.0)
                                    .max(0.0),
                            );
                            self.scroll_to = false;
                        }
                        scroll.show_rows(ui, row_height, self.rows.len(), |ui, range| {
                            for index in range.take(MAX_RENDER_ROWS) {
                                let choice = &self.choices[self.rows[index].index];
                                let target = ui
                                    .allocate_exact_size(
                                        vec2(ui.available_width(), row_height),
                                        Sense::hover(),
                                    )
                                    .0;
                                let origin =
                                    pos2(target.left(), target.top() - index as f32 * row_height)
                                        .to_vec2();
                                let placement = self.motion.place_in_view(
                                    area_id.with(("row", choice.id)),
                                    Rect::from_min_size(
                                        pos2(0.0, index as f32 * row_height),
                                        target.size(),
                                    ),
                                    true,
                                    ui.clip_rect().translate(-origin),
                                );
                                let rect = placement.rect.translate(origin);
                                let response = ui.interact(
                                    rect,
                                    area_id.with(("choice", choice.id)),
                                    Sense::CLICK,
                                );
                                result_row(
                                    ui,
                                    &response,
                                    list_id,
                                    input_id,
                                    &choice.label,
                                    self.selected == Some(choice.id),
                                    true,
                                );
                                let key = TextKey {
                                    fonts: foldex_ui::fonts::revision(ctx),
                                    width: rect.width().to_bits(),
                                    font: t.font_size.to_bits(),
                                    ppp: ctx.pixels_per_point().to_bits(),
                                    foreground: t.fg(),
                                    muted: t.dim(),
                                    accent: t.accent(),
                                };
                                if self
                                    .cache
                                    .get(&choice.id)
                                    .is_none_or(|cached| cached.key != key)
                                {
                                    let label = bounded_prefix(&choice.label);
                                    let detail = bounded_prefix(&choice.detail);
                                    foldex_ui::fonts::request_text(ctx, label);
                                    foldex_ui::fonts::request_text(ctx, detail);
                                    let detail_width = if detail.is_empty() {
                                        0.0
                                    } else {
                                        (detail.chars().count().min(80) as f32
                                            * (t.font_size - 1.0)
                                            * 0.5)
                                            .clamp(
                                                40.0 * scale,
                                                (rect.width() * 0.52).max(40.0 * scale),
                                            )
                                    };
                                    self.cache.insert(
                                        choice.id,
                                        CachedRow {
                                            key,
                                            hover: 0.0,
                                            selected: 0.0,
                                            seen: frame,
                                            label: ui.painter().layout_job(highlight(
                                                label,
                                                &self
                                                    .matcher
                                                    .match_indices(label)
                                                    .unwrap_or_default(),
                                                t.font(),
                                                t.fg(),
                                                t.accent(),
                                                (rect.width() - 46.0 * scale - detail_width)
                                                    .max(1.0),
                                            )),
                                            detail: ui.painter().layout_job(highlight(
                                                detail,
                                                &self
                                                    .matcher
                                                    .match_indices(detail)
                                                    .unwrap_or_default(),
                                                FontId::proportional((t.font_size - 1.0).max(10.0)),
                                                t.dim(),
                                                t.accent(),
                                                detail_width.max(1.0),
                                            )),
                                        },
                                    );
                                }
                                let cached = self.cache.get_mut(&choice.id).unwrap();
                                cached.seen = frame;
                                moving |= gains.step_normalized(
                                    &mut cached.hover,
                                    if response.hovered() { 1.0 } else { 0.0 },
                                    MotionProfile::Standard,
                                );
                                moving |= gains.step_normalized(
                                    &mut cached.selected,
                                    if self.selected == Some(choice.id) {
                                        1.0
                                    } else {
                                        0.0
                                    },
                                    MotionProfile::Standard,
                                );
                                let mut painter =
                                    ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                                painter.multiply_opacity(placement.opacity);
                                painter.rect_filled(
                                    rect,
                                    t.radius,
                                    Theme::color(t.hover)
                                        .gamma_multiply(cached.hover * (1.0 - cached.selected)),
                                );
                                painter.rect_filled(
                                    rect,
                                    t.radius,
                                    Theme::color(t.selection).gamma_multiply(cached.selected),
                                );
                                foldex_ui::components::focus_ring(
                                    ui,
                                    rect,
                                    list_id.with(choice.id),
                                    t,
                                    self.selected == Some(choice.id)
                                        && ctx.memory(|memory| memory.has_focus(list_id)),
                                );
                                icon(
                                    &painter,
                                    pos2(rect.left() + 14.0 * scale, rect.center().y),
                                    choice.icon,
                                    if matches!(choice.icon, Icon::Folder) {
                                        Theme::color(t.folder)
                                    } else {
                                        t.dim()
                                    },
                                    m.icon,
                                );
                                painter.galley(
                                    pos2(
                                        rect.left() + 28.0 * scale,
                                        rect.center().y - cached.label.size().y / 2.0,
                                    ),
                                    Arc::clone(&cached.label),
                                    t.fg(),
                                );
                                painter.galley(
                                    pos2(
                                        rect.right() - 10.0 * scale - cached.detail.size().x,
                                        rect.center().y - cached.detail.size().y / 2.0,
                                    ),
                                    Arc::clone(&cached.detail),
                                    t.dim(),
                                );
                                if response.clicked() && !self.closing {
                                    picked = Some(choice.id);
                                }
                            }
                        });
                        if self.rows.is_empty() {
                            foldex_ui::components::message_at(
                                ui,
                                body,
                                Id::new("picker-empty"),
                                t,
                                "No matches",
                                t.dim(),
                            );
                        }
                    },
                );
                let footer = layout.footer;
                ui.painter().line_segment(
                    [footer.left_top(), footer.right_top()],
                    Stroke::new(1.0, t.line()),
                );
                foldex_ui::components::popup_hint(
                    ui.painter(),
                    footer,
                    t,
                    &self.footer,
                    "Enter  select     Esc  close",
                );
            },
        );
        self.cache.retain(|_, row| row.seen == frame);
        if execute && !self.closing {
            picked = self.selected;
        }
        if !self.closing
            && frame > self.opened_frame.unwrap_or_default()
            && ctx.input(|input| {
                input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|position| !outer.contains(position))
            })
        {
            self.close();
        }
        if picked.is_some() {
            self.close();
        }
        self.focus_scope
            .update(ctx, input_id, &members, !self.closing);
        if moving || self.motion.is_moving() || changed || (self.closing && !self.finished()) {
            ctx.request_repaint();
        }
        picked
    }
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
    fn keyboard_frame(
        ctx: &egui::Context,
        picker: &mut Picker,
        frame: u32,
        events: Vec<egui::Event>,
    ) -> Option<u64> {
        let mut picked = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1100.0, 700.0))),
                events,
                ..Default::default()
            },
            |ui| {
                picked = picker
                    .ui(
                        ui.ctx(),
                        &Theme {
                            reduced_motion: true,
                            ..Default::default()
                        },
                    )
                    .or(picked);
            },
        );
        output.textures_delta.clear();
        picked
    }
    fn keyboard_picker() -> Picker {
        Picker::new(
            "Keyboard",
            "Search",
            (0..24)
                .map(|id| Choice {
                    id,
                    label: format!("Item {id:02}"),
                    detail: String::new(),
                    icon: Icon::File,
                })
                .collect(),
        )
    }

    #[test]
    fn picker_tab_skips_virtual_rows_and_space_activates_only_results() {
        let ctx = egui::Context::default();
        let mut picker = keyboard_picker();
        keyboard_frame(&ctx, &mut picker, 0, vec![]);
        let area = Id::new(("choice-picker", "Keyboard"));
        keyboard_frame(&ctx, &mut picker, 1, vec![key(Key::Tab, Modifiers::NONE)]);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(area.with("close"))
        );
        keyboard_frame(&ctx, &mut picker, 2, vec![key(Key::Tab, Modifiers::NONE)]);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(area.with("results-focus"))
        );
        keyboard_frame(&ctx, &mut picker, 3, vec![key(Key::End, Modifiers::NONE)]);
        assert_eq!(picker.selected(), Some(23));
        keyboard_frame(
            &ctx,
            &mut picker,
            4,
            vec![key(Key::PageUp, Modifiers::NONE)],
        );
        let page =
            (390.0 / foldex_ui::components::menu_row_height(&Theme::default())).floor() as u64;
        let expected = 23u64.saturating_sub(page);
        assert_eq!(picker.selected(), Some(expected));
        assert_eq!(
            keyboard_frame(
                &ctx,
                &mut picker,
                5,
                vec![
                    key(Key::Space, Modifiers::NONE),
                    egui::Event::Text(" ".into())
                ]
            ),
            Some(expected)
        );
        assert!(!picker.is_open());
    }

    #[test]
    fn picker_typing_from_results_filters_and_modified_enter_is_preserved() {
        let ctx = egui::Context::default();
        let mut picker = keyboard_picker();
        keyboard_frame(&ctx, &mut picker, 0, vec![]);
        ctx.memory_mut(|memory| {
            memory.request_focus(Id::new(("choice-picker", "Keyboard")).with("results-focus"))
        });
        keyboard_frame(&ctx, &mut picker, 1, vec![egui::Event::Text("23".into())]);
        assert_eq!(picker.query(), "23");
        assert_eq!(picker.selected(), Some(23));
        assert_eq!(
            keyboard_frame(&ctx, &mut picker, 2, vec![key(Key::Enter, Modifiers::ALT)]),
            None
        );
        picker.set_query("missing");
        assert_eq!(
            keyboard_frame(&ctx, &mut picker, 3, vec![key(Key::Enter, Modifiers::NONE)]),
            None
        );
        assert!(picker.is_open());
    }

    #[test]
    fn duplicate_paths_return_the_chosen_opaque_identity() {
        let mut picker = Picker::new(
            "Tabs",
            "Search tabs…",
            vec![
                Choice {
                    id: 41,
                    label: "Documents".into(),
                    detail: "C:\\same".into(),
                    icon: Icon::Folder,
                },
                Choice {
                    id: 42,
                    label: "Documents".into(),
                    detail: "C:\\same".into(),
                    icon: Icon::Folder,
                },
            ],
        );
        assert_eq!(picker.len(), 2);
        picker.move_selection(1);
        assert_eq!(picker.selected(), Some(42));
        picker.set_query("docs");
        assert_eq!(picker.selected(), Some(42));
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut result = None;
        let mut input = egui::RawInput {
            time: Some(0.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
            ..Default::default()
        };
        input.events.push(egui::Event::Key {
            key: Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
        let mut output = ctx.run_ui(input, |ui| {
            if let Some(id) = picker.ui(ui.ctx(), &theme) {
                result = Some(id);
            }
        });
        output.textures_delta.clear();
        assert_eq!(result, Some(42));
        assert!(!picker.is_open());
    }

    #[test]
    fn rendering_keeps_only_visible_text_and_motion_for_large_choice_models() {
        let choices = (0..10_000)
            .map(|id| Choice {
                id,
                label: format!("Design{id:06}"),
                detail: format!("C:\\Generated\\{id:06}"),
                icon: Icon::Folder,
            })
            .collect();
        let mut picker = Picker::new("History", "Search history…", choices);
        let ctx = egui::Context::default();
        let theme = Theme::default();
        for frame in 0..120 {
            let input = egui::RawInput {
                time: Some(frame as f64 / 60.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                picker.ui(ui.ctx(), &theme);
            });
            output.textures_delta.clear();
            assert!(picker.cache.len() <= MAX_RENDER_ROWS);
            assert!(picker.motion.retained_items() <= MAX_RENDER_ROWS);
        }
        assert!(!picker.motion.is_moving());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new(("choice-picker", "History")).with("input"))
        );
        assert_eq!(picker.len(), 10_000);
        picker.set_query("qqqq");
        assert!(picker.is_empty());
        assert!(picker.cache.is_empty());
    }

    #[test]
    fn newly_opened_picker_accepts_same_frame_query_text() {
        let ctx = egui::Context::default();
        let mut picker = Picker::new("Tabs", "Search…", vec![]);
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(0.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1131.0, 611.0))),
                events: vec![egui::Event::Text("docs".into())],
                ..Default::default()
            },
            |ui| {
                picker.ui(ui.ctx(), &Theme::default());
            },
        );
        output.textures_delta.clear();
        assert_eq!(picker.query(), "docs");
    }
}
