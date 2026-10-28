//! Custom searchable host for the retained Windows context-menu worker.
//! App owns opening targets/HWND and refresh/error handling; this module owns
//! popup input, search, pinning, lazy hierarchy and bounded visible-item motion.

use crate::commands::{SurfaceFocus, take_key};
use crate::operation_history::HistoryState;
use egui::{Align2, Color32, Id, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
#[cfg(test)]
use foldex_shell::BuiltinItem;
use foldex_shell::{
    BuiltinEvent, BuiltinKind, BuiltinMenus, BuiltinSnapshot, Event, MenuOptions, ShellItem,
    ShellMenu,
};
use foldex_ui::{
    Icon, Theme, icon,
    motion::{LayoutMotion, MotionGains, MotionProfile},
};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

const WIDTH: f32 = 272.0;
const HEADER: f32 = foldex_ui::components::POPUP_HEADER;
const MAX_PINS: usize = 64;
const MAX_PANELS: usize = 7;
const APP_ACTIONS: u32 = 0xe000_0000;
const APP_NEW: u32 = 0xe100_0000;
const APP_COMPRESS: u32 = 0xe200_0000;
const LOADING_COMMANDS: &str = "Loading…";

#[derive(Default)]
pub struct ContextUpdate {
    /// A native verb completed; refresh the affected directory as appropriate.
    pub invoked: bool,
    pub errors: Vec<String>,
    pub pins_changed: bool,
    pub truncated: bool,
    pub core_action: Option<CoreAction>,
    /// A New menu template is being created in this directory, for the
    /// origin pane, which then opens the new item's name editor.
    pub new_template: Option<(u64, PathBuf)>,
    /// A menu Tab dismissal: true requests previous root control, false next.
    /// Root routing should apply this after drawing/closing the popup.
    pub tab_exit: Option<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreVerb {
    Open,
    Copy,
    Cut,
    Paste,
    Rename,
    Delete,
    Refresh,
    Properties,
    Undo,
    Redo,
    NewFolder,
    NewFile,
}
pub struct CoreAction {
    pub verb: CoreVerb,
    pub origin: u64,
    /// Original selection, or one captured destination directory for New.
    pub paths: Vec<PathBuf>,
    /// Creation commands carry their destination as a background target.
    pub background: bool,
}

pub struct ContextMenu {
    shell: ShellMenu,
    system: BuiltinMenus,
    builtin_cache: [Option<Arc<BuiltinSnapshot>>; 2],
    history: HistoryState,
    popup: Option<Popup>,
    pending: Option<Pending>,
    pending_close: Option<u64>,
    sequence: u64,
    pins: HashSet<String>,
    pins_changed: bool,
}

impl ContextMenu {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Ok(Self {
            shell: ShellMenu::new(wake.clone())?,
            system: BuiltinMenus::new(wake)?,
            builtin_cache: Default::default(),
            history: Default::default(),
            popup: None,
            pending: None,
            pending_close: None,
            sequence: 0,
            pins: HashSet::new(),
            pins_changed: false,
        })
    }

    pub fn open(&mut self, owner: isize, paths: Vec<PathBuf>, background: bool, position: Pos2) {
        self.open_for_pane(0, owner, paths, background, position);
    }

    pub fn open_for_pane(
        &mut self,
        origin: u64,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
        position: Pos2,
    ) {
        self.open_internal(
            origin,
            owner,
            paths,
            background,
            position,
            MenuOptions::default(),
        );
    }

    pub fn open_for_pane_with_options(
        &mut self,
        origin: u64,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
        position: Pos2,
        options: MenuOptions,
    ) {
        self.open_internal(origin, owner, paths, background, position, options);
    }

    pub fn open_with_options(
        &mut self,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
        position: Pos2,
        options: MenuOptions,
    ) {
        self.open_internal(0, owner, paths, background, position, options);
    }

    fn open_internal(
        &mut self,
        origin: u64,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
        position: Pos2,
        options: MenuOptions,
    ) {
        if let Some(old) = &self.popup {
            self.pending_close = Some(old.request);
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.pending = Some(Pending {
            request: self.sequence,
            owner,
            paths: paths.clone(),
            background,
            options,
        });
        self.popup = Some(Popup::new(
            self.sequence,
            position,
            origin,
            paths,
            background,
        ));
        if let Some(popup) = &mut self.popup {
            popup.owner = owner;
            for kind in [BuiltinKind::New, BuiltinKind::Compress] {
                if let Some(snapshot) = &self.builtin_cache[builtin_index(kind)] {
                    popup.replace_builtin(kind, snapshot);
                }
            }
        }
    }

    /// Capture the open pane's directory, including multi-folder search
    /// selections. None disables New on Home/virtual backgrounds.
    pub fn set_new_directory(&mut self, directory: Option<PathBuf>) {
        if let Some(popup) = &mut self.popup {
            popup.new_directory = directory;
            for row in popup
                .flat
                .iter_mut()
                .filter(|row| row.id == APP_NEW || row.parent == Some(APP_NEW))
            {
                row.enabled = popup.new_directory.is_some();
            }
            popup.dirty = true;
        }
    }

    pub fn set_history(&mut self, state: HistoryState) {
        self.history = state;
    }

    pub fn is_open(&self) -> bool {
        self.popup.is_some()
    }

    /// Use the invoking composite/control ID for keyboard context-menu opens.
    /// The caller supplies its selected-row or background anchor to `open_for_pane`.
    pub fn set_return_focus(&mut self, id: Id) {
        if let Some(popup) = &mut self.popup {
            popup.focus_scope.set_return_focus(id);
        }
    }

    pub fn release_focus(&mut self, ctx: &egui::Context) {
        if let Some(popup) = &mut self.popup {
            let members = popup.focus_members();
            popup
                .focus_scope
                .update(ctx, Id::new("context-search"), &members, false);
        }
    }

    pub(crate) fn benchmark_more_actions(&mut self) {
        if let Some(popup) = &mut self.popup {
            popup.show_submenu(0, APP_ACTIONS, popup.panels[0].anchor);
        }
    }

    /// Read-only screenshot setup. New takes an owned fixture directory;
    /// Compress takes an owned selected sample path. This only opens a panel;
    /// `ui` discovers its definitions through the ordinary bounded worker.
    pub(crate) fn benchmark_open_builtin(
        &mut self,
        ctx: &egui::Context,
        target: PathBuf,
        kind: BuiltinKind,
    ) -> bool {
        if !target.is_absolute() || target.as_os_str().as_encoded_bytes().contains(&0) {
            return false;
        }
        let position = ctx.content_rect().center() - vec2(WIDTH, HEADER);
        self.open(0, vec![target], kind == BuiltinKind::New, position);
        let opened = self.popup.as_mut().is_some_and(|popup| {
            popup.show_submenu(
                0,
                builtin_parent(kind),
                position + vec2(WIDTH - 2.0, HEADER),
            )
        });
        ctx.request_repaint();
        opened
    }

    pub(crate) fn benchmark_builtin_state(
        &self,
        kind: BuiltinKind,
    ) -> Result<Option<usize>, String> {
        let Some(popup) = &self.popup else {
            return Ok(None);
        };
        if let Some(error) = &popup.builtin_errors[builtin_index(kind)] {
            return Err(error.clone());
        }
        Ok((!popup.builtin_loading[builtin_index(kind)]).then(|| {
            popup
                .flat
                .iter()
                .filter(|row| row.parent == Some(builtin_parent(kind)) && !row.separator)
                .count()
        }))
    }

    pub(crate) fn benchmark_native_state(&self) -> Result<Option<usize>, String> {
        let Some(popup) = &self.popup else {
            return Ok(None);
        };
        if let Some(error) = &popup.error {
            return Err(error.clone());
        }
        Ok((!popup.loading).then(|| {
            popup
                .flat
                .iter()
                .filter(|row| row.id < APP_ACTIONS && !row.separator)
                .count()
        }))
    }

    pub fn close(&mut self) {
        if let Some(popup) = &mut self.popup {
            popup.closing = true;
            self.pending_close = Some(popup.request);
        }
        self.pending = None;
    }

    pub fn set_pins(&mut self, pins: &[String]) {
        self.pins = pins
            .iter()
            .filter(|key| key.len() <= 1024)
            .take(MAX_PINS)
            .cloned()
            .collect();
        if let Some(popup) = &mut self.popup {
            popup.dirty = true;
        }
    }

    pub fn pins(&self) -> Vec<String> {
        let mut pins: Vec<_> = self.pins.iter().cloned().collect();
        pins.sort();
        pins
    }

    pub fn ui(&mut self, ctx: &egui::Context, theme: &Theme) -> ContextUpdate {
        let mut update = ContextUpdate::default();
        if let Some(request) = self.pending_close {
            match self.shell.close(request) {
                Ok(()) => self.pending_close = None,
                Err(error) if error.contains("mailbox busy") => {
                    ctx.request_repaint_after(Duration::from_millis(16));
                }
                Err(error) => {
                    update.errors.push(error);
                    self.pending_close = None;
                }
            }
        }
        // Basic commands never initialize installed extensions. Hovering/opening
        // More actions or searching explicitly opts into the native menu lane.
        if let Some(pending) = self
            .pending
            .as_ref()
            .filter(|_| self.popup.as_ref().is_some_and(Popup::wants_native))
        {
            match self.shell.open_with_options(
                pending.request,
                pending.owner,
                pending.paths.clone(),
                pending.background,
                pending.options,
            ) {
                Ok(()) => self.pending = None,
                Err(error) if error.contains("mailbox busy") => {
                    ctx.request_repaint_after(Duration::from_millis(16))
                }
                Err(error) => {
                    if let Some(popup) = &mut self.popup {
                        popup.error = Some(crate::friendly::present(
                            "Couldn't load more actions",
                            &error,
                        ));
                        popup.loading = false;
                    }
                    update.errors.push(error);
                    self.pending = None;
                }
            }
        }
        while let Some(event) = self.shell.try_recv() {
            match event {
                Event::Ready {
                    request,
                    items,
                    truncated,
                }
                | Event::Updated {
                    request,
                    items,
                    truncated,
                } => {
                    if let Some(popup) = &mut self.popup {
                        if popup.request == request && !popup.closing {
                            popup.replace_native(&items);
                            popup.loading = false;
                            popup.error = None;
                            popup.truncated = truncated;
                            popup.dirty = true;
                        }
                    }
                }
                Event::Failed { request, message } => {
                    if let Some(popup) = self
                        .popup
                        .as_mut()
                        .filter(|popup| popup.request == request && !popup.closing)
                    {
                        // A provider/load failure belongs in More actions. It
                        // must not interrupt a perfectly usable core command.
                        popup.error = Some(crate::friendly::present(
                            "Couldn't load more actions",
                            &message,
                        ));
                        popup.loading = false;
                    } else {
                        // A committed command can complete after the popup has
                        // closed; its invocation failures must remain visible.
                        update.errors.push(message);
                    }
                }
                Event::Invoked { .. } => update.invoked = true,
            }
        }
        while let Some(event) = self.system.try_recv() {
            match event {
                BuiltinEvent::Ready { kind, snapshot } => {
                    self.builtin_cache[builtin_index(kind)] = Some(snapshot.clone());
                    if let Some(popup) = &mut self.popup {
                        if !popup.closing {
                            popup.replace_builtin(kind, &snapshot);
                        }
                    }
                }
                BuiltinEvent::DiscoveryFailed { kind, message } => {
                    if let Some(popup) = &mut self.popup {
                        popup.builtin_failed(kind, message);
                    }
                }
                BuiltinEvent::Invoked { .. } => update.invoked = true,
                BuiltinEvent::Failed { message, .. } => update.errors.push(message),
            }
        }
        let Some(popup) = &mut self.popup else {
            return update;
        };
        popup.update_history(self.history);
        for kind in [BuiltinKind::New, BuiltinKind::Compress] {
            if popup.wants_builtin(kind) && !popup.builtin_requested[builtin_index(kind)] {
                let (paths, background) = popup.builtin_target(kind);
                match self.system.discover(kind, popup.owner, paths, background) {
                    Ok(snapshot) => {
                        popup.builtin_requested[builtin_index(kind)] = true;
                        if let Some(snapshot) = snapshot {
                            popup.replace_builtin(kind, &snapshot);
                        }
                    }
                    Err(error) if error.contains("mailbox busy") => {
                        ctx.request_repaint_after(Duration::from_millis(16));
                    }
                    Err(error) => {
                        popup.builtin_requested[builtin_index(kind)] = true;
                        popup.builtin_failed(kind, error);
                    }
                }
            }
        }
        let opening_frame = *popup.opened_frame.get_or_insert(ctx.cumulative_frame_nr())
            == ctx.cumulative_frame_nr();
        let mut focus_members = popup.focus_members();
        popup.focus_scope.update(
            ctx,
            Id::new("context-search"),
            &focus_members,
            !popup.closing,
        );
        for &id in focus_members.iter().skip(1) {
            popup
                .focus_scope
                .redirect_text(ctx, id, Id::new("context-search"));
        }
        update.truncated = popup.truncated;
        let gains = MotionGains::new(ctx.input(|input| input.stable_dt), theme.duration());
        popup.layout.set_pixels_per_point(ctx.pixels_per_point());
        popup
            .layout
            .begin_frame_with_gains(foldex_ui::motion::layout_gains(ctx, theme.duration()));
        if popup.dirty {
            popup.rebuild(&self.pins);
        }

        if ctx.input(|input| input.pointer.delta().length_sq() > 0.0 || input.pointer.any_pressed())
        {
            popup.hover_paused = false;
        }
        let mut activation = if popup.closing {
            None
        } else {
            popup
                .pending_invoke
                .take()
                .map(|id| (popup.focus_panel, id))
        };
        let mut close = false;
        let mut exit_direction = None;
        let mut keyboard_pin = None;
        let mut keyboard_navigation = false;
        if !popup.closing && popup.focus_scope.accepts_keys(ctx, &focus_members) {
            let backwards = take_key(ctx, egui::Modifiers::SHIFT, Key::Tab);
            if backwards || take_key(ctx, egui::Modifiers::NONE, Key::Tab) {
                close = true;
                popup.focus_search = false;
                exit_direction = Some(if backwards {
                    egui::FocusDirection::Previous
                } else {
                    egui::FocusDirection::Next
                });
            }
            let results_focus = ctx.memory(|memory| {
                memory
                    .focused()
                    .is_some_and(|id| focus_members[1..].contains(&id))
            });
            if take_key(ctx, egui::Modifiers::NONE, Key::Escape) {
                activation = None;
                close = !popup.close_deepest();
                if !close {
                    popup.focus_search = false;
                    keyboard_navigation = true;
                }
            }
            if (results_focus || popup.query.is_empty())
                && popup.panels.len() > 1
                && take_key(ctx, egui::Modifiers::NONE, Key::ArrowLeft)
            {
                activation = None;
                popup.close_deepest();
                popup.focus_search = false;
                keyboard_navigation = true;
            }
            let header = if popup.focus_panel == 0 {
                theme.metrics().popup_header
            } else {
                5.0 * theme.metrics().scale
            };
            let page = ((popup.panels[popup.focus_panel].height - header)
                / foldex_ui::components::menu_row_height(theme))
            .floor()
            .max(1.0) as isize;
            for (key, delta) in [
                (Key::ArrowDown, 1isize),
                (Key::ArrowUp, -1),
                (Key::PageDown, page),
                (Key::PageUp, -page),
            ] {
                if take_key(ctx, egui::Modifiers::NONE, key) {
                    activation = None;
                    popup.move_cursor(delta);
                    popup.focus_search = false;
                    keyboard_navigation = true;
                }
            }
            if (results_focus || popup.query.is_empty())
                && take_key(ctx, egui::Modifiers::NONE, Key::Home)
            {
                popup.select_edge(false);
                popup.focus_search = false;
                keyboard_navigation = true;
            }
            if (results_focus || popup.query.is_empty())
                && take_key(ctx, egui::Modifiers::NONE, Key::End)
            {
                popup.select_edge(true);
                popup.focus_search = false;
                keyboard_navigation = true;
            }
            if take_key(ctx, egui::Modifiers::NONE, Key::Enter)
                || ((results_focus || keyboard_navigation)
                    && take_key(ctx, egui::Modifiers::NONE, Key::Space))
            {
                activation = popup.selected.map(|id| (popup.focus_panel, id));
            }
            if (results_focus || keyboard_navigation)
                && take_key(ctx, egui::Modifiers::CTRL, Key::Space)
            {
                keyboard_pin = popup
                    .selected
                    .and_then(|id| {
                        popup
                            .flat
                            .iter()
                            .find(|row| row.id == id && !row.submenu && row.enabled)
                    })
                    .map(|row| row.pin.clone());
            }
            if (results_focus || popup.query.is_empty())
                && popup
                    .selected
                    .is_some_and(|id| popup.flat.iter().any(|row| row.id == id && row.submenu))
                && take_key(ctx, egui::Modifiers::NONE, Key::ArrowRight)
            {
                activation = popup.selected.map(|id| (popup.focus_panel, id));
            }
            if keyboard_navigation && !close {
                ctx.memory_mut(|memory| {
                    memory.request_focus(popup.panel_focus_id(popup.focus_panel))
                });
            }
        }
        if popup.dirty {
            popup.rebuild(&self.pins);
        }
        if !popup.query.is_empty() && !popup.closing {
            // Progressive native subtree discovery supports searching actions
            // that have not yet been hovered. Enumeration never invokes them.
            for row in &popup.flat {
                if row.submenu
                    && row.enabled
                    && !row.separator
                    && row.id < APP_ACTIONS
                    && popup.expanded.insert(row.id)
                {
                    popup.pending_expands.push_back(row.id);
                }
            }
        }
        for _ in 0..4 {
            let Some(id) = popup.pending_expands.front().copied() else {
                break;
            };
            match self.shell.expand(popup.request, id) {
                Ok(()) => {
                    popup.pending_expands.pop_front();
                }
                Err(error) if error.contains("busy") || error.contains("queue is full") => {
                    ctx.request_repaint_after(Duration::from_millis(24));
                    break;
                }
                Err(_) => {
                    popup.pending_expands.pop_front();
                }
            }
        }

        let mut result = PanelResult::default();
        result.pin = keyboard_pin;
        let mut submenu_hover = None;
        let mut rects = Vec::with_capacity(popup.panels.len());
        let mut moving = false;
        for level in 0..popup.panels.len() {
            let child_parent = popup
                .panels
                .get(level + 1)
                .filter(|panel| !panel.closing)
                .and_then(|panel| panel.parent);
            let panel_closing = popup.closing || popup.panels[level].closing;
            let panel = &mut popup.panels[level];
            let builtin = match panel.parent {
                Some(APP_NEW) => Some(BuiltinKind::New),
                Some(APP_COMPRESS) => Some(BuiltinKind::Compress),
                _ => None,
            };
            let loading = builtin.map_or(popup.loading, |kind| {
                popup.builtin_loading[builtin_index(kind)]
            });
            let error = builtin.map_or(popup.error.as_deref(), |kind| {
                popup.builtin_errors[builtin_index(kind)].as_deref()
            });
            let panel_result = draw_panel(
                ctx,
                theme,
                popup.request,
                level,
                panel,
                &mut popup.query,
                &mut popup.focus_search,
                &mut popup.flat,
                popup.selected,
                loading,
                error,
                panel_closing,
                &self.pins,
                &mut popup.layout,
                &mut popup.hovers,
                gains,
                popup.scroll_selected,
                child_parent,
                opening_frame,
                popup.focus_panel == level,
            );
            if let Some(anchor) = panel_result.child_anchor {
                if let Some(child) = popup.panels.get_mut(level + 1) {
                    child.anchor = anchor;
                    child.parent_left = panel_result.rect.left();
                }
            }
            rects.push(panel_result.rect);
            moving |= panel_result.moving;
            if panel_result.query_changed {
                popup.dirty = true;
                popup.panels.truncate(1);
                popup.focus_panel = 0;
                popup.hover_paused = true;
                popup.hover_intent = None;
                activation = None;
                result.pin = None;
                break;
            }
            if let Some(selected) = panel_result.selected {
                popup.selected = Some(selected);
                popup.focus_panel = level;
                let leaf = popup
                    .flat
                    .iter()
                    .any(|row| row.id == selected && !row.submenu);
                if leaf
                    && popup
                        .panels
                        .get(level + 1)
                        .is_some_and(|child| !child.closing && child.parent != Some(selected))
                {
                    if !popup
                        .close_intent
                        .is_some_and(|(old_level, old, _)| old_level == level && old == selected)
                    {
                        popup.close_intent = Some((level, selected, Instant::now()));
                    }
                } else {
                    popup.close_intent = None;
                }
            }
            if let Some(id) = panel_result.activation {
                // Hover may already have moved keyboard focus into the child.
                // A click still belongs to the panel that produced it.
                activation = Some((level, id));
            }
            if panel_result.pin.is_some() {
                result.pin = panel_result.pin;
            }
            if let Some(hover) = panel_result.submenu_hover {
                submenu_hover = Some((level, hover.0, hover.1));
            }
        }
        popup.scroll_selected = false;
        if !opening_frame && ctx.input(|input| input.pointer.any_pressed()) {
            if let Some(position) = ctx.input(|input| input.pointer.interact_pos()) {
                if !rects.iter().any(|rect| rect.contains(position)) {
                    close = true;
                }
            }
        }
        if close {
            activation = None;
        }
        if let Some(key) = result.pin {
            if !self.pins.remove(&key) && self.pins.len() < MAX_PINS {
                self.pins.insert(key);
            }
            self.pins_changed = true;
            popup.dirty = true;
        }
        if let Some((level, id)) = activation {
            if let Some(row) = popup.flat.iter().find(|row| {
                row.id == id
                    && row.enabled
                    && !row.separator
                    && popup.panels.get(level).is_some_and(|panel| {
                        !panel.closing && panel.rows.iter().any(|index| popup.flat[*index].id == id)
                    })
            }) {
                if row.submenu {
                    let anchor = rects.get(level).map_or(popup.panels[0].anchor, |rect| {
                        pos2(
                            rect.right() - 2.0,
                            rect.top() + theme.metrics().popup_header,
                        )
                    });
                    if popup.show_submenu(level, id, anchor) && popup.focus_panel != level + 1 {
                        popup.focus_panel = level + 1;
                        popup.dirty = true;
                    }
                    popup.focus_search = false;
                    ctx.memory_mut(|memory| {
                        memory.request_focus(popup.panel_focus_id(popup.focus_panel))
                    });
                } else {
                    if let Some(verb) = row.core {
                        let creating = matches!(verb, CoreVerb::NewFolder | CoreVerb::NewFile);
                        update.core_action = Some(CoreAction {
                            verb,
                            origin: popup.origin,
                            // Creation targets the captured pane directory,
                            // including when the menu came from search results.
                            paths: if creating {
                                popup.new_directory.iter().cloned().collect()
                            } else {
                                popup.paths.clone()
                            },
                            background: creating || popup.background,
                        });
                        popup.closing = true;
                        self.pending = None;
                        self.pending_close = Some(popup.request);
                    } else {
                        let invocation = if let Some((kind, key)) = &row.builtin {
                            let (paths, background) = popup.builtin_target(*kind);
                            if *kind == BuiltinKind::New
                                && key.starts_with("new:template:")
                                && let Some(directory) = &popup.new_directory
                            {
                                update.new_template = Some((popup.origin, directory.clone()));
                            }
                            self.system.invoke(
                                popup.request,
                                *kind,
                                popup.owner,
                                paths,
                                background,
                                key.clone(),
                            )
                        } else {
                            self.shell.invoke(popup.request, id)
                        };
                        match invocation {
                            Ok(()) => {
                                popup.closing = true;
                                self.pending = None;
                                self.pending_close = Some(popup.request);
                            }
                            Err(error) if error.contains("mailbox busy") => {
                                popup.pending_invoke = Some(id);
                                ctx.request_repaint_after(Duration::from_millis(16));
                            }
                            Err(error) => update.errors.push(error),
                        }
                    }
                }
            }
        }
        if popup
            .blocked_hover
            .is_some_and(|blocked| submenu_hover.is_none_or(|(_, id, _)| id != blocked))
        {
            popup.blocked_hover = None;
        }
        if let Some((level, id, anchor)) = submenu_hover.filter(|(_, id, _)| {
            activation.is_none() && !popup.hover_paused && popup.blocked_hover != Some(*id)
        }) {
            let same = popup
                .hover_intent
                .is_some_and(|(old, _, old_level)| old == id && old_level == level);
            if !same {
                popup.hover_intent = Some((id, Instant::now(), level));
            }
            if popup
                .hover_intent
                .is_some_and(|(_, start, _)| start.elapsed() >= Duration::from_millis(110))
            {
                popup.show_submenu(level, id, anchor);
            } else {
                ctx.request_repaint_after(Duration::from_millis(110));
            }
        } else if ctx.input(|input| input.pointer.delta().length_sq() > 0.0) {
            popup.hover_intent = None;
        }

        if let Some((level, id, start)) = popup.close_intent {
            let pointer = ctx.input(|input| input.pointer.hover_pos());
            let in_parent = pointer
                .is_some_and(|point| rects.get(level).is_some_and(|rect| rect.contains(point)));
            let in_child = pointer.is_some_and(|point| {
                rects
                    .iter()
                    .skip(level + 1)
                    .any(|rect| rect.contains(point))
            });
            if !in_parent || in_child || popup.selected != Some(id) || popup.focus_panel != level {
                popup.close_intent = None;
            } else if start.elapsed() >= Duration::from_millis(110) {
                popup.close_descendants(level);
                ctx.request_repaint();
            } else {
                ctx.request_repaint_after(
                    Duration::from_millis(110).saturating_sub(start.elapsed()),
                );
            }
        }
        while popup.panels.len() > 1
            && popup
                .panels
                .last()
                .is_some_and(|panel| panel.closing && panel.opacity <= 0.01 && panel.height <= 0.25)
        {
            popup.panels.pop();
        }
        if close {
            popup.closing = true;
            self.pending = None;
            self.pending_close = Some(popup.request);
        }
        focus_members = popup.focus_members();
        popup.focus_scope.update(
            ctx,
            Id::new("context-search"),
            &focus_members,
            !popup.closing,
        );
        if let Some(direction) = exit_direction {
            ctx.memory_mut(|memory| memory.move_focus(direction));
        }
        update.tab_exit =
            exit_direction.map(|direction| direction == egui::FocusDirection::Previous);
        if moving || popup.layout.is_moving() || popup.dirty || popup.closing {
            ctx.request_repaint();
        }
        let finished = popup.closing && popup.panels.iter().all(|panel| panel.opacity <= 0.01);
        if finished {
            self.popup = None;
        }
        update.pins_changed = std::mem::take(&mut self.pins_changed);
        update
    }
}

struct Pending {
    request: u64,
    owner: isize,
    paths: Vec<PathBuf>,
    background: bool,
    options: MenuOptions,
}
struct Popup {
    request: u64,
    owner: isize,
    new_directory: Option<PathBuf>,
    builtin_requested: [bool; 2],
    builtin_loading: [bool; 2],
    builtin_errors: [Option<String>; 2],
    builtin_ids: [HashMap<String, u32>; 2],
    next_builtin_id: [u32; 2],
    query: String,
    flat: Vec<Row>,
    panels: Vec<Panel>,
    selected: Option<u32>,
    focus_panel: usize,
    focus_search: bool,
    loading: bool,
    error: Option<String>,
    dirty: bool,
    closing: bool,
    opened_frame: Option<u64>,
    truncated: bool,
    expanded: HashSet<u32>,
    pending_expands: VecDeque<u32>,
    pending_invoke: Option<u32>,
    hover_intent: Option<(u32, Instant, usize)>,
    close_intent: Option<(usize, u32, Instant)>,
    blocked_hover: Option<u32>,
    hover_paused: bool,
    layout: LayoutMotion,
    hovers: HashMap<Id, f32>,
    scroll_selected: bool,
    origin: u64,
    paths: Vec<PathBuf>,
    background: bool,
    focus_scope: SurfaceFocus,
}
impl Popup {
    fn new(
        request: u64,
        position: Pos2,
        origin: u64,
        paths: Vec<PathBuf>,
        background: bool,
    ) -> Self {
        let new_directory = if background {
            paths.first().cloned()
        } else {
            paths
                .first()
                .and_then(|path| path.parent())
                .filter(|parent| paths.iter().all(|path| path.parent() == Some(*parent)))
                .map(std::path::Path::to_path_buf)
        };
        let can_create = new_directory.is_some();
        let mut flat = root_rows(background, paths.len());
        for row in flat
            .iter_mut()
            .filter(|row| row.id == APP_NEW || row.parent == Some(APP_NEW))
        {
            row.enabled = can_create;
        }
        Self {
            request,
            owner: 0,
            new_directory,
            builtin_requested: [false; 2],
            builtin_loading: [true; 2],
            builtin_errors: Default::default(),
            builtin_ids: [
                [
                    ("new:folder".into(), APP_NEW + 0x100),
                    ("new:shortcut".into(), APP_NEW + 0x101),
                    ("new:text".into(), APP_NEW + 0x102),
                ]
                .into_iter()
                .collect(),
                HashMap::new(),
            ],
            next_builtin_id: [APP_NEW + 0x103, APP_COMPRESS + 0x100],
            query: String::new(),
            flat,
            panels: vec![Panel::new(None, position)],
            selected: None,
            focus_panel: 0,
            focus_search: true,
            loading: true,
            error: None,
            dirty: true,
            closing: false,
            opened_frame: None,
            truncated: false,
            expanded: HashSet::new(),
            pending_expands: VecDeque::new(),
            pending_invoke: None,
            hover_intent: None,
            close_intent: None,
            blocked_hover: None,
            hover_paused: false,
            layout: LayoutMotion::default(),
            hovers: HashMap::new(),
            scroll_selected: false,
            origin,
            paths,
            background,
            focus_scope: Default::default(),
        }
    }
    fn panel_focus_id(&self, level: usize) -> Id {
        Id::new(("context-menu-focus", self.request, level))
    }
    fn focus_members(&self) -> Vec<Id> {
        std::iter::once(Id::new("context-search"))
            .chain((0..self.panels.len()).map(|level| self.panel_focus_id(level)))
            .collect()
    }
    fn select_edge(&mut self, last: bool) {
        let rows = &self.panels[self.focus_panel].rows;
        let selected = if last {
            rows.iter()
                .rev()
                .map(|index| &self.flat[*index])
                .find(|row| row.enabled && !row.separator)
        } else {
            rows.iter()
                .map(|index| &self.flat[*index])
                .find(|row| row.enabled && !row.separator)
        };
        self.selected = selected.map(|row| row.id);
        self.scroll_selected = true;
    }
    fn wants_native(&self) -> bool {
        !self.closing
            && (!self.query.trim().is_empty()
                || self
                    .panels
                    .iter()
                    .any(|panel| panel.parent == Some(APP_ACTIONS) && !panel.closing))
    }

    fn wants_builtin(&self, kind: BuiltinKind) -> bool {
        !self.closing
            && (kind != BuiltinKind::New || self.new_directory.is_some())
            && (kind != BuiltinKind::Compress || !self.background && !self.paths.is_empty())
            && (!self.query.trim().is_empty()
                || self
                    .panels
                    .iter()
                    .any(|panel| panel.parent == Some(builtin_parent(kind)) && !panel.closing))
    }

    fn builtin_target(&self, kind: BuiltinKind) -> (Vec<PathBuf>, bool) {
        if kind == BuiltinKind::New {
            (self.new_directory.iter().cloned().collect(), true)
        } else {
            (self.paths.clone(), self.background)
        }
    }

    fn replace_builtin(&mut self, kind: BuiltinKind, snapshot: &BuiltinSnapshot) {
        let parent = builtin_parent(kind);
        let at = builtin_index(kind);
        self.flat.retain(|row| {
            row.parent != Some(parent)
                || matches!(row.core, Some(CoreVerb::NewFolder | CoreVerb::NewFile))
                || row
                    .builtin
                    .as_ref()
                    .is_some_and(|(kind, key)| *kind == BuiltinKind::New && key == "new:shortcut")
        });
        let mut items = Vec::new();
        for item in snapshot.items.iter().take(foldex_shell::MAX_ITEMS) {
            // The standard Folder/.txt commands belong to Foldex's tracked
            // mutation pipeline. A native catalogue must never replace them
            // with Shell invocations that cannot produce an inverse record.
            if builtin_core(kind, &item.key).is_some()
                || kind == BuiltinKind::New && item.key == "new:shortcut"
            {
                continue;
            }
            let id = if let Some(id) = self.builtin_ids[at].get(&item.key) {
                *id
            } else {
                if self.builtin_ids[at].len() >= foldex_shell::MAX_ITEMS * 2 {
                    self.truncated = true;
                    continue;
                }
                let id = self.next_builtin_id[at];
                self.next_builtin_id[at] += 1;
                self.builtin_ids[at].insert(item.key.clone(), id);
                id
            };
            items.push(ShellItem {
                id,
                label: item.label.clone(),
                canonical_verb: Some(format!("foldex-{}", item.key)),
                enabled: kind != BuiltinKind::New || self.new_directory.is_some(),
                ..Default::default()
            });
        }
        let mut rows = flatten(&items);
        for row in &mut rows {
            row.parent = Some(parent);
            row.path = if kind == BuiltinKind::New {
                "New"
            } else {
                "Compress to"
            }
            .into();
            row.search = format!("{} {}", row.path, row.label).to_lowercase();
            row.builtin = row
                .verb
                .as_deref()
                .and_then(|verb| verb.strip_prefix("foldex-"))
                .map(|key| (kind, key.to_owned()));
        }
        self.flat.extend(rows);
        self.builtin_loading[builtin_index(kind)] = false;
        self.builtin_errors[builtin_index(kind)] = None;
        self.truncated |= snapshot.truncated;
        self.dirty = true;
    }

    fn builtin_failed(&mut self, kind: BuiltinKind, error: String) {
        let error = crate::friendly::present("Couldn't load the Windows menu items", &error);
        if kind == BuiltinKind::New {
            if let Some(row) = self.flat.iter_mut().find(|row| row.id == APP_NEW + 0xf000) {
                row.label = "Windows templates unavailable".into();
                row.search = row.label.to_lowercase();
                row.help = error.clone();
                row.galley = None;
            }
        }
        self.builtin_errors[builtin_index(kind)] = Some(error);
        self.builtin_loading[builtin_index(kind)] = false;
        self.dirty = true;
    }

    fn update_history(&mut self, state: HistoryState) {
        for row in &mut self.flat {
            let (label, enabled) = match row.core {
                Some(CoreVerb::Undo) => (state.undo_label(), state.can_undo()),
                Some(CoreVerb::Redo) => (state.redo_label(), state.can_redo()),
                _ => continue,
            };
            if row.label != label || row.enabled != enabled {
                row.label = label;
                row.search = row.label.to_lowercase();
                row.enabled = enabled;
                row.galley = None;
                self.dirty = true;
            }
        }
    }

    fn replace_native(&mut self, items: &[ShellItem]) {
        // Preserve cached system commands and live history labels when the
        // separate installed-app snapshot arrives or lazily expands.
        let mut rows = std::mem::take(&mut self.flat);
        rows.retain(|row| row.id >= APP_ACTIONS);
        let mut native = flatten(items);
        // Keep Foldex's immediately available commands and their stable IDs.
        // Nested Open-with handlers retain their native IDs and invocation.
        native.retain(|row| row.core.is_none());
        for row in &mut native {
            if row.parent.is_none() {
                row.parent = Some(APP_ACTIONS);
            }
        }
        // Native providers may leave separators behind when core verbs are
        // removed. Normalize each sibling group without changing its order.
        let mut previous: HashMap<Option<u32>, bool> = HashMap::new();
        native.retain(|row| {
            let separator = previous.entry(row.parent).or_insert(true);
            let keep = !row.separator || !*separator;
            if keep {
                *separator = row.separator;
            }
            keep
        });
        let mut last: HashMap<Option<u32>, usize> = HashMap::new();
        for (index, row) in native.iter().enumerate() {
            last.insert(row.parent, index);
        }
        let trailing: HashSet<_> = last
            .values()
            .copied()
            .filter(|index| native[*index].separator)
            .collect();
        rows.extend(
            native
                .into_iter()
                .enumerate()
                .filter_map(|(index, row)| (!trailing.contains(&index)).then_some(row)),
        );
        self.flat = rows;
        let submenus: HashSet<_> = self
            .flat
            .iter()
            .filter(|row| row.submenu && row.enabled && !row.separator && row.id < APP_ACTIONS)
            .map(|row| row.id)
            .collect();
        self.expanded.retain(|id| submenus.contains(id));
        self.pending_expands.retain(|id| submenus.contains(id));
        self.reconcile_panels();
        self.dirty = true;
    }
    fn submenu_belongs_to(&self, level: usize, id: u32) -> bool {
        self.panels.get(level).is_some_and(|panel| {
            self.flat.iter().any(|row| {
                row.id == id
                    && row.submenu
                    && row.enabled
                    && !row.separator
                    && (row.parent == panel.parent
                        || (level == 0
                            && !self.query.trim().is_empty()
                            && fuzzy_score(&row.search, &self.query.trim().to_lowercase())
                                .is_some()))
            })
        })
    }
    fn reconcile_panels(&mut self) {
        let keep = (1..self.panels.len())
            .find(|&level| {
                self.panels[level].parent.is_none_or(|id| {
                    !self.submenu_belongs_to(level - 1, id)
                        || self.panels[..level]
                            .iter()
                            .any(|panel| panel.parent == Some(id))
                })
            })
            .unwrap_or(self.panels.len());
        if keep < self.panels.len() {
            self.panels.truncate(keep);
            self.hover_intent = None;
            self.close_intent = None;
            self.dirty = true;
        }
        let deepest = self
            .panels
            .iter()
            .rposition(|panel| !panel.closing)
            .unwrap_or(0);
        self.focus_panel = self.focus_panel.min(deepest);
    }
    fn rebuild(&mut self, pins: &HashSet<String>) {
        self.reconcile_panels();
        let query = self.query.trim().to_lowercase();
        for (level, panel) in self.panels.iter_mut().enumerate() {
            panel.rows.clear();
            panel.content_width = None;
            if level == 0 && !query.is_empty() {
                let mut ranked: Vec<_> = self
                    .flat
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| !row.separator)
                    .filter_map(|(index, row)| {
                        fuzzy_score(&row.search, &query).map(|score| {
                            (index, score + if pins.contains(&row.pin) { 12 } else { 0 })
                        })
                    })
                    .collect();
                ranked.sort_by_key(|(index, score)| (std::cmp::Reverse(*score), *index));
                panel
                    .rows
                    .extend(ranked.into_iter().map(|(index, _)| index));
            } else {
                panel.rows.extend(
                    self.flat
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| row.parent == panel.parent)
                        .map(|(index, _)| index),
                );
                panel
                    .rows
                    .sort_by_key(|index| (!pins.contains(&self.flat[*index].pin), *index));
                let mut separator = true;
                panel.rows.retain(|index| {
                    let next = self.flat[*index].separator;
                    let keep = !next || !separator;
                    if keep {
                        separator = next;
                    }
                    keep
                });
                while panel
                    .rows
                    .last()
                    .is_some_and(|index| self.flat[*index].separator)
                {
                    panel.rows.pop();
                }
            }
        }
        self.focus_panel = self.focus_panel.min(self.panels.len().saturating_sub(1));
        let visible = &self.panels[self.focus_panel].rows;
        if self.selected.is_none_or(|id| {
            !visible
                .iter()
                .map(|index| &self.flat[*index])
                .any(|row| row.id == id && row.enabled && !row.separator)
        }) {
            self.selected = visible
                .iter()
                .map(|index| &self.flat[*index])
                .find(|row| row.enabled && !row.separator)
                .map(|row| row.id);
        }
        for row in &mut self.flat {
            row.galley = None;
        }
        self.dirty = false;
    }
    fn move_cursor(&mut self, delta: isize) {
        self.focus_panel = self.focus_panel.min(self.panels.len().saturating_sub(1));
        let candidates: Vec<_> = self.panels[self.focus_panel]
            .rows
            .iter()
            .map(|index| &self.flat[*index])
            .filter(|row| row.enabled && !row.separator)
            .map(|row| row.id)
            .collect();
        if candidates.is_empty() {
            self.selected = None;
            return;
        }
        let current = self
            .selected
            .and_then(|id| candidates.iter().position(|candidate| *candidate == id));
        let index = current.map_or(
            if delta < 0 {
                candidates.len() as isize - 1
            } else {
                0
            },
            |at| {
                if delta.abs() > 1 {
                    (at as isize + delta).clamp(0, candidates.len() as isize - 1)
                } else {
                    (at as isize + delta).rem_euclid(candidates.len() as isize)
                }
            },
        );
        self.selected = Some(candidates[index as usize]);
        self.scroll_selected = true;
    }
    fn close_descendants(&mut self, level: usize) {
        for panel in self.panels.iter_mut().skip(level + 1) {
            panel.closing = true;
        }
        self.hover_intent = None;
        self.close_intent = None;
    }
    fn close_deepest(&mut self) -> bool {
        let level = self
            .panels
            .iter()
            .rposition(|panel| !panel.closing)
            .unwrap_or(0);
        if level == 0 {
            return false;
        }
        let parent = self.panels[level].parent;
        self.blocked_hover = parent;
        self.close_descendants(level - 1);
        self.focus_panel = level - 1;
        self.selected = parent;
        self.focus_search = true;
        true
    }
    // `level` is the panel containing the row, independent of keyboard focus.
    // Repeated hover only updates the anchor; explicit activation owns focus.
    fn show_submenu(&mut self, level: usize, id: u32, anchor: Pos2) -> bool {
        if self.closing
            || level >= MAX_PANELS - 1
            || !self.submenu_belongs_to(level, id)
            || self.panels[level].closing
            || self.panels[..=level]
                .iter()
                .any(|panel| panel.parent == Some(id))
        {
            return false;
        }
        self.close_intent = None;
        self.blocked_hover = None;
        if id < APP_ACTIONS && self.expanded.insert(id) {
            self.pending_expands.push_back(id);
        }
        let parent_left = self.panels[level].visual.x;
        if let Some(panel) = self.panels.get_mut(level + 1) {
            panel.parent_left = parent_left;
            panel.anchor = anchor;
            let same = panel.parent == Some(id);
            if same {
                let reopened = panel.closing;
                panel.closing = false;
                if reopened {
                    panel.scroll_reset = true;
                    self.focus_panel = level + 1;
                    self.dirty = true;
                }
                return true;
            }
            panel.parent = Some(id);
            panel.closing = false;
            panel.rows.clear();
            panel.content_width = None;
            panel.scroll_reset = true;
            self.panels.truncate(level + 2);
        } else {
            let mut panel = Panel::new(Some(id), anchor);
            panel.parent_left = parent_left;
            self.panels.push(panel);
        }
        self.focus_panel = level + 1;
        self.dirty = true;
        true
    }
}

struct Panel {
    parent: Option<u32>,
    anchor: Pos2,
    visual: Pos2,
    height: f32,
    opacity: f32,
    placed: bool,
    closing: bool,
    parent_left: f32,
    content_width: Option<(u32, u32, usize, f32)>,
    scroll_reset: bool,
    rows: Vec<usize>,
}
impl Panel {
    fn new(parent: Option<u32>, anchor: Pos2) -> Self {
        Self {
            parent,
            anchor,
            visual: anchor + vec2(0.0, -4.0),
            height: 0.0,
            opacity: 0.0,
            placed: false,
            closing: false,
            parent_left: anchor.x,
            content_width: None,
            scroll_reset: true,
            rows: Vec::new(),
        }
    }
}
struct Row {
    id: u32,
    parent: Option<u32>,
    label: String,
    path: String,
    search: String,
    pin: String,
    help: String,
    shortcut: Option<String>,
    enabled: bool,
    separator: bool,
    submenu: bool,
    checked: bool,
    verb: Option<String>,
    core: Option<CoreVerb>,
    builtin: Option<(BuiltinKind, String)>,
    galley: Option<(u64, Arc<egui::Galley>)>,
}

fn flatten(items: &[ShellItem]) -> Vec<Row> {
    fn visit(items: &[ShellItem], parent: Option<u32>, ancestry: &str, rows: &mut Vec<Row>) {
        for item in items.iter().take(512) {
            if rows.len() >= 512 {
                break;
            }
            let path = if ancestry.is_empty() {
                item.label.clone()
            } else {
                format!("{ancestry} › {}", item.label)
            };
            let pin = format!(
                "{ancestry}/{}",
                item.canonical_verb.as_deref().unwrap_or(&item.label)
            );
            let core = if item.has_submenu || parent.is_some() {
                None
            } else {
                core_verb(item.canonical_verb.as_deref())
            };
            rows.push(Row {
                id: core.map_or(item.id, core_id),
                parent,
                label: item.label.clone(),
                search: path.to_lowercase(),
                path: ancestry.to_owned(),
                pin,
                help: item.help.clone(),
                shortcut: item.shortcut.clone(),
                enabled: item.enabled,
                separator: item.separator,
                submenu: item.has_submenu,
                checked: item.checked,
                verb: item.canonical_verb.clone(),
                core,
                builtin: None,
                galley: None,
            });
            visit(&item.children, Some(item.id), &path, rows);
        }
    }
    let mut rows = Vec::with_capacity(items.len().min(512));
    visit(items, None, "", &mut rows);
    rows
}

fn fuzzy_score(text: &str, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let mut score = 0i32;
    for token in query.split_whitespace() {
        if let Some(at) = text.find(token) {
            score += 180 + token.len() as i32 * 8 - at.min(80) as i32;
            continue;
        }
        let mut needle = token.chars();
        let mut next = needle.next();
        let mut previous = None;
        for (at, character) in text.chars().enumerate() {
            if Some(character) == next {
                score += if previous.is_some_and(|last| last + 1 == at) {
                    14
                } else {
                    3
                };
                previous = Some(at);
                next = needle.next();
                if next.is_none() {
                    break;
                }
            }
        }
        if next.is_some() {
            return None;
        }
    }
    Some(score)
}

struct PanelResult {
    rect: Rect,
    moving: bool,
    query_changed: bool,
    selected: Option<u32>,
    activation: Option<u32>,
    pin: Option<String>,
    submenu_hover: Option<(u32, Pos2)>,
    child_anchor: Option<Pos2>,
}

impl Default for PanelResult {
    fn default() -> Self {
        Self {
            rect: Rect::NOTHING,
            moving: false,
            query_changed: false,
            selected: None,
            activation: None,
            pin: None,
            submenu_hover: None,
            child_anchor: None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_panel(
    ctx: &egui::Context,
    theme: &Theme,
    request: u64,
    level: usize,
    panel: &mut Panel,
    query: &mut String,
    focus_search: &mut bool,
    rows: &mut [Row],
    selected: Option<u32>,
    loading: bool,
    error: Option<&str>,
    closing: bool,
    pins: &HashSet<String>,
    layout: &mut LayoutMotion,
    hovers: &mut HashMap<Id, f32>,
    gains: MotionGains,
    scroll_selected: bool,
    child_parent: Option<u32>,
    opening_frame: bool,
    active_panel: bool,
) -> PanelResult {
    let m = theme.metrics();
    let scale = m.scale;
    let searching = level == 0 && !query.trim().is_empty();
    let header = if level == 0 {
        m.popup_header
    } else {
        5.0 * scale
    };
    let row_height = foldex_ui::components::menu_row_height(theme);
    let content_height: f32 = panel
        .rows
        .iter()
        .map(|index| {
            if rows[*index].separator {
                7.0 * scale
            } else if searching && !rows[*index].path.is_empty() {
                row_height + 13.0 * scale
            } else {
                row_height
            }
        })
        .sum();
    let empty_height = if panel.rows.is_empty() {
        row_height + 6.0 * scale
    } else {
        0.0
    };
    let mut screen = ctx.content_rect().shrink(5.0);
    screen.min.y = screen.min.y.max(ctx.content_rect().top() + 36.0);
    let target_height = (header + content_height + empty_height + 5.0 * scale)
        .min(460.0 * scale)
        .min(screen.height().max(1.0));
    let mut result = PanelResult::default();
    let geometry = foldex_ui::motion::popup_gains(ctx, theme.duration());
    result.moving |= geometry.step_pixels(
        &mut panel.height,
        if closing { 0.0 } else { target_height },
        MotionProfile::Fast,
    );
    result.moving |= gains.step_normalized(
        &mut panel.opacity,
        if closing { 0.0 } else { 1.0 },
        MotionProfile::Standard,
    );
    let base_width = (WIDTH * scale).min(screen.width().max(1.0));
    let width = if level == 0 {
        base_width
    } else {
        let font_key = theme.font_size.to_bits();
        let ppp = ctx.pixels_per_point().to_bits();
        let fonts = foldex_ui::fonts::revision(ctx);
        let measured = if let Some((_, _, _, width)) =
            panel.content_width.filter(|(font, pixels, revision, _)| {
                *font == font_key && *pixels == ppp && *revision == fonts
            }) {
            width
        } else {
            let width = ctx.fonts_mut(|fonts| {
                panel
                    .rows
                    .iter()
                    .filter_map(|index| rows.get(*index))
                    .filter(|row| !row.separator)
                    .map(|row| {
                        let label = fonts
                            .layout_no_wrap(row.label.clone(), theme.font(), theme.fg())
                            .size()
                            .x;
                        let shortcut = row.shortcut.as_ref().map_or(0.0, |shortcut| {
                            fonts
                                .layout_no_wrap(
                                    shortcut.clone(),
                                    egui::FontId::proportional((theme.font_size - 1.0).max(10.0)),
                                    theme.dim(),
                                )
                                .size()
                                .x
                                + 18.0 * scale
                        });
                        label + shortcut + 66.0 * scale
                    })
                    .fold(160.0 * scale, f32::max)
            });
            panel.content_width = Some((font_key, ppp, fonts, width));
            width
        };
        measured.min(base_width)
    };
    let x = if level == 0 || panel.anchor.x + width <= screen.right() {
        panel.anchor.x
    } else if panel.parent_left - width + 2.0 >= screen.left() {
        panel.parent_left - width + 2.0
    } else {
        screen.right() - width
    };
    let target = pos2(
        x.clamp(screen.left(), (screen.right() - width).max(screen.left())),
        panel.anchor.y.clamp(
            screen.top(),
            (screen.bottom() - target_height).max(screen.top()),
        ),
    );
    if level == 0 {
        // Once a root popup has moved upward to fit, filtering must keep its
        // search header in place instead of returning to the original click.
        panel.anchor = target;
    }
    if !panel.placed {
        // Clamp the opening geometry before introducing the small entrance
        // offset; edge menus should not fly hundreds of pixels into view.
        panel.visual = target - vec2(0.0, 4.0);
        panel.placed = true;
    }
    result.moving |= geometry.step_pixels(&mut panel.visual.x, target.x, MotionProfile::Standard);
    result.moving |= geometry.step_pixels(&mut panel.visual.y, target.y, MotionProfile::Standard);
    let rect = Rect::from_min_size(panel.visual, vec2(width, panel.height.max(1.0)));
    result.rect = rect;
    if level > 0 {
        // Clicking the root Area raises its full-screen modal backdrop. Keep
        // every child above that blocker, including children already open on
        // hover. egui supports one sublayer level, so all descendants attach
        // directly to the root rather than forming a nested layer chain.
        ctx.set_sublayer(
            egui::LayerId::new(
                egui::Order::Foreground,
                Id::new(("foldex-native-context", 0_usize)),
            ),
            egui::LayerId::new(
                egui::Order::Foreground,
                Id::new(("foldex-native-context", level)),
            ),
        );
    }
    foldex_ui::components::popup_surface_with_backdrop(
        ctx,
        egui::Area::new(Id::new(("foldex-native-context", level)))
            .order(egui::Order::Foreground)
            .constrain(false)
            .fixed_pos(panel.visual)
            .movable(false)
            .fade_in(false),
        !closing,
        level == 0,
        |ui| {
            ui.set_min_size(rect.size());
            ui.set_max_size(rect.size());
            ui.set_clip_rect(rect.intersect(screen));
            ui.set_opacity(panel.opacity);
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            ui.painter()
                .rect_filled(rect, theme.radius, Theme::color(theme.surface));
            ui.painter().rect_stroke(
                rect,
                theme.radius,
                Stroke::new(1.0, theme.line()),
                StrokeKind::Inside,
            );
            let panel_focus_id = Id::new(("context-menu-focus", request, level));
            foldex_ui::components::PopupFocus::surface(ui, rect, panel_focus_id, "Context menu");
            ctx.accesskit_node_builder(panel_focus_id, |node| {
                node.set_role(egui::accesskit::Role::Menu)
            });
            if level == 0 {
                let search = Rect::from_min_size(
                    rect.min + vec2(6.0 * scale, 5.0 * scale),
                    vec2(width - 12.0 * scale, m.control),
                );
                let input_id = Id::new("context-search");
                let acquire_focus = *focus_search && !closing;
                if acquire_focus && !ui.memory(|memory| memory.has_focus(input_id)) {
                    ui.memory_mut(|memory| memory.request_focus(input_id));
                }
                let response = foldex_ui::components::search_input(
                    ui,
                    search,
                    input_id,
                    theme,
                    query,
                    "Select a system command…",
                    1024,
                )
                .response;
                if acquire_focus {
                    *focus_search = ui.is_sizing_pass() || !response.has_focus() || opening_frame;
                }
                result.query_changed = response.changed();
                ui.painter().line_segment(
                    [
                        pos2(rect.left() + 6.0 * scale, rect.top() + m.popup_header - 1.0),
                        pos2(
                            rect.right() - 6.0 * scale,
                            rect.top() + m.popup_header - 1.0,
                        ),
                    ],
                    Stroke::new(1.0, theme.line()),
                );
            }
            let body = Rect::from_min_size(
                pos2(rect.left() + 4.0 * scale, rect.top() + header),
                vec2(
                    (width - 8.0 * scale).max(1.0),
                    (target_height - header - 4.0 * scale).max(0.0),
                ),
            );
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(body)
                    .accessibility_parent(panel_focus_id),
                |ui| {
                    ui.set_clip_rect(body.intersect(rect).intersect(screen));
                    // The layout uses its settled body height while the outer
                    // surface reveals it. Entrance motion must not create a
                    // temporary scrollbar or squeeze the empty/loading row.
                    let mut scroll = foldex_ui::components::menu_scroll_area_for_content(
                        ui,
                        theme,
                        ("context-rows", level),
                        (target_height - header - 4.0 * scale).max(1.0),
                        content_height.max(empty_height),
                    );
                    if panel.scroll_reset {
                        scroll = scroll.vertical_scroll_offset(0.0);
                        if !ui.is_sizing_pass() {
                            panel.scroll_reset = false;
                        }
                    }
                    scroll.show(ui, |ui| {
                        let origin = ui.cursor().min;
                        ui.allocate_space(vec2(
                            ui.available_width(),
                            content_height.max(empty_height),
                        ));
                        if panel.rows.is_empty() {
                            let message = error.unwrap_or(if loading {
                                LOADING_COMMANDS
                            } else if searching {
                                "No matching commands"
                            } else {
                                "No commands available"
                            });
                            let message_rect = Rect::from_min_size(
                                origin + vec2(30.0 * scale, 0.0),
                                vec2((width - 46.0 * scale).max(1.0), row_height),
                            );
                            let color = if error.is_some() {
                                Theme::color(theme.danger)
                            } else {
                                theme.dim()
                            };
                            let galley =
                                ui.painter()
                                    .layout_no_wrap(message.into(), theme.font(), color);
                            let text_rect = Rect::from_min_size(
                                pos2(
                                    message_rect.left(),
                                    message_rect.center().y - galley.size().y * 0.5,
                                ),
                                galley.size(),
                            );
                            // Reveal the short loading label as a whole when its
                            // line fits the animated surface. Partial glyphs look
                            // like a clipped or broken message during expansion.
                            if !loading
                                || error.is_some()
                                || ui.clip_rect().contains_rect(text_rect)
                            {
                                foldex_ui::components::elided_label(
                                    ui.painter(),
                                    message_rect,
                                    theme,
                                    message,
                                    color,
                                    false,
                                );
                            }
                            let hint = ui.interact(
                                message_rect,
                                Id::new(("context-empty", level)),
                                Sense::hover(),
                            );
                            hint.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Label,
                                    ui.is_enabled(),
                                    message,
                                )
                            });
                            if error.is_some() {
                                hint.on_hover_text(message);
                            }
                        }
                        let mut y = 0.0;
                        for index in panel.rows.iter().copied() {
                            let row = &mut rows[index];
                            let height = if row.separator {
                                7.0 * scale
                            } else if searching && !row.path.is_empty() {
                                row_height + 13.0 * scale
                            } else {
                                row_height
                            };
                            let target = Rect::from_min_size(
                                pos2(0.0, y),
                                vec2(width - 8.0 * scale, height),
                            );
                            y += height;
                            let id = Id::new(("context-motion-row", request, level, row.id));
                            let placement = layout.place(id, target, true);
                            let visual = placement.rect.translate(origin.to_vec2());
                            if child_parent == Some(row.id) {
                                result.child_anchor =
                                    Some(pos2(rect.right() - 2.0, visual.top() - 5.0 * scale));
                            }
                            if !ui.is_rect_visible(visual.expand(32.0)) {
                                continue;
                            }
                            if row.separator {
                                ui.painter().line_segment(
                                    [
                                        visual.left_center() + vec2(6.0 * scale, 0.0),
                                        visual.right_center() - vec2(6.0 * scale, 0.0),
                                    ],
                                    Stroke::new(1.0, theme.line()),
                                );
                                continue;
                            }
                            let action_rect = Rect::from_min_max(
                                visual.min,
                                pos2(visual.right() - 22.0 * scale, visual.bottom()),
                            );
                            let response = ui.interact(
                                action_rect,
                                id,
                                if closing {
                                    Sense::hover()
                                } else {
                                    Sense::CLICK
                                },
                            );
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    ui.is_enabled() && row.enabled,
                                    &row.label,
                                )
                            });
                            ctx.accesskit_node_builder(id, |node| {
                                node.set_role(if row.checked {
                                    egui::accesskit::Role::MenuItemCheckBox
                                } else {
                                    egui::accesskit::Role::MenuItem
                                });
                                if row.submenu {
                                    node.set_expanded(child_parent == Some(row.id));
                                }
                                if row.checked {
                                    node.set_toggled(egui::accesskit::Toggled::True);
                                }
                            });
                            if active_panel
                                && selected == Some(row.id)
                                && visual.intersects(ui.clip_rect())
                            {
                                for owner in [panel_focus_id, Id::new("context-search")] {
                                    ctx.accesskit_node_builder(owner, |node| {
                                        node.set_active_descendant(id.accesskit_id())
                                    });
                                }
                            }
                            if response.hovered()
                                && ctx.input(|input| input.pointer.delta().length_sq() > 0.0)
                            {
                                result.selected = Some(row.id);
                            }
                            if response.clicked() && row.enabled {
                                result.activation = Some(row.id);
                            }
                            if response.hovered()
                                && row.submenu
                                && row.enabled
                                && !searching
                                && !closing
                            {
                                result.submenu_hover = Some((
                                    row.id,
                                    pos2(rect.right() - 2.0, visual.top() - 5.0 * scale),
                                ));
                            }
                            if !row.help.trim().is_empty()
                                && !row.help.trim().eq_ignore_ascii_case(row.label.trim())
                            {
                                response.clone().on_hover_text(&row.help);
                            }
                            let value = hovers.entry(id).or_insert(0.0);
                            let active = row.enabled
                                && (response.hovered()
                                    || selected == Some(row.id)
                                    || child_parent == Some(row.id));
                            result.moving |= gains.step_normalized(
                                value,
                                if active { 1.0 } else { 0.0 },
                                MotionProfile::Fast,
                            );
                            let mut painter = ui.painter().clone();
                            painter.multiply_opacity(placement.opacity);
                            if *value > 0.0 {
                                painter.rect_filled(
                                    visual.shrink(0.5),
                                    3,
                                    Theme::color(theme.hover).gamma_multiply(*value),
                                );
                            }
                            foldex_ui::components::focus_ring(
                                ui,
                                visual,
                                id.with("cursor"),
                                theme,
                                active_panel
                                    && selected == Some(row.id)
                                    && ctx.memory(|memory| memory.has_focus(panel_focus_id)),
                            );
                            let color = if row.enabled { theme.fg() } else { theme.dim() };
                            let cy = visual.top() + row_height * 0.5;
                            if row.checked {
                                icon(
                                    &painter,
                                    pos2(visual.left() + 16.0 * scale, cy),
                                    Icon::Check,
                                    color,
                                    m.icon,
                                );
                            } else if let Some(action_icon) = verb_icon(row.verb.as_deref()) {
                                icon(
                                    &painter,
                                    pos2(visual.left() + 16.0 * scale, cy),
                                    action_icon,
                                    color,
                                    m.icon,
                                );
                            }
                            let label_width = (width
                                - if row.shortcut.is_some() {
                                    119.0 * scale
                                } else {
                                    66.0 * scale
                                })
                            .max(20.0);
                            let galley = label_galley(ui, row, query, theme, label_width, color);
                            painter.galley(
                                pos2(visual.left() + 30.0 * scale, cy - galley.size().y * 0.5),
                                galley,
                                color,
                            );
                            if searching && !row.path.is_empty() {
                                let caption = Theme {
                                    font_size: (theme.font_size - 1.5).max(10.0),
                                    ..theme.clone()
                                };
                                let detail = Rect::from_min_max(
                                    pos2(
                                        visual.left() + 30.0 * scale,
                                        visual.top() + row_height - 5.0 * scale,
                                    ),
                                    pos2(
                                        (visual.right() - 30.0 * scale)
                                            .max(visual.left() + 30.0 * scale),
                                        visual.bottom() - 2.0 * scale,
                                    ),
                                );
                                foldex_ui::components::elided_label(
                                    &painter,
                                    detail,
                                    &caption,
                                    &row.path,
                                    theme.dim(),
                                    false,
                                );
                            }
                            if let Some(shortcut) = &row.shortcut {
                                painter.text(
                                    pos2(visual.right() - 29.0 * scale, cy),
                                    Align2::RIGHT_CENTER,
                                    shortcut,
                                    egui::FontId::proportional((theme.font_size - 1.0).max(10.0)),
                                    theme.dim(),
                                );
                            }
                            if row.submenu {
                                icon(
                                    &painter,
                                    pos2(visual.right() - 11.0 * scale, cy),
                                    Icon::Forward,
                                    theme.dim().lerp_to_gamma(theme.fg(), *value),
                                    10.0 * scale,
                                );
                            } else {
                                let pin_rect = Rect::from_min_max(
                                    pos2(visual.right() - 22.0 * scale, visual.top()),
                                    visual.max,
                                );
                                let pin_response = ui.interact(
                                    pin_rect,
                                    id.with("pin"),
                                    if closing {
                                        Sense::hover()
                                    } else {
                                        Sense::CLICK
                                    },
                                );
                                let pinned = pins.contains(&row.pin);
                                pin_response.widget_info(|| {
                                    egui::WidgetInfo::selected(
                                        egui::WidgetType::Checkbox,
                                        ui.is_enabled(),
                                        pinned,
                                        format!("Pin {}", row.label),
                                    )
                                });
                                let visible = foldex_ui::animated_state(
                                    ctx,
                                    id.with("pin-visible"),
                                    pinned || response.hovered() || pin_response.hovered(),
                                    theme,
                                    MotionProfile::Fast,
                                );
                                let filled = foldex_ui::animated_state(
                                    ctx,
                                    id.with("pin-filled"),
                                    pinned,
                                    theme,
                                    MotionProfile::Standard,
                                );
                                let highlight = foldex_ui::animated_state(
                                    ctx,
                                    id.with("pin-hover"),
                                    pin_response.hovered(),
                                    theme,
                                    MotionProfile::Fast,
                                );
                                if visible > 0.0 {
                                    let center = pos2(pin_rect.center().x, cy);
                                    icon(
                                        &painter,
                                        center,
                                        Icon::StarOutline,
                                        theme
                                            .dim()
                                            .lerp_to_gamma(theme.fg(), highlight)
                                            .gamma_multiply(visible * (1.0 - filled)),
                                        m.icon,
                                    );
                                    if filled > 0.0 {
                                        icon(
                                            &painter,
                                            center,
                                            Icon::Star,
                                            theme.accent().gamma_multiply(visible * filled),
                                            m.icon,
                                        );
                                    }
                                }
                                pin_response.clone().on_hover_text(if pinned {
                                    "Unpin command · Ctrl+Space in menu"
                                } else {
                                    "Pin command · Ctrl+Space in menu"
                                });
                                if pin_response.clicked() {
                                    result.pin = Some(row.pin.clone());
                                }
                            }
                            if scroll_selected && selected == Some(row.id) {
                                ui.scroll_to_rect(
                                    target.translate(origin.to_vec2()),
                                    Some(egui::Align::Center),
                                );
                            }
                        }
                    });
                },
            );
        },
    );
    result
}

fn label_galley(
    ui: &Ui,
    row: &mut Row,
    query: &str,
    theme: &Theme,
    width: f32,
    color: Color32,
) -> Arc<egui::Galley> {
    foldex_ui::fonts::request_text(ui.ctx(), &row.label);
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    (
        width.to_bits(),
        theme.font_size.to_bits(),
        foldex_ui::fonts::revision(ui.ctx()),
        ui.ctx().pixels_per_point().to_bits(),
        color,
        theme.accent(),
        query,
    )
        .hash(&mut hash);
    let key = hash.finish();
    if let Some((old, galley)) = &row.galley {
        if *old == key {
            return Arc::clone(galley);
        }
    }
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let mut wanted = query
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase);
    let mut next = wanted.next();
    for character in row.label.chars() {
        let matches = next.is_some() && character.to_lowercase().any(|lower| Some(lower) == next);
        if matches {
            next = wanted.next();
        }
        job.append(
            &character.to_string(),
            0.0,
            egui::TextFormat {
                font_id: theme.font(),
                color: if matches { theme.accent() } else { color },
                ..Default::default()
            },
        );
    }
    let galley = ui.painter().layout_job(job);
    row.galley = Some((key, Arc::clone(&galley)));
    galley
}

fn verb_icon(verb: Option<&str>) -> Option<Icon> {
    match verb? {
        "copy" => Some(Icon::Copy),
        "cut" => Some(Icon::Cut),
        "paste" | "pastelink" => Some(Icon::Paste),
        "delete" => Some(Icon::Trash),
        "rename" => Some(Icon::Rename),
        "properties" => Some(Icon::Inspect),
        "foldex-app-actions" => Some(Icon::More),
        "foldex-new" | "foldex-new:folder" => Some(Icon::Folder),
        "foldex-new:text" => Some(Icon::File),
        "foldex-new:shortcut" => Some(Icon::Forward),
        "foldex-compress" => Some(Icon::Archive),
        "undo" => Some(Icon::Back),
        "redo" => Some(Icon::Forward),
        verb if verb.starts_with("foldex-compress:") => Some(Icon::Archive),
        _ => None,
    }
}

fn core_verb(verb: Option<&str>) -> Option<CoreVerb> {
    match verb? {
        "open" => Some(CoreVerb::Open),
        "copy" => Some(CoreVerb::Copy),
        "cut" => Some(CoreVerb::Cut),
        "paste" => Some(CoreVerb::Paste),
        "rename" => Some(CoreVerb::Rename),
        "delete" => Some(CoreVerb::Delete),
        "refresh" => Some(CoreVerb::Refresh),
        "properties" => Some(CoreVerb::Properties),
        "undo" => Some(CoreVerb::Undo),
        "redo" => Some(CoreVerb::Redo),
        _ => None,
    }
}
fn core_id(verb: CoreVerb) -> u32 {
    0xf000_0000 + verb as u32
}
fn core_fallback(background: bool, selection: usize) -> Vec<Row> {
    let items: Vec<_> = [
        ("Open", "open", None),
        ("Cut", "cut", Some("Ctrl+X")),
        ("Copy", "copy", Some("Ctrl+C")),
        ("Paste", "paste", Some("Ctrl+V")),
        ("Rename", "rename", Some("F2")),
        ("Delete", "delete", Some("Del")),
        ("Refresh", "refresh", Some("F5")),
        ("Properties", "properties", None),
    ]
    .into_iter()
    .filter(|(_, verb, _)| !background || matches!(*verb, "paste" | "refresh" | "properties"))
    .map(|(label, verb, shortcut)| ShellItem {
        id: core_id(core_verb(Some(verb)).unwrap()),
        label: label.into(),
        canonical_verb: Some(verb.into()),
        enabled: verb != "rename" || selection == 1,
        shortcut: shortcut.map(str::to_owned),
        ..Default::default()
    })
    .collect();
    flatten(&items)
}

fn root_rows(background: bool, selection: usize) -> Vec<Row> {
    let mut rows = core_fallback(background, selection);
    if background {
        rows.extend(flatten(&[
            ShellItem {
                id: core_id(CoreVerb::Undo),
                label: "Undo".into(),
                canonical_verb: Some("undo".into()),
                shortcut: Some("Ctrl+Z".into()),
                ..Default::default()
            },
            ShellItem {
                id: core_id(CoreVerb::Redo),
                label: "Redo".into(),
                canonical_verb: Some("redo".into()),
                shortcut: Some("Ctrl+Y".into()),
                ..Default::default()
            },
        ]));
    }
    rows.extend(flatten(&[ShellItem {
        id: APP_NEW,
        label: "New".into(),
        canonical_verb: Some("foldex-new".into()),
        help: "Create a folder, shortcut or registered Windows document type".into(),
        enabled: true,
        has_submenu: true,
        ..Default::default()
    }]));
    // Folder/.txt use the tracked platform jobs and their filename dialog.
    // Shortcut keeps the real Windows wizard on the system-menu worker.
    for (index, (key, label)) in [
        ("new:folder", "Folder"),
        ("new:shortcut", "Shortcut"),
        ("new:text", "Text document (.txt)"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut defaults = flatten(&[ShellItem {
            id: APP_NEW + 0x100 + index as u32,
            label: label.into(),
            canonical_verb: Some(format!("foldex-{key}")),
            enabled: true,
            ..Default::default()
        }]);
        defaults[0].parent = Some(APP_NEW);
        defaults[0].path = "New".into();
        defaults[0].search = format!("new {label}").to_lowercase();
        defaults[0].core = builtin_core(BuiltinKind::New, key);
        defaults[0].builtin = defaults[0]
            .core
            .is_none()
            .then(|| (BuiltinKind::New, key.into()));
        rows.extend(defaults);
    }
    let mut status = flatten(&[ShellItem {
        id: APP_NEW + 0xf000,
        label: "Loading registered templates…".into(),
        ..Default::default()
    }]);
    status[0].parent = Some(APP_NEW);
    rows.extend(status);
    if !background {
        rows.extend(flatten(&[ShellItem {
            id: APP_COMPRESS,
            label: "Compress to…".into(),
            canonical_verb: Some("foldex-compress".into()),
            help: "Archive the selected items using available Windows formats".into(),
            enabled: selection > 0,
            has_submenu: true,
            ..Default::default()
        }]));
    }
    rows.extend(flatten(&[
        ShellItem {
            id: APP_ACTIONS + 1,
            separator: true,
            ..Default::default()
        },
        ShellItem {
            id: APP_ACTIONS,
            label: "More actions".into(),
            canonical_verb: Some("foldex-app-actions".into()),
            help: "Commands from Windows and your installed apps".into(),
            enabled: true,
            has_submenu: true,
            ..Default::default()
        },
    ]));
    rows
}

fn builtin_index(kind: BuiltinKind) -> usize {
    match kind {
        BuiltinKind::New => 0,
        BuiltinKind::Compress => 1,
    }
}
fn builtin_parent(kind: BuiltinKind) -> u32 {
    match kind {
        BuiltinKind::New => APP_NEW,
        BuiltinKind::Compress => APP_COMPRESS,
    }
}

fn builtin_core(kind: BuiltinKind, key: &str) -> Option<CoreVerb> {
    match (kind, key) {
        (BuiltinKind::New, "new:folder") => Some(CoreVerb::NewFolder),
        (BuiltinKind::New, "new:text") => Some(CoreVerb::NewFile),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn basic_commands_dispatch_on_the_first_frame_without_native_enumeration() {
        let ctx = egui::Context::default();
        let mut menu = ContextMenu::new(Arc::new(|| {})).unwrap();
        let mut times = Vec::new();
        for frame in 0..256 {
            let verb = [
                CoreVerb::Copy,
                CoreVerb::Delete,
                CoreVerb::Rename,
                CoreVerb::Properties,
            ][frame % 4];
            let start = Instant::now();
            menu.open_for_pane(
                99,
                0,
                vec![PathBuf::from("C:/fixture.txt")],
                false,
                pos2(300.0, 180.0),
            );
            menu.popup.as_mut().unwrap().selected = Some(core_id(verb));
            let mut dispatched = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                    time: Some(frame as f64 / 60.0),
                    events: vec![egui::Event::Key {
                        key: Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    let update = menu.ui(ui.ctx(), &Theme::default());
                    if let Some(action) = update.core_action {
                        assert!(dispatched.is_none(), "one action per Enter");
                        dispatched = Some(action);
                    }
                    assert!(update.errors.is_empty());
                },
            );
            output.textures_delta.clear();
            let action = dispatched.expect("first-frame core action");
            assert_eq!(action.verb, verb);
            assert_eq!(action.origin, 99);
            assert!(menu.pending.is_none());
            assert!(
                menu.popup
                    .as_ref()
                    .unwrap()
                    .flat
                    .iter()
                    .all(|row| row.id >= APP_ACTIONS)
            );
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        eprintln!(
            "256 immediate context opens+core dispatches (headless egui): p95={:.3}ms max={:.3}ms",
            times[243], times[255]
        );
    }
    #[test]
    fn native_actions_are_lazy_and_never_replace_or_move_basic_commands() {
        let mut popup = Popup::new(
            1,
            Pos2::ZERO,
            0,
            vec![PathBuf::from("C:/fixture.txt")],
            false,
        );
        assert!(!popup.wants_native());
        let basic: Vec<_> = popup
            .flat
            .iter()
            .filter_map(|row| row.core.map(|verb| (row.id, verb)))
            .collect();
        assert!(basic.iter().any(|(_, verb)| *verb == CoreVerb::Properties));
        popup.show_submenu(0, APP_ACTIONS, pos2(272.0, 40.0));
        assert!(popup.wants_native());
        assert!(popup.pending_expands.is_empty());
        popup.replace_native(&[
            ShellItem {
                id: 1,
                label: "Remove".into(),
                canonical_verb: Some("delete".into()),
                enabled: true,
                ..Default::default()
            },
            ShellItem {
                id: 2,
                separator: true,
                ..Default::default()
            },
            ShellItem {
                id: 3,
                label: "An installed extractor".into(),
                enabled: true,
                ..Default::default()
            },
            ShellItem {
                id: 4,
                label: "Open with".into(),
                enabled: true,
                has_submenu: true,
                children: vec![ShellItem {
                    id: 5,
                    label: "A registered editor".into(),
                    canonical_verb: Some("open".into()),
                    enabled: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
            ShellItem {
                id: 6,
                separator: true,
                ..Default::default()
            },
            ShellItem {
                id: 7,
                label: "Another registered action".into(),
                enabled: true,
                ..Default::default()
            },
        ]);
        let after: Vec<_> = popup
            .flat
            .iter()
            .filter_map(|row| row.core.map(|verb| (row.id, verb)))
            .collect();
        assert_eq!(basic, after);
        assert!(
            popup
                .flat
                .iter()
                .any(|row| row.id == 3 && row.parent == Some(APP_ACTIONS))
        );
        assert!(
            popup
                .flat
                .iter()
                .any(|row| row.id == 5 && row.parent == Some(4) && row.core.is_none())
        );
        let app_rows: Vec<_> = popup
            .flat
            .iter()
            .filter(|row| row.parent == Some(APP_ACTIONS))
            .collect();
        assert!(!app_rows.first().unwrap().separator && !app_rows.last().unwrap().separator);
        let pin = popup
            .flat
            .iter()
            .find(|row| row.id == 7)
            .unwrap()
            .pin
            .clone();
        popup.rebuild(&HashSet::from([pin]));
        assert_eq!(popup.flat[popup.panels[1].rows[0]].id, 7);
        // Typing a query closes child panels; choosing a matching group must
        // still be able to open its native submenu while the query remains.
        popup.query = "open with".into();
        popup.panels.truncate(1);
        popup.rebuild(&HashSet::new());
        assert_eq!(popup.selected, Some(4));
        popup.show_submenu(0, 4, Pos2::ZERO);
        popup.rebuild(&HashSet::new());
        assert_eq!(popup.panels[1].parent, Some(4));
        popup.query = "registered editor".into();
        popup.panels.truncate(1);
        popup.rebuild(&HashSet::new());
        assert_eq!(popup.selected, Some(5));
    }
    #[test]
    fn fuzzy_context_search_accepts_subsequence_and_requires_every_token() {
        assert!(fuzzy_score("open with › visual studio code", "vs cd").is_some());
        assert!(fuzzy_score("copy as path", "rename").is_none());
        assert!(
            fuzzy_score("open", "open").unwrap()
                > fuzzy_score("other options", "open").unwrap_or(i32::MIN)
        );
    }
    #[test]
    fn hierarchy_search_keeps_native_ids_and_canonical_pin_keys() {
        let rows = flatten(&[ShellItem {
            id: 1,
            label: "Open with".into(),
            has_submenu: true,
            children: vec![ShellItem {
                id: 2,
                label: "Editor".into(),
                canonical_verb: Some("edit".into()),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }]);
        assert_eq!(rows[1].id, 2);
        assert_eq!(rows[1].parent, Some(1));
        assert!(rows[1].search.contains("open with"));
        assert_eq!(rows[1].pin, "Open with/edit");
    }

    #[test]
    fn filtering_cannot_leave_a_hidden_destructive_command_selected() {
        let mut popup = Popup::new(
            1,
            pos2(10.0, 10.0),
            99,
            vec![PathBuf::from("fixture")],
            false,
        );
        popup.selected = Some(core_id(CoreVerb::Delete));
        popup.query = "copy".into();
        popup.rebuild(&HashSet::new());
        assert_eq!(popup.selected, Some(core_id(CoreVerb::Copy)));
        assert!(
            popup.panels[0]
                .rows
                .iter()
                .all(|index| popup.flat[*index].core != Some(CoreVerb::Delete))
        );
    }

    #[test]
    fn installed_submenu_open_verb_keeps_its_actual_native_target() {
        let rows = flatten(&[ShellItem {
            id: 10,
            label: "Open with".into(),
            has_submenu: true,
            children: vec![ShellItem {
                id: 11,
                label: "Specific editor".into(),
                canonical_verb: Some("open".into()),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }]);
        assert_eq!(rows[1].id, 11);
        assert!(rows[1].core.is_none());
    }

    fn submenu_fixture() -> Vec<ShellItem> {
        vec![
            ShellItem {
                id: 10,
                label: "First group".into(),
                has_submenu: true,
                enabled: true,
                children: vec![ShellItem {
                    id: 11,
                    label: "Nested group".into(),
                    has_submenu: true,
                    enabled: true,
                    children: vec![ShellItem {
                        id: 12,
                        label: "Nested action".into(),
                        enabled: true,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
            ShellItem {
                id: 20,
                label: "Second group".into(),
                has_submenu: true,
                enabled: true,
                ..Default::default()
            },
        ]
    }

    #[test]
    fn repeated_submenu_expansion_preserves_branch_focus_scroll_and_one_native_request() {
        let mut popup = Popup::new(1, Pos2::ZERO, 0, vec![], false);
        popup.replace_native(&submenu_fixture());
        assert!(popup.show_submenu(0, APP_ACTIONS, pos2(272.0, 40.0)));
        assert!(popup.show_submenu(1, 10, pos2(500.0, 40.0)));
        assert!(popup.show_submenu(2, 11, pos2(650.0, 40.0)));
        popup.rebuild(&HashSet::new());
        popup.panels[1].scroll_reset = false;
        popup.panels[2].scroll_reset = false;
        for _ in 0..8 {
            assert!(popup.show_submenu(0, APP_ACTIONS, pos2(270.0, 42.0)));
            assert!(popup.show_submenu(1, 10, pos2(498.0, 42.0)));
            assert_eq!(popup.panels.len(), 4);
            assert_eq!(
                popup.focus_panel, 3,
                "passive parent hover keeps child focus"
            );
            assert!(!popup.dirty);
            assert!(!popup.panels[1].scroll_reset && !popup.panels[2].scroll_reset);
        }
        assert_eq!(popup.panels[1].anchor, pos2(270.0, 42.0));
        assert_eq!(
            popup.pending_expands.iter().copied().collect::<Vec<_>>(),
            [10, 11]
        );
        // A stale focus depth must never append the same parent inside itself.
        assert!(!popup.show_submenu(popup.focus_panel, APP_ACTIONS, Pos2::ZERO));
        assert!(!popup.show_submenu(2, 10, Pos2::ZERO));
        assert_eq!(popup.panels.len(), 4);
    }

    #[test]
    fn switching_and_reopening_submenu_branches_discards_stale_descendants() {
        let mut popup = Popup::new(1, Pos2::ZERO, 0, vec![], false);
        popup.replace_native(&submenu_fixture());
        popup.show_submenu(0, APP_ACTIONS, Pos2::ZERO);
        popup.show_submenu(1, 10, Pos2::ZERO);
        popup.show_submenu(2, 11, Pos2::ZERO);
        assert!(popup.close_deepest());
        assert!(popup.show_submenu(2, 11, pos2(40.0, 50.0)));
        assert_eq!(popup.panels.len(), 4);
        assert!(!popup.panels[3].closing);
        assert!(popup.panels[3].scroll_reset);
        assert!(popup.show_submenu(1, 20, Pos2::ZERO));
        assert_eq!(popup.panels.len(), 3);
        assert_eq!(popup.panels[2].parent, Some(20));
        assert!(!popup.show_submenu(2, 11, Pos2::ZERO));
        assert_eq!(popup.panels.len(), 3);
    }

    #[test]
    fn native_updates_prune_reparented_disabled_and_duplicate_submenu_panels() {
        let mut popup = Popup::new(1, Pos2::ZERO, 0, vec![], false);
        let mut items = submenu_fixture();
        popup.replace_native(&items);
        popup.show_submenu(0, APP_ACTIONS, Pos2::ZERO);
        popup.show_submenu(1, 10, Pos2::ZERO);
        popup.show_submenu(2, 11, Pos2::ZERO);
        popup.panels.push(Panel::new(Some(APP_ACTIONS), Pos2::ZERO));
        popup.replace_native(&items);
        assert_eq!(popup.panels.len(), 4);
        let reparented = items[0].children.remove(0);
        items.push(reparented);
        popup.replace_native(&items);
        popup.rebuild(&HashSet::new());
        assert_eq!(
            popup.panels.len(),
            3,
            "existing IDs must retain their ancestry"
        );
        assert_eq!(popup.focus_panel, 2);
        items[0].enabled = false;
        popup.replace_native(&items);
        popup.rebuild(&HashSet::new());
        assert_eq!(popup.panels.len(), 2);
        assert_eq!(popup.focus_panel, 1);
        assert_eq!(popup.selected, Some(20));
        assert!(!popup.pending_expands.contains(&10));
        items.clear();
        popup.replace_native(&items);
        popup.rebuild(&HashSet::new());
        assert_eq!(
            popup.panels.len(),
            2,
            "More actions survives an empty update"
        );
        assert!(popup.panels[1].rows.is_empty());
        assert!(popup.pending_expands.is_empty());
    }

    #[test]
    fn submenu_depth_limit_does_not_mutate_the_last_open_branch() {
        let mut child = ShellItem {
            id: 100,
            label: "Last action".into(),
            enabled: true,
            ..Default::default()
        };
        for id in (1..=MAX_PANELS as u32).rev() {
            child = ShellItem {
                id,
                label: format!("Group {id}"),
                has_submenu: true,
                enabled: true,
                children: vec![child],
                ..Default::default()
            };
        }
        let mut popup = Popup::new(1, Pos2::ZERO, 0, vec![], false);
        popup.replace_native(&[child]);
        popup.show_submenu(0, APP_ACTIONS, Pos2::ZERO);
        for level in 1..MAX_PANELS - 1 {
            assert!(popup.show_submenu(level, level as u32, Pos2::ZERO));
        }
        assert_eq!(popup.panels.len(), MAX_PANELS);
        let focus = popup.focus_panel;
        let requests = popup.pending_expands.len();
        assert!(!popup.show_submenu(MAX_PANELS - 1, (MAX_PANELS - 1) as u32, Pos2::ZERO));
        assert!(!popup.show_submenu(usize::MAX, APP_ACTIONS, Pos2::ZERO));
        assert_eq!(popup.panels.len(), MAX_PANELS);
        assert_eq!(popup.focus_panel, focus);
        assert_eq!(popup.pending_expands.len(), requests);
    }

    // Keep native enumeration out of these input regressions. The real host
    // still owns the Area, text editor and opening/closing state; only its
    // worker request is omitted and its immediate core rows are used.
    fn input_menu() -> ContextMenu {
        let mut menu = ContextMenu::new(Arc::new(|| {})).unwrap();
        reopen_input_menu(&mut menu);
        menu
    }

    fn reopen_input_menu(menu: &mut ContextMenu) {
        let mut popup = Popup::new(7, pos2(100.0, 100.0), 1, vec![], false);
        popup.loading = false;
        menu.popup = Some(popup);
        menu.pending = None;
    }

    fn input_frame(
        ctx: &egui::Context,
        menu: &mut ContextMenu,
        frame: u32,
        events: Vec<egui::Event>,
    ) {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                time: Some(frame as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                let update = menu.ui(ui.ctx(), &Theme::default());
                assert!(update.core_action.is_none());
                assert!(!update.invoked);
                assert!(update.errors.is_empty());
            },
        );
        output.textures_delta.clear();
    }

    fn key(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn keyboard_menu_edges_and_submenu_escape_keep_focus_in_the_visible_parent() {
        let ctx = egui::Context::default();
        let mut menu = input_menu();
        {
            let popup = menu.popup.as_mut().unwrap();
            popup.flat = flatten(&submenu_fixture());
            popup.expanded.extend(
                popup
                    .flat
                    .iter()
                    .filter(|row| row.submenu)
                    .map(|row| row.id),
            );
        }
        input_frame(&ctx, &mut menu, 0, vec![]);
        input_frame(
            &ctx,
            &mut menu,
            1,
            vec![key(Key::Home, egui::Modifiers::NONE)],
        );
        let popup = menu.popup.as_ref().unwrap();
        let parent = popup.selected.unwrap();
        assert!(popup.flat.iter().any(|row| row.id == parent && row.submenu));
        input_frame(
            &ctx,
            &mut menu,
            2,
            vec![key(Key::ArrowRight, egui::Modifiers::NONE)],
        );
        assert_eq!(menu.popup.as_ref().unwrap().focus_panel, 1);
        input_frame(
            &ctx,
            &mut menu,
            3,
            vec![key(Key::Escape, egui::Modifiers::NONE)],
        );
        let popup = menu.popup.as_ref().unwrap();
        assert!(!popup.closing);
        assert_eq!(popup.focus_panel, 0);
        assert_eq!(popup.selected, Some(parent));
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(popup.panel_focus_id(0))
        );
        input_frame(
            &ctx,
            &mut menu,
            4,
            vec![key(Key::End, egui::Modifiers::NONE)],
        );
        let popup = menu.popup.as_ref().unwrap();
        let last = popup.panels[0]
            .rows
            .iter()
            .rev()
            .map(|index| &popup.flat[*index])
            .find(|row| row.enabled && !row.separator)
            .unwrap();
        assert_eq!(popup.selected, Some(last.id));
    }

    #[test]
    fn menu_tab_exits_and_modified_keys_and_ime_do_not_activate_background_rows() {
        let ctx = egui::Context::default();
        let mut menu = input_menu();
        input_frame(&ctx, &mut menu, 0, vec![]);
        let selected = menu.popup.as_ref().unwrap().selected;
        input_frame(
            &ctx,
            &mut menu,
            1,
            vec![
                key(Key::ArrowDown, egui::Modifiers::ALT),
                key(Key::Enter, egui::Modifiers::SHIFT),
            ],
        );
        assert_eq!(menu.popup.as_ref().unwrap().selected, selected);
        input_frame(
            &ctx,
            &mut menu,
            2,
            vec![
                egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "に".into(),
                    active_range_chars: None,
                }),
                key(Key::Enter, egui::Modifiers::NONE),
            ],
        );
        assert!(!menu.popup.as_ref().unwrap().closing);
        // Dismiss the composition before using Tab to leave the menu.
        input_frame(
            &ctx,
            &mut menu,
            3,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: String::new(),
                active_range_chars: None,
            })],
        );
        input_frame(
            &ctx,
            &mut menu,
            4,
            vec![key(Key::Tab, egui::Modifiers::SHIFT)],
        );
        assert!(menu.popup.as_ref().is_none_or(|popup| popup.closing));
    }

    #[test]
    fn typing_after_menu_navigation_returns_to_search_and_escape_closes_filtered_menu() {
        let ctx = egui::Context::default();
        let mut menu = input_menu();
        input_frame(&ctx, &mut menu, 0, vec![]);
        input_frame(
            &ctx,
            &mut menu,
            1,
            vec![key(Key::ArrowDown, egui::Modifiers::NONE)],
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(menu.popup.as_ref().unwrap().panel_focus_id(0))
        );
        input_frame(&ctx, &mut menu, 2, vec![egui::Event::Text("copy".into())]);
        assert_eq!(menu.popup.as_ref().unwrap().query, "copy");
        input_frame(
            &ctx,
            &mut menu,
            3,
            vec![key(Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(menu.popup.as_ref().is_none_or(|popup| popup.closing));
    }

    #[test]
    fn stationary_click_after_hover_opens_only_one_more_actions_panel() {
        let ctx = egui::Context::default();
        let mut menu = input_menu();
        for frame in 0..30 {
            input_frame(&ctx, &mut menu, frame, vec![]);
        }
        let row = Id::new(("context-motion-row", 7_u64, 0_usize, APP_ACTIONS));
        let position = ctx.read_response(row).unwrap().rect.center();
        input_frame(
            &ctx,
            &mut menu,
            30,
            vec![egui::Event::PointerMoved(position)],
        );
        menu.popup.as_mut().unwrap().hover_intent =
            Some((APP_ACTIONS, Instant::now() - Duration::from_millis(120), 0));
        input_frame(&ctx, &mut menu, 31, vec![]);
        assert_eq!(menu.popup.as_ref().unwrap().panels.len(), 2);
        assert_eq!(menu.popup.as_ref().unwrap().focus_panel, 1);
        for frame in 32..40 {
            input_frame(
                &ctx,
                &mut menu,
                frame,
                vec![egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed: frame % 2 == 0,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            let popup = menu.popup.as_ref().unwrap();
            assert_eq!(
                popup.panels.len(),
                2,
                "hover and click must reuse one child"
            );
            assert_eq!(popup.panels[1].parent, Some(APP_ACTIONS));
            assert!(popup.pending_expands.is_empty());
        }
        input_frame(
            &ctx,
            &mut menu,
            40,
            vec![true, false]
                .into_iter()
                .map(|pressed| egui::Event::PointerButton {
                    pos: position,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                })
                .collect(),
        );
        assert_eq!(menu.popup.as_ref().unwrap().panels.len(), 2);
    }

    #[test]
    fn click_and_hover_more_actions_keep_child_hover_scroll_and_invocation() {
        for (open_by_click, click_parent) in [(true, false), (false, false), (false, true)] {
            let ctx = egui::Context::default();
            let mut menu = input_menu();
            let items: Vec<_> = (100..150)
                .map(|id| ShellItem {
                    id,
                    label: format!("Fixture command {id}"),
                    enabled: true,
                    ..Default::default()
                })
                .collect();
            let popup = menu.popup.as_mut().unwrap();
            popup.replace_native(&items);
            // A safe application action proves that the child click dispatches
            // without invoking any installed extension or touching user files.
            popup
                .flat
                .iter_mut()
                .find(|row| row.id == 110)
                .unwrap()
                .core = Some(CoreVerb::Copy);
            for frame in 0..30 {
                input_frame(&ctx, &mut menu, frame, vec![]);
            }
            let parent_id = Id::new(("context-motion-row", 7_u64, 0_usize, APP_ACTIONS));
            let parent = ctx.read_response(parent_id).unwrap().rect.center();
            input_frame(&ctx, &mut menu, 30, vec![egui::Event::PointerMoved(parent)]);
            if open_by_click {
                for (frame, pressed) in [(31, true), (32, false)] {
                    input_frame(
                        &ctx,
                        &mut menu,
                        frame,
                        vec![egui::Event::PointerButton {
                            pos: parent,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                }
            } else {
                menu.popup.as_mut().unwrap().hover_intent =
                    Some((APP_ACTIONS, Instant::now() - Duration::from_millis(120), 0));
                input_frame(&ctx, &mut menu, 31, vec![]);
                input_frame(&ctx, &mut menu, 32, vec![]);
            }
            for frame in 33..62 {
                input_frame(&ctx, &mut menu, frame, vec![]);
            }
            if click_parent {
                for (frame, pressed) in [(62, true), (63, false)] {
                    input_frame(
                        &ctx,
                        &mut menu,
                        frame,
                        vec![egui::Event::PointerButton {
                            pos: parent,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        }],
                    );
                }
            } else {
                input_frame(&ctx, &mut menu, 62, vec![]);
                input_frame(&ctx, &mut menu, 63, vec![]);
            }
            let child_id = Id::new(("context-motion-row", 7_u64, 1_usize, 110_u32));
            let before = ctx.read_response(child_id).unwrap().rect;
            let point = before.center();
            input_frame(&ctx, &mut menu, 64, vec![egui::Event::PointerMoved(point)]);
            let child_layer = egui::LayerId::new(
                egui::Order::Foreground,
                Id::new(("foldex-native-context", 1_usize)),
            );
            assert_eq!(
                ctx.layer_id_at(point),
                Some(child_layer),
                "parent click must not raise its backdrop above the child"
            );
            assert!(ctx.memory(|memory| memory.allows_interaction(child_layer)));
            assert!(
                ctx.read_response(child_id).unwrap().hovered(),
                "child hover after click={click_parent}"
            );
            input_frame(
                &ctx,
                &mut menu,
                65,
                vec![egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: vec2(0.0, -150.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            for frame in 66..96 {
                input_frame(&ctx, &mut menu, frame, vec![]);
            }
            let after = ctx.read_response(child_id).unwrap().rect;
            assert!(
                after.top() < before.top() - 20.0,
                "child wheel must scroll after click={click_parent}: {before:?} -> {after:?}"
            );
            let popup = menu.popup.as_ref().unwrap();
            assert_eq!(popup.panels.len(), 2);
            assert_eq!(popup.panels[1].parent, Some(APP_ACTIONS));
            assert!(!popup.panels[1].scroll_reset);
            let point = after.center();
            input_frame(&ctx, &mut menu, 96, vec![egui::Event::PointerMoved(point)]);
            input_frame(
                &ctx,
                &mut menu,
                97,
                vec![egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            let mut action = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                    time: Some(98.0 / 60.0),
                    events: vec![egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ui| {
                    let update = menu.ui(ui.ctx(), &Theme::default());
                    assert!(update.errors.is_empty());
                    if update.core_action.is_some() {
                        assert!(action.is_none());
                        action = update.core_action;
                    }
                },
            );
            output.textures_delta.clear();
            assert_eq!(
                action.expect("child command must dispatch").verb,
                CoreVerb::Copy
            );
        }
    }

    #[test]
    fn new_and_compress_are_top_level_lazy_and_survive_installed_menu_updates() {
        for background in [false, true] {
            let mut popup = Popup::new(
                1,
                Pos2::ZERO,
                1,
                vec![PathBuf::from("C:/fixture/item")],
                background,
            );
            assert!(
                popup
                    .flat
                    .iter()
                    .any(|row| row.id == APP_NEW && row.parent.is_none() && row.submenu)
            );
            assert_eq!(
                popup
                    .flat
                    .iter()
                    .any(|row| row.id == APP_COMPRESS && row.parent.is_none()),
                !background
            );
            assert!(!popup.wants_native() && !popup.wants_builtin(BuiltinKind::New));
            assert!(popup.show_submenu(0, APP_NEW, Pos2::ZERO));
            assert!(popup.wants_builtin(BuiltinKind::New));
            assert!(!popup.wants_native());
            assert!(popup.pending_expands.is_empty());
            popup.replace_builtin(
                BuiltinKind::New,
                &BuiltinSnapshot {
                    items: vec![BuiltinItem {
                        key: "new:template:fixture".into(),
                        label: "Fixture template".into(),
                    }]
                    .into(),
                    truncated: false,
                },
            );
            popup.replace_native(&[ShellItem {
                id: 3,
                label: "Installed app".into(),
                enabled: true,
                ..Default::default()
            }]);
            let template = popup
                .flat
                .iter()
                .find(|row| row.label == "Fixture template")
                .unwrap();
            assert_eq!(template.parent, Some(APP_NEW));
            assert_eq!(
                template.builtin,
                Some((BuiltinKind::New, "new:template:fixture".into()))
            );
            popup.rebuild(&HashSet::new());
            assert_eq!(popup.panels.len(), 2);
            assert_eq!(popup.panels[1].parent, Some(APP_NEW));
        }
    }

    #[test]
    fn background_history_uses_cached_action_labels_and_busy_availability() {
        let mut popup = Popup::new(1, Pos2::ZERO, 1, vec![], true);
        assert!(
            popup
                .flat
                .iter()
                .filter(|row| matches!(row.core, Some(CoreVerb::Undo | CoreVerb::Redo)))
                .all(|row| !row.enabled)
        );
        let history = HistoryState {
            undo: Some("Move"),
            redo: Some("Rename"),
            busy: false,
        };
        popup.update_history(history);
        let undo = popup
            .flat
            .iter()
            .find(|row| row.core == Some(CoreVerb::Undo))
            .unwrap();
        assert_eq!(undo.label, "Undo Move");
        assert!(undo.enabled);
        assert_eq!(undo.shortcut.as_deref(), Some("Ctrl+Z"));
        let redo = popup
            .flat
            .iter()
            .find(|row| row.core == Some(CoreVerb::Redo))
            .unwrap();
        assert_eq!(redo.label, "Redo Rename");
        assert!(redo.enabled);
        popup.update_history(HistoryState {
            busy: true,
            ..history
        });
        assert!(
            popup
                .flat
                .iter()
                .filter(|row| matches!(row.core, Some(CoreVerb::Undo | CoreVerb::Redo)))
                .all(|row| !row.enabled)
        );
        popup.replace_native(&[]);
        assert!(
            popup
                .flat
                .iter()
                .any(|row| row.label == "Undo Move" && !row.enabled)
        );
    }

    #[test]
    fn cached_template_updates_preserve_command_identity_when_order_changes() {
        let mut popup = Popup::new(1, Pos2::ZERO, 1, vec![PathBuf::from("C:/fixture")], true);
        popup.show_submenu(0, APP_NEW, Pos2::ZERO);
        popup.rebuild(&HashSet::new());
        popup.selected = Some(APP_NEW + 0x102); // The immediate .txt default.
        popup.replace_builtin(
            BuiltinKind::New,
            &BuiltinSnapshot {
                items: vec![
                    BuiltinItem {
                        key: "new:template:third-party".into(),
                        label: "Registered template".into(),
                    },
                    BuiltinItem {
                        key: "new:text".into(),
                        label: "Text document (.txt)".into(),
                    },
                    BuiltinItem {
                        key: "new:folder".into(),
                        label: "Folder".into(),
                    },
                ]
                .into(),
                truncated: false,
            },
        );
        popup.rebuild(&HashSet::new());
        let selected = popup
            .flat
            .iter()
            .find(|row| Some(row.id) == popup.selected)
            .unwrap();
        assert_eq!(selected.core, Some(CoreVerb::NewFile));
        assert!(selected.builtin.is_none());
        let template = popup
            .flat
            .iter()
            .find(|row| row.label == "Registered template")
            .unwrap()
            .id;
        assert_ne!(template, APP_NEW + 0x102);
        popup.replace_builtin(
            BuiltinKind::New,
            &BuiltinSnapshot {
                items: vec![BuiltinItem {
                    key: "new:template:third-party".into(),
                    label: "Registered template".into(),
                }]
                .into(),
                truncated: false,
            },
        );
        assert_eq!(
            popup
                .flat
                .iter()
                .find(|row| row.label == "Registered template")
                .unwrap()
                .id,
            template
        );
    }

    #[test]
    fn standard_new_commands_dispatch_tracked_creation_before_and_after_native_discovery() {
        for background in [false, true] {
            for cached in [false, true] {
                for (id, verb) in [
                    (APP_NEW + 0x100, CoreVerb::NewFolder),
                    (APP_NEW + 0x102, CoreVerb::NewFile),
                ] {
                    let ctx = egui::Context::default();
                    let mut menu = input_menu();
                    let destination = PathBuf::from("C:/captured-pane");
                    menu.popup = Some(Popup::new(
                        7,
                        pos2(100.0, 100.0),
                        41,
                        vec![PathBuf::from("C:/selection/source.txt")],
                        background,
                    ));
                    menu.set_new_directory(Some(destination.clone()));
                    let popup = menu.popup.as_mut().unwrap();
                    // No native calls occur in this input regression.
                    popup.builtin_requested = [true; 2];
                    if cached {
                        popup.replace_builtin(
                            BuiltinKind::New,
                            &BuiltinSnapshot {
                                items: vec![
                                    BuiltinItem {
                                        key: "new:text".into(),
                                        label: "Native text template".into(),
                                    },
                                    BuiltinItem {
                                        key: "new:folder".into(),
                                        label: "Native folder".into(),
                                    },
                                    BuiltinItem {
                                        key: "new:shortcut".into(),
                                        label: "Native shortcut".into(),
                                    },
                                    BuiltinItem {
                                        key: "new:template:fixture".into(),
                                        label: "Registered template".into(),
                                    },
                                ]
                                .into(),
                                truncated: false,
                            },
                        );
                        popup.replace_native(&[]);
                        let template = popup
                            .flat
                            .iter()
                            .find(|row| row.label == "Registered template")
                            .unwrap();
                        assert!(template.core.is_none() && template.builtin.is_some());
                    }
                    let standard = popup.flat.iter().find(|row| row.id == id).unwrap();
                    assert_eq!(standard.core, Some(verb));
                    assert!(
                        standard.builtin.is_none(),
                        "standard creation must not submit a native Shell invocation"
                    );
                    assert_eq!(
                        popup
                            .flat
                            .iter()
                            .filter(|row| row.core == Some(verb))
                            .count(),
                        1
                    );
                    popup.show_submenu(0, APP_NEW, Pos2::ZERO);
                    popup.rebuild(&HashSet::new());
                    popup.selected = Some(id);
                    let mut action = None;
                    let mut output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                            time: Some(0.0),
                            events: vec![key(Key::Enter, egui::Modifiers::NONE)],
                            ..Default::default()
                        },
                        |ui| {
                            let update = menu.ui(ui.ctx(), &Theme::default());
                            assert!(update.errors.is_empty() && !update.invoked);
                            if update.core_action.is_some() {
                                assert!(action.is_none());
                                action = update.core_action;
                            }
                        },
                    );
                    output.textures_delta.clear();
                    let action =
                        action.expect("tracked New command must dispatch on the first frame");
                    assert_eq!(action.verb, verb);
                    assert_eq!(action.origin, 41);
                    assert_eq!(action.paths, vec![destination]);
                    assert!(action.background);
                }
            }
        }
    }

    #[test]
    fn builtin_capture_hook_only_opens_and_waits_for_definitions() {
        let ctx = egui::Context::default();
        let mut menu = ContextMenu::new(Arc::new(|| {})).unwrap();
        assert!(!menu.benchmark_open_builtin(&ctx, PathBuf::from("relative"), BuiltinKind::New));
        assert!(!menu.is_open());
        for kind in [BuiltinKind::New, BuiltinKind::Compress] {
            let target = if kind == BuiltinKind::New {
                std::env::temp_dir()
            } else {
                std::env::temp_dir().join("foldex-owned-capture-sample.txt")
            };
            assert!(menu.benchmark_open_builtin(&ctx, target, kind));
            let popup = menu.popup.as_ref().unwrap();
            assert_eq!(popup.panels.len(), 2);
            assert_eq!(popup.panels[1].parent, Some(builtin_parent(kind)));
            assert!(popup.pending_invoke.is_none());
            assert!(popup.wants_builtin(kind) && !popup.wants_native());
            assert_eq!(menu.benchmark_builtin_state(kind).unwrap(), None);
        }
    }

    #[test]
    fn more_actions_loading_label_is_complete_during_popup_expansion() {
        fn check_text(shape: &egui::Shape, clip: Rect) -> usize {
            match shape {
                egui::Shape::Vec(shapes) => {
                    shapes.iter().map(|shape| check_text(shape, clip)).sum()
                }
                egui::Shape::Text(text) if text.galley.text() == LOADING_COMMANDS => {
                    assert!(
                        !text.galley.elided,
                        "the loading label must fit without truncation"
                    );
                    assert!(
                        clip.expand(0.5).contains_rect(text.visual_bounding_rect()),
                        "loading glyphs must fit their actual paint clip: {:?} in {clip:?}",
                        text.visual_bounding_rect()
                    );
                    1
                }
                _ => 0,
            }
        }
        for (font_size, pixels_per_point) in [(14.0, 1.0), (18.0, 1.5), (24.0, 2.0)] {
            let ctx = egui::Context::default();
            ctx.set_pixels_per_point(pixels_per_point);
            let theme = Theme {
                font_size,
                ..Default::default()
            };
            let mut popup = Popup::new(1, pos2(20.0, 80.0), 1, vec![], false);
            popup.show_submenu(0, APP_ACTIONS, pos2(150.0, 80.0));
            popup.rebuild(&HashSet::new());
            let mut painted = 0;
            let mut settled = 0;
            for frame in 0..60 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
                        time: Some(frame as f64 / 60.0),
                        ..Default::default()
                    },
                    |ui| {
                        let gains = MotionGains::new(1.0 / 60.0, theme.duration());
                        popup.layout.begin_frame_with_gains(gains);
                        draw_panel(
                            ui.ctx(),
                            &theme,
                            popup.request,
                            1,
                            &mut popup.panels[1],
                            &mut popup.query,
                            &mut popup.focus_search,
                            &mut popup.flat,
                            popup.selected,
                            true,
                            None,
                            false,
                            &HashSet::new(),
                            &mut popup.layout,
                            &mut popup.hovers,
                            gains,
                            false,
                            None,
                            frame == 0,
                            false,
                        );
                    },
                );
                let count: usize = output
                    .shapes
                    .iter()
                    .map(|shape| check_text(&shape.shape, shape.clip_rect))
                    .sum();
                painted += count;
                if frame == 59 {
                    settled = count;
                }
                output.textures_delta.clear();
            }
            assert!(
                painted > 0,
                "loading status never appeared at font size {font_size}"
            );
            assert_eq!(settled, 1, "settled loading status should paint once");
            assert_eq!(popup.panels.len(), 2);
        }
    }

    #[test]
    fn keyboard_submenu_activation_keeps_its_panel_when_pointer_changes_focus() {
        let ctx = egui::Context::default();
        let mut menu = input_menu();
        {
            let popup = menu.popup.as_mut().unwrap();
            popup.replace_native(&submenu_fixture());
            popup.show_submenu(0, APP_ACTIONS, Pos2::ZERO);
        }
        for frame in 0..30 {
            input_frame(&ctx, &mut menu, frame, vec![]);
        }
        let popup = menu.popup.as_mut().unwrap();
        popup.focus_panel = 1;
        popup.selected = Some(10);
        let row = Id::new(("context-motion-row", 7_u64, 0_usize, APP_ACTIONS));
        let position = ctx.read_response(row).unwrap().rect.center();
        input_frame(
            &ctx,
            &mut menu,
            30,
            vec![
                egui::Event::PointerMoved(position),
                egui::Event::Key {
                    key: Key::ArrowRight,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let popup = menu.popup.as_ref().unwrap();
        assert_eq!(popup.panels.len(), 3);
        assert_eq!(popup.panels[1].parent, Some(APP_ACTIONS));
        assert_eq!(popup.panels[2].parent, Some(10));
        assert_eq!(popup.focus_panel, 2);
    }

    #[test]
    fn opening_search_accepts_same_frame_text_and_keeps_focus_after_sizing() {
        let ctx = egui::Context::default();
        let mut menu = input_menu();
        input_frame(&ctx, &mut menu, 0, vec![egui::Event::Text("copy".into())]);
        assert_eq!(menu.popup.as_ref().unwrap().query, "copy");
        for frame in 1..30 {
            input_frame(&ctx, &mut menu, frame, vec![]);
        }
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("context-search"))
        );
        assert!(ctx.egui_wants_keyboard_input());
        assert_eq!(menu.popup.as_ref().unwrap().query, "copy");
        assert!(!menu.popup.as_ref().unwrap().focus_search);
    }

    #[test]
    fn cached_popup_reopen_by_outside_pointer_keeps_search_focused() {
        let ctx = egui::Context::default();
        let mut menu = input_menu();
        for frame in 0..30 {
            input_frame(&ctx, &mut menu, frame, vec![]);
        }
        menu.close();
        for frame in 30..90 {
            input_frame(&ctx, &mut menu, frame, vec![]);
        }
        assert!(!menu.is_open());

        // Reuse the same request/Area ID to exercise egui's cached Area. The
        // pointer press represents the click which opened this popup elsewhere.
        reopen_input_menu(&mut menu);
        let pointer = pos2(600.0, 450.0);
        input_frame(
            &ctx,
            &mut menu,
            90,
            vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        input_frame(
            &ctx,
            &mut menu,
            91,
            vec![egui::Event::PointerButton {
                pos: pointer,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        for frame in 92..120 {
            input_frame(&ctx, &mut menu, frame, vec![]);
        }
        let popup = menu
            .popup
            .as_ref()
            .expect("opening click must not close it");
        assert!(!popup.closing && !popup.focus_search);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("context-search"))
        );
        assert!(ctx.egui_wants_keyboard_input());
        input_frame(&ctx, &mut menu, 120, vec![egui::Event::Text("copy".into())]);
        assert_eq!(menu.popup.as_ref().unwrap().query, "copy");
    }

    fn geometry_frame(
        ctx: &egui::Context,
        popup: &mut Popup,
        theme: &Theme,
        frame: u32,
    ) -> PanelResult {
        let mut result = PanelResult::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            },
            |ui| {
                let ctx = ui.ctx();
                let gains = MotionGains::new(1.0 / 60.0, theme.duration());
                popup.layout.begin_frame_with_gains(gains);
                let child_parent = popup.panels.get(1).and_then(|panel| panel.parent);
                let opening_frame = *popup.opened_frame.get_or_insert(ctx.cumulative_frame_nr())
                    == ctx.cumulative_frame_nr();
                result = draw_panel(
                    ctx,
                    theme,
                    popup.request,
                    0,
                    &mut popup.panels[0],
                    &mut popup.query,
                    &mut popup.focus_search,
                    &mut popup.flat,
                    popup.selected,
                    false,
                    None,
                    popup.closing,
                    &HashSet::new(),
                    &mut popup.layout,
                    &mut popup.hovers,
                    gains,
                    false,
                    child_parent,
                    opening_frame,
                    true,
                );
                if let Some(anchor) = result.child_anchor {
                    if let Some(child) = popup.panels.get_mut(1) {
                        child.anchor = anchor;
                    }
                }
            },
        );
        output.textures_delta.clear();
        result
    }

    #[test]
    fn corner_popup_clamps_before_entrance_then_settles_and_closes() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut popup = Popup::new(1, pos2(390.0, 290.0), 1, vec![], false);
        popup.rebuild(&HashSet::new());
        let first = geometry_frame(&ctx, &mut popup, &theme, 0);
        assert!(first.rect.min.is_finite() && first.rect.max.is_finite());
        assert!(first.rect.right() <= 395.0);
        assert!(popup.panels[0].opacity > 0.0 && popup.panels[0].opacity < 1.0);
        let mut settled = first;
        for frame in 1..60 {
            settled = geometry_frame(&ctx, &mut popup, &theme, frame);
        }
        assert!(settled.rect.left() >= 5.0 && settled.rect.bottom() <= 295.0);
        assert_eq!(popup.panels[0].opacity, 1.0);
        assert!(!settled.moving && !popup.layout.is_moving());
        popup.closing = true;
        for frame in 60..120 {
            settled = geometry_frame(&ctx, &mut popup, &theme, frame);
        }
        assert_eq!(popup.panels[0].opacity, 0.0);
        assert_eq!(popup.panels[0].height, 0.0);
        assert!(!settled.moving);
    }

    #[test]
    fn reduced_motion_popup_is_immediate_and_does_not_keep_animation_alive() {
        let ctx = egui::Context::default();
        let theme = Theme {
            reduced_motion: true,
            ..Default::default()
        };
        let mut popup = Popup::new(1, pos2(390.0, 290.0), 1, vec![], false);
        popup.rebuild(&HashSet::new());
        let result = geometry_frame(&ctx, &mut popup, &theme, 0);
        assert_eq!(popup.panels[0].opacity, 1.0);
        assert!(result.rect.bottom() <= 295.0);
        assert!(!result.moving && !popup.layout.is_moving());
    }

    #[test]
    fn filtering_keeps_the_root_header_at_its_clamped_top_after_growth() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut popup = Popup::new(1, pos2(300.0, 184.0), 1, vec![], false);
        popup.rebuild(&HashSet::new());
        for frame in 0..30 {
            geometry_frame(&ctx, &mut popup, &theme, frame);
        }
        let mut items: Vec<_> = (0..40)
            .map(|index| ShellItem {
                id: index + 1,
                label: format!("Command {index}"),
                enabled: true,
                ..Default::default()
            })
            .collect();
        items.push(ShellItem {
            id: 50,
            label: "Copy path".into(),
            enabled: true,
            ..Default::default()
        });
        popup.flat = flatten(&items);
        popup.rebuild(&HashSet::new());
        let mut before = PanelResult::default();
        for frame in 30..60 {
            before = geometry_frame(&ctx, &mut popup, &theme, frame);
        }
        assert_eq!(before.rect.top(), 36.0);
        popup.query = "copy path".into();
        popup.rebuild(&HashSet::new());
        let mut filtered = PanelResult::default();
        for frame in 60..90 {
            filtered = geometry_frame(&ctx, &mut popup, &theme, frame);
        }
        assert_eq!(filtered.rect.min, before.rect.min);
        assert!(filtered.rect.height() < before.rect.height());
        assert!(!filtered.moving && !popup.layout.is_moving());
    }

    #[test]
    fn open_child_anchor_follows_the_parent_row_during_layout_motion() {
        let ctx = egui::Context::default();
        let theme = Theme::default();
        let mut popup = Popup::new(1, pos2(90.0, 100.0), 1, vec![], false);
        popup.flat = flatten(&[ShellItem {
            id: 10,
            label: "Open with".into(),
            has_submenu: true,
            enabled: true,
            children: vec![ShellItem {
                id: 11,
                label: "Editor".into(),
                enabled: true,
                ..Default::default()
            }],
            ..Default::default()
        }]);
        popup.show_submenu(0, 10, Pos2::ZERO);
        popup.rebuild(&HashSet::new());
        let mut before = PanelResult::default();
        for frame in 0..60 {
            before = geometry_frame(&ctx, &mut popup, &theme, frame);
        }
        let old_anchor = popup.panels[1].anchor;
        popup.panels[0].anchor -= vec2(20.0, 30.0);
        let moved = geometry_frame(&ctx, &mut popup, &theme, 61);
        let parent_delta = moved.rect.min - before.rect.min;
        let child_delta = popup.panels[1].anchor - old_anchor;
        assert!(parent_delta.length() > 0.0);
        assert!(
            (child_delta - parent_delta).length() <= 1.0 / ctx.pixels_per_point(),
            "parent moved {parent_delta:?}, child anchor moved {child_delta:?}"
        );
    }
}
