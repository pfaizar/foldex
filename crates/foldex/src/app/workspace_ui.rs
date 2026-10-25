use super::*;
use crate::workspace::{
    NamedWorkspace, PaneState, SavedFolderView, TabState, ViewSettings, WorkspaceState,
};

impl Foldex {
    pub(super) fn tab_state(tab: &Tab) -> TabState {
        TabState {
            left: PaneState::capture(&tab.left),
            right: tab.right.as_ref().map(PaneState::capture),
            vertical: tab.pending_axis.unwrap_or(tab.vertical),
            split: tab.split,
        }
    }
    pub(super) fn workspace_state(&self) -> WorkspaceState {
        WorkspaceState {
            tree: Some(self.desk.capture(&self.tabs)),
            tabs: self.tabs.iter().map(Self::tab_state).collect(),
            active: self.active,
            active_pane: self.active_pane,
            sidebar: self.sidebar,
            sidebar_width: self.theme.sidebar_width,
            inspector: self.inspector,
            inspector_width: self.inspector_target_width,
        }
    }
    fn restore_tab(&mut self, state: TabState) -> Tab {
        let mut left = Pane::dormant(self.next_id, &self.engine, state.left.path);
        left.attach_search(self.search.client());
        self.next_id += 1;
        state.left.view.apply(&mut left);
        left.restore_navigation(state.left.navigation.unwrap_or_else(|| {
            crate::navigation::Navigation::new(
                state
                    .left
                    .route
                    .unwrap_or_else(|| crate::navigation::Route::Folder(left.path.clone())),
            )
        }));
        let right = state.right.map(|p| {
            let mut right = Pane::dormant(self.next_id, &self.engine, p.path);
            right.attach_search(self.search.client());
            self.next_id += 1;
            p.view.apply(&mut right);
            right.restore_navigation(p.navigation.unwrap_or_else(|| {
                crate::navigation::Navigation::new(
                    p.route
                        .unwrap_or_else(|| crate::navigation::Route::Folder(right.path.clone())),
                )
            }));
            right
        });
        let reveal = if right.is_some() { 1.0 } else { 0.0 };
        Tab {
            left,
            right,
            vertical: state.vertical,
            pending_axis: None,
            split: if state.split.is_finite() {
                state.split.clamp(0.15, 0.85)
            } else {
                0.5
            },
            reveal,
            closing: false,
        }
    }
    pub(super) fn restore_workspace(&mut self, state: WorkspaceState) {
        if state.tabs.is_empty() {
            return;
        }
        let tree = state.tree;
        let replacement: Vec<_> = state
            .tabs
            .into_iter()
            .take(4096)
            .map(|tab| self.restore_tab(tab))
            .collect();
        let old = std::mem::replace(&mut self.tabs, replacement);
        for tab in old {
            self.retire_pane(tab.left);
            if let Some(pane) = tab.right {
                self.retire_pane(pane);
            }
        }
        self.active = state.active.min(self.tabs.len() - 1);
        self.active_pane = 0;
        let active_left = self.tabs[self.active].left.id;
        let vertical = self.tabs[self.active].vertical;
        let active_right = self.tabs[self.active].right.as_ref().map(|pane| pane.id);
        let left_ids = self.tabs.iter().map(|tab| tab.left.id).collect();
        let mut right_tabs = vec![];
        for tab in &mut self.tabs {
            if let Some(pane) = tab.right.take() {
                right_tabs.push(Tab {
                    left: pane,
                    right: None,
                    vertical: false,
                    pending_axis: None,
                    split: 0.5,
                    reveal: 0.0,
                    closing: false,
                });
            }
        }
        self.desk = workspace_canvas::Desk::new(left_ids, active_left);
        if !right_tabs.is_empty() {
            let right_ids: Vec<_> = right_tabs.iter().map(|tab| tab.left.id).collect();
            let selected = active_right.unwrap_or(right_ids[0]);
            self.tabs.extend(right_tabs);
            self.desk.split(selected, vertical);
            let right_group = self.desk.groups.last_mut().unwrap();
            right_group.tabs = right_ids;
            right_group.active = selected;
            let focus = if state.active_pane > 0 {
                selected
            } else {
                active_left
            };
            self.focus_pane(focus);
        } else if let Some(tree) = tree {
            if let Some(desk) = workspace_canvas::Desk::restore(tree, &self.tabs) {
                self.desk = desk;
            }
            let focused = self
                .desk
                .groups
                .iter()
                .find(|group| group.id == self.desk.focused)
                .map(|group| group.active)
                .unwrap_or(active_left);
            self.focus_pane(focused);
        }
        self.sidebar = state.sidebar;
        if state.sidebar_width.is_finite() && state.sidebar_width >= 160.0 {
            self.theme.sidebar_width = state.sidebar_width.clamp(160.0, 500.0);
        }
        self.inspector = state.inspector;
        self.inspector_target_width =
            if state.inspector_width.is_finite() && state.inspector_width >= 160.0 {
                state.inspector_width.clamp(160.0, 700.0)
            } else {
                320.0
            };
        self.session_dirty = true;
        self.save_due = Some(Instant::now() + Duration::from_millis(300));
    }
    pub(super) fn reopen_tab(&mut self) {
        if let Some(state) = self.closed_tabs.pop_front() {
            let tab = self.restore_tab(state);
            let id = tab.left.id;
            self.tabs.push(tab);
            self.desk.add(id);
            self.active = self.tabs.len() - 1;
            self.active_pane = 0;
            self.session_dirty = true;
        }
    }
    pub(super) fn duplicate_tab(&mut self) {
        let state = Self::tab_state(&self.tabs[self.active]);
        let tab = self.restore_tab(state);
        let id = tab.left.id;
        self.tabs.push(tab);
        self.desk.add(id);
        self.active = self.tabs.len() - 1;
        self.active_pane = 0;
        self.session_dirty = true;
    }
    pub(super) fn save_folder_view(&mut self) {
        if self.pane().is_virtual() {
            return;
        }
        let view = SavedFolderView {
            path: self.pane().path.clone(),
            view: ViewSettings::capture(self.pane()),
        };
        if let Some(old) = self.folder_views.iter_mut().find(|v| v.path == view.path) {
            *old = view;
        } else {
            if self.folder_views.len() >= 128 {
                self.folder_views.remove(0);
            }
            self.folder_views.push(view);
        }
        self.session_dirty = true;
        self.save_due = Some(Instant::now());
        self.toast("View saved for this folder", false);
    }
    pub(super) fn apply_folder_view(&mut self) {
        if self.pane().is_virtual() {
            return;
        }
        if let Some(view) =
            crate::workspace::find_folder_view(&self.folder_views, &self.pane().path).cloned()
        {
            view.apply(self.pane_mut());
            self.pane_mut().refilter();
        }
    }
    pub(super) fn workspace_dialogs(&mut self, ctx: &egui::Context) {
        let mut save = false;
        let mut cancel = false;
        if let Some(name) = &mut self.save_layout_name {
            let mut open = true;
            foldex_ui::components::dialog(
                ctx,
                Id::new("save-layout"),
                &self.theme,
                "Save layout",
                400.0 * self.theme.metrics().scale,
                &mut open,
                |ui| {
                    ui.label("Remember these tabs, panels, view sizes and sidebar.");
                    let (field, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), self.theme.metrics().control),
                        Sense::hover(),
                    );
                    let r = foldex_ui::components::input_box(
                        ui,
                        field,
                        Id::new("layout-name"),
                        &self.theme,
                        name,
                        "Layout name",
                        64,
                    )
                    .response;
                    let initial = ctx
                        .data(|data| data.get_temp::<bool>(Id::new("save-layout-initial-focus")))
                        .unwrap_or(true);
                    if initial && foldex_ui::components::can_focus(ui) {
                        r.request_focus();
                        if r.has_focus() && !ui.is_sizing_pass() {
                            ctx.data_mut(|data| {
                                data.insert_temp(Id::new("save-layout-initial-focus"), false)
                            });
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        save = foldex_ui::components::text_button_enabled(
                            ui,
                            &self.theme,
                            "Save layout",
                            true,
                            !name.trim().is_empty(),
                        )
                        .clicked()
                            || r.has_focus()
                                && !name.trim().is_empty()
                                && ui.input(|i| i.key_pressed(Key::Enter));
                        if foldex_ui::components::text_button(ui, &self.theme, "Cancel", false)
                            .clicked()
                        {
                            ui.close();
                        }
                    });
                },
            );
            cancel |= !open;
        }
        if save {
            if let Some(name) = self
                .save_layout_name
                .take()
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty())
            {
                let item = NamedWorkspace {
                    name,
                    state: self.workspace_state(),
                };
                if let Some(old) = self.saved_layouts.iter_mut().find(|s| s.name == item.name) {
                    *old = item;
                } else {
                    if self.saved_layouts.len() >= 16 {
                        self.saved_layouts.remove(0);
                    }
                    self.saved_layouts.push(item);
                }
                self.session_dirty = true;
                self.save_due = Some(Instant::now());
            }
        } else if cancel {
            self.save_layout_name = None;
        }
        if self.save_layout_name.is_none() {
            ctx.data_mut(|data| data.remove::<bool>(Id::new("save-layout-initial-focus")));
        }
        let mut load = None;
        let mut remove = None;
        let mut open = self.load_layout_open;
        if open {
            foldex_ui::components::dialog(
                ctx,
                Id::new("saved-layouts"),
                &self.theme,
                "Saved layouts",
                400.0 * self.theme.metrics().scale,
                &mut open,
                |ui| {
                    if self.saved_layouts.is_empty() {
                        ui.label("Save a layout from the command menu to find it here.");
                    }
                    for (i, layout) in self.saved_layouts.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let width = (ui.available_width()
                                - self.theme.metrics().control
                                - self.theme.metrics().gap)
                                .max(1.0);
                            let (id, rect) =
                                ui.allocate_space(vec2(width, self.theme.metrics().control));
                            if foldex_ui::components::text_button_at(
                                ui,
                                rect,
                                id,
                                &self.theme,
                                &layout.name,
                                false,
                            )
                            .clicked()
                            {
                                load = Some(i);
                            }
                            if icon_button(
                                ui,
                                &self.theme,
                                Icon::Trash,
                                "Remove saved layout",
                                false,
                            )
                            .clicked()
                            {
                                remove = Some(i);
                            }
                        });
                    }
                },
            );
        }
        self.load_layout_open = open;
        if let Some(i) = load {
            let state = self.saved_layouts[i].state.clone();
            self.restore_workspace(state);
            self.load_layout_open = false;
        }
        if let Some(i) = remove {
            self.saved_layouts.remove(i);
            self.session_dirty = true;
            self.save_due = Some(Instant::now());
        }
    }
}

pub(super) fn hash_pane(p: &Pane, h: &mut impl Hasher) {
    p.navigation.hash(h);
    p.zoom.to_bits().hash(h);
    p.column_name.to_bits().hash(h);
    p.hash_columns(h);
    (p.query.sort as u8).hash(h);
    p.query.descending.hash(h);
    p.query.show_hidden.hash(h);
    (p.query.kind as u8).hash(h);
    p.compact_paths.hash(h);
    p.breadcrumb_history.hash(h);
}
