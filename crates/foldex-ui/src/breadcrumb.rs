//! Bounded breadcrumb geometry, hit areas and retained text layouts.
use crate::{
    Icon, Theme, animated_state, chevron, components, fonts, icon,
    motion::{LayoutMotion, MotionProfile},
};
use egui::{Color32, Id, Rect, Ui, pos2, vec2};
use std::sync::Arc;

pub struct Item {
    pub id: Id,
    pub label: String,
}

#[derive(Default)]
pub struct Outcome {
    pub navigate: Option<(usize, bool)>,
    pub children: Option<(usize, Rect, Id)>,
    pub ancestors: Option<(usize, Rect, Id)>,
    pub context: Option<(usize, Rect, Id)>,
    pub address: bool,
    pub active_rect: Option<Rect>,
    pub current_rect: Option<Rect>,
    pub hidden_count: usize,
}

struct TextLayout {
    id: Id,
    full: Arc<egui::Galley>,
    clipped: Option<(u32, Arc<egui::Galley>)>,
    emphasis: f32,
    history: f32,
}

pub struct Bar {
    motion: LayoutMotion,
    text: Vec<TextLayout>,
    widths: Vec<f32>,
    style: (u32, u32, usize),
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            motion: LayoutMotion::new(64),
            text: Vec::new(),
            widths: Vec::new(),
            style: (0, 0, 0),
        }
    }
}

impl Bar {
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        id: Id,
        t: &Theme,
        items: &[Item],
        current: usize,
        active: Option<Id>,
        hint: impl Fn(usize) -> String,
    ) -> Outcome {
        let mut out = Outcome::default();
        if rect.width() <= 0.0 || items.is_empty() {
            return out;
        }
        let count = items.len().min(48);
        let items = &items[..count];
        let current = current.min(count - 1);
        let m = t.metrics();
        let style = (
            t.font_size.to_bits(),
            ui.ctx().pixels_per_point().to_bits(),
            fonts::revision(ui.ctx()),
        );
        let style_changed = self.style != style;
        self.style = style;
        self.text.truncate(count);
        for (index, item) in items.iter().enumerate() {
            // Navigation usually extends/truncates a common path. Retain its
            // layouts (and color transitions), including clipped variants.
            // A rename can change a label without changing its stable identity.
            if !style_changed
                && self.text.get(index).is_some_and(|layout| {
                    layout.id == item.id && layout.full.job.text == item.label
                })
            {
                continue;
            }
            fonts::request_text(ui.ctx(), &item.label);
            let layout = TextLayout {
                id: item.id,
                full: ui.painter().layout_no_wrap(
                    item.label.clone(),
                    t.font(),
                    Color32::PLACEHOLDER,
                ),
                clipped: None,
                emphasis: if index == current { 1.0 } else { 0.0 },
                history: if index > current { 1.0 } else { 0.0 },
            };
            if let Some(previous) = self.text.get_mut(index) {
                *previous = layout;
            } else {
                self.text.push(layout);
            }
        }
        let color_gains =
            crate::motion::MotionGains::new(ui.input(|input| input.stable_dt), t.duration());
        let mut color_moving = false;
        for (index, layout) in self.text.iter_mut().enumerate() {
            color_moving |= color_gains.step_normalized(
                &mut layout.emphasis,
                if index == current { 1.0 } else { 0.0 },
                MotionProfile::Standard,
            );
            color_moving |= color_gains.step_normalized(
                &mut layout.history,
                if index > current { 1.0 } else { 0.0 },
                MotionProfile::Standard,
            );
        }
        let separator = 12.0 * m.scale;
        // Fit measured labels, reserving readable space for the current folder.
        // Short root names cost only their natural width; depth alone is not a
        // collapse condition. The same calculation follows font and pane size.
        let minimum = 36.0 * m.scale;
        let min_width = |index: usize| {
            (self.text[index].full.size().x + 12.0 * m.scale).min(if index == current {
                80.0 * m.scale
            } else {
                minimum
            })
        };
        let total =
            (0..count).map(min_width).sum::<f32>() + separator * count.saturating_sub(1) as f32;
        let overflow_width = m.control + separator;
        let (mut start, mut end) = (0, count);
        if total > rect.width() {
            start = current;
            end = current + 1;
            let mut used = min_width(current)
                + if current + 1 < count { separator } else { 0.0 }
                + if current > 0 { overflow_width } else { 0.0 };
            // Keep the current folder and, where possible, its next visited
            // child. Older parents remain reachable through the overflow menu.
            if end < count {
                let cost = min_width(end) + if end + 1 < count { separator } else { 0.0 };
                if used + cost <= rect.width() {
                    used += cost;
                    end += 1;
                }
            }
            while start > 0 {
                let cost = min_width(start - 1) + separator
                    - if start == 1 { overflow_width } else { 0.0 };
                if used + cost > rect.width() {
                    break;
                }
                used += cost;
                start -= 1;
            }
            while end < count {
                let cost = min_width(end) + if end + 1 < count { separator } else { 0.0 };
                if used + cost > rect.width() {
                    break;
                }
                used += cost;
                end += 1;
            }
            // A tiny pane gets one complete menu button, rather than an icon
            // and a one-letter current folder competing for the same pixels.
            if rect.width()
                < (if current > 0 { overflow_width } else { 0.0 })
                    + min_width(current).min(40.0 * m.scale)
            {
                start = current + 1;
                end = start;
            }
        }
        let visible = end - start;
        let overflow = if visible == 0 {
            rect.width().min(overflow_width)
        } else if start > 0 {
            overflow_width.min(rect.width())
        } else {
            0.0
        };
        out.hidden_count = start;
        let arrows = visible.saturating_sub(1) + usize::from(visible > 0 && end < count);
        let budget = (rect.width() - overflow - separator * arrows as f32).max(1.0);
        self.widths.clear();
        self.widths.extend((start..end).map(min_width));
        let base: f32 = self.widths.iter().sum();
        if base > budget {
            for width in &mut self.widths {
                *width *= budget / base;
            }
        } else {
            let mut remaining = budget - base;
            for _ in 0..visible {
                let weight: f32 = (0..visible)
                    .filter(|row| {
                        self.widths[*row] + 0.01
                            < self.text[start + *row].full.size().x + 12.0 * m.scale
                    })
                    .map(|row| if start + row == current { 3.0 } else { 1.0 })
                    .sum();
                if remaining <= 0.01 || weight == 0.0 {
                    break;
                }
                let share = remaining / weight;
                let mut used = 0.0;
                for row in 0..visible {
                    let natural = self.text[start + row].full.size().x + 12.0 * m.scale;
                    let add = (share * if start + row == current { 3.0 } else { 1.0 })
                        .min((natural - self.widths[row]).max(0.0));
                    self.widths[row] += add;
                    used += add;
                }
                remaining -= used;
            }
        }
        self.motion
            .set_pixels_per_point(ui.ctx().pixels_per_point());
        self.motion
            .begin_frame_with_gains(crate::motion::layout_gains(ui.ctx(), t.duration()));
        let painter = ui.painter().with_clip_rect(rect);
        let origin = rect.min.to_vec2();
        let mut x = rect.left();
        let mut painted_right = x;
        if start > 0 {
            let key = id.with("ancestors");
            let target = Rect::from_min_size(pos2(x, rect.top()), vec2(overflow, rect.height()));
            let group = self
                .motion
                .place(key, target.translate(-origin), false)
                .rect
                .translate(origin)
                .intersect(rect);
            let selected = active == Some(key)
                || active.is_some_and(|id| items[..start].iter().any(|item| item.id == id));
            let has_arrow = group.width() > m.control;
            let label = Rect::from_min_max(
                group.min,
                pos2(
                    group.right() - if has_arrow { separator } else { 0.0 },
                    group.bottom(),
                ),
            );
            let hit = components::button_response(ui, label, key);
            hit.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    if visible == 0 {
                        "Choose a folder in this path"
                    } else {
                        "Hidden parent folders"
                    },
                )
            });
            let hover = animated_state(
                ui.ctx(),
                key.with("hover"),
                hit.hovered() || selected && !has_arrow,
                t,
                MotionProfile::Fast,
            );
            painter.rect_filled(group, 0, t.bg());
            painter.rect_filled(label, t.radius, Theme::color(t.hover).gamma_multiply(hover));
            let glyph_size = m.icon.min((group.width() - 8.0 * m.scale).max(0.0));
            let glyph_x = if visible == 0 {
                group.center().x
            } else {
                group.left() + m.control * 0.5
            };
            icon(
                &painter,
                pos2(glyph_x, group.center().y),
                Icon::Ellipsis,
                t.dim().lerp_to_gamma(t.fg(), hover),
                glyph_size,
            );
            if has_arrow {
                let arrow = Rect::from_min_max(pos2(label.right(), group.top()), group.max);
                let hit = components::button_response(ui, arrow, key.with("arrow"))
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                hit.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        ui.is_enabled(),
                        "Hidden parent folders",
                    )
                });
                let hover = animated_state(
                    ui.ctx(),
                    key.with("arrow-hover"),
                    hit.hovered() || selected,
                    t,
                    MotionProfile::Fast,
                );
                painter.rect_filled(
                    arrow,
                    t.radius.min(3),
                    Theme::color(t.hover).gamma_multiply(hover),
                );
                chevron(
                    &painter,
                    arrow.center(),
                    animated_state(
                        ui.ctx(),
                        key.with("open"),
                        selected,
                        t,
                        MotionProfile::Standard,
                    ),
                    t.dim().lerp_to_gamma(t.fg(), hover),
                    8.0 * m.scale,
                );
                components::focus_ring(ui, arrow, hit.id, t, hit.has_focus());
                if hit.clicked() {
                    hit.request_focus();
                    out.ancestors = Some((start, group, key));
                }
            }
            components::focus_ring(ui, label, key, t, hit.has_focus());
            if hit.clicked() {
                hit.request_focus();
                out.ancestors = Some((start, group, key));
            }
            if selected {
                out.active_rect = Some(group);
            }
            if visible == 0 {
                out.current_rect = Some(group);
            }
            x = target.right();
            painted_right = painted_right.max(group.right());
        }
        for row in 0..visible {
            let index = start + row;
            let item = &items[index];
            let has_children = index + 1 < count;
            let arrow = if has_children { separator } else { 0.0 };
            let target = Rect::from_min_size(
                pos2(x, rect.top()),
                vec2(self.widths[row] + arrow, rect.height()),
            );
            let group = self
                .motion
                .place(item.id, target.translate(-origin), false)
                .rect
                .translate(origin);
            let label = Rect::from_min_max(
                group.min,
                pos2((group.right() - arrow).max(group.left()), group.bottom()),
            );
            let arrow_rect = Rect::from_min_max(pos2(label.right(), group.top()), group.max);
            let selected = active == Some(item.id);
            // Moving labels must not show through a neighbouring breadcrumb.
            painter.rect_filled(group, 0, t.bg());
            let hit = components::button_response(ui, label.intersect(rect), item.id.with("label"))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let hover = animated_state(
                ui.ctx(),
                item.id.with("hover"),
                hit.hovered(),
                t,
                MotionProfile::Fast,
            );
            painter.rect_filled(label, t.radius, Theme::color(t.hover).gamma_multiply(hover));
            hit.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    index == current,
                    format!("Go to {}", item.label),
                )
            });
            let label_color = t
                .dim()
                .lerp_to_gamma(t.bg(), self.text[index].history * 0.3)
                .lerp_to_gamma(t.fg(), self.text[index].emphasis)
                .lerp_to_gamma(t.fg(), hover);
            let text_rect = label.shrink2(vec2(6.0 * m.scale, 0.0)).intersect(rect);
            if text_rect.width() > 0.0 {
                let layout = &mut self.text[index];
                let galley = if layout.full.size().x <= text_rect.width() {
                    layout.full.clone()
                } else {
                    let clipped_width = (text_rect.width() * ui.ctx().pixels_per_point()).floor()
                        / ui.ctx().pixels_per_point();
                    let width = clipped_width.to_bits();
                    if layout.clipped.as_ref().is_none_or(|(old, _)| *old != width) {
                        let mut job = egui::text::LayoutJob::simple(
                            item.label.clone(),
                            t.font(),
                            Color32::PLACEHOLDER,
                            clipped_width.max(0.0),
                        );
                        job.wrap.max_rows = 1;
                        job.wrap.break_anywhere = true;
                        layout.clipped = Some((width, painter.layout_job(job)));
                    }
                    layout.clipped.as_ref().unwrap().1.clone()
                };
                painter
                    .with_clip_rect(text_rect.intersect(rect))
                    .galley_with_override_text_color(
                        pos2(
                            text_rect.left(),
                            text_rect.center().y - galley.size().y * 0.5,
                        ),
                        galley,
                        label_color,
                    );
            }
            components::focus_ring(ui, label.intersect(rect), hit.id, t, hit.has_focus());
            if hit.clicked() || hit.middle_clicked() {
                out.navigate = Some((index, hit.middle_clicked()));
            }
            if hit.secondary_clicked() {
                hit.request_focus();
                out.context = Some((index, group, item.id));
            }
            if !components::tooltips_blocked(ui.ctx()) {
                hit.on_hover_ui(|ui| {
                    ui.label(hint(index));
                });
            }
            if has_children && arrow_rect.width() > 0.0 {
                let hit = components::button_response(
                    ui,
                    arrow_rect.intersect(rect),
                    item.id.with("children"),
                )
                .on_hover_cursor(egui::CursorIcon::PointingHand);
                hit.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        ui.is_enabled(),
                        format!("Contents of {}", item.label),
                    )
                });
                let hover = animated_state(
                    ui.ctx(),
                    item.id.with("children-hover"),
                    hit.hovered() || selected,
                    t,
                    MotionProfile::Fast,
                );
                painter.rect_filled(
                    arrow_rect,
                    t.radius.min(3),
                    Theme::color(t.hover).gamma_multiply(hover),
                );
                let open = animated_state(
                    ui.ctx(),
                    item.id.with("open"),
                    selected,
                    t,
                    MotionProfile::Standard,
                );
                chevron(
                    &painter,
                    arrow_rect.center(),
                    open,
                    t.dim().lerp_to_gamma(t.fg(), hover),
                    8.0 * m.scale,
                );
                components::focus_ring(ui, arrow_rect.intersect(rect), hit.id, t, hit.has_focus());
                if hit.clicked() {
                    hit.request_focus();
                    out.children = Some((index, group, item.id));
                }
                if hit.secondary_clicked() {
                    hit.request_focus();
                    out.context = Some((index, group, item.id));
                }
            }
            if selected {
                out.active_rect = Some(group.intersect(rect));
            }
            if index == current {
                out.current_rect = Some(group.intersect(rect));
            }
            painted_right = painted_right.max(group.right());
            x = target.right();
        }
        let blank = x.max(painted_right).clamp(rect.left(), rect.right());
        if blank < rect.right() {
            let empty = Rect::from_min_max(pos2(blank, rect.top()), rect.max);
            let hit = components::button_response(ui, empty, id.with("address"))
                .on_hover_cursor(egui::CursorIcon::Text);
            hit.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    ui.is_enabled(),
                    "Edit this path",
                )
            });
            let hover = animated_state(
                ui.ctx(),
                hit.id.with("hover"),
                hit.hovered(),
                t,
                MotionProfile::Fast,
            );
            painter.rect_filled(
                empty,
                t.radius,
                Theme::color(t.hover).gamma_multiply(hover * 0.35),
            );
            components::focus_ring(ui, empty, hit.id, t, hit.has_focus());
            out.address = hit.clicked();
            if !components::tooltips_blocked(ui.ctx()) {
                hit.on_hover_text("Edit this path · Ctrl+L");
            }
        }
        if self.motion.is_moving() || color_moving {
            ui.ctx().request_repaint();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_events(key: egui::Key, modifiers: egui::Modifiers) -> Vec<egui::Event> {
        [true, false]
            .into_iter()
            .map(|pressed| egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            })
            .collect()
    }

    fn path_items() -> [Item; 2] {
        [
            Item {
                id: Id::new("keyboard-parent"),
                label: "Projects".into(),
            },
            Item {
                id: Id::new("keyboard-child"),
                label: "Foldex".into(),
            },
        ]
    }

    fn frame(
        ctx: &egui::Context,
        bar: &mut Bar,
        items: &[Item],
        rect: Rect,
        number: u64,
        active: Option<Id>,
        events: Vec<egui::Event>,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(900.0, 500.0))),
                time: Some(number as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                outcome = bar.ui(
                    ui,
                    rect,
                    Id::new("breadcrumb"),
                    &Theme {
                        reduced_motion: true,
                        ..Default::default()
                    },
                    items,
                    items.len() - 1,
                    active,
                    |_| String::new(),
                );
            },
        )
        .drop_without_applying_deltas();
        outcome
    }

    fn hover_value(ctx: &egui::Context, id: Id) -> f32 {
        let key = Id::new(("foldex-control-motion", ctx.viewport_id()));
        ctx.data(|data| data.get_temp::<crate::ControlStates>(key).unwrap().items[&id].0)
    }

    #[test]
    fn navigation_keeps_common_ancestor_layouts_after_egui_cache_expiry() {
        let ctx = egui::Context::default();
        let mut bar = Bar::default();
        let mut items = Vec::from(path_items());
        let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(600.0, 28.0));
        frame(&ctx, &mut bar, &items, rect, 0, None, vec![]);
        let retained = bar.text[0].full.clone();
        for _ in 0..3 {
            ctx.run_ui(Default::default(), |_| {})
                .drop_without_applying_deltas();
        }
        items.push(Item {
            id: Id::new("new-child"),
            label: "New folder".into(),
        });
        frame(&ctx, &mut bar, &items, rect, 4, None, vec![]);
        assert!(Arc::ptr_eq(&retained, &bar.text[0].full));
        assert_eq!(bar.text.len(), 3);
        items.pop();
        frame(&ctx, &mut bar, &items, rect, 5, None, vec![]);
        assert!(Arc::ptr_eq(&retained, &bar.text[0].full));
        assert_eq!(bar.text.len(), 2);
    }

    #[test]
    fn stable_breadcrumb_identity_still_refreshes_a_renamed_label() {
        let ctx = egui::Context::default();
        let mut bar = Bar::default();
        let mut items = path_items();
        let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(600.0, 28.0));
        frame(&ctx, &mut bar, &items, rect, 0, None, vec![]);
        items[0].label = "Renamed parent".into();
        frame(&ctx, &mut bar, &items, rect, 1, None, vec![]);
        assert_eq!(bar.text[0].full.job.text, items[0].label);
        assert_eq!(bar.text[0].id, items[0].id);
    }

    #[test]
    fn separator_hover_and_click_are_independent_and_keep_the_parent_anchor() {
        let ctx = egui::Context::default();
        let mut bar = Bar::default();
        let items = [
            Item {
                id: Id::new("parent"),
                label: "Projects".into(),
            },
            Item {
                id: Id::new("child"),
                label: "Foldex".into(),
            },
        ];
        let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(600.0, 28.0));
        frame(&ctx, &mut bar, &items, rect, 0, None, vec![]);
        let label = ctx.read_response(items[0].id.with("label")).unwrap().rect;
        let arrow = ctx
            .read_response(items[0].id.with("children"))
            .unwrap()
            .rect;
        assert!(label.right() <= arrow.left());
        frame(
            &ctx,
            &mut bar,
            &items,
            rect,
            1,
            None,
            vec![egui::Event::PointerMoved(arrow.center())],
        );
        assert_eq!(hover_value(&ctx, items[0].id.with("hover")), 0.0);
        assert_eq!(hover_value(&ctx, items[0].id.with("children-hover")), 1.0);
        frame(
            &ctx,
            &mut bar,
            &items,
            rect,
            2,
            None,
            vec![egui::Event::PointerMoved(label.center())],
        );
        assert_eq!(hover_value(&ctx, items[0].id.with("hover")), 1.0);
        assert_eq!(hover_value(&ctx, items[0].id.with("children-hover")), 0.0);
        frame(
            &ctx,
            &mut bar,
            &items,
            rect,
            3,
            None,
            vec![
                egui::Event::PointerMoved(arrow.center()),
                egui::Event::PointerButton {
                    pos: arrow.center(),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let outcome = frame(
            &ctx,
            &mut bar,
            &items,
            rect,
            4,
            None,
            vec![egui::Event::PointerButton {
                pos: arrow.center(),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(outcome.navigate.is_none());
        assert_eq!(
            outcome.children,
            Some((0, Rect::from_min_max(label.min, arrow.max), items[0].id))
        );
        let outcome = frame(
            &ctx,
            &mut bar,
            &items,
            rect,
            5,
            Some(items[0].id),
            vec![egui::Event::PointerMoved(pos2(800.0, 400.0))],
        );
        assert_eq!(hover_value(&ctx, items[0].id.with("hover")), 0.0);
        assert_eq!(hover_value(&ctx, items[0].id.with("children-hover")), 1.0);
        assert_eq!(
            outcome.active_rect,
            Some(Rect::from_min_max(label.min, arrow.max))
        );
    }

    #[test]
    fn collapsed_ancestor_separator_has_its_own_hover_surface() {
        let ctx = egui::Context::default();
        let mut bar = Bar::default();
        let items: Vec<_> = (0..8)
            .map(|index| Item {
                id: Id::new(index),
                label: format!("Long parent folder {index}"),
            })
            .collect();
        let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(160.0, 28.0));
        frame(&ctx, &mut bar, &items, rect, 0, None, vec![]);
        let key = Id::new("breadcrumb").with("ancestors");
        let arrow = ctx.read_response(key.with("arrow")).unwrap().rect;
        frame(
            &ctx,
            &mut bar,
            &items,
            rect,
            1,
            None,
            vec![egui::Event::PointerMoved(arrow.center())],
        );
        assert_eq!(hover_value(&ctx, key.with("hover")), 0.0);
        assert_eq!(hover_value(&ctx, key.with("arrow-hover")), 1.0);
        assert!(!ctx.read_response(key).unwrap().hovered());
    }

    #[test]
    fn chevron_enter_and_space_open_once_and_consume_the_invocation_key() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let ctx = egui::Context::default();
            let mut bar = Bar::default();
            let items = path_items();
            let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(600.0, 28.0));
            frame(&ctx, &mut bar, &items, rect, 0, None, vec![]);
            let focus = items[0].id.with("children");
            let label = ctx.read_response(items[0].id.with("label")).unwrap().rect;
            let arrow = ctx.read_response(focus).unwrap().rect;
            ctx.memory_mut(|memory| memory.request_focus(focus));
            let mut events = key_events(key, egui::Modifiers::NONE);
            if key == egui::Key::Space {
                events.insert(1, egui::Event::Text(" ".into()));
            }
            let outcome = frame(&ctx, &mut bar, &items, rect, 1, None, events);
            assert_eq!(
                outcome.children,
                Some((0, Rect::from_min_max(label.min, arrow.max), items[0].id)),
            );
            assert!(outcome.navigate.is_none());
            assert!(outcome.ancestors.is_none());
            assert!(!outcome.address);
            assert!(
                !ctx.input(|input| input.key_pressed(key)),
                "{key:?} must be consumed before the parent opens its submenu"
            );
            assert!(!ctx.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Text(text) if text == " "))
            }));
            let idle = frame(&ctx, &mut bar, &items, rect, 2, Some(items[0].id), vec![]);
            assert!(
                idle.children.is_none(),
                "the idle frame must not reopen the submenu"
            );
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(focus));
        }
    }

    #[test]
    fn modified_enter_does_not_activate_a_chevron_or_consume_its_shortcut() {
        for modifiers in [
            egui::Modifiers::CTRL,
            egui::Modifiers::ALT,
            egui::Modifiers::SHIFT,
        ] {
            let ctx = egui::Context::default();
            let mut bar = Bar::default();
            let items = path_items();
            let rect = Rect::from_min_size(pos2(20.0, 40.0), vec2(600.0, 28.0));
            frame(&ctx, &mut bar, &items, rect, 0, None, vec![]);
            let focus = items[0].id.with("children");
            ctx.memory_mut(|memory| memory.request_focus(focus));
            let outcome = frame(
                &ctx,
                &mut bar,
                &items,
                rect,
                1,
                None,
                key_events(egui::Key::Enter, modifiers),
            );
            assert!(outcome.children.is_none());
            assert!(outcome.navigate.is_none());
            assert!(outcome.ancestors.is_none());
            assert!(!outcome.address);
            assert!(
                ctx.input(|input| input.events.iter().any(|event| {
                    matches!(event, egui::Event::Key {
                    key: egui::Key::Enter, pressed: true, modifiers: actual, ..
                } if *actual == modifiers)
                })),
                "the modified Enter event must remain available to its shortcut owner"
            );
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(focus));
        }
    }
}
