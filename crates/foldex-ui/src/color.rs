//! Bounded native color editing, using the same fields, focus and dialog motion
//! as the rest of the interface. Gradients use a handful of GPU quads.
use crate::{Theme, components, motion::MotionProfile};
use egui::{Color32, Id, Key, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

pub fn color_button(
    ui: &mut Ui,
    rect: Rect,
    id: Id,
    t: &Theme,
    value: [u8; 3],
    open: bool,
) -> egui::Response {
    let m = t.metrics();
    let response = components::button_response(ui, rect, id);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::ColorButton,
            ui.is_enabled(),
            format!("Color {}", hex(value)),
        )
    });
    let hover = crate::animated_state(
        ui.ctx(),
        id.with("hover"),
        response.hovered() || open,
        t,
        MotionProfile::Fast,
    );
    ui.painter().rect_filled(
        rect,
        t.radius,
        Theme::color(t.chrome).lerp_to_gamma(Theme::color(t.hover), hover),
    );
    ui.painter().rect_stroke(
        rect,
        t.radius,
        Stroke::new(1.0, t.line().lerp_to_gamma(t.accent(), hover)),
        StrokeKind::Inside,
    );
    let swatch = Rect::from_center_size(
        pos2(rect.left() + m.inset + m.icon * 0.5, rect.center().y),
        vec2(m.icon, m.icon),
    );
    ui.painter()
        .rect_filled(swatch, t.radius.min(3), Theme::color(value));
    ui.painter().rect_stroke(
        swatch,
        t.radius.min(3),
        Stroke::new(1.0, t.fg().gamma_multiply(0.25)),
        StrokeKind::Inside,
    );
    let text = Rect::from_min_max(
        pos2(swatch.right() + m.gap, rect.top()),
        pos2(
            (rect.right() - m.inset).max(swatch.right() + m.gap),
            rect.bottom(),
        ),
    );
    components::elided_label(ui.painter(), text, t, &hex(value), t.fg(), false);
    components::focus_ring(ui, rect, id, t, response.has_focus());
    if response.clicked() {
        response.request_focus();
    }
    if !open && !response.clicked() {
        response.on_hover_text("Edit color")
    } else {
        response
    }
}

#[derive(Default)]
pub struct ColorPicker {
    open: bool,
    hsv: egui::ecolor::Hsva,
    input: String,
    focus: bool,
    invalid: bool,
}
impl ColorPicker {
    pub fn is_open(&self) -> bool {
        self.open
    }
    pub fn open(&mut self, value: [u8; 3]) {
        self.open = true;
        self.hsv = from_srgb(value);
        self.input = hex(value);
        self.focus = true;
        self.invalid = false;
    }
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        id: Id,
        t: &Theme,
        title: &str,
        value: &mut [u8; 3],
    ) -> bool {
        if !self.open {
            return false;
        }
        let mut open = true;
        let mut changed = false;
        components::dialog(
            ctx,
            id,
            t,
            title,
            336.0 * t.metrics().scale,
            &mut open,
            |ui| {
                let m = t.metrics();
                let field_id = id.with("hex");
                let (row, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), m.control), Sense::hover());
                let label = Rect::from_min_size(row.min, vec2(36.0 * m.scale, row.height()));
                components::elided_label(ui.painter(), label, t, "Hex", t.fg(), false);
                let field = Rect::from_min_max(pos2(label.right() + m.gap, row.top()), row.max);
                if self.focus && components::can_focus(ui) {
                    ui.memory_mut(|memory| memory.request_focus(field_id));
                    let mut state = egui::text_edit::TextEditState::load(ui.ctx(), field_id)
                        .unwrap_or_default();
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(self.input.chars().count()),
                        )));
                    state.store(ui.ctx(), field_id);
                }
                let edit =
                    components::input_box(ui, field, field_id, t, &mut self.input, "#RRGGBB", 7)
                        .response;
                components::label_control(&edit, "Hex color");
                if self.focus && components::can_focus(ui) {
                    self.focus = ui.is_sizing_pass() || !edit.has_focus();
                }
                let hex_changed = edit.changed();
                if hex_changed {
                    self.invalid = false;
                    if let Some(rgb) = parse_hex(&self.input) {
                        self.set_rgb(rgb);
                        *value = rgb;
                        changed = true;
                    }
                }
                if (edit.has_focus() || edit.lost_focus())
                    && components::consume_key(ui.ctx(), egui::Modifiers::NONE, Key::Enter)
                {
                    if let Some(rgb) = parse_hex(&self.input) {
                        self.set_rgb(rgb);
                        *value = rgb;
                        changed = true;
                        ui.close();
                    } else {
                        self.invalid = true;
                        edit.request_focus();
                    }
                }
                if self.invalid {
                    ui.label(
                        egui::RichText::new("Use six hex digits, such as #17A1CC")
                            .color(Theme::color(t.danger)),
                    );
                }

                let height = (ctx.content_rect().height() * 0.30).clamp(80.0, 200.0 * m.scale);
                let (plane, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
                let knob = (5.0 * m.scale).clamp(5.0, 8.0);
                let gradient = plane.shrink(knob + 2.0 * m.scale);
                let previous_sv = (self.hsv.s, self.hsv.v);
                let mut hit = ui
                    .interact(plane, id.with("saturation-value"), Sense::drag())
                    .on_hover_cursor(egui::CursorIcon::Crosshair);
                if let Some(point) = components::drag_position(ui, plane, &hit) {
                    hit.request_focus();
                    self.hsv.s =
                        ((point.x - gradient.left()) / gradient.width().max(1.0)).clamp(0.0, 1.0);
                    self.hsv.v = (1.0 - (point.y - gradient.top()) / gradient.height().max(1.0))
                        .clamp(0.0, 1.0);
                    changed = true;
                }
                if hit.has_focus() {
                    lock_directions(ui, hit.id);
                    components::consume_key(ui.ctx(), egui::Modifiers::NONE, Key::Enter);
                    components::consume_key(ui.ctx(), egui::Modifiers::NONE, Key::Space);
                    for (key, ds, dv) in [
                        (Key::ArrowLeft, -1.0, 0.0),
                        (Key::ArrowRight, 1.0, 0.0),
                        (Key::ArrowUp, 0.0, 1.0),
                        (Key::ArrowDown, 0.0, -1.0),
                    ] {
                        // Read the modifier from the key event: a quick chord can
                        // release Shift before this frame is rendered.
                        let step = {
                            if components::consume_key(ui.ctx(), egui::Modifiers::SHIFT, key) {
                                0.1
                            } else if components::consume_key(ui.ctx(), egui::Modifiers::NONE, key)
                            {
                                0.01
                            } else {
                                0.0
                            }
                        };
                        if step > 0.0 {
                            self.hsv.s = (self.hsv.s + ds * step).clamp(0.0, 1.0);
                            self.hsv.v = (self.hsv.v + dv * step).clamp(0.0, 1.0);
                            changed = true;
                        }
                    }
                }
                if previous_sv != (self.hsv.s, self.hsv.v) {
                    hit.mark_changed();
                }
                hit.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Other,
                        ui.is_enabled(),
                        format!(
                            "Saturation {:.0}%, brightness {:.0}%",
                            self.hsv.s * 100.0,
                            self.hsv.v * 100.0
                        ),
                    )
                });
                let hue = to_srgb(egui::ecolor::Hsva {
                    h: self.hsv.h,
                    s: 1.0,
                    v: 1.0,
                    a: 1.0,
                });
                quad(
                    ui.painter(),
                    gradient,
                    [
                        Color32::WHITE,
                        Theme::color(hue),
                        Theme::color(hue),
                        Color32::WHITE,
                    ],
                );
                quad(
                    ui.painter(),
                    gradient,
                    [
                        Color32::TRANSPARENT,
                        Color32::TRANSPARENT,
                        Color32::BLACK,
                        Color32::BLACK,
                    ],
                );
                ui.painter().rect_stroke(
                    gradient,
                    0,
                    Stroke::new(1.0, t.line()),
                    StrokeKind::Inside,
                );
                let point = pos2(
                    gradient.left() + self.hsv.s * gradient.width(),
                    gradient.bottom() - self.hsv.v * gradient.height(),
                );
                ui.painter()
                    .circle_filled(point, knob, Theme::color(to_srgb(self.hsv)));
                ui.painter().circle_stroke(
                    point,
                    knob,
                    Stroke::new(2.5, Color32::BLACK.gamma_multiply(0.7)),
                );
                ui.painter()
                    .circle_stroke(point, knob - 1.0, Stroke::new(1.0, Color32::WHITE));
                components::focus_ring(ui, plane, id.with("saturation-value"), t, hit.has_focus());

                let (hue_row, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), m.control), Sense::hover());
                let rail = hue_row.shrink2(vec2(m.inset, 7.0 * m.scale));
                let previous_hue = self.hsv.h;
                let mut hit = ui
                    .interact(hue_row, id.with("hue"), Sense::drag())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if let Some(point) = components::drag_position(ui, hue_row, &hit) {
                    hit.request_focus();
                    self.hsv.h = ((point.x - rail.left()) / rail.width().max(1.0)).clamp(0.0, 1.0);
                    changed = true;
                }
                if hit.has_focus() {
                    lock_directions(ui, hit.id);
                    components::consume_key(ui.ctx(), egui::Modifiers::NONE, Key::Enter);
                    components::consume_key(ui.ctx(), egui::Modifiers::NONE, Key::Space);
                    for (key, sign) in [
                        (Key::ArrowLeft, -1.0),
                        (Key::ArrowRight, 1.0),
                        (Key::ArrowDown, -1.0),
                        (Key::ArrowUp, 1.0),
                    ] {
                        let step = if components::consume_key(ui.ctx(), egui::Modifiers::SHIFT, key)
                        {
                            0.1
                        } else if components::consume_key(ui.ctx(), egui::Modifiers::NONE, key) {
                            0.01
                        } else {
                            0.0
                        };
                        if step > 0.0 {
                            self.hsv.h = (self.hsv.h + sign * step).clamp(0.0, 1.0);
                            changed = true;
                        }
                    }
                    for (key, endpoint) in [(Key::Home, 0.0), (Key::End, 1.0)] {
                        if components::consume_key(ui.ctx(), egui::Modifiers::NONE, key) {
                            changed |= self.hsv.h != endpoint;
                            self.hsv.h = endpoint;
                        }
                    }
                    for (key, step) in [(Key::PageDown, -0.1), (Key::PageUp, 0.1)] {
                        if components::consume_key(ui.ctx(), egui::Modifiers::NONE, key) {
                            self.hsv.h = (self.hsv.h + step).clamp(0.0, 1.0);
                            changed = true;
                        }
                    }
                }
                if ui.is_enabled() {
                    let next = ui.input(|input| {
                        use egui::accesskit::{Action, ActionData};
                        let delta = input.num_accesskit_action_requests(hit.id, Action::Increment)
                            as f32
                            - input.num_accesskit_action_requests(hit.id, Action::Decrement) as f32;
                        let mut next = (self.hsv.h + delta * 0.01).clamp(0.0, 1.0);
                        for request in input.accesskit_action_requests(hit.id, Action::SetValue) {
                            if let Some(ActionData::NumericValue(value)) = request.data {
                                next = (value as f32 / 360.0).clamp(0.0, 1.0);
                            }
                        }
                        next
                    });
                    self.hsv.h = next;
                }
                if previous_hue != self.hsv.h {
                    changed = true;
                    hit.mark_changed();
                }
                hit.widget_info(|| {
                    egui::WidgetInfo::slider(ui.is_enabled(), self.hsv.h as f64 * 360.0, "Hue")
                });
                ui.ctx().accesskit_node_builder(hit.id, |node| {
                    node.set_min_numeric_value(0.0);
                    node.set_max_numeric_value(360.0);
                    node.set_numeric_value_step(3.6);
                    node.add_action(egui::accesskit::Action::SetValue);
                    node.add_action(egui::accesskit::Action::Increment);
                    node.add_action(egui::accesskit::Action::Decrement);
                });
                ui.painter().add(egui::Shape::mesh(hue_mesh(rail)));
                ui.painter()
                    .rect_stroke(rail, 0, Stroke::new(1.0, t.line()), StrokeKind::Inside);
                let marker = Rect::from_center_size(
                    pos2(rail.left() + self.hsv.h * rail.width(), hue_row.center().y),
                    vec2(5.0 * m.scale, hue_row.height() - 6.0 * m.scale),
                );
                ui.painter().rect_stroke(
                    marker,
                    2,
                    Stroke::new(3.0, Color32::BLACK.gamma_multiply(0.7)),
                    StrokeKind::Inside,
                );
                ui.painter().rect_stroke(
                    marker.shrink(1.0),
                    2,
                    Stroke::new(1.0, Color32::WHITE),
                    StrokeKind::Inside,
                );
                components::focus_ring(ui, hue_row, id.with("hue"), t, hit.has_focus());

                let mut rgb = to_srgb(self.hsv);
                let (rgb_row, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), m.control), Sense::hover());
                let group_width = (rgb_row.width() - 2.0 * m.gap) / 3.0;
                let label_width = ["R", "G", "B"]
                    .into_iter()
                    .map(|label| crate::fonts::no_wrap(ui.ctx(), label, t.font()).size().x)
                    .fold(0.0_f32, f32::max);
                for (index, label) in ["R", "G", "B"].into_iter().enumerate() {
                    let rect = Rect::from_min_size(
                        rgb_row.min + vec2(index as f32 * (group_width + m.gap), 0.0),
                        vec2(group_width, m.control),
                    );
                    let mut group = ui.new_child(
                        UiBuilder::new()
                            .id_salt(id.with(("rgb", index)))
                            .max_rect(rect)
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    group.spacing_mut().item_spacing.x = m.gap;
                    let name = group.label(label);
                    group.add_space((label_width - name.rect.width()).max(0.0));
                    let width = (rect.width() - label_width - m.gap).max(1.0);
                    let response = group
                        .add_sized(
                            [width, m.control],
                            egui::DragValue::new(&mut rgb[index])
                                .range(0..=255)
                                .speed(1.0),
                        )
                        .labelled_by(name.id);
                    components::label_control(&response, ["Red", "Green", "Blue"][index]);
                    components::focus_ring(
                        &group,
                        response.rect,
                        response.id,
                        t,
                        response.has_focus(),
                    );
                    if response.changed() {
                        self.set_rgb(rgb);
                        changed = true;
                    }
                }
                if changed {
                    *value = to_srgb(self.hsv);
                    if !hex_changed {
                        self.input = hex(*value);
                    }
                    self.invalid = false;
                }
            },
        );
        self.open = open;
        changed
    }
    fn set_rgb(&mut self, value: [u8; 3]) {
        let mut next = from_srgb(value);
        if next.s < 0.0001 {
            next.h = self.hsv.h;
        }
        self.hsv = next;
    }
}

fn hex(value: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", value[0], value[1], value[2])
}
fn parse_hex(input: &str) -> Option<[u8; 3]> {
    let input = input.trim().strip_prefix('#').unwrap_or(input.trim());
    if input.len() != 6 || !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&input[0..2], 16).ok()?,
        u8::from_str_radix(&input[2..4], 16).ok()?,
        u8::from_str_radix(&input[4..6], 16).ok()?,
    ])
}
// Work in byte-space HSV so the two cheap gradients match the editable sRGB value.
fn from_srgb(value: [u8; 3]) -> egui::ecolor::Hsva {
    egui::ecolor::Hsva::from_rgb(value.map(|byte| byte as f32 / 255.0))
}
fn to_srgb(value: egui::ecolor::Hsva) -> [u8; 3] {
    value
        .to_rgb()
        .map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
}
fn lock_directions(ui: &Ui, id: Id) {
    ui.memory_mut(|memory| {
        // A newly focused widget cannot install egui's filter until the next
        // pass. Do not let its first arrow also schedule spatial focus motion.
        memory.move_focus(egui::FocusDirection::None);
        memory.set_focus_lock_filter(
            id,
            egui::EventFilter {
                tab: false,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: false,
            },
        )
    });
}
fn hue_mesh(rect: Rect) -> egui::Mesh {
    let colors = [
        Color32::RED,
        Color32::YELLOW,
        Color32::GREEN,
        Color32::from_rgb(0, 255, 255),
        Color32::BLUE,
        Color32::from_rgb(255, 0, 255),
        Color32::RED,
    ];
    let mut mesh = egui::Mesh::default();
    mesh.reserve_vertices(14);
    mesh.reserve_triangles(12);
    for (index, color) in colors.into_iter().enumerate() {
        let x = rect.left() + rect.width() * index as f32 / 6.0;
        mesh.colored_vertex(pos2(x, rect.top()), color);
        mesh.colored_vertex(pos2(x, rect.bottom()), color);
        if index > 0 {
            let left = (index as u32 - 1) * 2;
            // Same diagonal, colors and interpolation as the six original
            // quads, sharing the adjacent endpoint vertices in one submission.
            mesh.add_triangle(left, left + 2, left + 3);
            mesh.add_triangle(left, left + 3, left + 1);
        }
    }
    mesh
}

fn quad(painter: &egui::Painter, rect: Rect, colors: [Color32; 4]) {
    let mut mesh = egui::Mesh::default();
    for (pos, color) in [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
    .into_iter()
    .zip(colors)
    {
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv: egui::epaint::WHITE_UV,
            color,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    use egui::Modifiers;

    #[test]
    fn hue_mesh_keeps_every_original_segment_triangle_and_endpoint_color() {
        let rail = Rect::from_min_size(pos2(20.5, 40.25), vec2(257.75, 14.0));
        let mesh = hue_mesh(rail);
        assert_eq!(mesh.vertices.len(), 14);
        assert_eq!(mesh.indices.len(), 36);
        let colors = [
            Color32::RED,
            Color32::YELLOW,
            Color32::GREEN,
            Color32::from_rgb(0, 255, 255),
            Color32::BLUE,
            Color32::from_rgb(255, 0, 255),
            Color32::RED,
        ];
        for segment in 0..6 {
            let left = rail.left() + rail.width() * segment as f32 / 6.0;
            let right = rail.left() + rail.width() * (segment + 1) as f32 / 6.0;
            let expected = [
                (pos2(left, rail.top()), colors[segment]),
                (pos2(right, rail.top()), colors[segment + 1]),
                (pos2(right, rail.bottom()), colors[segment + 1]),
                (pos2(left, rail.top()), colors[segment]),
                (pos2(right, rail.bottom()), colors[segment + 1]),
                (pos2(left, rail.bottom()), colors[segment]),
            ];
            for (index, (pos, color)) in mesh.indices[segment * 6..segment * 6 + 6]
                .iter()
                .zip(expected)
            {
                let vertex = mesh.vertices[*index as usize];
                assert_eq!(vertex.pos, pos);
                assert_eq!(vertex.color, color);
                assert_eq!(vertex.uv, egui::epaint::WHITE_UV);
            }
        }
    }

    fn id() -> Id {
        Id::new("color-keyboard-test")
    }
    fn origin() -> Id {
        Id::new("color-keyboard-origin")
    }

    fn keys(key: Key, modifiers: Modifiers) -> Vec<egui::Event> {
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

    fn frame(
        ctx: &egui::Context,
        number: u32,
        picker: &mut ColorPicker,
        value: &mut [u8; 3],
        events: Vec<egui::Event>,
    ) -> bool {
        let t = Theme {
            reduced_motion: true,
            ..Theme::default()
        };
        let mut background_clicked = false;
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 480.0))),
                time: Some(number as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                let rect = Rect::from_min_size(pos2(20.0, 20.0), vec2(100.0, 28.0));
                components::register_focus_target(ui, rect, origin());
                picker.ui(ui.ctx(), id(), &t, "Color", value);
                background_clicked =
                    components::text_button_at(ui, rect, origin(), &t, "Color", false).clicked();
            },
        )
        .drop_without_applying_deltas();
        background_clicked
    }

    fn opened(ctx: &egui::Context) -> (ColorPicker, [u8; 3]) {
        let mut picker = ColorPicker::default();
        let mut value = [255, 0, 0];
        frame(ctx, 0, &mut picker, &mut value, vec![]);
        ctx.memory_mut(|memory| memory.request_focus(origin()));
        picker.open(value);
        for number in 1..4 {
            frame(ctx, number, &mut picker, &mut value, vec![]);
        }
        (picker, value)
    }

    #[test]
    fn hue_supports_arrow_chords_and_endpoints_without_moving_focus() {
        let ctx = egui::Context::default();
        let (mut picker, mut value) = opened(&ctx);
        let hue = id().with("hue");
        ctx.memory_mut(|memory| memory.request_focus(hue));
        frame(
            &ctx,
            4,
            &mut picker,
            &mut value,
            keys(Key::ArrowRight, Modifiers::SHIFT),
        );
        assert!((picker.hsv.h - 0.1).abs() < 0.0001);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(hue));
        frame(
            &ctx,
            5,
            &mut picker,
            &mut value,
            keys(Key::ArrowRight, Modifiers::CTRL),
        );
        assert!((picker.hsv.h - 0.1).abs() < 0.0001);
        frame(
            &ctx,
            6,
            &mut picker,
            &mut value,
            keys(Key::End, Modifiers::NONE),
        );
        assert_eq!(picker.hsv.h, 1.0);
        frame(
            &ctx,
            7,
            &mut picker,
            &mut value,
            keys(Key::Home, Modifiers::NONE),
        );
        assert_eq!(picker.hsv.h, 0.0);
    }

    #[test]
    fn saturation_brightness_arrows_stay_inside_and_tab_leaves_the_plane() {
        let ctx = egui::Context::default();
        let (mut picker, mut value) = opened(&ctx);
        let plane = id().with("saturation-value");
        ctx.memory_mut(|memory| memory.request_focus(plane));
        let mut events = keys(Key::ArrowLeft, Modifiers::SHIFT);
        events.extend(keys(Key::ArrowDown, Modifiers::NONE));
        frame(&ctx, 4, &mut picker, &mut value, events);
        assert!((picker.hsv.s - 0.9).abs() < 0.0001);
        assert!((picker.hsv.v - 0.99).abs() < 0.0001);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(plane));
        frame(
            &ctx,
            5,
            &mut picker,
            &mut value,
            keys(Key::Tab, Modifiers::NONE),
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(id().with("hue"))
        );
    }

    #[test]
    fn invalid_hex_enter_keeps_the_dialog_and_focus_on_the_field() {
        let ctx = egui::Context::default();
        let (mut picker, mut value) = opened(&ctx);
        picker.input = "nope".into();
        frame(
            &ctx,
            4,
            &mut picker,
            &mut value,
            keys(Key::Enter, Modifiers::NONE),
        );
        assert!(picker.is_open());
        assert!(picker.invalid);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(id().with("hex"))
        );
    }

    #[test]
    fn hex_ime_preedit_and_commit_preserve_the_editor_focus() {
        let ctx = egui::Context::default();
        let (mut picker, mut value) = opened(&ctx);
        frame(
            &ctx,
            4,
            &mut picker,
            &mut value,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "12AB34".into(),
                active_range_chars: Some(0..6),
            })],
        );
        frame(
            &ctx,
            5,
            &mut picker,
            &mut value,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("12AB34".into()))],
        );
        assert_eq!(picker.input, "12AB34");
        assert_eq!(value, [0x12, 0xAB, 0x34]);
        assert!(picker.is_open());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(id().with("hex"))
        );
    }

    #[test]
    fn valid_hex_enter_closes_and_returns_focus_without_clicking_the_origin() {
        let ctx = egui::Context::default();
        let (mut picker, mut value) = opened(&ctx);
        picker.input = "#123456".into();
        assert!(!frame(
            &ctx,
            4,
            &mut picker,
            &mut value,
            keys(Key::Enter, Modifiers::NONE)
        ));
        assert!(!frame(&ctx, 5, &mut picker, &mut value, vec![]));
        assert!(!frame(&ctx, 6, &mut picker, &mut value, vec![]));
        assert!(!picker.is_open());
        assert_eq!(value, [0x12, 0x34, 0x56]);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(origin()));
    }
}
