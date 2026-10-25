//! Each leaf owns its tab order and active tab; panes remain in the shared store.
use super::*;
use crate::workspace_tree::{Divider, GroupState, Node, TreeState};
use foldex_ui::motion::{LayoutMotion, MotionProfile};

const RETAINED_GEOMETRY: usize = 512;

#[derive(Default)]
struct GeometryBuffers {
    leaves: Vec<(u64, Rect)>,
    dividers: Vec<Divider>,
    placements: Vec<(u64, Rect)>,
}

impl GeometryBuffers {
    fn layout(
        &mut self,
        root: &Node,
        content: Rect,
        pixels_per_point: f32,
        motion: &mut LayoutMotion,
    ) {
        self.leaves.clear();
        self.dividers.clear();
        self.placements.clear();
        root.geometry(
            content,
            pixels_per_point,
            4.0,
            &mut self.leaves,
            &mut self.dividers,
        );
        self.placements
            .extend(self.leaves.iter().map(|&(id, target)| {
                let rect = motion
                    .place(Id::new(("workspace", id)), target, false)
                    .rect
                    .intersect(content);
                (id, rect)
            }));
    }

    fn recycle(&mut self) {
        fn recycle<T>(values: &mut Vec<T>) {
            if values.capacity() > RETAINED_GEOMETRY {
                // Large workspaces still paint every group. Only their scratch
                // allocation is released instead of retained indefinitely.
                *values = Vec::new();
            } else {
                values.clear();
            }
        }
        recycle(&mut self.leaves);
        recycle(&mut self.dividers);
        recycle(&mut self.placements);
    }
}

pub(super) struct Group {
    pub id: u64,
    pub tabs: Vec<u64>,
    pub active: u64,
    pub rect: Rect,
    pub motion: LayoutMotion,
}
pub(super) struct Desk {
    pub root: Node,
    pub groups: Vec<Group>,
    pub focused: u64,
    pub next_id: u64,
    pub motion: LayoutMotion,
    geometry: GeometryBuffers,
}
impl Desk {
    pub fn hash_state(&self, h: &mut impl Hasher) {
        self.root.hash_state(h);
        self.focused.hash(h);
        for group in &self.groups {
            group.id.hash(h);
            group.tabs.hash(h);
            group.active.hash(h);
        }
    }
    pub fn new(tabs: Vec<u64>, active: u64) -> Self {
        Self {
            root: Node::leaf(1),
            groups: vec![Group {
                id: 1,
                tabs,
                active,
                rect: Rect::NOTHING,
                motion: Default::default(),
            }],
            focused: 1,
            next_id: 2,
            motion: Default::default(),
            geometry: Default::default(),
        }
    }
    pub fn group_for(&self, tab: u64) -> Option<usize> {
        self.groups
            .iter()
            .position(|group| group.tabs.contains(&tab))
    }
    pub fn visible(&self) -> Vec<u64> {
        self.groups.iter().map(|group| group.active).collect()
    }
    pub fn focus(&mut self, tab: u64) {
        if let Some(at) = self.group_for(tab) {
            self.focused = self.groups[at].id;
            self.groups[at].active = tab;
        }
    }
    pub fn add(&mut self, tab: u64) {
        let group = self
            .groups
            .iter_mut()
            .find(|group| group.id == self.focused)
            .unwrap();
        let at = group
            .tabs
            .iter()
            .position(|id| *id == group.active)
            .map_or(group.tabs.len(), |at| at + 1);
        group.tabs.insert(at, tab);
        group.active = tab;
    }
    pub fn split(&mut self, new_tab: u64, vertical: bool) {
        let group_id = self.next_id;
        let node_id = group_id + 1;
        self.next_id += 2;
        let origin = self
            .groups
            .iter()
            .find(|group| group.id == self.focused)
            .map_or(Rect::NOTHING, |group| group.rect);
        if self.root.split(self.focused, group_id, node_id, vertical) {
            self.groups.push(Group {
                id: group_id,
                tabs: vec![new_tab],
                active: new_tab,
                rect: origin,
                motion: Default::default(),
            });
            if origin.is_finite() {
                let collapsed = if vertical {
                    Rect::from_min_max(origin.left_bottom(), origin.right_bottom())
                } else {
                    Rect::from_min_max(origin.right_top(), origin.right_bottom())
                };
                self.motion
                    .seed(Id::new(("workspace", group_id)), collapsed);
            }
            self.focused = group_id;
        }
    }
    pub fn capture(&self, tabs: &[Tab]) -> TreeState {
        TreeState {
            root: self.root.clone(),
            focused: self.focused,
            groups: self
                .groups
                .iter()
                .map(|group| GroupState {
                    id: group.id,
                    tabs: group
                        .tabs
                        .iter()
                        .filter_map(|id| tabs.iter().position(|tab| tab.left.id == *id))
                        .collect(),
                    active: group
                        .tabs
                        .iter()
                        .position(|id| *id == group.active)
                        .unwrap_or(0),
                })
                .collect(),
        }
    }
    pub fn restore(state: TreeState, tabs: &[Tab]) -> Option<Self> {
        if state.groups.is_empty() || state.groups.len() > tabs.len() {
            return None;
        }
        let mut leaves = vec![];
        state.root.leaves(&mut leaves);
        if leaves.len() != state.groups.len() {
            return None;
        }
        let mut used = std::collections::HashSet::new();
        let mut group_ids = std::collections::HashSet::new();
        let mut groups = vec![];
        for group in state.groups {
            if group.tabs.is_empty() || !group_ids.insert(group.id) || !leaves.contains(&group.id) {
                return None;
            }
            let mut ids = vec![];
            for index in group.tabs {
                if !used.insert(index) {
                    return None;
                }
                ids.push(tabs.get(index)?.left.id);
            }
            let active = ids[group.active.min(ids.len() - 1)];
            groups.push(Group {
                id: group.id,
                tabs: ids,
                active,
                rect: Rect::NOTHING,
                motion: Default::default(),
            });
        }
        if used.len() != tabs.len() {
            return None;
        }
        let focused = if group_ids.contains(&state.focused) {
            state.focused
        } else {
            groups[0].id
        };
        // Node IDs and group IDs share one allocation sequence when created.
        let next_id = max_node_id(&state.root)
            .max(group_ids.into_iter().max().unwrap_or(1))
            .saturating_add(1);
        Some(Self {
            root: state.root,
            groups,
            focused,
            next_id,
            motion: Default::default(),
            geometry: Default::default(),
        })
    }
}
fn max_node_id(node: &Node) -> u64 {
    match node {
        Node::Leaf { group } => *group,
        Node::Split { id, children, .. } => {
            children.iter().map(max_node_id).max().unwrap_or(0).max(*id)
        }
    }
}

enum StripAction {
    Focus(u64),
    Close(u64),
    Add(u64),
    Pick(u64),
    Reorder(u64, usize),
    Context(u64, egui::Pos2),
}

impl Foldex {
    pub(super) fn next_workspace(&mut self, previous: bool) {
        let mut order = vec![];
        self.desk.root.leaves(&mut order);
        let at = order
            .iter()
            .position(|id| *id == self.desk.focused)
            .unwrap_or(0);
        let next = if previous {
            (at + order.len() - 1) % order.len()
        } else {
            (at + 1) % order.len()
        };
        if let Some(id) = self
            .desk
            .groups
            .iter()
            .find(|group| group.id == order[next])
            .map(|group| group.active)
        {
            self.focus_pane(id);
        }
    }
    pub(super) fn next_group_tab(&mut self, previous: bool) {
        let Some(group) = self
            .desk
            .groups
            .iter()
            .find(|group| group.id == self.desk.focused)
        else {
            return;
        };
        let at = group
            .tabs
            .iter()
            .position(|id| *id == group.active)
            .unwrap_or(0);
        let next = if previous {
            (at + group.tabs.len() - 1) % group.tabs.len()
        } else {
            (at + 1) % group.tabs.len()
        };
        self.focus_pane(group.tabs[next]);
    }
    pub(super) fn split_workspace(&mut self, path: PathBuf, vertical: bool) {
        let view = crate::workspace::ViewSettings::capture(self.pane());
        let mut pane = Pane::new(self.next_id, &self.engine, path);
        pane.attach_search(self.search.client());
        view.apply(&mut pane);
        let id = pane.id;
        self.next_id += 1;
        self.tabs.push(Tab {
            left: pane,
            right: None,
            vertical: false,
            pending_axis: None,
            split: 0.5,
            reveal: 0.0,
            closing: false,
        });
        self.desk.split(id, vertical);
        self.active = self.tabs.len() - 1;
        self.active_pane = 0;
        self.session_dirty = true;
    }
    pub(super) fn close_workspace(&mut self, group_id: u64) {
        let Some(at) = self
            .desk
            .groups
            .iter()
            .position(|group| group.id == group_id)
        else {
            return;
        };
        if self.desk.groups.len() == 1 {
            // The last tab/workspace closes the explorer just like its native X.
            // Keep a valid root and its session while the tray owns the process.
            self.request_explorer_close(&self.ui_context.clone());
            return;
        }
        let group = self.desk.groups.remove(at);
        self.desk.root.remove(group_id);
        let focused = self.desk.groups[at.saturating_sub(1).min(self.desk.groups.len() - 1)].active;
        for id in group.tabs {
            if let Some(index) = self.tabs.iter().position(|tab| tab.left.id == id) {
                let tab = self.tabs.remove(index);
                self.closed_tabs.push_front(Self::tab_state(&tab));
                self.retire_pane(tab.left);
                if let Some(right) = tab.right {
                    self.retire_pane(right);
                }
            }
        }
        self.closed_tabs.truncate(64);
        self.focus_pane(focused);
        self.session_dirty = true;
    }
    pub(super) fn workspace_canvas(&mut self, ui: &mut Ui, content: Rect, direct: bool) -> bool {
        self.desk.focus(self.tabs[self.active].left.id);
        let mut geometry = std::mem::take(&mut self.desk.geometry);
        self.desk
            .motion
            .set_pixels_per_point(ui.ctx().pixels_per_point());
        self.desk
            .motion
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(
                ui.ctx(),
                self.theme.duration(),
            ));
        geometry.layout(
            &self.desk.root,
            content,
            ui.ctx().pixels_per_point(),
            &mut self.desk.motion,
        );
        self.assets.retain_views(&self.retained_asset_views());
        let mut strip_actions = vec![];
        let mut actions = vec![];
        let blocked = self.popup_active();
        let window = ui.ctx().content_rect();
        let titlebar = chrome::TitlebarGeometry::new(
            Rect::from_min_size(window.min, vec2(window.width(), 32.0)),
            self.sidebar_width,
        );
        let moving = self.desk.motion.is_moving();
        foldex_ui::motion::set_direct_layout(
            ui.ctx(),
            foldex_ui::motion::direct_layout(ui.ctx()) || direct || moving,
        );
        for &(id, rect) in &geometry.placements {
            let Some(at) = self.desk.groups.iter().position(|group| group.id == id) else {
                continue;
            };
            self.desk.groups[at].rect = rect;
            if rect.width() < 1.0 || rect.height() < 1.0 {
                continue;
            }
            let header_height = 32.0_f32.min(rect.height());
            let header = titlebar.workspace_header(Rect::from_min_max(
                rect.min,
                pos2(rect.right(), rect.top() + header_height),
            ));
            let body = Rect::from_min_max(pos2(rect.left(), rect.top() + header_height), rect.max);
            let selected = id == self.desk.focused;
            if !blocked
                && ui.input(|i| {
                    i.pointer.any_pressed()
                        && i.pointer
                            .interact_pos()
                            .is_some_and(|p| header.contains(p) || body.contains(p))
                })
            {
                strip_actions.push(StripAction::Focus(self.desk.groups[at].active));
            }
            // Keep the persistent titlebar controls visible while the sidebar
            // animates through their reserved space.
            ui.painter().rect_filled(body, 0, self.theme.bg());
            let group_count = self.desk.groups.len();
            let group = &mut self.desk.groups[at];
            draw_strip(
                ui,
                &self.theme,
                header,
                group,
                &self.tabs,
                &mut self.tab_drag,
                &mut strip_actions,
            );
            let Some(tab) = self.tabs.iter_mut().find(|tab| tab.left.id == group.active) else {
                continue;
            };
            tab.left.split_orientation = (group_count > 1).then_some(false);
            tab.left.bookmarked = !tab.left.is_virtual()
                && crate::bookmarks::contains(&self.bookmarks, &tab.left.path);
            tab.left.breadcrumb_active = self
                .goto
                .as_ref()
                .and_then(|popup| popup.active_breadcrumb(tab.left.id));
            ui.scope_builder(
                UiBuilder::new()
                    .id_salt((id, "workspace-body"))
                    .max_rect(body),
                |ui| {
                    ui.set_clip_rect(body.intersect(content).intersect(ui.clip_rect()));
                    let small = body.width() < 160.0 * self.theme.metrics().scale
                        || body.height() < 108.0 * self.theme.metrics().scale;
                    let result = if tab.left.is_home() {
                        home_ui::pane_ui(
                            ui,
                            &self.theme,
                            &mut tab.left,
                            &self.home,
                            &mut self.assets,
                            selected,
                            self.inspector,
                        )
                    } else if small {
                        compact_pane(ui, &self.theme, &mut tab.left, body, selected)
                    } else {
                        tab.left.ui(
                            ui,
                            &self.theme,
                            selected,
                            self.inspector,
                            Some(&mut self.assets),
                        )
                    };
                    actions.extend(result.into_iter().map(|action| (tab.left.id, action)));
                },
            );
            if !selected {
                // Restrained inactive tint, without changing geometry or raster scale.
                ui.painter()
                    .rect_filled(body, 0, self.theme.bg().gamma_multiply(0.12));
            }
        }
        // Leaf targets are no longer needed; reuse their allocation for the
        // currently painted bounds used by animated divider hit targets.
        geometry.leaves.clear();
        geometry
            .leaves
            .extend(self.desk.groups.iter().map(|group| (group.id, group.rect)));
        for mut divider in geometry.dividers.iter().copied() {
            divider.rect = self
                .desk
                .root
                .visual_divider(divider.node, divider.after, &geometry.leaves)
                .unwrap_or(divider.rect);
            divider.rect = titlebar.workspace_divider(divider.rect);
            if divider.rect.width() <= 0.0 || divider.rect.height() <= 0.0 {
                continue;
            }
            let hit = ui
                .interact(
                    divider.rect,
                    Id::new(("workspace-divider", divider.node, divider.after)),
                    Sense::drag(),
                )
                .on_hover_cursor(if divider.vertical {
                    egui::CursorIcon::ResizeVertical
                } else {
                    egui::CursorIcon::ResizeHorizontal
                });
            ui.painter()
                .rect_filled(divider.rect, 0, Theme::color(self.theme.chrome));
            foldex_ui::components::resize_divider(
                ui,
                divider.rect,
                &hit,
                &self.theme,
                !divider.vertical,
            );
            let delta = foldex_ui::components::divider_delta(ui, divider.rect, &hit);
            let delta = if divider.vertical { delta.y } else { delta.x };
            if delta != 0.0 {
                self.desk
                    .root
                    .resize(divider.node, divider.after, delta, divider.extent);
                self.session_dirty = true;
            }
        }
        geometry.recycle();
        self.desk.geometry = geometry;
        for action in strip_actions {
            match action {
                StripAction::Focus(id) => {
                    self.focus_pane(id);
                }
                StripAction::Close(id) => {
                    if let Some(at) = self.tabs.iter().position(|tab| tab.left.id == id) {
                        self.close_tab(at);
                    }
                }
                StripAction::Add(group) => {
                    if let Some(id) = self
                        .desk
                        .groups
                        .iter()
                        .find(|g| g.id == group)
                        .map(|g| g.active)
                    {
                        self.focus_pane(id);
                        self.new_current_route_tab();
                    }
                }
                StripAction::Pick(group) => {
                    if let Some(id) = self
                        .desk
                        .groups
                        .iter()
                        .find(|g| g.id == group)
                        .map(|g| g.active)
                    {
                        self.focus_pane(id);
                        self.open_tab_picker();
                    }
                }
                StripAction::Reorder(id, to) => {
                    if let Some(at) = self.desk.group_for(id) {
                        let group = &mut self.desk.groups[at];
                        if let Some(from) = group.tabs.iter().position(|tab| *tab == id) {
                            let id = group.tabs.remove(from);
                            group.tabs.insert(to.min(group.tabs.len()), id);
                            self.session_dirty = true;
                        }
                    }
                }
                StripAction::Context(id, point) => {
                    self.tab_menu_id = Some(id);
                    self.tab_menu_anchor = Rect::from_min_size(point, vec2(240.0, 0.0));
                    self.tab_menu.close();
                    self.tab_menu.toggle(ui.ctx());
                }
            }
        }
        if let Some(drag) = self.tab_drag.as_mut()
            && let Some(point) = window_transfer::drag_pointer(ui.ctx(), self.window_owner)
        {
            drag.active |= drag.press.distance(point) > 4.0;
            if ui.input(|input| input.pointer.primary_released())
                && window_transfer::detach_intent(drag.press, point, window, drag.active)
            {
                let id = drag.id;
                self.tab_drag = None;
                self.move_tab_to_new_window(id);
            }
        }
        if !ui.input(|input| input.pointer.primary_down()) {
            self.tab_drag = None;
        }
        for (origin, action) in actions {
            if self.focus_pane(origin) {
                self.action(action);
            }
        }
        self.workspace_tab_menu(ui.ctx());
        if direct || moving {
            foldex_ui::components::quiet_tooltips(ui.ctx());
        }
        if moving {
            ui.ctx().request_repaint();
        }
        moving
    }
    fn workspace_tab_menu(&mut self, ctx: &egui::Context) {
        use foldex_ui::components::{MenuItem, MenuSection};
        let position = self.tab_menu_id.and_then(|id| {
            let group = &self.desk.groups[self.desk.group_for(id)?];
            Some((
                group.tabs.iter().position(|tab| *tab == id)?,
                group.tabs.len(),
            ))
        });
        let entries = [
            (0, "Duplicate tab", Icon::Copy),
            (7, "Move to new window", Icon::Window),
            (5, "Move tab left", Icon::Back),
            (6, "Move tab right", Icon::Forward),
            (2, "Split right", Icon::Split),
            (3, "Split below", Icon::SplitBelow),
            (1, "Close tab", Icon::Close),
            (4, "Close workspace", Icon::Close),
        ];
        let items: Vec<_> = entries
            .iter()
            .map(|(id, label, glyph)| MenuItem {
                id: *id,
                label,
                shortcut: "",
                glyph: *glyph,
                checked: None,
                enabled: match *id {
                    5 => position.is_some_and(|(at, _)| at > 0),
                    6 => position.is_some_and(|(at, len)| at + 1 < len),
                    _ => true,
                },
            })
            .collect();
        if let Some(command) = self.tab_menu.show_sections(
            ctx,
            Id::new("tab-menu"),
            &self.theme,
            self.tab_menu_anchor,
            &items,
            &[MenuSection::separator(2), MenuSection::separator(1)],
        ) && let Some(id) = self.tab_menu_id
            && self.focus_pane(id)
        {
            match command {
                0 => self.duplicate_tab(),
                7 => self.move_tab_to_new_window(id),
                1 => self.close_tab(self.active),
                2 => self.split_current_route(false),
                3 => self.split_current_route(true),
                4 => self.close_workspace(self.desk.focused),
                5 | 6 => {
                    if let Some(at) = self.desk.group_for(id) {
                        let group = &mut self.desk.groups[at];
                        if let Some(from) = group.tabs.iter().position(|tab| *tab == id) {
                            let to = if command == 5 {
                                from.saturating_sub(1)
                            } else {
                                (from + 1).min(group.tabs.len() - 1)
                            };
                            group.tabs.swap(from, to);
                            self.session_dirty = true;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn draw_strip(
    ui: &mut Ui,
    t: &Theme,
    rect: Rect,
    group: &mut Group,
    tabs: &[Tab],
    drag: &mut Option<TabDrag>,
    actions: &mut Vec<StripAction>,
) {
    if rect.width() < 12.0 || rect.height() < 12.0 {
        return;
    }
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, 0, Theme::color(t.chrome));
    let button = 28.0_f32.min(rect.width());
    let pick = Rect::from_min_max(
        pos2(rect.right() - button, rect.top() + 2.0),
        pos2(rect.right(), rect.bottom() - 2.0),
    );
    let pick_response = foldex_ui::components::icon_button_at(
        ui,
        pick,
        Id::new((group.id, "tab-picker")),
        t,
        Icon::Down,
        false,
    )
    .on_hover_text("Search tabs in this workspace");
    foldex_ui::components::label_control(&pick_response, "Search workspace tabs");
    crate::keyboard::register(ui, crate::keyboard::Region::Tabs, pick_response.id, pick);
    if pick_response.clicked() {
        actions.push(StripAction::Pick(group.id));
    }
    let plus = rect.width() >= 112.0;
    let count = group.tabs.len();
    let available = (pick.left() - rect.left() - if plus { 36.0 } else { 8.0 }).max(1.0);
    let w = ((available - 3.0 * count.saturating_sub(1) as f32) / count as f32).clamp(86.0, 192.0);
    let add_left = (rect.left() + 4.0 + count as f32 * (w + 3.0) + 1.0)
        .min(pick.left() - 28.0)
        .max(rect.left());
    let add = Rect::from_min_size(pos2(add_left, pick.top()), vec2(28.0, pick.height()));
    if plus {
        let add_response = foldex_ui::components::icon_button_at(
            ui,
            add,
            Id::new((group.id, "new-tab")),
            t,
            Icon::Plus,
            false,
        )
        .on_hover_text("New tab · Ctrl+T");
        foldex_ui::components::label_control(&add_response, "New tab");
        crate::keyboard::register(ui, crate::keyboard::Region::Tabs, add_response.id, add);
        if add_response.clicked() {
            actions.push(StripAction::Add(group.id));
        }
    }
    let strip = Rect::from_min_max(
        rect.min + vec2(4.0, 3.0),
        pos2(
            (if plus { add.left() } else { pick.left() } - 3.0).max(rect.left() + 4.0),
            rect.bottom(),
        ),
    );
    if strip.width() < 16.0 {
        return;
    }
    let focus_id = Id::new((group.id, "tab-strip-focus"));
    let focus = crate::keyboard::composite(
        ui,
        crate::keyboard::Region::Tabs,
        focus_id,
        strip,
        "Workspace tabs",
    );
    if focus.has_focus() && foldex_ui::components::can_focus(ui) {
        let take = |key| crate::commands::take_key(ui.ctx(), Modifiers::NONE, key);
        let at = group
            .tabs
            .iter()
            .position(|id| *id == group.active)
            .unwrap_or(0);
        let target = if take(Key::ArrowLeft) {
            Some(at.saturating_sub(1))
        } else if take(Key::ArrowRight) {
            Some((at + 1).min(count - 1))
        } else if take(Key::Home) {
            Some(0)
        } else if take(Key::End) {
            Some(count - 1)
        } else {
            None
        };
        if let Some(at) = target {
            actions.push(StripAction::Focus(group.tabs[at]));
        }
        if crate::commands::take_key(ui.ctx(), Modifiers::SHIFT, Key::F10) {
            let x = (strip.left() + at as f32 * (w + 3.0)).clamp(strip.left(), strip.right());
            actions.push(StripAction::Context(group.active, pos2(x, strip.bottom())));
        }
    }
    group
        .motion
        .set_pixels_per_point(ui.ctx().pixels_per_point());
    group
        .motion
        .begin_frame_with_gains(foldex_ui::motion::layout_gains(ui.ctx(), t.duration()));
    let stride = w + 3.0;
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let press = ui.input(|i| {
        i.events.iter().find_map(|event| match event {
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                ..
            } => Some(*pos),
            _ => None,
        })
    });
    let active_changed = ui.ctx().data_mut(|data| {
        let key = Id::new((group.id, "active-tab-scroll"));
        let current = (group.active, strip.width().to_bits());
        let changed = data.get_temp::<(u64, u32)>(key) != Some(current);
        data.insert_temp(key, current);
        changed
    });
    ui.scope_builder(
        UiBuilder::new()
            .id_salt((group.id, "tab-strip"))
            .max_rect(strip),
        |ui| {
            ui.set_clip_rect(strip.intersect(ui.clip_rect()));
            egui::ScrollArea::horizontal()
                .id_salt((group.id, "tab-scroll"))
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .auto_shrink([false, false])
                .max_height(29.0)
                .show_viewport(ui, |ui, viewport| {
                    let origin = ui.min_rect().min;
                    ui.set_min_size(vec2(count as f32 * stride - 3.0, 29.0));
                    if active_changed && drag.as_ref().is_none_or(|d| !d.active) {
                        if let Some(at) = group.tabs.iter().position(|id| *id == group.active) {
                            ui.scroll_to_rect_animation(
                                Rect::from_min_size(
                                    origin + vec2(at as f32 * stride, 0.0),
                                    vec2(w, 29.0),
                                ),
                                None,
                                ui.style().scroll_animation,
                            );
                        }
                    }
                    let first = (viewport.left() / stride).floor().max(0.0) as usize;
                    let end = ((viewport.right() / stride).ceil() as usize + 1).min(count);
                    for at in first.saturating_sub(1)..end {
                        let id = group.tabs[at];
                        let Some(tab) = tabs.iter().find(|tab| tab.left.id == id) else {
                            continue;
                        };
                        let identity = Id::new(("tab", id));
                        let target =
                            Rect::from_min_size(pos2(at as f32 * stride, 0.0), vec2(w, 29.0));
                        let mut r = group
                            .motion
                            .place(identity, target, false)
                            .rect
                            .translate(origin.to_vec2());
                        let response = ui.interact(
                            r.intersect(strip),
                            identity.with("hit"),
                            Sense::CLICK | Sense::DRAG,
                        );
                        if let Some(press) = press.filter(|pos| {
                            r.contains(*pos)
                                && strip.contains(*pos)
                                && pos.x < r.right() - 24.0
                                && ui.ctx().layer_id_at(*pos) == Some(ui.layer_id())
                        }) {
                            *drag = Some(TabDrag {
                                id,
                                offset: press.x - r.left(),
                                press,
                                active: false,
                            });
                        }
                        if let Some(d) = drag.as_mut().filter(|d| d.id == id)
                            && let Some(point) = pointer
                        {
                            d.active |= d.press.distance(point) > 4.0;
                            if d.active && ui.ctx().content_rect().contains(point) {
                                let x = (point.x - d.offset)
                                    .clamp(strip.left(), (strip.right() - w).max(strip.left()));
                                r = Rect::from_min_size(pos2(x, r.top()), r.size());
                                group.motion.snap(identity, r.translate(-origin.to_vec2()));
                                let to = ((point.x - d.offset - origin.x) / stride).round().max(0.0)
                                    as usize;
                                if to.min(count - 1) != at {
                                    actions.push(StripAction::Reorder(id, to.min(count - 1)));
                                }
                                actions.push(StripAction::Focus(id));
                                ui.ctx().request_repaint();
                            }
                        }
                        let selected = id == group.active;
                        let hover = foldex_ui::animated_state(
                            ui.ctx(),
                            identity.with("hover"),
                            response.hovered(),
                            t,
                            MotionProfile::Fast,
                        );
                        let active = foldex_ui::animated_state(
                            ui.ctx(),
                            identity.with("active"),
                            selected,
                            t,
                            MotionProfile::Standard,
                        );
                        let paint = ui.painter().with_clip_rect(strip);
                        paint.rect_filled(
                            r,
                            egui::CornerRadius {
                                nw: t.radius.saturating_add(2),
                                ne: t.radius.saturating_add(2),
                                sw: 0,
                                se: 0,
                            },
                            Theme::color(t.chrome)
                                .lerp_to_gamma(Theme::color(t.hover), hover * (1.0 - active))
                                .lerp_to_gamma(t.bg(), active),
                        );
                        if selected && focus.has_focus() {
                            foldex_ui::components::focus_ring(
                                ui,
                                r.intersect(strip).shrink(1.0),
                                focus_id,
                                t,
                                true,
                            );
                        }
                        icon(
                            &paint,
                            pos2(r.left() + 16.0, r.center().y),
                            if tab.left.is_home() {
                                Icon::Home
                            } else if tab.left.is_recycle_bin() {
                                Icon::Trash
                            } else {
                                Icon::Folder
                            },
                            if tab.left.is_virtual() {
                                t.dim()
                            } else {
                                Theme::color(t.folder)
                            },
                            16.0,
                        );
                        let title =
                            Rect::from_min_max(r.min + vec2(29.0, 0.0), r.max - vec2(26.0, 0.0));
                        if title.is_positive()
                            && title.intersects(paint.clip_rect())
                            && paint.is_visible()
                        {
                            foldex_ui::components::elided_label(
                                &paint,
                                title,
                                t,
                                &tab.left.title(),
                                t.dim().lerp_to_gamma(t.fg(), active),
                                false,
                            );
                        }
                        let close = Rect::from_center_size(
                            pos2(r.right() - 14.0, r.center().y),
                            vec2(22.0, 22.0),
                        );
                        let close_visible = strip.contains_rect(close);
                        let cr = ui.interact(
                            close.intersect(strip),
                            identity.with("close"),
                            if close_visible {
                                Sense::CLICK
                            } else {
                                Sense::hover()
                            },
                        );
                        if close_visible {
                            let k = foldex_ui::animated_state(
                                ui.ctx(),
                                cr.id.with("hover"),
                                cr.hovered(),
                                t,
                                MotionProfile::Fast,
                            );
                            paint.rect_filled(
                                close,
                                t.radius,
                                Theme::color(t.hover).gamma_multiply(k),
                            );
                            icon(&paint, close.center(), Icon::Close, t.dim(), 14.0);
                        }
                        if drag.as_ref().is_none_or(|d| !d.active) {
                            if cr.clicked() || response.middle_clicked() {
                                actions.push(StripAction::Close(id));
                            } else if response.clicked() {
                                crate::keyboard::request_id(ui.ctx(), focus_id);
                                actions.push(StripAction::Focus(id));
                            }
                        }
                        if response.secondary_clicked() {
                            crate::keyboard::request_id(ui.ctx(), focus_id);
                            actions.push(StripAction::Context(
                                id,
                                response.interact_pointer_pos().unwrap_or(r.left_bottom()),
                            ));
                        }
                        response.on_hover_ui(|ui| {
                            ui.set_max_width(ui.spacing().tooltip_width);
                            ui.label(tab.left.location_text());
                        });
                    }
                });
        },
    );
    if group.motion.is_moving() {
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_workspace_geometry_preserves_nested_resize_and_collapse() {
        let mut desk = Desk::new(vec![10], 10);
        desk.split(20, false);
        desk.split(30, true);
        let rect = Rect::from_min_size(pos2(120.0, 10.0), vec2(1000.0, 750.0));
        let mut geometry = GeometryBuffers::default();
        desk.motion.set_pixels_per_point(1.25);
        desk.motion.begin_frame(1.0 / 60.0, 0.0);
        geometry.layout(&desk.root, rect, 1.25, &mut desk.motion);
        assert_eq!(
            geometry
                .placements
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            [1, 2, 4]
        );
        assert_eq!(geometry.dividers.len(), 2);
        let fixed = geometry.placements[0].1;
        let first_height = geometry.placements[1].1.height();
        let divider = *geometry.dividers.iter().find(|d| d.vertical).unwrap();
        let storage = (
            geometry.leaves.as_ptr(),
            geometry.dividers.as_ptr(),
            geometry.placements.as_ptr(),
        );
        assert!(
            desk.root
                .resize(divider.node, divider.after, 40.0, divider.extent)
        );
        geometry.recycle();
        desk.motion.begin_frame(1.0 / 60.0, 0.0);
        geometry.layout(&desk.root, rect, 1.25, &mut desk.motion);
        assert_eq!(geometry.placements[0].1, fixed);
        assert!(geometry.placements[1].1.height() > first_height);
        for (at, (_, area)) in geometry.placements.iter().enumerate() {
            assert!(area.is_positive() && rect.contains_rect(*area));
            for (_, other) in &geometry.placements[at + 1..] {
                assert!(!area.intersect(*other).is_positive());
            }
        }
        assert_eq!(
            storage,
            (
                geometry.leaves.as_ptr(),
                geometry.dividers.as_ptr(),
                geometry.placements.as_ptr(),
            ),
            "same-sized layouts must reuse their scratch storage"
        );
        for (removed, remaining) in [(4, 2), (2, 1)] {
            assert!(desk.root.remove(removed));
            desk.groups.retain(|group| group.id != removed);
            geometry.recycle();
            desk.motion.begin_frame(1.0 / 60.0, 0.0);
            geometry.layout(&desk.root, rect, 1.25, &mut desk.motion);
            assert_eq!(geometry.placements.len(), remaining);
            assert_eq!(geometry.dividers.len(), remaining - 1);
            assert!(!geometry.placements.iter().any(|(id, _)| *id == removed));
        }
        assert_eq!(geometry.placements, [(1, rect)]);
        assert_eq!(desk.groups[0].tabs, [10]);
    }

    #[test]
    fn oversized_workspace_geometry_keeps_all_groups_but_releases_large_scratch() {
        let count = RETAINED_GEOMETRY + 8;
        let root = Node::Split {
            id: 1000,
            vertical: false,
            children: (1..=count as u64).map(Node::leaf).collect(),
            weights: vec![1.0; count],
        };
        let mut geometry = GeometryBuffers::default();
        let mut motion = LayoutMotion::default();
        motion.begin_frame(1.0 / 60.0, 0.0);
        geometry.layout(
            &root,
            Rect::from_min_size(egui::Pos2::ZERO, vec2(6400.0, 760.0)),
            1.0,
            &mut motion,
        );
        assert_eq!(geometry.leaves.len(), count);
        assert_eq!(geometry.placements.len(), count);
        assert_eq!(geometry.dividers.len(), count - 1);
        assert_eq!(geometry.placements.last().unwrap().0, count as u64);
        geometry.recycle();
        assert_eq!(geometry.leaves.capacity(), 0);
        assert_eq!(geometry.dividers.capacity(), 0);
        assert_eq!(geometry.placements.capacity(), 0);
    }
}

fn compact_pane(ui: &mut Ui, t: &Theme, pane: &mut Pane, rect: Rect, active: bool) -> Vec<Action> {
    // A parked or cancelled listing must resume even when its full toolbar is
    // omitted. Loading is independent of which presentation fits this pane.
    pane.prepare_view(ui.ctx());
    let mut actions = vec![];
    if rect.height() < 16.0 || rect.width() < 16.0 {
        return actions;
    }
    let row_height = t.file_row_height();
    let count = pane.view_len();
    let focused = pane.listing_focus_surface(ui, rect);
    let grid = foldex_ui::view::GridMetrics::new(rect.width(), t.font_size, row_height, 0.0, 0.0);
    pane.remember_listing_layout(grid);
    pane.grid_columns = 1;
    let offset = pane.keyboard_scroll_offset(grid, rect.height());
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt((pane.id, "compact-files"))
        .auto_shrink([false, false]);
    if let Some(offset) = offset {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    let result = scroll.show_rows(ui, row_height, count, |ui, range| {
        pane.visible_rows = range.clone();
        for row in range {
            let (r, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_height), Sense::CLICK);
            let Some(entry) = pane.entry_at(row) else {
                continue;
            };
            let title = entry.name.clone();
            let is_dir = entry.is_dir;
            let glyph = foldex_ui::file_icon(&title, is_dir);
            let color = if is_dir {
                Theme::color(t.folder)
            } else {
                t.dim()
            };
            let selected = pane.selected.contains(&(row as u32));
            let hover = foldex_ui::animated_state(
                ui.ctx(),
                response.id.with("hover"),
                response.hovered(),
                t,
                MotionProfile::Fast,
            );
            let p = ui.painter().with_clip_rect(rect);
            if selected || hover > 0.0 {
                p.rect_filled(
                    r.shrink2(vec2(2.0, 1.0)),
                    t.radius,
                    if selected {
                        Theme::color(t.selection)
                    } else {
                        Theme::color(t.hover).gamma_multiply(hover)
                    },
                );
            }
            if focused && pane.focused == Some(row) {
                foldex_ui::components::focus_ring(
                    ui,
                    r.shrink2(vec2(2.0, 1.0)),
                    pane.listing_focus_id(),
                    t,
                    true,
                );
            }
            icon(
                &p,
                pos2(r.left() + 12.0, r.center().y),
                glyph,
                color,
                14.0_f32.min(r.width() - 2.0),
            );
            if r.width() > 42.0 {
                foldex_ui::components::elided_label(
                    &p,
                    Rect::from_min_max(r.min + vec2(24.0, 0.0), r.max - vec2(5.0, 0.0)),
                    t,
                    &title,
                    t.fg(),
                    false,
                );
            }
            if response.clicked() {
                pane.focus_listing_from_pointer(ui);
                pane.selected.clear();
                pane.selected.insert(row as u32);
                pane.focused = Some(row);
            }
            if response.double_clicked()
                && let Some(path) = pane.path_at(row)
            {
                actions.push(if is_dir {
                    Action::Navigate(path)
                } else {
                    Action::Open(path)
                });
            }
            response.on_hover_text(title);
        }
    });
    pane.remember_listing_viewport(result.state.offset.y, result.inner_rect);
    let _ = active;
    pane.ensure_window(pane.visible_rows.clone());
    if count == 0 {
        icon(
            ui.painter(),
            rect.center(),
            Icon::Folder,
            t.dim(),
            16.0_f32.min(rect.width()),
        );
    }
    actions
}
