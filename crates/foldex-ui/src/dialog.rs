//! Dialog building blocks. Every modal shares one anatomy and vocabulary:
//!
//! * a header with the title, an optional one-line subtitle for context, and
//!   the close button;
//! * a body that scrolls on its own when the window is short;
//! * a footer whose actions all sit on the right, the primary action last.
//!   Its left side only ever carries status text, never a button.
//!
//! A dialog has at most one Primary (or Danger) action. Destructive
//! confirmations use Danger; dismissal is Secondary; anything optional that
//! should recede is Ghost. Typography has four levels and uses size and color
//! only, because the interface font ships without a bold weight.
use crate::{Icon, Theme, animated_state, components, motion::MotionProfile};
use egui::{
    Align2, Color32, FontId, Id, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2,
    vec2,
};

/// Space between the dialog edge and its content, in points.
pub const PAD: f32 = 20.0;
/// Space between related rows of one section.
pub const ROW_GAP: f32 = 8.0;
/// Space between sections, and before the footer.
pub const SECTION_GAP: f32 = 20.0;
const BUTTON_GAP: f32 = 8.0;
const BUTTON_MIN: f32 = 76.0;
const BUTTON_PAD: f32 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// Confirmations and single-field prompts.
    Small,
    /// Lists, status and short preference groups.
    Medium,
    /// Multi-section forms with a preview.
    Large,
}
impl Size {
    pub fn width(self, t: &Theme) -> f32 {
        let points = match self {
            Self::Small => 420.0,
            Self::Medium => 520.0,
            Self::Large => 680.0,
        };
        points * t.metrics().scale
    }
}

/// The four text levels, plus the quiet section label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    /// Dialog titles.
    Title,
    /// Empty-state headlines and list item names.
    Heading,
    /// Primary reading text and field labels.
    Body,
    /// Supporting sentences under a heading or a field.
    Secondary,
    /// Metadata: sizes, counts, timestamps.
    Caption,
    /// Names of a group of fields.
    Section,
}
impl Text {
    pub fn font(self, t: &Theme) -> FontId {
        let size = match self {
            Self::Title => t.font_size + 3.0,
            Self::Heading => t.font_size + 1.0,
            Self::Body | Self::Secondary => t.font_size,
            Self::Caption | Self::Section => (t.font_size - 1.0).max(10.0),
        };
        FontId::proportional(size)
    }
    pub fn color(self, t: &Theme) -> Color32 {
        match self {
            Self::Title | Self::Heading | Self::Body => t.fg(),
            Self::Secondary | Self::Caption | Self::Section => t.dim(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Danger,
}
pub fn tone_color(t: &Theme, tone: Tone) -> Color32 {
    match tone {
        Tone::Neutral => t.dim(),
        Tone::Accent => t.accent(),
        Tone::Danger => Theme::color(t.danger),
    }
}

/// Readable text on a filled color, for primary and danger buttons and checks.
pub fn on_color(rgb: [u8; 3]) -> Color32 {
    let linear = rgb.map(|byte| {
        let value = byte as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    });
    let luminance = 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
    if luminance > 0.36 {
        Color32::from_rgb(20, 20, 20)
    } else {
        Color32::WHITE
    }
}

fn row_height(ui: &Ui, font: &FontId) -> f32 {
    ui.fonts_mut(|fonts| fonts.row_height(font))
}

/// Wrapping text at the cursor, using the full available width.
pub fn text(ui: &mut Ui, t: &Theme, style: Text, value: &str) -> Response {
    text_colored(ui, t, style, value, style.color(t))
}
pub fn text_colored(ui: &mut Ui, t: &Theme, style: Text, value: &str, color: Color32) -> Response {
    let width = ui.available_width().max(1.0);
    let galley = crate::fonts::layout(ui.ctx(), value, style.font(t), color, width, usize::MAX);
    let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley, color);
    response
}
pub fn text_width(ui: &Ui, t: &Theme, style: Text, value: &str) -> f32 {
    crate::fonts::layout(
        ui.ctx(),
        value,
        style.font(t),
        Color32::PLACEHOLDER,
        f32::INFINITY,
        1,
    )
    .size()
    .x
}
/// Centered, wrapping text limited to `width`.
pub fn text_centered(ui: &mut Ui, t: &Theme, style: Text, value: &str, width: f32) {
    let width = width.min(ui.available_width()).max(1.0);
    let mut job =
        egui::text::LayoutJob::simple(value.to_owned(), style.font(t), style.color(t), width);
    job.halign = egui::Align::Center;
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y), Sense::hover());
    ui.painter()
        .galley(pos2(rect.center().x, rect.top()), galley, style.color(t));
}
/// One elided line inside `rect`, vertically centered.
pub fn text_at(
    ui: &Ui,
    rect: Rect,
    t: &Theme,
    style: Text,
    value: &str,
    color: Color32,
    align: Align2,
) {
    if rect.width() <= 0.0 || value.is_empty() {
        return;
    }
    let galley = crate::fonts::layout(
        ui.ctx(),
        value,
        style.font(t),
        Color32::PLACEHOLDER,
        rect.width(),
        1,
    );
    let x = match align.x() {
        egui::Align::Min => rect.left(),
        egui::Align::Center => rect.center().x - galley.size().x * 0.5,
        egui::Align::Max => rect.right() - galley.size().x,
    };
    ui.painter()
        .with_clip_rect(rect.intersect(ui.clip_rect()))
        .galley(
            pos2(x, rect.center().y - galley.size().y * 0.5),
            galley,
            color,
        );
}

/// A group label. The first section of a body gets no extra space above it.
pub fn section(ui: &mut Ui, t: &Theme, title: &str) {
    let scale = t.metrics().scale;
    if ui.min_rect().height() > 1.0 {
        ui.add_space((SECTION_GAP - ROW_GAP) * scale);
    }
    text(ui, t, Text::Section, title);
    ui.add_space(2.0 * scale);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The one action the dialog exists for.
    Primary,
    /// A primary action that destroys data.
    Danger,
    /// Dismissal and other equal-weight actions.
    Secondary,
    /// Optional actions that should recede.
    Ghost,
}

pub fn button_width(ui: &Ui, t: &Theme, label: &str) -> f32 {
    let scale = t.metrics().scale;
    let text = crate::fonts::layout(
        ui.ctx(),
        label,
        t.font(),
        Color32::PLACEHOLDER,
        f32::INFINITY,
        1,
    )
    .size()
    .x;
    (text + 2.0 * BUTTON_PAD * scale).max(BUTTON_MIN * scale)
}

/// A button at the cursor. Disabled buttons keep their place and fade.
pub fn button(ui: &mut Ui, id: Id, t: &Theme, label: &str, kind: Kind, enabled: bool) -> Response {
    let size = vec2(button_width(ui, t, label), t.metrics().control);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut child = ui.new_child(UiBuilder::new().id_salt(id.with("scope")).max_rect(rect));
    if !enabled {
        child.disable();
    }
    button_at(&child, rect, id, t, label, kind)
}

pub fn button_at(ui: &Ui, rect: Rect, id: Id, t: &Theme, label: &str, kind: Kind) -> Response {
    let response = components::button_response(ui, rect, id);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let enabled = ui.is_enabled();
    let hover = animated_state(
        ui.ctx(),
        id.with("hover"),
        enabled && response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let press = if enabled && response.is_pointer_button_down_on() {
        1.0
    } else {
        0.0
    };
    let (fill, text, stroke) = match kind {
        Kind::Primary | Kind::Danger => {
            let base = if kind == Kind::Primary {
                t.accent
            } else {
                t.danger
            };
            let on = on_color(base);
            (
                Theme::color(base)
                    .lerp_to_gamma(on, 0.1 * hover)
                    .lerp_to_gamma(Theme::color(t.background), 0.12 * press),
                on,
                Color32::TRANSPARENT,
            )
        }
        Kind::Secondary => (
            Theme::color(t.chrome).lerp_to_gamma(Theme::color(t.hover), hover),
            t.fg(),
            t.line(),
        ),
        Kind::Ghost => (
            Theme::color(t.hover).gamma_multiply(hover),
            t.fg(),
            Color32::TRANSPARENT,
        ),
    };
    let fade = if enabled { 1.0 } else { 0.4 };
    let painter = ui.painter();
    painter.rect_filled(rect, t.radius, fill.gamma_multiply(fade));
    if stroke != Color32::TRANSPARENT {
        painter.rect_stroke(
            rect,
            t.radius,
            Stroke::new(1.0, stroke.gamma_multiply(fade)),
            StrokeKind::Inside,
        );
    }
    text_at(
        ui,
        rect.shrink2(vec2(8.0 * t.metrics().scale, 0.0)),
        t,
        Text::Body,
        label,
        text.gamma_multiply(fade),
        Align2::CENTER_CENTER,
    );
    components::focus_ring(ui, rect, id, t, response.has_focus());
    response
}

/// Status marks drawn in a circle: done, failed, informational, waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Done,
    Error,
    Info,
    Waiting,
    Stopped,
}
pub fn glyph(ui: &Ui, center: egui::Pos2, size: f32, glyph: Glyph, color: Color32) {
    let p = ui.painter();
    let r = size * 0.5;
    let stroke = Stroke::new(1.3, color);
    p.circle_stroke(center, r - 0.65, stroke);
    let at = |x: f32, y: f32| center + vec2(x * r, y * r);
    match glyph {
        Glyph::Done => {
            p.line_segment([at(-0.42, 0.02), at(-0.12, 0.32)], stroke);
            p.line_segment([at(-0.12, 0.32), at(0.44, -0.3)], stroke);
        }
        Glyph::Error => {
            p.line_segment([at(0.0, -0.46), at(0.0, 0.1)], stroke);
            p.circle_filled(at(0.0, 0.4), 1.0, color);
        }
        Glyph::Info => {
            p.line_segment([at(0.0, -0.06), at(0.0, 0.46)], stroke);
            p.circle_filled(at(0.0, -0.38), 1.0, color);
        }
        Glyph::Waiting => {
            p.line_segment([at(0.0, -0.5), at(0.0, 0.0)], stroke);
            p.line_segment([at(0.0, 0.0), at(0.36, 0.2)], stroke);
        }
        Glyph::Stopped => {
            p.line_segment([at(-0.4, 0.0), at(0.4, 0.0)], stroke);
        }
    }
}

/// An indeterminate activity mark the same size as a glyph.
pub fn spinner(ui: &Ui, center: egui::Pos2, size: f32, color: Color32) {
    let time = ui.input(|input| input.time) as f32;
    let start = time * 5.0;
    let r = size * 0.5 - 0.65;
    let points: Vec<_> = (0..=20)
        .map(|step| {
            let angle = start + step as f32 / 20.0 * std::f32::consts::PI * 1.4;
            center + vec2(angle.cos(), angle.sin()) * r
        })
        .collect();
    ui.painter()
        .add(egui::Shape::line(points, Stroke::new(1.5, color)));
    ui.ctx().request_repaint();
}

/// An inline message that belongs to the content: an error that blocks the
/// primary action, or a note the person needs before deciding.
pub fn callout(ui: &mut Ui, t: &Theme, tone: Tone, message: &str) -> Response {
    let scale = t.metrics().scale;
    let color = tone_color(t, tone);
    let pad = vec2(12.0, 9.0) * scale;
    let icon = 14.0 * scale;
    let width = ui.available_width();
    let text_width = (width - pad.x * 2.0 - icon - 8.0 * scale).max(1.0);
    let galley = crate::fonts::layout(
        ui.ctx(),
        message,
        Text::Body.font(t),
        t.fg(),
        text_width,
        usize::MAX,
    );
    let height = (galley.size().y + pad.y * 2.0).max(t.metrics().control);
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let tint = if tone == Tone::Neutral {
        Theme::color(t.chrome)
    } else {
        color.gamma_multiply(0.12)
    };
    ui.painter().rect_filled(rect, t.radius, tint);
    ui.painter().rect_stroke(
        rect,
        t.radius,
        Stroke::new(
            1.0,
            if tone == Tone::Neutral {
                t.line()
            } else {
                color.gamma_multiply(0.35)
            },
        ),
        StrokeKind::Inside,
    );
    let first_line = row_height(ui, &Text::Body.font(t));
    glyph(
        ui,
        pos2(
            rect.left() + pad.x + icon * 0.5,
            rect.top() + pad.y + first_line * 0.5,
        ),
        icon,
        if tone == Tone::Danger {
            Glyph::Error
        } else {
            Glyph::Info
        },
        color,
    );
    ui.painter().galley(
        pos2(rect.left() + pad.x + icon + 8.0 * scale, rect.top() + pad.y),
        galley,
        t.fg(),
    );
    response
}

/// A thin determinate bar, or a travelling segment while the total is unknown.
pub fn progress(ui: &mut Ui, t: &Theme, fraction: Option<f32>) -> Rect {
    let height = 4.0 * t.metrics().scale;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    progress_at(ui, rect, t, fraction);
    rect
}
pub fn progress_at(ui: &Ui, rect: Rect, t: &Theme, fraction: Option<f32>) {
    let radius = (rect.height() * 0.5).round() as u8;
    let p = ui.painter();
    p.rect_filled(rect, radius, t.line());
    let fill = match fraction {
        Some(fraction) => Rect::from_min_size(
            rect.min,
            vec2(rect.width() * fraction.clamp(0.0, 1.0), rect.height()),
        ),
        None => {
            let time = ui.input(|input| input.time) as f32;
            let width = rect.width() * 0.3;
            let phase = if t.duration() == 0.0 {
                0.35
            } else {
                (time * 0.8).fract()
            };
            let left = rect.left() - width + (rect.width() + width) * phase;
            ui.ctx().request_repaint();
            Rect::from_min_max(
                pos2(left.max(rect.left()), rect.top()),
                pos2((left + width).min(rect.right()), rect.bottom()),
            )
        }
    };
    if fill.width() > 0.0 {
        p.rect_filled(fill, radius, t.accent());
    }
}

/// The empty state of a list: what is missing, why it matters, how to fill it.
/// Returns the response of the optional action.
pub fn empty_state(
    ui: &mut Ui,
    t: &Theme,
    icon: Icon,
    title: &str,
    description: &str,
    action: Option<(Id, &str)>,
) -> Option<Response> {
    let scale = t.metrics().scale;
    ui.add_space(14.0 * scale);
    let (badge, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), 44.0 * scale), Sense::hover());
    let circle = Rect::from_center_size(badge.center(), vec2(44.0, 44.0) * scale);
    ui.painter()
        .rect_filled(circle, (22.0 * scale) as u8, Theme::color(t.chrome));
    ui.painter().rect_stroke(
        circle,
        (22.0 * scale) as u8,
        Stroke::new(1.0, t.line()),
        StrokeKind::Inside,
    );
    crate::icon(ui.painter(), circle.center(), icon, t.dim(), 20.0 * scale);
    ui.add_space(12.0 * scale);
    text_centered(ui, t, Text::Heading, title, 320.0 * scale);
    ui.add_space(4.0 * scale);
    text_centered(ui, t, Text::Secondary, description, 320.0 * scale);
    let response = action.map(|(id, label)| {
        ui.add_space(16.0 * scale);
        let width = button_width(ui, t, label);
        let (row, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), t.metrics().control),
            Sense::hover(),
        );
        button_at(
            ui,
            Rect::from_center_size(row.center(), vec2(width, row.height())),
            id,
            t,
            label,
            Kind::Secondary,
        )
    });
    ui.add_space(14.0 * scale);
    response
}

/// Rows with a shared label column. Labels are measured once per dialog, so
/// every control starts on the same edge.
#[derive(Clone, Copy)]
pub struct Form {
    pub label_width: f32,
}
impl Form {
    pub fn new(ui: &Ui, t: &Theme, labels: &[&str]) -> Self {
        let widest = labels
            .iter()
            .map(|label| {
                crate::fonts::layout(
                    ui.ctx(),
                    label,
                    Text::Body.font(t),
                    Color32::PLACEHOLDER,
                    f32::INFINITY,
                    1,
                )
                .size()
                .x
            })
            .fold(0.0, f32::max);
        Self {
            label_width: (widest + 20.0 * t.metrics().scale).min(ui.available_width() * 0.4),
        }
    }
    /// A labelled row. The content column lays out left to right and may wrap
    /// onto more lines; the label stays level with its first line.
    pub fn row<R>(&self, ui: &mut Ui, t: &Theme, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
        let m = t.metrics();
        let top = ui.cursor().min;
        let width = ui.available_width();
        let content = Rect::from_min_size(
            pos2(top.x + self.label_width, top.y),
            vec2((width - self.label_width).max(1.0), m.control),
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .id_salt(("form-row", label))
                .max_rect(content)
                .layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true)),
        );
        child.spacing_mut().item_spacing = vec2(8.0 * m.scale, ROW_GAP * m.scale);
        let inner = add(&mut child);
        let used = child.min_rect().height().max(m.control);
        text_at(
            ui,
            Rect::from_min_size(top, vec2(self.label_width - 8.0 * m.scale, m.control)),
            t,
            Text::Body,
            label,
            Text::Body.color(t),
            Align2::LEFT_CENTER,
        );
        ui.allocate_exact_size(vec2(width, used), Sense::hover());
        inner
    }
    /// Content aligned with the controls, without a label (checkboxes, hints).
    pub fn indent<R>(&self, ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
        inset(ui, self.label_width, add)
    }
}

/// A text field that fills the rest of its row.
pub fn field(
    ui: &mut Ui,
    t: &Theme,
    id: Id,
    value: &mut String,
    hint: &str,
    limit: usize,
) -> egui::text_edit::TextEditOutput {
    // Measured from the cursor: in a wrapping row `available_width` is the
    // whole row, which would push the field onto a line of its own.
    let width = (ui.max_rect().right() - ui.cursor().min.x).max(t.metrics().control);
    field_sized(ui, t, id, value, hint, limit, width)
}

pub fn field_sized(
    ui: &mut Ui,
    t: &Theme,
    id: Id,
    value: &mut String,
    hint: &str,
    limit: usize,
    width: f32,
) -> egui::text_edit::TextEditOutput {
    let (rect, _) = ui.allocate_exact_size(vec2(width, t.metrics().control), Sense::hover());
    components::input_box(ui, rect, id, t, value, hint, limit)
}

/// The label of a second control on a form row, such as "Replace with"
/// after a Find field. It keeps a wider gap before it than after it.
pub fn inline_label(ui: &mut Ui, t: &Theme, label: &str) {
    let scale = t.metrics().scale;
    if ui.cursor().min.x > ui.max_rect().left() + 1.0 {
        ui.add_space(8.0 * scale);
    }
    let (rect, _) = ui.allocate_exact_size(
        vec2(text_width(ui, t, Text::Body, label), t.metrics().control),
        Sense::hover(),
    );
    text_at(
        ui,
        rect,
        t,
        Text::Body,
        label,
        Text::Body.color(t),
        Align2::LEFT_CENTER,
    );
}

/// A unit after a field, such as "days". It stays close to its field.
pub fn unit(ui: &mut Ui, t: &Theme, label: &str) {
    let (rect, _) = ui.allocate_exact_size(
        vec2(text_width(ui, t, Text::Body, label), t.metrics().control),
        Sense::hover(),
    );
    text_at(ui, rect, t, Text::Body, label, t.dim(), Align2::LEFT_CENTER);
}

/// Width for the first of two fields sharing a form row, so the second
/// field's label starts at the middle of the row.
pub fn half(ui: &Ui, t: &Theme) -> f32 {
    ((ui.available_width() - 8.0 * t.metrics().scale) * 0.5).max(1.0)
}

/// A compact insert action, such as a template token. Chips sit under the
/// field they feed, never in the label column.
pub fn chip(ui: &mut Ui, id: Id, t: &Theme, label: &str) -> Response {
    let scale = t.metrics().scale;
    let pad = 10.0 * scale;
    let icon_size = 10.0 * scale;
    let gap = 5.0 * scale;
    let width = pad * 2.0 + icon_size + gap + text_width(ui, t, Text::Caption, label);
    let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0 * scale), Sense::hover());
    let response = components::button_response(ui, rect, id);
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });
    let hover = animated_state(
        ui.ctx(),
        id.with("hover"),
        ui.is_enabled() && response.hovered(),
        t,
        MotionProfile::Fast,
    );
    let radius = (rect.height() * 0.5) as u8;
    ui.painter().rect_filled(
        rect,
        radius,
        Theme::color(t.chrome).lerp_to_gamma(Theme::color(t.hover), hover),
    );
    ui.painter()
        .rect_stroke(rect, radius, Stroke::new(1.0, t.line()), StrokeKind::Inside);
    let color = t.dim().lerp_to_gamma(t.fg(), hover);
    crate::icon(
        ui.painter(),
        pos2(rect.left() + pad + icon_size * 0.5, rect.center().y),
        Icon::Plus,
        color,
        icon_size,
    );
    text_at(
        ui,
        Rect::from_min_max(
            pos2(rect.left() + pad + icon_size + gap, rect.top()),
            rect.max,
        ),
        t,
        Text::Caption,
        label,
        color,
        Align2::LEFT_CENTER,
    );
    components::focus_ring_with_radius(ui, rect, id, t, response.has_focus(), radius);
    response
}

/// A top-down column that starts `left` points in from the current edge.
pub fn inset<R>(ui: &mut Ui, left: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let rect = ui.available_rect_before_wrap();
    let column = Rect::from_min_max(pos2(rect.left() + left, rect.top()), rect.max);
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(column)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        add,
    )
    .inner
}

/// A checkbox whose box sits flush with the text column. The toolbar checkbox
/// centres its box in a control-sized square, which reads as an indent here.
pub fn checkbox(ui: &mut Ui, t: &Theme, value: &mut bool, label: &str) -> Response {
    let m = t.metrics();
    let gap = 8.0 * m.scale;
    let width = (m.icon + gap + text_width(ui, t, Text::Body, label)).min(ui.available_width());
    let (id, rect) = ui.allocate_space(vec2(width, m.control));
    let mut response = components::button_response(ui, rect, id);
    if response.clicked() {
        *value = !*value;
        response.mark_changed();
        response.request_focus();
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Checkbox, ui.is_enabled(), *value, label)
    });
    let glyph = Rect::from_min_size(
        pos2(rect.left(), rect.center().y - m.icon * 0.5),
        vec2(m.icon, m.icon),
    );
    components::checkbox_mark(ui, glyph, &response, t, i8::from(*value));
    text_at(
        ui,
        Rect::from_min_max(pos2(glyph.right() + gap, rect.top()), rect.max),
        t,
        Text::Body,
        label,
        t.fg(),
        Align2::LEFT_CENTER,
    );
    components::focus_ring(ui, rect.expand(2.0 * m.scale), id, t, response.has_focus());
    response
}

/// One measurement in a read-only list: label left, value right.
pub fn property(ui: &mut Ui, t: &Theme, label: &str, value: &str) {
    let height = t.metrics().control * 0.85;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let half = rect.width() * 0.5;
    text_at(
        ui,
        Rect::from_min_size(rect.min, vec2(half, height)),
        t,
        Text::Body,
        label,
        t.dim(),
        Align2::LEFT_CENTER,
    );
    text_at(
        ui,
        Rect::from_min_max(pos2(rect.left() + half, rect.top()), rect.max),
        t,
        Text::Body,
        value,
        t.fg(),
        Align2::RIGHT_CENTER,
    );
}

pub struct Action<'a> {
    pub id: Id,
    pub label: &'a str,
    pub kind: Kind,
    pub enabled: bool,
}
impl<'a> Action<'a> {
    pub fn new(id: Id, label: &'a str, kind: Kind) -> Self {
        Self {
            id,
            label,
            kind,
            enabled: true,
        }
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Actions in visual order, left to right; the last one is the primary.
#[derive(Default)]
pub struct Footer<'a> {
    pub status: Option<(&'a str, Tone)>,
    pub actions: Vec<Action<'a>>,
}
impl<'a> Footer<'a> {
    pub fn new(actions: Vec<Action<'a>>) -> Self {
        Self {
            status: None,
            actions,
        }
    }
    pub fn status(mut self, text: &'a str, tone: Tone) -> Self {
        if !text.is_empty() {
            self.status = Some((text, tone));
        }
        self
    }
}

pub struct Dialog<'a> {
    pub id: Id,
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    pub width: f32,
    pub interactive: bool,
    pub footer: Option<Footer<'a>>,
}
pub struct Shown<R> {
    pub inner: R,
    /// Index into the footer's actions.
    pub clicked: Option<usize>,
    pub actions: Vec<Response>,
}
impl<'a> Dialog<'a> {
    pub fn new(id: Id, title: &'a str, width: f32) -> Self {
        Self {
            id,
            title,
            subtitle: None,
            width,
            interactive: true,
            footer: None,
        }
    }
    pub fn subtitle(mut self, subtitle: &'a str) -> Self {
        if !subtitle.is_empty() {
            self.subtitle = Some(subtitle);
        }
        self
    }
    pub fn interactive(mut self, interactive: bool) -> Self {
        self.interactive = interactive;
        self
    }
    pub fn footer(mut self, footer: Footer<'a>) -> Self {
        self.footer = Some(footer);
        self
    }
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        t: &Theme,
        open: &mut bool,
        body: impl FnOnce(&mut Ui) -> R,
    ) -> Shown<R> {
        components::dialog_frame(ctx, t, self, open, body)
    }

    pub(crate) fn header_height(&self, ctx: &egui::Context, t: &Theme) -> f32 {
        let scale = t.metrics().scale;
        let title = ctx.fonts_mut(|fonts| fonts.row_height(&Text::Title.font(t)));
        let subtitle = self.subtitle.map_or(0.0, |_| {
            2.0 * scale + ctx.fonts_mut(|fonts| fonts.row_height(&Text::Secondary.font(t)))
        });
        (PAD - 2.0) * scale + title + subtitle + 14.0 * scale
    }
    pub(crate) fn footer_height(&self, t: &Theme) -> f32 {
        let scale = t.metrics().scale;
        if self
            .footer
            .as_ref()
            .is_some_and(|footer| !footer.actions.is_empty() || footer.status.is_some())
        {
            SECTION_GAP * scale + t.metrics().control + PAD * scale
        } else {
            PAD * scale
        }
    }

    /// Header, close button and footer for a dialog occupying `outer`.
    pub(crate) fn paint_chrome(
        &self,
        ui: &mut Ui,
        t: &Theme,
        outer: Rect,
        overflow: bool,
    ) -> (bool, Option<usize>, Vec<Response>) {
        let scale = t.metrics().scale;
        let control = t.metrics().control;
        let header = Rect::from_min_size(
            outer.min,
            vec2(outer.width(), self.header_height(ui.ctx(), t)),
        );
        let title_height = row_height(ui, &Text::Title.font(t));
        let title_top = header.top() + (PAD - 2.0) * scale;
        let close = Rect::from_center_size(
            pos2(
                outer.right() - (PAD - 6.0) * scale - control * 0.5,
                title_top + title_height * 0.5,
            ),
            vec2(control, control),
        );
        text_at(
            ui,
            Rect::from_min_max(
                pos2(outer.left() + PAD * scale, title_top),
                pos2(close.left() - 8.0 * scale, title_top + title_height),
            ),
            t,
            Text::Title,
            self.title,
            Text::Title.color(t),
            Align2::LEFT_CENTER,
        );
        if let Some(subtitle) = self.subtitle {
            let top = title_top + title_height + 2.0 * scale;
            text_at(
                ui,
                Rect::from_min_max(
                    pos2(outer.left() + PAD * scale, top),
                    pos2(
                        close.left() - 8.0 * scale,
                        top + row_height(ui, &Text::Secondary.font(t)),
                    ),
                ),
                t,
                Text::Secondary,
                subtitle,
                Text::Secondary.color(t),
                Align2::LEFT_CENTER,
            );
        }
        let bottom = outer.bottom();
        let footer_height = self.footer_height(t);
        if overflow {
            let line = Stroke::new(1.0, t.line());
            ui.painter().hline(outer.x_range(), header.bottom(), line);
            ui.painter()
                .hline(outer.x_range(), bottom - footer_height + 8.0 * scale, line);
        }
        let mut clicked = None;
        let mut responses = Vec::new();
        if let Some(footer) = &self.footer {
            let row = Rect::from_min_size(
                pos2(outer.left() + PAD * scale, bottom - PAD * scale - control),
                vec2(outer.width() - 2.0 * PAD * scale, control),
            );
            let mut right = row.right();
            let mut rects = Vec::with_capacity(footer.actions.len());
            for action in footer.actions.iter().rev() {
                let width = button_width(ui, t, action.label);
                rects.push(Rect::from_min_max(
                    pos2(right - width, row.top()),
                    pos2(right, row.bottom()),
                ));
                right -= width + BUTTON_GAP * scale;
            }
            rects.reverse();
            for (index, (action, rect)) in footer.actions.iter().zip(rects).enumerate() {
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .id_salt(action.id.with("footer"))
                        .max_rect(rect),
                );
                if !action.enabled {
                    child.disable();
                }
                let response = button_at(&child, rect, action.id, t, action.label, action.kind);
                if response.clicked() {
                    clicked = Some(index);
                }
                responses.push(response);
            }
            if let Some((status, tone)) = footer.status {
                text_at(
                    ui,
                    Rect::from_min_max(
                        row.min,
                        pos2(right + BUTTON_GAP * scale - 12.0 * scale, row.bottom()),
                    ),
                    t,
                    Text::Body,
                    status,
                    if tone == Tone::Neutral {
                        t.dim()
                    } else {
                        tone_color(t, tone)
                    },
                    Align2::LEFT_CENTER,
                );
            }
        }
        // Registered last so Tab reaches the footer actions before the close
        // button; Escape already dismisses without it.
        let dismiss =
            components::icon_button_at(ui, close, self.id.with("close"), t, Icon::Close, false)
                .on_hover_text("Close")
                .clicked();
        (dismiss, clicked, responses)
    }
}

/// The items an action affects, in a quiet frame: up to `names.len()` rows,
/// then a count of the rest. Each row reports its hover for a full-path tip.
pub fn item_list(ui: &mut Ui, t: &Theme, id: Id, names: &[(String, String)], total: usize) {
    let scale = t.metrics().scale;
    let row = t.metrics().control * 0.9;
    let rest = total.saturating_sub(names.len());
    let pad = vec2(12.0, 6.0) * scale;
    let height = pad.y * 2.0 + row * (names.len() + usize::from(rest > 0)) as f32;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    ui.painter()
        .rect_filled(rect, t.radius, Theme::color(t.background));
    ui.painter().rect_stroke(
        rect,
        t.radius,
        Stroke::new(1.0, t.line()),
        StrokeKind::Inside,
    );
    let icon_size = 14.0 * scale;
    for (index, (name, tip)) in names.iter().enumerate() {
        let line = Rect::from_min_size(
            pos2(rect.left() + pad.x, rect.top() + pad.y + row * index as f32),
            vec2(rect.width() - pad.x * 2.0, row),
        );
        crate::icon(
            ui.painter(),
            pos2(line.left() + icon_size * 0.5, line.center().y),
            Icon::File,
            t.dim(),
            icon_size,
        );
        text_at(
            ui,
            Rect::from_min_max(
                pos2(line.left() + icon_size + 8.0 * scale, line.top()),
                line.max,
            ),
            t,
            Text::Body,
            name,
            t.fg(),
            Align2::LEFT_CENTER,
        );
        ui.interact(line, id.with(("item", index)), Sense::hover())
            .on_hover_text(tip);
    }
    if rest > 0 {
        let line = Rect::from_min_size(
            pos2(
                rect.left() + pad.x + icon_size + 8.0 * scale,
                rect.top() + pad.y + row * names.len() as f32,
            ),
            vec2(rect.width() - pad.x * 2.0, row),
        );
        text_at(
            ui,
            line,
            t,
            Text::Secondary,
            &format!("and {rest} more"),
            t.dim(),
            Align2::LEFT_CENTER,
        );
    }
}

/// A selectable row in a dialog list: icon, name, quiet detail and an
/// optional trailing icon action. Returns the row and the trailing action.
pub fn list_row(
    ui: &mut Ui,
    t: &Theme,
    id: Id,
    leading: Icon,
    name: &str,
    detail: &str,
    trailing: Option<(Icon, &str)>,
) -> (Response, Option<Response>) {
    let m = t.metrics();
    let scale = m.scale;
    let height = m.control + 8.0 * scale;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let row = components::button_response(ui, rect, id);
    row.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), name));
    let hover = animated_state(
        ui.ctx(),
        id.with("hover"),
        row.hovered(),
        t,
        MotionProfile::Fast,
    );
    ui.painter()
        .rect_filled(rect, t.radius, Theme::color(t.hover).gamma_multiply(hover));
    let inset = 10.0 * scale;
    let icon_size = m.icon;
    crate::icon(
        ui.painter(),
        pos2(rect.left() + inset + icon_size * 0.5, rect.center().y),
        leading,
        t.dim().lerp_to_gamma(t.fg(), hover),
        icon_size,
    );
    let action_rect = trailing.map(|_| {
        Rect::from_center_size(
            pos2(
                rect.right() - 4.0 * scale - m.control * 0.5,
                rect.center().y,
            ),
            vec2(m.control, m.control),
        )
    });
    let end = action_rect.map_or(rect.right() - inset, |action| action.left() - 4.0 * scale);
    let detail_width = if detail.is_empty() {
        0.0
    } else {
        crate::fonts::layout(
            ui.ctx(),
            detail,
            Text::Caption.font(t),
            Color32::PLACEHOLDER,
            f32::INFINITY,
            1,
        )
        .size()
        .x
    };
    let text_left = rect.left() + inset + icon_size + 10.0 * scale;
    text_at(
        ui,
        Rect::from_min_max(
            pos2(text_left, rect.top()),
            pos2(end - detail_width - 12.0 * scale, rect.bottom()),
        ),
        t,
        Text::Body,
        name,
        t.fg(),
        Align2::LEFT_CENTER,
    );
    text_at(
        ui,
        Rect::from_min_max(
            pos2(end - detail_width, rect.top()),
            pos2(end, rect.bottom()),
        ),
        t,
        Text::Caption,
        detail,
        t.dim(),
        Align2::RIGHT_CENTER,
    );
    components::focus_ring(ui, rect, id, t, row.has_focus());
    let action = trailing.zip(action_rect).map(|((glyph, tip), rect)| {
        components::icon_button_at(ui, rect, id.with("trailing"), t, glyph, false)
            .on_hover_text(tip)
    });
    (row, action)
}
