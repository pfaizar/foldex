use super::*;
mod sidebar_ui;
use foldex_ui::motion::{LayoutMotion, MotionGains, MotionProfile};

const TITLEBAR_CONTROLS_WIDTH: f32 = 74.0;
const WINDOW_CONTROLS_WIDTH: f32 = 138.0;

pub(super) struct TitlebarGeometry {
    controls: Rect,
    menu: Rect,
    sidebar: Rect,
    caption_left: f32,
}
impl TitlebarGeometry {
    pub(super) fn new(rect: Rect, sidebar_width: f32) -> Self {
        let caption_left = (rect.right() - WINDOW_CONTROLS_WIDTH).max(rect.left());
        let width = sidebar_width
            .max(TITLEBAR_CONTROLS_WIDTH)
            .min(caption_left - rect.left());
        let controls = Rect::from_min_size(rect.min, vec2(width, rect.height()));
        let compact = (width / TITLEBAR_CONTROLS_WIDTH).min(1.0);
        let height = 26.0_f32.min(rect.height());
        Self {
            controls,
            menu: Rect::from_center_size(
                pos2(rect.left() + 20.5 * compact, rect.center().y),
                vec2(33.0 * compact, height),
            ),
            sidebar: Rect::from_center_size(
                pos2(controls.right() - 18.0 * compact, rect.center().y),
                vec2(28.0 * compact, height),
            ),
            caption_left,
        }
    }

    pub(super) fn workspace_header(&self, mut header: Rect) -> Rect {
        if header.top() < self.controls.bottom() && header.bottom() > self.controls.top() {
            header.max.x = header.right().min(self.caption_left).max(header.left());
            header.min.x = header.left().max(self.controls.right()).min(header.right());
        }
        header
    }

    pub(super) fn workspace_divider(&self, mut divider: Rect) -> Rect {
        if divider.top() < self.controls.bottom()
            && (divider.left() < self.controls.right() || divider.right() > self.caption_left)
        {
            divider.min.y = self.controls.bottom().min(divider.bottom());
        }
        divider
    }
}

#[derive(Default)]
pub(super) struct MainMenu {
    state: foldex_ui::components::MenuState,
}
fn main_menu_entries(
    layouts: bool,
) -> [(
    crate::commands::Command,
    &'static str,
    &'static str,
    Icon,
    bool,
); 9] {
    use crate::commands::Command;
    [
        (Command::GoTo, "GoTo", "Ctrl+P", Icon::Search, true),
        (
            Command::Commands,
            "Commands",
            "Ctrl+Shift+P",
            Icon::Commands,
            true,
        ),
        (Command::Launcher, "Launcher", "", Icon::Window, true),
        (Command::Operations, "File operations", "", Icon::Copy, true),
        (Command::LoadLayout, "Load layout", "", Icon::Split, layouts),
        (Command::SaveLayout, "Save layout", "", Icon::Save, true),
        (Command::Options, "Options", "Ctrl+,", Icon::Settings, true),
        (Command::Hotkeys, "Hotkeys", "", Icon::Keyboard, true),
        (
            Command::KeyboardHelp,
            "Keyboard navigation",
            "F1",
            Icon::Help,
            true,
        ),
    ]
}
const MAIN_MENU_SECTIONS: [foldex_ui::components::MenuSection<'static>; 2] = [
    foldex_ui::components::MenuSection::separator(3),
    foldex_ui::components::MenuSection::separator(6),
];
impl MainMenu {
    pub(super) fn is_open(&self) -> bool {
        self.state.active()
    }
    fn open(&self) -> bool {
        self.state.open
    }
    fn toggle(&mut self, ctx: &egui::Context) {
        self.state.toggle(ctx);
    }
    pub(super) fn ui(
        &mut self,
        ctx: &egui::Context,
        t: &Theme,
        layouts: bool,
    ) -> Option<crate::commands::Command> {
        let entries = main_menu_entries(layouts);
        let items: [foldex_ui::components::MenuItem<'_>; 9] =
            std::array::from_fn(|i| foldex_ui::components::MenuItem {
                id: i as u32,
                label: entries[i].1,
                shortcut: entries[i].2,
                glyph: entries[i].3,
                checked: None,
                enabled: entries[i].4,
            });
        let scale = t.metrics().scale;
        let anchor = Rect::from_min_size(
            ctx.content_rect().min + vec2(6.0 * scale, 28.0),
            vec2(248.0 * scale, 0.0),
        );
        self.state
            .show_sections(
                ctx,
                Id::new("application-menu"),
                t,
                anchor,
                &items,
                &MAIN_MENU_SECTIONS,
            )
            .map(|i| entries[i as usize].0)
    }
}
pub(super) struct SidebarSections {
    collapsed: [bool; crate::sidebar::Section::COUNT],
    heights: [Option<f32>; crate::sidebar::Section::COUNT],
    arrows: [Option<f32>; crate::sidebar::Section::COUNT],
    motion: LayoutMotion,
    preferences: crate::sidebar::Preferences,
    menu: foldex_ui::components::MenuState,
    menu_target: Option<SidebarMenuTarget>,
    menu_anchor: Rect,
    benchmark_headers: [Option<Rect>; crate::sidebar::Section::COUNT],
    benchmark_background: Option<Rect>,
}
#[derive(Clone)]
enum SidebarMenuTarget {
    Background,
    Section(crate::sidebar::Section),
    Bookmark(PathBuf),
    Recent(PathBuf),
}
struct SidebarRow {
    glyph: Icon,
    label: String,
    path: PathBuf,
    cut_key: Option<crate::cut_state::PathKey>,
    place: Option<crate::sidebar::PlaceKind>,
    usage: Option<crate::sidebar::Usage>,
    network_connected: Option<bool>,
}
impl Default for SidebarSections {
    fn default() -> Self {
        Self {
            collapsed: std::array::from_fn(|i| i == crate::sidebar::Section::Network.index()),
            heights: [None; crate::sidebar::Section::COUNT],
            arrows: [None; crate::sidebar::Section::COUNT],
            motion: Default::default(),
            preferences: Default::default(),
            menu: Default::default(),
            menu_target: None,
            menu_anchor: Rect::ZERO,
            benchmark_headers: [None; crate::sidebar::Section::COUNT],
            benchmark_background: None,
        }
    }
}
impl SidebarSections {
    pub(super) fn from_bits(bits: u8) -> Self {
        Self {
            collapsed: std::array::from_fn(|i| {
                i >= crate::sidebar::Section::LEGACY_COUNT || bits & (1 << i) != 0
            }),
            ..Default::default()
        }
    }
    pub(super) fn from_preferences(bits: u8, mut preferences: crate::sidebar::Preferences) -> Self {
        preferences.normalize();
        let mut sections = Self::from_bits(bits);
        sections.collapsed[crate::sidebar::Section::Network.index()] =
            !preferences.network_expanded;
        Self {
            preferences,
            ..sections
        }
    }
    pub(super) fn preferences(&self) -> &crate::sidebar::Preferences {
        &self.preferences
    }
    pub(super) fn preferences_mut(&mut self) -> &mut crate::sidebar::Preferences {
        &mut self.preferences
    }
    pub(super) fn menu_active(&self) -> bool {
        self.menu.active()
    }
    fn network_active(&self, sidebar_visible: bool, explorer_visible: bool) -> bool {
        sidebar_visible
            && explorer_visible
            && self
                .preferences
                .sections
                .visible(crate::sidebar::Section::Network)
            && !self.collapsed[crate::sidebar::Section::Network.index()]
    }
    /// Available after a visible sidebar benchmark frame. These are clipped
    /// current category header rectangles, suitable for normal secondary input.
    pub(crate) fn benchmark_header_rect(&self, section: crate::sidebar::Section) -> Option<Rect> {
        self.benchmark_headers[section.index()]
    }
    /// A rendered, clipped empty region, excluding child controls and grips.
    pub(crate) fn benchmark_background_rect(&self) -> Option<Rect> {
        self.benchmark_background
    }
    pub(crate) fn benchmark_open_section_menu(
        &mut self,
        section: crate::sidebar::Section,
        ctx: &egui::Context,
        scale: f32,
    ) -> bool {
        let Some(rect) = self.benchmark_header_rect(section) else {
            return false;
        };
        self.open_menu(
            ctx,
            SidebarMenuTarget::Section(section),
            rect.center(),
            248.0 * scale,
        );
        true
    }
    fn open_menu(
        &mut self,
        ctx: &egui::Context,
        target: SidebarMenuTarget,
        pointer: egui::Pos2,
        width: f32,
    ) {
        self.menu_target = Some(target);
        self.menu_anchor = Rect::from_min_size(pointer, vec2(width, 0.0));
        self.menu.open = false;
        self.menu.toggle(ctx);
    }
    pub(super) fn bits(&self) -> u8 {
        self.collapsed
            .iter()
            .take(crate::sidebar::Section::LEGACY_COUNT)
            .enumerate()
            .fold(0, |bits, (i, closed)| bits | (u8::from(*closed) << i))
    }
}

impl Foldex {
    /// Open an already rendered menu for the owned GPU capture harness. Uses
    /// the real trigger/anchor and normal popup focus; no synthetic OS input.
    /// Returns false until that trigger was visible in a previous pass.
    pub(super) fn benchmark_open_menu(&mut self, ctx: &egui::Context, menu: &str) -> bool {
        match menu {
            "burger" => {
                let trigger = Id::new("main-menu-button");
                if !ctx
                    .read_response(trigger)
                    .is_some_and(|response| response.rect.is_positive())
                {
                    return false;
                }
                ctx.memory_mut(|memory| memory.request_focus(trigger));
                self.main_menu.state.close();
                self.main_menu.toggle(ctx);
                true
            }
            "pane-more" => self.pane_mut().benchmark_open_more_menu(ctx),
            "places" => self.benchmark_open_places_menu(ctx),
            _ => false,
        }
    }

    pub(super) fn titlebar(&mut self, ui: &mut Ui, rect: Rect) {
        let t = &self.theme;
        let geometry = TitlebarGeometry::new(rect, self.sidebar_width);
        ui.painter()
            .rect_filled(geometry.controls, 0, Theme::color(t.chrome));
        let drag = ui.interact(rect, Id::new("title-drag"), Sense::CLICK | Sense::DRAG);
        if drag.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(
                !ui.input(|i| i.viewport().maximized.unwrap_or(false)),
            ));
        }
        let menu_rect = geometry.menu;
        let menu =
            foldex_ui::components::button_response(ui, menu_rect, Id::new("main-menu-button"));
        foldex_ui::components::label_control(&menu, "Application menu");
        crate::keyboard::register(ui, crate::keyboard::Region::Tabs, menu.id, menu_rect);
        foldex_ui::components::focus_ring(ui, menu_rect, menu.id, t, menu.has_focus());
        let k = foldex_ui::animated_state(
            ui.ctx(),
            menu.id.with("hover"),
            menu.hovered() || self.main_menu.open(),
            t,
            MotionProfile::Fast,
        );
        ui.painter()
            .rect_filled(menu_rect, t.radius, Theme::color(t.hover).gamma_multiply(k));
        icon(
            ui.painter(),
            menu_rect.center(),
            Icon::Menu,
            if self.main_menu.open() {
                t.fg()
            } else {
                t.dim()
            },
            16.0,
        );
        if menu.clicked() {
            self.main_menu.toggle(ui.ctx());
        }
        menu.on_hover_text("Menu");
        let r = geometry.sidebar;
        let response = foldex_ui::components::button_response(ui, r, Id::new("sidebar-toggle"));
        foldex_ui::components::label_control(&response, "Toggle sidebar");
        crate::keyboard::register(ui, crate::keyboard::Region::Tabs, response.id, r);
        foldex_ui::components::focus_ring(ui, r, response.id, t, response.has_focus());
        let k = foldex_ui::animated_state(
            ui.ctx(),
            response.id.with("hover"),
            response.hovered(),
            t,
            MotionProfile::Fast,
        );
        ui.painter()
            .rect_filled(r, t.radius, Theme::color(t.hover).gamma_multiply(k));
        icon(ui.painter(), r.center(), Icon::Inspect, t.dim(), 16.0);
        if response.clicked() {
            self.sidebar = !self.sidebar;
        }
        response.on_hover_text("Toggle sidebar  ·  Ctrl+B");
    }

    pub(super) fn window_controls(&mut self, ui: &mut Ui, rect: Rect) {
        let t = &self.theme;
        ui.painter().rect_filled(
            Rect::from_min_max(
                pos2(rect.right() - WINDOW_CONTROLS_WIDTH, rect.top()),
                rect.max,
            ),
            0,
            Theme::color(t.chrome),
        );
        for n in 0..3 {
            let r = Rect::from_min_size(
                pos2(
                    rect.right() - WINDOW_CONTROLS_WIDTH + n as f32 * 46.0,
                    rect.top(),
                ),
                vec2(46.0, rect.height()),
            );
            let rr = foldex_ui::components::button_response(ui, r, Id::new(("window", n)));
            foldex_ui::components::label_control(
                &rr,
                [
                    "Minimize window",
                    "Maximize or restore window",
                    "Close window",
                ][n],
            );
            crate::keyboard::register(ui, crate::keyboard::Region::Tabs, rr.id, r);
            let hovered = rr.hovered()
                || self
                    .native_frame
                    .as_ref()
                    .is_some_and(|frame| frame.hover(n));
            let k = foldex_ui::animated_state(
                ui.ctx(),
                rr.id.with("hover"),
                hovered,
                t,
                MotionProfile::Fast,
            );
            ui.painter().rect_filled(
                r,
                0,
                Theme::color(if n == 2 { t.danger } else { t.hover })
                    .gamma_multiply(k * if n == 2 { 0.75 } else { 1.0 }),
            );
            foldex_ui::components::focus_ring(ui, r.shrink(2.0), rr.id, t, rr.has_focus());
            let fg = t
                .dim()
                .lerp_to_gamma(if n == 2 { egui::Color32::WHITE } else { t.fg() }, k);
            if n == 0 {
                ui.painter().line_segment(
                    [r.center() - vec2(5.0, 0.0), r.center() + vec2(5.0, 0.0)],
                    Stroke::new(1.0, fg),
                );
            } else if n == 1 {
                if ui.input(|input| input.viewport().maximized.unwrap_or(false)) {
                    let back = Rect::from_center_size(r.center() + vec2(1.5, -1.5), vec2(8.0, 8.0));
                    ui.painter()
                        .rect_stroke(back, 0, Stroke::new(1.0, fg), StrokeKind::Inside);
                    let front = back.translate(vec2(-3.0, 3.0));
                    ui.painter().rect_filled(front, 0, Theme::color(t.chrome));
                    ui.painter()
                        .rect_stroke(front, 0, Stroke::new(1.0, fg), StrokeKind::Inside);
                } else {
                    ui.painter().rect_stroke(
                        Rect::from_center_size(r.center(), vec2(9.0, 9.0)),
                        0,
                        Stroke::new(1.0, fg),
                        StrokeKind::Inside,
                    );
                }
            } else {
                icon(ui.painter(), r.center(), Icon::Close, fg, 13.0);
            }
            if rr.clicked() && self.native_frame.is_none() {
                if n == 2 {
                    if self.close_failed
                        || self
                            .close_requested
                            .is_some_and(|at| at.elapsed() > Duration::from_secs(2))
                    {
                        self.allow_close = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    } else {
                        self.close_requested.get_or_insert(Instant::now());
                        self.session_dirty = true;
                        self.save_due = Some(Instant::now());
                    }
                } else {
                    ui.ctx().send_viewport_cmd(if n == 0 {
                        egui::ViewportCommand::Minimized(true)
                    } else {
                        egui::ViewportCommand::Maximized(
                            !ui.input(|i| i.viewport().maximized.unwrap_or(false)),
                        )
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_menu_groups_navigation_layouts_and_configuration() {
        use crate::commands::Command;
        let entries = main_menu_entries(false);
        assert_eq!(entries[3].0, Command::Operations);
        assert!(matches!(entries[2].0, Command::Launcher));
        assert!(matches!(entries[4].0, Command::LoadLayout));
        assert!(matches!(entries[5].0, Command::SaveLayout));
        assert!(matches!(entries[6].0, Command::Options));
        assert!(!entries[4].4);
        assert!(main_menu_entries(true)[4].4);
        assert!(matches!(entries[1].3, Icon::Commands));
        assert!(matches!(entries[5].3, Icon::Save));
        assert!(matches!(entries[7].3, Icon::Keyboard));
        assert!(matches!(entries[8].3, Icon::Help));
        assert_eq!(MAIN_MENU_SECTIONS.map(|section| section.before), [3, 6]);
        assert!(
            MAIN_MENU_SECTIONS
                .iter()
                .all(|section| section.label.is_empty())
        );
    }

    #[test]
    fn reordered_sections_keep_legacy_collapse_bits_attached_to_categories() {
        use crate::sidebar::{Preferences, Section};
        let mut sections = SidebarSections::from_preferences(0b1010, Preferences::default());
        assert!(sections.collapsed[Section::Bookmarks.index()]);
        assert!(sections.collapsed[Section::Places.index()]);
        assert!(sections.preferences.move_by(Section::Places, -1));
        assert!(sections.preferences.move_by(Section::Recents, -1));
        assert_eq!(sections.bits(), 0b1010);
        let restored =
            SidebarSections::from_preferences(sections.bits(), sections.preferences.clone());
        assert_eq!(restored.preferences.order, sections.preferences.order);
        assert_eq!(restored.collapsed, sections.collapsed);
    }

    #[test]
    fn sidebar_preferences_round_trip_with_old_and_new_workspace_sessions() {
        use crate::sidebar::Section;
        let mut session: Session =
            serde_json::from_str(r#"{"sidebar_collapsed":10,"bookmarks":[]}"#).unwrap();
        assert_eq!(session.sidebar_preferences.order, Section::DEFAULT_ORDER);
        session.sidebar_preferences.order = vec![
            Section::Storage,
            Section::Places,
            Section::Recents,
            Section::Bookmarks,
        ];
        session.sidebar_preferences.places.music = true;
        session.sidebar_preferences.storage_usage = false;
        session.sidebar_preferences.sections.bookmarks = false;
        session.sidebar_preferences.sections.network = false;
        let restored: Session =
            serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
        assert_eq!(restored.sidebar_preferences, session.sidebar_preferences);
        assert_eq!(restored.sidebar_collapsed, 10);
    }
    #[test]
    fn hiding_sections_retains_collapse_order_and_network_expansion_but_disables_discovery() {
        use crate::sidebar::{Preferences, Section};
        let mut preferences = Preferences::default();
        preferences.network_expanded = true;
        assert!(preferences.move_by(Section::Network, -3));
        let mut sections = SidebarSections::from_preferences(0b1010, preferences);
        assert!(sections.network_active(true, true));
        assert!(!sections.network_active(false, true));
        assert!(!sections.network_active(true, false));
        let order = sections.preferences.order.clone();
        let collapsed = sections.collapsed;
        for section in Section::DEFAULT_ORDER {
            sections.preferences.sections.toggle(section);
        }
        assert!(!sections.network_active(true, true));
        assert_eq!(sections.preferences.order, order);
        assert_eq!(sections.collapsed, collapsed);
        assert!(sections.preferences.network_expanded);
        assert_eq!(sections.bits(), 0b1010);
        let mut restored =
            SidebarSections::from_preferences(sections.bits(), sections.preferences.clone());
        assert_eq!(restored.collapsed, collapsed);
        assert_eq!(restored.preferences, sections.preferences);
        restored.preferences.sections.toggle(Section::Network);
        assert!(restored.network_active(true, true));
        restored.collapsed[Section::Network.index()] = true;
        assert!(!restored.network_active(true, true));
    }
    #[test]
    fn network_expansion_is_separate_from_legacy_collapse_bits_and_survives_restore() {
        use crate::sidebar::{Preferences, Section};
        let old: Preferences =
            serde_json::from_str(r#"{"order":["places","recents","bookmarks","storage"]}"#)
                .unwrap();
        let old = SidebarSections::from_preferences(0b0101, old);
        assert!(old.collapsed[Section::Network.index()]);
        assert_eq!(old.bits(), 0b0101);
        assert_eq!(old.preferences.order.last(), Some(&Section::Network));
        let mut preferences = old.preferences.clone();
        preferences.network_expanded = true;
        assert!(preferences.move_by(Section::Network, -4));
        let restored = SidebarSections::from_preferences(old.bits(), preferences.clone());
        assert!(!restored.collapsed[Section::Network.index()]);
        assert_eq!(restored.bits(), old.bits());
        assert_eq!(restored.preferences, preferences);
    }
    #[test]
    fn titlebar_keeps_both_controls_clear_of_tabs_through_sidebar_animation() {
        for window_width in [420.0, 720.0, 1400.0] {
            let window = Rect::from_min_size(pos2(30.0, 50.0), vec2(window_width, 32.0));
            for sidebar_width in [0.0, 0.25, 1.0, 37.0, 73.0, 74.0, 99.0, 100.0, 160.0, 240.0] {
                let geometry = TitlebarGeometry::new(window, sidebar_width);
                assert!(geometry.controls.contains_rect(geometry.menu));
                assert!(geometry.controls.contains_rect(geometry.sidebar));
                assert!(geometry.menu.right() < geometry.sidebar.left());
                assert_eq!(geometry.menu.width(), 33.0);
                assert_eq!(geometry.sidebar.width(), 28.0);
                let header = geometry.workspace_header(Rect::from_min_max(
                    pos2(window.left() + sidebar_width, window.top()),
                    window.max,
                ));
                assert!(header.left() >= geometry.sidebar.right());
                assert!(header.right() <= geometry.caption_left);
                assert!(header.min.is_finite() && header.max.is_finite());
                assert!(header.width() >= 0.0);
                assert_eq!(header.left(), window.left() + sidebar_width.max(74.0));
            }
        }
    }

    #[test]
    fn titlebar_reservation_clips_top_split_headers_and_leaves_lower_groups_unchanged() {
        let window = Rect::from_min_size(pos2(30.0, 50.0), vec2(420.0, 32.0));
        let geometry = TitlebarGeometry::new(window, 0.0);
        let hidden = geometry.workspace_header(Rect::from_min_size(window.min, vec2(40.0, 32.0)));
        assert_eq!(hidden.width(), 0.0);
        let next = geometry.workspace_header(Rect::from_min_size(
            window.min + vec2(44.0, 0.0),
            vec2(140.0, 32.0),
        ));
        assert_eq!(next.left(), geometry.controls.right());
        assert_eq!(next.right(), window.left() + 184.0);
        let animating = geometry.workspace_header(window.translate(vec2(0.0, 12.0)));
        assert_eq!(animating.left(), geometry.controls.right());
        assert_eq!(animating.right(), geometry.caption_left);
        let caption = geometry.workspace_header(Rect::from_min_size(
            pos2(geometry.caption_left + 4.0, window.top()),
            vec2(80.0, 32.0),
        ));
        assert_eq!(caption.width(), 0.0);
        let lower = window.translate(vec2(0.0, 180.0));
        assert_eq!(geometry.workspace_header(lower), lower);
        let divider = Rect::from_min_size(window.min + vec2(40.0, 0.0), vec2(4.0, 300.0));
        let clipped = geometry.workspace_divider(divider);
        assert_eq!(clipped.top(), window.bottom());
        assert_eq!(clipped.bottom(), divider.bottom());
        assert!(!clipped.intersects(geometry.sidebar));
        let unobstructed = divider.translate(vec2(74.0, 0.0));
        assert_eq!(geometry.workspace_divider(unobstructed), unobstructed);
    }
}
