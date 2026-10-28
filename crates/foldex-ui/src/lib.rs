//! Small reusable design system. All visual decisions live here or in theme.toml.
pub mod breadcrumb;
pub mod color;
pub mod components;
pub mod focus;
pub mod fonts;
pub mod inline_edit;
pub mod motion;
pub mod rolling;
pub mod view;
use egui::{Align2, Color32, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
pub mod theme;
pub use theme::{ColorMode, ColorScheme, Palette, ResolvedMode, Theme};

#[derive(Clone, Copy)]
pub enum Icon {
    Empty,
    Folder,
    File,
    Image,
    Code,
    Archive,
    Music,
    Video,
    Back,
    Forward,
    Up,
    Down,
    MoveDown,
    ChevronUp,
    Search,
    Close,
    Plus,
    Minus,
    Menu,
    More,
    Ellipsis,
    Split,
    SplitBelow,
    Commands,
    Keyboard,
    Help,
    Inspect,
    Settings,
    Refresh,
    Bookmark,
    BookmarkFilled,
    Clock,
    Drive,
    Home,
    Copy,
    Cut,
    Paste,
    Save,
    Star,
    StarOutline,
    Rename,
    Trash,
    Check,
    SelectAll,
    SelectNone,
    InvertSelection,
    Expand,
    Recursive,
    List,
    Grid,
    Columns,
    Picture,
    Window,
    Calculator,
    Filter,
}

pub fn icon(p: &egui::Painter, center: Pos2, icon: Icon, color: Color32, size: f32) {
    if !p.is_visible() || p.opacity() == 0.0 || color == Color32::TRANSPARENT {
        return;
    }
    let r = Rect::from_center_size(center, vec2(size, size));
    let q = |x: f32, y: f32| pos2(r.left() + x * size, r.top() + y * size);
    let s = Stroke::new(1.2, color);
    let line = |a: (f32, f32), b: (f32, f32)| {
        p.line_segment([q(a.0, a.1), q(b.0, b.1)], s);
    };
    match icon {
        Icon::Window => {
            p.rect_stroke(
                Rect::from_min_max(q(0.08, 0.12), q(0.92, 0.88)),
                2,
                s,
                egui::StrokeKind::Inside,
            );
            line((0.08, 0.34), (0.92, 0.34));
            line((0.2, 0.23), (0.3, 0.23));
        }
        Icon::Calculator => {
            p.rect_stroke(
                Rect::from_min_max(q(0.18, 0.04), q(0.82, 0.96)),
                2,
                s,
                egui::StrokeKind::Inside,
            );
            p.rect_stroke(
                Rect::from_min_max(q(0.3, 0.17), q(0.7, 0.36)),
                1,
                s,
                egui::StrokeKind::Inside,
            );
            for y in [0.52, 0.68, 0.84] {
                for x in [0.34, 0.5, 0.66] {
                    p.circle_filled(q(x, y), 0.045 * size, color);
                }
            }
        }
        Icon::Empty => {}
        Icon::Filter => {
            p.add(egui::Shape::closed_line(
                vec![
                    q(0.08, 0.14),
                    q(0.92, 0.14),
                    q(0.60, 0.52),
                    q(0.60, 0.86),
                    q(0.40, 0.76),
                    q(0.40, 0.52),
                ],
                s,
            ));
        }
        Icon::Recursive => {
            // Two folder branches keep recursive search distinct from zoom or
            // window expansion, including at the footer's small control size.
            line((0.10, 0.25), (0.43, 0.25));
            line((0.22, 0.25), (0.22, 0.76));
            line((0.22, 0.76), (0.43, 0.76));
            for y in [0.07, 0.58] {
                p.add(egui::Shape::closed_line(
                    vec![
                        q(0.43, y),
                        q(0.65, y),
                        q(0.73, y + 0.07),
                        q(0.96, y + 0.07),
                        q(0.96, y + 0.35),
                        q(0.43, y + 0.35),
                    ],
                    s,
                ));
            }
        }
        Icon::Folder => {
            p.rect_filled(Rect::from_min_max(q(0.04, 0.18), q(0.47, 0.46)), 2, color);
            p.rect_filled(Rect::from_min_max(q(0.03, 0.32), q(0.97, 0.87)), 2, color);
            p.line_segment(
                [q(0.1, 0.38), q(0.9, 0.38)],
                Stroke::new(1.0, Color32::from_white_alpha(65)),
            );
        }
        Icon::Back => {
            line((0.65, 0.18), (0.32, 0.5));
            line((0.32, 0.5), (0.65, 0.82));
        }
        Icon::Forward => {
            line((0.35, 0.18), (0.68, 0.5));
            line((0.68, 0.5), (0.35, 0.82));
        }
        Icon::Up => {
            line((0.2, 0.48), (0.5, 0.2));
            line((0.5, 0.2), (0.8, 0.48));
            line((0.5, 0.2), (0.5, 0.9));
        }
        Icon::Down => {
            line((0.2, 0.35), (0.5, 0.65));
            line((0.5, 0.65), (0.8, 0.35));
        }
        Icon::MoveDown => {
            line((0.2, 0.52), (0.5, 0.8));
            line((0.5, 0.8), (0.8, 0.52));
            line((0.5, 0.1), (0.5, 0.8));
        }
        Icon::ChevronUp => {
            line((0.2, 0.65), (0.5, 0.35));
            line((0.5, 0.35), (0.8, 0.65));
        }
        Icon::Close => {
            line((0.25, 0.25), (0.75, 0.75));
            line((0.25, 0.75), (0.75, 0.25));
        }
        Icon::Plus => {
            line((0.18, 0.5), (0.82, 0.5));
            line((0.5, 0.18), (0.5, 0.82));
        }
        Icon::Minus => line((0.18, 0.5), (0.82, 0.5)),
        Icon::Search => {
            p.circle_stroke(q(0.43, 0.43), size * 0.28, s);
            line((0.64, 0.64), (0.9, 0.9));
        }
        Icon::Menu => {
            for y in [0.25, 0.5, 0.75] {
                line((0.18, y), (0.82, y));
            }
        }
        Icon::More => {
            for y in [0.2, 0.5, 0.8] {
                p.circle_filled(q(0.5, y), 1.2, color);
            }
        }
        Icon::Ellipsis => {
            for x in [0.2, 0.5, 0.8] {
                p.circle_filled(q(x, 0.5), 1.2, color);
            }
        }
        Icon::List => {
            for y in [0.22, 0.5, 0.78] {
                p.circle_filled(q(0.13, y), size * 0.045, color);
                line((0.32, y), (0.92, y));
            }
        }
        Icon::Grid => {
            for y in [0.12, 0.57] {
                for x in [0.12, 0.57] {
                    p.rect_stroke(
                        Rect::from_min_max(q(x, y), q(x + 0.3, y + 0.3)),
                        1,
                        s,
                        StrokeKind::Inside,
                    );
                }
            }
        }
        Icon::Split => {
            p.rect_stroke(r.shrink(size * 0.12), 1, s, StrokeKind::Inside);
            line((0.5, 0.15), (0.5, 0.85));
        }
        Icon::SplitBelow => {
            p.rect_stroke(r.shrink(size * 0.12), 1, s, StrokeKind::Inside);
            line((0.15, 0.5), (0.85, 0.5));
        }
        Icon::Commands => {
            line((0.12, 0.2), (0.38, 0.5));
            line((0.38, 0.5), (0.12, 0.8));
            line((0.52, 0.76), (0.9, 0.76));
        }
        Icon::Keyboard => {
            p.rect_stroke(
                Rect::from_min_max(q(0.04, 0.2), q(0.96, 0.8)),
                2,
                s,
                StrokeKind::Inside,
            );
            for y in [0.37, 0.53] {
                for x in [0.22, 0.41, 0.6, 0.79] {
                    p.circle_filled(q(x, y), size * 0.035, color);
                }
            }
            line((0.32, 0.67), (0.68, 0.67));
        }
        Icon::Help => {
            p.circle_stroke(center, size * 0.4, s);
            p.add(egui::Shape::line(
                vec![
                    q(0.36, 0.32),
                    q(0.43, 0.25),
                    q(0.58, 0.25),
                    q(0.65, 0.34),
                    q(0.63, 0.43),
                    q(0.5, 0.52),
                    q(0.5, 0.58),
                ],
                s,
            ));
            p.circle_filled(q(0.5, 0.73), size * 0.04, color);
        }
        Icon::Columns => {
            p.rect_stroke(r.shrink(size * 0.12), 1, s, StrokeKind::Inside);
            for x in [0.37, 0.63] {
                line((x, 0.15), (x, 0.85));
            }
        }
        Icon::Picture => {
            p.rect_stroke(r.shrink(size * 0.12), 1, s, StrokeKind::Inside);
            p.circle_filled(q(0.35, 0.34), size * 0.065, color);
            line((0.2, 0.75), (0.48, 0.48));
            line((0.48, 0.48), (0.8, 0.76));
        }
        Icon::Inspect => {
            p.rect_stroke(r.shrink(size * 0.12), 1, s, StrokeKind::Inside);
            line((0.62, 0.15), (0.62, 0.85));
            line((0.73, 0.32), (0.82, 0.32));
            line((0.73, 0.5), (0.82, 0.5));
        }
        Icon::Drive => {
            p.rect_stroke(
                Rect::from_min_max(q(0.1, 0.36), q(0.9, 0.76)),
                2,
                s,
                StrokeKind::Inside,
            );
            line((0.18, 0.36), (0.28, 0.17));
            line((0.28, 0.17), (0.72, 0.17));
            line((0.72, 0.17), (0.82, 0.36));
            p.circle_filled(q(0.76, 0.58), 1.0, color);
        }
        Icon::Home => {
            line((0.1, 0.45), (0.5, 0.12));
            line((0.5, 0.12), (0.9, 0.45));
            line((0.22, 0.35), (0.22, 0.88));
            line((0.22, 0.88), (0.78, 0.88));
            line((0.78, 0.88), (0.78, 0.35));
        }
        Icon::Bookmark => {
            line((0.25, 0.14), (0.75, 0.14));
            line((0.25, 0.14), (0.25, 0.87));
            line((0.75, 0.14), (0.75, 0.87));
            line((0.25, 0.87), (0.5, 0.67));
            line((0.75, 0.87), (0.5, 0.67));
        }
        Icon::BookmarkFilled => {
            // The notch makes this polygon concave. Two convex halves retain
            // the exact outline while remaining valid epaint fill primitives.
            for points in [
                vec![q(0.25, 0.14), q(0.5, 0.14), q(0.5, 0.67), q(0.25, 0.87)],
                vec![q(0.5, 0.14), q(0.75, 0.14), q(0.75, 0.87), q(0.5, 0.67)],
            ] {
                p.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
            }
        }
        Icon::Save => {
            for (a, b) in [
                ((0.12, 0.12), (0.72, 0.12)),
                ((0.72, 0.12), (0.88, 0.28)),
                ((0.88, 0.28), (0.88, 0.88)),
                ((0.88, 0.88), (0.12, 0.88)),
                ((0.12, 0.88), (0.12, 0.12)),
            ] {
                line(a, b);
            }
            p.rect_stroke(
                Rect::from_min_max(q(0.3, 0.12), q(0.68, 0.43)),
                0,
                s,
                StrokeKind::Inside,
            );
            p.rect_stroke(
                Rect::from_min_max(q(0.28, 0.61), q(0.72, 0.88)),
                0,
                s,
                StrokeKind::Inside,
            );
        }
        Icon::Star | Icon::StarOutline => {
            let points: Vec<_> = (0..10)
                .map(|n| {
                    let angle =
                        std::f32::consts::TAU * n as f32 / 10.0 - std::f32::consts::FRAC_PI_2;
                    let radius = size * if n % 2 == 0 { 0.44 } else { 0.20 };
                    center + vec2(angle.cos(), angle.sin()) * radius
                })
                .collect();
            if matches!(icon, Icon::Star) {
                let mut mesh = egui::epaint::Mesh::default();
                mesh.reserve_vertices(11);
                mesh.reserve_triangles(10);
                mesh.colored_vertex(center, color);
                for point in &points {
                    mesh.colored_vertex(*point, color);
                }
                for n in 0..10 {
                    mesh.add_triangle(0, n + 1, (n + 1) % 10 + 1);
                }
                p.add(mesh);
            }
            p.add(egui::Shape::Path(egui::epaint::PathShape {
                points,
                closed: true,
                fill: Color32::TRANSPARENT,
                stroke: s.into(),
            }));
        }
        Icon::Clock | Icon::Refresh => {
            p.circle_stroke(center, size * 0.36, s);
            if matches!(icon, Icon::Clock) {
                line((0.5, 0.25), (0.5, 0.5));
                line((0.5, 0.5), (0.7, 0.6));
            } else {
                line((0.72, 0.12), (0.88, 0.16));
                line((0.88, 0.16), (0.85, 0.34));
            }
        }
        Icon::Settings => {
            p.circle_stroke(center, size * 0.25, s);
            p.circle_stroke(center, size * 0.08, s);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let v = vec2(a.cos(), a.sin());
                p.line_segment([center + v * size * 0.26, center + v * size * 0.43], s);
            }
        }
        Icon::SelectAll | Icon::SelectNone | Icon::InvertSelection => {
            p.rect_stroke(
                Rect::from_min_max(q(0.12, 0.12), q(0.88, 0.88)),
                1,
                s,
                StrokeKind::Inside,
            );
            match icon {
                Icon::SelectAll => {
                    line((0.27, 0.49), (0.43, 0.65));
                    line((0.43, 0.65), (0.74, 0.34));
                }
                Icon::InvertSelection => {
                    p.rect_filled(Rect::from_min_max(q(0.5, 0.2), q(0.8, 0.8)), 0, color);
                    line((0.5, 0.2), (0.5, 0.8));
                }
                _ => {}
            }
        }
        Icon::Check => {
            line((0.15, 0.5), (0.4, 0.75));
            line((0.4, 0.75), (0.88, 0.2));
        }
        Icon::Expand => {
            for (a, b) in [
                ((0.1, 0.4), (0.1, 0.1)),
                ((0.1, 0.1), (0.4, 0.1)),
                ((0.6, 0.9), (0.9, 0.9)),
                ((0.9, 0.9), (0.9, 0.6)),
            ] {
                line(a, b);
            }
            line((0.15, 0.15), (0.85, 0.85));
        }
        Icon::Trash => {
            line((0.15, 0.25), (0.85, 0.25));
            line((0.35, 0.12), (0.65, 0.12));
            line((0.25, 0.33), (0.3, 0.88));
            line((0.3, 0.88), (0.7, 0.88));
            line((0.7, 0.88), (0.75, 0.33));
        }
        Icon::Cut => {
            p.circle_stroke(q(0.25, 0.73), size * 0.13, s);
            p.circle_stroke(q(0.75, 0.73), size * 0.13, s);
            line((0.3, 0.6), (0.75, 0.12));
            line((0.7, 0.6), (0.25, 0.12));
        }
        Icon::Copy | Icon::Paste => {
            p.rect_stroke(
                Rect::from_min_max(q(0.12, 0.1), q(0.68, 0.73)),
                1,
                s,
                StrokeKind::Inside,
            );
            p.rect_stroke(
                Rect::from_min_max(q(0.32, 0.3), q(0.88, 0.93)),
                1,
                s,
                StrokeKind::Inside,
            );
        }
        Icon::Rename => {
            line((0.18, 0.82), (0.78, 0.18));
            line((0.18, 0.82), (0.42, 0.77));
            line((0.18, 0.82), (0.22, 0.6));
            line((0.18, 0.93), (0.87, 0.93));
        }
        Icon::Archive => {
            // A folded document and zipper pull stay distinct from split-pane
            // icons at menu size; all details scale with the requested glyph.
            p.add(egui::Shape::closed_line(
                vec![
                    q(0.2, 0.06),
                    q(0.6, 0.06),
                    q(0.82, 0.28),
                    q(0.82, 0.94),
                    q(0.2, 0.94),
                ],
                s,
            ));
            line((0.6, 0.06), (0.6, 0.28));
            line((0.6, 0.28), (0.82, 0.28));
            for (x, y) in [(0.4, 0.23), (0.5, 0.34), (0.4, 0.45), (0.5, 0.56)] {
                line((x, y), (x + 0.1, y));
            }
            p.rect_stroke(
                Rect::from_min_max(q(0.4, 0.67), q(0.6, 0.82)),
                1,
                s,
                StrokeKind::Inside,
            );
        }
        other => {
            p.rect_stroke(
                Rect::from_min_max(q(0.18, 0.08), q(0.82, 0.92)),
                1,
                s,
                StrokeKind::Inside,
            );
            match other {
                Icon::Image => {
                    p.circle_filled(q(0.38, 0.32), size * 0.07, color);
                    line((0.26, 0.75), (0.47, 0.5));
                    line((0.47, 0.5), (0.74, 0.78));
                }
                Icon::Code => {
                    line((0.42, 0.35), (0.3, 0.5));
                    line((0.3, 0.5), (0.42, 0.65));
                    line((0.58, 0.35), (0.7, 0.5));
                    line((0.7, 0.5), (0.58, 0.65));
                }
                Icon::Video => {
                    p.add(egui::Shape::convex_polygon(
                        vec![q(0.4, 0.32), q(0.7, 0.5), q(0.4, 0.68)],
                        color,
                        Stroke::NONE,
                    ));
                }
                Icon::Music => {
                    p.circle_filled(q(0.43, 0.67), size * 0.1, color);
                    line((0.52, 0.67), (0.52, 0.27));
                    line((0.52, 0.27), (0.7, 0.24));
                }
                _ => {
                    line((0.31, 0.35), (0.68, 0.35));
                    line((0.31, 0.51), (0.68, 0.51));
                    line((0.31, 0.67), (0.59, 0.67));
                }
            }
        }
    }
}

pub fn chevron(p: &egui::Painter, center: Pos2, openness: f32, color: Color32, size: f32) {
    if !p.is_visible() || p.opacity() == 0.0 || color == Color32::TRANSPARENT {
        return;
    }
    let rotation = egui::emath::Rot2::from_angle(openness * std::f32::consts::FRAC_PI_2);
    let points = [vec2(-0.2, -0.32), vec2(0.2, 0.0), vec2(-0.2, 0.32)]
        .map(|point| center + rotation * (point * size));
    p.add(egui::Shape::line(points.to_vec(), Stroke::new(1.2, color)));
}

pub fn icon_button(ui: &mut Ui, t: &Theme, i: Icon, label: &str, active: bool) -> Response {
    let control = t.metrics().control;
    let (r, response) = ui.allocate_exact_size(vec2(control, control), Sense::hover());
    let response = components::icon_button_at(ui, r, response.id.with("button"), t, i, active);
    components::label_control(&response, label);
    response.on_hover_text(label)
}

pub fn text(p: &egui::Painter, t: &Theme, at: Pos2, align: Align2, s: &str, color: Color32) {
    p.text(at, align, s, t.font(), color);
}

pub fn item_count(count: usize) -> String {
    format!("{count} item{}", if count == 1 { "" } else { "s" })
}

/// Retargetable exponential motion, driven by frame delta; no fixed per-frame steps.
pub fn approach(current: &mut f32, target: f32, dt: f32, duration: f32) -> bool {
    motion::MotionGains::new(dt, duration).step_pixels(
        current,
        target,
        motion::MotionProfile::Standard,
    )
}

/// Small control states use the same recovered response curve as layout motion.
pub fn animated_state(
    ctx: &egui::Context,
    id: egui::Id,
    active: bool,
    t: &Theme,
    profile: motion::MotionProfile,
) -> f32 {
    let frame = ctx.cumulative_frame_nr();
    let dt = ctx.input(|i| i.stable_dt);
    let (value, moving) = ctx.data_mut(|data| {
        let states =
            data.get_temp_mut_or_default::<ControlStates>(egui::Id::new("foldex-control-motion"));
        states.step(id, active, frame, dt, t.duration(), profile)
    });
    if moving {
        ctx.request_repaint();
    }
    value
}

#[derive(Clone)]
struct ControlStates {
    items: egui::IdMap<(f32, u64)>,
    frame: Option<u64>,
    duration: u32,
    gains: motion::MotionGains,
}
impl Default for ControlStates {
    fn default() -> Self {
        Self {
            items: Default::default(),
            frame: None,
            duration: 0,
            gains: motion::MotionGains::new(0.0, 0.0),
        }
    }
}
impl ControlStates {
    fn step(
        &mut self,
        id: egui::Id,
        active: bool,
        frame: u64,
        dt: f32,
        duration: f32,
        profile: motion::MotionProfile,
    ) -> (f32, bool) {
        if self.frame != Some(frame) {
            self.items
                .retain(|_, (_, seen)| frame.wrapping_sub(*seen) <= 2);
            self.frame = Some(frame);
            self.gains = motion::MotionGains::new(dt, duration);
            self.duration = duration.to_bits();
        } else if self.duration != duration.to_bits() {
            self.gains = motion::MotionGains::new(dt, duration);
            self.duration = duration.to_bits();
        }
        let target = if active { 1.0 } else { 0.0 };
        if self.items.len() >= 1024 && !self.items.contains_key(&id) {
            return (target, false);
        }
        let (value, seen) = self.items.entry(id).or_insert((0.0, frame.wrapping_sub(1)));
        let moving = if *seen != frame || self.gains.is_immediate() {
            self.gains.step_normalized(value, target, profile)
        } else {
            (*value - target).abs() > motion::NORMALIZED_SNAP
        };
        *seen = frame;
        (*value, moving)
    }
}

#[cfg(test)]
mod control_motion_tests {
    use super::*;
    #[test]
    fn navigation_churn_cannot_grow_control_states_and_stale_controls_retire() {
        let mut states = ControlStates::default();
        for frame in 0..50 {
            for control in 0..1500 {
                states.step(
                    egui::Id::new((frame, control)),
                    true,
                    frame,
                    1.0 / 60.0,
                    0.145,
                    motion::MotionProfile::Fast,
                );
            }
            assert!(states.items.len() <= 1024);
        }
        states.step(
            egui::Id::new("new-view"),
            false,
            100,
            1.0 / 60.0,
            0.145,
            motion::MotionProfile::Fast,
        );
        assert_eq!(states.items.len(), 1);
    }
    #[test]
    fn control_response_advances_once_per_frame_and_eventually_sleeps() {
        let mut states = ControlStates::default();
        let id = egui::Id::new("control");
        let first = states
            .step(id, true, 0, 1.0 / 60.0, 0.145, motion::MotionProfile::Fast)
            .0;
        assert_eq!(
            states
                .step(id, true, 0, 1.0 / 60.0, 0.145, motion::MotionProfile::Fast)
                .0,
            first
        );
        for frame in 1..40 {
            states.step(
                id,
                true,
                frame,
                1.0 / 60.0,
                0.145,
                motion::MotionProfile::Fast,
            );
        }
        assert_eq!(
            states.step(id, true, 40, 1.0 / 60.0, 0.145, motion::MotionProfile::Fast),
            (1.0, false)
        );
        assert_eq!(
            states.step(id, false, 40, 1.0 / 60.0, 0.0, motion::MotionProfile::Fast),
            (0.0, false)
        );
    }
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let units = ["KB", "MB", "GB", "TB", "PB"];
    let mut n = bytes as f64 / 1024.0;
    let mut i = 0;
    while n >= 1024.0 && i < 4 {
        n /= 1024.0;
        i += 1;
    }
    if n >= 100.0 {
        format!("{n:.0} {}", units[i])
    } else {
        format!("{n:.1} {}", units[i])
    }
}

pub fn file_icon(name: &str, is_dir: bool) -> Icon {
    if is_dir {
        return Icon::Folder;
    }
    let ext = name.rsplit('.').next().unwrap_or("");
    if ["png", "jpg", "jpeg", "webp", "gif", "bmp", "svg"]
        .iter()
        .any(|s| ext.eq_ignore_ascii_case(s))
    {
        Icon::Image
    } else if [
        "rs", "js", "ts", "tsx", "py", "json", "toml", "css", "html", "c", "cpp", "md",
    ]
    .iter()
    .any(|s| ext.eq_ignore_ascii_case(s))
    {
        Icon::Code
    } else if ["zip", "7z", "rar", "gz", "tar"]
        .iter()
        .any(|s| ext.eq_ignore_ascii_case(s))
    {
        Icon::Archive
    } else if ["mp3", "wav", "flac", "ogg"]
        .iter()
        .any(|s| ext.eq_ignore_ascii_case(s))
    {
        Icon::Music
    } else if ["mp4", "mkv", "mov", "webm"]
        .iter()
        .any(|s| ext.eq_ignore_ascii_case(s))
    {
        Icon::Video
    } else {
        Icon::File
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn theme_roundtrip() {
        let t = Theme {
            inspector_font_size: 18.5,
            ..Theme::default()
        };
        let s = t.write().unwrap();
        assert_eq!(Theme::read(&s).unwrap().accent, t.accent);
        assert_eq!(Theme::read(&s).unwrap().inspector_font_size, 18.5);
        assert_eq!(Theme::read("").unwrap().inspector_font_size, 40.0 / 3.0);
    }
    #[test]
    fn invalid_theme_dimensions_are_rejected() {
        for text in [
            "row_height = nan",
            "font_size = -1",
            "inspector_font_size = nan",
            "inspector_font_size = 7.9",
            "inspector_font_size = 28.1",
            "radius = 255",
            "motion_ms = inf",
        ] {
            assert!(Theme::read(text).is_err(), "{text}");
        }
    }
    #[test]
    fn motion_is_refresh_rate_independent() {
        let (mut a, mut b) = (0.0, 0.0);
        for _ in 0..12 {
            approach(&mut a, 100.0, 1.0 / 60.0, 0.15);
        }
        for _ in 0..24 {
            approach(&mut b, 100.0, 1.0 / 120.0, 0.15);
        }
        assert!((a - b).abs() < 0.01);
    }
}
