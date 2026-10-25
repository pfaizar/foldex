use super::*;
use crate::commands::Command;

impl Foldex {
    pub(super) fn open_options(&mut self, ctx: &egui::Context) {
        self.options_popup.prepare_open(ctx);
        self.options = true;
    }
    pub(super) fn popup_active(&self) -> bool {
        self.launcher_open
            || self.show_stats
            || self.operations_open
            || self.keyboard_help
            || (self.inspector
                && self
                    .inspector_view
                    .as_ref()
                    .is_some_and(|view| view.has_popup()))
            || self.tab_menu.active()
            || self.tabs[self.active].left.has_popup()
            || self.tabs[self.active]
                .right
                .as_ref()
                .is_some_and(|p| p.has_popup())
            || self.main_menu.is_open()
            || self.sidebar_sections.menu_active()
            || self.goto.is_some()
            || !self.commands.finished()
            || self.context.is_open()
            || self.picker.is_some()
            || self.options
            || self.updates.open
            || self.updates.exit_pending
            || self.options_popup.owns_navigation(&self.ui_context)
            || self.edit.is_some()
            || self.deletion.is_open()
            || self.recycle.is_open()
            || self.save_layout_name.is_some()
            || self.load_layout_open
            || self.hotkeys_open
            || self.batch.as_ref().is_some_and(|b| b.is_open())
    }
    pub(super) fn execute_selection(&mut self, command: crate::pane::SelectionCommand) {
        if let Some(action) = self.pane_mut().request_selection(command) {
            self.action(action);
        }
    }
    fn dismiss_navigation_popup(&mut self) {
        if let Some(mut old) = self.goto.take() {
            old.release_focus(&self.ui_context);
            if let Err(old) = self.engine.defer(old) {
                self.retiring_goto.push(old);
            }
        }
    }
    pub(super) fn open_commands(&mut self) {
        self.dismiss_navigation_popup();
        self.context.release_focus(&self.ui_context);
        self.context.close();
        if let Some(picker) = &mut self.picker {
            picker.release_focus(&self.ui_context);
        }
        self.picker = None;
        if let Some(id) = self.ui_context.memory(|memory| memory.focused()) {
            self.commands.set_return_focus(id);
        }
        self.commands.open();
        crate::keyboard::request_id(&self.ui_context, Id::new("command-palette-input"));
    }
    pub(super) fn open_tab_picker(&mut self) {
        self.dismiss_navigation_popup();
        self.context.release_focus(&self.ui_context);
        self.context.close();
        self.commands.release_focus(&self.ui_context);
        self.commands.close();
        let choices = self
            .tabs
            .iter()
            .filter(|t| {
                self.desk
                    .group_for(t.left.id)
                    .is_some_and(|at| self.desk.groups[at].id == self.desk.focused)
            })
            .flat_map(|t| std::iter::once(&t.left).chain(t.right.iter()))
            .map(|p| crate::picker::Choice {
                id: p.id,
                label: p.title(),
                detail: p.location_text(),
                icon: if p.is_home() {
                    Icon::Home
                } else if p.is_recycle_bin() {
                    Icon::Trash
                } else {
                    Icon::Folder
                },
            })
            .collect();
        self.picker = Some(crate::picker::Picker::new(
            "Tabs",
            "Search open tabs and panels…",
            choices,
        ));
        self.picker_layout = false;
        if let Some(picker) = &mut self.picker {
            if let Some(id) = self.ui_context.memory(|memory| memory.focused()) {
                picker.set_return_focus(id);
            }
        }
        crate::keyboard::request_id(
            &self.ui_context,
            Id::new(("choice-picker", "Tabs")).with("input"),
        );
    }
    pub(super) fn focus_pane(&mut self, id: u64) -> bool {
        if let Some((tab, pane)) = self.tabs.iter().enumerate().find_map(|(i, t)| {
            if t.left.id == id {
                Some((i, 0))
            } else if t.right.as_ref().is_some_and(|p| p.id == id) {
                Some((i, 1))
            } else {
                None
            }
        }) {
            self.active = tab;
            self.active_pane = pane;
            self.desk.focus(id);
            true
        } else {
            false
        }
    }
    pub(super) fn execute_command(&mut self, ctx: &egui::Context, command: Command) {
        if self.pane().is_recycle_bin() && !recycle_ui::command_allowed(command) {
            return;
        }
        if self.pane().is_home() && folder_only(command) {
            self.toast("Open a folder to use this command", false);
            return;
        }
        match command {
            Command::NextFocusRegion => self.cycle_focus_region(ctx, false),
            Command::PreviousFocusRegion => self.cycle_focus_region(ctx, true),
            Command::KeyboardHelp => self.keyboard_help = true,
            Command::Operations => self.operations_open = true,
            Command::Commands => self.open_commands(),
            Command::Back => self.pane_mut().back(),
            Command::Forward => self.pane_mut().forward(),
            Command::Parent => self.pane_mut().up(),
            Command::GoTo => self.open_goto(None),
            Command::EditAddress => self.action(Action::Address),
            Command::Filter => {
                let pane = self.pane_mut();
                pane.focus_filter = true;
                ctx.memory_mut(|m| m.request_focus(Id::new((pane.id, "filter"))));
            }
            Command::ExpandedSearch => self.pane_mut().toggle_recursive(),
            Command::ToggleInspector => self.action(Action::ToggleInspector),
            Command::ToggleSidebar => self.sidebar = !self.sidebar,
            Command::SplitRight => self.action(Action::Split(false)),
            Command::SplitBelow => self.action(Action::Split(true)),
            Command::ClosePanel => self.action(Action::ClosePane),
            Command::NextPanel | Command::PreviousPanel => {
                self.next_workspace(command == Command::PreviousPanel);
                crate::keyboard::request_id(ctx, crate::keyboard::listing_id(self.pane().id));
            }
            Command::NewTab => self.new_current_route_tab(),
            Command::NewWindow => {
                if self.pane().is_home() {
                    self.job(Job::NewHomeWindow);
                } else if self.pane().is_recycle_bin() {
                    self.job(Job::NewWindow(self.pane().location_text().into()));
                } else {
                    self.job(Job::NewWindow(self.pane().path.clone()));
                }
            }
            Command::DuplicateTab => self.duplicate_tab(),
            Command::MoveTabToNewWindow => {
                self.move_tab_to_new_window(self.tabs[self.active].left.id)
            }
            Command::CloseTab => self.close_tab(self.active),
            Command::ReopenTab => self.reopen_tab(),
            Command::SearchTabs => self.open_tab_picker(),
            Command::Refresh => {
                if self.pane().is_home() {
                    self.home.refresh();
                }
                let _ = self.assets.invalidate_visible();
                self.pane_mut().refresh();
            }
            Command::ReloadTheme => self.job(Job::Preview {
                path: self.theme_path.clone(),
                id: u64::MAX,
            }),
            Command::ToggleHidden => {
                let p = self.pane_mut();
                p.query.show_hidden = !p.query.show_hidden;
                p.refilter();
            }
            Command::ViewDetails => self.pane_mut().set_zoom(0.0),
            Command::ViewList => self.pane_mut().set_zoom(0.1),
            Command::ViewColumns => self.pane_mut().set_zoom(0.2),
            Command::ViewMedium => self.pane_mut().set_zoom(0.3),
            Command::ViewLarge => self.pane_mut().set_zoom(0.65),
            Command::ViewExtraLarge => self.pane_mut().set_zoom(1.0),
            Command::Options => self.open_options(ctx),
            Command::Launcher => self.open_launcher(false),
            Command::LauncherSettings => self.open_launcher(true),
            Command::OpenExplorer => self.job(Job::Explorer(self.pane().path.clone())),
            Command::OpenConsole => self.job(Job::Console(self.pane().path.clone())),
            Command::Fullscreen => {
                self.fullscreen = !self.fullscreen;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
            }
            Command::Copy => self.execute_selection(crate::pane::SelectionCommand::Copy),
            Command::Undo => self.job(Job::Undo),
            Command::Redo => self.job(Job::Redo),
            Command::Cut => self.execute_selection(crate::pane::SelectionCommand::Cut),
            Command::Paste => self.action(Action::Paste(self.pane().path.clone())),
            Command::Rename => self.execute_selection(crate::pane::SelectionCommand::Rename),
            Command::Recycle => self.execute_selection(crate::pane::SelectionCommand::Recycle),
            Command::DeletePermanent => self.execute_selection(
                crate::pane::SelectionCommand::Delete(crate::pane::DeleteMode::Permanent),
            ),
            Command::NewFolder => self.action(Action::NewFolder(self.pane().path.clone())),
            Command::NewFile => {
                self.edit = Some(EditDialog {
                    path: self.pane().path.clone(),
                    name: "New file.txt".into(),
                    create: false,
                    create_file: true,
                    focus: true,
                });
            }
            Command::SelectAll => self.pane_mut().select_all(),
            Command::InvertSelection => self.pane_mut().invert_selection(),
            Command::CopyPath => self.execute_selection(crate::pane::SelectionCommand::CopyPaths),
            Command::Properties => {
                self.execute_selection(crate::pane::SelectionCommand::Properties)
            }
            Command::SaveLayout => self.save_layout_name = Some(String::new()),
            Command::LoadLayout => {
                let choices = self
                    .saved_layouts
                    .iter()
                    .enumerate()
                    .map(|(i, s)| crate::picker::Choice {
                        id: i as u64,
                        label: s.name.clone(),
                        detail: format!("{} tabs", s.state.tabs.len()),
                        icon: Icon::Split,
                    })
                    .collect();
                self.picker = Some(crate::picker::Picker::new(
                    "Saved layouts",
                    "Search layouts…",
                    choices,
                ));
                self.picker_layout = true;
                if let Some(picker) = &mut self.picker {
                    if let Some(id) = ctx.memory(|memory| memory.focused()) {
                        picker.set_return_focus(id);
                    }
                }
                crate::keyboard::request_id(
                    ctx,
                    Id::new(("choice-picker", "Saved layouts")).with("input"),
                );
            }
            Command::SaveFolderView => self.save_folder_view(),
            Command::InspectorDetails => {
                if !crate::pane::inspector_available(ctx, &self.theme) {
                    self.toast("Widen the window to show the inspector", false);
                    return;
                }
                self.inspector_details = !self.inspector_details;
                self.inspector = true;
                if let Some(view) = &mut self.inspector_view {
                    view.set_details(self.inspector_details);
                }
            }
            Command::Hotkeys => {
                // Transfer the palette's original invoker before the new
                // surface captures focus during the palette's closing pass.
                self.commands.release_focus(ctx);
                self.commands.close();
                self.hotkeys_open = true;
                self.hotkeys.focus_editor();
            }
        }
    }
    pub(super) fn commands_ui(&mut self, ctx: &egui::Context) {
        if self.hotkeys_open {
            let mut open = true;
            let previous = self.hotkeys.bindings().to_vec();
            let changed = self.hotkeys.editor_popup_ui(ctx, &self.theme, &mut open);
            self.hotkeys_open = open;
            if !open {
                self.hotkeys.cancel_recording();
            }
            if changed {
                self.hotkeys
                    .apply_launcher_change(&mut self.launcher, previous);
                self.hotkeys.update_palette(&mut self.commands);
                self.session_dirty = true;
                self.save_due = Some(Instant::now() + Duration::from_millis(300));
            }
        }
        if !self.commands.finished() {
            let pane = self.pane();
            let availability = [
                (Command::Undo, self.platform.history_state().can_undo()),
                (Command::Redo, self.platform.history_state().can_redo()),
                (Command::Back, pane.navigation.can_back()),
                (Command::Forward, pane.navigation.can_forward()),
                (
                    Command::Parent,
                    !pane.is_home() && pane.path.parent().is_some(),
                ),
                (Command::ClosePanel, self.desk.groups.len() > 1),
                (Command::NextPanel, self.desk.groups.len() > 1),
                (Command::PreviousPanel, self.desk.groups.len() > 1),
                (Command::ReopenTab, !self.closed_tabs.is_empty()),
                (
                    Command::ToggleInspector,
                    crate::pane::inspector_available(ctx, &self.theme),
                ),
                (
                    Command::InspectorDetails,
                    crate::pane::inspector_available(ctx, &self.theme),
                ),
            ];
            for (command, enabled) in availability {
                self.commands.set_enabled(command, enabled);
            }
            for entry in crate::commands::COMMANDS {
                if folder_only(entry.command) && entry.command != Command::Parent {
                    self.commands
                        .set_enabled(entry.command, !self.pane().is_home());
                }
                if self.pane().is_recycle_bin() && !recycle_ui::command_allowed(entry.command) {
                    self.commands.set_enabled(entry.command, false);
                }
            }
            let before = self.commands.bookmarks();
            if let Some(command) = self.commands.ui(ctx, &self.theme) {
                self.execute_command(ctx, command);
            }
            if before != self.commands.bookmarks() {
                self.session_dirty = true;
                self.save_due = Some(Instant::now() + Duration::from_millis(300));
            }
        }
        if let Some(picker) = &mut self.picker {
            let choice = picker.ui(ctx, &self.theme);
            let finished = picker.finished();
            if let Some(id) = choice {
                if self.picker_layout {
                    if let Some(state) =
                        self.saved_layouts.get(id as usize).map(|s| s.state.clone())
                    {
                        self.restore_workspace(state);
                    }
                } else {
                    self.focus_pane(id);
                }
            }
            if finished {
                self.picker = None;
            }
        }
    }
    pub(super) fn context_ui(&mut self, ctx: &egui::Context) {
        if let Some(batch) = &mut self.batch {
            let update = batch.ui(ctx, &self.theme);
            if update.refresh {
                let old = (self.active, self.active_pane);
                if update.origin == Some(0) {
                    for tab in &mut self.tabs {
                        tab.left.refresh();
                        if let Some(right) = &mut tab.right {
                            right.refresh();
                        }
                    }
                } else if update.origin.is_some_and(|id| self.focus_pane(id)) {
                    self.pane_mut().refresh();
                }
                self.active = old.0.min(self.tabs.len() - 1);
                self.active_pane = old.1;
            }
            if let Some(message) = update.message.filter(|m| !m.is_empty()) {
                self.toast(message, false);
            }
            for error in update.errors {
                self.toast(error, true);
            }
        }
        self.context.set_history(self.platform.history_state());
        let update = self.context.ui(ctx, &self.theme);
        if let Some(backwards) = update.tab_exit {
            self.pending_focus_exit = Some((backwards, ctx.cumulative_frame_nr()));
            ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        for error in update.errors {
            self.toast(error, true);
        }
        if update.invoked {
            self.pane_mut().refresh();
        }
        if update.pins_changed {
            self.session_dirty = true;
            self.save_due = Some(Instant::now() + Duration::from_millis(300));
        }
        if let Some(core) = update.core_action {
            if core.origin != 0 && !self.focus_pane(core.origin) {
                self.toast("The command's panel has closed", true);
                return;
            }
            use crate::context::CoreVerb;
            match core.verb {
                CoreVerb::Open => {
                    if let Some(path) = core.paths.into_iter().next() {
                        self.action(Action::GoPath(path));
                        self.resolve_intent = ResolveIntent::Activate;
                    }
                }
                CoreVerb::Copy => self.action(Action::Copy(core.paths, false)),
                CoreVerb::Cut => self.action(Action::Copy(core.paths, true)),
                CoreVerb::Paste => {
                    if core.paths.len() == 1 {
                        if core.background {
                            self.action(Action::Paste(core.paths[0].clone()));
                        } else {
                            self.action(Action::GoPath(core.paths[0].clone()));
                            self.resolve_intent = ResolveIntent::Paste;
                        }
                    } else if !self.pane().is_home() {
                        self.action(Action::Paste(self.pane().path.clone()));
                    } else {
                        self.toast("Open a folder before pasting files", false);
                    }
                }
                CoreVerb::Rename => self.action(Action::RenameMany(core.paths)),
                CoreVerb::Delete => self.action(Action::Recycle(core.paths)),
                CoreVerb::Refresh => {
                    let _ = self.assets.invalidate_visible();
                    self.pane_mut().refresh();
                }
                CoreVerb::Properties => self.job(Job::Properties(core.paths)),
                CoreVerb::Undo => self.job(Job::Undo),
                CoreVerb::Redo => self.job(Job::Redo),
                CoreVerb::NewFolder => {
                    if let Some(parent) = core.paths.into_iter().next() {
                        self.action(Action::NewFolder(parent));
                    }
                }
                CoreVerb::NewFile => {
                    if let Some(parent) = core.paths.into_iter().next() {
                        self.edit = Some(EditDialog {
                            path: parent,
                            name: "New file.txt".into(),
                            create: false,
                            create_file: true,
                            focus: true,
                        });
                    }
                }
            }
        }
    }
}

fn folder_only(command: Command) -> bool {
    matches!(
        command,
        Command::Parent
            | Command::ExpandedSearch
            | Command::OpenExplorer
            | Command::OpenConsole
            | Command::Paste
            | Command::NewFolder
            | Command::NewFile
            | Command::SaveFolderView
            | Command::ToggleHidden
            | Command::ViewDetails
            | Command::ViewList
            | Command::ViewColumns
            | Command::ViewMedium
            | Command::ViewLarge
            | Command::ViewExtraLarge
            | Command::SelectAll
            | Command::InvertSelection
    )
}

#[cfg(test)]
mod tests {
    use super::super::tests::{pump_events, ready_fixture};
    use super::*;

    #[test]
    fn launcher_binding_in_hotkeys_updates_registration_and_both_saved_settings() {
        let (mut app, _, _) = super::super::tests::fixture_app();
        let previous = app.hotkeys.bindings().to_vec();
        let mut modifiers = Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT;
        modifiers.mac_cmd = true;
        app.hotkeys
            .assign(
                crate::hotkeys::Binding::single(Command::Launcher, Key::A, modifiers),
                None,
                false,
            )
            .unwrap();
        app.hotkeys
            .apply_launcher_change(&mut app.launcher, previous);
        app.hotkeys.settle_launcher_change(&mut app.launcher);
        let saved = app.session_snapshot();
        assert_eq!(saved.launcher.hotkey, "Ctrl+Alt+Shift+Win+A");
        assert_eq!(
            app.launcher.native.hotkey_feedback().requested,
            saved.launcher.hotkey
        );
        let bindings: Vec<_> = saved
            .hotkeys
            .unwrap()
            .into_iter()
            .filter(|binding| binding.command == Command::Launcher)
            .collect();
        assert_eq!(bindings.len(), 1);
        assert!(bindings[0].sequence[0].cmd);
        // Removing the only Launcher binding disables the same native shortcut.
        let previous = app.hotkeys.bindings().to_vec();
        let index = previous
            .iter()
            .position(|binding| binding.command == Command::Launcher)
            .unwrap();
        app.hotkeys.remove(index);
        app.hotkeys
            .apply_launcher_change(&mut app.launcher, previous);
        assert!(app.launcher.settings.hotkey.is_empty());
        assert!(app.launcher.native.hotkey_feedback().requested.is_empty());
        assert!(
            !app.session_snapshot()
                .hotkeys
                .unwrap()
                .iter()
                .any(|binding| binding.command == Command::Launcher)
        );
    }

    #[test]
    fn hotkeys_invoked_from_commands_returns_focus_to_the_original_listing() {
        let (mut app, ctx, mut frame) = ready_fixture();
        app.theme.reduced_motion = true;
        let listing = crate::keyboard::listing_id(app.pane().id);
        crate::keyboard::request_id(&ctx, listing);
        pump_events(&mut app, &ctx, frame, vec![]);
        frame += 1;
        app.open_commands();
        app.commands.set_query("Hotkeys");
        for _ in 0..5 {
            pump_events(&mut app, &ctx, frame, vec![]);
            frame += 1;
        }
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        pump_events(&mut app, &ctx, frame, vec![key(Key::Enter)]);
        frame += 1;
        for _ in 0..5 {
            pump_events(&mut app, &ctx, frame, vec![]);
            frame += 1;
        }
        assert!(app.hotkeys_open);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("hotkeys-search"))
        );
        pump_events(&mut app, &ctx, frame, vec![key(Key::Escape)]);
        frame += 1;
        for _ in 0..5 {
            pump_events(&mut app, &ctx, frame, vec![]);
            frame += 1;
        }
        assert!(!app.hotkeys_open);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(listing));
    }
}
