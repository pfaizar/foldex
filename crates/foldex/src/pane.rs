use crate::search::{
    MAX_ACTION_PATHS, SearchClient, SearchEvent, SearchWindow, WINDOW_LEAD_ROWS, WINDOW_ROWS,
};
use egui::{
    Align2, Color32, Id, Key, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2,
};
use foldex_core::{Browser, Engine, Event, Query, Snapshot, SortColumn, ViewRestore};
use foldex_ui::{Icon, Theme, icon};
mod breadcrumbs;
mod chrome;
pub(crate) mod columns;
mod footer;
mod keyboard;
mod listing;
mod navigation;
mod recycle;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const MIN_CONTENT_WIDTH: f32 = 360.0;
pub(crate) const MIN_INSPECTOR_WIDTH: f32 = 180.0;
pub(crate) fn inspector_available(ctx: &egui::Context, t: &Theme) -> bool {
    ctx.content_rect().width() >= (MIN_CONTENT_WIDTH + MIN_INSPECTOR_WIDTH) * t.metrics().scale
}

/// Deletion intent stays explicit while a virtual selection resolves its paths.
/// The native worker receives `DeletePermanent` only after app confirmation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteMode {
    Recycle,
    Permanent,
}

#[derive(Clone)]
pub enum Action {
    RecycleBin(crate::recycle_bin::Intent),
    Navigate(PathBuf),
    GoPath(PathBuf),
    Address,
    EditPath(PathBuf),
    BreadcrumbChildren {
        path: PathBuf,
        current: Option<PathBuf>,
        anchor: Rect,
        trigger: Id,
    },
    BreadcrumbAncestors {
        paths: Vec<PathBuf>,
        anchor: Rect,
        trigger: Id,
    },
    Open(PathBuf),
    Reveal(PathBuf),
    NewTab(PathBuf),
    NewWindow(PathBuf),
    MoveTabToNewWindow,
    Copy(Vec<PathBuf>, bool),
    DragFiles(Vec<PathBuf>),
    Paste(PathBuf),
    Rename(PathBuf),
    RenameMany(Vec<PathBuf>),
    CopyPaths(Vec<PathBuf>),
    Properties(Vec<PathBuf>),
    Notify(String),
    Recycle(Vec<PathBuf>),
    Delete {
        paths: Vec<PathBuf>,
        mode: DeleteMode,
    },
    NewFolder(PathBuf),
    Inspect(PathBuf),
    ToggleInspector,
    Bookmark(PathBuf),
    FolderOptions,
    Split(bool),
    SplitAt {
        path: PathBuf,
        vertical: bool,
    },
    ClosePane,
    Context {
        paths: Vec<PathBuf>,
        background: bool,
        position: Pos2,
    },
}

#[derive(Clone)]
pub enum SelectionCommand {
    Copy,
    Cut,
    Recycle,
    Delete(DeleteMode),
    Context(Pos2),
    Rename,
    CopyPaths,
    Properties,
}

impl SelectionCommand {
    fn action(self, paths: Vec<PathBuf>) -> Action {
        match self {
            Self::Copy => Action::Copy(paths, false),
            Self::Cut => Action::Copy(paths, true),
            Self::Recycle => Action::Recycle(paths),
            Self::Delete(mode) => Action::Delete { paths, mode },
            Self::Context(position) => Action::Context {
                paths,
                background: false,
                position,
            },
            Self::Rename => Action::RenameMany(paths),
            Self::CopyPaths => Action::CopyPaths(paths),
            Self::Properties => Action::Properties(paths),
        }
    }
}

struct Marquee {
    anchor: Pos2,
    press: Pos2,
    base: crate::selection::Selection,
    additive: bool,
    active: bool,
}

struct MemoryRetry {
    used_at_failure: usize,
    after: Instant,
}

pub struct Pane {
    pub id: u64,
    pub browser: Browser,
    pub path: PathBuf,
    /// Derived from the application's one persisted bookmark list before paint.
    pub bookmarked: bool,
    breadcrumb_path: PathBuf,
    breadcrumb_current: usize,
    breadcrumbs: Vec<foldex_ui::breadcrumb::Item>,
    breadcrumb_bar: foldex_ui::breadcrumb::Bar,
    pub breadcrumb_active: Option<Id>,
    pub breadcrumb_anchor: Option<Rect>,
    pub breadcrumb_hidden: usize,
    pub compact_paths: bool,
    pub breadcrumb_history: bool,
    breadcrumb_menu: breadcrumbs::BreadcrumbMenu,
    pub snapshot: Option<Arc<Snapshot>>,
    pending_snapshot: Option<Arc<Snapshot>>,
    retired: Vec<Arc<Snapshot>>,
    pub indices: Arc<[u32]>,
    pub query: Query,
    rendered_query: Query,
    pub filter: String,
    pub filter_lower: String,
    pub address: String,
    pub address_mode: bool,
    pub focus_address: bool,
    pub focus_filter: bool,
    pub filter_input_frame: Option<u64>,
    pub selected: crate::selection::Selection,
    pub anchor: Option<usize>,
    pub focused: Option<usize>,
    pub navigation: crate::navigation::Navigation,
    visit_pending: bool,
    pub home_selected: Option<PathBuf>,
    recycle_model: Option<Arc<crate::recycle_bin::Model>>,
    recycle_requested: Option<(u64, u64, u64)>,
    pub(crate) recycle_refresh_requested: bool,
    pub(crate) recycle_busy: bool,
    pub loading: bool,
    pub resolving: bool,
    pub recursive: bool,
    pub scanning: bool,
    pub error: Option<String>,
    pub cached: bool,
    pub pending_selection: Option<std::ffi::OsString>,
    pub needs_refresh: bool,
    memory_retry: Option<MemoryRetry>,
    nav_generation: u64,
    routing_revision: u64,
    query_generation: u64,
    render_epoch: u64,
    pub load_time: Duration,
    pub query_time: Duration,
    pub requested_at: Instant,
    pub last_selection: Option<PathBuf>,
    pub scroll_to: Option<usize>,
    row_cache: HashMap<u32, listing::RowText>,
    pub column_name: f32,
    columns: columns::ColumnState,
    pub visible_rows: std::ops::Range<usize>,
    focus_visual: Option<Rect>,
    keyboard: keyboard::ListingKeyboard,
    pub zoom: f32,
    pub split_orientation: Option<bool>,
    pub grid_columns: usize,
    header_height: f32,
    listing_motion: foldex_ui::motion::LayoutMotion,
    listing_clicks: listing::ListingClicks,
    numeric_motion: foldex_ui::rolling::RollingNumbers,
    listing_snapshot: Option<u64>,
    presented_entries: HashMap<Id, columns::EntryRevision>,
    presented_scratch: HashMap<Id, columns::EntryRevision>,
    listing_appearing: usize,
    listing_primed: bool,
    last_grid: Option<(foldex_ui::view::GridMetrics, [f32; 2])>,
    zoom_from: Option<(foldex_ui::view::GridMetrics, [f32; 2])>,
    listing_states: HashMap<Id, [f32; 4]>,
    name_cache: HashMap<u32, (u32, Arc<egui::Galley>, u64)>,
    name_style: (u32, [u8; 3], [u8; 3], u32, usize),
    match_paint: foldex_match::Matcher,
    zoom_open: bool,
    zoom_height: f32,
    zoom_thumb: f32,
    zoom_opened: u64,
    zoom_anchor: Rect,
    zoom_focus: foldex_ui::components::PopupFocus,
    zoom_hover_blocked: bool,
    footer_layout: footer::FooterLayout,
    footer_counts: footer::CountLabels,
    kind_counts: Option<(usize, usize)>,
    search_timer_started: Option<Instant>,
    search_elapsed: Duration,
    more_menu: foldex_ui::components::MenuState,
    more_anchor: Rect,
    marquee: Option<Marquee>,
    search: Option<SearchClient>,
    active: bool,
    search_request: u64,
    requested_start: Option<usize>,
    window_start: usize,
    search_total: Option<usize>,
    // The advertised extent survives partial publications while an exact count is pending.
    search_extent: usize,
    search_more: bool,
    pub(crate) search_status: Option<foldex_index::IndexStatus>,
    window_status: Option<foldex_index::IndexStatus>,
    selection_boundary: Option<foldex_index::IndexStatus>,
    select_all_pending: bool,
    invert_pending: bool,
    pending_action: Option<(u64, SelectionCommand, Vec<(u32, u32)>)>,
    deferred_action: Option<Action>,
}

const PANE_MENU_SECTIONS: [foldex_ui::components::MenuSection<'static>; 4] = [
    foldex_ui::components::MenuSection::separator(9),
    foldex_ui::components::MenuSection::separator(3),
    foldex_ui::components::MenuSection::separator(1),
    foldex_ui::components::MenuSection::separator(6),
];

fn pane_menu_items(
    recursive: bool,
    split_orientation: Option<bool>,
    show_hidden: bool,
    has_parent: bool,
    item_count: usize,
    selected_count: usize,
    virtual_route: bool,
) -> [foldex_ui::components::MenuItem<'static>; 13] {
    use foldex_ui::components::MenuItem;
    [
        MenuItem {
            id: 8,
            label: "Parent folder",
            shortcut: "Alt+Up",
            glyph: Icon::Up,
            checked: None,
            enabled: has_parent && !virtual_route,
        },
        MenuItem {
            id: 5,
            label: "Refresh",
            shortcut: "F5",
            glyph: Icon::Refresh,
            checked: None,
            enabled: true,
        },
        MenuItem {
            id: 0,
            label: "Expanded search",
            shortcut: "Ctrl+E",
            glyph: Icon::Recursive,
            checked: Some(recursive),
            enabled: !virtual_route,
        },
        MenuItem {
            id: 4,
            label: "Show hidden files",
            shortcut: "Ctrl+H",
            glyph: Icon::File,
            checked: Some(show_hidden),
            enabled: !virtual_route,
        },
        MenuItem {
            id: 9,
            label: "Select All",
            shortcut: "Ctrl+A",
            glyph: Icon::SelectAll,
            checked: None,
            enabled: item_count > 0,
        },
        MenuItem {
            id: 10,
            label: "Select None",
            shortcut: "",
            glyph: Icon::SelectNone,
            checked: None,
            enabled: selected_count > 0,
        },
        MenuItem {
            id: 11,
            label: "Invert Selection",
            shortcut: "",
            glyph: Icon::InvertSelection,
            checked: None,
            enabled: selected_count > 0,
        },
        MenuItem {
            id: 3,
            label: "New tab",
            shortcut: "Ctrl+T",
            glyph: Icon::Plus,
            checked: None,
            enabled: true,
        },
        MenuItem {
            id: 12,
            label: "Move to New Window",
            shortcut: "",
            glyph: Icon::Window,
            checked: None,
            enabled: true,
        },
        MenuItem {
            id: 1,
            label: "Split right",
            shortcut: "Ctrl+Shift+S",
            glyph: Icon::Split,
            checked: Some(split_orientation == Some(false)),
            enabled: true,
        },
        MenuItem {
            id: 2,
            label: "Split below",
            shortcut: "Ctrl+S",
            glyph: Icon::SplitBelow,
            checked: Some(split_orientation == Some(true)),
            enabled: true,
        },
        MenuItem {
            id: 7,
            label: "Close split pane",
            shortcut: "",
            glyph: Icon::Close,
            checked: None,
            enabled: split_orientation.is_some(),
        },
        MenuItem {
            id: 6,
            label: "Folder Options",
            shortcut: "",
            glyph: Icon::Settings,
            checked: None,
            enabled: true,
        },
    ]
}

impl Pane {
    fn more_menu_command(&mut self, item: u32) -> Option<Action> {
        match item {
            0 if !self.is_virtual() => self.toggle_recursive(),
            1 => return Some(Action::Split(false)),
            2 => return Some(Action::Split(true)),
            3 => {
                return Some(Action::NewTab(if self.is_virtual() {
                    PathBuf::from(self.location_text())
                } else {
                    self.path.clone()
                }));
            }
            4 if !self.is_virtual() => {
                self.query.show_hidden = !self.query.show_hidden;
                self.refilter();
            }
            5 => self.refresh(),
            6 => return Some(Action::FolderOptions),
            7 if self.split_orientation.is_some() => return Some(Action::ClosePane),
            8 if !self.is_virtual() => self.up(),
            // These operate on global row intervals, including unloaded pages.
            9 if self.view_len() > 0 => self.select_all(),
            10 if !self.selected.is_empty() => self.clear_selection(),
            11 if !self.selected.is_empty() => self.invert_selection(),
            12 => return Some(Action::MoveTabToNewWindow),
            _ => {}
        }
        None
    }

    pub(crate) fn benchmark_open_more_menu(&mut self, ctx: &egui::Context) -> bool {
        let trigger = Id::new((self.id, "more"));
        if !self.more_anchor.is_positive()
            || !ctx
                .read_response(trigger)
                .is_some_and(|response| response.rect.is_positive())
        {
            return false;
        }
        ctx.memory_mut(|memory| memory.request_focus(trigger));
        self.more_menu.close();
        self.more_menu.toggle(ctx);
        true
    }

    pub fn has_popup(&self) -> bool {
        self.more_menu.active()
            || self.breadcrumb_menu.menu.active()
            || self.zoom_open
            || self.zoom_height > 0.25
            || self.columns.has_popup()
    }
    pub fn new(id: u64, engine: &Engine, path: PathBuf) -> Self {
        if let Some(route) = crate::navigation::special_route(&path) {
            let fallback = std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("C:/"));
            let mut pane = Self::dormant(id, engine, fallback);
            pane.restore_navigation(crate::navigation::Navigation::new(route));
            return pane;
        }
        let mut pane = Self::dormant(id, engine, path);
        pane.nav_generation = pane.browser.navigate(pane.path.clone(), false);
        pane.loading = true;
        pane
    }

    /// Share an already presented folder model while this browser verifies it.
    /// Only bounded data identities are seeded; hover/selection and layout epochs
    /// never determine whether an unchanged entry repeats its entrance fade.
    pub(crate) fn prime_presented_from(&mut self, existing: &Pane) -> bool {
        let Some(snapshot) = existing.snapshot.as_ref().filter(|snapshot| {
            !self.is_virtual()
                && !existing.is_virtual()
                && !existing.recursive
                && snapshot.complete
                && snapshot.path == self.path
                && existing.displayed_view_is_current()
                && existing.rendered_query == self.query
                && existing.zoom == self.zoom
                && existing.columns.same_presentation(&self.columns)
        }) else {
            return false;
        };
        self.accept(snapshot.clone());
        self.cached = true;
        self.listing_snapshot = Some(snapshot.id);
        self.listing_primed = true;
        for &index in self.indices.iter().take(512) {
            if let Some(entry) = snapshot.entries.get(index as usize) {
                self.presented_entries.insert(
                    Id::new((
                        self.id,
                        &snapshot.path,
                        &entry.raw_name,
                        &entry.relative_path,
                    )),
                    columns::EntryRevision::of(entry),
                );
            }
        }
        self.columns.prime_from(
            &existing.columns,
            self.path.clone(),
            self.navigation_epoch(),
            self.render_epoch,
            self.indices.clone(),
        );
        true
    }
    pub fn dormant(id: u64, engine: &Engine, path: PathBuf) -> Self {
        let browser = engine.browser();
        browser.set_active(false);
        let generation = 0;
        let breadcrumbs = breadcrumbs::make_breadcrumbs(id, &path, true);
        let breadcrumb_current = breadcrumbs.len().saturating_sub(1);
        Self {
            id,
            browser,
            path: path.clone(),
            bookmarked: false,
            breadcrumb_path: path.clone(),
            breadcrumb_current,
            breadcrumbs,
            breadcrumb_bar: Default::default(),
            breadcrumb_active: None,
            breadcrumb_anchor: None,
            breadcrumb_hidden: 0,
            compact_paths: true,
            breadcrumb_history: true,
            breadcrumb_menu: Default::default(),
            snapshot: None,
            pending_snapshot: None,
            retired: vec![],
            indices: Arc::from([]),
            query: Query::default(),
            rendered_query: Query::default(),
            filter: String::new(),
            filter_lower: String::new(),
            address: path.to_string_lossy().into_owned(),
            address_mode: false,
            focus_address: false,
            focus_filter: false,
            filter_input_frame: None,
            selected: crate::selection::Selection::default(),
            anchor: None,
            focused: None,
            navigation: crate::navigation::Navigation::new(crate::navigation::Route::Folder(path)),
            visit_pending: true,
            home_selected: None,
            recycle_model: None,
            recycle_requested: None,
            recycle_refresh_requested: false,
            recycle_busy: false,
            loading: false,
            resolving: false,
            recursive: false,
            scanning: false,
            error: None,
            cached: false,
            pending_selection: None,
            needs_refresh: false,
            memory_retry: None,
            nav_generation: generation,
            routing_revision: 0,
            query_generation: 0,
            render_epoch: 0,
            load_time: Duration::ZERO,
            query_time: Duration::ZERO,
            requested_at: Instant::now(),
            last_selection: None,
            scroll_to: None,
            row_cache: HashMap::new(),
            column_name: 0.51,
            columns: Default::default(),
            visible_rows: 0..0,
            focus_visual: None,
            keyboard: Default::default(),
            zoom: 0.0,
            split_orientation: None,
            grid_columns: 1,
            header_height: 25.0,
            listing_motion: Default::default(),
            listing_clicks: Default::default(),
            numeric_motion: foldex_ui::rolling::RollingNumbers::new(512),
            listing_snapshot: None,
            presented_entries: HashMap::new(),
            presented_scratch: HashMap::new(),
            listing_appearing: 0,
            listing_primed: false,
            last_grid: None,
            zoom_from: None,
            listing_states: HashMap::new(),
            name_cache: HashMap::new(),
            name_style: (0, [0; 3], [0; 3], 0, 0),
            match_paint: foldex_match::Matcher::new(""),
            zoom_open: false,
            zoom_height: 0.0,
            zoom_thumb: 0.0,
            zoom_opened: 0,
            zoom_anchor: Rect::ZERO,
            zoom_focus: Default::default(),
            zoom_hover_blocked: false,
            footer_layout: Default::default(),
            footer_counts: Default::default(),
            kind_counts: None,
            search_timer_started: None,
            search_elapsed: Duration::ZERO,
            more_menu: Default::default(),
            more_anchor: Rect::ZERO,
            marquee: None,
            search: None,
            active: false,
            search_request: 0,
            requested_start: None,
            window_start: 0,
            search_total: None,
            search_extent: 0,
            search_more: false,
            search_status: None,
            window_status: None,
            selection_boundary: None,
            select_all_pending: false,
            invert_pending: false,
            pending_action: None,
            deferred_action: None,
        }
    }

    pub fn attach_search(&mut self, client: SearchClient) {
        client.set_active(self.active && self.recursive);
        self.search = Some(client);
    }

    pub fn set_active(&mut self, active: bool) {
        if self.active && !active && self.loading && !self.recursive {
            self.browser.cancel_navigation();
            self.nav_generation = 0;
            self.loading = false;
        }
        self.active = active;
        if !active {
            self.columns.cancel(self.id);
        }
        self.browser
            .set_active(active && !self.recursive && !self.is_virtual());
        if let Some(search) = &self.search {
            search.set_active(active && self.recursive);
        }
        if !active {
            self.pending_action = None;
        }
    }

    /// Release large inactive models under pressure. Scroll position remains
    /// under the same UI identity; path, history, filter and view choices survive.
    pub fn park_model(&mut self, engine: &Engine) -> Option<Self> {
        if self.is_virtual()
            || self.active
            || self.recursive
            || !self.selected.is_empty()
            || self
                .snapshot
                .as_ref()
                .is_none_or(|snapshot| snapshot.managed_bytes < 256 * 1024)
        {
            return None;
        }
        let view = crate::workspace::ViewSettings::capture(self);
        let mut replacement = Self::dormant(self.id, engine, self.path.clone());
        view.apply(&mut replacement);
        replacement.navigation = self.navigation.clone();
        replacement.visit_pending = self.visit_pending;
        replacement.filter = self.filter.clone();
        replacement.filter_lower = self.filter_lower.clone();
        replacement.query = self.query.clone();
        replacement.kind_counts = self.kind_counts;
        replacement.search = self.search.take();
        replacement.breadcrumb_path = self.breadcrumb_path.clone();
        replacement.breadcrumb_current = self.breadcrumb_current;
        replacement.breadcrumbs = std::mem::take(&mut self.breadcrumbs);
        Some(std::mem::replace(self, replacement))
    }

    pub fn view_len(&self) -> usize {
        if self.recursive {
            self.search_total.unwrap_or_else(|| {
                self.search_extent.max(
                    self.window_start
                        .saturating_add(self.snapshot.as_ref().map_or(0, |s| s.entries.len()))
                        .saturating_add(usize::from(self.search_more)),
                )
            })
        } else {
            self.indices.len()
        }
    }

    pub fn entry_at(&self, row: usize) -> Option<&foldex_core::Entry> {
        let snapshot = self.snapshot.as_ref()?;
        if self.recursive {
            snapshot.entries.get(row.checked_sub(self.window_start)?)
        } else {
            snapshot.entries.get(*self.indices.get(row)? as usize)
        }
    }

    pub fn path_at(&self, row: usize) -> Option<PathBuf> {
        if self.is_recycle_bin() {
            return None;
        }
        Some(self.entry_at(row)?.path(self.snapshot.as_ref()?))
    }

    pub fn clear_selection(&mut self) {
        self.keyboard.cancel_range();
        self.selected.clear();
        self.select_all_pending = false;
        self.invert_pending = false;
        self.sync_selection();
    }

    pub fn select_all(&mut self) {
        self.keyboard.cancel_range();
        self.invert_pending = false;
        self.select_all_pending = false;
        self.selected.clear();
        if self.recursive && (self.search_total.is_none() || self.query != self.rendered_query) {
            self.select_all_pending = true;
            self.selection_boundary = if self.query == self.rendered_query {
                self.window_status.clone()
            } else {
                None
            };
            self.request_window(self.window_start, false);
        } else {
            let count = self.view_len().min(u32::MAX as usize + 1);
            if count > 0 {
                self.selected.select(0, (count - 1) as u32);
            }
            self.sync_selection();
        }
    }

    pub fn invert_selection(&mut self) {
        self.keyboard.cancel_range();
        self.select_all_pending = false;
        if self.recursive && (self.search_total.is_none() || self.query != self.rendered_query) {
            self.invert_pending = true;
            self.selection_boundary = if self.query == self.rendered_query {
                self.window_status.clone()
            } else {
                None
            };
            self.request_window(self.window_start, false);
        } else {
            self.invert_pending = false;
            self.selected.invert(self.view_len());
            self.sync_selection();
        }
    }

    /// Keep selected global positions tied to the append-only generation that painted them.
    pub fn sync_selection(&mut self) {
        if !self.recursive {
            return;
        }
        if self.query != self.rendered_query {
            if !self.selected.is_empty() {
                self.selected.clear();
                self.selection_boundary = None;
                self.pending_action = None;
                if let Some(search) = &self.search {
                    search.cancel_selection();
                }
            }
            return;
        }
        if !self.selected.is_empty() || self.select_all_pending || self.invert_pending {
            if self.selection_boundary.is_none() {
                self.selection_boundary = self.window_status.clone();
                if self.selection_boundary.is_some() {
                    self.request_window(self.window_start, false);
                }
            }
        } else if self.selection_boundary.take().is_some() {
            if let Some(search) = &self.search {
                search.cancel_selection();
            }
            self.pending_action = None;
            self.request_window(self.window_start, false);
        }
    }

    pub fn request_selection(&mut self, command: SelectionCommand) -> Option<Action> {
        if self.is_recycle_bin() {
            return self.recycle_selection(command);
        }
        if self.is_home() {
            return self
                .home_selected
                .clone()
                .map(|path| command.action(vec![path]));
        }
        self.sync_selection();
        if self.selected.is_empty() {
            return None;
        }
        if self.selected.len() > MAX_ACTION_PATHS {
            let message = format!(
                "This action supports up to {MAX_ACTION_PATHS} files; {} are selected",
                self.selected.len()
            );
            self.deferred_action = Some(Action::Notify(message.clone()));
            self.error = Some(message);
            return None;
        }
        if let Some(paths) = self.complete_selected_paths() {
            return Some(command.action(paths));
        }
        let (Some(search), Some(boundary)) = (&self.search, self.selection_boundary.clone()) else {
            let message = "The selected files are no longer available in these results".to_string();
            self.deferred_action = Some(Action::Notify(message.clone()));
            self.error = Some(message);
            return None;
        };
        let selection = search.resolve_selection(
            self.rendered_query.clone(),
            boundary,
            self.selected.intervals(),
        );
        self.pending_action = Some((selection, command, self.selected.intervals()));
        None
    }

    pub fn drain_action(&mut self) -> Option<Action> {
        self.deferred_action.take()
    }

    fn complete_selected_paths(&self) -> Option<Vec<PathBuf>> {
        if self.selected.len() > MAX_ACTION_PATHS {
            return None;
        }
        self.selected
            .iter()
            .map(|row| self.path_at(row as usize))
            .collect()
    }

    fn request_window(&mut self, start: usize, refresh: bool) {
        if let Some(search) = &self.search {
            self.search_request = search.request_as_of(
                self.path.clone(),
                self.query.clone(),
                start,
                refresh,
                self.selection_boundary.clone(),
            );
            self.requested_start = Some(start);
            self.requested_at = Instant::now();
        } else {
            self.error = Some("Expanded search is unavailable".into());
            self.scanning = false;
            self.finish_search_timer();
        }
    }

    pub(super) fn ensure_window(&mut self, visible: std::ops::Range<usize>) {
        if !self.recursive || visible.is_empty() || self.query != self.rendered_query {
            return;
        }
        let pending = self
            .search
            .as_ref()
            .is_some_and(|search| search.window_pending(self.search_request));
        if pending
            && self.requested_start.is_some_and(|start| {
                visible.start >= start && visible.end <= start.saturating_add(WINDOW_ROWS)
            })
        {
            // Small drag/wheel updates within the requested window must not cancel
            // its deep seek or its remaining pages before they can be published.
            return;
        }
        let loaded_end = self
            .window_start
            .saturating_add(self.snapshot.as_ref().map_or(0, |s| s.entries.len()));
        if visible.start < self.window_start || visible.end > loaded_end {
            // The first page must include the viewport, not end just before it.
            let start = visible.start.saturating_sub(WINDOW_LEAD_ROWS);
            // One request per viewport; worker notifications repaint when it is ready.
            if self.search_request == 0 || self.requested_window_start() != Some(start) {
                self.request_window(start, false);
            }
        } else if pending {
            // The user returned to the retained window during a distant seek.
            // Supersede it so a late response cannot evict the current viewport.
            self.request_window(self.window_start, false);
        }
    }

    fn requested_window_start(&self) -> Option<usize> {
        self.requested_start
    }
    pub fn navigate(&mut self, path: PathBuf, remember: bool) {
        if let Some(route) = crate::navigation::special_route(&path) {
            self.navigate_route(route, remember);
            return;
        }
        self.columns.leave_recycle();
        self.recycle_model = None;
        self.recycle_requested = None;
        self.recycle_refresh_requested = false;
        self.recycle_busy = false;
        self.memory_retry = None;
        self.resolving = false;
        self.routing_revision = self.routing_revision.wrapping_add(1);
        if remember {
            self.navigation
                .visit(crate::navigation::Route::Folder(path.clone()));
        }
        self.visit_pending = true;
        self.browser.cancel_scan();
        if let Some(search) = &self.search {
            search.cancel();
            search.set_active(false);
        }
        self.recursive = false;
        self.browser.set_active(self.active);
        self.reset_search();
        self.scanning = false;
        self.path = path;
        self.columns.refresh_pending = false;
        if !self.breadcrumb_history
            || !self
                .breadcrumb_path
                .ancestors()
                .take(self.breadcrumbs.len())
                .any(|path| path == self.path.as_path())
        {
            self.breadcrumb_path = self.path.clone();
            self.breadcrumbs =
                breadcrumbs::make_breadcrumbs(self.id, &self.breadcrumb_path, self.compact_paths);
        }
        self.breadcrumb_current = self.breadcrumbs.len().saturating_sub(
            1 + self
                .breadcrumb_path
                .ancestors()
                .take(self.breadcrumbs.len())
                .position(|path| path == self.path.as_path())
                .unwrap_or(0),
        );
        self.breadcrumb_active = None;
        self.breadcrumb_anchor = None;
        self.breadcrumb_hidden = 0;
        self.address = self.path.to_string_lossy().into_owned();
        self.address_mode = false;
        self.error = None;
        self.selected.clear();
        self.anchor = None;
        self.focused = None;
        self.last_selection = None;
        self.pending_selection = None;
        self.filter.clear();
        self.filter_lower.clear();
        self.query.text.clear();
        self.query.kind = foldex_core::KindFilter::All;
        self.more_menu.close();
        self.breadcrumb_menu.menu.close();
        self.zoom_open = false;
        self.marquee = None;
        self.focus_visual = None;
        self.listing_motion.clear();
        self.listing_clicks.clear();
        self.numeric_motion.clear();
        self.listing_snapshot = None;
        self.presented_entries.clear();
        self.presented_scratch.clear();
        self.listing_appearing = 0;
        self.listing_primed = false;
        self.last_grid = None;
        self.zoom_from = None;
        self.listing_states.clear();
        self.name_cache.clear();
        self.loading = true;
        self.requested_at = Instant::now();
        self.nav_generation = self.browser.navigate(self.path.clone(), false);
        if let Some(s) = self.snapshot.take() {
            self.retire(s);
        }
        if let Some(s) = self.pending_snapshot.take() {
            self.retire(s);
        }
        self.indices = Arc::from([]);
        self.kind_counts = None;
        self.render_epoch = self.render_epoch.wrapping_add(1);
        self.row_cache.clear();
        self.scroll_to = Some(0);
    }
    pub fn refresh(&mut self) {
        if self.is_recycle_bin() {
            self.recycle_refresh_requested = true;
            return;
        }
        if self.is_home() {
            return;
        }
        self.columns.source = None;
        self.columns.refresh_pending = true;
        self.memory_retry = None;
        self.routing_revision = self.routing_revision.wrapping_add(1);
        self.requested_at = Instant::now();
        self.needs_refresh = false;
        if self.recursive {
            self.selection_boundary = None;
            self.selected.clear();
            self.pending_action = None;
            self.select_all_pending = false;
            self.invert_pending = false;
            self.loading = self.snapshot.is_none();
            self.scanning = true;
            self.start_search_timer();
            self.request_window(self.window_start, true);
        } else {
            self.loading = true;
            self.nav_generation = self.browser.navigate(self.path.clone(), true);
        }
    }
    pub fn navigation_epoch(&self) -> u64 {
        self.routing_revision
    }
    pub fn back(&mut self) {
        if let Some(route) = self.navigation.back() {
            self.navigate_route(route, false);
        }
    }
    pub fn forward(&mut self) {
        if let Some(route) = self.navigation.forward() {
            self.navigate_route(route, false);
        }
    }
    pub fn up(&mut self) {
        if self.is_virtual() {
            return;
        }
        if let Some(p) = self.path.parent() {
            self.navigate(p.to_owned(), true);
        }
    }
    pub fn refilter(&mut self) {
        self.keyboard.cancel_range();
        self.filter_lower = self.filter.to_lowercase();
        self.query.text = self.filter.clone();
        if self.is_recycle_bin() {
            self.query_generation = self.query_generation.wrapping_add(1);
            self.recycle_requested = None;
            return;
        }
        if self.recursive {
            self.selected.clear();
            self.anchor = None;
            self.focused = None;
            self.selection_boundary = None;
            self.select_all_pending = false;
            self.invert_pending = false;
            self.pending_action = None;
            self.scroll_to = Some(0);
            self.kind_counts = None;
            self.search_total = None;
            self.search_extent = 0;
            self.start_search_timer();
            self.request_window(0, false);
            return;
        }
        if let Some(s) = self
            .pending_snapshot
            .as_ref()
            .or(self.snapshot.as_ref())
            .cloned()
        {
            self.query.text = self.filter.clone();
            let restore = if self.pending_selection.is_some()
                || !self.selected.is_empty()
                || self.focused.is_some()
            {
                Some(ViewRestore {
                    snapshot: self.snapshot.clone().unwrap_or_else(|| s.clone()),
                    indices: self.indices.clone(),
                    selected: self.selected.intervals(),
                    focused: self.focused,
                    target: self.pending_selection.clone(),
                })
            } else {
                None
            };
            self.query_generation =
                self.browser
                    .filter_with_restore(s, self.query.clone(), restore);
        }
    }
    fn retire(&mut self, s: Arc<Snapshot>) {
        if let Err(s) = self.browser.retire(s) {
            self.retired.push(s);
        }
    }
    fn accept(&mut self, s: Arc<Snapshot>) {
        // The worker prepares both hidden-file variants of the default order.
        // Publishing either view takes constant work at any directory size.
        // Custom queries and selection restoration remain on the worker path.
        static FAST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *FAST.get_or_init(|| std::env::var("FOLDEX_FAST_NAVIGATION").as_deref() != Ok("off"))
            && self.query.text.is_empty()
            && self.query.sort == foldex_core::SortColumn::Name
            && !self.query.descending
            && self.query.kind == foldex_core::KindFilter::All
            && self.query.type_filter.is_none()
            && self.selected.is_empty()
            && self.focused.is_none()
            && self.pending_selection.is_none()
        {
            let begin = Instant::now();
            self.query_generation = self.browser.cancel_filter();
            let visible = s.default_indices(self.query.show_hidden);
            let changed = self.snapshot.as_ref().is_none_or(|old| old.id != s.id)
                || self.rendered_query != self.query;
            if let Some(old) = self.snapshot.replace(s) {
                self.retire(old);
            }
            if let Some(old) = self.pending_snapshot.take() {
                self.retire(old);
            }
            self.kind_counts = self
                .snapshot
                .as_ref()
                .map(|s| s.kind_counts(self.query.show_hidden));
            self.indices = visible;
            if changed {
                self.anchor = None;
                self.render_epoch = self.render_epoch.wrapping_add(1);
                self.row_cache.clear();
                self.name_cache.clear();
            }
            self.rendered_query = self.query.clone();
            self.clamp_keyboard_state();
            self.match_paint = foldex_match::Matcher::new("").with_mode(self.query.match_mode);
            self.query_time = begin.elapsed();
            self.error = None;
            return;
        }
        if let Some(old) = self.pending_snapshot.replace(s) {
            self.retire(old);
        }
        self.refilter();
    }
    pub(crate) fn displayed_view_is_current(&self) -> bool {
        if self.is_recycle_bin() {
            return self.recycle_model.as_ref().is_some_and(|model| {
                self.snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.id == model.snapshot.id)
                    && self.query == self.rendered_query
            });
        }
        self.pending_snapshot.is_none()
            && self.rendered_query == self.query
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.path == self.path)
    }
    pub fn toggle_recursive(&mut self) {
        if self.is_virtual() {
            return;
        }
        self.memory_retry = None;
        self.routing_revision = self.routing_revision.wrapping_add(1);
        self.recursive = !self.recursive;
        if self.recursive {
            self.columns.enter_recursive(&mut self.query);
        }
        self.selected.clear();
        self.focused = None;
        self.anchor = None;
        self.reset_search();
        if self.recursive {
            self.browser.cancel_navigation();
            self.nav_generation = 0;
            self.browser.set_active(false);
            if let Some(search) = &self.search {
                search.set_active(self.active);
            }
            if let Some(snapshot) = self.snapshot.take() {
                self.retire(snapshot);
            }
            if let Some(snapshot) = self.pending_snapshot.take() {
                self.retire(snapshot);
            }
            self.indices = Arc::from([]);
            self.render_epoch = self.render_epoch.wrapping_add(1);
            self.loading = true;
            self.scanning = true;
            self.kind_counts = None;
            self.start_search_timer();
            self.scroll_to = Some(0);
            self.request_window(0, false);
        } else {
            if let Some(search) = &self.search {
                search.cancel();
                search.set_active(false);
            }
            self.browser.set_active(self.active);
            self.scanning = false;
            self.refresh();
        }
    }
    fn reset_search(&mut self) {
        self.keyboard = Default::default();
        self.search_request = 0;
        self.requested_start = None;
        self.window_start = 0;
        self.search_total = None;
        self.search_extent = 0;
        self.search_more = false;
        self.search_timer_started = None;
        self.search_elapsed = Duration::ZERO;
        self.search_status = None;
        self.window_status = None;
        self.selection_boundary = None;
        self.select_all_pending = false;
        self.invert_pending = false;
        self.pending_action = None;
        self.deferred_action = None;
        self.row_cache.clear();
        self.name_cache.clear();
    }
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        if self.is_recycle_bin() {
            return Vec::new();
        }
        if self.is_home() {
            return self.home_selected.iter().cloned().collect();
        }
        self.complete_selected_paths().unwrap_or_default()
    }
    pub fn primary_selected_path(&self) -> Option<PathBuf> {
        if self.is_recycle_bin() {
            return None;
        }
        if self.is_home() {
            return self.home_selected.clone();
        }
        let row = self
            .focused
            .filter(|n| self.selected.contains(&(*n as u32)))
            .map(|n| n as u32)
            .or_else(|| self.selected.iter().next())?;
        self.path_at(row as usize)
    }
    pub fn poll(&mut self) {
        self.sync_selection();
        if self.is_recycle_bin() {
            return;
        }
        for _ in 0..8 {
            let Some(event) = self.search.as_ref().and_then(SearchClient::try_recv) else {
                break;
            };
            match event {
                SearchEvent::Window(window)
                    if self.recursive && window.request == self.search_request =>
                {
                    self.accept_window(window)
                }
                SearchEvent::Progress { request, status }
                    if self.recursive && request == self.search_request =>
                {
                    self.scanning =
                        status.state == foldex_index::BuildState::Building || status.from_cache;
                    self.search_status = Some(status);
                }
                SearchEvent::Failed { request, message }
                    if self.recursive && request == self.search_request =>
                {
                    self.loading = false;
                    self.scanning = false;
                    self.finish_search_timer();
                    self.error = Some(message);
                }
                SearchEvent::Selection { selection, result } => {
                    if let Some((id, command, ranges)) = self.pending_action.take() {
                        if id == selection && ranges == self.selected.intervals() {
                            match result {
                                Ok(paths) => self.deferred_action = Some(command.action(paths)),
                                Err(error) => {
                                    let message = error.to_string();
                                    self.deferred_action = Some(Action::Notify(message.clone()));
                                    self.error = Some(message);
                                }
                            }
                        } else if id != selection {
                            self.pending_action = Some((id, command, ranges));
                        }
                    }
                }
                _ => {}
            }
        }
        while let Some(s) = self.retired.pop() {
            if let Err(s) = self.browser.retire(s) {
                self.retired.push(s);
                break;
            }
        }
        let begin = Instant::now();
        for _ in 0..24 {
            if begin.elapsed() > Duration::from_micros(700) {
                break;
            }
            let Some(e) = self.browser.try_recv() else {
                break;
            };
            match e {
                Event::Directory {
                    generation,
                    snapshot,
                    cached,
                    done,
                } if generation == self.nav_generation && !self.recursive => {
                    self.load_time = snapshot.elapsed;
                    self.loading = !done;
                    self.cached = cached;
                    let pressure = done && snapshot.shared_memory_pressure;
                    if pressure {
                        self.memory_retry = Some(MemoryRetry {
                            used_at_failure: self.browser.model_memory_usage().0,
                            after: Instant::now() + Duration::from_millis(500),
                        });
                    } else if done {
                        self.memory_retry = None;
                    }
                    // Preserve the complete view/selection through a watcher refresh's
                    // partial batches; only reconcile identities against its final model.
                    if ((!done && !cached) || pressure)
                        && self
                            .snapshot
                            .as_ref()
                            .is_some_and(|s| s.complete && s.path == snapshot.path)
                    {
                        if pressure {
                            self.cached = true;
                        }
                        self.retire(snapshot);
                    } else {
                        self.accept(snapshot);
                    }
                }
                Event::View {
                    generation,
                    snapshot,
                    indices,
                    elapsed,
                    kind_counts,
                    restored,
                } if generation == self.query_generation && !self.recursive => {
                    if self
                        .pending_snapshot
                        .as_ref()
                        .or(self.snapshot.as_ref())
                        .is_some_and(|s| s.id == snapshot.id)
                    {
                        // Publish model and its permutation together; old indices can never address a new revision.
                        if self.snapshot.as_ref().is_none_or(|s| s.id != snapshot.id)
                            || self.query != self.rendered_query
                        {
                            self.selected.clear();
                            self.focused = None;
                            self.anchor = None;
                            self.row_cache.clear();
                            self.name_cache.clear();
                        }
                        if let Some(old) = self.snapshot.replace(snapshot) {
                            self.retire(old);
                        }
                        if let Some(old) = self.pending_snapshot.take() {
                            self.retire(old);
                        }
                        self.indices = indices;
                        self.kind_counts = Some(kind_counts);
                        self.render_epoch = self.render_epoch.wrapping_add(1);
                        self.rendered_query = self.query.clone();
                        self.match_paint = foldex_match::Matcher::new(&self.rendered_query.text)
                            .with_mode(self.rendered_query.match_mode);
                        self.query_time = elapsed;
                        self.error = None;
                        if let Some(restored) = restored {
                            self.selected.replace_intervals(restored.selected);
                            self.focused = restored.focused;
                            self.anchor = restored.focused;
                            if self.pending_selection.is_some() {
                                if let Some(row) = restored.focused {
                                    self.scroll_to = Some(row.saturating_sub(3));
                                    self.pending_selection = None;
                                } else if self
                                    .snapshot
                                    .as_ref()
                                    .is_some_and(|snapshot| snapshot.complete)
                                    && !self.loading
                                {
                                    self.pending_selection = None;
                                    self.error = Some(
                                        "The requested file is no longer in this folder".into(),
                                    );
                                }
                            }
                            if restored.truncated {
                                self.error = Some(
                                    "Selection changed because its restoration budget was reached"
                                        .into(),
                                );
                            }
                        }
                        self.keyboard.cancel_range();
                        self.clamp_keyboard_state();
                    }
                }
                Event::Error {
                    generation,
                    message,
                    ..
                } if generation == self.nav_generation && !self.recursive => {
                    self.loading = false;
                    self.scanning = false;
                    self.error = Some(message);
                }
                _ => {}
            }
        }
    }

    fn accept_window(&mut self, window: Arc<SearchWindow>) {
        if self.selection_boundary.as_ref().is_some_and(|boundary| {
            boundary.generation != window.status.generation
                || boundary.entries != window.status.entries
                || boundary.revision != window.status.revision
        }) {
            return;
        }
        let same_query = self.rendered_query == self.query;
        let replaced_order = !same_query
            || self.window_status.as_ref().is_some_and(|status| {
                status.generation != window.status.generation
                    || status.revision != window.status.revision
                    || status.entries != window.status.entries
                    || status.scope != window.status.scope
                    || status.database_path != window.status.database_path
            });
        if replaced_order {
            // An unselected keyboard cursor does not pin a disk-search prefix.
            // A replacement prefix must never reinterpret its old row as a path.
            self.focused = None;
            self.anchor = None;
            self.keyboard.cancel_range();
            self.keyboard.reveal = None;
        }
        let changed = self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.id != window.snapshot.id);
        if changed {
            if let Some(old) = self.snapshot.replace(window.snapshot.clone()) {
                self.retire(old);
            }
            self.indices = window.snapshot.default_order.clone();
            self.render_epoch = self.render_epoch.wrapping_add(1);
            self.row_cache.clear();
            self.name_cache.clear();
        }
        self.window_start = window.start;
        if replaced_order || self.window_status.is_none() {
            self.kind_counts = None;
        }
        if let Some(counts) = window.kind_counts {
            self.kind_counts = Some(counts);
        }
        // A window without a count is a partial publication, not a smaller
        // result set. Keep the exact total for the same ordering/prefix. While
        // revalidating a prefix, retain its extent until a new exact count arrives.
        self.search_total = window.total.or(if replaced_order {
            None
        } else {
            self.search_total
        });
        self.search_extent = self.search_total.unwrap_or_else(|| {
            (if same_query { self.search_extent } else { 0 }).max(
                window
                    .start
                    .saturating_add(window.snapshot.entries.len())
                    .saturating_add(usize::from(window.has_more)),
            )
        });
        self.search_more = window.has_more;
        self.window_status = Some(window.status.clone());
        if self.search_status.as_ref().is_none_or(|status| {
            status.generation != window.status.generation
                || status.revision <= window.status.revision
        }) {
            self.scanning = window.status.state == foldex_index::BuildState::Building
                || window.status.from_cache;
            self.search_status = Some(window.status.clone());
        }
        self.cached = window.status.from_cache;
        self.rendered_query = self.query.clone();
        self.clamp_keyboard_state();
        self.match_paint = foldex_match::Matcher::new(&self.rendered_query.text)
            .with_mode(self.rendered_query.match_mode);
        self.query_time = window.elapsed;
        self.loading = false;
        self.error = None;
        if !self.scanning && window.total.is_some() && window.kind_counts.is_some() {
            self.finish_search_timer();
        }
        if (self.select_all_pending || self.invert_pending)
            && let Some(total) = self.search_total
        {
            self.selection_boundary = Some(window.status.clone());
            let count = total.min(u32::MAX as usize + 1);
            if self.invert_pending {
                self.selected.invert(count);
            } else if count > 0 {
                self.selected.select(0, (count - 1) as u32);
            }
            self.select_all_pending = false;
            self.invert_pending = false;
            if total > count {
                self.error = Some("Selection supports up to 4,294,967,296 rows in one view".into());
            }
            self.request_window(self.window_start, false);
        }
    }

    pub fn breadcrumb_trail(&self) -> Vec<(PathBuf, Id)> {
        if self.is_virtual() {
            return Vec::new();
        }
        let count = self.breadcrumbs.len();
        self.breadcrumbs
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                self.breadcrumb_path
                    .ancestors()
                    .nth(count - 1 - index)
                    .map(|path| (path.to_path_buf(), item.id))
            })
            .collect()
    }

    pub(crate) fn memory_retry_pending(&self) -> bool {
        self.memory_retry.is_some()
    }

    pub fn prepare_view(&mut self, ctx: &egui::Context) {
        if self.is_virtual() {
            return;
        }
        if !self.recursive
            && !self.loading
            && let Some(retry) = &mut self.memory_retry
        {
            let now = Instant::now();
            if now >= retry.after {
                let (used, capacity) = self.browser.model_memory_usage();
                let relief = (capacity / 16).max(16 * 1024);
                if capacity.saturating_sub(used) >= relief
                    && (used <= capacity / 2
                        || retry.used_at_failure.saturating_sub(used) >= relief)
                {
                    // The incomplete prefix itself can occupy space needed by
                    // its replacement. Release it when no selection needs it.
                    if self.selected.is_empty()
                        && self.focused.is_none()
                        && self
                            .snapshot
                            .as_ref()
                            .is_some_and(|s| s.shared_memory_pressure)
                    {
                        let previous = self.snapshot.take();
                        self.indices = Arc::from([]);
                        if let Some(previous) = previous {
                            self.retire(previous);
                        }
                        if let Some(previous) = self.pending_snapshot.take() {
                            self.retire(previous);
                        }
                        self.render_epoch = self.render_epoch.wrapping_add(1);
                        self.row_cache.clear();
                        self.name_cache.clear();
                    }
                    self.refresh();
                } else {
                    retry.after = now + Duration::from_secs(1);
                    ctx.request_repaint_after(Duration::from_secs(1));
                }
            } else {
                ctx.request_repaint_after(retry.after.saturating_duration_since(now));
            }
        }
        if self.needs_refresh {
            self.refresh();
        }
        if self.nav_generation == 0 && !self.resolving && !self.recursive {
            self.nav_generation = self.browser.navigate(self.path.clone(), false);
            self.loading = true;
            self.requested_at = Instant::now();
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        t: &Theme,
        active: bool,
        inspector: bool,
        assets: Option<&mut crate::assets::AssetManager>,
    ) -> Vec<Action> {
        self.sync_selection();
        self.prepare_view(ui.ctx());
        let mut actions = vec![];
        let all = ui.max_rect();
        let m = t.metrics();
        let scale = m.scale;
        let chrome = self.chrome(ui, t, all, active, inspector, &mut actions);
        let toolbar = chrome.toolbar;
        let footer = chrome.footer;
        let gains = foldex_ui::motion::layout_gains(ui.ctx(), t.duration());
        if gains.step_pixels(
            &mut self.header_height,
            if self.zoom < 0.1 && all.height() >= m.toolbar + m.footer + m.columns + 48.0 * scale {
                m.columns
            } else {
                0.0
            },
            foldex_ui::motion::MotionProfile::Standard,
        ) {
            ui.ctx().request_repaint();
        }
        let header = Rect::from_min_max(
            pos2(all.left() + 16.0 * scale, toolbar.bottom()),
            pos2(
                all.right() - 10.0 * scale,
                toolbar.bottom() + self.header_height,
            ),
        );
        let columns = self.details_header(ui, t, header, active);
        let body = Rect::from_min_max(
            pos2(all.left() + 16.0 * scale, header.bottom() + 3.0 * scale),
            pos2(all.right() - 10.0 * scale, footer.top() - 2.0 * scale),
        );
        self.listing(ui, t, active, body, &columns, assets, &mut actions);
        if self.view_len() == 0 && !(self.is_recycle_bin() && self.error.is_some()) {
            let message = if let Some(e) = &self.error {
                e.as_str()
            } else if self.loading {
                if self.is_recycle_bin() {
                    "Loading Recycle Bin…"
                } else {
                    "Opening folder…"
                }
            } else if self.recursive && self.scanning {
                "Scanning folder…"
            } else if self.query.kind == foldex_core::KindFilter::None {
                "Folders and files are hidden"
            } else if !self.filter.is_empty() {
                "No matching files"
            } else {
                if self.is_recycle_bin() {
                    "The Recycle Bin is empty"
                } else {
                    "This folder is empty"
                }
            };
            foldex_ui::components::message_at(
                ui,
                body,
                Id::new((self.id, "empty-message")),
                t,
                message,
                if self.error.is_some() {
                    Theme::color(t.danger)
                } else {
                    t.dim()
                },
            );
        }
        self.footer(ui, t, footer);
        if self.is_recycle_bin() {
            self.recycle_status_ui(ui, t, body, &mut actions);
            self.recycle_probe(
                ui.ctx(),
                chrome.breadcrumbs,
                body,
                footer,
                chrome.recycle_controls,
            );
        }
        actions
    }
}

pub fn menu_item(ui: &mut Ui, label: &str, shortcut: &str) -> bool {
    ui.add(
        egui::Button::new(label)
            .shortcut_text(shortcut)
            .min_size(vec2(225.0, 25.0))
            .frame(false),
    )
    .clicked()
}

pub fn path_label(path: &std::path::Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            friendly_path(path)
                .trim_end_matches(['\\', '/'])
                .to_string()
        })
}
pub fn friendly_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    }
}
fn file_kind(name: &str) -> String {
    let Some((_, ext)) = name.rsplit_once('.') else {
        return "File".into();
    };
    if ext.len() > 10 {
        return "File".into();
    }
    format!("{} file", ext.to_ascii_uppercase())
}
pub fn format_date(seconds: i64) -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{FILETIME, SYSTEMTIME},
            System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTimeEx},
        };
        if let Ok(ticks) = u64::try_from((seconds as i128 + 11_644_473_600) * 10_000_000) {
            let ft = FILETIME {
                dwLowDateTime: ticks as u32,
                dwHighDateTime: (ticks >> 32) as u32,
            };
            let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
            let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
            // Pure time conversion; no file query. The formatted result is cached per visible row.
            if unsafe { FileTimeToSystemTime(&ft, &mut utc) } != 0
                && unsafe { SystemTimeToTzSpecificLocalTimeEx(std::ptr::null(), &utc, &mut local) }
                    != 0
            {
                return format!(
                    "{:04}-{:02}-{:02} {:02}:{:02}",
                    local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
                );
            }
        }
    }
    format_date_utc(seconds)
}
fn format_date_utc(seconds: i64) -> String {
    if seconds <= 0 {
        return "—".into();
    }
    let days = seconds.div_euclid(86400) + 719468;
    let era = days.div_euclid(146097);
    let doe = days - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    if m <= 2 {
        y += 1;
    }
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        seconds.rem_euclid(86400) / 3600,
        seconds.rem_euclid(3600) / 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use foldex_core::{EngineOptions, Entry};

    #[test]
    fn folder_menu_groups_actions_without_changing_command_ids_or_split_state() {
        let items = pane_menu_items(true, Some(true), false, false, 7, 2, false);
        assert_eq!(
            items.iter().map(|item| item.id).collect::<Vec<_>>(),
            [8, 5, 0, 4, 9, 10, 11, 3, 12, 1, 2, 7, 6]
        );
        assert!(!items[0].enabled);
        assert_eq!(items[2].checked, Some(true));
        assert!(matches!(items[2].glyph, Icon::Recursive));
        assert_eq!(items[3].checked, Some(false));
        assert_eq!(items[9].checked, Some(false));
        assert_eq!(items[10].checked, Some(true));
        assert!(matches!(items[9].glyph, Icon::Split));
        assert!(matches!(items[10].glyph, Icon::SplitBelow));
        assert!(items[11].enabled);
        assert!(!pane_menu_items(false, None, false, true, 0, 0, false)[11].enabled);
        assert_eq!(
            PANE_MENU_SECTIONS.map(|section| section.before),
            [9, 3, 1, 6]
        );
        assert!(
            PANE_MENU_SECTIONS
                .iter()
                .all(|section| section.label.is_empty())
        );
        assert_eq!(items[12].label, "Folder Options");
        assert_eq!(items[12].shortcut, "");
        assert_eq!(items[8].label, "Move to New Window");
        assert!(matches!(items[8].glyph, Icon::Window));
    }

    #[test]
    fn folder_menu_selection_enablement_uses_counts_and_keeps_empty_invert_disabled() {
        for (items, selected, expected) in [
            (0, 0, [false, false, false]),
            (12, 0, [true, false, false]),
            (12, 1, [true, true, true]),
            (12, 12, [true, true, true]),
            (usize::MAX, usize::MAX, [true, true, true]),
        ] {
            let menu = pane_menu_items(false, None, false, true, items, selected, false);
            assert_eq!(
                [menu[4].enabled, menu[5].enabled, menu[6].enabled],
                expected
            );
            assert_eq!([menu[4].id, menu[5].id, menu[6].id], [9, 10, 11]);
            assert_eq!(menu[4].shortcut, "Ctrl+A");
            assert!(matches!(menu[4].glyph, Icon::SelectAll));
            assert!(matches!(menu[5].glyph, Icon::SelectNone));
            assert!(matches!(menu[6].glyph, Icon::InvertSelection));
        }
    }

    #[test]
    fn virtual_folder_menu_disables_filesystem_controls_but_keeps_route_and_selection_actions() {
        let menu = pane_menu_items(false, None, false, true, 8, 2, true);
        for id in [0, 4, 8] {
            assert!(!menu.iter().find(|item| item.id == id).unwrap().enabled);
        }
        for id in [3, 5, 6, 9, 10, 11, 12] {
            assert!(menu.iter().find(|item| item.id == id).unwrap().enabled);
        }
    }

    #[test]
    fn folder_menu_dispatch_keeps_native_options_and_tab_transfer_explicit() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        assert!(matches!(
            pane.more_menu_command(6),
            Some(Action::FolderOptions)
        ));
        assert!(matches!(
            pane.more_menu_command(12),
            Some(Action::MoveTabToNewWindow)
        ));
        assert!(pane.more_menu_command(u32::MAX).is_none());
        assert!(pane.more_menu_command(7).is_none());
        pane.split_orientation = Some(false);
        assert!(matches!(pane.more_menu_command(7), Some(Action::ClosePane)));
    }

    #[test]
    fn folder_menu_selection_operates_on_unloaded_result_intervals() {
        let engine = engine();
        let hub = crate::search::SearchHub::unstarted_for_test();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        let total = 1_000_123;
        pane.recursive = true;
        pane.accept_window(disk_window(750_000, total, 1));
        pane.search = Some(hub.client());
        assert!(pane.path_at(2).is_none());
        assert!(pane.more_menu_command(9).is_none());
        assert_eq!(pane.selected.len(), total);
        assert_eq!(pane.selected.intervals(), [(0, total as u32 - 1)]);
        assert!(!pane.select_all_pending);
        assert!(pane.pending_action.is_none());

        assert!(pane.more_menu_command(10).is_none());
        assert!(pane.selected.is_empty());
        assert!(!pane.select_all_pending && !pane.invert_pending);
        assert!(pane.selection_boundary.is_none());
        // A disabled Invert item must remain a no-op even if a stale activation arrives.
        assert!(pane.more_menu_command(11).is_none());
        assert!(pane.selected.is_empty());

        pane.selected.insert(2);
        assert!(pane.more_menu_command(11).is_none());
        assert_eq!(pane.selected.len(), total - 1);
        assert_eq!(pane.selected.intervals(), [(0, 1), (3, total as u32 - 1)]);
        assert!(pane.pending_action.is_none());
    }

    #[test]
    fn folder_menu_selection_preserves_pending_exact_counts() {
        let engine = engine();
        let hub = crate::search::SearchHub::unstarted_for_test();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(uncounted_window(750_000, 1_000_123, 1));
        pane.search = Some(hub.client());
        assert!(pane.more_menu_command(9).is_none());
        assert!(pane.select_all_pending);
        assert!(pane.selected.is_empty());
        assert!(pane.pending_action.is_none());
        assert!(pane.more_menu_command(10).is_none());
        // Select None is disabled while no selection exists; do not discard an
        // already requested all-results selection through a stale menu event.
        assert!(pane.select_all_pending);
        pane.clear_selection();
        pane.selected.insert(750_001);
        assert!(pane.more_menu_command(11).is_none());
        assert!(pane.invert_pending);
        assert!(!pane.select_all_pending);
    }

    fn engine() -> Engine {
        Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn snapshot(count: usize) -> Arc<Snapshot> {
        Arc::new(Snapshot::from_entries(
            PathBuf::from("virtual-fixture"),
            (0..count)
                .map(|i| {
                    Entry::from_name(format!("file{i:06}.txt"), false, i as u64, 1_792_454_400)
                })
                .collect(),
        ))
    }

    fn paint_grid(pane: &mut Pane, ctx: &egui::Context, frame: usize) {
        let theme = Theme::default();
        let body = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 650.0));
        let output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(body),
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            },
            |ui| {
                pane.listing(
                    ui,
                    &theme,
                    true,
                    body,
                    &columns::DetailsLayout::new(
                        &columns::StoredColumns::default(),
                        body,
                        1.0,
                        0.5,
                    ),
                    None,
                    &mut Vec::new(),
                )
            },
        );
        output.drop_without_applying_deltas();
    }
    #[test]
    fn same_folder_tab_and_unchanged_publications_never_replay_entry_opacity() {
        let engine = engine();
        let mut existing = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        existing.zoom = 1.0;
        existing.accept(snapshot(4));
        let mut duplicate = Pane::dormant(2, &engine, existing.path.clone());
        duplicate.zoom = existing.zoom;
        assert!(duplicate.prime_presented_from(&existing));
        let ctx = egui::Context::default();
        paint_grid(&mut duplicate, &ctx, 0);
        assert_eq!(duplicate.listing_appearing, 0);
        assert!(!duplicate.listing_motion.is_moving());
        // A sort/filter/layout epoch is not a new set of data identities.
        duplicate.render_epoch += 1;
        paint_grid(&mut duplicate, &ctx, 1);
        assert_eq!(duplicate.listing_appearing, 0);
        duplicate.accept(snapshot(4));
        paint_grid(&mut duplicate, &ctx, 2);
        assert_eq!(duplicate.listing_appearing, 0);
        let mut changed = (0..5)
            .map(|i| Entry::from_name(format!("file{i:06}.txt"), false, i as u64, 1_792_454_400))
            .collect::<Vec<_>>();
        changed[0].size += 1;
        duplicate.accept(Arc::new(Snapshot::from_entries(
            existing.path.clone(),
            changed,
        )));
        paint_grid(&mut duplicate, &ctx, 3);
        assert_eq!(
            duplicate.listing_appearing, 0,
            "changed and newly published files must not repeat arrival motion"
        );
        assert!(!duplicate.listing_motion.is_moving());
        duplicate.set_zoom(0.0);
        paint_grid(&mut duplicate, &ctx, 4);
        assert!(
            duplicate.listing_motion.is_moving(),
            "zoom must retain layout motion"
        );
    }
    #[test]
    fn duplicate_priming_rejects_a_changed_displayed_query_or_directory() {
        let engine = engine();
        let mut existing = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        existing.accept(snapshot(4));
        let mut different = Pane::dormant(2, &engine, PathBuf::from("different-fixture"));
        assert!(!different.prime_presented_from(&existing));
        let mut duplicate = Pane::dormant(3, &engine, existing.path.clone());
        duplicate.query.show_hidden = !existing.query.show_hidden;
        assert!(!duplicate.prime_presented_from(&existing));
        assert!(duplicate.snapshot.is_none());
    }
    #[test]
    fn evicting_old_listing_rows_keeps_the_visible_selection_settled() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.zoom = 1.0;
        pane.accept(snapshot(4));
        pane.selected.insert(0);
        let ctx = egui::Context::default();
        for frame in 0..60 {
            paint_grid(&mut pane, &ctx, frame);
        }
        let snapshot = pane.snapshot.as_ref().unwrap();
        let entry = &snapshot.entries[0];
        let identity = Id::new((
            pane.id,
            &snapshot.path,
            &entry.raw_name,
            &entry.relative_path,
        ));
        assert_eq!(pane.listing_states[&identity][1], 1.0);
        for row in 0..513 {
            pane.listing_states
                .insert(Id::new(("old-viewport", row)), [0.0; 4]);
        }
        paint_grid(&mut pane, &ctx, 60);
        assert_eq!(
            pane.listing_states[&identity][1], 1.0,
            "eviction must not restart a visible row's selection fade"
        );
        assert!(pane.listing_states.len() <= 512);
        assert_eq!(pane.listing_appearing, 0);
    }
    #[test]
    fn a_hundred_thousand_entries_create_only_visible_rows() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        let s = snapshot(100_000);
        pane.indices = s.default_order.clone();
        pane.snapshot = Some(s);
        pane.nav_generation = 1;
        let ctx = egui::Context::default();
        let theme = Theme::default();
        theme.apply(&ctx);
        for frame in 0..4 {
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 650.0))),
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| {
                    pane.ui(ui, &theme, true, false, None);
                },
            );
            assert!(
                pane.visible_rows.len() < 40,
                "created {} rows",
                pane.visible_rows.len()
            );
            assert!(
                output.shapes.len() < 1000,
                "created {} shapes",
                output.shapes.len()
            );
            output.drop_without_applying_deltas();
        }
        assert!(
            pane.row_cache.is_empty(),
            "Details rows must not prepare unused grid captions or metadata"
        );
        assert_eq!(pane.indices.len(), 100_000);
    }
    #[test]
    fn dragging_scrollbar_never_starts_marquee_or_changes_selection() {
        let engine = engine();
        for floating in [true, false] {
            for zoom in [0.0, 1.0] {
                let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
                let s = snapshot(1000);
                pane.indices = s.default_order.clone();
                pane.snapshot = Some(s);
                pane.zoom = zoom;
                pane.selected.select(2, 2);
                let ctx = egui::Context::default();
                let theme = Theme {
                    reduced_motion: true,
                    ..Default::default()
                };
                theme.apply(&ctx);
                ctx.style_mut_of(egui::Theme::Dark, |style| {
                    style.spacing.scroll.floating = floating;
                });
                let body = Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0));
                let mut frame = 0;
                let mut draw = |pane: &mut Pane, events: Vec<egui::Event>| {
                    let output = ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(body),
                            time: Some(frame as f64 / 60.0),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            pane.listing(
                                ui,
                                &theme,
                                true,
                                body,
                                &columns::DetailsLayout::new(
                                    &columns::StoredColumns::default(),
                                    body,
                                    1.0,
                                    0.5,
                                ),
                                None,
                                &mut Vec::new(),
                            );
                        },
                    );
                    output.drop_without_applying_deltas();
                    frame += 1;
                };
                for _ in 0..4 {
                    draw(&mut pane, Vec::new());
                }
                let start = pos2(596.0, 8.0);
                let end = pos2(596.0, 210.0);
                let button = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::default(),
                };
                draw(
                    &mut pane,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                assert!(pane.marquee.is_none(), "scrollbar press started selection");
                draw(&mut pane, vec![egui::Event::PointerMoved(end)]);
                assert!(pane.marquee.is_none(), "scrollbar drag started selection");
                draw(&mut pane, vec![button(end, false)]);
                draw(&mut pane, Vec::new());
                assert!(pane.visible_rows.start > 0, "scrollbar did not scroll");
                assert_eq!(pane.selected.intervals(), vec![(2, 2)]);

                // Empty content still supports marquee selection. A press on
                // a file now starts a file drag instead of stealing selection.
                let single = snapshot(1);
                pane.indices = single.default_order.clone();
                pane.snapshot = Some(single);
                for _ in 0..4 {
                    draw(&mut pane, Vec::new());
                }
                let start = pos2(350.0, 250.0);
                let end = pos2(500.0, 320.0);
                draw(
                    &mut pane,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                assert!(pane.marquee.is_some());
                draw(&mut pane, vec![egui::Event::PointerMoved(end)]);
                assert!(pane.marquee.as_ref().is_some_and(|drag| drag.active));
                draw(&mut pane, vec![button(end, false)]);
                assert!(pane.marquee.is_none());
            }
        }
    }

    #[test]
    fn a_new_snapshot_never_uses_the_previous_views_indices() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        let old = snapshot(1000);
        pane.indices = old.default_order.clone();
        pane.snapshot = Some(old);
        // A deselected range can retain an anchor even without a focused row.
        // Neither publication path may carry that row into a replacement model.
        pane.anchor = Some(999);
        let new = snapshot(1);
        let id = new.id;
        pane.accept(new);
        let deadline = Instant::now() + Duration::from_secs(3);
        while pane.snapshot.as_ref().unwrap().id != id && Instant::now() < deadline {
            pane.poll();
            let visible = pane.snapshot.as_ref().unwrap();
            assert!(
                pane.indices
                    .iter()
                    .all(|i| (*i as usize) < visible.entries.len())
            );
            std::thread::yield_now();
        }
        assert_eq!(pane.snapshot.as_ref().unwrap().id, id);
        assert_eq!(pane.indices.len(), 1);
        assert_eq!(pane.anchor, None);
    }

    #[test]
    fn shared_memory_pressure_recovers_and_preserves_a_complete_view() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/performance-fixtures");
        let root = base.join(format!(
            "owned-memory-retry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = root.join("first");
        let second = root.join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        for (folder, count) in [(&first, 2400), (&second, 1200)] {
            for i in 0..count {
                std::fs::File::create(folder.join(format!("file_{i:05}_metadata.txt"))).unwrap();
            }
        }
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                managed_bytes: 1024 * 1024,
                cache_bytes: 0,
                cache_directories: 0,
                native_watch: false,
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let ctx = egui::Context::default();
        let mut holder = engine.browser();
        holder.navigate(first.clone(), true);
        let take_finished = |browser: &mut Browser| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                assert!(Instant::now() < deadline, "holder did not finish");
                if let Some(Event::Directory {
                    snapshot,
                    done: true,
                    ..
                }) = browser.try_recv()
                {
                    break snapshot;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        let held = take_finished(&mut holder);
        assert!(held.complete, "fixture must fit the pool by itself");
        let mut pane = Pane::new(1, &engine, second.clone());
        pane.set_active(true);
        let drive = |pane: &mut Pane, pressure: bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                pane.poll();
                pane.prepare_view(&ctx);
                if pressure && pane.memory_retry_pending() {
                    break;
                }
                if !pressure
                    && !pane.loading
                    && !pane.memory_retry_pending()
                    && pane
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.complete)
                {
                    break;
                }
                assert!(Instant::now() < deadline, "memory recovery timed out");
                assert!(engine.managed_bytes() <= 1024 * 1024);
                std::thread::sleep(Duration::from_millis(2));
            }
        };
        drive(&mut pane, true);
        assert!(pane.snapshot.as_ref().unwrap().shared_memory_pressure);
        holder.cancel_navigation();
        drop(held);
        drive(&mut pane, false);
        assert_eq!(pane.view_len(), 1200);

        // A refresh under pressure must retain a known complete listing, then
        // revalidate it once the competing reservation is released.
        let before = pane.snapshot.as_ref().unwrap().id;
        holder.navigate(first, true);
        let held = take_finished(&mut holder);
        pane.refresh();
        drive(&mut pane, true);
        assert_eq!(pane.snapshot.as_ref().unwrap().id, before);
        assert_eq!(pane.view_len(), 1200);
        assert!(pane.cached);
        holder.cancel_navigation();
        drop(held);
        drive(&mut pane, false);
        assert_eq!(pane.view_len(), 1200);
        assert!(!pane.cached);
        drop(pane);
        drop(holder);
        drop(engine);
        let absolute = root.canonicalize().unwrap();
        let allowed = base.canonicalize().unwrap();
        assert!(absolute.starts_with(&allowed) && absolute != allowed);
        std::fs::remove_dir_all(absolute).unwrap();
    }
    #[test]
    fn formats_epoch_date_without_rounding_days() {
        assert_eq!(format_date_utc(86_400), "1970-01-02 00:00");
    }

    fn disk_window(start: usize, total: usize, revision: u64) -> Arc<SearchWindow> {
        let snapshot = snapshot(total.saturating_sub(start).min(256));
        Arc::new(SearchWindow {
            request: 1,
            start,
            snapshot: snapshot.clone(),
            total: Some(total),
            kind_counts: Some((0, total)),
            has_more: start + 256 < total,
            elapsed: Duration::ZERO,
            status: foldex_index::IndexStatus {
                generation: "owned-virtual-fixture".into(),
                revision,
                scope: snapshot.path.clone(),
                database_path: PathBuf::from("virtual-fixture-index"),
                state: foldex_index::BuildState::Complete,
                complete: true,
                entries: total as u64,
                files: total as u64,
                folders: 0,
                skipped: 0,
                excluded_links: 0,
                excluded_cache: 0,
                started_unix_ms: 0,
                updated_unix_ms: 0,
                source_root_modified_secs: None,
                from_cache: false,
                synthetic: true,
            },
        })
    }

    fn uncounted_window(start: usize, total: usize, revision: u64) -> Arc<SearchWindow> {
        let mut window = disk_window(start, total, revision);
        let partial = Arc::get_mut(&mut window).unwrap();
        partial.total = None;
        partial.kind_counts = None;
        window
    }

    #[test]
    fn partial_middle_and_end_windows_preserve_global_extent_and_selection() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        let total = 10_000_123;
        pane.recursive = true;
        pane.accept_window(disk_window(0, total, 1));
        pane.focused = Some(total - 1);
        pane.anchor = Some(total - 2);
        pane.selected.select((total - 2) as u32, (total - 1) as u32);
        pane.sync_selection();
        let selected = pane.selected.intervals();
        for start in [total / 2, total - 120, 128, total / 2 + 256] {
            pane.accept_window(uncounted_window(start, total, 1));
            assert_eq!(pane.view_len(), total, "partial window at {start}");
            assert_eq!(pane.search_total, Some(total));
            assert_eq!(pane.focused, Some(total - 1));
            assert_eq!(pane.anchor, Some(total - 2));
            assert_eq!(pane.selected.intervals(), selected);
            assert!(pane.snapshot.as_ref().unwrap().entries.len() <= 256);
        }
    }

    #[test]
    fn a_pending_new_prefix_keeps_extent_until_its_own_count_arrives() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(0, 1_000_123, 1));
        pane.accept_window(uncounted_window(500_000, 1_000_124, 2));
        assert_eq!(
            pane.search_total, None,
            "old prefix count is no longer exact"
        );
        assert_eq!(
            pane.view_len(),
            1_000_123,
            "pending counts must not clamp the viewport"
        );
        pane.accept_window(disk_window(500_000, 1_000_124, 2));
        assert_eq!(pane.view_len(), 1_000_124);
        pane.query.text = "no matches".into();
        pane.accept_window(uncounted_window(0, 0, 3));
        assert_eq!(
            pane.view_len(),
            0,
            "another query must not inherit the old extent"
        );
        pane.accept_window(disk_window(0, 0, 3));
        assert_eq!(pane.search_total, Some(0));
    }

    #[test]
    fn pending_seeks_coalesce_nearby_rows_and_cancel_when_returning_to_loaded_rows() {
        let engine = engine();
        let hub = crate::search::SearchHub::unstarted_for_test();
        let client = hub.client();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(0, 1_000_123, 1));
        pane.search = Some(client.clone());
        pane.ensure_window(500_000..500_040);
        let middle = pane.search_request;
        assert_eq!(pane.requested_start, Some(500_000 - WINDOW_LEAD_ROWS));
        assert!(client.window_pending(middle));
        for shift in [1, 20, 80, 256, 0] {
            pane.ensure_window(500_000 + shift..500_040 + shift);
            assert_eq!(
                pane.search_request, middle,
                "nearby rows restarted the seek"
            );
        }
        pane.ensure_window(900_000..900_040);
        let end = pane.search_request;
        assert!(end > middle && !client.window_pending(middle));
        pane.ensure_window(0..40);
        assert!(pane.search_request > end && !client.window_pending(end));
        assert_eq!(pane.requested_start, Some(0));
        let current = pane.search_request;
        pane.ensure_window(1..41);
        assert_eq!(pane.search_request, current);

        pane.query.text = "new query".into();
        pane.request_window(0, false);
        let query = pane.search_request;
        pane.ensure_window(700_000..700_040);
        assert_eq!(
            pane.search_request, query,
            "old rows superseded a pending query"
        );
        client.cancel();
        assert!(!client.window_pending(query));
    }

    #[test]
    fn a_new_prefix_during_pointer_use_does_not_reveal_an_automatic_row_zero_cursor() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(500_000, 1_000_123, 1));
        pane.focused = Some(500_000);
        pane.scroll_to = Some(500_000);
        let ctx = egui::Context::default();
        crate::keyboard::request_id(&ctx, pane.listing_focus_id());
        paint_grid(&mut pane, &ctx, 0);
        let before = pane.visible_rows.clone();
        assert!(before.start > 400_000);
        assert!(pane.listing_owns_keyboard(&ctx));
        pane.accept_window(uncounted_window(500_000, 1_000_124, 2));
        assert_eq!(
            pane.focused, None,
            "a new prefix must invalidate old row identities"
        );
        paint_grid(&mut pane, &ctx, 1);
        assert_eq!(
            pane.visible_rows, before,
            "automatic focus scrolled over the pointer viewport"
        );
        assert!(
            pane.listing_owns_keyboard(&ctx),
            "pointer modality lost the list's logical owner"
        );
    }

    #[test]
    fn million_row_scrollbar_middle_end_and_retouch_keep_the_same_viewport() {
        let engine = engine();
        let total = 1_000_123;
        for floating in [true, false] {
            for zoom in [0.0, 1.0] {
                let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
                pane.recursive = true;
                pane.accept_window(disk_window(0, total, 1));
                pane.zoom = zoom;
                pane.selected.insert(2);
                let ctx = egui::Context::default();
                let theme = Theme {
                    reduced_motion: true,
                    ..Default::default()
                };
                theme.apply(&ctx);
                ctx.style_mut_of(egui::Theme::Dark, |style| {
                    style.spacing.scroll.floating = floating
                });
                let body = Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0));
                let mut frame = 0;
                let mut draw = |pane: &mut Pane, events: Vec<egui::Event>| {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(body),
                            time: Some(frame as f64 / 60.0),
                            events,
                            ..Default::default()
                        },
                        |ui| {
                            pane.listing(
                                ui,
                                &theme,
                                true,
                                body,
                                &columns::DetailsLayout::new(
                                    &columns::StoredColumns::default(),
                                    body,
                                    1.0,
                                    0.5,
                                ),
                                None,
                                &mut Vec::new(),
                            )
                        },
                    )
                    .drop_without_applying_deltas();
                    frame += 1;
                };
                let button = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                };
                for _ in 0..4 {
                    draw(&mut pane, Vec::new());
                }
                let mut previous = pos2(596.0, 8.0);
                for (point, end) in [(pos2(596.0, 210.0), false), (pos2(596.0, 398.0), true)] {
                    draw(
                        &mut pane,
                        vec![egui::Event::PointerMoved(previous), button(previous, true)],
                    );
                    draw(&mut pane, vec![egui::Event::PointerMoved(point)]);
                    draw(&mut pane, vec![button(point, false)]);
                    draw(&mut pane, Vec::new());
                    let before = pane.visible_rows.clone();
                    assert!(before.start > if end { total * 9 / 10 } else { total / 4 });
                    if !end {
                        assert!(before.start < total * 3 / 4);
                    }
                    let start = before.start.saturating_sub(WINDOW_LEAD_ROWS);
                    pane.accept_window(uncounted_window(start, total, 1));
                    draw(&mut pane, Vec::new());
                    assert_eq!(pane.visible_rows, before, "partial page moved the thumb");
                    pane.accept_window(disk_window(start, total, 1));
                    draw(&mut pane, Vec::new());
                    assert_eq!(
                        pane.visible_rows, before,
                        "count publication moved the thumb"
                    );
                    draw(&mut pane, vec![button(point, true)]);
                    draw(&mut pane, vec![button(point, false)]);
                    draw(&mut pane, Vec::new());
                    assert_eq!(pane.visible_rows, before, "touching the thumb jumped rows");
                    assert_eq!(pane.selected.intervals(), vec![(2, 2)]);
                    assert!(pane.marquee.is_none());
                    assert!(pane.snapshot.as_ref().unwrap().entries.len() <= 256);
                    previous = point;
                }
            }
        }
    }

    #[test]
    fn a_million_global_rows_use_one_bounded_window_and_selection_interval() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(999_000, 1_000_123, 1));
        assert_eq!(pane.view_len(), 1_000_123);
        assert!(pane.entry_at(0).is_none());
        assert_eq!(pane.entry_at(999_000).unwrap().name, "file000000.txt");
        assert!(pane.entry_at(999_256).is_none());
        assert_eq!(pane.snapshot.as_ref().unwrap().entries.len(), 256);
        pane.select_all();
        assert_eq!(pane.selected.len(), 1_000_123);
        assert_eq!(pane.selected.intervals(), vec![(0, 1_000_122)]);
        assert!(pane.request_selection(SelectionCommand::Copy).is_none());
        assert!(
            matches!(pane.drain_action(), Some(Action::Notify(message)) if message.contains("1000123"))
        );
        assert!(
            pane.selected_paths().is_empty(),
            "a cross-page selection is never returned partially"
        );
    }

    #[test]
    fn selected_rows_reject_a_newer_prefix_and_keep_exact_loaded_paths() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(700, 900, 1));
        pane.selected.select(701, 702);
        pane.sync_selection();
        let id = pane.snapshot.as_ref().unwrap().id;
        pane.accept_window(disk_window(0, 901, 2));
        assert_eq!(pane.snapshot.as_ref().unwrap().id, id);
        assert_eq!(pane.view_len(), 900);
        assert!(
            matches!(pane.request_selection(SelectionCommand::Copy), Some(Action::Copy(paths, false)) if paths.len() == 2 && paths[0].ends_with("file000001.txt") && paths[1].ends_with("file000002.txt"))
        );
    }

    #[test]
    fn properties_uses_the_complete_selection_from_its_pinned_result_prefix() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(700, 900, 1));
        pane.selected.select(701, 702);
        // Focus outside the selection must not substitute a different path.
        pane.focused = Some(708);
        pane.sync_selection();
        let id = pane.snapshot.as_ref().unwrap().id;
        pane.accept_window(disk_window(0, 901, 2));
        assert_eq!(pane.snapshot.as_ref().unwrap().id, id);
        assert!(matches!(
            pane.request_selection(SelectionCommand::Properties),
            Some(Action::Properties(paths)) if paths.len() == 2
                && paths[0].ends_with("file000001.txt")
                && paths[1].ends_with("file000002.txt")
        ));
    }

    #[test]
    fn properties_never_returns_only_the_loaded_subset_of_a_selection() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(700, 2_000, 1));
        pane.selected.insert(701);
        pane.selected.insert(1_200);
        assert!(pane.path_at(701).is_some());
        assert!(pane.path_at(1_200).is_none());
        assert!(
            pane.request_selection(SelectionCommand::Properties)
                .is_none()
        );
        assert!(pane.pending_action.is_none());
        assert!(matches!(pane.drain_action(), Some(Action::Notify(message))
            if message.contains("no longer available")));
        assert_eq!(pane.selected.intervals(), vec![(701, 701), (1_200, 1_200)]);
    }

    #[test]
    fn properties_cannot_use_selection_positions_from_a_stale_query() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(700, 900, 1));
        pane.selected.select(701, 702);
        pane.sync_selection();
        pane.query.text = "different query".into();
        assert!(
            pane.request_selection(SelectionCommand::Properties)
                .is_none()
        );
        assert!(pane.selected.is_empty());
        assert!(pane.selection_boundary.is_none());
        assert!(pane.pending_action.is_none());
        assert!(pane.drain_action().is_none());
    }

    #[test]
    fn properties_obeys_the_existing_selection_action_limit() {
        let engine = engine();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("virtual-fixture"));
        pane.recursive = true;
        pane.accept_window(disk_window(0, MAX_ACTION_PATHS + 1, 1));
        pane.select_all();
        assert!(
            pane.request_selection(SelectionCommand::Properties)
                .is_none()
        );
        assert!(pane.pending_action.is_none());
        assert!(matches!(pane.drain_action(), Some(Action::Notify(message))
            if message.contains(&format!("up to {MAX_ACTION_PATHS}"))));
    }
}
