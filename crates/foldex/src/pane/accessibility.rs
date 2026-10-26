//! Virtual semantics share raw identities and the listing's existing action path.
use super::*;
use egui::accesskit::{Action as AkAction, ActionData, CustomAction, Role};
use foldex_core::Entry;

pub(super) const ROW_LIMIT: usize = 512;
pub(super) struct AccessibleRow {
    pub row: usize,
    pub id: Id,
    pub rect: Option<Rect>,
}
impl Pane {
    pub(super) fn row_identity(&self, snapshot: &Snapshot, index: u32, entry: &Entry) -> Id {
        if self.is_recycle_bin() {
            self.recycle_model
                .as_ref()
                .and_then(|model| model.items.get(index as usize))
                .map_or_else(
                    || Id::new((self.id, "recycle-row", index)),
                    |item| Id::new((self.id, "recycle-row", &item.id)),
                )
        } else {
            Id::new((
                self.id,
                &snapshot.path,
                &entry.raw_name,
                &entry.relative_path,
            ))
        }
    }
    pub(super) fn accessible_listing(
        &mut self,
        ui: &mut Ui,
        body: Rect,
        mut rows: Vec<AccessibleRow>,
        actions: &mut Vec<Action>,
    ) {
        let list = self.listing_focus_id();
        if ui.ctx().accesskit_node_builder(list, |_| ()).is_none() {
            return;
        }
        if let Some(row) = self
            .focused
            .filter(|row| !rows.iter().any(|value| value.row == *row))
        {
            if let (Some(snapshot), Some(entry)) = (&self.snapshot, self.entry_at(row)) {
                let index = if self.recursive {
                    row.saturating_sub(self.window_start) as u32
                } else {
                    self.indices[row]
                };
                rows.push(AccessibleRow {
                    row,
                    id: self.row_identity(snapshot, index, entry),
                    rect: None,
                });
            }
        }
        let enabled = foldex_ui::components::can_focus(ui) && self.displayed_view_is_current();
        let frame = ui.ctx().cumulative_frame_nr();
        let mut requests = Vec::new();
        for row in &rows {
            let id = row.id.with("accessible");
            ui.input_mut(|input| {
                input.consume_accesskit_action_requests(id, |request| {
                    if enabled
                        && matches!(
                            request.action,
                            AkAction::Click
                                | AkAction::Focus
                                | AkAction::ScrollIntoView
                                | AkAction::CustomAction
                                | AkAction::ShowContextMenu
                        )
                    {
                        requests.push((row.row, request.clone()));
                    }
                    true
                })
            });
        }
        // A discarded egui pass must not invoke Open twice or toggle selection twice.
        let handled = list.with("accessible-actions-frame");
        if !requests.is_empty() && ui.ctx().data(|d| d.get_temp::<u64>(handled)) != Some(frame) {
            ui.ctx().data_mut(|d| d.insert_temp(handled, frame));
            for (row, request) in requests.into_iter().take(32) {
                self.focused = Some(row);
                self.anchor = Some(row);
                self.scroll_to = Some(row);
                crate::keyboard::request_id(ui.ctx(), list);
                match request.action {
                    AkAction::Click => {
                        self.keyboard.cancel_range();
                        self.select_all_pending = false;
                        self.invert_pending = false;
                        if !self.selected.remove(&(row as u32)) {
                            self.selected.insert(row as u32);
                        }
                    }
                    AkAction::CustomAction if request.data == Some(ActionData::CustomAction(1)) => {
                        if self.is_recycle_bin() {
                            self.selected.clear();
                            self.selected.insert(row as u32);
                            actions.push(Action::RecycleBin(
                                crate::recycle_bin::Intent::PropertiesSelected,
                            ));
                        } else if let (Some(path), Some(entry)) =
                            (self.path_at(row), self.entry_at(row))
                        {
                            actions.push(if entry.is_dir {
                                Action::Navigate(path)
                            } else {
                                Action::Open(path)
                            });
                        }
                    }
                    AkAction::ShowContextMenu => {
                        if !self.selected.contains(&(row as u32)) {
                            self.selected.clear();
                            self.selected.insert(row as u32);
                        }
                        if let Some(action) =
                            self.request_selection(SelectionCommand::Context(body.left_top()))
                        {
                            actions.push(action);
                        }
                    }
                    _ => {}
                }
                self.sync_selection();
                self.last_selection = self.primary_selected_path();
                ui.ctx().request_repaint();
            }
        }
        let len = self.view_len();
        let active = self
            .focused
            .and_then(|focused| rows.iter().find(|r| r.row == focused))
            .map(|r| r.id.with("accessible").accesskit_id());
        ui.ctx().accesskit_node_builder(list, |node| {
            node.set_role(Role::ListBox);
            node.set_multiselectable();
            node.set_size_of_set(len);
            node.set_label(format!("Files in {}", self.path.display()));
            if self.loading || self.scanning || !self.displayed_view_is_current() {
                node.set_busy();
            }
            if let Some(id) = active {
                node.set_active_descendant(id);
            } else {
                node.clear_active_descendant();
            }
        });
        ui.scope_builder(
            UiBuilder::new()
                .id_salt((self.id, "file-semantics"))
                .max_rect(body)
                .accessibility_parent(list),
            |ui| {
                for row in rows {
                    let Some(entry) = self.entry_at(row.row) else {
                        continue;
                    };
                    let id = row.id.with("accessible");
                    // Empty interaction geometry registers parentage without covering pointer rows.
                    ui.interact(Rect::NOTHING, id, Sense::hover());
                    ui.ctx().accesskit_node_builder(id, |node| {
                        node.set_role(Role::ListBoxOption);
                        node.set_label(entry.name.clone());
                        node.set_selected(self.selected.contains(&(row.row as u32)));
                        node.set_position_in_set(row.row.saturating_add(1));
                        node.set_size_of_set(len);
                        node.set_role_description(if entry.is_dir { "Folder" } else { "File" });
                        let parent = entry
                            .relative_path
                            .as_deref()
                            .and_then(Path::parent)
                            .unwrap_or(&self.path);
                        let modified = if entry.modified_secs == 0 {
                            "unknown".into()
                        } else {
                            format_date(entry.modified_secs)
                        };
                        node.set_description(format!(
                            "{}, {}. Modified {}. {}{}{}",
                            parent.display(),
                            if entry.is_dir {
                                "Folder".into()
                            } else if entry.metadata_known {
                                format!(
                                    "{} ({} bytes)",
                                    foldex_ui::format_size(entry.size),
                                    entry.size
                                )
                            } else {
                                "Size unknown".into()
                            },
                            modified,
                            if entry.hidden { "Hidden. " } else { "" },
                            if entry.symlink { "Link. " } else { "" },
                            entry.availability.label()
                        ));
                        node.set_keyboard_shortcut(
                            "Enter opens; Space selects; Shift+F10 opens the context menu",
                        );
                        if let Some(rect) = row.rect {
                            node.set_bounds(egui::accesskit::Rect {
                                x0: rect.min.x as f64,
                                y0: rect.min.y as f64,
                                x1: rect.max.x as f64,
                                y1: rect.max.y as f64,
                            });
                        }
                        if enabled {
                            for action in [
                                AkAction::Click,
                                AkAction::Focus,
                                AkAction::ScrollIntoView,
                                AkAction::CustomAction,
                                AkAction::ShowContextMenu,
                            ] {
                                node.add_action(action);
                            }
                            node.set_custom_actions(vec![CustomAction {
                                id: 1,
                                description: if self.is_recycle_bin() {
                                    "Properties".into()
                                } else {
                                    "Open".into()
                                },
                            }]);
                        } else {
                            node.set_disabled();
                        }
                    });
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::{ActionRequest, Node, NodeId, TreeId};
    struct Harness {
        ctx: egui::Context,
        theme: Theme,
        frame: u64,
    }
    impl Harness {
        fn new() -> Self {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let theme = Theme {
                reduced_motion: true,
                ..Default::default()
            };
            theme.apply(&ctx);
            Self {
                ctx,
                theme,
                frame: 0,
            }
        }
        fn draw(
            &mut self,
            pane: &mut Pane,
            events: Vec<egui::Event>,
            discard: bool,
        ) -> (Vec<(NodeId, Node)>, Vec<Action>) {
            let body = Rect::from_min_size(Pos2::ZERO, vec2(900.0, 450.0));
            let mut actions = Vec::new();
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(body),
                    time: Some(self.frame as f64 / 60.0),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ui| {
                    if let Some(action) = pane.keyboard_navigation(ui.ctx()) {
                        actions.push(action);
                    }
                    pane.listing(
                        ui,
                        &self.theme,
                        true,
                        body,
                        &columns::DetailsLayout::new(
                            &columns::StoredColumns::default(),
                            body,
                            1.0,
                            0.5,
                        ),
                        None,
                        &mut actions,
                    );
                    if discard && ui.ctx().current_pass_index() == 0 {
                        ui.ctx().request_discard("repeat accessibility pass");
                    }
                },
            );
            let nodes = output
                .platform_output
                .accesskit_update
                .take()
                .unwrap()
                .nodes;
            output.drop_without_applying_deltas();
            self.frame += 1;
            (nodes, actions)
        }
    }
    fn fixture(count: usize) -> (Engine, Pane) {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            foldex_core::EngineOptions {
                native_watch: false,
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut pane = Pane::dormant(
            88_001,
            &engine,
            std::env::temp_dir().join("foldex-semantics"),
        );
        pane.accept(Arc::new(Snapshot::from_entries(
            pane.path.clone(),
            (0..count)
                .map(|i| {
                    Entry::from_name(
                        format!("report{i:05}.txt"),
                        false,
                        i as u64 + 10,
                        1_700_000_000,
                    )
                })
                .collect(),
        )));
        pane.loading = false;
        (engine, pane)
    }
    fn event(id: NodeId, action: AkAction, data: Option<ActionData>) -> egui::Event {
        egui::Event::AccessKitActionRequest(ActionRequest {
            target_tree: TreeId::ROOT,
            target_node: id,
            action,
            data,
        })
    }
    #[test]
    fn virtual_rows_expose_names_positions_selection_metadata_and_bounded_parentage() {
        let (_engine, mut pane) = fixture(10_000);
        let mut h = Harness::new();
        pane.selected.insert(1);
        pane.focused = Some(1);
        crate::keyboard::request_id(&h.ctx, pane.listing_focus_id());
        let (nodes, _) = h.draw(&mut pane, vec![], false);
        let list = nodes
            .iter()
            .find(|(id, _)| *id == pane.listing_focus_id().accesskit_id())
            .unwrap();
        assert_eq!(list.1.role(), Role::ListBox);
        assert_eq!(list.1.size_of_set(), Some(10_000));
        assert!(list.1.is_multiselectable());
        let rows = nodes
            .iter()
            .filter(|(_, n)| n.role() == Role::ListBoxOption)
            .collect::<Vec<_>>();
        assert!(!rows.is_empty() && rows.len() <= ROW_LIMIT + 1 && rows.len() < 100);
        for (id, _) in &rows {
            let mut current = *id;
            for _ in 0..8 {
                if current == list.0 {
                    break;
                }
                current = nodes
                    .iter()
                    .find(|(_, n)| n.children().contains(&current))
                    .expect("every row is attached")
                    .0;
            }
            assert_eq!(current, list.0);
        }
        let (id, selected) = rows
            .iter()
            .find(|(_, n)| n.label() == Some("report00001.txt"))
            .unwrap();
        assert_eq!(selected.is_selected(), Some(true));
        assert_eq!(selected.position_in_set(), Some(2));
        assert_eq!(list.1.active_descendant(), Some(*id));
        assert!(selected.description().unwrap().contains("11 bytes"));
        assert!(
            selected.supports_action(AkAction::Focus) && selected.supports_action(AkAction::Click)
        );
        assert!(selected.supports_action(AkAction::CustomAction));
        for (id, _) in &rows {
            assert_eq!(
                nodes
                    .iter()
                    .filter(|(_, n)| n.children().contains(id))
                    .count(),
                1
            );
        }
        println!(
            "ACCESSIBILITY_RESULT {}",
            serde_json::json!({"rows": 10000, "visible_semantic_rows": rows.len(), "filename_exposed": true, "node_count": nodes.len()})
        );
    }
    #[test]
    fn assistive_actions_select_focus_scroll_and_open_once_across_discarded_passes() {
        let (_engine, mut pane) = fixture(1000);
        let mut h = Harness::new();
        let (nodes, _) = h.draw(&mut pane, vec![], false);
        let id = nodes
            .iter()
            .find(|(_, n)| n.label() == Some("report00001.txt"))
            .unwrap()
            .0;
        h.draw(&mut pane, vec![event(id, AkAction::Click, None)], true);
        assert!(pane.selected.contains(&1));
        assert_eq!(pane.focused, Some(1));
        assert_eq!(h.ctx.memory(|m| m.focused()), Some(pane.listing_focus_id()));
        let (_, actions) = h.draw(
            &mut pane,
            vec![event(
                id,
                AkAction::CustomAction,
                Some(ActionData::CustomAction(1)),
            )],
            true,
        );
        assert!(
            matches!(actions.as_slice(), [Action::Open(path)] if path.ends_with("report00001.txt"))
        );
        h.draw(&mut pane, vec![event(id, AkAction::Focus, None)], false);
        let (_, actions) = h.draw(
            &mut pane,
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            false,
        );
        assert!(
            matches!(actions.as_slice(), [Action::Open(path)] if path.ends_with("report00001.txt"))
        );
        pane.focused = Some(900);
        pane.scroll_to = None;
        pane.keyboard.reveal = None;
        let (nodes, _) = h.draw(&mut pane, vec![], false);
        let offscreen = nodes
            .iter()
            .find(|(_, n)| n.label() == Some("report00900.txt"))
            .unwrap();
        assert!(offscreen.1.bounds().is_none());
        h.draw(
            &mut pane,
            vec![event(offscreen.0, AkAction::ScrollIntoView, None)],
            false,
        );
        let (nodes, _) = h.draw(&mut pane, vec![], false);
        assert!(
            nodes
                .iter()
                .find(|(id, _)| *id == offscreen.0)
                .unwrap()
                .1
                .bounds()
                .is_some()
        );
        pane.query.text = "new pending query".into();
        let (nodes, actions) = h.draw(
            &mut pane,
            vec![event(
                offscreen.0,
                AkAction::CustomAction,
                Some(ActionData::CustomAction(1)),
            )],
            false,
        );
        assert!(actions.is_empty());
        assert!(
            nodes
                .iter()
                .find(|(id, _)| *id == offscreen.0)
                .unwrap()
                .1
                .is_disabled()
        );
    }
    #[test]
    fn semantics_keep_raw_identity_across_reordering_and_same_spelling_metadata_updates() {
        let (_engine, mut pane) = fixture(2);
        let mut h = Harness::new();
        let (nodes, _) = h.draw(&mut pane, vec![], false);
        let original = nodes
            .iter()
            .find(|(_, n)| n.label() == Some("report00001.txt"))
            .unwrap()
            .0;
        pane.indices = Arc::from([1, 0]);
        let (nodes, _) = h.draw(&mut pane, vec![], false);
        let moved = nodes
            .iter()
            .find(|(_, n)| n.label() == Some("report00001.txt"))
            .unwrap();
        assert_eq!(moved.0, original);
        assert_eq!(moved.1.position_in_set(), Some(1));
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            let entries = [0xfe, 0xff]
                .map(|b| Entry::from_name(std::ffi::OsString::from_vec(vec![b]), false, 0, 0));
            let snapshot = Snapshot::from_entries(pane.path.clone(), entries.to_vec());
            assert_eq!(snapshot.entries[0].name, snapshot.entries[1].name);
            assert_ne!(
                pane.row_identity(&snapshot, 0, &snapshot.entries[0]),
                pane.row_identity(&snapshot, 1, &snapshot.entries[1])
            );
        }
    }
}
