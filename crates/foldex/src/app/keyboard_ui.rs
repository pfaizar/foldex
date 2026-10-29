//! Application-level focus movement and the discoverable keyboard reference.
use super::*;
use crate::{
    commands::Command,
    keyboard::{self, Region},
};

impl Foldex {
    fn can_type_to_search(&self, ctx: &egui::Context) -> bool {
        if ctx.viewport_id() != egui::ViewportId::ROOT
            || self.popup_active()
            || ctx.text_edit_focused()
            || ctx.memory(|memory| memory.top_modal_layer().is_some())
            || egui::Popup::is_any_open(ctx)
            || self.pane().address_mode
            || self.pane().focus_address
            || ctx
                .plugin_opt::<egui::text_selection::LabelSelectionState>()
                .is_some_and(|selection| selection.lock().has_selection())
        {
            return false;
        }
        matches!(
            keyboard::focused_region(ctx),
            Some(
                Region::Tabs
                    | Region::Sidebar
                    | Region::Toolbar(_)
                    | Region::Listing(_)
                    | Region::Footer(_)
            )
        ) || keyboard::listing_owns(ctx, self.pane().id)
    }

    pub(super) fn dispatch_keyboard_command(
        &mut self,
        ctx: &egui::Context,
        late: bool,
    ) -> Option<Command> {
        if !keyboard::has_search_text(ctx) || !self.can_type_to_search(ctx) {
            return self.hotkeys.dispatch(ctx, !Self::editing(ctx));
        }
        let (command, reached_text) = self.hotkeys.dispatch_before_text(ctx, !Self::editing(ctx));
        if reached_text && self.can_type_to_search(ctx) {
            self.pane_mut().focus_search_from_typing(ctx, late);
            self.hotkeys.cancel_pending();
        }
        command
    }

    pub(super) fn service_focus_exit(&mut self, ctx: &egui::Context) {
        if let Some((backwards, frame)) = self.pending_focus_exit {
            if !self.popup_active() && ctx.cumulative_frame_nr() > frame {
                if keyboard::tab(ctx, &self.keyboard_regions(true), backwards) {
                    self.pending_focus_exit = None;
                    self.sync_keyboard_pane(ctx);
                }
            }
        }
    }

    pub(super) fn sync_keyboard_pane(&mut self, ctx: &egui::Context) {
        let pane = match keyboard::focused_region(ctx) {
            Some(
                Region::Toolbar(id) | Region::Listing(id) | Region::Search(id) | Region::Footer(id),
            ) => Some(id),
            _ => None,
        };
        if let Some(id) = pane {
            let visible = self.desk.groups.iter().any(|group| {
                self.tabs
                    .iter()
                    .find(|tab| tab.left.id == group.active)
                    .is_some_and(|tab| {
                        tab.left.id == id || tab.right.as_ref().is_some_and(|pane| pane.id == id)
                    })
            });
            if visible {
                self.focus_pane(id);
            } else {
                // A closed/hidden tab's last-pass target must never reactivate
                // that tab after Ctrl+T, Ctrl+Tab or a restored workspace.
                keyboard::request_id(ctx, keyboard::listing_id(self.pane().id));
            }
        }
    }

    pub(super) fn cycle_focus_region(&mut self, ctx: &egui::Context, backwards: bool) {
        if keyboard::cycle(ctx, &self.keyboard_regions(false), backwards).is_some() {
            self.sync_keyboard_pane(ctx);
        }
    }

    pub(super) fn keyboard_regions(&self, tab_order: bool) -> Vec<Region> {
        let mut order = vec![Region::Tabs];
        if self.sidebar && self.sidebar_width >= 1.0 {
            order.push(Region::Sidebar);
        }
        let mut leaves = Vec::new();
        self.desk.root.leaves(&mut leaves);
        for leaf in leaves {
            let Some(group) = self.desk.groups.iter().find(|group| group.id == leaf) else {
                continue;
            };
            let Some(tab) = self.tabs.iter().find(|tab| tab.left.id == group.active) else {
                continue;
            };
            for pane in std::iter::once(&tab.left).chain(tab.right.iter()) {
                order.push(Region::Toolbar(pane.id));
                if tab_order {
                    order.push(Region::Search(pane.id));
                }
                order.push(Region::Listing(pane.id));
                if !tab_order {
                    order.push(Region::Search(pane.id));
                }
                order.push(Region::Footer(pane.id));
            }
        }
        if self.inspector && self.inspector_width >= 1.0 {
            order.push(Region::Inspector);
        }
        order
    }

    pub(super) fn keyboard_help_ui(&mut self, ctx: &egui::Context) {
        if !self.keyboard_help {
            return;
        }
        let mut open = true;
        let t = &self.theme;
        let scale = t.metrics().scale;
        let binding = |command| {
            let label = self.hotkeys.display_for(command);
            if label.is_empty() {
                "Unassigned".to_owned()
            } else {
                label.to_owned()
            }
        };
        let sections = [
            (
                "Move around",
                vec![
                    ("Tab / Shift+Tab".into(), "Next / previous control"),
                    (binding(Command::NextFocusRegion), "Next region"),
                    (binding(Command::PreviousFocusRegion), "Previous region"),
                    ("↑ ↓ ← →".into(), "Move within lists, tabs and menus"),
                    (
                        "Home / End / PgUp / PgDn".into(),
                        "Move to an edge or by a page",
                    ),
                ],
            ),
            (
                "Files and search",
                vec![
                    (
                        "Type to search".into(),
                        "Start typing from files or navigation",
                    ),
                    (binding(Command::Filter), "Focus this folder’s search"),
                    ("↓ / Tab".into(), "Move from search to results"),
                    (
                        "Enter / Ctrl+Enter".into(),
                        "Open / open folder in a new tab",
                    ),
                    ("Shift+↑ / Shift+↓".into(), "Extend or shrink a selection"),
                    ("Ctrl+↑ / Ctrl+↓".into(), "Move focus, keep the selection"),
                    ("Ctrl+Space".into(), "Toggle the focused item"),
                    (binding(Command::Undo), "Undo a compatible file operation"),
                    (binding(Command::Redo), "Redo the file operation"),
                    (
                        binding(Command::Recycle),
                        "Move selected items to the Recycle Bin",
                    ),
                    (
                        binding(Command::DeletePermanent),
                        "Confirm permanent deletion",
                    ),
                    (
                        binding(Command::Properties),
                        "Show native properties for selected items",
                    ),
                    (
                        "Shift+F10 / Menu".into(),
                        "Open the focused item’s context menu",
                    ),
                    (binding(Command::EditAddress), "Enter a folder address"),
                ],
            ),
            (
                "Menus and commands",
                vec![
                    (
                        binding(Command::Commands),
                        "Find any command or its shortcut",
                    ),
                    (
                        "Enter / Space".into(),
                        "Activate a control or toggle an option",
                    ),
                    ("→ / ←".into(), "Open / leave a submenu"),
                    ("Shift+Enter".into(), "Reveal a launcher result in Foldex"),
                    ("Esc".into(), "Close the innermost popup and return focus"),
                ],
            ),
        ];
        let id = Id::new("keyboard-navigation-help");
        let shown = foldex_ui::dialog::Dialog::new(
            id,
            "Keyboard navigation",
            foldex_ui::dialog::Size::Medium.width(t),
        )
        .footer(foldex_ui::dialog::Footer::new(vec![
            foldex_ui::dialog::Action::new(
                id.with("customize"),
                "Customize shortcuts…",
                foldex_ui::dialog::Kind::Secondary,
            ),
        ]))
        .show(ctx, t, &mut open, |ui| {
            let scroll_id = Id::new("keyboard-guide-focus");
            let mut scroll_state = ctx
                .data(|data| data.get_temp::<(f32, f32, f32)>(scroll_id))
                .unwrap_or_default();
            let mut requested_scroll = false;
            if ctx.memory(|memory| memory.has_focus(scroll_id)) {
                for (key, delta) in [
                    (Key::ArrowUp, -24.0 * scale),
                    (Key::ArrowDown, 24.0 * scale),
                    (Key::PageUp, -scroll_state.2.max(24.0 * scale)),
                    (Key::PageDown, scroll_state.2.max(24.0 * scale)),
                ] {
                    if foldex_ui::components::consume_key(ctx, Modifiers::NONE, key) {
                        scroll_state.0 = (scroll_state.0 + delta).clamp(0.0, scroll_state.1);
                        requested_scroll = true;
                    }
                }
                for (key, offset) in [(Key::Home, 0.0), (Key::End, scroll_state.1)] {
                    if foldex_ui::components::consume_key(ctx, Modifiers::NONE, key) {
                        scroll_state.0 = offset;
                        requested_scroll = true;
                    }
                }
                ctx.memory_mut(|memory| {
                    memory.set_focus_lock_filter(
                        scroll_id,
                        egui::EventFilter {
                            tab: false,
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            escape: true,
                        },
                    )
                });
            }
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt("keyboard-help-scroll")
                .max_height((ctx.content_rect().height() * 0.62).max(100.0))
                .auto_shrink([false, true]);
            if requested_scroll {
                scroll = scroll.vertical_scroll_offset(scroll_state.0);
            }
            let body = scroll.show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for (index, (section, rows)) in sections.iter().enumerate() {
                    if index > 0 {
                        ui.add_space(16.0 * scale);
                    }
                    foldex_ui::dialog::text(ui, t, foldex_ui::dialog::Text::Section, section);
                    ui.add_space(4.0 * scale);
                    for (keys, label) in rows {
                        let height = (t.font_size + 10.0 * scale).max(24.0 * scale);
                        let (rect, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), height),
                            Sense::hover(),
                        );
                        let key_width = (226.0 * scale).min(rect.width() * 0.48);
                        let key_rect = Rect::from_min_size(rect.min, vec2(key_width, height));
                        foldex_ui::components::popup_hints(
                            ui.painter(),
                            key_rect.translate(vec2(-14.0 * scale, 0.0)),
                            t,
                            &[foldex_ui::components::PopupHint::new(keys, "")],
                            &[foldex_ui::components::PopupHint::new(keys, "")],
                        );
                        foldex_ui::components::elided_label(
                            ui.painter(),
                            Rect::from_min_max(pos2(rect.left() + key_width, rect.top()), rect.max),
                            t,
                            label,
                            t.fg(),
                            false,
                        );
                    }
                }
            });
            let focus = foldex_ui::components::PopupFocus::surface(
                ui,
                body.inner_rect,
                scroll_id,
                "Keyboard shortcuts; use arrow and Page keys to scroll",
            );
            foldex_ui::components::focus_ring(
                ui,
                body.inner_rect.expand(4.0 * scale),
                scroll_id,
                t,
                focus.has_focus(),
            );
            ctx.data_mut(|data| {
                data.insert_temp(
                    scroll_id,
                    (
                        body.state.offset.y,
                        (body.content_size.y - body.inner_rect.height()).max(0.0),
                        body.inner_rect.height(),
                    ),
                )
            });
        });
        let customize = shown.clicked == Some(0);
        self.keyboard_help = open && !customize;
        if customize {
            self.execute_command(ctx, Command::Hotkeys);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::{key_event, pump_events, ready_fixture};
    use egui::Event;

    #[test]
    fn type_to_search_inserts_first_text_in_the_same_frame_and_gates_later_file_keys() {
        let (mut app, ctx, frame) = ready_fixture();
        let pane = app.pane().id;
        keyboard::request_id(&ctx, keyboard::listing_id(pane));
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![
                key_event(Key::G, Modifiers::NONE),
                Event::Text("g".into()),
                key_event(Key::F2, Modifiers::NONE),
            ],
        );
        assert_eq!(app.pane().filter, "g");
        assert!(!app.renaming());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new((pane, "filter")))
        );
        assert!(app.deferred_editor_input.is_empty());
    }

    #[test]
    fn type_to_search_keeps_unicode_and_subsequent_spaces_in_the_search_editor() {
        let (mut app, ctx, frame) = ready_fixture();
        keyboard::request_id(&ctx, keyboard::listing_id(app.pane().id));
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![Event::Text("é\u{301}键🙂".into())],
        );
        assert_eq!(app.pane().filter, "é\u{301}键🙂");
        pump_events(
            &mut app,
            &ctx,
            frame + 1,
            vec![
                key_event(Key::Space, Modifiers::NONE),
                Event::Text(" ".into()),
                key_event(Key::B, Modifiers::NONE),
                Event::Text("b".into()),
            ],
        );
        assert_eq!(app.pane().filter, "é\u{301}键🙂 b");
        assert!(
            !app.inspector,
            "Space in the editor must keep its text ownership"
        );
    }

    #[test]
    fn type_to_search_leaves_assigned_plain_and_modified_shortcuts_first_refusal() {
        let (mut app, ctx, frame) = ready_fixture();
        app.hotkeys = crate::hotkeys::Hotkeys::from_bindings(vec![
            crate::hotkeys::Binding::single(Command::ToggleSidebar, Key::G, Modifiers::NONE),
            crate::hotkeys::Binding::single(Command::GoTo, Key::P, Modifiers::CTRL),
        ]);
        keyboard::request_id(&ctx, keyboard::listing_id(app.pane().id));
        let sidebar = app.sidebar;
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![key_event(Key::G, Modifiers::NONE), Event::Text("g".into())],
        );
        assert_eq!(app.sidebar, !sidebar);
        assert!(app.pane().filter.is_empty());
        pump_events(
            &mut app,
            &ctx,
            frame + 1,
            vec![
                key_event(Key::P, Modifiers::CTRL),
                Event::Text("destination".into()),
            ],
        );
        assert!(app.pane().filter.is_empty());
        assert_eq!(app.goto.as_ref().unwrap().input_text(), "destination");
    }

    #[test]
    fn type_to_search_does_not_take_text_from_rename_or_modal_surfaces() {
        let (mut app, ctx, frame) = ready_fixture();
        app.execute_command(&ctx, Command::Rename);
        let editor = app.pane().rename_editor_id();
        let mut text_frame = frame;
        // The inline editor requests focus after drawing, so text must wait
        // for that ownership.
        while text_frame < frame + 8 {
            pump_events(&mut app, &ctx, text_frame, vec![]);
            text_frame += 1;
            if ctx.memory(|memory| memory.focused()) == Some(editor)
                && ctx
                    .read_response(editor)
                    .is_some_and(|response| response.enabled())
            {
                break;
            }
        }
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(editor));
        pump_events(
            &mut app,
            &ctx,
            text_frame,
            vec![Event::Text("new-name".into())],
        );
        assert!(app.pane().filter.is_empty());
        assert!(app.pane().rename_text().unwrap().contains("new-name"));
        assert_ne!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new((app.pane().id, "filter")))
        );

        // Test stale listing ownership during opening too, before a modal has
        // painted its text editor or registered its egui layer.
        app.pane_mut().drop_rename();
        for surface in 0..4 {
            app.options = surface == 0;
            app.launcher_open = surface == 1;
            app.keyboard_help = surface == 2;
            app.hotkeys_open = surface == 3;
            keyboard::request_id(&ctx, keyboard::listing_id(app.pane().id));
            assert!(!app.can_type_to_search(&ctx));
        }
        app.hotkeys_open = false;
        app.open_commands();
        assert!(!app.can_type_to_search(&ctx));
    }

    #[test]
    fn type_to_search_keeps_other_text_editors_and_inspector_navigation_local() {
        let (mut app, ctx, _) = ready_fixture();
        let editor = Id::new("type-to-search-owned-editor");
        let mut draft = String::new();
        ctx.run_ui(egui::RawInput::default(), |ui| {
            keyboard::request_id(ui.ctx(), editor);
            ui.add(egui::TextEdit::singleline(&mut draft).id(editor));
        })
        .drop_without_applying_deltas();
        ctx.run_ui(
            egui::RawInput {
                events: vec![Event::Text("owned".into())],
                ..Default::default()
            },
            |ui| {
                assert_eq!(app.dispatch_keyboard_command(ui.ctx(), false), None);
                ui.add(egui::TextEdit::singleline(&mut draft).id(editor));
            },
        )
        .drop_without_applying_deltas();
        assert_eq!(draft, "owned");
        assert!(app.pane().filter.is_empty());

        ctx.run_ui(
            egui::RawInput {
                events: vec![Event::Text("inspector".into())],
                ..Default::default()
            },
            |ui| {
                let inspector = Id::new("type-to-search-inspector-navigation");
                keyboard::composite(ui, Region::Inspector, inspector, ui.max_rect(), "Preview");
                keyboard::request_id(ui.ctx(), inspector);
                assert_eq!(app.dispatch_keyboard_command(ui.ctx(), false), None);
                assert_eq!(ui.memory(|memory| memory.focused()), Some(inspector));
                assert!(ui.input(|input| {
                    input
                        .events
                        .iter()
                        .any(|event| matches!(event, Event::Text(text) if text == "inspector"))
                }));
            },
        )
        .drop_without_applying_deltas();
        assert!(app.pane().filter.is_empty());
    }

    #[test]
    fn type_to_search_follows_the_focused_visible_pane_and_home_navigation() {
        let (mut app, ctx, frame) = ready_fixture();
        let first = app.pane().id;
        app.split_current_route(false);
        let second = app.pane().id;
        // Paint both visible panes, then leave the application's active pane
        // stale deliberately. Keyboard ownership chooses its search target.
        pump_events(&mut app, &ctx, frame, vec![]);
        app.focus_pane(first);
        keyboard::request_id(&ctx, keyboard::listing_id(second));
        pump_events(
            &mut app,
            &ctx,
            frame + 1,
            vec![Event::Text("second".into())],
        );
        assert_eq!(app.pane().id, second);
        assert_eq!(app.pane().filter, "second");
        assert!(
            app.tabs
                .iter()
                .find(|tab| tab.left.id == first)
                .unwrap()
                .left
                .filter
                .is_empty()
        );

        app.navigate_home();
        keyboard::request_id(&ctx, keyboard::listing_id(second));
        pump_events(
            &mut app,
            &ctx,
            frame + 2,
            vec![Event::Text("recent".into())],
        );
        assert!(app.pane().is_home());
        assert_eq!(app.pane().filter, "recent");
    }

    #[test]
    fn late_type_to_search_inserts_once_after_pointer_focus_in_the_same_frame() {
        let (mut app, ctx, frame) = ready_fixture();
        let position = pos2(650.0, 350.0);
        let pointer = |pressed| Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let output = pump_events(
            &mut app,
            &ctx,
            frame,
            vec![
                Event::PointerMoved(position),
                pointer(true),
                key_event(Key::G, Modifiers::NONE),
                Event::Text("g".into()),
            ],
        );
        assert_eq!(app.pane().filter, "g");
        assert!(output.platform_output.num_completed_passes > 1);
        assert!(app.deferred_editor_input.is_empty());
        pump_events(&mut app, &ctx, frame + 1, vec![pointer(false)]);
        assert_eq!(app.pane().filter, "g");
    }

    #[test]
    fn type_to_search_never_targets_the_explorer_from_another_viewport() {
        let (mut app, ctx, _) = ready_fixture();
        let child = egui::ViewportId::from_hash_of("type-to-search-other-viewport");
        let mut input = egui::RawInput {
            viewport_id: child,
            events: vec![Event::Text("child-only".into())],
            ..Default::default()
        };
        input.viewports.entry(child).or_default();
        ctx.run_ui(input, |ui| {
            keyboard::request_id(ui.ctx(), keyboard::listing_id(app.pane().id));
            assert!(!app.can_type_to_search(ui.ctx()));
            assert_eq!(app.dispatch_keyboard_command(ui.ctx(), false), None);
            assert!(app.pane().filter.is_empty());
        })
        .drop_without_applying_deltas();
    }
}
