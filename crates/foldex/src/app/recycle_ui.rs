//! Recycle Bin routing and confirmation. Shell work belongs to recycle_bin::Service.
use super::*;
use crate::{
    navigation::{Navigation, Route},
    recycle_bin::{Intent, Operation, Service, Target},
};

struct Confirmation {
    id: Id,
    origin: u64,
    navigation: u64,
    operation: Operation,
    target: Target,
    decision: Option<bool>,
    focus_cancel: bool,
}
struct Context {
    origin: u64,
    navigation: u64,
    anchor: Rect,
    target: Option<Target>,
    menu: foldex_ui::components::MenuState,
}
pub(super) struct RecycleUi {
    service: Service,
    confirmation: Option<Confirmation>,
    context: Option<Context>,
    next_confirmation: u64,
    #[cfg(test)]
    submitted: Vec<(Operation, usize)>,
}
impl RecycleUi {
    pub(super) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            service: Service::new(wake),
            confirmation: None,
            context: None,
            next_confirmation: 0,
            #[cfg(test)]
            submitted: Vec::new(),
        }
    }
    pub(super) fn is_open(&self) -> bool {
        self.confirmation.is_some()
            || self
                .context
                .as_ref()
                .is_some_and(|context| context.menu.active())
    }
}

/// A virtual bin item cannot use any ordinary filesystem action or old folder
/// fallback. Route navigation, settings and window/tab management remain usable.
pub(super) fn action_allowed(action: &Action) -> bool {
    matches!(
        action,
        Action::RecycleBin(_)
            | Action::Address
            | Action::EditPath(_)
            | Action::Navigate(_)
            | Action::Search(_)
            | Action::RemoveSearch(_)
            | Action::GoPath(_)
            | Action::NewTab(_)
            | Action::NewWindow(_)
            | Action::Notify(_)
            | Action::FolderOptions
            | Action::MoveTabToNewWindow
            | Action::Split(_)
            | Action::SplitAt { .. }
            | Action::ClosePane
    )
}

pub(super) fn command_allowed(command: crate::commands::Command) -> bool {
    use crate::commands::Command;
    !matches!(
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
            | Command::Copy
            | Command::Cut
            | Command::Rename
            | Command::CopyPath
            | Command::Undo
            | Command::Redo
            | Command::ToggleInspector
            | Command::InspectorDetails
    )
}

pub(super) fn job_allowed(job: &Job) -> bool {
    !matches!(
        job,
        Job::Open(_)
            | Job::Reveal(_)
            | Job::Explorer(_)
            | Job::Console(_)
            | Job::Properties(_)
            | Job::Copy { .. }
            | Job::DropFiles { .. }
            | Job::Recycle(_)
            | Job::DeletePermanent(_)
            | Job::NewFile { .. }
            | Job::NewFolder { .. }
            | Job::Rename { .. }
            | Job::Undo
            | Job::Redo
            | Job::WriteClipboard { .. }
            | Job::ReadClipboard { .. }
    ) && !matches!(job, Job::Preview { id, .. } if *id != u64::MAX)
}

impl Foldex {
    pub(crate) fn navigate_recycle_bin(&mut self) {
        if !self.pane().is_recycle_bin() {
            self.pane_mut().navigate_recycle_bin(true);
        }
        self.session_dirty = true;
    }
    pub(super) fn new_recycle_tab(&mut self, split: Option<bool>) {
        let mut pane = Pane::dormant(self.next_id, &self.engine, self.pane().path.clone());
        pane.attach_search(self.search.client());
        pane.restore_navigation(Navigation::new(Route::RecycleBin));
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
        if let Some(vertical) = split {
            self.desk.split(id, vertical);
        } else {
            self.desk.add(id);
        }
        self.active = self.tabs.len() - 1;
        self.active_pane = 0;
        self.session_dirty = true;
    }
    pub(super) fn service_recycle(&mut self, ctx: &egui::Context) {
        let visible = self.desk.visible();
        let any = self.explorer_visible
            && self
                .tabs
                .iter()
                .any(|tab| visible.contains(&tab.left.id) && tab.left.is_recycle_bin());
        self.recycle.service.set_visible(any);
        if !any {
            return;
        }
        let refresh = self
            .tabs
            .iter_mut()
            .filter(|tab| visible.contains(&tab.left.id))
            .filter(|tab| tab.left.is_recycle_bin())
            .fold(false, |refresh, tab| {
                std::mem::take(&mut tab.left.recycle_refresh_requested) || refresh
            });
        if refresh && !self.recycle.service.loading {
            self.recycle.service.refresh();
        }
        let views = self.recycle.service.poll();
        for view in views {
            if let Some(pane) = self
                .tabs
                .iter_mut()
                .map(|tab| &mut tab.left)
                .find(|pane| pane.id == view.request.pane && visible.contains(&pane.id))
            {
                pane.apply_recycle_view(view);
            }
        }
        let service = &mut self.recycle.service;
        for pane in self
            .tabs
            .iter_mut()
            .map(|tab| &mut tab.left)
            .filter(|pane| pane.is_recycle_bin() && visible.contains(&pane.id))
        {
            pane.recycle_busy = service.pending || service.loading || service.error.is_some();
            pane.loading = service.loading || pane.snapshot.is_none() && service.error.is_none();
            if let Some(error) = &service.error {
                pane.error = Some(crate::friendly::present(
                    "Couldn't finish that Recycle Bin action",
                    error,
                ));
                pane.loading = false;
            }
            if let Some(model) = service.model.as_ref()
                && let Some(request) = pane.recycle_view_request(model.clone())
            {
                if service.is_inert() {
                    pane.apply_recycle_view(Service::prepare_inert(request));
                } else if !service.request_view(request) {
                    pane.recycle_view_retry();
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
            }
        }
    }
    pub(super) fn recycle_action(&mut self, intent: Intent) {
        if !self.pane().is_recycle_bin() {
            return;
        }
        if let Intent::Context(position) = intent {
            let mut menu = foldex_ui::components::MenuState::default();
            menu.toggle(&self.ui_context);
            self.recycle.context = Some(Context {
                origin: self.pane().id,
                navigation: self.pane().navigation_epoch(),
                anchor: Rect::from_min_size(position, vec2(1.0, 1.0)),
                target: self.pane().recycle_target(false),
                menu,
            });
            self.ui_context.request_repaint();
            return;
        }
        if matches!(intent, Intent::Retry) {
            self.pane_mut().error = None;
            self.recycle.service.refresh();
            return;
        }
        if self.recycle.service.pending
            || self.recycle.service.loading
            || self.recycle.confirmation.is_some()
        {
            return;
        }
        let all = matches!(intent, Intent::Empty | Intent::RestoreAll);
        let Some(target) = self.pane().recycle_target(all) else {
            return;
        };
        match intent {
            Intent::Empty => self.confirm_recycle(Operation::Empty, target),
            Intent::RestoreAll => self.confirm_recycle(Operation::Restore, target),
            Intent::RestoreSelected => self.submit_recycle(Operation::Restore, target),
            Intent::DeleteSelected => self.submit_recycle(Operation::Remove, target),
            Intent::PropertiesSelected => self.submit_recycle(Operation::Properties, target),
            Intent::Context(_) | Intent::Retry => {}
        }
    }
    fn confirm_recycle(&mut self, operation: Operation, target: Target) {
        if target.count() == 0 || self.recycle.confirmation.is_some() {
            return;
        }
        self.recycle.next_confirmation = self.recycle.next_confirmation.wrapping_add(1);
        self.recycle.confirmation = Some(Confirmation {
            id: Id::new(("recycle-confirmation", self.recycle.next_confirmation & 1)),
            origin: self.pane().id,
            navigation: self.pane().navigation_epoch(),
            operation,
            target,
            decision: None,
            focus_cancel: true,
        });
        self.ui_context.request_repaint();
    }
    fn submit_recycle(&mut self, operation: Operation, target: Target) {
        #[cfg(test)]
        if self.recycle.service.is_inert() {
            self.recycle.submitted.push((operation, target.count()));
            return;
        }
        if self
            .recycle
            .service
            .submit(operation, target, self.window_owner)
        {
            for pane in self
                .tabs
                .iter_mut()
                .map(|tab| &mut tab.left)
                .filter(|pane| pane.is_recycle_bin())
            {
                pane.recycle_busy = true;
            }
        }
    }
    #[cfg(test)]
    pub(super) fn catalog_recycle_area(&self) -> Option<Id> {
        self.recycle.confirmation.as_ref().map(|pending| pending.id)
    }
    pub(super) fn recycle_dialog_ui(&mut self, ctx: &egui::Context) {
        self.recycle_context_ui(ctx);
        let Some(pending) = &mut self.recycle.confirmation else {
            return;
        };
        let origin_present = self.explorer_visible
            && self.tabs.iter().any(|tab| {
                tab.left.id == pending.origin
                    && tab.left.is_recycle_bin()
                    && tab.left.navigation_epoch() == pending.navigation
            });
        let composing = crate::keyboard::composing(ctx);
        if !origin_present
            || !composing && crate::commands::take_key(ctx, Modifiers::NONE, Key::Escape)
        {
            pending.decision = Some(false);
        }
        let mut open = true;
        let empty = pending.operation == Operation::Empty;
        let count = pending.target.count();
        let items = if count == 1 {
            "1 item".to_owned()
        } else {
            format!("{count} items")
        };
        let (title, description, label, kind) = if empty {
            (
                "Empty the Recycle Bin?",
                format!("{items} will be permanently deleted. This can't be undone."),
                "Empty Recycle Bin",
                foldex_ui::dialog::Kind::Danger,
            )
        } else {
            (
                "Restore all items?",
                format!("{items} will return to their original locations."),
                "Restore all",
                foldex_ui::dialog::Kind::Primary,
            )
        };
        let id = pending.id;
        let cancel = id.with("cancel");
        let confirm = id.with("confirm");
        use foldex_ui::dialog::{Action, Dialog, Footer, Kind, Size, Text};
        let shown = Dialog::new(id, title, Size::Small.width(&self.theme))
            .interactive(pending.decision.is_none())
            .footer(Footer::new(vec![
                Action::new(cancel, "Cancel", Kind::Secondary),
                Action::new(confirm, label, kind),
            ]))
            .show(ctx, &self.theme, &mut open, |ui| {
                foldex_ui::dialog::text(ui, &self.theme, Text::Secondary, &description);
            });
        match shown.clicked {
            Some(0) => pending.decision = Some(false),
            Some(_) if pending.decision.is_none() && !composing => pending.decision = Some(true),
            _ => {}
        }
        if pending.focus_cancel
            && pending.decision.is_none()
            && ctx
                .read_response(cancel)
                .is_some_and(|response| response.enabled() && response.interact_rect.is_positive())
        {
            ctx.memory_mut(|memory| memory.request_focus(cancel));
            pending.focus_cancel = false;
        }
        if pending.decision.is_some() {
            foldex_ui::components::close_dialog(ctx, id);
        }
        if !open {
            let finished = self.recycle.confirmation.take().unwrap();
            if finished.decision == Some(true) && origin_present {
                self.submit_recycle(finished.operation, finished.target);
            }
        }
    }
    fn recycle_context_ui(&mut self, ctx: &egui::Context) {
        use foldex_ui::components::{MenuItem, MenuSection};
        let Some(context) = &mut self.recycle.context else {
            return;
        };
        let origin_valid = self.tabs.iter().any(|tab| {
            tab.left.id == context.origin
                && tab.left.is_recycle_bin()
                && tab.left.navigation_epoch() == context.navigation
        });
        if !origin_valid {
            self.recycle.context = None;
            return;
        }
        let ready = !self.recycle.service.pending
            && !self.recycle.service.loading
            && self.recycle.service.error.is_none();
        let selected = ready && context.target.is_some();
        let all = ready
            && self
                .recycle
                .service
                .model
                .as_ref()
                .is_some_and(|model| !model.is_empty());
        let entries = [
            (0, "Restore", Icon::Back, selected),
            (1, "Delete permanently", Icon::Trash, selected),
            (2, "Properties", Icon::Inspect, selected),
            (3, "Empty Bin", Icon::Trash, all),
            (4, "Restore All", Icon::Back, all),
            (5, "Refresh", Icon::Refresh, !self.recycle.service.pending),
        ];
        let items = entries
            .iter()
            .map(|&(id, label, glyph, enabled)| MenuItem {
                id,
                label,
                glyph,
                enabled,
                shortcut: "",
                checked: None,
            })
            .collect::<Vec<_>>();
        let selected = context.menu.show_sections(
            ctx,
            Id::new("recycle-context"),
            &self.theme,
            context.anchor,
            &items,
            &[MenuSection::separator(3), MenuSection::separator(5)],
        );
        if let Some(selected) = selected {
            let context = self.recycle.context.take().unwrap();
            match selected {
                0..=2 => {
                    if let Some(target) = context.target {
                        self.submit_recycle(
                            match selected {
                                0 => Operation::Restore,
                                1 => Operation::Remove,
                                _ => Operation::Properties,
                            },
                            target,
                        );
                    }
                }
                3 => self.recycle_action(Intent::Empty),
                4 => self.recycle_action(Intent::RestoreAll),
                5 => self.recycle_action(Intent::Retry),
                _ => {}
            }
        } else if !context.menu.active() {
            self.recycle.context = None;
        }
    }

    /// Safe native GPU capture seed. These are inert presentation records, never
    /// native PIDLs, and Service::inject permanently disables native dispatch.
    pub(crate) fn benchmark_recycle_preview(&mut self, ctx: &egui::Context) {
        self.navigate_recycle_bin();
        self.pane_mut().filter.clear();
        self.pane_mut().query = foldex_core::Query::default();
        self.pane_mut().set_zoom(0.0);
        self.pane_mut().refilter();
        self.recycle.confirmation = None;
        self.recycle.context = None;
        let items = (0..36)
            .map(|index| {
                let name = match index {
                    0 | 1 => "Quarterly report.xlsx".into(),
                    2 => "Design archive".into(),
                    3 => "Meeting notes.md".into(),
                    _ => format!("Deleted document {:02}.txt", index - 3),
                };
                foldex_shell::recycle::Item {
                    id: vec![0xFA, 0xCE, index as u8],
                    original_path: PathBuf::from(if index < 2 {
                        "C:/Users/Preview/Documents"
                    } else {
                        "D:/Projects/Atlas"
                    })
                    .join(&name),
                    name,
                    deleted_at: 1_791_000_000 + index as i64 * 3600,
                    size: 1024 + index as u64 * 1536,
                    is_dir: index == 2 || index == 9,
                    file_system_path: None,
                }
            })
            .collect();
        let model = self.recycle.service.inject(items);
        self.pane_mut().recycle_busy = false;
        if let Some(request) = self.pane_mut().recycle_view_request(model) {
            self.pane_mut()
                .apply_recycle_view(Service::prepare_inert(request));
        }
        ctx.data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
        ctx.request_repaint();
    }
}

#[derive(Clone)]
pub(crate) struct RecycleProbe {
    pub items: usize,
    pub visible: std::ops::Range<usize>,
    pub rows: Vec<Rect>,
    pub address: Rect,
    pub body: Rect,
    pub footer: Rect,
    pub empty: Rect,
    pub restore_all: Rect,
}
pub(crate) fn probe(ctx: &egui::Context, pane: u64) -> Option<RecycleProbe> {
    ctx.data(|data| data.get_temp(Id::new((pane, "recycle-probe"))))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture_app, key_event, pump_events};
    use super::*;
    use crate::commands::Command;

    fn fixture() -> (Foldex, egui::Context) {
        let (mut app, ctx, _) = fixture_app();
        app.theme.reduced_motion = true;
        app.theme.apply(&ctx);
        app.benchmark_recycle_preview(&ctx);
        (app, ctx)
    }
    #[test]
    fn filesystem_actions_and_fallback_folder_commands_are_rejected() {
        let path = PathBuf::from("C:/previous-folder");
        for action in [
            Action::Open(path.clone()),
            Action::Inspect(path.clone()),
            Action::Paste(path.clone()),
            Action::Bookmark(path.clone()),
            Action::NewFolder(path.clone()),
            Action::Copy(vec![path.clone()], false),
            Action::Context {
                paths: vec![path.clone()],
                background: true,
                position: egui::Pos2::ZERO,
            },
        ] {
            assert!(!action_allowed(&action));
        }
        for command in [
            Command::Paste,
            Command::NewFile,
            Command::NewFolder,
            Command::OpenConsole,
            Command::OpenExplorer,
            Command::Undo,
            Command::Redo,
            Command::ExpandedSearch,
            Command::ToggleInspector,
            Command::CopyPath,
        ] {
            assert!(!command_allowed(command));
        }
        for command in [
            Command::Filter,
            Command::Properties,
            Command::ViewLarge,
            Command::SelectAll,
            Command::NewTab,
            Command::Refresh,
        ] {
            assert!(command_allowed(command));
        }
        assert!(!job_allowed(&Job::DropFiles {
            sources: vec![path.clone()],
            destination: path,
            control: false,
            shift: false
        }));
        assert!(!job_allowed(&Job::Undo));
        assert!(action_allowed(&Action::FolderOptions));
        assert!(action_allowed(&Action::MoveTabToNewWindow));
    }
    #[test]
    fn empty_and_restore_all_require_a_decision_and_escape_never_submits() {
        let (mut app, ctx) = fixture();
        let mut at = 0;
        for intent in [Intent::Empty, Intent::RestoreAll] {
            app.recycle_action(intent);
            assert!(app.recycle.confirmation.is_some());
            assert!(app.popup_active());
            assert!(app.recycle.submitted.is_empty());
            for _ in 0..3 {
                pump_events(&mut app, &ctx, at, vec![]).drop_without_applying_deltas();
                at += 1;
            }
            let cancel = app.recycle.confirmation.as_ref().unwrap().id.with("cancel");
            assert_eq!(ctx.memory(|memory| memory.focused()), Some(cancel));
            pump_events(
                &mut app,
                &ctx,
                at,
                vec![key_event(Key::Escape, Modifiers::NONE)],
            )
            .drop_without_applying_deltas();
            at += 1;
            for _ in 0..3 {
                pump_events(&mut app, &ctx, at, vec![]).drop_without_applying_deltas();
                at += 1;
            }
            assert!(app.recycle.confirmation.is_none());
            assert!(app.recycle.submitted.is_empty());
        }
    }
    #[test]
    fn confirmed_bulk_action_is_frozen_and_only_submitted_once_to_the_inert_boundary() {
        let (mut app, ctx) = fixture();
        app.recycle_action(Intent::RestoreAll);
        for at in 0..3 {
            pump_events(&mut app, &ctx, at, vec![]).drop_without_applying_deltas();
        }
        let confirm = app
            .recycle
            .confirmation
            .as_ref()
            .unwrap()
            .id
            .with("confirm");
        ctx.memory_mut(|memory| memory.request_focus(confirm));
        pump_events(
            &mut app,
            &ctx,
            3,
            vec![key_event(Key::Enter, Modifiers::NONE)],
        )
        .drop_without_applying_deltas();
        for at in 4..8 {
            pump_events(&mut app, &ctx, at, vec![]).drop_without_applying_deltas();
        }
        assert_eq!(app.recycle.submitted, [(Operation::Restore, 36)]);
        assert!(app.recycle.confirmation.is_none());
    }
    #[test]
    fn session_restore_and_route_duplication_keep_bin_virtual_and_lazy() {
        let (mut app, ctx) = fixture();
        let serialized = serde_json::to_vec(&app.workspace_state()).unwrap();
        let restored = serde_json::from_slice(&serialized).unwrap();
        app.restore_workspace(restored);
        assert!(app.pane().is_recycle_bin());
        assert_eq!(
            app.pane().location_text(),
            crate::navigation::RECYCLE_BIN_ADDRESS
        );
        assert!(app.pane().snapshot.is_none());
        app.pane_mut().prepare_view(&ctx);
        app.new_current_route_tab();
        assert!(app.pane().is_recycle_bin());
        app.pane_mut().back();
        assert!(app.pane().is_recycle_bin());
        assert!(app.recycle.submitted.is_empty());
    }
}
