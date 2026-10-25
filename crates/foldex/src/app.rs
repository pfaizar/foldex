use crate::{
    pane::{Action, Pane, path_label},
    platform::{self, Job, Platform, PreviewContent},
};
use egui::{
    Align2, Id, Key, Modifiers, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use foldex_core::Engine;
use foldex_ui::{Icon, Theme, icon, icon_button};
mod appearance;
mod chrome;
mod commands_ui;
mod deletion_ui;
mod fixes_benchmark;
mod home_ui;
mod keyboard_benchmark;
mod keyboard_ui;
mod launcher_benchmark;
mod launcher_ui;
pub(crate) mod lifecycle_probe;
#[cfg(test)]
mod lifecycle_tests;
mod operations_ui;
mod options_ui;
#[cfg(test)]
mod persistence_tests;
pub(crate) mod recycle_ui;
mod stress;
mod update_ui;
pub(crate) mod window_transfer;
pub(crate) mod window_transfer_probe;
mod workspace_canvas;
mod workspace_ui;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use workspace_ui::hash_pane;

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub tabs: Vec<PathBuf>,
    pub active: usize,
    pub bookmarks: Vec<PathBuf>,
    pub recent: Vec<PathBuf>,
    pub home_history: crate::home_history::History,
    pub sidebar_preferences: crate::sidebar::Preferences,
    pub sidebar: bool,
    pub sidebar_collapsed: u8,
    pub workspace: Option<crate::workspace::WorkspaceState>,
    pub saved_layouts: Vec<crate::workspace::NamedWorkspace>,
    pub folder_views: Vec<crate::workspace::SavedFolderView>,
    pub command_bookmarks: Option<Vec<crate::commands::Command>>,
    pub context_pins: Vec<String>,
    pub hotkeys: Option<Vec<crate::hotkeys::Binding>>,
    pub launcher: crate::launcher::Settings,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            tabs: vec![],
            active: 0,
            bookmarks: vec![],
            recent: vec![],
            home_history: Default::default(),
            sidebar_preferences: Default::default(),
            sidebar: true,
            sidebar_collapsed: 0,
            workspace: None,
            saved_layouts: vec![],
            folder_views: vec![],
            command_bookmarks: None,
            context_pins: vec![],
            hotkeys: None,
            launcher: Default::default(),
        }
    }
}
impl Session {
    pub fn compact_layouts(&mut self) {
        for layout in &mut self.saved_layouts {
            layout.state.clear_navigation_history();
        }
    }
}
struct Tab {
    left: Pane,
    right: Option<Pane>,
    vertical: bool,
    pending_axis: Option<bool>,
    split: f32,
    reveal: f32,
    closing: bool,
}
struct TabDrag {
    id: u64,
    offset: f32,
    press: egui::Pos2,
    active: bool,
}
struct EditDialog {
    path: PathBuf,
    name: String,
    create: bool,
    create_file: bool,
    focus: bool,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum ResolveIntent {
    Navigate,
    Activate,
    Paste,
}
struct Toast {
    text: String,
    until: Instant,
    error: bool,
    opacity: f32,
    scroll_reset: bool,
    suspended_at: Option<Instant>,
}
#[derive(Clone, Serialize)]
struct ProcessSample {
    elapsed_seconds: f64,
    private_bytes: u64,
    working_set_bytes: u64,
    handles: u32,
    cpu_seconds: f64,
    ui_frames: u64,
    renderer_cpu_ms: f64,
}

pub struct Foldex {
    ui_context: egui::Context,
    engine: Engine,
    search: crate::search::SearchHub,
    platform: Platform,
    theme: Theme,
    tabs: Vec<Tab>,
    desk: workspace_canvas::Desk,
    retiring_panes: Vec<Pane>,
    goto: Option<crate::goto::GoTo>,
    retiring_goto: Vec<crate::goto::GoTo>,
    active: usize,
    active_pane: usize,
    next_id: u64,
    sidebar: bool,
    sidebar_width: f32,
    sidebar_sections: chrome::SidebarSections,
    main_menu: chrome::MainMenu,
    tab_drag: Option<TabDrag>,
    window_transfer: window_transfer::Transfers,
    tab_menu: foldex_ui::components::MenuState,
    tab_menu_id: Option<u64>,
    tab_menu_anchor: Rect,
    layout_size: egui::Vec2,
    layout_scale: f32,
    resize_until: Instant,
    side_filter: String,
    bookmarks: Vec<PathBuf>,
    recent: VecDeque<PathBuf>,
    home_history: crate::home_history::History,
    home_usage_revision: u64,
    home: crate::home_history::Service,
    recycle: recycle_ui::RecycleUi,
    sidebar_data: crate::sidebar::Data,
    places: Vec<(Icon, String, PathBuf)>,
    options: bool,
    updates: crate::updates::Updates,
    options_popup: options_ui::OptionsPopup,
    edit: Option<EditDialog>,
    deletion: deletion_ui::Deletion,
    inspector: bool,
    inspector_width: f32,
    deferred_editor_input: Vec<egui::Event>,
    deferred_editor_target: Option<u64>,
    resolve_id: u64,
    resolve_target: u64,
    resolve_navigation: u64,
    resolve_intent: ResolveIntent,
    toast: Option<Toast>,
    theme_path: PathBuf,
    session_path: PathBuf,
    started: Instant,
    first_frame: Option<f64>,
    frames: VecDeque<f64>,
    show_stats: bool,
    operations_open: bool,
    keyboard_help: bool,
    pending_focus_exit: Option<(bool, u64)>,
    process_samples: Vec<ProcessSample>,
    metrics_requested: Instant,
    session_dirty: bool,
    persistence: crate::persistence::Persistence,
    session_signature: u64,
    session_revision: u64,
    saved_revision: u64,
    save_due: Option<Instant>,
    close_requested: Option<Instant>,
    allow_close: bool,
    close_failed: bool,
    adapter_info: String,
    frame_count: u64,
    renderer_cpu_ms: f64,
    saved_layouts: Vec<crate::workspace::NamedWorkspace>,
    folder_views: Vec<crate::workspace::SavedFolderView>,
    closed_tabs: VecDeque<crate::workspace::TabState>,
    save_layout_name: Option<String>,
    load_layout_open: bool,
    inspector_target_width: f32,
    assets: crate::assets::AssetManager,
    context: crate::context::ContextMenu,
    commands: crate::commands::Palette,
    window_owner: isize,
    native_frame: Option<crate::native_frame::NativeFrame>,
    fullscreen: bool,
    picker: Option<crate::picker::Picker>,
    picker_layout: bool,
    hotkeys_open: bool,
    inspector_details: bool,
    inspector_view: Option<crate::inspector::Inspector>,
    inspector_source: Option<(u64, PathBuf, Option<u64>)>,
    batch: Option<crate::batch::BatchRename>,
    hotkeys: crate::hotkeys::Hotkeys,
    benchmark: Option<crate::performance::UiRun>,
    launcher: crate::launcher::Launcher,
    launcher_open: bool,
    launcher_positioned: bool,
    launcher_initialized: bool,
    launcher_focused: bool,
    explorer_visible: bool,
    background_allowed: bool,
    launcher_auxiliary: bool,
    launcher_secondary: Option<crate::performance::monitor::Monitor>,
}

impl Foldex {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        theme: Theme,
        initial: Option<PathBuf>,
        theme_path: PathBuf,
        session_path: PathBuf,
        preferences_path: PathBuf,
        session: Session,
        started: Instant,
        benchmark: Option<crate::performance::UiRun>,
        launcher_runtime: crate::launcher::Runtime,
    ) -> std::io::Result<Self> {
        let mut session = session;
        session.compact_layouts();
        let persistence = crate::persistence::Persistence::new_session(
            Arc::new({
                let context = cc.egui_ctx.clone();
                move || context.request_repaint_of(egui::ViewportId::ROOT)
            }),
            preferences_path,
            &serde_json::to_value(&session)?,
        )?;
        let ctx = cc.egui_ctx.clone();
        // Workers and the tray are serviced by the root, even if a launcher
        // callback happens to be the active viewport when a worker finishes.
        let wake: Arc<dyn Fn() + Send + Sync> =
            Arc::new(move || ctx.request_repaint_of(egui::ViewportId::ROOT));
        let engine = Engine::new(wake.clone())?;
        let search = crate::search::SearchHub::new(
            session_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("foldex-cache"),
            wake.clone(),
        )?;
        let platform = Platform::new(wake.clone())?;
        let updates = crate::updates::Updates::new(
            session_path
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .to_owned(),
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(crate::installation::managed))
                .unwrap_or(false),
            cfg!(windows) && !cfg!(test) && benchmark.is_none() && !launcher_runtime.isolate,
            wake.clone(),
        );
        let mut context = crate::context::ContextMenu::new(wake.clone())?;
        context.set_pins(&session.context_pins);
        let mut assets = if benchmark.as_ref().is_some_and(|run| run.no_shell_assets) {
            crate::assets::AssetManager::disabled(&cc.egui_ctx)
        } else {
            crate::assets::AssetManager::new(&cc.egui_ctx)?
        };
        if benchmark.is_none() && (!launcher_runtime.isolate || launcher_runtime.auxiliary) {
            assets.enable_cut_tracking();
        }
        let mut commands = crate::commands::Palette::new();
        let mut hotkeys = session
            .hotkeys
            .clone()
            .map(|mut bindings| {
                crate::hotkeys::Hotkeys::add_keyboard_defaults(&mut bindings);
                crate::hotkeys::Hotkeys::from_bindings(bindings)
            })
            .unwrap_or_default();
        if let Some(pins) = session.command_bookmarks.clone() {
            commands.set_bookmarks(pins);
        }
        for command in [
            crate::commands::Command::SaveLayout,
            crate::commands::Command::LoadLayout,
            crate::commands::Command::SaveFolderView,
            crate::commands::Command::NewFile,
            crate::commands::Command::InvertSelection,
            crate::commands::Command::Hotkeys,
            crate::commands::Command::InspectorDetails,
        ] {
            commands.set_enabled(command, true);
        }
        commands.close();
        let window_owner = {
            use raw_window_handle::HasWindowHandle;
            match cc.window_handle().map(|h| h.as_raw()) {
                Ok(raw_window_handle::RawWindowHandle::Win32(h)) => h.hwnd.get(),
                _ => 0,
            }
        };
        platform.set_owner_window(window_owner);
        let mut launcher_settings = session.launcher.clone();
        if let Some(run) = benchmark
            .as_ref()
            .filter(|run| run.launcher.is_some() || run.fixes.is_some() || run.keyboard.is_some())
        {
            launcher_settings.file_roots = vec![run.scope.clone()];
        }
        let launcher = crate::launcher::Launcher::new(
            launcher_settings,
            &search,
            wake.clone(),
            launcher_runtime.isolate,
            launcher_runtime.channel,
        )?;
        hotkeys.set_launcher_shortcut(&launcher.settings.hotkey);
        hotkeys.update_palette(&mut commands);
        let home_history_service = crate::home_history::Service::new(
            wake.clone(),
            benchmark.is_none() && (!launcher_runtime.isolate || launcher_runtime.auxiliary),
        )?;
        let recycle = recycle_ui::RecycleUi::new(wake.clone());
        let mut sidebar_data = crate::sidebar::Data::new(wake.clone())?;
        sidebar_data.network.set_owner_window(window_owner);
        let mut home_history = session.home_history;
        home_history.migrate_recent(session.recent.iter().cloned());
        let native_frame = crate::native_frame::NativeFrame::new(window_owner, &cc.egui_ctx);
        #[cfg(feature = "renderer-glow")]
        let adapter_info = cc
            .gl
            .as_ref()
            .map(|gl| {
                use eframe::glow::HasContext;
                unsafe {
                    format!(
                        "{} · {} · {}",
                        gl.get_parameter_string(eframe::glow::VENDOR),
                        gl.get_parameter_string(eframe::glow::RENDERER),
                        gl.get_parameter_string(eframe::glow::VERSION)
                    )
                }
            })
            .unwrap_or_else(|| "OpenGL unavailable".into());
        #[cfg(feature = "renderer-wgpu")]
        let adapter_info = cc
            .wgpu_render_state
            .as_ref()
            .map(|r| {
                let a = r.adapter.get_info();
                format!("{:?} · {} · {:?}", a.backend, a.name, a.device_type)
            })
            .unwrap_or_else(|| "Wgpu unavailable".into());
        let home = std::env::var_os("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("C:/"));
        let explicit_route = initial
            .as_deref()
            .and_then(crate::navigation::special_route);
        let explicit_home = explicit_route == Some(crate::navigation::Route::Home);
        let initial = initial.filter(|path| crate::navigation::special_route(path).is_none());
        let startup_path = initial.clone();
        let start_home = explicit_home
            || (explicit_route.is_none()
                && startup_path.is_none()
                && session.tabs.is_empty()
                && session
                    .workspace
                    .as_ref()
                    .is_none_or(|state| state.tabs.is_empty()));
        let restored_workspace = if startup_path.is_none() && explicit_route.is_none() {
            session.workspace.clone()
        } else {
            None
        };
        let paths = if let Some(p) = initial {
            vec![p]
        } else if explicit_route.is_some() || session.tabs.is_empty() {
            vec![home.clone()]
        } else {
            session.tabs.into_iter().take(4096).collect()
        };
        let restore_active = session.active.min(paths.len() - 1);
        let mut next_id = 1;
        let mut tabs = vec![];
        for (i, p) in paths.into_iter().enumerate() {
            let mut left = if i == restore_active
                && startup_path.is_none()
                && !launcher_runtime.tray_only
                && !start_home
                && explicit_route.is_none()
                && restored_workspace.is_none()
            {
                Pane::new(next_id, &engine, p)
            } else {
                Pane::dormant(next_id, &engine, p)
            };
            left.attach_search(search.client());
            if let Some(route) = explicit_route
                .clone()
                .or_else(|| start_home.then_some(crate::navigation::Route::Home))
            {
                left.restore_navigation(crate::navigation::Navigation::new(route));
            }
            tabs.push(Tab {
                left,
                right: None,
                vertical: false,
                pending_axis: None,
                split: 0.5,
                reveal: 0.0,
                closing: false,
            });
            next_id += 1;
        }
        let active = session.active.min(tabs.len() - 1);
        let places = Vec::new();
        let sidebar_width = if session.sidebar {
            theme.sidebar_width
        } else {
            0.0
        };
        let desk = workspace_canvas::Desk::new(
            tabs.iter().map(|tab| tab.left.id).collect(),
            tabs[active].left.id,
        );
        let mut app = Self {
            ui_context: cc.egui_ctx.clone(),
            engine,
            search,
            platform,
            theme,
            tabs,
            desk,
            retiring_panes: Vec::new(),
            goto: None,
            retiring_goto: Vec::new(),
            active,
            active_pane: 0,
            next_id,
            sidebar: session.sidebar,
            sidebar_width,
            sidebar_sections: chrome::SidebarSections::from_preferences(
                session.sidebar_collapsed,
                session.sidebar_preferences,
            ),
            main_menu: Default::default(),
            tab_drag: None,
            window_transfer: Default::default(),
            tab_menu: Default::default(),
            tab_menu_id: None,
            tab_menu_anchor: Rect::ZERO,
            layout_size: egui::Vec2::ZERO,
            layout_scale: 1.0,
            resize_until: Instant::now(),
            side_filter: String::new(),
            bookmarks: crate::bookmarks::normalize(session.bookmarks),
            recent: session.recent.into_iter().take(10).collect(),
            home_history,
            home_usage_revision: 0,
            home: home_history_service,
            recycle,
            sidebar_data,
            places,
            options: false,
            updates,
            options_popup: options_ui::OptionsPopup::default(),
            edit: None,
            deletion: Default::default(),
            inspector: false,
            inspector_width: 0.0,
            deferred_editor_input: Vec::new(),
            deferred_editor_target: None,
            resolve_id: 0,
            resolve_target: 0,
            resolve_navigation: 0,
            resolve_intent: ResolveIntent::Navigate,
            toast: None,
            theme_path,
            session_path,
            started,
            first_frame: None,
            frames: VecDeque::with_capacity(240),
            show_stats: false,
            operations_open: false,
            keyboard_help: false,
            pending_focus_exit: None,
            process_samples: vec![],
            metrics_requested: Instant::now() - Duration::from_secs(2),
            session_dirty: false,
            persistence,
            session_signature: 0,
            session_revision: 0,
            saved_revision: 0,
            save_due: None,
            close_requested: None,
            allow_close: false,
            close_failed: false,
            adapter_info,
            frame_count: 0,
            renderer_cpu_ms: 0.0,
            saved_layouts: session.saved_layouts.into_iter().take(16).collect(),
            folder_views: session.folder_views.into_iter().take(128).collect(),
            closed_tabs: VecDeque::new(),
            save_layout_name: None,
            load_layout_open: false,
            inspector_target_width: 320.0,
            assets,
            context,
            commands,
            window_owner,
            native_frame,
            fullscreen: false,
            picker: None,
            picker_layout: false,
            hotkeys_open: false,
            inspector_details: false,
            inspector_view: None,
            inspector_source: None,
            batch: None,
            hotkeys,
            benchmark,
            launcher,
            launcher_open: false,
            launcher_positioned: false,
            launcher_initialized: false,
            launcher_focused: false,
            explorer_visible: !launcher_runtime.tray_only,
            background_allowed: !launcher_runtime.isolate,
            launcher_auxiliary: launcher_runtime.auxiliary,
            launcher_secondary: launcher_runtime.secondary,
        };
        app.options_popup
            .configure_themes(app.theme_path.clone(), app.window_owner);
        if let Some(state) = restored_workspace.filter(|s| !s.tabs.is_empty()) {
            app.restore_workspace(state);
        }
        if let Some(path) = startup_path {
            // Resolve both directory and file arguments on the independent path worker.
            app.pane_mut().loading = true;
            app.action(Action::GoPath(path));
        }
        if launcher_runtime.open_launcher {
            app.open_launcher(launcher_runtime.open_settings);
        }
        if let Some(run) = &mut app.benchmark {
            run.app_ready_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
        }
        Ok(app)
    }
    fn pane(&self) -> &Pane {
        if self.active_pane == 1 {
            self.tabs[self.active]
                .right
                .as_ref()
                .unwrap_or(&self.tabs[self.active].left)
        } else {
            &self.tabs[self.active].left
        }
    }
    fn pane_mut(&mut self) -> &mut Pane {
        let tab = &mut self.tabs[self.active];
        if self.active_pane == 1 && tab.right.is_some() {
            tab.right.as_mut().unwrap()
        } else {
            &mut tab.left
        }
    }
    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast = Some(Toast {
            text: text.into(),
            until: Instant::now() + Duration::from_secs(if error { 8 } else { 4 }),
            error,
            opacity: 0.0,
            scroll_reset: true,
            suspended_at: None,
        });
    }
    fn job(&mut self, j: Job) {
        if self.pane().is_recycle_bin() && !recycle_ui::job_allowed(&j) {
            return;
        }
        if let Err(e) = self.platform.try_submit(j) {
            self.toast(e, true);
        }
    }
    fn remember(&mut self, p: PathBuf) {
        self.recent.retain(|x| !crate::bookmarks::path_eq(x, &p));
        self.recent.push_front(p);
        self.recent.truncate(10);
        self.session_dirty = true;
    }
    fn new_tab(&mut self, path: PathBuf) {
        let mut pane = if let Some(route) = crate::navigation::special_route(&path) {
            let mut pane = Pane::dormant(self.next_id, &self.engine, self.pane().path.clone());
            pane.restore_navigation(crate::navigation::Navigation::new(route));
            pane
        } else {
            Pane::new(self.next_id, &self.engine, path)
        };
        pane.attach_search(self.search.client());
        self.tabs
            .iter()
            .flat_map(|tab| std::iter::once(&tab.left).chain(tab.right.iter()))
            .any(|existing| pane.prime_presented_from(existing));
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
        self.desk.add(id);
        self.active = self.tabs.len() - 1;
        self.active_pane = 0;
        self.session_dirty = true;
    }
    fn close_tab(&mut self, i: usize) {
        let Some(id) = self.tabs.get(i).map(|tab| tab.left.id) else {
            return;
        };
        let Some(at) = self.desk.group_for(id) else {
            return;
        };
        if self.desk.groups[at].tabs.len() == 1 {
            self.close_workspace(self.desk.groups[at].id);
            return;
        }
        let was_active = self.tabs[self.active].left.id;
        let group = &mut self.desk.groups[at];
        let position = group.tabs.iter().position(|tab| *tab == id).unwrap();
        group.tabs.remove(position);
        if group.active == id {
            group.active = group.tabs[position.saturating_sub(1).min(group.tabs.len() - 1)];
        }
        let focus = if was_active == id {
            group.active
        } else {
            was_active
        };
        let tab = self.tabs.remove(i);
        self.closed_tabs.push_front(Self::tab_state(&tab));
        self.closed_tabs.truncate(64);
        self.retire_pane(tab.left);
        if let Some(right) = tab.right {
            self.retire_pane(right);
        }
        self.focus_pane(focus);
        self.session_dirty = true;
    }
    fn retire_pane(&mut self, mut pane: Pane) {
        pane.set_active(false);
        pane.browser.close();
        if let Err(pane) = self.engine.defer(pane) {
            self.retiring_panes.push(pane);
        }
    }
    fn prepare_goto(&mut self) -> bool {
        self.commands.release_focus(&self.ui_context);
        self.commands.close();
        self.context.release_focus(&self.ui_context);
        self.context.close();
        if self.retiring_goto.len() >= 4 {
            return false;
        }
        if let Some(mut old) = self.goto.take() {
            old.release_focus(&self.ui_context);
            old.close();
            if let Err(old) = self.engine.defer(old) {
                self.retiring_goto.push(old);
            }
        }
        true
    }
    fn open_goto(&mut self, initial: Option<PathBuf>) {
        if !self.prepare_goto() {
            return;
        }
        let return_to = self.ui_context.memory(|memory| memory.focused());
        let mut common: Vec<crate::goto::Location> = self
            .places
            .iter()
            .map(|(icon, label, path)| crate::goto::Location {
                icon: *icon,
                label: label.clone(),
                path: path.clone(),
            })
            .collect();
        for path in self.bookmarks.iter().chain(self.recent.iter()) {
            if !common.iter().any(|p| p.path == *path) {
                common.push(crate::goto::Location {
                    icon: Icon::Folder,
                    label: path_label(path),
                    path: path.clone(),
                });
            }
        }
        self.goto = Some(crate::goto::GoTo::new(
            &self.engine,
            self.pane().id,
            initial,
            common,
        ));
        if let (Some(popup), Some(return_to)) = (&mut self.goto, return_to) {
            popup.set_return_focus(return_to);
        }
        crate::keyboard::request_id(&self.ui_context, Id::new("goto-input"));
    }
    fn inspect(&mut self, path: PathBuf) {
        if self.pane().is_recycle_bin() {
            return;
        }
        self.inspector = true;
        if self.inspector_view.is_none() {
            let context = self.ui_context.clone();
            match crate::inspector::Inspector::new(Arc::new(move || context.request_repaint())) {
                Ok(mut view) => {
                    view.set_details(self.inspector_details);
                    self.inspector_view = Some(view);
                }
                Err(error) => {
                    self.toast(error.to_string(), true);
                    return;
                }
            }
        }
        if let Err(error) = self.inspector_view.as_mut().unwrap().open_path(path) {
            self.toast(error.to_string(), true);
        }
    }
    fn sync_inspector(&mut self) {
        if self.pane().is_recycle_bin() {
            return;
        }
        if self.pane().is_home() && self.pane().primary_selected_path().is_none() {
            return;
        }
        let pane = self.pane();
        let selected = pane.primary_selected_path();
        let path = selected.as_deref().unwrap_or(&pane.path);
        // A finished browser revision also invalidates an unchanged preview.
        // Ignore partial scans and recursive search-window permutations.
        let revision = if !pane.loading && !pane.recursive {
            pane.snapshot.as_ref().map(|snapshot| snapshot.id)
        } else {
            self.inspector_source
                .as_ref()
                .filter(|(id, previous, _)| *id == pane.id && previous == path)
                .and_then(|(_, _, revision)| *revision)
        };
        let refresh = revision.is_some()
            && self
                .inspector_source
                .as_ref()
                .is_some_and(|(_, previous, old)| previous == path && *old != revision);
        let requested = self
            .inspector_source
            .as_ref()
            .is_none_or(|(id, previous, old)| {
                *id != pane.id || previous != path || *old != revision
            })
            .then(|| (pane.id, path.to_path_buf(), revision));
        if let Some((id, path, revision)) = requested {
            self.inspector_source = Some((id, path.clone(), revision));
            if self.inspector_view.as_ref().and_then(|view| view.path()) != Some(path.as_path()) {
                self.inspect(path);
            } else if refresh
                && let Some(view) = &mut self.inspector_view
                && let Err(error) = view.refresh()
            {
                self.toast(error.to_string(), true);
            }
        }
    }
    fn action(&mut self, a: Action) {
        if self.pane().is_recycle_bin() && !recycle_ui::action_allowed(&a) {
            return;
        }
        match a {
            Action::RecycleBin(intent) => self.recycle_action(intent),
            Action::Address => self.open_goto(if self.pane().is_recycle_bin() {
                Some(self.pane().location_text().into())
            } else {
                (!self.pane().is_virtual()).then(|| self.pane().path.clone())
            }),
            Action::EditPath(path) => self.open_goto(Some(path)),
            Action::BreadcrumbChildren {
                path,
                current,
                anchor,
                trigger,
            } => {
                if self.prepare_goto() {
                    self.goto = Some(crate::goto::GoTo::folder_menu(
                        &self.engine,
                        self.pane().id,
                        path,
                        current,
                        anchor,
                        trigger,
                        self.pane().breadcrumb_trail(),
                    ));
                    crate::keyboard::request_id(&self.ui_context, Id::new("breadcrumb-filter"));
                }
            }
            Action::BreadcrumbAncestors {
                paths,
                anchor,
                trigger,
            } => {
                if self.prepare_goto() {
                    let common = paths
                        .into_iter()
                        .map(|path| crate::goto::Location {
                            icon: if path.parent().is_none() {
                                Icon::Drive
                            } else {
                                Icon::Folder
                            },
                            label: path_label(&path),
                            path,
                        })
                        .collect();
                    self.goto = Some(crate::goto::GoTo::ancestor_menu(
                        &self.engine,
                        self.pane().id,
                        common,
                        anchor,
                        trigger,
                    ));
                    crate::keyboard::request_id(&self.ui_context, Id::new("breadcrumb-filter"));
                }
            }
            Action::Navigate(p) => {
                self.pane_mut().navigate(p, true);
                self.apply_folder_view();
            }
            Action::GoPath(path) => {
                if let Some(route) = crate::navigation::special_route(&path) {
                    match route {
                        crate::navigation::Route::Home => self.navigate_home(),
                        crate::navigation::Route::RecycleBin => self.navigate_recycle_bin(),
                        crate::navigation::Route::Folder(_) => unreachable!(),
                    }
                    return;
                }
                self.resolve_intent = ResolveIntent::Navigate;
                self.pane_mut().resolving = true;
                self.resolve_id += 1;
                self.resolve_target = self.pane().id;
                self.resolve_navigation = self.pane().navigation_epoch();
                self.job(Job::Resolve {
                    path,
                    id: self.resolve_id,
                });
            }
            Action::Open(p) => self.job(Job::Open(p)),
            Action::Reveal(p) => self.job(Job::Reveal(p)),
            Action::NewTab(p) => self.new_tab(p),
            Action::NewWindow(path) => self.job(Job::NewWindow(path)),
            Action::MoveTabToNewWindow => {
                self.move_tab_to_new_window(self.tabs[self.active].left.id)
            }
            Action::Copy(paths, cut) => {
                if !paths.is_empty() {
                    self.job(Job::WriteClipboard {
                        sources: paths,
                        cut,
                    });
                }
            }
            Action::Paste(destination) => self.job(Job::ReadClipboard { destination }),
            Action::Rename(path) => {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.edit = Some(EditDialog {
                    path,
                    name,
                    create: false,
                    create_file: false,
                    focus: true,
                });
            }
            Action::RenameMany(mut paths) => {
                if paths.len() == 1 {
                    self.action(Action::Rename(paths.remove(0)));
                } else if !paths.is_empty() {
                    if self.batch.is_none() {
                        let context = self.ui_context.clone();
                        match crate::batch::BatchRename::new_with_history(
                            Arc::new(move || context.request_repaint()),
                            self.platform.history_handle(),
                        ) {
                            Ok(batch) => self.batch = Some(batch),
                            Err(e) => {
                                self.toast(e.to_string(), true);
                                return;
                            }
                        }
                    }
                    let origin = self.pane().id;
                    self.batch.as_mut().unwrap().open_for_pane(origin, paths);
                }
            }
            Action::CopyPaths(paths) => {
                let text = paths
                    .iter()
                    .map(|p| format!("\"{}\"", p.to_string_lossy()))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.ui_context.copy_text(text);
            }
            Action::Notify(message) => self.toast(message, true),
            Action::Properties(paths) => self.job(Job::Properties(paths)),
            Action::NewFolder(path) => {
                self.edit = Some(EditDialog {
                    path,
                    name: "New folder".into(),
                    create: true,
                    create_file: false,
                    focus: true,
                })
            }
            Action::Recycle(paths) => self.request_delete(paths, crate::pane::DeleteMode::Recycle),
            Action::Delete { paths, mode } => self.request_delete(paths, mode),
            Action::Inspect(p) => {
                if self.inspector
                    && self
                        .inspector_view
                        .as_ref()
                        .and_then(|v| v.path())
                        .is_some_and(|path| path == p)
                {
                    self.inspector = false;
                } else {
                    self.inspect(p);
                }
            }
            Action::ToggleInspector => {
                if self.pane().is_home() && self.pane().primary_selected_path().is_none() {
                    self.toast("Select a recent item to inspect it", false);
                    return;
                }
                if !crate::pane::inspector_available(&self.ui_context, &self.theme) {
                    self.toast("Widen the window to show the inspector", false);
                    return;
                }
                self.inspector = !self.inspector;
                if self.inspector {
                    self.inspect(
                        self.pane()
                            .primary_selected_path()
                            .unwrap_or_else(|| self.pane().path.clone()),
                    );
                }
            }
            Action::Bookmark(p) => {
                if crate::bookmarks::toggle(&mut self.bookmarks, p) {
                    self.session_dirty = true;
                } else {
                    self.toast(
                        "Bookmarks are full (32). Remove one before adding another.",
                        false,
                    );
                }
            }
            Action::FolderOptions => self.job(Job::FolderOptions),
            Action::Context {
                paths,
                background,
                position,
            } => {
                self.commands.release_focus(&self.ui_context);
                self.commands.close();
                self.context.open_for_pane_with_options(
                    self.pane().id,
                    self.window_owner,
                    paths,
                    background,
                    position,
                    foldex_shell::MenuOptions {
                        extended: self.ui_context.input(|input| input.modifiers.shift),
                    },
                );
                self.context
                    .set_new_directory((!self.pane().is_home()).then(|| self.pane().path.clone()));
                if let Some(id) = self.ui_context.memory(|memory| memory.focused()) {
                    self.context.set_return_focus(id);
                }
                crate::keyboard::request_id(&self.ui_context, Id::new("context-search"));
            }
            Action::Split(vertical) => self.split_current_route(vertical),
            Action::SplitAt { path, vertical } => {
                self.split_workspace(path.clone(), vertical);
                self.remember(path);
                self.apply_folder_view();
            }
            Action::ClosePane => self.close_workspace(self.desk.focused),
        }
    }
    fn poll(&mut self, ctx: &egui::Context) {
        self.service_recycle(ctx);
        if self.engine.managed_bytes() > 48 * 1024 * 1024 {
            let mut parked = vec![];
            let visible = self.desk.visible();
            for tab in &mut self.tabs {
                if parked.len() >= 4 {
                    break;
                }
                if visible.contains(&tab.left.id) {
                    continue;
                }
                if let Some(pane) = tab.left.park_model(&self.engine) {
                    parked.push(pane);
                }
            }
            for pane in parked {
                self.retire_pane(pane);
            }
        }
        while let Some(popup) = self.retiring_goto.pop() {
            if let Err(popup) = self.engine.defer(popup) {
                self.retiring_goto.push(popup);
                break;
            }
        }
        if !self.retiring_goto.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        while let Some(pane) = self.retiring_panes.pop() {
            if let Err(pane) = self.engine.defer(pane) {
                self.retiring_panes.push(pane);
                break;
            }
        }
        if !self.retiring_panes.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        let mut completed = vec![];
        let mut visits = vec![];
        let visible = self.desk.visible();
        for tab in &mut self.tabs {
            tab.left
                .set_active(self.explorer_visible && visible.contains(&tab.left.id));
            tab.left.poll();
            if let Some(path) = tab.left.take_folder_visit() {
                visits.push(path);
            }
            while let Some(action) = tab.left.drain_action() {
                completed.push((tab.left.id, action));
            }
            if let Some(p) = &mut tab.right {
                p.set_active(self.explorer_visible && visible.contains(&p.id));
                p.poll();
                if let Some(path) = p.take_folder_visit() {
                    visits.push(path);
                }
                while let Some(action) = p.drain_action() {
                    completed.push((p.id, action));
                }
            }
        }
        for path in visits {
            self.remember(path.clone());
            self.record_home_usage(path, crate::home_history::Access::Visited);
        }
        for (origin, action) in completed {
            let previous = self.pane().id;
            if self.focus_pane(origin) {
                self.action(action);
            }
            if !self.focus_pane(previous) {
                self.active = self.active.min(self.tabs.len() - 1);
                self.active_pane = 0;
            }
        }
        for _ in 0..8 {
            let Some(e) = self.platform.try_recv() else {
                break;
            };
            match e {
                platform::Event::Opened(path) => {
                    self.record_home_usage(path, crate::home_history::Access::Opened)
                }
                platform::Event::ExplorerIntegration { id, result } => {
                    if let Err(error) = &result {
                        self.toast(error.clone(), true);
                    }
                    self.options_popup.finish_integration(id, result);
                }
                platform::Event::Done { message, refresh } => {
                    self.toast(message, false);
                    if refresh {
                        for (i, t) in self.tabs.iter_mut().enumerate() {
                            if i == self.active {
                                t.left.refresh();
                            } else {
                                t.left.needs_refresh = true;
                            }
                            if let Some(p) = &mut t.right {
                                if i == self.active {
                                    p.refresh();
                                } else {
                                    p.needs_refresh = true;
                                }
                            }
                        }
                    }
                }
                platform::Event::Failed(e) => {
                    if !self.explorer_visible {
                        self.show_explorer(ctx);
                    }
                    self.toast(e, true);
                }
                platform::Event::ResolveFailed { id, message } => {
                    if id == self.resolve_id {
                        for tab in &mut self.tabs {
                            let pane = if tab.left.id == self.resolve_target {
                                Some(&mut tab.left)
                            } else {
                                tab.right.as_mut().filter(|p| p.id == self.resolve_target)
                            };
                            if let Some(p) =
                                pane.filter(|p| p.navigation_epoch() == self.resolve_navigation)
                            {
                                p.resolving = false;
                                p.loading = false;
                                p.error = Some(message.clone());
                            }
                        }
                        self.toast(message, true);
                    }
                }
                platform::Event::Metrics {
                    id,
                    private_bytes,
                    working_set_bytes,
                    handles,
                    cpu_seconds,
                } => {
                    let _request_identity = id;
                    if self.process_samples.len() >= 120 {
                        self.process_samples.remove(0);
                    }
                    self.process_samples.push(ProcessSample {
                        elapsed_seconds: self.started.elapsed().as_secs_f64(),
                        private_bytes,
                        working_set_bytes,
                        handles,
                        cpu_seconds,
                        ui_frames: self.frame_count,
                        renderer_cpu_ms: self.renderer_cpu_ms,
                    });
                }
                platform::Event::Resolved { id, path, is_dir } => {
                    if id == self.resolve_id {
                        let mut follow_up = None;
                        for tab in &mut self.tabs {
                            let pane = if tab.left.id == self.resolve_target {
                                Some(&mut tab.left)
                            } else {
                                tab.right.as_mut().filter(|p| p.id == self.resolve_target)
                            };
                            if let Some(p) =
                                pane.filter(|p| p.navigation_epoch() == self.resolve_navigation)
                            {
                                p.resolving = false;
                                if self.resolve_intent == ResolveIntent::Paste {
                                    follow_up = Some(Job::ReadClipboard {
                                        destination: if is_dir {
                                            path.clone()
                                        } else {
                                            path.parent().unwrap_or(&p.path).to_path_buf()
                                        },
                                    });
                                } else if is_dir {
                                    p.navigate(path.clone(), true);
                                    if let Some(view) = crate::workspace::find_folder_view(
                                        &self.folder_views,
                                        &p.path,
                                    ) {
                                        view.apply(p);
                                    }
                                } else if self.resolve_intent == ResolveIntent::Activate {
                                    follow_up = Some(Job::Open(path.clone()));
                                } else if let (Some(parent), Some(name)) =
                                    (path.parent(), path.file_name())
                                {
                                    p.navigate(parent.to_owned(), true);
                                    p.pending_selection = Some(name.to_owned());
                                    p.query.show_hidden = true;
                                }
                                break;
                            }
                        }
                        if let Some(job) = follow_up {
                            self.job(job);
                        }
                    }
                }
                platform::Event::Preview { id, content, .. } => {
                    if id == u64::MAX {
                        if let PreviewContent::Text(s) = content {
                            match Theme::read(&s) {
                                Ok(mut t) => {
                                    t.resolve_system(ctx.system_theme());
                                    self.theme = t;
                                    self.options_popup.theme_palette_changed(&mut self.theme);
                                    self.theme.apply(ctx);
                                    self.toast("Theme reloaded", false);
                                }
                                Err(e) => self.toast(format!("Theme: {e}"), true),
                            }
                        }
                    }
                }
            }
        }
    }
    fn editing(ctx: &egui::Context) -> bool {
        (ctx.egui_wants_keyboard_input() && !crate::keyboard::any_listing_owns(ctx))
            || ctx
                .plugin_opt::<egui::text_selection::LabelSelectionState>()
                .is_some_and(|p| p.lock().has_selection())
    }
    fn keyboard(&mut self, ctx: &egui::Context, late: bool) {
        if !ctx.input(|input| input.focused) {
            let _ = crate::native_keys::take();
            self.hotkeys.cancel_pending();
            return;
        }
        let native = crate::native_keys::take();
        if self.hotkeys.is_recording() || (!self.popup_active() && !Self::editing(ctx)) {
            ctx.input_mut(|i| crate::native_keys::merge(&mut i.events, native));
        }
        if self.hotkeys.is_recording() {
            return;
        }
        if self.show_stats
            && ctx.input_mut(|i| i.consume_key(Modifiers::CTRL | Modifiers::ALT, Key::P))
        {
            foldex_ui::components::close_dialog(ctx, Id::new("performance-dialog"));
            return;
        }
        if self.popup_active() {
            self.hotkeys.cancel_pending();
            return;
        }
        // Focus follows keyboard navigation between split panes, just as a
        // pointer press chooses its pane before dispatching file operations.
        self.sync_keyboard_pane(ctx);
        if ctx.memory(|memory| memory.focused().is_none()) {
            crate::keyboard::request_id(ctx, crate::keyboard::listing_id(self.pane().id));
        }
        let first_stroke = ctx.input(|input| {
            input
                .events
                .iter()
                .take_while(|event| {
                    !matches!(event, egui::Event::Text(text)
                        if crate::keyboard::printable_search_text(text))
                })
                .find_map(|event| match event {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } if !matches!(
                        key,
                        Key::ShiftLeft
                            | Key::ShiftRight
                            | Key::ControlLeft
                            | Key::ControlRight
                            | Key::AltLeft
                            | Key::AltRight
                            | Key::SuperLeft
                            | Key::SuperRight
                    ) =>
                    {
                        Some((*key, *modifiers))
                    }
                    _ => None,
                })
        });
        if self.pane().is_home()
            && crate::keyboard::listing_owns(ctx, self.pane().id)
            && first_stroke.is_some_and(|(key, modifiers)| {
                modifiers.matches_exact(Modifiers::NONE)
                    && matches!(
                        key,
                        Key::Space
                            | Key::Enter
                            | Key::ArrowUp
                            | Key::ArrowDown
                            | Key::ArrowLeft
                            | Key::ArrowRight
                            | Key::Home
                            | Key::End
                            | Key::PageUp
                            | Key::PageDown
                    )
            })
        {
            self.hotkeys.cancel_pending();
            return;
        }
        let context_first = first_stroke.is_some_and(|(key, modifiers)| {
            key == Key::F10 && modifiers.matches_exact(Modifiers::SHIFT)
        });
        if context_first {
            // Open the local context surface before later commands in this
            // input batch. Its search/editor owns any following keys.
            if !self.pane().is_home() && crate::keyboard::listing_owns(ctx, self.pane().id) {
                if let Some(action) = self.pane_mut().keyboard_navigation(ctx) {
                    self.action(action);
                }
            }
            self.hotkeys.cancel_pending();
            return;
        }
        for _ in 0..16 {
            let Some(command) = self.dispatch_keyboard_command(ctx, late) else {
                break;
            };
            let filter_already_owned = late
                && command == crate::commands::Command::Filter
                && self.pane().filter_input_frame == Some(ctx.cumulative_frame_nr());
            self.execute_command(ctx, command);
            if late && command == crate::commands::Command::Filter {
                // The clicked panel was already painted. Carry only editor payloads
                // to its next frame. TextEdit reads rather than removes text events;
                // replaying payloads it already received would duplicate the query.
                if !filter_already_owned {
                    self.defer_filter_input(ctx);
                }
                self.hotkeys.cancel_pending();
                ctx.request_repaint();
                return;
            }
            if self.popup_active() {
                self.hotkeys.cancel_pending();
                ctx.input_mut(|i| crate::native_keys::strip_synthetic(&mut i.events));
                return;
            }
        }
        let ctrl = Modifiers::CTRL;
        let cs = Modifiers::CTRL | Modifiers::SHIFT;
        let shortcut = |m, k| crate::commands::take_key(ctx, m, k);
        if shortcut(cs, Key::Tab) {
            self.next_group_tab(true);
            crate::keyboard::request_id(ctx, crate::keyboard::listing_id(self.pane().id));
        } else if shortcut(ctrl, Key::Tab) {
            self.next_group_tab(false);
            crate::keyboard::request_id(ctx, crate::keyboard::listing_id(self.pane().id));
        }
        if shortcut(Modifiers::CTRL | Modifiers::ALT, Key::P) {
            self.show_stats = !self.show_stats;
        }
        if !crate::keyboard::composing(ctx) {
            let backwards = shortcut(Modifiers::SHIFT, Key::Tab);
            if backwards || shortcut(Modifiers::NONE, Key::Tab) {
                let editor_payload = ctx.input(|input| {
                    input.events.iter().any(|event| {
                        matches!(
                            event,
                            egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Ime(_)
                        )
                    })
                });
                if self.pane().focus_filter || ctx.text_edit_focused() && editor_payload {
                    // Let the current editor receive this batch's text before
                    // moving focus. This also handles Ctrl+F, query, Tab batched
                    // into one native frame without editing the old file list.
                    self.pending_focus_exit =
                        Some((backwards, ctx.cumulative_frame_nr().saturating_sub(1)));
                    ctx.request_repaint();
                    return;
                }
                if crate::keyboard::tab(ctx, &self.keyboard_regions(true), backwards) {
                    self.sync_keyboard_pane(ctx);
                    return;
                }
            }
        }
        if Self::editing(ctx)
            || self.inspector
                && self
                    .inspector_view
                    .as_ref()
                    .is_some_and(|preview| preview.owns_navigation(ctx))
        {
            return;
        }
        if !crate::keyboard::listing_owns(ctx, self.pane().id) || self.pane().is_home() {
            return;
        }
        if shortcut(Modifiers::NONE, Key::Backspace) {
            self.pane_mut().up();
        }
        if shortcut(Modifiers::NONE, Key::Escape) {
            self.assets.cancel_cut();
            let p = self.pane_mut();
            if !p.filter.is_empty() {
                p.filter.clear();
                p.refilter();
            } else if p.recursive {
                p.toggle_recursive();
            } else {
                p.clear_selection();
            }
        }
        if let Some(action) = self.pane_mut().keyboard_navigation(ctx) {
            self.action(action);
        }
    }

    fn defer_filter_input(&mut self, ctx: &egui::Context) {
        const MAX_EVENTS: usize = 32;
        const MAX_BYTES: usize = 4096;
        self.deferred_editor_input.clear();
        self.deferred_editor_target = Some(self.pane().id);
        let mut remaining = MAX_BYTES;
        ctx.input_mut(|input| {
            input.events.retain_mut(|event| {
                let text = match event {
                    egui::Event::Text(text)
                    | egui::Event::Paste(text)
                    | egui::Event::Ime(egui::ImeEvent::Commit(text))
                    | egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => Some(text),
                    egui::Event::Ime(_) => None,
                    _ => return true,
                };
                if self.deferred_editor_input.len() >= MAX_EVENTS || remaining == 0 {
                    return false;
                }
                if let Some(text) = text {
                    let mut end = text.len().min(remaining);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    remaining -= end;
                }
                if let egui::Event::Ime(egui::ImeEvent::Preedit {
                    text,
                    active_range_chars: Some(range),
                }) = event
                {
                    let chars = text.chars().count();
                    range.start = range.start.min(chars);
                    range.end = range.end.min(chars).max(range.start);
                }
                self.deferred_editor_input
                    .push(std::mem::replace(event, egui::Event::Text(String::new())));
                false
            })
        });
        if self.deferred_editor_input.is_empty() {
            self.deferred_editor_target = None;
        }
    }

    fn inspector_ui(&mut self, ui: &mut Ui, rect: Rect) {
        if self.pane().is_recycle_bin() {
            return;
        }
        if self.pane().is_home() && self.pane().primary_selected_path().is_none() {
            return;
        }
        let grip =
            Rect::from_center_size(pos2(rect.left(), rect.center().y), vec2(7.0, rect.height()));
        let resize = ui
            .interact(grip, Id::new("inspector-resize"), Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        let delta = foldex_ui::components::divider_delta(ui, grip, &resize);
        if delta.x != 0.0 {
            self.inspector_target_width = (self.inspector_target_width - delta.x).clamp(
                180.0 * self.theme.metrics().scale,
                (self.layout_size.x * 0.5).max(180.0 * self.theme.metrics().scale),
            );
        }
        if self.inspector_view.is_none() {
            self.inspect(
                self.pane()
                    .primary_selected_path()
                    .unwrap_or_else(|| self.pane().path.clone()),
            );
        }
        if let Some(view) = &mut self.inspector_view {
            let response = view.ui(ui, rect, &self.theme);
            if response.close {
                self.inspector = false;
            }
            if let Some(path) = response.open {
                self.job(Job::Open(path));
            }
            if let Some(path) = response.go_to {
                self.action(Action::GoPath(path));
            }
            if let Some(path) = response.reveal {
                self.job(Job::Reveal(path));
            }
        }
        foldex_ui::components::resize_divider(ui, grip, &resize, &self.theme, true);
    }
    fn dialogs(&mut self, ctx: &egui::Context) {
        self.keyboard_help_ui(ctx);
        let mut apply_theme = false;
        let mut save_theme = false;
        if self.options {
            let response = self.options_popup.ui_with_services(
                ctx,
                &mut self.theme,
                &mut self.launcher,
                self.sidebar_sections.preferences_mut(),
            );
            if response.sidebar_changed {
                self.service_sidebar_network();
                self.session_dirty = true;
                self.save_due = Some(Instant::now() + Duration::from_millis(300));
            }
            if let Some(request) = response.integration {
                if let Err(error) = self.platform.try_submit(Job::ExplorerIntegration {
                    id: request.id,
                    enabled: request.enabled,
                }) {
                    self.options_popup
                        .finish_integration(request.id, Err(error.clone()));
                    self.toast(error, true);
                }
            }
            apply_theme = response.apply;
            save_theme = response.save;
            if response.closed {
                self.options = false;
            }
            if response.updates {
                self.options = false;
                self.updates.open = true;
            }
        }
        if apply_theme {
            self.theme.remember_active_palette();
            self.theme.apply(ctx);
            self.options_popup.persist_theme(ctx, &self.theme, false);
        }
        if save_theme {
            self.options_popup.persist_theme(ctx, &self.theme, true);
        }
        let can_install = self.can_install_update();
        self.updates.ui(ctx, &self.theme, can_install);
        if self.updates.exit_pending && !self.updates.open {
            self.cancel_update_exit(
                ctx,
                "Update installation was cancelled. Your current version is unchanged.",
            );
        }
        let mut edit_result = None;
        let mut edit_cancel = false;
        if let Some(d) = &mut self.edit {
            let m = self.theme.metrics();
            let width =
                (400.0 * m.scale).min((ctx.content_rect().width() - 32.0 * m.scale).max(160.0));
            let mut edit_open = true;
            foldex_ui::components::dialog(
                ctx,
                Id::new("rename-dialog"),
                &self.theme,
                if d.create_file {
                    "New file"
                } else if d.create {
                    "New folder"
                } else {
                    "Rename"
                },
                width,
                &mut edit_open,
                |ui| {
                    ui.spacing_mut().item_spacing = vec2(m.gap, m.gap);
                    let width = ui.available_width();
                    let (path_rect, path_hit) = ui.allocate_exact_size(
                        vec2(width, (self.theme.font_size + 8.0).max(20.0)),
                        Sense::hover(),
                    );
                    let caption = Theme {
                        font_size: (self.theme.font_size - 1.0).max(10.0),
                        ..self.theme.clone()
                    };
                    foldex_ui::components::elided_label(
                        ui.painter(),
                        path_rect,
                        &caption,
                        &d.path.to_string_lossy(),
                        self.theme.dim(),
                        false,
                    );
                    path_hit.on_hover_text(d.path.to_string_lossy());
                    let (field, _) = ui.allocate_exact_size(vec2(width, m.control), Sense::hover());
                    let r = foldex_ui::components::input_box(
                        ui,
                        field,
                        Id::new("rename-name"),
                        &self.theme,
                        &mut d.name,
                        "",
                        1024,
                    )
                    .response;
                    if d.focus && foldex_ui::components::can_focus(ui) {
                        r.request_focus();
                        d.focus = ui.is_sizing_pass() || !r.has_focus();
                    }
                    ui.add_space(m.gap);
                    ui.horizontal_wrapped(|ui| {
                        if foldex_ui::components::text_button(
                            ui,
                            &self.theme,
                            if d.create_file {
                                "Create file"
                            } else if d.create {
                                "Create folder"
                            } else {
                                "Rename"
                            },
                            true,
                        )
                        .clicked()
                            || r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter))
                        {
                            edit_result =
                                Some((d.path.clone(), d.name.clone(), d.create, d.create_file));
                        }
                        if foldex_ui::components::text_button(ui, &self.theme, "Cancel", false)
                            .clicked()
                        {
                            ui.close();
                        }
                    });
                },
            );
            edit_cancel |= !edit_open;
        }
        if edit_cancel {
            self.edit = None;
        }
        if let Some((path, name, create, file)) = edit_result {
            self.edit = None;
            self.job(if file {
                Job::NewFile { parent: path, name }
            } else if create {
                Job::NewFolder { parent: path, name }
            } else {
                Job::Rename { path, name }
            });
        }
        self.delete_confirmation_ui(ctx);
        self.recycle_dialog_ui(ctx);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use eframe::App;
    use std::sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    };
    pub(super) fn fixture_app() -> (Foldex, egui::Context, PathBuf) {
        static BASE: OnceLock<PathBuf> = OnceLock::new();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let base = BASE.get_or_init(|| {
            let p =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/app-test-fixtures");
            std::fs::create_dir_all(&p).unwrap();
            p
        });
        let root = base.join(format!(
            "owned-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("Selected.txt");
        std::fs::write(&file, b"generated fixture").unwrap();
        let ctx = egui::Context::default();
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = Foldex::new(
            &cc,
            Theme::default(),
            Some(file),
            root.join("theme.toml"),
            root.join("session.json"),
            root.join("preferences.json"),
            Session::default(),
            Instant::now(),
            None,
            crate::launcher::Runtime {
                isolate: true,
                auxiliary: false,
                tray_only: false,
                open_launcher: false,
                open_settings: false,
                channel: None,
                secondary: None,
            },
        )
        .unwrap();
        (app, ctx, root)
    }
    fn pump(app: &mut Foldex, ctx: &egui::Context, n: usize, close: bool) -> egui::FullOutput {
        let mut input = egui::RawInput {
            time: Some(n as f64 / 60.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 760.0))),
            ..Default::default()
        };
        if close {
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(egui::ViewportEvent::Close);
        }
        let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
        // This headless check exercises layout/scheduling only; there is no GPU uploader.
        output.textures_delta.clear();
        output
    }
    pub(super) fn ready_fixture() -> (Foldex, egui::Context, usize) {
        let (mut app, ctx, _) = fixture_app();
        let deadline = Instant::now() + Duration::from_secs(4);
        for n in 0..2000 {
            pump(&mut app, &ctx, n, false);
            if !app.pane().loading && app.pane().primary_selected_path().is_some() {
                return (app, ctx, n + 1);
            }
            assert!(
                Instant::now() < deadline,
                "Generated input fixture did not finish loading"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("Generated input fixture did not load");
    }
    #[test]
    fn operations_command_opens_a_modal_with_a_bounded_history() {
        let (mut app, ctx, frame) = ready_fixture();
        app.execute_command(&ctx, crate::commands::Command::Operations);
        assert!(app.operations_open && app.popup_active());
        for index in 0..3 {
            ctx.run_ui(
                egui::RawInput {
                    time: Some((frame + index) as f64 / 60.0),
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 760.0))),
                    ..Default::default()
                },
                |ui| app.operations_ui(ui.ctx()),
            )
            .drop_without_applying_deltas();
        }
        app.operations_open = false;
        assert!(!app.operations_open);
    }

    #[test]
    fn root_panel_widths_advance_once_per_frame_and_later_resize_still_snaps() {
        for resize_on_second_pass in [false, true] {
            let (mut app, ctx, frame) = ready_fixture();
            app.sidebar = false;
            app.sidebar_width = 200.0;
            app.inspector = true;
            app.inspector_width = 0.0;
            app.layout_size = vec2(1200.0, 760.0);
            app.layout_scale = app.theme.metrics().scale;
            app.resize_until = Instant::now() - Duration::from_secs(1);
            let mut widths = Vec::new();
            ctx.run_ui(
                egui::RawInput {
                    time: Some(frame as f64 / 60.0),
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 760.0))),
                    ..Default::default()
                },
                |ui| {
                    let later_pass = ui.ctx().current_pass_index() > 0;
                    let width = if later_pass && resize_on_second_pass {
                        1100.0
                    } else {
                        1200.0
                    };
                    ui.scope_builder(
                        UiBuilder::new()
                            .max_rect(Rect::from_min_size(egui::Pos2::ZERO, vec2(width, 760.0))),
                        |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()),
                    );
                    widths.push((app.sidebar_width, app.inspector_width));
                    if !later_pass {
                        ui.ctx().request_discard("exercise the root sizing pass");
                    }
                },
            )
            .drop_without_applying_deltas();
            assert!(
                widths.len() >= 2,
                "the root regression must run multiple passes"
            );
            assert!(widths[0].0 > 0.0 && widths[0].0 < 200.0);
            assert!(widths[0].1 > 0.0 && widths[0].1 < app.inspector_target_width);
            if resize_on_second_pass {
                assert_eq!(widths[1], (0.0, app.inspector_target_width));
            } else {
                assert_eq!(
                    widths[1], widths[0],
                    "a sizing pass must not advance panel widths"
                );
            }
        }
    }
    #[test]
    fn rapid_viewport_resizes_keep_every_workspace_on_its_current_edges() {
        let (mut app, ctx, first_frame) = ready_fixture();
        app.split_workspace(app.pane().path.clone(), false);
        app.split_workspace(app.pane().path.clone(), true);
        app.inspector = true;
        for (index, width) in [1020.0, 780.0, 1140.0, 860.0, 1280.0, 940.0]
            .into_iter()
            .cycle()
            .take(48)
            .enumerate()
        {
            let height = if index % 2 == 0 { 760.0 } else { 540.0 };
            let bounds = Rect::from_min_size(egui::Pos2::ZERO, vec2(width, height));
            ctx.run_ui(
                egui::RawInput {
                    time: Some((first_frame + index) as f64 / 144.0),
                    screen_rect: Some(bounds),
                    ..Default::default()
                },
                |ui| {
                    app.ui(ui, &mut eframe::Frame::_new_kittest());
                    assert!(foldex_ui::motion::direct_layout(ui.ctx()));
                    assert!(!app.desk.motion.is_moving());
                    let content = Rect::from_min_max(
                        pos2(bounds.left() + app.sidebar_width, bounds.top()),
                        pos2(bounds.right() - app.inspector_width, bounds.bottom()),
                    );
                    let mut expected = Vec::new();
                    let mut dividers = Vec::new();
                    app.desk.root.geometry(
                        content,
                        ui.ctx().pixels_per_point(),
                        4.0,
                        &mut expected,
                        &mut dividers,
                    );
                    for (id, rect) in expected {
                        let actual = app.desk.groups.iter().find(|group| group.id == id).unwrap();
                        assert_eq!(actual.rect, rect, "workspace lagged at resize {index}");
                    }
                },
            )
            .drop_without_applying_deltas();
        }
    }
    #[test]
    fn home_tabs_restore_and_folder_commands_cannot_use_the_previous_directory() {
        let (mut app, ctx, _) = ready_fixture();
        let folder = app.pane().path.clone();
        app.navigate_home();
        assert!(app.pane().is_home() && app.pane().navigation.can_back());
        for command in [
            crate::commands::Command::NewFolder,
            crate::commands::Command::NewFile,
            crate::commands::Command::Paste,
            crate::commands::Command::OpenConsole,
            crate::commands::Command::SaveFolderView,
        ] {
            app.execute_command(&ctx, command);
        }
        assert!(app.edit.is_none());
        assert!(app.folder_views.is_empty());
        app.pane_mut().back();
        assert_eq!(
            app.pane().route(),
            &crate::navigation::Route::Folder(folder)
        );
        app.pane_mut().forward();
        assert!(app.pane().is_home());
        app.new_current_route_tab();
        assert!(app.pane().is_home() && !app.pane().navigation.can_back());
        app.duplicate_tab();
        assert!(app.pane().is_home());
        let state = app.workspace_state();
        let state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        app.restore_workspace(state);
        assert!(app.pane().is_home());
        app.pane_mut().prepare_view(&ctx);
        assert!(!app.pane().loading);
    }
    pub(super) fn key_event(key: Key, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed: true,
            repeat: false,
            modifiers,
        }
    }
    fn pointer_event(position: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }
    pub(super) fn pump_events(
        app: &mut Foldex,
        ctx: &egui::Context,
        n: usize,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut input = egui::RawInput {
            time: Some(n as f64 / 60.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 760.0))),
            events,
            ..Default::default()
        };
        app.raw_input_hook(ctx, &mut input);
        let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
        output.textures_delta.clear();
        output
    }

    #[test]
    fn keyboard_filter_accepts_opening_text_and_gates_later_file_commands() {
        let (mut app, ctx, frame) = ready_fixture();
        app.hotkeys = crate::hotkeys::Hotkeys::from_bindings(vec![
            crate::hotkeys::Binding::single(
                crate::commands::Command::Filter,
                Key::F,
                Modifiers::CTRL,
            ),
            crate::hotkeys::Binding::single(
                crate::commands::Command::Refresh,
                Key::G,
                Modifiers::NONE,
            ),
            crate::hotkeys::Binding::single(
                crate::commands::Command::Rename,
                Key::F2,
                Modifiers::NONE,
            ),
        ]);
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![
                key_event(Key::F, Modifiers::CTRL),
                key_event(Key::G, Modifiers::NONE),
                egui::Event::Text("g".into()),
                key_event(Key::F2, Modifiers::NONE),
            ],
        );
        assert_eq!(app.pane().filter, "g");
        assert!(
            app.edit.is_none(),
            "F2 must not rename a file after the filter gained focus"
        );
        assert_eq!(
            ctx.memory(|m| m.focused()),
            Some(Id::new((app.pane().id, "filter")))
        );
    }

    #[test]
    fn keyboard_region_commands_leave_files_and_return_without_losing_selection() {
        let (mut app, ctx, frame) = ready_fixture();
        let pane = app.pane().id;
        let selection = app.pane().selected.clone();
        crate::keyboard::request_id(&ctx, crate::keyboard::listing_id(pane));
        let release = egui::Event::Key {
            key: Key::F6,
            physical_key: Some(Key::F6),
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![key_event(Key::F6, Modifiers::NONE), release],
        );
        assert_eq!(
            crate::keyboard::focused_region(&ctx),
            Some(crate::keyboard::Region::Search(pane))
        );
        pump_events(
            &mut app,
            &ctx,
            frame + 1,
            vec![key_event(Key::F2, Modifiers::NONE)],
        );
        assert!(
            app.edit.is_none(),
            "A focused filter must not receive file rename commands"
        );
        pump_events(
            &mut app,
            &ctx,
            frame + 2,
            vec![key_event(Key::F6, Modifiers::SHIFT)],
        );
        assert!(crate::keyboard::listing_owns(&ctx, pane));
        assert_eq!(
            app.pane().selected.iter().collect::<Vec<_>>(),
            selection.iter().collect::<Vec<_>>()
        );
    }

    #[test]
    fn keyboard_help_is_modal_and_escape_returns_to_the_list() {
        let (mut app, ctx, frame) = ready_fixture();
        let pane = app.pane().id;
        crate::keyboard::request_id(&ctx, crate::keyboard::listing_id(pane));
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![key_event(Key::F1, Modifiers::NONE)],
        );
        for n in frame + 1..frame + 10 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert!(app.keyboard_help && app.popup_active());
        pump_events(
            &mut app,
            &ctx,
            frame + 10,
            vec![key_event(Key::F2, Modifiers::NONE)],
        );
        assert!(app.edit.is_none());
        pump_events(
            &mut app,
            &ctx,
            frame + 11,
            vec![key_event(Key::Escape, Modifiers::NONE)],
        );
        for n in frame + 12..frame + 40 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert!(!app.keyboard_help);
        assert!(crate::keyboard::listing_owns(&ctx, pane));
    }

    #[test]
    fn keyboard_opening_goto_from_filter_delivers_text_to_only_the_new_editor() {
        let (mut app, ctx, frame) = ready_fixture();
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![
                key_event(Key::F, Modifiers::CTRL),
                egui::Event::Text("seed".into()),
            ],
        );
        pump_events(
            &mut app,
            &ctx,
            frame + 1,
            vec![
                key_event(Key::P, Modifiers::CTRL),
                egui::Event::Text("destination".into()),
            ],
        );
        assert_eq!(app.pane().filter, "seed");
        assert_eq!(app.goto.as_ref().unwrap().input_text(), "destination");
        pump_events(
            &mut app,
            &ctx,
            frame + 2,
            vec![key_event(Key::Escape, Modifiers::NONE)],
        );
        for n in frame + 3..frame + 38 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new((app.pane().id, "filter")))
        );
        assert_eq!(app.pane().filter, "seed");
    }

    #[test]
    fn closing_palette_restores_search_after_brief_and_settled_openings() {
        for open_frames in [1, 30] {
            let (mut app, ctx, frame) = ready_fixture();
            pump_events(
                &mut app,
                &ctx,
                frame,
                vec![
                    key_event(Key::F, Modifiers::CTRL),
                    egui::Event::Text("seed".into()),
                ],
            );
            app.open_commands();
            for n in frame + 1..=frame + open_frames {
                pump_events(&mut app, &ctx, n, vec![]);
            }
            let close_frame = frame + open_frames + 1;
            pump_events(
                &mut app,
                &ctx,
                close_frame,
                vec![key_event(Key::Escape, Modifiers::NONE)],
            );
            for n in close_frame + 1..close_frame + 50 {
                pump_events(&mut app, &ctx, n, vec![]);
            }
            assert!(app.commands.finished());
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(Id::new((app.pane().id, "filter"))),
                "palette was open for {open_frames} frames"
            );
            assert_eq!(app.pane().filter, "seed");
        }
    }

    #[test]
    fn keyboard_new_tabs_and_tab_switches_do_not_reactivate_stale_focus() {
        let (mut app, ctx, frame) = ready_fixture();
        let first = app.pane().id;
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![key_event(Key::T, Modifiers::CTRL)],
        );
        let second = app.pane().id;
        assert_ne!(first, second);
        for n in frame + 1..frame + 12 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert_eq!(app.pane().id, second);
        pump_events(
            &mut app,
            &ctx,
            frame + 12,
            vec![key_event(Key::Tab, Modifiers::CTRL)],
        );
        for n in frame + 13..frame + 22 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert_eq!(app.pane().id, first);
        assert!(crate::keyboard::listing_owns(&ctx, first));
        let group = app.desk.focused;
        crate::keyboard::request_id(&ctx, Id::new((group, "tab-strip-focus")));
        pump_events(
            &mut app,
            &ctx,
            frame + 22,
            vec![key_event(Key::ArrowRight, Modifiers::NONE)],
        );
        for n in frame + 23..frame + 28 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert_eq!(app.pane().id, second);
        assert_eq!(
            ctx.memory(|m| m.focused()),
            Some(Id::new((group, "tab-strip-focus")))
        );
    }

    #[test]
    fn keyboard_save_layout_starts_in_the_name_field_and_cancels_without_saving() {
        let (mut app, ctx, frame) = ready_fixture();
        app.execute_command(&ctx, crate::commands::Command::SaveLayout);
        for n in frame..frame + 8 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert_eq!(ctx.memory(|m| m.focused()), Some(Id::new("layout-name")));
        pump_events(
            &mut app,
            &ctx,
            frame + 8,
            vec![egui::Event::Text("Keyboard layout".into())],
        );
        assert_eq!(app.save_layout_name.as_deref(), Some("Keyboard layout"));
        pump_events(
            &mut app,
            &ctx,
            frame + 9,
            vec![key_event(Key::Escape, Modifiers::NONE)],
        );
        for n in frame + 10..frame + 40 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert!(app.save_layout_name.is_none() && app.saved_layouts.is_empty());
    }

    #[test]
    fn keyboard_tab_visits_every_root_region_without_a_search_results_loop() {
        let (mut app, ctx, frame) = ready_fixture();
        let listing = crate::keyboard::listing_id(app.pane().id);
        crate::keyboard::request_id(&ctx, listing);
        let mut regions = std::collections::HashSet::new();
        let mut wrapped = false;
        for at in 0..96 {
            pump_events(
                &mut app,
                &ctx,
                frame + at,
                vec![
                    key_event(Key::Tab, Modifiers::NONE),
                    egui::Event::Key {
                        key: Key::Tab,
                        physical_key: Some(Key::Tab),
                        pressed: false,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    },
                ],
            );
            if let Some(region) = crate::keyboard::focused_region(&ctx) {
                regions.insert(region);
            }
            if ctx.memory(|memory| memory.focused()) == Some(listing) {
                wrapped = true;
                break;
            }
        }
        let pane = app.pane().id;
        for required in [
            crate::keyboard::Region::Tabs,
            crate::keyboard::Region::Sidebar,
            crate::keyboard::Region::Toolbar(pane),
            crate::keyboard::Region::Search(pane),
            crate::keyboard::Region::Listing(pane),
            crate::keyboard::Region::Footer(pane),
        ] {
            assert!(
                regions.contains(&required),
                "Tab skipped {required:?}: {regions:?}"
            );
        }
        assert!(
            wrapped,
            "Tab must return to its starting collection within one bounded circuit"
        );
    }

    #[test]
    fn keyboard_tab_stays_with_ime_candidates_then_returns_to_normal_traversal() {
        let (mut app, ctx, frame) = ready_fixture();
        let filter = Id::new((app.pane().id, "filter"));
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![key_event(Key::F, Modifiers::CTRL)],
        );
        pump_events(
            &mut app,
            &ctx,
            frame + 1,
            vec![
                egui::Event::Ime(egui::ImeEvent::Preedit {
                    text: "键".into(),
                    active_range_chars: None,
                }),
                key_event(Key::Tab, Modifiers::NONE),
            ],
        );
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(filter));
        pump_events(
            &mut app,
            &ctx,
            frame + 2,
            vec![key_event(Key::Tab, Modifiers::NONE)],
        );
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(filter));
        pump_events(
            &mut app,
            &ctx,
            frame + 3,
            vec![egui::Event::Ime(egui::ImeEvent::Commit("Selected".into()))],
        );
        pump_events(
            &mut app,
            &ctx,
            frame + 4,
            vec![key_event(Key::Tab, Modifiers::NONE)],
        );
        assert!(crate::keyboard::listing_owns(&ctx, app.pane().id));
    }

    #[test]
    fn keyboard_space_toggles_the_focused_footer_control_without_opening_preview() {
        let (mut app, ctx, frame) = ready_fixture();
        let pane = app.pane().id;
        crate::keyboard::request_id(&ctx, Id::new((pane, "kind-count", false)));
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![key_event(Key::Space, Modifiers::NONE)],
        );
        assert!(!app.pane().query.kind.includes(false));
        assert!(!app.inspector);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new((pane, "kind-count", false)))
        );
    }

    #[test]
    fn keyboard_guide_can_scroll_to_its_last_shortcut_on_a_small_screen() {
        let (mut app, ctx, frame) = ready_fixture();
        app.execute_command(&ctx, crate::commands::Command::KeyboardHelp);
        let mut at = frame;
        let mut small_frame = |events| {
            let mut input = egui::RawInput {
                time: Some(at as f64 / 60.0),
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(640.0, 430.0))),
                events,
                ..Default::default()
            };
            at += 1;
            app.raw_input_hook(&ctx, &mut input);
            let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
            output.textures_delta.clear();
        };
        for _ in 0..10 {
            small_frame(vec![]);
        }
        let guide = Id::new("keyboard-guide-focus");
        crate::keyboard::request_id(&ctx, guide);
        small_frame(vec![key_event(Key::End, Modifiers::NONE)]);
        small_frame(vec![]);
        let (offset, maximum, _) = ctx
            .data(|data| data.get_temp::<(f32, f32, f32)>(guide))
            .unwrap();
        assert!(
            maximum > 0.0 && offset > 0.0,
            "The guide must expose its last shortcuts through keyboard scrolling"
        );
        assert!((offset - maximum).abs() < 1.0);
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(guide));
    }

    #[test]
    fn keyboard_search_text_and_tab_in_one_native_batch_preserve_the_query() {
        let (mut app, ctx, frame) = ready_fixture();
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![
                key_event(Key::F, Modifiers::CTRL),
                egui::Event::Text("Selected".into()),
                key_event(Key::Tab, Modifiers::NONE),
            ],
        );
        assert_eq!(app.pane().filter, "Selected");
        assert!(crate::keyboard::listing_owns(&ctx, app.pane().id));
        assert!(!app.pane().focus_filter);
    }

    #[test]
    fn keyboard_commands_do_not_run_in_an_unfocused_browser_viewport() {
        let (mut app, ctx, frame) = ready_fixture();
        let mut input = egui::RawInput {
            focused: false,
            time: Some(frame as f64 / 60.0),
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 760.0))),
            events: vec![
                key_event(Key::T, Modifiers::CTRL),
                key_event(Key::F2, Modifiers::NONE),
            ],
            ..Default::default()
        };
        app.raw_input_hook(&ctx, &mut input);
        let mut output = ctx.run_ui(input, |ui| app.ui(ui, &mut eframe::Frame::_new_kittest()));
        output.textures_delta.clear();
        assert_eq!(app.tabs.len(), 1);
        assert!(app.edit.is_none());
    }

    #[test]
    fn keyboard_space_selects_the_focused_home_item_without_opening_preview() {
        let (mut app, ctx, frame) = ready_fixture();
        let path = app.pane().primary_selected_path().unwrap();
        app.record_home_usage(path, crate::home_history::Access::Visited);
        app.navigate_home();
        let mut at = frame;
        for n in frame..frame + 100 {
            pump_events(&mut app, &ctx, n, vec![]);
            at = n + 1;
            if !app.home.rows.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!app.home.rows.is_empty());
        crate::keyboard::request_id(&ctx, crate::keyboard::listing_id(app.pane().id));
        pump_events(
            &mut app,
            &ctx,
            at,
            vec![key_event(Key::Home, Modifiers::NONE)],
        );
        pump_events(
            &mut app,
            &ctx,
            at + 1,
            vec![key_event(Key::Space, Modifiers::NONE)],
        );
        assert!(app.pane().home_selected.is_some());
        assert!(!app.inspector);
    }

    #[test]
    fn keyboard_goto_and_address_accept_opening_text_and_retain_focus() {
        for key in [Key::P, Key::L] {
            let (mut app, ctx, frame) = ready_fixture();
            pump_events(
                &mut app,
                &ctx,
                frame,
                vec![
                    key_event(key, Modifiers::CTRL),
                    egui::Event::Text("fixture replacement".into()),
                ],
            );
            assert_eq!(
                app.goto.as_ref().unwrap().input_text(),
                "fixture replacement"
            );
            for n in frame + 1..frame + 8 {
                pump_events(&mut app, &ctx, n, vec![]);
            }
            assert_eq!(ctx.memory(|m| m.focused()), Some(Id::new("goto-input")));
            assert!(ctx.egui_wants_keyboard_input());
            app.goto.as_mut().unwrap().close();
            for n in frame + 8..frame + 32 {
                pump_events(&mut app, &ctx, n, vec![]);
            }
            assert!(app.goto.is_none());
            // Reusing an existing Area skips its initial sizing pass. A pointer
            // opening the popup must still leave its text field focused.
            app.open_goto(None);
            let position = pos2(650.0, 350.0);
            pump_events(
                &mut app,
                &ctx,
                frame + 32,
                vec![
                    egui::Event::PointerMoved(position),
                    pointer_event(position, true),
                ],
            );
            pump_events(
                &mut app,
                &ctx,
                frame + 33,
                vec![pointer_event(position, false)],
            );
            for n in frame + 34..frame + 39 {
                pump_events(&mut app, &ctx, n, vec![]);
            }
            assert_eq!(ctx.memory(|m| m.focused()), Some(Id::new("goto-input")));
        }
    }

    #[test]
    fn keyboard_address_keeps_the_prefilled_path_selected_after_area_sizing() {
        let (mut app, ctx, frame) = ready_fixture();
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![key_event(Key::L, Modifiers::CTRL)],
        );
        for n in frame + 1..frame + 8 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        assert_eq!(ctx.memory(|m| m.focused()), Some(Id::new("goto-input")));
        let state = egui::text_edit::TextEditState::load(&ctx, Id::new("goto-input")).unwrap();
        let selected = state.cursor.char_range().unwrap().as_sorted_char_range();
        assert_eq!(selected.start.0, 0);
        assert_eq!(
            selected.end.0,
            app.goto.as_ref().unwrap().input_text().chars().count()
        );
    }

    #[test]
    fn keyboard_clicking_filter_then_ctrl_f_does_not_replay_delivered_text() {
        for already_focused in [false, true] {
            let (mut app, ctx, mut frame) = ready_fixture();
            if already_focused {
                pump_events(
                    &mut app,
                    &ctx,
                    frame,
                    vec![key_event(Key::F, Modifiers::CTRL)],
                );
                frame += 1;
            }
            let filter_id = Id::new((app.pane().id, "filter"));
            let position = ctx.read_response(filter_id).unwrap().rect.center();
            pump_events(
                &mut app,
                &ctx,
                frame,
                vec![
                    egui::Event::PointerMoved(position),
                    pointer_event(position, true),
                    key_event(Key::F, Modifiers::CTRL),
                    egui::Event::Text("g".into()),
                ],
            );
            pump_events(
                &mut app,
                &ctx,
                frame + 1,
                vec![pointer_event(position, false)],
            );
            assert_eq!(app.pane().filter, "g");
            assert!(app.deferred_editor_input.is_empty());
        }
    }

    #[test]
    fn keyboard_late_filter_defers_only_undelivered_payload_once() {
        let (mut app, ctx, frame) = ready_fixture();
        let position = pos2(650.0, 350.0);
        pump_events(
            &mut app,
            &ctx,
            frame,
            vec![
                egui::Event::PointerMoved(position),
                pointer_event(position, true),
                key_event(Key::F, Modifiers::CTRL),
                egui::Event::Text("g".into()),
            ],
        );
        assert_eq!(app.pane().filter, "");
        assert_eq!(app.deferred_editor_input.len(), 1);
        pump_events(
            &mut app,
            &ctx,
            frame + 1,
            vec![pointer_event(position, false)],
        );
        assert_eq!(app.pane().filter, "g");
        pump_events(&mut app, &ctx, frame + 2, vec![]);
        assert_eq!(app.pane().filter, "g");
        assert!(app.deferred_editor_input.is_empty());
    }

    #[test]
    fn keyboard_deferred_payload_is_bounded_and_keeps_its_editor_owner() {
        let (mut app, ctx, frame) = ready_fixture();
        let events = (0..100)
            .map(|_| egui::Event::Text("界".repeat(10_000)))
            .collect();
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(frame as f64 / 60.0),
                events,
                ..Default::default()
            },
            |ui| {
                app.defer_filter_input(ui.ctx());
            },
        );
        output.textures_delta.clear();
        assert!(app.deferred_editor_input.len() <= 32);
        assert!(
            app.deferred_editor_input
                .iter()
                .map(|event| match event {
                    egui::Event::Text(text) => text.len(),
                    _ => 0,
                })
                .sum::<usize>()
                <= 4096
        );
        ctx.memory_mut(|m| m.request_focus(Id::new("different-editor")));
        let mut incoming = egui::RawInput {
            events: vec![egui::Event::Text("fresh".into())],
            ..Default::default()
        };
        app.raw_input_hook(&ctx, &mut incoming);
        assert_eq!(incoming.events.len(), 1);
        assert!(matches!(&incoming.events[0], egui::Event::Text(text) if text == "fresh"));
        assert!(app.deferred_editor_input.is_empty());
    }

    #[test]
    fn keyboard_closing_hotkeys_window_cancels_recording_and_restores_routing() {
        let (mut app, ctx, frame) = ready_fixture();
        app.hotkeys_open = true;
        for n in frame..frame + 30 {
            pump_events(&mut app, &ctx, n, vec![]);
        }
        let close = ctx
            .read_response(Id::new("hotkeys-editor-window").with("close"))
            .expect("The Hotkeys dialog must expose its close button")
            .rect
            .center();
        pump_events(
            &mut app,
            &ctx,
            frame + 30,
            vec![
                egui::Event::PointerMoved(close),
                pointer_event(close, true),
                pointer_event(close, false),
            ],
        );
        // A recording modal deliberately blocks interaction with its owner.
        // Initiate the real pointer close first, then keep a recorder alive
        // during the animated owner close to exercise its lifetime cleanup.
        assert!(
            app.hotkeys_open,
            "The owner must remain alive while closing"
        );
        app.hotkeys.record(crate::commands::Command::Parent);
        assert!(app.hotkeys.is_recording());
        let mut next_frame = frame + 31;
        for n in frame + 31..frame + 151 {
            pump_events(&mut app, &ctx, n, vec![]);
            next_frame = n + 1;
            if !app.hotkeys_open {
                break;
            }
            assert!(
                app.hotkeys.is_recording(),
                "The recorder must remain active until its owner closes"
            );
        }
        assert!(
            !app.hotkeys_open,
            "The generated pointer event must close the outer Hotkeys window"
        );
        assert!(!app.hotkeys.is_recording());
        // Retire the last modal layer before checking ordinary app routing.
        pump_events(&mut app, &ctx, next_frame, vec![]);
        let sidebar = app.sidebar;
        pump_events(
            &mut app,
            &ctx,
            next_frame + 1,
            vec![key_event(Key::B, Modifiers::CTRL)],
        );
        assert_ne!(app.sidebar, sidebar);
    }
    #[test]
    fn startup_file_resolves_and_idle_ui_stops_requesting_frames() {
        let (mut app, ctx, root) = fixture_app();
        let expected = std::fs::canonicalize(root.join("Selected.txt")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut quiet = 0;
        for n in 0..2000 {
            let out = pump(&mut app, &ctx, n, false);
            let ready = !app.pane().loading
                && app.pane().primary_selected_path().is_some()
                && app.saved_revision > 0;
            let delay = out.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            // Visible drive usage refreshes once a minute. This distant timer is
            // compatible with an idle UI; any immediate or unrelated wake is not.
            let causes = ctx.repaint_causes();
            let sidebar_timer = delay >= Duration::from_secs(55)
                && delay <= Duration::from_secs(61)
                && !ctx.requested_repaint_last_pass()
                && !causes.is_empty()
                && causes
                    .iter()
                    .all(|cause| cause.file.replace('\\', "/").ends_with("/sidebar.rs"));
            let idle = delay == Duration::MAX || sidebar_timer;
            quiet = if ready && idle { quiet + 1 } else { 0 };
            if quiet >= 8 {
                let actual =
                    std::fs::canonicalize(app.pane().primary_selected_path().unwrap()).unwrap();
                assert_eq!(
                    actual, expected,
                    "The startup file lost selection while its folder refreshed"
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "UI did not settle: {:?}, loading={}, saved={}, selected={:?}, expected={:?}",
                ctx.repaint_causes(),
                app.pane().loading,
                app.saved_revision,
                app.pane().primary_selected_path(),
                expected
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("UI did not become idle");
    }
    #[test]
    fn native_close_saves_atomically_then_exits_without_blocking_the_frame() {
        let (mut app, ctx, root) = fixture_app();
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut saw_cancel = false;
        for n in 0..2000 {
            let out = pump(&mut app, &ctx, n, n == 0);
            let commands = &out.viewport_output[&egui::ViewportId::ROOT].commands;
            saw_cancel |= commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::CancelClose));
            if commands
                .iter()
                .any(|c| matches!(c, egui::ViewportCommand::Close))
            {
                assert!(saw_cancel);
                let session: Session =
                    serde_json::from_slice(&std::fs::read(root.join("session.json")).unwrap())
                        .unwrap();
                assert_eq!(session.tabs.len(), 1);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Atomic close acknowledgement was not delivered"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("Close never completed");
    }
}

impl eframe::App for Foldex {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        // eframe clears its gamma-space framebuffer with these values as-is.
        // Linearizing Color32 here turns Graphite into an almost black clear.
        self.theme.bg().to_normalized_gamma_f32()
    }
    fn run_ui_when_hidden(&self) -> bool {
        self.launcher_open || self.window_transfer.receiving()
    }
    fn repaint_on_window_move(&self) -> bool {
        // Native movement reuses the compositor's existing surface. Window
        // geometry is refreshed by eframe without another GPU presentation.
        false
    }
    fn logic(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let root_ui_skipped = !self.explorer_visible
            || ctx.input(|input| {
                let root = input.viewport();
                root.minimized == Some(true) || root.occluded == Some(true)
            });
        if !self.launcher_open && root_ui_skipped {
            // eframe skips App::ui when every window is minimized or occluded.
            // A normally minimized explorer is still logically open/visible.
            // Tray/IPC events and save acknowledgements still need an owner.
            // Keep nonvisual async services (e.g. theme replies/debounce) here.
            self.service_theme(ctx);
            self.service_updates(ctx);
            self.service_launcher(ctx);
            self.poll(ctx);
            self.service_session(ctx);
        }
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.options_popup.observe_system_theme(input.system_theme);
        crate::native_keys::inject_context_menu(input);
        if self
            .benchmark
            .as_ref()
            .is_some_and(|run| run.fixes.is_some())
        {
            for event in &input.events {
                if let egui::Event::Screenshot {
                    user_data, image, ..
                } = event
                    && let Some(label) = user_data
                        .data
                        .as_ref()
                        .and_then(|value| value.downcast_ref::<String>())
                    && label.starts_with("fix-")
                {
                    self.fixes_capture(label, image);
                }
            }
        }
        if self
            .benchmark
            .as_ref()
            .is_some_and(|run| run.launcher.is_some())
        {
            for event in &input.events {
                if let egui::Event::Screenshot {
                    viewport_id,
                    user_data,
                    image,
                } = event
                    && let Some(label) = user_data
                        .data
                        .as_ref()
                        .and_then(|value| value.downcast_ref::<String>())
                    && (*viewport_id == egui::ViewportId::from_hash_of("foldex-focused-launcher")
                        || (*viewport_id == egui::ViewportId::ROOT && label.starts_with("root-")))
                {
                    self.launcher_capture(label, image);
                }
            }
        }
        if ctx.viewport_id() != egui::ViewportId::ROOT {
            input.max_texture_side = Some(input.max_texture_side.unwrap_or(2048).min(2048));
            return;
        }
        if let Some(run) = self.benchmark.as_mut().and_then(|run| run.fixes.as_mut()) {
            if run.force_focus {
                input.focused = true;
            }
            if let Some(pointer) = run.pointer {
                input.events.push(egui::Event::PointerMoved(pointer));
            }
            input.events.append(&mut run.input);
        }
        self.hotkeys.prepare_input(input);
        let timed = self
            .benchmark
            .as_ref()
            .is_some_and(|run| run.stress.is_some())
            .then(Instant::now);
        if self
            .benchmark
            .as_ref()
            .is_some_and(|run| run.full_search.is_some())
        {
            input.events.push(egui::Event::PointerMoved(pos2(
                500.0,
                160.0 + (self.frame_count % 20) as f32 * 20.0,
            )));
        }
        if !self.deferred_editor_input.is_empty() {
            let owner = self.deferred_editor_target.take();
            if input.focused
                && owner == Some(self.pane().id)
                && (ctx.memory(|m| m.focused()) == owner.map(|id| Id::new((id, "filter")))
                    || (ctx.memory(|m| m.focused()).is_none() && self.pane().focus_filter))
            {
                let mut deferred = std::mem::take(&mut self.deferred_editor_input);
                deferred.append(&mut input.events);
                input.events = deferred;
            } else {
                self.deferred_editor_input.clear();
            }
        }
        // A compact atlas is sufficient for text and icons, and avoids oversized
        // font allocations or precision problems on low-end GL implementations.
        input.max_texture_side = Some(input.max_texture_side.unwrap_or(2048).min(2048));
        if let Some(run) = self
            .benchmark
            .as_mut()
            .and_then(|run| run.launcher.as_mut())
        {
            match run.atlas_cycle {
                1 => {
                    // Exercise a real font-atlas recreation after the popup stress.
                    input.max_texture_side = Some(1024);
                    run.atlas_cycle = 2;
                }
                2 => run.atlas_cycle = 3,
                _ => {}
            }
        }
        if let Some(begin) = timed
            && let Some(stress) = self.benchmark.as_mut().and_then(|run| run.stress.as_mut())
        {
            stress.raw_input_ms = begin.elapsed().as_secs_f64() * 1000.0;
        }
    }
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.frame_count = self.frame_count.wrapping_add(1);
        self.renderer_cpu_ms = frame.info().cpu_usage.unwrap_or_default() as f64 * 1000.0;
        let begin = Instant::now();
        let trace_stress = self
            .benchmark
            .as_ref()
            .is_some_and(|run| run.stress.is_some());
        let mut stages = [0.0; 5];
        let ctx = ui.ctx().clone();
        self.service_theme(&ctx);
        self.service_updates(&ctx);
        self.service_launcher(&ctx);
        self.fixes_benchmark_step(&ctx);
        self.launcher_benchmark_step(&ctx);
        self.keyboard_benchmark_step(&ctx);
        self.assets.set_motion_duration(self.theme.duration());
        self.desk.focus(self.tabs[self.active].left.id);
        self.assets.retain_views(&self.retained_asset_views());
        self.assets.begin_frame(&ctx);
        self.service_sidebar(&ctx);
        self.service_home();
        // A nested viewport may recycle the shared glyph atlas at begin_pass.
        // Run it before producing any retained text shapes for the explorer.
        self.launcher_viewport(&ctx);
        if !self.explorer_visible {
            crate::native_keys::set_file_mode(false);
            self.poll(&ctx);
            self.service_session(&ctx);
            self.service_benchmark(&ctx);
            return;
        }
        if self
            .benchmark
            .as_ref()
            .is_some_and(|run| run.resize.is_some())
        {
            ctx.data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
        }
        self.search_stress_step(&ctx);
        self.navigation_stress_step(&ctx);
        self.stress_step(&ctx);
        if trace_stress {
            stages[0] = begin.elapsed().as_secs_f64() * 1000.0;
        }
        if let Some(frame) = &self.native_frame {
            frame.update(self.theme.background, self.theme.border, self.fullscreen);
        }
        if trace_stress {
            stages[1] = begin.elapsed().as_secs_f64() * 1000.0;
        }
        let poll_started = Instant::now();
        self.poll(&ctx);
        if trace_stress {
            stages[2] = begin.elapsed().as_secs_f64() * 1000.0;
        }
        if let Some(run) = self
            .benchmark
            .as_mut()
            .and_then(|run| run.full_search.as_mut())
        {
            run.poll_ms = poll_started.elapsed().as_secs_f64() * 1000.0;
        }
        let keyboard_after_pointer = ctx.input(|i| i.pointer.any_pressed());
        if !keyboard_after_pointer {
            self.keyboard(&ctx, false);
        }
        let dropped: Vec<_> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if !dropped.is_empty() && !self.updates.exit_pending {
            if self.pane().is_virtual() {
                self.toast("Open a folder before dropping files", false);
            } else {
                self.job(Job::DropFiles {
                    sources: dropped,
                    destination: self.pane().path.clone(),
                    control: ctx.input(|input| input.modifiers.ctrl),
                    shift: ctx.input(|input| input.modifiers.shift),
                });
            }
        }
        if self.first_frame.is_none() {
            self.first_frame = Some(self.started.elapsed().as_secs_f64() * 1000.0);
        }
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0, self.theme.bg());
        let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.1);
        if self.layout_size != rect.size() || self.layout_scale != self.theme.metrics().scale {
            self.layout_size = rect.size();
            self.layout_scale = self.theme.metrics().scale;
            self.resize_until = Instant::now() + Duration::from_millis(120);
        }
        let direct_layout = Instant::now() < self.resize_until
            || foldex_ui::components::divider_dragging(&ctx)
            || self
                .native_frame
                .as_ref()
                .is_some_and(|frame| frame.is_sizing())
            || ["sidebar-resize", "inspector-resize", "pane-splitter"]
                .iter()
                .any(|id| ctx.is_being_dragged(Id::new(*id)));
        let layout_duration = if direct_layout {
            0.0
        } else {
            self.theme.duration()
        };
        foldex_ui::motion::set_interactive_resize(&ctx, direct_layout);
        let scale = self.theme.metrics().scale;
        let content_minimum = crate::pane::MIN_CONTENT_WIDTH * scale;
        let inspector_room = (rect.width() - content_minimum).max(0.0);
        let inspector_target = if self.inspector
            && !self.pane().is_recycle_bin()
            && (!self.pane().is_home() || self.pane().primary_selected_path().is_some())
            && inspector_room >= crate::pane::MIN_INSPECTOR_WIDTH * scale
        {
            self.inspector_target_width
                .min(rect.width() * 0.5)
                .min(inspector_room)
        } else {
            0.0
        };
        let sidebar_target =
            if self.sidebar && rect.width() - inspector_target - content_minimum >= 160.0 * scale {
                self.theme
                    .sidebar_width
                    .min((rect.width() - inspector_target - content_minimum).max(0.0))
            } else {
                0.0
            };
        // Discard/sizing passes share one frame delta. Immediate/direct layout
        // still snaps when a later pass discovers a new target or window size.
        let panel_dt = if ctx.current_pass_index() == 0 {
            dt
        } else {
            0.0
        };
        let mut moving = foldex_ui::approach(
            &mut self.sidebar_width,
            sidebar_target,
            panel_dt,
            layout_duration,
        );
        moving |= foldex_ui::approach(
            &mut self.inspector_width,
            inspector_target,
            panel_dt,
            layout_duration,
        );
        foldex_ui::motion::set_direct_layout(&ctx, direct_layout || moving);
        if self.inspector_width < 1.0 {
            if let Some(view) = &mut self.inspector_view {
                view.close_popups();
            }
        }
        if !self.inspector && self.inspector_width < 1.0 {
            if let Some(view) = &mut self.inspector_view {
                if view.path().is_some() {
                    view.close();
                }
            }
        }
        foldex_ui::components::set_overlay_active(&ctx, self.popup_active());
        // A shortcut can open a modal before that modal has registered its
        // egui layer. Covered editors must already yield this input batch.
        let mut background_ui = ui.new_child(
            UiBuilder::new()
                .id_salt("browser-background")
                .max_rect(rect),
        );
        let ui = &mut background_ui;
        if self.popup_active() && !self.launcher_open {
            foldex_ui::components::disable_interaction(ui);
        }
        self.titlebar(ui, Rect::from_min_size(rect.min, vec2(rect.width(), 32.0)));
        let side = Rect::from_min_max(
            pos2(rect.left(), rect.top() + 32.0),
            pos2(rect.left() + self.sidebar_width, rect.bottom()),
        );
        let actions = if side.width() > 1.0 {
            self.sidebar_ui(ui, side)
        } else {
            vec![]
        };
        for a in actions {
            self.action(a);
        }
        let content = Rect::from_min_max(
            pos2(side.right(), rect.top()),
            pos2(rect.right() - self.inspector_width, rect.bottom()),
        );
        if trace_stress {
            stages[3] = begin.elapsed().as_secs_f64() * 1000.0;
        }
        moving |= self.workspace_canvas(ui, content, direct_layout);
        if trace_stress {
            stages[4] = begin.elapsed().as_secs_f64() * 1000.0;
        }
        self.window_controls(ui, Rect::from_min_size(rect.min, vec2(rect.width(), 32.0)));
        if self.inspector_width > 5.0 {
            if self.inspector {
                self.sync_inspector();
            }
            self.inspector_ui(
                ui,
                Rect::from_min_max(pos2(content.right(), side.top()), rect.max),
            );
        }
        // Paint shared boundaries after child backgrounds so the footer cannot erase them.
        if side.width() > 0.5 {
            ui.painter().vline(
                side.right() - 0.5,
                side.y_range(),
                Stroke::new(1.0, self.theme.line()),
            );
        }
        if keyboard_after_pointer {
            self.keyboard(&ctx, true);
        }
        self.dialogs(&ctx);
        self.operations_ui(&ctx);
        if !self.popup_active() {
            self.updates.notification(&ctx, &self.theme);
        }
        self.workspace_dialogs(&ctx);
        self.commands_ui(&ctx);
        self.context_ui(&ctx);
        if let Some(command) = self
            .main_menu
            .ui(&ctx, &self.theme, !self.saved_layouts.is_empty())
        {
            self.execute_command(&ctx, command);
        }
        if let Some(popup) = &mut self.goto {
            let target = popup.target;
            let source = self.tabs.iter().find_map(|tab| {
                if tab.left.id == target {
                    Some(&tab.left)
                } else {
                    tab.right.as_ref().filter(|pane| pane.id == target)
                }
            });
            let anchor = source.and_then(|pane| pane.breadcrumb_anchor);
            if let Some(previous) = popup.ancestor_count()
                && let Some(pane) = source
                && previous != pane.breadcrumb_hidden
            {
                let locations = pane
                    .breadcrumb_trail()
                    .into_iter()
                    .take(pane.breadcrumb_hidden)
                    .map(|(path, _)| crate::goto::Location {
                        icon: if path.parent().is_none() {
                            Icon::Drive
                        } else {
                            Icon::Folder
                        },
                        label: path_label(&path),
                        path,
                    })
                    .collect();
                popup.update_ancestors(locations);
            }
            popup.update_anchor(anchor);
            let destination = popup.ui(&ctx, &self.theme);
            if let Some(backwards) = popup.take_tab_exit() {
                self.pending_focus_exit = Some((backwards, ctx.cumulative_frame_nr()));
                ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
            }
            let finished = popup.finished();
            if let Some(destination) = destination {
                if let Some((tab, pane)) = self.tabs.iter().enumerate().find_map(|(i, t)| {
                    if t.left.id == target {
                        Some((i, 0))
                    } else if t.right.as_ref().is_some_and(|p| p.id == target) {
                        Some((i, 1))
                    } else {
                        None
                    }
                }) {
                    self.active = tab;
                    self.active_pane = pane;
                    if destination.new_tab {
                        if destination.is_file
                            && let (Some(parent), Some(name)) =
                                (destination.path.parent(), destination.path.file_name())
                        {
                            let previous = self.pane().id;
                            self.new_tab(parent.to_path_buf());
                            if self.pane().id != previous {
                                let pane = self.pane_mut();
                                pane.pending_selection = Some(name.to_owned());
                                pane.query.show_hidden = true;
                            }
                        } else {
                            self.new_tab(destination.path);
                        }
                    } else {
                        self.action(Action::GoPath(destination.path));
                    }
                }
                let _ = destination.keep_open;
            }
            if finished {
                if let Some(old) = self.goto.take() {
                    if let Err(old) = self.engine.defer(old) {
                        self.retiring_goto.push(old);
                    }
                }
            }
        }
        self.service_focus_exit(&ctx);
        self.service_session(&ctx);
        let overlay_open = self.popup_active();
        let mut retire_toast = false;
        if let Some(toast) = &mut self.toast {
            let now = Instant::now();
            if overlay_open {
                if toast.suspended_at.is_none() && now < toast.until {
                    toast.suspended_at = Some(now);
                }
            } else if let Some(since) = toast.suspended_at.take() {
                toast.until += now.saturating_duration_since(since);
            }
            let expired = now >= toast.until;
            let visible = !overlay_open && !expired;
            let theme = &self.theme;
            let m = theme.metrics();
            if foldex_ui::motion::MotionGains::new(dt, theme.duration()).step_normalized(
                &mut toast.opacity,
                if visible { 1.0 } else { 0.0 },
                foldex_ui::motion::MotionProfile::Standard,
            ) {
                ctx.request_repaint();
            }
            if toast.opacity > 0.0 {
                let width = (570.0 * m.scale).min((rect.width() - 62.0 * m.scale).max(m.control));
                let response = egui::Area::new(Id::new("toast"))
                    .sense(Sense::hover())
                    .movable(false)
                    .fade_in(false)
                    .anchor(
                        Align2::CENTER_BOTTOM,
                        vec2(
                            0.0,
                            -m.footer - 12.0 * m.scale + 8.0 * m.scale * (1.0 - toast.opacity),
                        ),
                    )
                    .order(egui::Order::Tooltip)
                    .show(&ctx, |ui| {
                        ui.set_opacity(toast.opacity);
                        egui::Frame::new()
                            .fill(Theme::color(theme.surface))
                            .stroke(Stroke::new(
                                1.0,
                                if toast.error {
                                    Theme::color(theme.danger)
                                } else {
                                    theme.line()
                                },
                            ))
                            .corner_radius(theme.radius.saturating_add(2))
                            .inner_margin(egui::Margin::symmetric(
                                (15.0 * m.scale).round() as i8,
                                (10.0 * m.scale).round() as i8,
                            ))
                            .show(ui, |ui| {
                                ui.set_max_width(width);
                                let mut scroll = egui::ScrollArea::vertical()
                                    .id_salt("notification-body")
                                    .max_height((rect.height() * 0.35).max(m.control))
                                    .auto_shrink([true, true]);
                                if toast.scroll_reset {
                                    scroll = scroll.vertical_scroll_offset(0.0);
                                }
                                scroll.show(ui, |ui| {
                                    let mut job = egui::text::LayoutJob::simple(
                                        toast.text.clone(),
                                        theme.font(),
                                        theme.fg(),
                                        width,
                                    );
                                    job.wrap.break_anywhere = false;
                                    ui.add(egui::Label::new(job).wrap());
                                });
                                if !ui.is_sizing_pass() {
                                    toast.scroll_reset = false;
                                }
                            });
                    });
                if visible
                    && ctx.input(|input| {
                        input
                            .pointer
                            .hover_pos()
                            .is_some_and(|point| response.response.rect.contains(point))
                    })
                {
                    toast.until = toast.until.max(Instant::now() + Duration::from_secs(2));
                }
                if visible {
                    ctx.request_repaint_after(
                        toast.until.saturating_duration_since(Instant::now()),
                    );
                }
            } else if expired && !overlay_open {
                retire_toast = true;
            }
        }
        if retire_toast {
            self.toast = None;
        }
        if self.show_stats {
            if self.metrics_requested.elapsed() >= Duration::from_secs(1) {
                self.metrics_requested = Instant::now();
                self.job(Job::Metrics {
                    id: self.process_samples.len() as u64,
                });
            }
            ctx.request_repaint_after(
                Duration::from_secs(1).saturating_sub(self.metrics_requested.elapsed()),
            );
            let mut stats_open = true;
            let mut report_to_save = None;
            foldex_ui::components::dialog(
                &ctx,
                Id::new("performance-dialog"),
                &self.theme,
                "Performance",
                420.0 * scale,
                &mut stats_open,
                |ui| {
                    ui.label(format!(
                        "Launch to first UI callback: {:.1} ms",
                        self.first_frame.unwrap_or_default()
                    ));
                    let mut samples: Vec<_> = self.frames.iter().copied().collect();
                    samples.sort_by(f64::total_cmp);
                    let p95 = samples
                        .get((samples.len() as f64 * 0.95) as usize)
                        .copied()
                        .unwrap_or_default();
                    ui.label(format!(
                        "UI update p95: {p95:.3} ms ({} frames)",
                        samples.len()
                    ));
                    ui.label(format!(
                        "Directory: {:.2} ms · query: {:.3} ms",
                        self.pane().load_time.as_secs_f64() * 1000.0,
                        self.pane().query_time.as_secs_f64() * 1000.0
                    ));
                    ui.label(format!(
                        "Model: {}",
                        self.pane()
                            .snapshot
                            .as_ref()
                            .map(|s| foldex_ui::format_size(s.managed_bytes as u64))
                            .unwrap_or_default()
                    ));
                    ui.label("Ctrl+Alt+P to hide. UI timing excludes GPU presentation.");
                    ui.label(egui::RichText::new(&self.adapter_info).small());
                    ui.label(format!("UI frames: {}", self.frame_count));
                    ui.label(format!(
                        "Previous renderer CPU: {:.3} ms",
                        self.renderer_cpu_ms
                    ));
                    if let Some(s) = self.process_samples.last() {
                        ui.separator();
                        ui.label(format!(
                            "Private: {} · working set: {}",
                            foldex_ui::format_size(s.private_bytes),
                            foldex_ui::format_size(s.working_set_bytes)
                        ));
                        ui.label(format!(
                            "Handles: {} · managed model: {}",
                            s.handles,
                            foldex_ui::format_size(self.engine.managed_bytes() as u64)
                        ));
                    }
                    if foldex_ui::components::text_button(
                        ui,
                        &self.theme,
                        "Save performance report",
                        false,
                    )
                    .clicked()
                    {
                        let report = serde_json::json!({"renderer":if cfg!(feature="renderer-glow"){"glow"}else{"wgpu-dx12"},"adapter":self.adapter_info,"ui_frame_count":self.frame_count,"renderer_cpu_ms":self.renderer_cpu_ms,"repaint_causes":format!("{:?}",ctx.repaint_causes()),"launch_to_first_ui_ms":self.first_frame,"ui_samples_ms":samples,"process_samples":self.process_samples,"model_bytes":self.engine.managed_bytes(),"presentation_measured":false});
                        report_to_save = serde_json::to_vec_pretty(&report).ok();
                    }
                },
            );
            self.show_stats = stats_open;
            if let Some(bytes) = report_to_save {
                self.job(Job::Save {
                    path: self.theme_path.with_file_name("performance-report.json"),
                    bytes,
                });
            }
        }
        self.frames
            .push_back(begin.elapsed().as_secs_f64() * 1000.0);
        self.search_stress_sample(begin.elapsed().as_secs_f64() * 1000.0);
        self.navigation_stress_sample();
        self.resize_stress_step(&ctx);
        if let Some(stress) = self.benchmark.as_mut().and_then(|run| run.stress.as_mut()) {
            let graphics_ns = crate::performance::graphics_timings();
            if stress.actions > 0 && !stress.finished {
                let ui_ms = begin.elapsed().as_secs_f64() * 1000.0;
                stress.frame_ms.push(ui_ms);
                stress.frames.push(crate::performance::StressFrame {
                    at_ms: self.started.elapsed().as_secs_f64() * 1000.0,
                    interval_ms: stress.intervals_ms.last().copied().unwrap_or(0.0),
                    raw_input_ms: stress.raw_input_ms,
                    graphics_ns,
                    managed_bytes: self.engine.managed_bytes(),
                    action: stress.actions,
                    tabs: self.tabs.len(),
                    panes: self.desk.groups.len(),
                    setup_ms: stages[0],
                    assets_ms: stages[1] - stages[0],
                    poll_ms: stages[2] - stages[1],
                    chrome_ms: stages[3] - stages[2],
                    workspace_ms: stages[4] - stages[3],
                    overlays_ms: ui_ms - stages[4],
                    ui_ms,
                    previous_renderer_ms: self.renderer_cpu_ms,
                });
            }
        }
        if self.frames.len() > 240 {
            self.frames.pop_front();
        }
        if moving {
            ctx.request_repaint();
        }
        // Platforms without the Win32 frame retain a resize affordance.
        if self.native_frame.is_none() {
            let r = ui
                .interact(
                    Rect::from_min_size(rect.right_bottom() - vec2(14.0, 14.0), vec2(14.0, 14.0)),
                    Id::new("resize-window"),
                    Sense::drag(),
                )
                .on_hover_cursor(egui::CursorIcon::ResizeNwSe);
            if r.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(
                    egui::ResizeDirection::SouthEast,
                ));
            }
        }
        crate::native_keys::set_file_mode(
            !self.launcher_open
                && !self.popup_active()
                && !Self::editing(&ctx)
                && ctx.input(|i| i.focused),
        );
        self.service_benchmark(&ctx);
    }
}
impl Foldex {
    fn session_snapshot(&self) -> Session {
        Session {
            tabs: self.tabs.iter().map(|t| t.left.path.clone()).collect(),
            active: self.active,
            bookmarks: self.bookmarks.clone(),
            recent: self.recent.iter().cloned().collect(),
            home_history: self.home_history.clone(),
            sidebar_preferences: self.sidebar_sections.preferences().clone(),
            sidebar: self.sidebar,
            sidebar_collapsed: self.sidebar_sections.bits(),
            workspace: Some(self.workspace_state()),
            saved_layouts: self.saved_layouts.clone(),
            folder_views: self.folder_views.clone(),
            command_bookmarks: Some(self.commands.bookmarks()),
            context_pins: self.context.pins(),
            hotkeys: Some(self.hotkeys.bindings().to_vec()),
            launcher: self.launcher.settings.clone(),
        }
    }
    fn service_session(&mut self, ctx: &egui::Context) {
        self.service_window_transfers(ctx);
        if self.persistence.preferences_pending() {
            let local = serde_json::to_value(self.session_snapshot());
            if let Ok(local) = local
                && let Some(result) = self.persistence.poll_preferences(&local)
            {
                match result.and_then(|value| {
                    if crate::persistence::shared::preferences(&value)
                        == crate::persistence::shared::preferences(&local)
                    {
                        Ok(None)
                    } else {
                        serde_json::from_value::<Session>(value)
                            .map(Some)
                            .map_err(|error| error.to_string())
                    }
                }) {
                    Ok(Some(session)) => self.apply_shared_preferences(session),
                    Ok(None) => {}
                    Err(error) => self.toast(
                        format!("Shared preferences could not be refreshed: {error}"),
                        true,
                    ),
                }
            }
        }
        // Benchmark sessions already live in disposable isolated directories.
        // Include normal hashing, debounce and persistence in their timings.
        let now = Instant::now();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for tab in &self.tabs {
            tab.left.path.hash(&mut hash);
            hash_pane(&tab.left, &mut hash);
            if let Some(p) = &tab.right {
                p.path.hash(&mut hash);
                hash_pane(p, &mut hash);
            }
            tab.vertical.hash(&mut hash);
            tab.split.to_bits().hash(&mut hash);
        }
        self.active.hash(&mut hash);
        self.desk.hash_state(&mut hash);
        self.bookmarks.hash(&mut hash);
        self.recent.hash(&mut hash);
        self.home_usage_revision.hash(&mut hash);
        self.sidebar.hash(&mut hash);
        self.sidebar_sections.bits().hash(&mut hash);
        self.sidebar_sections.preferences().hash(&mut hash);
        self.theme.sidebar_width.to_bits().hash(&mut hash);
        self.active_pane.hash(&mut hash);
        self.inspector.hash(&mut hash);
        self.inspector_target_width.to_bits().hash(&mut hash);
        self.launcher.settings.hash(&mut hash);
        let signature = hash.finish();
        if signature != self.session_signature {
            self.session_signature = signature;
            self.session_dirty = true;
            self.save_due = Some(now + Duration::from_millis(300));
        }
        if let Some((revision, result)) = self.persistence.poll() {
            match result {
                Ok(()) => self.saved_revision = revision,
                Err(e) => {
                    let closing = self.close_requested.is_some();
                    if self.updates.exit_pending {
                        self.cancel_update_exit(ctx, "The workspace could not be saved. Installation was cancelled; your current version is unchanged.");
                    }
                    self.close_failed = closing;
                    if closing && !self.explorer_visible {
                        self.show_explorer(ctx);
                    }
                    self.toast(
                        format!(
                            "Workspace could not be saved: {e}{}",
                            if closing {
                                ". Close again to exit without saving."
                            } else {
                                ""
                            }
                        ),
                        true,
                    );
                    self.session_dirty = true;
                    self.save_due = None;
                }
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_explorer_close(ctx);
            if self.allow_close {
                return;
            }
        }
        if self.close_requested.is_some() && self.session_dirty && self.save_due.is_some() {
            self.save_due = Some(now);
        }
        if self.close_requested.is_some() {
            self.options_popup.flush_theme_preferences(ctx);
        }
        if self.session_dirty && self.save_due.is_some_and(|due| due <= now) {
            match serde_json::to_vec(&self.session_snapshot())
                .map_err(|error| crate::persistence::SubmitError::Invalid(error.to_string()))
                .and_then(|bytes| self.persistence.save(self.session_path.clone(), bytes))
            {
                Ok(revision) => {
                    self.session_revision = revision;
                    self.session_dirty = false;
                    self.save_due = None;
                }
                Err(e) => {
                    let retryable = e.retryable();
                    self.toast(format!("Workspace could not be queued: {e}"), true);
                    if self.updates.exit_pending {
                        self.cancel_update_exit(ctx, "The workspace could not be queued for saving. Installation was cancelled.");
                        self.save_due = None;
                    } else if retryable {
                        self.save_due = Some(now + Duration::from_millis(50));
                    } else {
                        self.save_due = None;
                        self.close_failed = self.close_requested.is_some();
                    }
                }
            }
        }
        if self.close_requested.is_some() && !self.allow_close {
            if self.updates.exit_pending
                && let Some(error) = self.options_popup.theme_preferences_error()
            {
                let error =
                    format!("Appearance could not be saved: {error}. Installation was cancelled.");
                self.cancel_update_exit(ctx, &error);
                return;
            }
            if self.updates.exit_pending && self.platform.update_busy() {
                self.cancel_update_exit(ctx, "File work started while preparing the update. Finish it, then try installing again.");
                return;
            }
            if !self.session_dirty
                && self.session_revision > 0
                && self.saved_revision >= self.session_revision
                && self.options_popup.theme_preferences_saved()
            {
                if !self.updates.exit_pending || self.updates.arm_handoff() {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.cancel_update_exit(
                        ctx,
                        "Could not prepare the installer. Check for updates and try again.",
                    );
                }
            } else if !self.close_failed {
                ctx.request_repaint_after(Duration::from_millis(16));
            }
        } else if let Some(due) = self.save_due {
            ctx.request_repaint_after(due.saturating_duration_since(now));
        }
    }

    fn apply_shared_preferences(&mut self, mut session: Session) {
        self.bookmarks = crate::bookmarks::normalize(session.bookmarks);
        self.saved_layouts = session.saved_layouts.into_iter().take(16).collect();
        for layout in &mut self.saved_layouts {
            layout.state.clear_navigation_history();
        }
        self.folder_views = session.folder_views.into_iter().take(128).collect();
        if self.context.pins() != session.context_pins {
            self.context.set_pins(&session.context_pins);
        }
        if let Some(bookmarks) = session.command_bookmarks {
            self.commands.set_bookmarks(bookmarks);
        }
        if let Some(mut bindings) = session.hotkeys.take() {
            crate::hotkeys::Hotkeys::add_keyboard_defaults(&mut bindings);
            if self.hotkeys.bindings() != bindings {
                self.hotkeys = crate::hotkeys::Hotkeys::from_bindings(bindings);
            }
        }
        if self.sidebar_sections.preferences() != &session.sidebar_preferences {
            self.sidebar_sections = chrome::SidebarSections::from_preferences(
                self.sidebar_sections.bits(),
                session.sidebar_preferences,
            );
        }
        session.home_history.normalize();
        if self.home_history != session.home_history {
            self.home_history = session.home_history;
            self.home_usage_revision = self.home_usage_revision.wrapping_add(1);
        }
        self.recent = session.recent.into_iter().take(10).collect();
        let settings = session.launcher;
        if self.launcher.settings.background != settings.background
            || self.launcher.settings.clipboard_history != settings.clipboard_history
        {
            self.launcher
                .set_launcher_flags(settings.background, settings.clipboard_history);
        }
        if self.launcher.settings.file_roots != settings.file_roots {
            let roots = settings
                .file_roots
                .iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join(";");
            if let Err(error) = self.launcher.apply_launcher_roots(&roots) {
                self.toast(error, true);
            }
        }
        if self.launcher.settings.hotkey != settings.hotkey
            && let Err(error) = self.launcher.apply_launcher_shortcut(&settings.hotkey)
        {
            self.toast(error, true);
        }
        self.hotkeys
            .set_launcher_shortcut(&self.launcher.settings.hotkey);
        self.hotkeys.update_palette(&mut self.commands);
        self.service_sidebar_network();
    }

    fn service_benchmark(&mut self, ctx: &egui::Context) {
        let Some(run) = &mut self.benchmark else {
            return;
        };
        if run.done {
            return;
        }
        let ready = self.tabs[self.active].left.snapshot.is_some();
        if ready && run.first_directory_ms.is_none() {
            run.first_directory_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
        }
        let expires = run.expires;
        if Instant::now() >= expires {
            run.done = true;
            let pane = &self.tabs[self.active].left;
            let final_view = serde_json::json!({"path":pane.path,"entries":pane.view_len(),
                "model_entries":pane.snapshot.as_ref().map(|snapshot|snapshot.entries.len()),
                "snapshot_complete":pane.snapshot.as_ref().is_some_and(|snapshot|snapshot.complete),
                "loading":pane.loading,"recursive":pane.recursive,
                "limit":pane.snapshot.as_ref().and_then(|snapshot|snapshot.limit).map(|limit|format!("{limit:?}")),
                "shared_memory_pressure":pane.snapshot.as_ref().is_some_and(|snapshot|snapshot.shared_memory_pressure),
                "retry_pending":pane.memory_retry_pending(),
                "publication_ready":pane.displayed_view_is_current(),"error":pane.error});
            let report = serde_json::json!({"schema":1,"workload":"native-startup-idle","renderer":if cfg!(feature="renderer-glow"){"glow"}else{"wgpu"},"adapter":self.adapter_info,"pre_framework_ms":run.pre_framework_ms,"framework_ready_ms":run.framework_ready_ms,"app_ready_ms":run.app_ready_ms,"shell_assets":!run.no_shell_assets,"first_ui_callback_ms":self.first_frame,"first_directory_ui_ms":run.first_directory_ms,"elapsed_ms":self.started.elapsed().as_secs_f64()*1000.0,"ui_callbacks":self.frame_count,"ui_samples_ms":self.frames,"process_samples":self.process_samples,"managed_model_bytes":self.engine.managed_bytes(),"final_view":final_view,"presentation_measured":false,"scheduled_metrics_hz":if run.no_metrics{0}else{1},"final_repaint_causes":format!("{:?}",ctx.repaint_causes()),"monitor":run.monitor,"stress":run.stress.as_ref().map(crate::performance::StressRun::report),"full_search":run.full_search.as_ref().map(|search| &search.samples),"navigation":run.navigation.as_ref().map(|nav| &nav.samples),"window_scaling":crate::performance::window_scaling(),"resize":run.resize.as_ref().map(|resize| &resize.samples)});
            let mut report = report;
            report["fixes"] = run
                .fixes
                .as_ref()
                .map(crate::performance::FixesUiRun::report)
                .unwrap_or(serde_json::Value::Null);
            report["launcher"] = run
                .launcher
                .as_ref()
                .map(crate::performance::LauncherUiRun::report)
                .unwrap_or(serde_json::Value::Null);
            report["keyboard"] = run
                .keyboard
                .as_ref()
                .map(crate::performance::KeyboardUiRun::report)
                .unwrap_or(serde_json::Value::Null);
            let result = (|| -> std::io::Result<()> {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&run.output)?;
                file.write_all(&serde_json::to_vec_pretty(&report)?)
            })();
            if let Err(error) = result {
                eprintln!("UI benchmark report failed: {error}");
            }
            self.allow_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if run.no_metrics {
            ctx.request_repaint_after(expires.saturating_duration_since(Instant::now()));
        } else {
            if self.metrics_requested.elapsed() >= Duration::from_secs(1) {
                self.metrics_requested = Instant::now();
                self.job(Job::Metrics {
                    id: self.process_samples.len() as u64,
                });
            }
            ctx.request_repaint_after(
                Duration::from_secs(1)
                    .saturating_sub(self.metrics_requested.elapsed())
                    .min(expires.saturating_duration_since(Instant::now())),
            );
        }
    }
}
