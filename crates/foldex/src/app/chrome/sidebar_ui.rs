use super::*;
use crate::{
    assets::{AssetStamp, ViewportSignature, VisibleAsset},
    sidebar::{ICON_VIEW, PlaceKind, Section, network::UserAction as NetworkAction},
};
use foldex_ui::components::{MenuItem, MenuSection};
use std::{collections::VecDeque, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
enum NavigationItem {
    Home,
    Section(Section),
    Folder(Section, PathBuf),
    NetworkAction(NetworkAction),
}

#[derive(Clone, Default)]
struct SidebarKeyboard {
    focused: Option<NavigationItem>,
    background: bool,
    reveal: bool,
    was_focused: bool,
    composing: bool,
    prefix: String,
    prefix_until: f64,
}

/// Registered before children, so they retain pointer ownership. The explicit
/// exclusions also protect hover-only status text and clipped/animated controls.
struct SidebarBackground {
    bounds: Rect,
    controls: Vec<Rect>,
}
impl SidebarBackground {
    fn new(bounds: Rect) -> Self {
        Self {
            bounds,
            controls: Vec::with_capacity(16),
        }
    }
    fn exclude(&mut self, rect: Rect) {
        let clipped = rect.intersect(self.bounds);
        if clipped.is_positive() {
            self.controls.push(clipped);
        }
    }
    fn contains(&self, point: egui::Pos2) -> bool {
        self.bounds.contains(point) && !self.controls.iter().any(|rect| rect.contains(point))
    }
    fn clicked(&self, response: &egui::Response) -> Option<(egui::Pos2, bool)> {
        let secondary = response.secondary_clicked();
        if !response.clicked() && !secondary {
            return None;
        }
        let point = response.interact_pointer_pos()?;
        self.contains(point).then_some((point, secondary))
    }
    /// Capture harness only: choose an actual empty region after all children
    /// and the scrollbar have been rendered. No synthetic input is generated.
    fn empty_rect(&self) -> Option<Rect> {
        if self.bounds.width() <= 1.0 || self.bounds.height() <= 1.0 {
            return None;
        }
        let mut empty = vec![self.bounds];
        for &control in &self.controls {
            let mut next = Vec::with_capacity(empty.len() + 3);
            for area in empty {
                let overlap = area.intersect(control);
                if !overlap.is_positive() {
                    next.push(area);
                    continue;
                }
                next.extend(
                    [
                        Rect::from_min_max(area.min, pos2(area.right(), overlap.top())),
                        Rect::from_min_max(pos2(area.left(), overlap.bottom()), area.max),
                        Rect::from_min_max(
                            pos2(area.left(), overlap.top()),
                            pos2(overlap.left(), overlap.bottom()),
                        ),
                        Rect::from_min_max(
                            pos2(overlap.right(), overlap.top()),
                            pos2(area.right(), overlap.bottom()),
                        ),
                    ]
                    .into_iter()
                    .filter(|rect| rect.width() > 1.0 && rect.height() > 1.0),
                );
            }
            empty = next;
        }
        empty.into_iter().max_by(|left, right| {
            (left.width() * left.height()).total_cmp(&(right.width() * right.height()))
        })
    }
}

fn sidebar_restore_focus(
    state: &mut SidebarKeyboard,
    visible: &[(NavigationItem, String)],
    current: &std::path::Path,
    home: bool,
) {
    if state.background && state.focused.is_none() {
        return;
    }
    if !visible
        .iter()
        .any(|(item, _)| Some(item) == state.focused.as_ref())
    {
        let parent = match state.focused.as_ref() {
            Some(NavigationItem::Folder(section, _)) => Some(NavigationItem::Section(*section)),
            Some(NavigationItem::NetworkAction(_)) => {
                Some(NavigationItem::Section(Section::Network))
            }
            _ => None,
        };
        state.focused = parent
            .filter(|parent| visible.iter().any(|(item, _)| item == parent))
            .or_else(|| {
                visible
                    .iter()
                    .find(|(item, _)| match item {
                        NavigationItem::Home => home,
                        NavigationItem::Folder(_, path) => {
                            !home && crate::bookmarks::path_eq(path, current)
                        }
                        NavigationItem::Section(_) | NavigationItem::NetworkAction(_) => false,
                    })
                    .map(|(item, _)| item.clone())
            })
            .or(Some(NavigationItem::Home));
        state.reveal = true;
    }
    state.background = false;
}

fn sidebar_step(current: usize, length: usize, key: Key, page: usize) -> usize {
    match key {
        Key::Home => 0,
        Key::End => length.saturating_sub(1),
        Key::ArrowUp => current.saturating_sub(1),
        Key::ArrowDown => current.saturating_add(1).min(length.saturating_sub(1)),
        Key::PageUp => current.saturating_sub(page.max(1)),
        Key::PageDown => current
            .saturating_add(page.max(1))
            .min(length.saturating_sub(1)),
        _ => current,
    }
}

fn sidebar_focus(
    ui: &Ui,
    rect: Rect,
    item: &NavigationItem,
    state: &mut SidebarKeyboard,
    t: &Theme,
) {
    let id = Id::new("sidebar-navigation-focus");
    let focused = state.focused.as_ref() == Some(item);
    if focused && state.reveal {
        ui.scroll_to_rect_animation(
            rect,
            None,
            egui::style::ScrollAnimation::duration(t.duration()),
        );
        if rect.intersect(ui.clip_rect()).height() > 1.0 {
            state.reveal = false;
        }
    }
    if focused && ui.memory(|memory| memory.has_focus(id)) {
        foldex_ui::components::focus_ring(ui, rect, id.with("item-ring"), t, true);
    }
}

/// Sidebar Escape returns to navigation without clearing the query. Keep the
/// input and clear affordance shared with search fields, including their slot.
fn sidebar_filter(ui: &mut Ui, rect: Rect, id: Id, t: &Theme, value: &mut String, composing: bool) {
    let m = t.metrics();
    let visible = !value.is_empty() && rect.width() >= 63.0 * m.scale;
    let reveal = foldex_ui::animated_state(
        ui.ctx(),
        id.with("clear-reveal"),
        visible,
        t,
        MotionProfile::Fast,
    );
    let field = Rect::from_min_max(
        rect.min,
        pos2(
            (rect.right() - m.control * reveal).max(rect.left()),
            rect.bottom(),
        ),
    );
    if reveal > 0.01 {
        let button = Rect::from_center_size(
            pos2(rect.right() - m.control * 0.5, rect.center().y),
            vec2(m.control, m.control),
        );
        let mut action_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(id.with("clear-surface"))
                .max_rect(button),
        );
        action_ui.set_clip_rect(
            Rect::from_min_max(pos2(field.right(), rect.top()), rect.max).intersect(ui.clip_rect()),
        );
        action_ui.multiply_opacity(reveal);
        if !visible {
            foldex_ui::components::disable_interaction(&mut action_ui);
        }
        let clear = foldex_ui::components::icon_button_at(
            &action_ui,
            button,
            id.with("clear"),
            t,
            Icon::Close,
            false,
        )
        .on_hover_text("Clear sidebar filter");
        clear.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, visible, "Clear sidebar filter")
        });
        crate::keyboard::register(
            &action_ui,
            crate::keyboard::Region::Sidebar,
            clear.id,
            button,
        );
        if clear.clicked() {
            value.clear();
            crate::keyboard::request_id(ui.ctx(), id);
        }
    }
    let response =
        foldex_ui::components::search_input(ui, field, id, t, value, "Filter…", 256).response;
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                id,
                egui::EventFilter {
                    tab: composing,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            )
        });
    }
    if response.changed() {
        ui.ctx().request_repaint();
    }
}

struct RowIcon {
    painter: egui::Painter,
    rect: Rect,
    key: foldex_assets::AssetKey,
    fallback: Icon,
    tint: egui::Color32,
}

/// Sources are bounded (10 recents, 32 bookmarks, 26 volumes, seven places,
/// 64 network resources). Network rows are cached without probing share paths.
/// Compare saved paths without allocating; only rebuild labels, matching and
/// keyboard identities when one of those sources or the user's filter changes.
struct SidebarRows {
    revision: u64,
    network_revision: u64,
    places: crate::sidebar::PlacesVisibility,
    sections: crate::sidebar::SectionVisibility,
    order: Vec<Section>,
    collapsed: [bool; Section::COUNT],
    filter: String,
    groups: [Vec<SidebarRow>; Section::COUNT],
    filtered: [Vec<usize>; Section::COUNT],
    visible: Arc<Vec<(NavigationItem, String)>>,
}
impl SidebarRows {
    fn matches(
        &self,
        recent: &VecDeque<PathBuf>,
        bookmarks: &[PathBuf],
        data: &crate::sidebar::Data,
        preferences: &crate::sidebar::Preferences,
        collapsed: &[bool; Section::COUNT],
        filter: &str,
    ) -> bool {
        self.revision == data.revision()
            && self.network_revision == data.network.revision()
            && self.places == preferences.places
            && self.sections == preferences.sections
            && self.order == preferences.order
            && self.collapsed == *collapsed
            && self.filter == filter
            && self.groups[Section::Recents.index()]
                .iter()
                .map(|row| &row.path)
                .eq(recent.iter().take(10))
            && self.groups[Section::Bookmarks.index()]
                .iter()
                .map(|row| &row.path)
                .eq(bookmarks.iter())
    }
    fn new(
        recent: &VecDeque<PathBuf>,
        bookmarks: &[PathBuf],
        data: &crate::sidebar::Data,
        preferences: &crate::sidebar::Preferences,
        collapsed: [bool; Section::COUNT],
        filter: &str,
    ) -> Self {
        let folder_row = |path: &PathBuf| SidebarRow {
            glyph: Icon::Folder,
            label: path_label(path),
            path: path.clone(),
            cut_key: crate::cut_state::PathKey::new(path),
            place: None,
            usage: None,
            network_connected: None,
        };
        let groups = [
            recent.iter().take(10).map(folder_row).collect::<Vec<_>>(),
            bookmarks.iter().map(folder_row).collect(),
            data.volumes()
                .iter()
                .map(|volume| SidebarRow {
                    glyph: Icon::Drive,
                    label: volume.label.clone(),
                    path: volume.path.clone(),
                    cut_key: crate::cut_state::PathKey::new(&volume.path),
                    place: None,
                    usage: volume.usage,
                    network_connected: None,
                })
                .collect(),
            data.places()
                .iter()
                .cloned()
                .chain(std::iter::once(crate::sidebar::Place {
                    kind: PlaceKind::RecycleBin,
                    path: crate::navigation::RECYCLE_BIN_ADDRESS.into(),
                }))
                .filter(|place| preferences.places.visible(place.kind))
                .map(|place| SidebarRow {
                    glyph: if place.kind == PlaceKind::User {
                        Icon::Home
                    } else if place.kind == PlaceKind::RecycleBin {
                        Icon::Trash
                    } else {
                        Icon::Folder
                    },
                    label: place.kind.label().to_owned(),
                    path: place.path.clone(),
                    cut_key: (place.kind != PlaceKind::RecycleBin)
                        .then(|| crate::cut_state::PathKey::new(&place.path))
                        .flatten(),
                    place: Some(place.kind),
                    usage: None,
                    network_connected: None,
                })
                .collect(),
            data.network
                .resources()
                .iter()
                .map(|resource| SidebarRow {
                    glyph: Icon::Drive,
                    label: if resource.connected {
                        resource.label.clone()
                    } else {
                        format!("{} · Saved", resource.label)
                    },
                    path: resource.path.clone(),
                    cut_key: crate::cut_state::PathKey::new(&resource.path),
                    place: None,
                    usage: None,
                    network_connected: Some(resource.connected),
                })
                .collect(),
        ];
        let matcher = foldex_match::Matcher::new(filter);
        let filtered = std::array::from_fn(|section| {
            groups[section]
                .iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    (filter.is_empty() || matcher.score(&row.label).is_some()).then_some(index)
                })
                .collect()
        });
        let mut rows = Self {
            revision: data.revision(),
            network_revision: data.network.revision(),
            places: preferences.places.clone(),
            sections: preferences.sections.clone(),
            order: preferences.order.clone(),
            collapsed,
            filter: filter.to_owned(),
            groups,
            filtered,
            visible: Arc::default(),
        };
        rows.visible = rows.navigation(&collapsed);
        rows
    }
    fn navigation(&self, collapsed: &[bool; Section::COUNT]) -> Arc<Vec<(NavigationItem, String)>> {
        let mut visible =
            Vec::with_capacity(10 + self.filtered.iter().map(Vec::len).sum::<usize>());
        visible.push((NavigationItem::Home, "Home".to_owned()));
        for &section in &self.order {
            if !self.sections.visible(section) {
                continue;
            }
            visible.push((NavigationItem::Section(section), section.label().to_owned()));
            if !collapsed[section.index()] {
                visible.extend(self.filtered[section.index()].iter().map(|&index| {
                    let row = &self.groups[section.index()][index];
                    (
                        NavigationItem::Folder(section, row.path.clone()),
                        row.label.clone(),
                    )
                }));
                if section == Section::Network {
                    visible.extend(NetworkAction::ALL.map(|action| {
                        (
                            NavigationItem::NetworkAction(action),
                            action.label().to_owned(),
                        )
                    }));
                }
            }
        }
        Arc::new(visible)
    }
}

struct SidebarSelection {
    rows: Arc<SidebarRows>,
    current: PathBuf,
    home: bool,
    selected: [Vec<bool>; Section::COUNT],
}
impl SidebarSelection {
    fn get(
        ctx: &egui::Context,
        rows: &Arc<SidebarRows>,
        current: &std::path::Path,
        home: bool,
    ) -> Arc<Self> {
        let id = Id::new("sidebar-current-selection");
        if let Some(cached) = ctx.data(|data| data.get_temp::<Arc<Self>>(id))
            && Arc::ptr_eq(&cached.rows, rows)
            && cached.home == home
            && (home || cached.current == current)
        {
            return cached;
        }
        let selection = Arc::new(Self {
            rows: rows.clone(),
            current: current.to_path_buf(),
            home,
            selected: std::array::from_fn(|section| {
                rows.groups[section]
                    .iter()
                    .map(|row| !home && crate::bookmarks::path_eq(&row.path, current))
                    .collect()
            }),
        });
        ctx.data_mut(|data| data.insert_temp(id, selection.clone()));
        selection
    }
}

impl Foldex {
    pub(in crate::app) fn benchmark_open_places_menu(&mut self, ctx: &egui::Context) -> bool {
        if self
            .sidebar_sections
            .benchmark_header_rect(Section::Places)
            .is_none()
        {
            return false;
        }
        let focus = crate::keyboard::sidebar_id();
        ctx.data_mut(|data| {
            data.get_temp_mut_or_default::<SidebarKeyboard>(focus)
                .focused = Some(NavigationItem::Section(Section::Places));
        });
        crate::keyboard::request_id(ctx, focus);
        self.sidebar_sections.benchmark_open_section_menu(
            Section::Places,
            ctx,
            self.theme.metrics().scale,
        )
    }

    /// Isolated screenshot harness only: show controlled network data without
    /// enumeration, dialogs or share access. Network is moved first only in this
    /// isolated capture state so the rows, status and actions remain visible.
    pub(in crate::app) fn benchmark_network_sidebar(
        &mut self,
        resources: &[(&str, Option<&str>, bool)],
        error: Option<&str>,
    ) {
        self.sidebar_data
            .network
            .benchmark_snapshot(resources, error);
        self.sidebar_sections.collapsed[Section::Network.index()] = false;
        self.sidebar_sections.preferences.network_expanded = true;
        while self
            .sidebar_sections
            .preferences
            .move_by(Section::Network, -1)
        {}
    }

    /// Opens only at a real empty region from the preceding rendered sidebar.
    /// The native capture owner can inspect preferences().sections independently.
    pub(in crate::app) fn benchmark_open_sidebar_background_menu(
        &mut self,
        ctx: &egui::Context,
    ) -> bool {
        if !self.sidebar || !self.explorer_visible {
            return false;
        }
        let Some(rect) = self.sidebar_sections.benchmark_background_rect() else {
            return false;
        };
        self.sidebar_sections.open_menu(
            ctx,
            SidebarMenuTarget::Background,
            rect.center(),
            248.0 * self.theme.metrics().scale,
        );
        true
    }

    /// Reapply this gate immediately after Options updates sidebar settings.
    pub(in crate::app) fn service_sidebar_network(&mut self) {
        self.sidebar_data.network.service(
            self.sidebar_sections
                .network_active(self.sidebar, self.explorer_visible),
        );
    }

    pub(in crate::app) fn service_sidebar(&mut self, ctx: &egui::Context) {
        self.service_sidebar_network();
        if self
            .sidebar_data
            .service(ctx, self.sidebar && self.explorer_visible)
            || self.places.is_empty()
        {
            self.places = self
                .sidebar_data
                .places()
                .iter()
                .map(|place| {
                    (
                        if place.kind == PlaceKind::User {
                            Icon::Home
                        } else {
                            Icon::Folder
                        },
                        place.kind.label().to_owned(),
                        place.path.clone(),
                    )
                })
                .chain(std::iter::once((
                    Icon::Trash,
                    "Recycle Bin".into(),
                    PathBuf::from(crate::navigation::RECYCLE_BIN_ADDRESS),
                )))
                .chain(
                    self.sidebar_data
                        .volumes()
                        .iter()
                        .map(|volume| (Icon::Drive, volume.label.clone(), volume.path.clone())),
                )
                .collect();
        }
        if !self.sidebar || !self.explorer_visible {
            self.sidebar_sections.menu.close();
            self.sidebar_sections.benchmark_background = None;
        }
    }

    pub(in crate::app) fn sidebar_ui(&mut self, ui: &mut Ui, rect: Rect) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut network_expanded = false;
        let mut network_action = None;
        let benchmark = self.benchmark.is_some();
        if benchmark {
            self.sidebar_sections.benchmark_headers = [None; Section::COUNT];
            self.sidebar_sections.benchmark_background = None;
        }
        let t = &self.theme;
        let m = t.metrics();
        let scale = m.scale;
        let row_height = t.file_row_height();
        ui.painter().rect_filled(rect, 0, Theme::color(t.sidebar));
        ui.painter().line_segment(
            [rect.right_top(), rect.right_bottom()],
            Stroke::new(1.0, t.line()),
        );
        if rect.width() < 100.0 {
            self.sidebar_sections.menu.close();
            return actions;
        }
        let current = if self.pane().is_recycle_bin() {
            PathBuf::from(crate::navigation::RECYCLE_BIN_ADDRESS)
        } else {
            self.pane().path.clone()
        };
        let home_selected = self.pane().is_home();
        let mut home_clicked = false;
        let focus_id = crate::keyboard::sidebar_id();
        let filter_id = Id::new("sidebar-filter");
        let background_response = ui.interact(rect, Id::new("sidebar-background"), Sense::CLICK);
        let mut keyboard = ui
            .ctx()
            .data(|data| data.get_temp::<SidebarKeyboard>(focus_id))
            .unwrap_or_default();
        if ui.memory(|memory| memory.has_focus(focus_id))
            && Pane::consume_navigation_key(ui.ctx(), egui::Modifiers::SHIFT, Key::Tab)
        {
            crate::keyboard::request_id(ui.ctx(), filter_id);
        }
        let filter_owned = ui.memory(|memory| memory.has_focus(filter_id));
        if filter_owned {
            ui.input(|input| {
                for event in &input.events {
                    match event {
                        egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                            keyboard.composing = !text.is_empty();
                        }
                        egui::Event::Ime(egui::ImeEvent::Commit(_)) => keyboard.composing = false,
                        _ => {}
                    }
                }
            });
        } else {
            keyboard.composing = false;
        }
        if filter_owned && keyboard.composing {
            ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        let mut filter_transfer = false;
        if filter_owned && !keyboard.composing {
            let leave = [Key::ArrowDown, Key::Tab, Key::Escape]
                .into_iter()
                .any(|key| Pane::consume_navigation_key(ui.ctx(), egui::Modifiers::NONE, key));
            if leave {
                ui.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
                keyboard.reveal = true;
                filter_transfer = true;
            }
        }
        let filter_rect = Rect::from_min_max(
            pos2(rect.left() + m.inset, rect.top() + 2.0 * scale),
            pos2(rect.right() - m.inset, rect.top() + 30.0 * scale),
        );
        sidebar_filter(
            ui,
            filter_rect,
            filter_id,
            t,
            &mut self.side_filter,
            keyboard.composing,
        );
        if filter_transfer {
            crate::keyboard::request_id(ui.ctx(), focus_id);
        }
        crate::keyboard::register(ui, crate::keyboard::Region::Sidebar, filter_id, filter_rect);
        ui.painter().line_segment(
            [
                pos2(rect.left(), filter_rect.bottom()),
                pos2(rect.right(), filter_rect.bottom()),
            ],
            Stroke::new(1.0, t.line()),
        );
        let cache_id = Id::new("sidebar-derived-rows");
        let cached = ui
            .ctx()
            .data(|data| data.get_temp::<Arc<SidebarRows>>(cache_id));
        let rows = cached
            .filter(|rows| {
                rows.matches(
                    &self.recent,
                    &self.bookmarks,
                    &self.sidebar_data,
                    &self.sidebar_sections.preferences,
                    &self.sidebar_sections.collapsed,
                    &self.side_filter,
                )
            })
            .unwrap_or_else(|| {
                let rows = Arc::new(SidebarRows::new(
                    &self.recent,
                    &self.bookmarks,
                    &self.sidebar_data,
                    &self.sidebar_sections.preferences,
                    self.sidebar_sections.collapsed,
                    &self.side_filter,
                ));
                ui.ctx()
                    .data_mut(|data| data.insert_temp(cache_id, rows.clone()));
                rows
            });
        let groups = &rows.groups;
        let selection = SidebarSelection::get(ui.ctx(), &rows, &current, home_selected);
        let order = &rows.order;
        let show_usage = self.sidebar_sections.preferences.storage_usage;
        let content = Rect::from_min_max(
            pos2(rect.left() + m.inset, filter_rect.bottom() + 5.0 * scale),
            pos2(rect.right() - m.inset, rect.bottom()),
        );
        let mut background = SidebarBackground::new(
            Rect::from_min_max(
                pos2(rect.left(), filter_rect.bottom() + 1.0),
                pos2(rect.right() - 3.0, rect.bottom()),
            )
            .intersect(ui.clip_rect()),
        );
        crate::keyboard::composite(
            ui,
            crate::keyboard::Region::Sidebar,
            focus_id,
            content,
            "Sidebar navigation",
        );
        foldex_ui::components::register_focus_target(ui, content, focus_id);
        let mut visible = rows.visible.clone();
        if filter_transfer
            && !self.side_filter.is_empty()
            && !visible.iter().any(|(item, _)| {
                matches!(item, NavigationItem::Folder(_, _))
                    && Some(item) == keyboard.focused.as_ref()
            })
        {
            keyboard.focused = visible
                .iter()
                .find(|(item, _)| matches!(item, NavigationItem::Folder(_, _)))
                .map(|(item, _)| item.clone());
        }
        sidebar_restore_focus(&mut keyboard, &visible, &current, home_selected);
        let focused = ui.memory(|memory| memory.has_focus(focus_id));
        let mut activate = false;
        let mut context_menu = false;
        if focused && foldex_ui::components::can_focus(ui) && !self.sidebar_sections.menu.active() {
            keyboard.reveal |= !keyboard.was_focused;
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    focus_id,
                    egui::EventFilter {
                        tab: false,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            for _ in 0..32 {
                let Some(key) = Pane::take_composite_key(
                    ui.ctx(),
                    &[
                        Key::ArrowDown,
                        Key::ArrowUp,
                        Key::Home,
                        Key::End,
                        Key::PageDown,
                        Key::PageUp,
                        Key::ArrowLeft,
                        Key::ArrowRight,
                        Key::Enter,
                        Key::Space,
                        Key::F10,
                        Key::Escape,
                    ],
                ) else {
                    break;
                };
                if key == Key::Escape {
                    self.assets.cancel_cut();
                    break;
                }
                if matches!(key, Key::Enter | Key::Space) {
                    activate = true;
                    break;
                }
                if key == Key::F10 {
                    context_menu = true;
                    break;
                }
                let position = visible
                    .iter()
                    .position(|(item, _)| Some(item) == keyboard.focused.as_ref());
                let at = position.unwrap_or(0);
                if !matches!(key, Key::ArrowLeft | Key::ArrowRight) {
                    let page = (content.height() / row_height).floor().max(1.0) as usize;
                    let next = position.map_or_else(
                        || {
                            if matches!(key, Key::ArrowUp | Key::PageUp | Key::End) {
                                visible.len().saturating_sub(1)
                            } else {
                                0
                            }
                        },
                        |at| sidebar_step(at, visible.len(), key, page),
                    );
                    keyboard.focused = Some(visible[next].0.clone());
                    keyboard.reveal = true;
                    keyboard.prefix.clear();
                    continue;
                }
                let mut changed = false;
                match keyboard.focused.clone() {
                    Some(NavigationItem::Section(section)) => {
                        let closed = &mut self.sidebar_sections.collapsed[section.index()];
                        if key == Key::ArrowRight && !*closed {
                            if let Some((item, _)) = visible.get(at + 1) {
                                let child = matches!(item, NavigationItem::Folder(child, _) if *child == section)
                                    || section == Section::Network
                                        && matches!(item, NavigationItem::NetworkAction(_));
                                if child {
                                    keyboard.focused = Some(item.clone());
                                }
                            }
                        } else {
                            let target = key == Key::ArrowLeft;
                            changed = *closed != target;
                            *closed = target;
                            network_expanded |= changed && section == Section::Network && !target;
                        }
                        keyboard.reveal = true;
                    }
                    Some(NavigationItem::Folder(section, _)) if key == Key::ArrowLeft => {
                        keyboard.focused = Some(NavigationItem::Section(section));
                        keyboard.reveal = true;
                    }
                    Some(NavigationItem::NetworkAction(_)) if key == Key::ArrowLeft => {
                        keyboard.focused = Some(NavigationItem::Section(Section::Network));
                        keyboard.reveal = true;
                    }
                    _ => {}
                }
                if changed {
                    visible = rows.navigation(&self.sidebar_sections.collapsed);
                }
            }
            let at = visible
                .iter()
                .position(|(item, _)| Some(item) == keyboard.focused.as_ref())
                .unwrap_or(0);
            let (now, text) = ui.input(|input| {
                (
                    input.time,
                    if input.modifiers.any() {
                        String::new()
                    } else {
                        input
                            .events
                            .iter()
                            .filter_map(|event| {
                                if let egui::Event::Text(text) = event {
                                    Some(text.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<String>()
                    },
                )
            });
            if !text.is_empty() && !activate && !context_menu {
                if now > keyboard.prefix_until {
                    keyboard.prefix.clear();
                }
                keyboard.prefix.extend(
                    text.chars()
                        .filter(|c| !c.is_control())
                        .take(32)
                        .flat_map(char::to_lowercase),
                );
                keyboard.prefix = keyboard.prefix.chars().take(32).collect();
                keyboard.prefix_until = now + 0.7;
                let repeated = keyboard
                    .prefix
                    .chars()
                    .next()
                    .filter(|first| keyboard.prefix.chars().all(|c| c == *first));
                let prefix = repeated.map_or_else(|| keyboard.prefix.clone(), |c| c.to_string());
                if let Some(next) = (1..=visible.len())
                    .map(|offset| (at + offset) % visible.len())
                    .find(|&next| visible[next].1.to_lowercase().starts_with(&prefix))
                {
                    keyboard.focused = Some(visible[next].0.clone());
                    keyboard.reveal = true;
                }
            }
        }
        keyboard.was_focused = focused;
        let gains = MotionGains::new(ui.input(|i| i.stable_dt), t.duration());
        let geometry_gains = foldex_ui::motion::layout_gains(ui.ctx(), t.duration());
        self.sidebar_sections
            .motion
            .set_pixels_per_point(ui.ctx().pixels_per_point());
        self.sidebar_sections
            .motion
            .begin_frame_with_gains(geometry_gains);
        let physical_size = (m.icon * ui.ctx().pixels_per_point())
            .ceil()
            .clamp(1.0, 256.0) as u16;
        let mut icons = Vec::new();
        let scroll = ui.scope_builder(UiBuilder::new().max_rect(content), |ui| {
            egui::ScrollArea::vertical()
                .id_salt("sidebar-scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // Home is a destination, with the same icon/text alignment as
                    // category headers and no misleading accordion chevron.
                    let (home, _) = ui
                        .allocate_exact_size(vec2(ui.available_width(), m.columns), Sense::hover());
                    background.exclude(home.intersect(ui.clip_rect()));
                    let response = ui.interact(home, Id::new("sidebar-home"), Sense::CLICK);
                    let home_item = NavigationItem::Home;
                    if response.clicked() {
                        keyboard.focused = Some(home_item.clone());
                        crate::keyboard::request_id(ui.ctx(), focus_id);
                    }
                    let hover = foldex_ui::animated_state(
                        ui.ctx(),
                        response.id.with("hover"),
                        response.hovered() || home_selected,
                        t,
                        MotionProfile::Fast,
                    );
                    ui.painter().rect_filled(
                        home,
                        t.radius,
                        Theme::color(if home_selected { t.selection } else { t.hover })
                            .gamma_multiply(hover),
                    );
                    if home.intersect(ui.clip_rect()).height() > 1.0 {
                        icons.push(RowIcon {
                            painter: ui.painter().with_clip_rect(ui.clip_rect()),
                            rect: Rect::from_center_size(
                                pos2(home.left() + 14.0 * scale, home.center().y),
                                vec2(m.icon, m.icon),
                            ),
                            key: foldex_assets::AssetKey::known_folder_icon(
                                foldex_assets::KnownFolderIcon::Home,
                                physical_size,
                            ),
                            fallback: Icon::Home,
                            tint: if home_selected { t.fg() } else { t.dim() },
                        });
                    }
                    foldex_ui::components::elided_label(
                        ui.painter(),
                        Rect::from_min_max(
                            pos2(home.left() + 28.0 * scale, home.top()),
                            pos2(home.right() - 8.0 * scale, home.bottom()),
                        ),
                        t,
                        "Home",
                        t.fg(),
                        false,
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            ui.is_enabled(),
                            home_selected,
                            "Home",
                        )
                    });
                    home_clicked = response.clicked()
                        || activate && keyboard.focused.as_ref() == Some(&home_item);
                    sidebar_focus(ui, home, &home_item, &mut keyboard, t);
                    ui.add_space(4.0 * scale);
                    for &section in order {
                        if !rows.sections.visible(section) {
                            continue;
                        }
                        let index = section.index();
                        let glyph = section_glyph(section);
                        let (header, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), m.columns),
                            Sense::hover(),
                        );
                        background.exclude(header.intersect(ui.clip_rect()));
                        if benchmark {
                            let visible = header.intersect(ui.clip_rect());
                            self.sidebar_sections.benchmark_headers[index] =
                                (visible.width() > 1.0 && visible.height() > 1.0)
                                    .then_some(visible);
                        }
                        let response = ui.interact(
                            header,
                            Id::new(("sidebar-section", section)),
                            Sense::CLICK,
                        );
                        let header_item = NavigationItem::Section(section);
                        if response.clicked() || response.secondary_clicked() {
                            keyboard.focused = Some(header_item.clone());
                            crate::keyboard::request_id(ui.ctx(), focus_id);
                        }
                        let hover = foldex_ui::animated_state(
                            ui.ctx(),
                            response.id.with("hover"),
                            response.hovered(),
                            t,
                            MotionProfile::Fast,
                        );
                        ui.painter().rect_filled(
                            header,
                            t.radius,
                            Theme::color(t.hover).gamma_multiply(hover),
                        );
                        if section == Section::Network {
                            if header.intersect(ui.clip_rect()).height() > 1.0 {
                                icons.push(RowIcon {
                                    painter: ui.painter().with_clip_rect(ui.clip_rect()),
                                    rect: Rect::from_center_size(
                                        pos2(header.left() + 14.0 * scale, header.center().y),
                                        vec2(m.icon, m.icon),
                                    ),
                                    key: foldex_assets::AssetKey::stock_icon(foldex_assets::StockIcon::Network, physical_size),
                                    fallback: glyph,
                                    tint: t.dim(),
                                });
                            }
                        } else {
                            icon(
                                ui.painter(),
                                pos2(header.left() + 14.0 * scale, header.center().y),
                                glyph,
                                t.dim(),
                                m.icon,
                            );
                        }
                        foldex_ui::components::elided_label(
                            ui.painter(),
                            Rect::from_min_max(
                                pos2(header.left() + 28.0 * scale, header.top()),
                                pos2(header.right() - 28.0 * scale, header.bottom()),
                            ),
                            t,
                            section.label(),
                            t.dim(),
                            false,
                        );
                        let target = if self.sidebar_sections.collapsed[index] {
                            0.0
                        } else {
                            1.0
                        };
                        let arrow = self.sidebar_sections.arrows[index].get_or_insert(target);
                        if gains.step_normalized(arrow, target, MotionProfile::Standard) {
                            ui.ctx().request_repaint();
                        }
                        foldex_ui::chevron(
                            ui.painter(),
                            pos2(header.right() - 16.0 * scale, header.center().y),
                            *arrow,
                            t.dim(),
                            11.0 * scale,
                        );
                        if response.clicked()
                            || activate && keyboard.focused.as_ref() == Some(&header_item)
                        {
                            self.sidebar_sections.collapsed[index] =
                                !self.sidebar_sections.collapsed[index];
                            network_expanded |= section == Section::Network
                                && !self.sidebar_sections.collapsed[index];
                        }
                        if response.secondary_clicked()
                            || context_menu && keyboard.focused.as_ref() == Some(&header_item)
                        {
                            self.sidebar_sections.open_menu(
                                ui.ctx(),
                                SidebarMenuTarget::Section(section),
                                if response.secondary_clicked() {
                                    response.interact_pointer_pos()
                                } else {
                                    None
                                }
                                .unwrap_or(header.left_bottom()),
                                248.0 * scale,
                            );
                        }
                        response.widget_info(|| {
                            egui::WidgetInfo::selected(
                                egui::WidgetType::CollapsingHeader,
                                ui.is_enabled(),
                                !self.sidebar_sections.collapsed[index],
                                section.label(),
                            )
                        });
                        sidebar_focus(ui, header, &header_item, &mut keyboard, t);
                        response.on_hover_ui(|ui| {
                            ui.set_max_width(ui.spacing().tooltip_width);
                            ui.label(format!(
                                "{} {} · Right-click for options",
                                if self.sidebar_sections.collapsed[index] {
                                    "Expand"
                                } else {
                                    "Collapse"
                                },
                                section.label()
                            ));
                        });
                        let filtered = &rows.filtered[index];
                        let storage_extra = if section == Section::Storage && show_usage {
                            8.0 * scale
                        } else {
                            0.0
                        };
                        let height_per_row = row_height + storage_extra;
                        let extra_rows = if section == Section::Network {
                            NetworkAction::ALL.len() + 1
                        } else {
                            0
                        };
                        let target = if self.sidebar_sections.collapsed[index] {
                            0.0
                        } else {
                            (filtered.len() + extra_rows) as f32 * height_per_row
                        };
                        let height = self.sidebar_sections.heights[index].get_or_insert(target);
                        if geometry_gains.step_pixels(height, target, MotionProfile::Standard) {
                            ui.ctx().request_repaint();
                        }
                        let (body, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), *height),
                            Sense::hover(),
                        );
                        let clip = body.intersect(ui.clip_rect());
                        let origin = body.min.to_vec2();
                        for (row, &source) in filtered.iter().enumerate() {
                            let item = &groups[index][source];
                            let identity = Id::new(("sidebar-item", section, &item.path));
                            let target = Rect::from_min_size(
                                pos2(0.0, row as f32 * height_per_row),
                                vec2(body.width(), height_per_row),
                            );
                            let r = self
                                .sidebar_sections
                                .motion
                                .place(identity, target, false)
                                .rect
                                .translate(origin);
                            if keyboard.reveal && matches!(keyboard.focused.as_ref(),
                                Some(NavigationItem::Folder(group, path)) if *group == section && path == &item.path)
                            {
                                ui.scroll_to_rect_animation(
                                    r,
                                    None,
                                    egui::style::ScrollAnimation::duration(t.duration()),
                                );
                            }
                            if r.intersect(clip).height() < 1.0 {
                                continue;
                            }
                            let row_item = NavigationItem::Folder(section, item.path.clone());
                            let response =
                                ui.interact(r.intersect(clip), identity.with("hit"), Sense::CLICK);
                            crate::drag_drop::target(ui, r.intersect(clip), item.path.clone());
                            background.exclude(r.intersect(clip));
                            if response.clicked()
                                || response.secondary_clicked()
                                || response.middle_clicked()
                            {
                                keyboard.focused = Some(row_item.clone());
                                crate::keyboard::request_id(ui.ctx(), focus_id);
                            }
                            let selected = selection.selected[index][source];
                            let hover = foldex_ui::animated_state(
                                ui.ctx(),
                                identity.with("hover"),
                                response.hovered() || selected,
                                t,
                                MotionProfile::Fast,
                            );
                            let painter = ui.painter().with_clip_rect(clip);
                            painter.rect_filled(
                                r.shrink2(vec2(0.0, 1.0)),
                                t.radius,
                                Theme::color(if selected { t.selection } else { t.hover })
                                    .gamma_multiply(hover),
                            );
                            let mut content = painter.clone();
                            content.multiply_opacity(crate::cut_state::content_opacity(
                                self.assets.is_cut_key(item.cut_key.as_ref()),
                            ));
                            let label_row = Rect::from_min_size(r.min, vec2(r.width(), row_height));
                            let key = item
                                .place
                                .map(|kind| kind.icon_key(physical_size))
                                .unwrap_or_else(|| {
                                    if let Some(connected) = item.network_connected {
                                        foldex_assets::AssetKey::stock_icon(
                                            if connected {
                                                foldex_assets::StockIcon::NetworkDrive
                                            } else {
                                                foldex_assets::StockIcon::OfflineNetworkDrive
                                            },
                                            physical_size,
                                        )
                                    } else if section == Section::Storage {
                                        foldex_assets::AssetKey {
                                            path: item.path.clone(),
                                            size: physical_size,
                                            thumbnail: false,
                                        }
                                    } else {
                                        // Saved/offline folders use the native folder
                                        // stock icon without probing each saved path.
                                        foldex_assets::AssetKey::stock_icon(
                                            foldex_assets::StockIcon::Folder,
                                            physical_size,
                                        )
                                    }
                                });
                            icons.push(RowIcon {
                                painter: content.clone(),
                                rect: Rect::from_center_size(
                                    pos2(r.left() + 37.0 * scale, label_row.center().y),
                                    vec2(m.icon, m.icon),
                                ),
                                key,
                                fallback: item.glyph,
                                tint: if matches!(item.glyph, Icon::Folder) {
                                    Theme::color(t.folder)
                                } else {
                                    t.dim()
                                },
                            });
                            foldex_ui::components::elided_label(
                                &content,
                                Rect::from_min_max(
                                    pos2(r.left() + 52.0 * scale, label_row.top()),
                                    pos2(r.right() - 8.0 * scale, label_row.bottom()),
                                ),
                                t,
                                &item.label,
                                t.fg(),
                                false,
                            );
                            if storage_extra > 0.0 {
                                let bar = Rect::from_min_max(
                                    pos2(r.left() + 52.0 * scale, r.bottom() - 7.0 * scale),
                                    pos2(r.right() - 8.0 * scale, r.bottom() - 3.0 * scale),
                                );
                                painter.rect_filled(bar, 2.0 * scale, t.line());
                                if let Some(usage) = item.usage {
                                    let fill = Rect::from_min_size(
                                        bar.min,
                                        vec2(bar.width() * usage.fraction(), bar.height()),
                                    );
                                    painter.rect_filled(
                                        fill,
                                        2.0 * scale,
                                        if usage.near_full() {
                                            Theme::color(t.danger)
                                        } else {
                                            t.accent()
                                        },
                                    );
                                }
                            }
                            if response.clicked()
                                || activate && keyboard.focused.as_ref() == Some(&row_item)
                            {
                                actions.push(Action::Navigate(item.path.clone()));
                            }
                            if response.middle_clicked() {
                                actions.push(Action::NewTab(item.path.clone()));
                            }
                            if response.secondary_clicked()
                                || context_menu && keyboard.focused.as_ref() == Some(&row_item)
                            {
                                let target = match section {
                                    Section::Bookmarks => {
                                        Some(SidebarMenuTarget::Bookmark(item.path.clone()))
                                    }
                                    Section::Recents => {
                                        Some(SidebarMenuTarget::Recent(item.path.clone()))
                                    }
                                    _ => None,
                                };
                                if let Some(target) = target {
                                    self.sidebar_sections.open_menu(
                                        ui.ctx(),
                                        target,
                                        if response.secondary_clicked() {
                                            response.interact_pointer_pos()
                                        } else {
                                            None
                                        }
                                        .unwrap_or(r.left_bottom()),
                                        248.0 * scale,
                                    );
                                } else if item.place == Some(PlaceKind::RecycleBin) {
                                    let position = response.interact_pointer_pos().unwrap_or(r.left_bottom());
                                    actions.push(Action::Navigate(item.path.clone()));
                                    actions.push(Action::RecycleBin(crate::recycle_bin::Intent::Context(position)));
                                } else {
                                    actions.push(Action::Context {
                                        paths: vec![item.path.clone()],
                                        background: false,
                                        position: if response.secondary_clicked() {
                                            response.interact_pointer_pos()
                                        } else {
                                            None
                                        }
                                        .unwrap_or(r.left_bottom()),
                                    });
                                }
                            }
                            response.widget_info(|| {
                                egui::WidgetInfo::selected(
                                    egui::WidgetType::SelectableLabel,
                                    ui.is_enabled(),
                                    selected,
                                    &item.label,
                                )
                            });
                            sidebar_focus(ui, r.intersect(clip), &row_item, &mut keyboard, t);
                            if !foldex_ui::components::tooltips_blocked(ui.ctx()) {
                                response.on_hover_ui(|ui| {
                                    ui.set_max_width(ui.spacing().tooltip_width);
                                    ui.label(if let Some(usage) = item.usage {
                                        format!(
                                            "{}\n{} free of {} · {:.0}% full",
                                            crate::pane::friendly_path(&item.path),
                                            format_bytes(usage.free),
                                            format_bytes(usage.total),
                                            usage.fraction() * 100.0
                                        )
                                    } else if section == Section::Storage {
                                        format!(
                                            "{}\nCapacity unavailable",
                                            crate::pane::friendly_path(&item.path)
                                        )
                                    } else if let Some(connected) = item.network_connected {
                                        format!("{}\n{}", crate::pane::friendly_path(&item.path),
                                            if connected { "Connected network share" } else { "Saved connection · may be offline. Open to retry, or use Connect network drive." })
                                    } else {
                                        crate::pane::friendly_path(&item.path)
                                    });
                                });
                            }
                        }
                        if section == Section::Network {
                            let footer = body.top() + filtered.len() as f32 * height_per_row;
                            let note = Rect::from_min_size(pos2(body.left(), footer), vec2(body.width(), row_height));
                            if note.intersect(clip).height() > 1.0 {
                                background.exclude(note.intersect(clip));
                                let painter = ui.painter().with_clip_rect(clip);
                                foldex_ui::components::elided_label(
                                    &painter,
                                    note.shrink2(vec2(14.0 * scale, 0.0)),
                                    t,
                                    self.sidebar_data.network.status(),
                                    if self.sidebar_data.network.has_error() { Theme::color(t.danger) } else { t.dim() },
                                    false,
                                );
                                ui.interact(note.intersect(clip), Id::new("sidebar-network-status"), Sense::hover())
                                    .on_hover_text(self.sidebar_data.network.detail());
                            }
                            for (row, action) in NetworkAction::ALL.into_iter().enumerate() {
                                let item = NavigationItem::NetworkAction(action);
                                let r = Rect::from_min_size(
                                    pos2(body.left(), footer + (row + 1) as f32 * row_height),
                                    vec2(body.width(), row_height),
                                );
                                if keyboard.reveal && keyboard.focused.as_ref() == Some(&item) {
                                    ui.scroll_to_rect_animation(r, None, egui::style::ScrollAnimation::duration(t.duration()));
                                }
                                if r.intersect(clip).height() < 1.0 {
                                    continue;
                                }
                                background.exclude(r.intersect(clip));
                                let enabled = self.sidebar_data.network.action_enabled(action)
                                    && !self.sidebar_sections.collapsed[index];
                                let response = ui.interact(r.intersect(clip), Id::new(("sidebar-network-action", row)), Sense::CLICK);
                                if response.clicked() || response.secondary_clicked() {
                                    keyboard.focused = Some(item.clone());
                                    crate::keyboard::request_id(ui.ctx(), focus_id);
                                }
                                let hover = foldex_ui::animated_state(ui.ctx(), response.id.with("hover"), enabled && response.hovered(), t, MotionProfile::Fast);
                                let painter = ui.painter().with_clip_rect(clip);
                                painter.rect_filled(r.shrink2(vec2(0.0, 1.0)), t.radius, Theme::color(t.hover).gamma_multiply(hover));
                                let tint = if enabled { t.dim() } else { t.dim().gamma_multiply(0.45) };
                                let glyph = match action {
                                    NetworkAction::Connect => Icon::Plus,
                                    NetworkAction::Browse => Icon::Folder,
                                    NetworkAction::Manage => Icon::Settings,
                                    NetworkAction::Refresh => Icon::Refresh,
                                };
                                icon(&painter, pos2(r.left() + 37.0 * scale, r.center().y), glyph, tint, m.icon);
                                let label = if action == NetworkAction::Refresh && self.sidebar_data.network.has_error() {
                                    "Retry connections"
                                } else { action.label() };
                                foldex_ui::components::elided_label(&painter,
                                    Rect::from_min_max(pos2(r.left() + 52.0 * scale, r.top()), pos2(r.right() - 8.0 * scale, r.bottom())),
                                    t, label, tint, false);
                                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label));
                                if enabled && (response.clicked() || activate && keyboard.focused.as_ref() == Some(&item)) {
                                    network_action = Some(action);
                                }
                                if response.secondary_clicked() || context_menu && keyboard.focused.as_ref() == Some(&item) {
                                    self.sidebar_sections.open_menu(ui.ctx(), SidebarMenuTarget::Section(Section::Network),
                                        response.interact_pointer_pos().unwrap_or(r.left_bottom()), 248.0 * scale);
                                }
                                sidebar_focus(ui, r.intersect(clip), &item, &mut keyboard, t);
                                response.on_hover_text(action.help());
                            }
                        }
                    }
                })
        });
        // egui reserves the full scrollbar hit rectangle even while it fades.
        // Use that current response rather than guessing from its painted width.
        if let Some(scrollbar) = ui.ctx().read_response(scroll.inner.id.with(1_usize)) {
            background.exclude(scrollbar.rect);
        }
        if benchmark {
            self.sidebar_sections.benchmark_background = background.empty_rect();
        }
        if foldex_ui::components::can_focus(ui) {
            if let Some((point, secondary)) = background.clicked(&background_response) {
                keyboard.focused = None;
                keyboard.background = true;
                keyboard.reveal = false;
                keyboard.prefix.clear();
                crate::keyboard::request_id(ui.ctx(), focus_id);
                if secondary {
                    self.sidebar_sections.open_menu(
                        ui.ctx(),
                        SidebarMenuTarget::Background,
                        point,
                        248.0 * scale,
                    );
                }
            } else if context_menu && keyboard.focused.is_none() {
                self.sidebar_sections.open_menu(
                    ui.ctx(),
                    SidebarMenuTarget::Background,
                    content.left_top(),
                    248.0 * scale,
                );
            }
        }
        ui.ctx()
            .data_mut(|data| data.insert_temp(focus_id, keyboard));
        // Resolve request positions before drawing icons, so scrolling/filtering
        // cannot momentarily display an icon belonging to a previous row.
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for icon in &icons {
            icon.key.hash(&mut hash);
        }
        let epoch = hash.finish();
        let signature = ViewportSignature {
            epoch,
            revision: self
                .sidebar_data
                .revision()
                .wrapping_add(self.sidebar_data.network.revision()),
            first: 0,
            end: icons.len(),
            size: physical_size,
            thumbnails: false,
        };
        let submitted = self
            .assets
            .set_requests(ICON_VIEW, signature, || {
                icons
                    .iter()
                    .map(|icon| VisibleAsset {
                        key: icon.key.clone(),
                        stamp: AssetStamp::UNOBSERVED,
                    })
                    .collect()
            })
            .is_ok();
        for (row, item) in icons.into_iter().enumerate() {
            let image = submitted
                .then(|| self.assets.image(ICON_VIEW, epoch, row))
                .flatten();
            let fallback = image.map_or(1.0, |image| 1.0 - image.opacity);
            if fallback > 0.0 {
                icon(
                    &item.painter,
                    item.rect.center(),
                    item.fallback,
                    item.tint.gamma_multiply(fallback),
                    m.icon,
                );
            }
            if let Some(image) = image {
                let draw = |texture, size: egui::Vec2, opacity: f32| {
                    let factor = (item.rect.width() / size.x).min(item.rect.height() / size.y);
                    item.painter.image(
                        texture,
                        Rect::from_center_size(item.rect.center(), size * factor),
                        Rect::from_min_max(egui::Pos2::ZERO, pos2(1.0, 1.0)),
                        egui::Color32::WHITE.gamma_multiply(opacity),
                    );
                };
                if let Some(previous) = image.previous {
                    draw(
                        previous.texture,
                        previous.size,
                        previous.opacity * (1.0 - image.opacity),
                    );
                }
                draw(image.texture, image.size, image.opacity);
            }
        }
        if self.sidebar_sections.motion.is_moving() {
            ui.ctx().request_repaint();
        }
        let splitter = Rect::from_min_max(
            pos2(rect.right() - 3.0, rect.top()),
            pos2(rect.right() + 3.0, rect.bottom()),
        );
        let response = ui
            .interact(splitter, Id::new("sidebar-resize"), Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        foldex_ui::components::resize_divider(ui, splitter, &response, t, true);
        let delta = foldex_ui::components::divider_delta(ui, splitter, &response);
        if delta.x != 0.0 {
            self.theme.sidebar_width = (self.theme.sidebar_width + delta.x).clamp(160.0, 500.0);
        }
        self.sidebar_context_ui(
            ui,
            &current,
            home_selected || self.pane().is_recycle_bin(),
            &mut actions,
        );
        let network_visible = self
            .sidebar_sections
            .preferences
            .sections
            .visible(Section::Network);
        if network_expanded && network_visible {
            self.sidebar_data.network.expanded();
        }
        if let Some(action) = network_action.filter(|_| network_visible) {
            self.sidebar_data.network.perform(action);
        }
        if network_expanded || network_action.is_some() {
            ui.ctx().request_repaint();
        }
        self.sidebar_sections.preferences.network_expanded =
            !self.sidebar_sections.collapsed[Section::Network.index()];
        self.service_sidebar_network();
        if home_clicked {
            self.navigate_home();
        }
        actions
    }

    fn sidebar_context_ui(
        &mut self,
        ui: &mut Ui,
        current: &std::path::Path,
        home: bool,
        actions: &mut Vec<Action>,
    ) {
        let Some(target) = self.sidebar_sections.menu_target.clone() else {
            return;
        };
        let section = match &target {
            SidebarMenuTarget::Background => None,
            SidebarMenuTarget::Section(section) => Some(*section),
            SidebarMenuTarget::Bookmark(_) => Some(Section::Bookmarks),
            SidebarMenuTarget::Recent(_) => Some(Section::Recents),
        };
        if section
            .is_some_and(|section| !self.sidebar_sections.preferences.sections.visible(section))
        {
            self.sidebar_sections.menu.close();
            self.sidebar_sections.menu_target = None;
            return;
        }
        let mut items = Vec::with_capacity(10);
        match &target {
            SidebarMenuTarget::Background => {
                items.extend(section_menu_items(
                    &self.sidebar_sections.preferences.sections,
                ));
            }
            SidebarMenuTarget::Section(section) => {
                let position = self
                    .sidebar_sections
                    .preferences
                    .order
                    .iter()
                    .position(|item| item == section)
                    .unwrap_or(0);
                items.push(menu_item(0, "Move up", Icon::Up, None, position > 0));
                items.push(menu_item(
                    1,
                    "Move down",
                    Icon::MoveDown,
                    None,
                    position + 1 < self.sidebar_sections.preferences.order.len(),
                ));
                items.push(menu_item(
                    2,
                    if self.sidebar_sections.collapsed[section.index()] {
                        "Expand"
                    } else {
                        "Collapse"
                    },
                    if self.sidebar_sections.collapsed[section.index()] {
                        Icon::Forward
                    } else {
                        Icon::ChevronUp
                    },
                    None,
                    true,
                ));
                match section {
                    Section::Bookmarks => {
                        let saved = crate::bookmarks::contains(&self.bookmarks, current);
                        items.push(menu_item(
                            3,
                            if saved {
                                "Remove current folder bookmark"
                            } else {
                                "Bookmark current folder"
                            },
                            if saved {
                                Icon::BookmarkFilled
                            } else {
                                Icon::Bookmark
                            },
                            None,
                            !home,
                        ));
                        items.push(menu_item(
                            4,
                            "Clear all bookmarks",
                            Icon::Trash,
                            None,
                            !self.bookmarks.is_empty(),
                        ));
                    }
                    Section::Storage => items.push(menu_item(
                        5,
                        "Show usage bars",
                        Icon::Drive,
                        Some(self.sidebar_sections.preferences.storage_usage),
                        true,
                    )),
                    Section::Places => {
                        items.extend(place_menu_items(&self.sidebar_sections.preferences.places));
                    }
                    Section::Recents => items.push(menu_item(
                        4,
                        "Clear recents",
                        Icon::Trash,
                        None,
                        !self.recent.is_empty(),
                    )),
                    Section::Network => {
                        for (index, action) in NetworkAction::ALL.into_iter().enumerate() {
                            items.push(menu_item(
                                200 + index as u32,
                                if action == NetworkAction::Refresh
                                    && self.sidebar_data.network.has_error()
                                {
                                    "Retry connections"
                                } else {
                                    action.label()
                                },
                                match action {
                                    NetworkAction::Connect => Icon::Plus,
                                    NetworkAction::Browse => Icon::Folder,
                                    NetworkAction::Manage => Icon::Settings,
                                    NetworkAction::Refresh => Icon::Refresh,
                                },
                                None,
                                self.sidebar_data.network.action_enabled(action),
                            ));
                        }
                    }
                }
            }
            SidebarMenuTarget::Bookmark(path) | SidebarMenuTarget::Recent(path) => {
                items.push(menu_item(10, "Open", Icon::Folder, None, true));
                items.push(menu_item(11, "Open in new tab", Icon::Plus, None, true));
                if matches!(&target, SidebarMenuTarget::Bookmark(_)) {
                    let position = self
                        .bookmarks
                        .iter()
                        .position(|saved| crate::bookmarks::path_eq(saved, path));
                    items.push(menu_item(
                        13,
                        "Move up",
                        Icon::Up,
                        None,
                        position.is_some_and(|index| index > 0),
                    ));
                    items.push(menu_item(
                        14,
                        "Move down",
                        Icon::MoveDown,
                        None,
                        position.is_some_and(|index| index + 1 < self.bookmarks.len()),
                    ));
                }
                items.push(menu_item(
                    12,
                    if matches!(&target, SidebarMenuTarget::Bookmark(_)) {
                        "Remove bookmark"
                    } else {
                        "Remove from recents"
                    },
                    Icon::Trash,
                    None,
                    true,
                ));
            }
        }
        let Some(selected) = self.sidebar_sections.menu.show_sections_width(
            ui.ctx(),
            Id::new("sidebar-context-menu"),
            &self.theme,
            self.sidebar_sections.menu_anchor,
            &items,
            sidebar_menu_sections(&target),
            288.0 * self.theme.metrics().scale,
        ) else {
            return;
        };
        let mut changed = false;
        let bookmark_target = matches!(&target, SidebarMenuTarget::Bookmark(_));
        match target {
            SidebarMenuTarget::Background => {
                if let Some(&section) = selected
                    .checked_sub(300)
                    .and_then(|index| Section::DEFAULT_ORDER.get(index as usize))
                {
                    self.sidebar_sections.preferences.sections.toggle(section);
                    // In particular, cancel a hidden Network request in this
                    // same pass while retaining its cached data and expansion.
                    self.service_sidebar_network();
                    changed = true;
                }
            }
            SidebarMenuTarget::Section(section) => match selected {
                0 | 1 => {
                    changed = self
                        .sidebar_sections
                        .preferences
                        .move_by(section, if selected == 0 { -1 } else { 1 })
                }
                2 => {
                    self.sidebar_sections.collapsed[section.index()] =
                        !self.sidebar_sections.collapsed[section.index()];
                    if section == Section::Network
                        && !self.sidebar_sections.collapsed[section.index()]
                    {
                        self.sidebar_data.network.expanded();
                    }
                    changed = true;
                }
                3 if !home => actions.push(Action::Bookmark(current.to_path_buf())),
                4 => {
                    if section == Section::Bookmarks {
                        self.bookmarks.clear();
                    } else if section == Section::Recents {
                        self.recent.clear();
                    }
                    changed = true;
                }
                5 => {
                    self.sidebar_sections.preferences.storage_usage =
                        !self.sidebar_sections.preferences.storage_usage;
                    changed = true;
                }
                100..=107 => {
                    self.sidebar_sections
                        .preferences
                        .places
                        .toggle(PlaceKind::ALL[(selected - 100) as usize]);
                    changed = true;
                }
                200..=203 if section == Section::Network => {
                    self.sidebar_sections.collapsed[section.index()] = false;
                    self.sidebar_data
                        .network
                        .perform(NetworkAction::ALL[(selected - 200) as usize]);
                    changed = true;
                }
                _ => {}
            },
            SidebarMenuTarget::Bookmark(path) | SidebarMenuTarget::Recent(path) => match selected {
                10 => actions.push(Action::Navigate(path)),
                11 => actions.push(Action::NewTab(path)),
                12 => {
                    if bookmark_target {
                        self.bookmarks
                            .retain(|saved| !crate::bookmarks::path_eq(saved, &path));
                    } else {
                        self.recent
                            .retain(|saved| !crate::bookmarks::path_eq(saved, &path));
                    }
                    changed = true;
                }
                13 | 14 => {
                    changed = crate::bookmarks::move_by(
                        &mut self.bookmarks,
                        &path,
                        if selected == 13 { -1 } else { 1 },
                    )
                }
                _ => {}
            },
        }
        if changed {
            self.session_dirty = true;
        }
        ui.ctx().request_repaint();
    }
}

fn sidebar_menu_sections(target: &SidebarMenuTarget) -> &'static [MenuSection<'static>] {
    const BACKGROUND: &[MenuSection<'static>] = &[MenuSection::new(300, "Visible sections")];
    const PLACES: &[MenuSection<'static>] = &[
        MenuSection::new(0, "Section"),
        MenuSection::new(100, "Visible places"),
    ];
    const NETWORK: &[MenuSection<'static>] = &[
        MenuSection::new(0, "Section"),
        MenuSection::new(200, "Connections"),
    ];
    const BOOKMARKS: &[MenuSection<'static>] = &[MenuSection::separator(3)];
    const STORAGE: &[MenuSection<'static>] = &[MenuSection::separator(5)];
    const RECENTS: &[MenuSection<'static>] = &[MenuSection::separator(4)];
    const BOOKMARK: &[MenuSection<'static>] =
        &[MenuSection::separator(13), MenuSection::separator(12)];
    const RECENT: &[MenuSection<'static>] = &[MenuSection::separator(12)];
    match target {
        SidebarMenuTarget::Background => BACKGROUND,
        SidebarMenuTarget::Section(Section::Places) => PLACES,
        SidebarMenuTarget::Section(Section::Network) => NETWORK,
        SidebarMenuTarget::Section(Section::Bookmarks) => BOOKMARKS,
        SidebarMenuTarget::Section(Section::Storage) => STORAGE,
        SidebarMenuTarget::Section(Section::Recents) => RECENTS,
        SidebarMenuTarget::Bookmark(_) => BOOKMARK,
        SidebarMenuTarget::Recent(_) => RECENT,
    }
}

fn section_glyph(section: Section) -> Icon {
    match section {
        Section::Bookmarks => Icon::Bookmark,
        Section::Storage => Icon::Drive,
        Section::Places => Icon::Folder,
        Section::Network => Icon::Recursive,
        Section::Recents => Icon::Clock,
    }
}

fn section_menu_items(
    sections: &crate::sidebar::SectionVisibility,
) -> [MenuItem<'static>; Section::COUNT] {
    std::array::from_fn(|index| {
        let section = Section::DEFAULT_ORDER[index];
        menu_item(
            300 + index as u32,
            section.label(),
            section_glyph(section),
            Some(sections.visible(section)),
            true,
        )
    })
}

fn place_menu_items(places: &crate::sidebar::PlacesVisibility) -> [MenuItem<'static>; 8] {
    std::array::from_fn(|index| {
        let kind = PlaceKind::ALL[index];
        menu_item(
            100 + index as u32,
            kind.label(),
            match kind {
                PlaceKind::User => Icon::Home,
                PlaceKind::Desktop => Icon::Window,
                PlaceKind::Downloads => Icon::MoveDown,
                PlaceKind::Documents => Icon::File,
                PlaceKind::Pictures => Icon::Picture,
                PlaceKind::Music => Icon::Music,
                PlaceKind::Videos => Icon::Video,
                PlaceKind::RecycleBin => Icon::Trash,
            },
            Some(places.visible(kind)),
            true,
        )
    })
}

fn menu_item(
    id: u32,
    label: &'static str,
    glyph: Icon,
    checked: Option<bool>,
    enabled: bool,
) -> MenuItem<'static> {
    MenuItem {
        id,
        label,
        shortcut: "",
        glyph,
        checked,
        enabled,
    }
}
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;

    #[test]
    fn background_menu_groups_all_five_section_toggles_with_icons_and_checks() {
        let groups = sidebar_menu_sections(&SidebarMenuTarget::Background);
        assert_eq!(
            groups
                .iter()
                .map(|group| (group.before, group.label))
                .collect::<Vec<_>>(),
            [(300, "Visible sections")]
        );
        let mut visibility = crate::sidebar::SectionVisibility::default();
        visibility.toggle(Section::Bookmarks);
        visibility.toggle(Section::Network);
        let items = section_menu_items(&visibility);
        assert_eq!(
            items.map(|item| item.label),
            ["Bookmarks", "Storage", "Places", "Network", "Recents"]
        );
        for (index, item) in section_menu_items(&visibility).iter().enumerate() {
            let section = Section::DEFAULT_ORDER[index];
            assert_eq!(item.id, 300 + index as u32);
            assert_eq!(item.checked, Some(visibility.visible(section)));
            assert!(item.enabled);
            assert!(!matches!(item.glyph, Icon::Check | Icon::Empty));
        }
    }

    fn background_fixture(
        ctx: &egui::Context,
        frame: usize,
        events: Vec<egui::Event>,
    ) -> (Option<(egui::Pos2, bool)>, Vec<bool>) {
        let mut routed = None;
        let mut children = Vec::new();
        let sidebar = Rect::from_min_size(egui::Pos2::ZERO, vec2(240.0, 300.0));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(260.0, 300.0))),
                time: Some(frame as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                let response = ui.interact(sidebar, Id::new("fixture-background"), Sense::CLICK);
                let mut background =
                    SidebarBackground::new(Rect::from_min_max(pos2(0.0, 31.0), pos2(237.0, 300.0)));
                // Filter, Home, section, item, hover-only status, child action,
                // scrollbar and resize grip are drawn after the background.
                for (index, (rect, sense)) in [
                    (
                        Rect::from_min_max(pos2(8.0, 2.0), pos2(232.0, 30.0)),
                        Sense::CLICK,
                    ),
                    (
                        Rect::from_min_max(pos2(8.0, 34.0), pos2(232.0, 64.0)),
                        Sense::CLICK,
                    ),
                    (
                        Rect::from_min_max(pos2(8.0, 70.0), pos2(232.0, 100.0)),
                        Sense::CLICK,
                    ),
                    (
                        Rect::from_min_max(pos2(8.0, 106.0), pos2(232.0, 136.0)),
                        Sense::CLICK,
                    ),
                    (
                        Rect::from_min_max(pos2(8.0, 140.0), pos2(232.0, 164.0)),
                        Sense::hover(),
                    ),
                    (
                        Rect::from_min_max(pos2(8.0, 170.0), pos2(232.0, 198.0)),
                        Sense::CLICK,
                    ),
                    (
                        Rect::from_min_max(pos2(220.0, 34.0), pos2(232.0, 300.0)),
                        Sense::CLICK | Sense::DRAG,
                    ),
                    (
                        Rect::from_min_max(pos2(237.0, 0.0), pos2(243.0, 300.0)),
                        Sense::DRAG,
                    ),
                ]
                .into_iter()
                .enumerate()
                {
                    let child = ui.interact(rect, Id::new(("fixture-child", index)), sense);
                    background.exclude(rect);
                    children.push(child.secondary_clicked());
                }
                routed = background.clicked(&response);
                let empty = background.empty_rect().unwrap();
                assert!(background.contains(empty.center()));
                assert!(
                    background
                        .controls
                        .iter()
                        .all(|rect| !rect.intersect(empty).is_positive())
                );
            },
        );
        output.textures_delta.clear();
        (routed, children)
    }

    fn fixture_secondary(point: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn background_context_routing_preserves_headers_rows_scrollbars_and_child_controls() {
        for (point, owner) in [
            (pos2(100.0, 250.0), None),
            (pos2(100.0, 15.0), Some(0)),
            (pos2(100.0, 48.0), Some(1)),
            (pos2(100.0, 83.0), Some(2)),
            (pos2(100.0, 119.0), Some(3)),
            (pos2(100.0, 151.0), Some(4)),
            (pos2(100.0, 183.0), Some(5)),
            (pos2(225.0, 250.0), Some(6)),
            (pos2(238.0, 250.0), Some(7)),
        ] {
            let ctx = egui::Context::default();
            background_fixture(&ctx, 0, vec![]);
            background_fixture(
                &ctx,
                1,
                vec![
                    egui::Event::PointerMoved(point),
                    fixture_secondary(point, true),
                ],
            );
            let (routed, children) =
                background_fixture(&ctx, 2, vec![fixture_secondary(point, false)]);
            if let Some(owner) = owner {
                assert_eq!(
                    routed, None,
                    "child {owner} must retain this context action"
                );
                if matches!(owner, 0..=3 | 5 | 6) {
                    assert!(
                        children[owner],
                        "child {owner} must own its secondary click"
                    );
                }
            } else {
                assert_eq!(routed, Some((point, true)));
            }
        }
        // A click which began on a header cannot become a background menu if
        // the release drifts into the adjacent blank gap.
        let ctx = egui::Context::default();
        background_fixture(&ctx, 0, vec![]);
        let press = pos2(100.0, 99.0);
        let release = pos2(100.0, 103.0);
        background_fixture(
            &ctx,
            1,
            vec![
                egui::Event::PointerMoved(press),
                fixture_secondary(press, true),
            ],
        );
        assert_eq!(
            background_fixture(
                &ctx,
                2,
                vec![
                    egui::Event::PointerMoved(release),
                    fixture_secondary(release, false)
                ]
            )
            .0,
            None
        );
    }

    #[test]
    fn rendered_scrollbar_hit_rect_is_excluded_from_the_background_capture_region() {
        let ctx = egui::Context::default();
        let bounds = Rect::from_min_size(egui::Pos2::ZERO, vec2(200.0, 100.0));
        let mut found = false;
        for frame in 0..5 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(bounds),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    let scroll = ui.scope_builder(UiBuilder::new().max_rect(bounds), |ui| {
                        egui::ScrollArea::vertical()
                            .id_salt("fixture-sidebar-scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                ui.allocate_space(vec2(180.0, 400.0));
                            })
                    });
                    if let Some(response) = ctx.read_response(scroll.inner.id.with(1_usize)) {
                        let mut background = SidebarBackground::new(bounds);
                        background.exclude(response.rect);
                        assert!(!background.contains(response.rect.center()));
                        let empty = background.empty_rect().unwrap();
                        assert!(!empty.intersect(response.rect).is_positive());
                        found = true;
                    }
                },
            );
            output.textures_delta.clear();
        }
        assert!(
            found,
            "the real scroll bar must provide its full interaction rect"
        );
    }

    #[test]
    fn hidden_sections_leave_home_reachable_and_repair_hidden_row_focus() {
        let mut data = crate::sidebar::Data::new(Arc::new(|| {})).unwrap();
        data.network
            .benchmark_snapshot(&[(r"\\studio\Projects", Some("P:"), true)], None);
        let mut preferences = crate::sidebar::Preferences::default();
        let collapsed = [false; Section::COUNT];
        let recent = VecDeque::from([PathBuf::from("C:/recent")]);
        let bookmarks = [PathBuf::from("C:/bookmark")];
        let original = SidebarRows::new(&recent, &bookmarks, &data, &preferences, collapsed, "");
        preferences.sections.toggle(Section::Bookmarks);
        preferences.sections.toggle(Section::Network);
        assert!(!original.matches(&recent, &bookmarks, &data, &preferences, &collapsed, ""));
        let hidden = SidebarRows::new(&recent, &bookmarks, &data, &preferences, collapsed, "");
        assert!(hidden.visible.iter().all(|(item, _)| !matches!(
            item,
            NavigationItem::Section(Section::Bookmarks | Section::Network)
                | NavigationItem::Folder(Section::Bookmarks | Section::Network, _)
                | NavigationItem::NetworkAction(_)
        )));
        assert_eq!(hidden.groups[Section::Network.index()].len(), 1);
        assert_eq!(hidden.groups[Section::Bookmarks.index()].len(), 1);
        let mut keyboard = SidebarKeyboard {
            focused: Some(NavigationItem::NetworkAction(NetworkAction::Refresh)),
            ..Default::default()
        };
        sidebar_restore_focus(&mut keyboard, &hidden.visible, &bookmarks[0], false);
        assert_eq!(keyboard.focused, Some(NavigationItem::Home));

        for section in Section::DEFAULT_ORDER {
            if preferences.sections.visible(section) {
                preferences.sections.toggle(section);
            }
        }
        let all_hidden = SidebarRows::new(&recent, &bookmarks, &data, &preferences, collapsed, "");
        assert_eq!(
            all_hidden.visible.as_slice(),
            [(NavigationItem::Home, "Home".into())]
        );
        keyboard.focused = None;
        keyboard.background = true;
        sidebar_restore_focus(&mut keyboard, &all_hidden.visible, &bookmarks[0], false);
        assert!(
            keyboard.focused.is_none(),
            "an intentional background focus must remain available for Shift+F10/Menu"
        );
        for section in Section::DEFAULT_ORDER {
            preferences.sections.toggle(section);
        }
        let restored = SidebarRows::new(&recent, &bookmarks, &data, &preferences, collapsed, "");
        assert_eq!(restored.visible, original.visible);
    }

    #[test]
    fn places_menu_separates_section_actions_from_visibility_without_turning_glyphs_into_checks() {
        let groups = sidebar_menu_sections(&SidebarMenuTarget::Section(Section::Places));
        assert_eq!(
            groups
                .iter()
                .map(|group| (group.before, group.label))
                .collect::<Vec<_>>(),
            [(0, "Section"), (100, "Visible places")]
        );
        let mut places = crate::sidebar::PlacesVisibility::default();
        places.toggle(PlaceKind::Desktop);
        let items = place_menu_items(&places);
        for (index, item) in items.iter().enumerate() {
            let kind = PlaceKind::ALL[index];
            assert_eq!(item.id, 100 + index as u32);
            assert_eq!(item.label, kind.label());
            assert_eq!(item.checked, Some(places.visible(kind)));
            assert!(item.enabled);
            assert!(!matches!(item.glyph, Icon::Check | Icon::Empty));
        }
        assert_eq!(items[1].checked, Some(false));
        assert!(matches!(items[1].glyph, Icon::Window));
        assert!(matches!(items[2].glyph, Icon::MoveDown));
        for section in [Section::Bookmarks, Section::Storage, Section::Recents] {
            assert!(
                sidebar_menu_sections(&SidebarMenuTarget::Section(section))
                    .iter()
                    .all(|group| group.label.is_empty())
            );
        }
    }

    #[test]
    fn network_rows_share_the_existing_cache_and_actions_stay_keyboard_reachable() {
        let mut data = crate::sidebar::Data::new(Arc::new(|| {})).unwrap();
        let preferences = crate::sidebar::Preferences::default();
        let collapsed = [false; Section::COUNT];
        let recent = VecDeque::new();
        let old = SidebarRows::new(&recent, &[], &data, &preferences, collapsed, "");
        data.network.benchmark_snapshot(
            &[
                (r"\\studio\Projects", Some("P:"), true),
                (r"\\backup\Archive", Some("B:"), false),
            ],
            Some("Offline provider fixture"),
        );
        assert!(!old.matches(&recent, &[], &data, &preferences, &collapsed, ""));
        let rows = SidebarRows::new(&recent, &[], &data, &preferences, collapsed, "");
        let network = &rows.groups[Section::Network.index()];
        assert_eq!(network.len(), 2);
        assert_eq!(network[0].network_connected, Some(true));
        assert_eq!(network[1].network_connected, Some(false));
        assert_eq!(network[0].path, PathBuf::from(r"\\studio\Projects"));
        assert!(network[1].label.contains("Saved"));
        for action in NetworkAction::ALL {
            assert!(
                rows.visible
                    .iter()
                    .any(|(item, _)| item == &NavigationItem::NetworkAction(action))
            );
        }
        let filtered = SidebarRows::new(&recent, &[], &data, &preferences, collapsed, "projects");
        assert_eq!(filtered.filtered[Section::Network.index()], [0]);
        assert!(
            filtered
                .visible
                .iter()
                .any(|(item, _)| item == &NavigationItem::NetworkAction(NetworkAction::Refresh))
        );
        let mut collapsed = collapsed;
        collapsed[Section::Network.index()] = true;
        assert!(!rows.navigation(&collapsed).iter().any(|(item, _)| {
            matches!(
                item,
                NavigationItem::NetworkAction(_) | NavigationItem::Folder(Section::Network, _)
            )
        }));
        assert_eq!(data.network.resources().len(), 2);
    }

    #[test]
    fn selected_sidebar_paths_are_cached_and_follow_folder_and_home_navigation() {
        let ctx = egui::Context::default();
        let data = crate::sidebar::Data::new(Arc::new(|| {})).unwrap();
        let bookmarks = vec![PathBuf::from("C:/Alpha"), PathBuf::from("C:/Zulu")];
        let rows = Arc::new(SidebarRows::new(
            &VecDeque::new(),
            &bookmarks,
            &data,
            &crate::sidebar::Preferences::default(),
            [false; Section::COUNT],
            "",
        ));
        let first = SidebarSelection::get(&ctx, &rows, &bookmarks[0], false);
        assert_eq!(first.selected[Section::Bookmarks.index()], [true, false]);
        assert!(Arc::ptr_eq(
            &first,
            &SidebarSelection::get(&ctx, &rows, &bookmarks[0], false)
        ));
        let next = SidebarSelection::get(&ctx, &rows, &bookmarks[1], false);
        assert!(!Arc::ptr_eq(&first, &next));
        assert_eq!(next.selected[Section::Bookmarks.index()], [false, true]);
        let home = SidebarSelection::get(&ctx, &rows, &bookmarks[1], true);
        assert!(home.selected.iter().flatten().all(|selected| !selected));
        #[cfg(windows)]
        assert_eq!(
            SidebarSelection::get(&ctx, &rows, std::path::Path::new(r"c:\ALPHA"), false).selected
                [Section::Bookmarks.index()],
            [true, false]
        );
    }

    #[test]
    fn sidebar_cache_tracks_saved_order_filter_preferences_and_the_existing_recent_limit() {
        let data = crate::sidebar::Data::new(Arc::new(|| {})).unwrap();
        let mut recent: VecDeque<_> = (0..12)
            .map(|index| PathBuf::from(format!("C:/recent_{index:02}")))
            .collect();
        let mut bookmarks = vec![PathBuf::from("C:/Alpha"), PathBuf::from("C:/Zulu")];
        let mut preferences = crate::sidebar::Preferences::default();
        let collapsed = [false; Section::COUNT];
        let rows = SidebarRows::new(&recent, &bookmarks, &data, &preferences, collapsed, "");
        assert_eq!(rows.groups[Section::Recents.index()].len(), 10);
        assert!(rows.matches(&recent, &bookmarks, &data, &preferences, &collapsed, ""));
        recent[11] = "C:/unrendered_tail".into();
        assert!(rows.matches(&recent, &bookmarks, &data, &preferences, &collapsed, ""));
        recent.swap(0, 1);
        assert!(!rows.matches(&recent, &bookmarks, &data, &preferences, &collapsed, ""));
        recent.swap(0, 1);
        bookmarks.swap(0, 1);
        assert!(!rows.matches(&recent, &bookmarks, &data, &preferences, &collapsed, ""));
        bookmarks.swap(0, 1);
        preferences.places.documents = false;
        assert!(!rows.matches(&recent, &bookmarks, &data, &preferences, &collapsed, ""));
        preferences.places.documents = true;
        preferences.order.swap(0, 1);
        assert!(!rows.matches(&recent, &bookmarks, &data, &preferences, &collapsed, ""));
        preferences.order.swap(0, 1);
        assert!(!rows.matches(
            &recent,
            &bookmarks,
            &data,
            &preferences,
            &collapsed,
            "alpha"
        ));

        let filtered =
            SidebarRows::new(&recent, &bookmarks, &data, &preferences, collapsed, "alpha");
        assert_eq!(filtered.filtered[Section::Bookmarks.index()], [0]);
        assert!(
            filtered.visible.iter().any(|(item, _)| item
                == &NavigationItem::Folder(Section::Bookmarks, bookmarks[0].clone()))
        );
        let mut collapsed = collapsed;
        collapsed[Section::Bookmarks.index()] = true;
        let navigation = filtered.navigation(&collapsed);
        assert!(
            navigation
                .iter()
                .any(|(item, _)| item == &NavigationItem::Section(Section::Bookmarks))
        );
        assert!(
            !navigation
                .iter()
                .any(|(item, _)| matches!(item, NavigationItem::Folder(Section::Bookmarks, _)))
        );
        assert!(!filtered.matches(
            &recent,
            &bookmarks,
            &data,
            &preferences,
            &collapsed,
            "alpha"
        ));
    }

    #[test]
    fn recent_cache_preserves_logical_order_when_the_deque_wraps() {
        let data = crate::sidebar::Data::new(Arc::new(|| {})).unwrap();
        let preferences = crate::sidebar::Preferences::default();
        let collapsed = [false; Section::COUNT];
        let mut recent = VecDeque::with_capacity(12);
        let capacity = recent.capacity();
        recent.extend((0..capacity).map(|index| PathBuf::from(format!("C:/recent_{index:02}"))));
        for _ in 0..capacity / 2 {
            let first = recent.pop_front().unwrap();
            recent.push_back(first);
        }
        assert!(
            !recent.as_slices().1.is_empty(),
            "fixture must wrap storage"
        );
        let expected: Vec<_> = recent.iter().take(10).cloned().collect();
        let rows = SidebarRows::new(&recent, &[], &data, &preferences, collapsed, "");
        assert_eq!(
            rows.groups[Section::Recents.index()]
                .iter()
                .map(|row| &row.path)
                .collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>()
        );
        assert!(rows.matches(&recent, &[], &data, &preferences, &collapsed, ""));

        // Changing only physical storage leaves the logical cache source equal.
        recent.make_contiguous();
        assert!(recent.as_slices().1.is_empty());
        assert!(rows.matches(&recent, &[], &data, &preferences, &collapsed, ""));
        let first = recent.pop_front().unwrap();
        recent.push_back(first);
        assert!(!rows.matches(&recent, &[], &data, &preferences, &collapsed, ""));
    }

    #[test]
    fn sidebar_pages_and_edges_stay_inside_visible_navigation() {
        assert_eq!(sidebar_step(0, 17, Key::ArrowUp, 5), 0);
        assert_eq!(sidebar_step(16, 17, Key::ArrowDown, 5), 16);
        assert_eq!(sidebar_step(7, 17, Key::Home, 5), 0);
        assert_eq!(sidebar_step(7, 17, Key::End, 5), 16);
        assert_eq!(sidebar_step(7, 17, Key::PageUp, 5), 2);
        assert_eq!(sidebar_step(7, 17, Key::PageDown, 5), 12);
        assert_eq!(sidebar_step(7, 17, Key::PageDown, usize::MAX), 16);
    }

    #[test]
    fn sidebar_editor_escape_does_not_clear_query() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        theme.apply(&ctx);
        let id = Id::new("sidebar-filter");
        let mut query = "project".to_owned();
        let draw = |query: &mut String, frame: u64, events: Vec<egui::Event>| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 200.0))),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    sidebar_filter(
                        ui,
                        Rect::from_min_size(pos2(10.0, 10.0), vec2(260.0, 28.0)),
                        id,
                        &theme,
                        query,
                        false,
                    )
                },
            )
            .drop_without_applying_deltas();
        };
        draw(&mut query, 0, vec![]);
        crate::keyboard::request_id(&ctx, id);
        draw(&mut query, 1, vec![]);
        draw(
            &mut query,
            2,
            vec![egui::Event::Key {
                key: Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(query, "project");
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(id));
    }
}
