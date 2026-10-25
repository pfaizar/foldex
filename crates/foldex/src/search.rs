//! Bounded, worker-owned disk search. The UI only exchanges latest requests and small windows.
use crate::launcher::{matching::Matcher as LauncherMatcher, providers};
use foldex_core::{Entry, Query, Snapshot};
use foldex_index::{
    BuildOptions, Cancellation, Index, IndexRow, IndexStatus, KindFilter, QuerySpec, SortColumn,
    catalog::{NativeIndex, NativeIndexBuilder as IndexBuilder},
};
use std::{
    cell::RefCell,
    io,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
#[cfg(test)]
pub(crate) mod live_tests;
mod scopes;

const MAX_CLIENTS: usize = 4096;
// Subscriptions hold bounded result windows and selected-generation leases.
// They do not reserve a traversal or a query turn after completing their work.
const MAX_SUBSCRIPTIONS: usize = 32;
const QUERY_TURNS: usize = 2;
const PAGE_ROWS: usize = 256;
const WINDOW_PAGES: usize = 3;
pub(crate) const WINDOW_ROWS: usize = WINDOW_PAGES * PAGE_ROWS;
pub(crate) const WINDOW_LEAD_ROWS: usize = PAGE_ROWS / 2;
const REFRESH_INTERVAL: Duration = Duration::from_millis(150);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);
pub const MAX_ACTION_PATHS: usize = 4096;
const MAX_ACTION_BYTES: usize = 4 * 1024 * 1024;
type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub struct SearchWindow {
    pub request: u64,
    /// Absolute result offset, not an index into the source directory.
    pub start: usize,
    /// Entries retain backend order. Only this bounded window is materialized.
    pub snapshot: Arc<Snapshot>,
    pub total: Option<usize>,
    /// Counts with the current text/hidden settings, before kind toggles.
    pub kind_counts: Option<(usize, usize)>,
    pub has_more: bool,
    pub status: IndexStatus,
    pub elapsed: Duration,
}

#[derive(Debug)]
pub struct LauncherFile {
    pub path: PathBuf,
    pub is_dir: bool,
    pub score: i32,
}

#[derive(Debug)]
pub enum SearchEvent {
    Window(Arc<SearchWindow>),
    /// Opt-in launcher ranks are computed entirely on the search worker.
    Launcher {
        request: u64,
        rows: Vec<LauncherFile>,
        status: IndexStatus,
        complete: bool,
    },
    Progress {
        request: u64,
        status: IndexStatus,
    },
    Failed {
        request: u64,
        message: String,
    },
    Selection {
        selection: u64,
        result: Result<Vec<PathBuf>, SelectionFailure>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SelectionFailure {
    TooMany { selected: u64 },
    Changed,
    MemoryBudget,
    InvalidRange,
    Failed(String),
}

impl std::fmt::Display for SelectionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooMany { selected } => write!(
                f,
                "This action supports up to {MAX_ACTION_PATHS} files; {selected} are selected"
            ),
            Self::Changed => {
                f.write_str("The search results changed. Select the files again before this action")
            }
            Self::MemoryBudget => {
                f.write_str("The selected paths exceed this action's memory budget")
            }
            Self::InvalidRange => f.write_str("The selected rows are outside these search results"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

#[derive(Clone)]
struct Request {
    id: u64,
    serial: u64,
    scan_epoch: u64,
    root: PathBuf,
    query: Query,
    start: usize,
    refresh: bool,
    boundary: Option<IndexStatus>,
    launcher: Option<bool>,
    scan_cancel: Cancellation,
    query_cancel: Cancellation,
    started: Instant,
}

#[derive(Default)]
struct Slots {
    window: Option<Arc<SearchWindow>>,
    launcher: Option<(u64, Vec<LauncherFile>, IndexStatus, bool)>,
    progress: Option<(u64, IndexStatus)>,
    failed: Option<(u64, String)>,
    selection: Option<(u64, Result<Vec<PathBuf>, SelectionFailure>)>,
}

#[derive(Clone)]
struct SelectionRequest {
    id: u64,
    scan_epoch: u64,
    query: Query,
    boundary: IndexStatus,
    ranges: Vec<(u32, u32)>,
    cancel: Cancellation,
}

#[derive(Default)]
struct Commands {
    latest: Option<Request>,
    scan_epoch: u64,
    selection: Option<SelectionRequest>,
}

struct ClientState {
    shared: Weak<Shared>,
    registered: bool,
    owners: AtomicUsize,
    active: AtomicBool,
    closed: AtomicBool,
    sequence: AtomicU64,
    selection_sequence: AtomicU64,
    failed_request: AtomicU64,
    window_ready: AtomicU64,
    commands: Mutex<Commands>,
    slots: Mutex<Slots>,
}

impl ClientState {
    fn accepts(&self, request: &Request) -> bool {
        self.active.load(Ordering::Acquire)
            && !self.closed.load(Ordering::Acquire)
            && self.sequence.load(Ordering::Acquire) == request.id
            && !request.query_cancel.is_cancelled()
    }

    fn input(&self) -> Option<Request> {
        if !self.active.load(Ordering::Acquire) || self.closed.load(Ordering::Acquire) {
            return None;
        }
        let request = lock(&self.commands).latest.clone()?;
        if self.failed_request.load(Ordering::Acquire) == request.id {
            return None;
        }
        Some(request)
    }

    fn notify(&self) {
        if let Some(shared) = self.shared.upgrade() {
            shared.notify();
        }
    }

    fn stop(&self) {
        self.closed.store(true, Ordering::Release);
        self.active.store(false, Ordering::Release);
        let mut commands = lock(&self.commands);
        if let Some(request) = commands.latest.take() {
            request.query_cancel.cancel();
            request.scan_cancel.cancel();
        }
        if let Some(selection) = commands.selection.take() {
            selection.cancel.cancel();
        }
        drop(commands);
        self.notify();
    }

    fn window(&self, request: &Request, window: Arc<SearchWindow>) {
        if self.accepts(request) {
            let mut slots = lock(&self.slots);
            if !self.accepts(request) {
                return;
            }
            if slots.progress.as_ref().is_some_and(|(id, status)| {
                *id == request.id
                    && status.generation == window.status.generation
                    && status.revision <= window.status.revision
            }) {
                slots.progress = None;
            }
            slots.window = Some(window);
            slots.failed = None;
            drop(slots);
            if let Some(shared) = self.shared.upgrade() {
                (shared.wake)();
            }
        }
    }

    fn progress(&self, request: &Request, status: IndexStatus) {
        if self.accepts(request) {
            let mut slots = lock(&self.slots);
            if !self.accepts(request) {
                return;
            }
            slots.progress = Some((request.id, status));
            drop(slots);
            if let Some(shared) = self.shared.upgrade() {
                (shared.wake)();
            }
        }
    }

    fn launcher(
        &self,
        request: &Request,
        rows: Vec<LauncherFile>,
        status: IndexStatus,
        complete: bool,
    ) {
        if self.accepts(request) {
            let mut slots = lock(&self.slots);
            if !self.accepts(request) {
                return;
            }
            slots.launcher = Some((request.id, rows, status, complete));
            slots.failed = None;
            drop(slots);
            if let Some(shared) = self.shared.upgrade() {
                (shared.wake)();
            }
        }
    }

    fn failed(&self, request: &Request, message: String) {
        if self.accepts(request) {
            self.failed_request.store(request.id, Ordering::Release);
            let mut slots = lock(&self.slots);
            slots.failed = Some((request.id, message));
            drop(slots);
            if let Some(shared) = self.shared.upgrade() {
                (shared.wake)();
            }
        }
    }

    fn selection_result(
        &self,
        request: &SelectionRequest,
        result: Result<Vec<PathBuf>, SelectionFailure>,
    ) {
        if self.closed.load(Ordering::Acquire)
            || request.cancel.is_cancelled()
            || self.selection_sequence.load(Ordering::Acquire) != request.id
        {
            return;
        }
        lock(&self.slots).selection = Some((request.id, result));
        let mut commands = lock(&self.commands);
        if commands
            .selection
            .as_ref()
            .is_some_and(|value| value.id == request.id)
        {
            commands.selection = None;
        }
        drop(commands);
        if let Some(shared) = self.shared.upgrade() {
            (shared.wake)();
        }
    }
}

impl Drop for ClientState {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Shared {
    clients: Mutex<Vec<Weak<ClientState>>>,
    serial: AtomicU64,
    stopped: AtomicBool,
    interrupt: Mutex<Cancellation>,
    changed: Mutex<u64>,
    ready: Condvar,
    wake: Wake,
}

impl Shared {
    fn notify(&self) {
        let mut interrupt = lock(&self.interrupt);
        interrupt.cancel();
        *interrupt = Cancellation::new();
        drop(interrupt);
        let mut changed = lock(&self.changed);
        *changed = changed.wrapping_add(1);
        self.ready.notify_one();
    }
}

pub struct SearchHub {
    shared: Arc<Shared>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SearchHub {
    /// Deterministic request/publication tests without filesystem or worker work.
    #[cfg(test)]
    pub(crate) fn unstarted_for_test() -> Self {
        Self {
            shared: Arc::new(Shared {
                clients: Mutex::new(Vec::new()),
                serial: AtomicU64::new(1),
                stopped: AtomicBool::new(false),
                interrupt: Mutex::new(Cancellation::new()),
                changed: Mutex::new(0),
                ready: Condvar::new(),
                wake: Arc::new(|| {}),
            }),
            worker: None,
        }
    }

    /// Spawns the worker. Creating directories or opening databases happens on that worker.
    pub fn new(cache_base: PathBuf, wake: Wake) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            clients: Mutex::new(Vec::with_capacity(16)),
            serial: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            interrupt: Mutex::new(Cancellation::new()),
            changed: Mutex::new(0),
            ready: Condvar::new(),
            wake,
        });
        let worker = shared.clone();
        // Deliberately detached on drop: cancellation and connection cleanup run off the UI thread.
        let worker = thread::Builder::new()
            .name("foldex-search".into())
            .spawn(move || run(worker, cache_base))?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    pub fn client(&self) -> SearchClient {
        let mut clients = lock(&self.shared.clients);
        clients.retain(|client| client.strong_count() > 0);
        let registered = clients.len() < MAX_CLIENTS;
        let state = Arc::new(ClientState {
            shared: Arc::downgrade(&self.shared),
            registered,
            owners: AtomicUsize::new(1),
            active: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            selection_sequence: AtomicU64::new(0),
            failed_request: AtomicU64::new(0),
            window_ready: AtomicU64::new(0),
            commands: Mutex::new(Commands::default()),
            slots: Mutex::new(Slots::default()),
        });
        if registered {
            clients.push(Arc::downgrade(&state));
        }
        SearchClient { state }
    }

    /// Benchmark/process shutdown only. Ordinary UI destruction stays detached.
    pub fn shutdown(mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        for state in lock(&self.shared.clients).iter().filter_map(Weak::upgrade) {
            state.stop();
        }
        self.shared.notify();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for SearchHub {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        for state in lock(&self.shared.clients).iter().filter_map(Weak::upgrade) {
            state.stop();
        }
        self.shared.notify();
    }
}

pub struct SearchClient {
    state: Arc<ClientState>,
}

impl Clone for SearchClient {
    fn clone(&self) -> Self {
        self.state.owners.fetch_add(1, Ordering::Relaxed);
        Self {
            state: self.state.clone(),
        }
    }
}

impl Drop for SearchClient {
    fn drop(&mut self) {
        if self.state.owners.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.state.stop();
        }
    }
}

impl SearchClient {
    pub(crate) fn window_pending(&self, request: u64) -> bool {
        request != 0
            && self.state.active.load(Ordering::Acquire)
            && !self.state.closed.load(Ordering::Acquire)
            && self.state.sequence.load(Ordering::Acquire) == request
            && self.state.failed_request.load(Ordering::Acquire) != request
            && self.state.window_ready.load(Ordering::Acquire) != request
    }

    pub fn set_active(&self, active: bool) {
        if self.state.closed.load(Ordering::Acquire) {
            return;
        }
        if self.state.active.swap(active, Ordering::AcqRel) == active {
            return;
        }
        let mut commands = lock(&self.state.commands);
        if let Some(selection) = commands.selection.take() {
            selection.cancel.cancel();
        }
        commands.scan_epoch = commands.scan_epoch.wrapping_add(1);
        let epoch = commands.scan_epoch;
        if let Some(request) = &mut commands.latest {
            request.query_cancel.cancel();
            request.scan_cancel.cancel();
            if active {
                request.query_cancel = Cancellation::new();
                request.scan_cancel = Cancellation::new();
                request.scan_epoch = epoch;
                request.refresh = false;
                request.started = Instant::now();
                if let Some(shared) = self.state.shared.upgrade() {
                    request.serial = shared.serial.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.state.failed_request.store(0, Ordering::Release);
        self.state.window_ready.store(0, Ordering::Release);
        drop(commands);
        self.state.notify();
    }

    /// Latest-only: superseding a query interrupts an in-flight SQL count or page request.
    /// Changing only the query/viewport preserves the current background traversal.
    pub fn request(&self, root: PathBuf, query: Query, start: usize, refresh: bool) -> u64 {
        self.request_as_of(root, query, start, refresh, None)
    }

    /// Pin global row identities to a published append-only prefix while selecting or acting.
    pub fn request_as_of(
        &self,
        root: PathBuf,
        query: Query,
        start: usize,
        refresh: bool,
        boundary: Option<IndexStatus>,
    ) -> u64 {
        self.request_kind(root, query, start, refresh, boundary, None)
    }

    /// Launcher matching bypasses the explorer's subsequence prefilter, retaining
    /// only 64 ranks and decoding their paths off the UI thread. `explicit` keeps
    /// the launcher's existing configured-root noise policy.
    pub fn request_launcher(&self, root: PathBuf, text: String, explicit: bool) -> u64 {
        self.request_kind(
            root,
            Query {
                text,
                ..Query::default()
            },
            0,
            false,
            None,
            Some(explicit),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn request_kind(
        &self,
        root: PathBuf,
        query: Query,
        start: usize,
        refresh: bool,
        boundary: Option<IndexStatus>,
        launcher: Option<bool>,
    ) -> u64 {
        let id = self.state.sequence.fetch_add(1, Ordering::AcqRel) + 1;
        if self.state.closed.load(Ordering::Acquire) {
            return id;
        }
        let mut commands = lock(&self.state.commands);
        let old = commands.latest.take();
        if refresh
            || old.as_ref().is_none_or(|old| {
                old.root != root || old.query != query || old.launcher != launcher
            })
        {
            if let Some(selection) = commands.selection.take() {
                selection.cancel.cancel();
            }
        }
        let same_scan = old
            .as_ref()
            .is_some_and(|old| old.root == root && !refresh && !old.scan_cancel.is_cancelled());
        if let Some(old) = &old {
            old.query_cancel.cancel();
        }
        let scan_cancel = if same_scan {
            old.as_ref().unwrap().scan_cancel.clone()
        } else {
            if let Some(old) = &old {
                old.scan_cancel.cancel();
            }
            commands.scan_epoch = commands.scan_epoch.wrapping_add(1);
            Cancellation::new()
        };
        let serial = self
            .state
            .shared
            .upgrade()
            .map_or(0, |shared| shared.serial.fetch_add(1, Ordering::Relaxed));
        let request = Request {
            id,
            serial,
            scan_epoch: commands.scan_epoch,
            root,
            query,
            start,
            refresh,
            boundary,
            launcher,
            scan_cancel,
            query_cancel: Cancellation::new(),
            started: Instant::now(),
        };
        commands.latest = Some(request.clone());
        self.state.failed_request.store(0, Ordering::Release);
        drop(commands);
        if !self.state.registered {
            self.state.failed(
                &request,
                format!("Search client limit ({MAX_CLIENTS}) reached"),
            );
        }
        self.state.notify();
        id
    }

    /// Resolve complete global ranges off the UI thread, or return an explicit bounded-action error.
    pub fn resolve_selection(
        &self,
        query: Query,
        boundary: IndexStatus,
        ranges: Vec<(u32, u32)>,
    ) -> u64 {
        let id = self.state.selection_sequence.fetch_add(1, Ordering::AcqRel) + 1;
        let mut commands = lock(&self.state.commands);
        if let Some(old) = commands.selection.take() {
            old.cancel.cancel();
        }
        let request = SelectionRequest {
            id,
            scan_epoch: commands.scan_epoch,
            query,
            boundary,
            ranges,
            cancel: Cancellation::new(),
        };
        let valid = commands
            .latest
            .as_ref()
            .is_some_and(|latest| latest.query == request.query);
        commands.selection = Some(request.clone());
        drop(commands);
        let error = if !valid || self.state.closed.load(Ordering::Acquire) {
            Some(SelectionFailure::Changed)
        } else {
            validate_ranges(&request.ranges).err()
        };
        if let Some(error) = error {
            self.state.selection_result(&request, Err(error));
        }
        self.state.notify();
        id
    }

    pub fn cancel_selection(&self) {
        self.state.selection_sequence.fetch_add(1, Ordering::AcqRel);
        if let Some(request) = lock(&self.state.commands).selection.take() {
            request.cancel.cancel();
        }
        self.state.notify();
    }

    pub fn cancel(&self) {
        self.state.sequence.fetch_add(1, Ordering::AcqRel);
        let mut commands = lock(&self.state.commands);
        if let Some(request) = commands.latest.take() {
            request.query_cancel.cancel();
            request.scan_cancel.cancel();
        }
        if let Some(request) = commands.selection.take() {
            request.cancel.cancel();
        }
        self.state.selection_sequence.fetch_add(1, Ordering::AcqRel);
        drop(commands);
        self.state.notify();
    }

    pub fn close(&self) {
        self.state.stop();
    }

    pub fn try_recv(&self) -> Option<SearchEvent> {
        let mut slots = lock(&self.state.slots);
        // Invalidated events are dropped before delivery, including events published just before cancellation.
        let current = self.state.sequence.load(Ordering::Acquire);
        if let Some((selection, result)) = slots.selection.take() {
            if selection == self.state.selection_sequence.load(Ordering::Acquire) {
                return Some(SearchEvent::Selection { selection, result });
            }
        }
        if let Some((request, message)) = slots.failed.take() {
            if request == current {
                return Some(SearchEvent::Failed { request, message });
            }
        }
        if let Some(window) = slots.window.take() {
            if window.request == current {
                return Some(SearchEvent::Window(window));
            }
        }
        if let Some((request, rows, status, complete)) = slots.launcher.take() {
            if request == current {
                return Some(SearchEvent::Launcher {
                    request,
                    rows,
                    status,
                    complete,
                });
            }
        }
        if let Some((request, status)) = slots.progress.take() {
            if request == current {
                return Some(SearchEvent::Progress { request, status });
            }
        }
        None
    }
}

enum Backend {
    Legacy(Index),
    Cached(NativeIndex),
    Building(IndexBuilder),
    Complete(NativeIndex),
    Shared(Rc<RefCell<SharedCatalog>>),
}

struct SharedCatalog {
    backend: Option<Backend>,
    stale: bool,
    last_status: IndexStatus,
}

impl Backend {
    fn database_path(&self) -> PathBuf {
        match self {
            Self::Building(builder) => builder.status().database_path.clone(),
            Self::Cached(index) | Self::Complete(index) => index.status().database_path.clone(),
            Self::Legacy(index) => index.status().database_path.clone(),
            Self::Shared(catalog) => {
                let catalog = catalog.borrow();
                catalog.backend.as_ref().map_or_else(
                    || catalog.last_status.database_path.clone(),
                    Backend::database_path,
                )
            }
        }
    }
    fn status(&self) -> IndexStatus {
        match self {
            Self::Building(builder) => builder.status().clone(),
            Self::Cached(index) | Self::Complete(index) => index.status().clone(),
            Self::Legacy(index) => index.status().clone(),
            Self::Shared(catalog) => {
                let catalog = catalog.borrow();
                let mut status = catalog
                    .backend
                    .as_ref()
                    .map_or_else(|| catalog.last_status.clone(), Backend::status);
                status.from_cache |= catalog.stale;
                status
            }
        }
    }
    fn complete(&self) -> bool {
        match self {
            Self::Complete(_) => true,
            Self::Shared(catalog) => {
                let catalog = catalog.borrow();
                !catalog.stale && catalog.backend.as_ref().is_some_and(Backend::complete)
            }
            _ => false,
        }
    }
    fn legacy(&self) -> bool {
        match self {
            Self::Legacy(_) => true,
            Self::Shared(catalog) => catalog
                .borrow()
                .backend
                .as_ref()
                .is_some_and(Backend::legacy),
            _ => false,
        }
    }
    fn open_database(path: &Path) -> foldex_index::Result<Self> {
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("catalog-"))
        {
            NativeIndex::open_database(path).map(Self::Cached)
        } else {
            Index::open_database(path).map(Self::Legacy)
        }
    }
    fn query(
        &mut self,
        spec: &QuerySpec,
        start: u64,
        size: usize,
        cancel: &Cancellation,
        boundary: Option<&IndexStatus>,
    ) -> foldex_index::Result<foldex_index::QueryPage> {
        if let Self::Shared(catalog) = self {
            let mut catalog = catalog.borrow_mut();
            let mut page = catalog
                .backend
                .as_mut()
                .ok_or_else(|| {
                    foldex_index::Error::InvalidIndex("Search scope is unavailable".into())
                })?
                .query(spec, start, size, cancel, boundary)?;
            page.status.from_cache |= catalog.stale;
            return Ok(page);
        }
        match (self, boundary) {
            (Self::Building(builder), Some(boundary)) => {
                builder.query_as_of(spec, boundary, start, size, cancel)
            }
            (Self::Building(builder), None) => builder.query(spec, start, size, cancel),
            (Self::Cached(index) | Self::Complete(index), Some(boundary)) => {
                index.query_as_of(spec, boundary, start, size, cancel)
            }
            (Self::Cached(index) | Self::Complete(index), None) => {
                index.query(spec, start, size, cancel)
            }
            (Self::Legacy(index), Some(boundary)) => {
                index.query_as_of(spec, boundary, start, size, cancel)
            }
            (Self::Legacy(index), None) => index.query(spec, start, size, cancel),
            (Self::Shared(_), _) => unreachable!(),
        }
    }

    fn query_after(
        &mut self,
        spec: &QuerySpec,
        cursor: Option<&foldex_index::PageCursor>,
        size: usize,
        cancel: &Cancellation,
        boundary: Option<&IndexStatus>,
    ) -> foldex_index::Result<foldex_index::QueryPage> {
        if let Self::Shared(catalog) = self {
            let mut catalog = catalog.borrow_mut();
            let mut page = catalog
                .backend
                .as_mut()
                .ok_or_else(|| {
                    foldex_index::Error::InvalidIndex("Search scope is unavailable".into())
                })?
                .query_after(spec, cursor, size, cancel, boundary)?;
            page.status.from_cache |= catalog.stale;
            return Ok(page);
        }
        match (self, boundary) {
            (Self::Building(builder), Some(boundary)) => {
                builder.query_after_as_of(spec, boundary, cursor, size, cancel)
            }
            (Self::Building(builder), None) => builder.query_after(spec, cursor, size, cancel),
            (Self::Cached(index) | Self::Complete(index), Some(boundary)) => {
                index.query_after_as_of(spec, boundary, cursor, size, cancel)
            }
            (Self::Cached(index) | Self::Complete(index), None) => {
                index.query_after(spec, cursor, size, cancel)
            }
            (Self::Legacy(index), Some(boundary)) => {
                index.query_after_as_of(spec, boundary, cursor, size, cancel)
            }
            (Self::Legacy(index), None) => index.query_after(spec, cursor, size, cancel),
            (Self::Shared(_), _) => unreachable!(),
        }
    }

    fn candidate_batch(
        &mut self,
        lower: u64,
        cancel: &Cancellation,
        score: &mut impl FnMut(&str, &str, bool, bool) -> Option<i32>,
    ) -> foldex_index::Result<Option<foldex_index::catalog::CandidateBatch>> {
        match self {
            Self::Building(builder) => builder.candidate_batch(lower, cancel, score).map(Some),
            Self::Cached(index) | Self::Complete(index) => {
                index.candidate_batch(lower, cancel, score).map(Some)
            }
            // A legacy cache cannot supply unfiltered native names. The normal
            // worker transition builds a native catalog immediately afterwards.
            Self::Legacy(_) => Ok(None),
            Self::Shared(catalog) => {
                let mut catalog = catalog.borrow_mut();
                let mut batch = catalog
                    .backend
                    .as_mut()
                    .ok_or_else(|| {
                        foldex_index::Error::InvalidIndex("Search scope is unavailable".into())
                    })?
                    .candidate_batch(lower, cancel, score)?;
                if let Some(batch) = &mut batch {
                    batch.status.from_cache |= catalog.stale;
                }
                Ok(batch)
            }
        }
    }

    fn candidate_rows(
        &self,
        hits: &[foldex_index::catalog::Hit],
        cancel: &Cancellation,
    ) -> foldex_index::Result<Vec<IndexRow>> {
        match self {
            Self::Building(builder) => builder.candidate_rows(hits, cancel),
            Self::Cached(index) | Self::Complete(index) => index.candidate_rows(hits, cancel),
            Self::Legacy(_) => Ok(Vec::new()),
            Self::Shared(catalog) => catalog
                .borrow()
                .backend
                .as_ref()
                .ok_or_else(|| {
                    foldex_index::Error::InvalidIndex("Search scope is unavailable".into())
                })?
                .candidate_rows(hits, cancel),
        }
    }
}

struct Context {
    client: Weak<ClientState>,
    request: Request,
    cache_dir: PathBuf,
    backend: Option<Backend>,
    query_dirty: bool,
    count_dirty: bool,
    window: Option<Arc<Snapshot>>,
    window_status: Option<IndexStatus>,
    window_has_more: bool,
    counts: Option<CountCache>,
    last_window: Instant,
    last_progress: Instant,
    selection: Option<SelectionWork>,
    launcher: Option<LauncherWork>,
}

struct CountCache {
    query: Query,
    status: IndexStatus,
    kind_counts: (usize, usize),
}

impl CountCache {
    fn matches(&self, query: &Query, status: &IndexStatus) -> bool {
        // Counts precede kind toggles and do not depend on order or viewport.
        same_result_prefix(&self.status, status)
            && self.query.text == query.text
            && self.query.match_mode == query.match_mode
            && self.query.show_hidden == query.show_hidden
            && self.query.type_filter == query.type_filter
    }
}

fn same_result_prefix(a: &IndexStatus, b: &IndexStatus) -> bool {
    a.generation == b.generation
        && a.revision == b.revision
        && a.entries == b.entries
        && a.scope == b.scope
        && a.database_path == b.database_path
}

struct LauncherWork {
    matcher: LauncherMatcher,
    generation: String,
    prefix: u64,
    hits: Vec<foldex_index::catalog::Hit>,
}

impl LauncherWork {
    fn complete(&self, status: &IndexStatus) -> bool {
        self.prefix == status.entries
            && !status.from_cache
            && matches!(
                status.state,
                foldex_index::BuildState::Complete | foldex_index::BuildState::Partial
            )
    }

    fn merge(&mut self, batch: foldex_index::catalog::CandidateBatch) {
        self.hits.extend(batch.hits);
        self.hits
            .sort_unstable_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
        self.hits.truncate(64);
        self.prefix = batch.scanned_to;
    }
}

struct SelectionWork {
    request: SelectionRequest,
    range: usize,
    offset: u64,
    paths: Vec<PathBuf>,
    bytes: usize,
    cursor: Option<foldex_index::PageCursor>,
}

impl Drop for Context {
    fn drop(&mut self) {
        if let Some(Backend::Building(builder)) = self.backend.take() {
            // This destructor runs only on the search worker; preserve honest cancellation state.
            let _ = builder.finish_cancelled();
        }
    }
}

impl Context {
    fn new(
        client: &Arc<ClientState>,
        request: Request,
        cache_base: &Path,
    ) -> foldex_index::Result<Self> {
        let source_root = request
            .boundary
            .as_ref()
            .map_or(&request.root, |boundary| &boundary.scope);
        if request.boundary.is_some()
            && !crate::bookmarks::path_eq(source_root, &std::path::absolute(&request.root)?)
        {
            return Err(foldex_index::Error::InvalidCursor);
        }
        let cache_dir = Index::cache_dir_for(cache_base, source_root)?;
        let pinned = if let Some(boundary) = &request.boundary {
            let root = std::fs::canonicalize(&cache_dir)?;
            let path = std::fs::canonicalize(&boundary.database_path)?;
            if path.parent() != Some(root.as_path()) {
                return Err(foldex_index::Error::InvalidCursor);
            }
            let index = Backend::open_database(&path)?;
            if index.status().generation != boundary.generation
                || index.status().scope != boundary.scope
            {
                return Err(foldex_index::Error::InvalidCursor);
            }
            Some(index)
        } else {
            None
        };
        let cached = if pinned.is_some() {
            pinned
        } else if request.refresh {
            None
        } else {
            NativeIndex::open_scope(&cache_dir, &request.root)
                .map(Backend::Cached)
                .or_else(|_| Index::open_scope(&cache_dir, &request.root).map(Backend::Legacy))
                .ok()
        };
        let backend = if let Some(index) = cached {
            index
        } else {
            Backend::Building(new_builder(&cache_dir, cache_base, &request)?)
        };
        Ok(Self::with_backend(client, request, cache_dir, backend))
    }

    fn live(
        client: &Arc<ClientState>,
        request: Request,
        cache_base: &Path,
        scopes: &mut scopes::Scopes,
    ) -> foldex_index::Result<Self> {
        if request.boundary.is_some() {
            return Self::new(client, request, cache_base);
        }
        let (cache_dir, backend) = scopes.backend(&request)?;
        Ok(Self::with_backend(client, request, cache_dir, backend))
    }

    fn with_backend(
        client: &Arc<ClientState>,
        request: Request,
        cache_dir: PathBuf,
        backend: Backend,
    ) -> Self {
        Self {
            client: Arc::downgrade(client),
            request,
            cache_dir,
            backend: Some(backend),
            query_dirty: true,
            count_dirty: false,
            window: None,
            window_status: None,
            window_has_more: false,
            counts: None,
            last_window: Instant::now(),
            last_progress: Instant::now(),
            selection: None,
            launcher: None,
        }
    }

    fn replace_request(&mut self, request: Request) {
        if self.request.id != request.id {
            // Only a finished, unpinned native window can be replayed without
            // asking the backend to validate a new prefix or selection boundary.
            let replay = !self.query_dirty
                && !self.count_dirty
                && self.request.launcher.is_none()
                && request.launcher.is_none()
                && self.request.boundary.is_none()
                && request.boundary.is_none()
                && !request.refresh
                && self.request.scan_epoch == request.scan_epoch
                && self.request.root == request.root
                && self.request.query == request.query
                && self.request.start == request.start
                && self.backend.as_ref().is_some_and(Backend::complete)
                && self.window_status.as_ref().is_some_and(|status| {
                    same_result_prefix(status, &self.backend.as_ref().unwrap().status())
                });
            let counts = replay.then(|| self.cached_counts()).flatten();
            if self.request.query.text != request.query.text
                || self.request.launcher != request.launcher
            {
                self.launcher = None;
            }
            self.request = request;
            self.query_dirty = counts.is_none();
            self.count_dirty = false;
            if let Some(counts) = counts {
                self.publish_counts(self.window_status.as_ref().unwrap().clone(), Some(counts));
                if let Some(client) = self.client.upgrade() {
                    client
                        .window_ready
                        .store(self.request.id, Ordering::Release);
                }
            }
        }
    }

    fn step(
        &mut self,
        cache_base: &Path,
        cleanup_allowed: bool,
        work_cancel: &Cancellation,
        priority_cancel: &Cancellation,
    ) -> foldex_index::Result<bool> {
        if work_cancel.is_cancelled() {
            return Ok(false);
        }
        if self.query_dirty {
            self.fetch_window(work_cancel)?;
            return Ok(true);
        }
        if self.resolve_selection_step(priority_cancel)? {
            return Ok(true);
        }
        if self.count_dirty {
            self.fetch_count(work_cancel)?;
            return Ok(true);
        }
        if matches!(self.backend, Some(Backend::Shared(_))) {
            return Ok(false);
        }
        if matches!(self.backend, Some(Backend::Cached(_) | Backend::Legacy(_)))
            && self.request.boundary.is_none()
        {
            // Cached results already painted. Close that connection before constructing a fresh generation.
            self.backend = None;
            self.backend = Some(Backend::Building(new_builder(
                &self.cache_dir,
                cache_base,
                &self.request,
            )?));
            if cleanup_allowed {
                cleanup_cache(&self.cache_dir, &self.request.root, &[]);
            }
            self.last_window = Instant::now()
                .checked_sub(REFRESH_INTERVAL)
                .unwrap_or_else(Instant::now);
            return Ok(true);
        }
        let Some(Backend::Building(builder)) = self.backend.as_mut() else {
            return Ok(false);
        };
        let progress =
            builder.scan_step(PAGE_ROWS, &self.request.scan_cancel, &thread::yield_now)?;
        // Finishing below publishes the terminal status once, after it is saved.
        if !progress.done && self.last_progress.elapsed() >= PROGRESS_INTERVAL {
            if let Some(client) = self.client.upgrade() {
                client.progress(&self.request, progress.status.clone());
            }
            self.last_progress = Instant::now();
        }
        let refresh = progress.done
            || self.last_window.elapsed() >= REFRESH_INTERVAL
            || self
                .window
                .as_ref()
                .is_some_and(|window| window.entries.is_empty() && progress.status.entries > 0);
        if progress.done {
            let Some(Backend::Building(builder)) = self.backend.take() else {
                unreachable!()
            };
            let index = builder.finish(!self.request.scan_cancel.is_cancelled())?;
            if let Some(client) = self.client.upgrade() {
                client.progress(&self.request, index.status().clone());
            }
            self.backend = Some(Backend::Complete(index));
            if cleanup_allowed {
                cleanup_cache(&self.cache_dir, &self.request.root, &[]);
            }
        }
        if refresh && self.request.boundary.is_none() {
            self.query_dirty = true;
            self.count_dirty = false;
        }
        Ok(true)
    }

    fn fetch_window(&mut self, cancel: &Cancellation) -> foldex_index::Result<()> {
        if let Some(explicit) = self.request.launcher {
            return self.fetch_launcher(explicit, cancel);
        }
        let backend = self.backend.as_mut().expect("live backend");
        // Native pages already compute exact counts while choosing their head.
        // Carry that total on the first publication instead of temporarily
        // advertising only the materialized window and counting again later.
        let native = !backend.legacy();
        let spec = query_spec(&self.request.query, native);
        let first = backend.query(
            &spec,
            self.request.start as u64,
            PAGE_ROWS,
            cancel,
            self.request.boundary.as_ref(),
        )?;
        let mut rows = first.rows;
        let mut cursor = first.next;
        let mut more = first.has_more;
        let status = first.status;
        let kind_counts = self
            .counts
            .as_ref()
            .filter(|cache| native && cache.matches(&self.request.query, &status))
            .map(|cache| cache.kind_counts)
            .or_else(|| {
                (native && self.request.query.kind == foldex_core::KindFilter::All)
                    .then_some(first.kind_counts)
                    .flatten()
                    .and_then(|(folders, files)| Some((to_usize(folders)?, to_usize(files)?)))
            });
        let total = first
            .total
            .and_then(to_usize)
            .or_else(|| kind_counts.map(|counts| filtered_total(&self.request.query, counts)));
        let first_snapshot = snapshot(
            &status.scope,
            rows.iter(),
            &status,
            self.request.started.elapsed(),
        );
        if let Some(client) = self.client.upgrade() {
            client.window(
                &self.request,
                Arc::new(SearchWindow {
                    request: self.request.id,
                    start: self.request.start,
                    snapshot: first_snapshot.clone(),
                    total,
                    kind_counts,
                    has_more: more,
                    status: status.clone(),
                    elapsed: self.request.started.elapsed(),
                }),
            );
        }
        let mut result = first_snapshot;
        for _ in 1..WINDOW_PAGES {
            if !more || cursor.is_none() {
                break;
            }
            let page = backend.query_after(
                &spec,
                cursor.as_ref(),
                PAGE_ROWS,
                cancel,
                self.request.boundary.as_ref(),
            )?;
            rows.extend(page.rows);
            cursor = page.next;
            more = page.has_more;
        }
        if rows.len() != result.entries.len() {
            result = snapshot(
                &status.scope,
                rows.iter(),
                &status,
                self.request.started.elapsed(),
            );
            if let Some(client) = self.client.upgrade() {
                client.window(
                    &self.request,
                    Arc::new(SearchWindow {
                        request: self.request.id,
                        start: self.request.start,
                        snapshot: result.clone(),
                        total,
                        kind_counts,
                        has_more: more,
                        status: status.clone(),
                        elapsed: self.request.started.elapsed(),
                    }),
                );
            }
        }
        self.window = Some(result);
        self.window_status = Some(status.clone());
        self.window_has_more = more;
        self.query_dirty = false;
        self.count_dirty = kind_counts.is_none();
        if let Some(kind_counts) = kind_counts
            && self
                .counts
                .as_ref()
                .is_none_or(|cache| !cache.matches(&self.request.query, &status))
        {
            // Reused pre-toggle counts retain their source query and allocation.
            // A viewport or kind-only request does not create a new count result.
            self.counts = Some(CountCache {
                query: self.request.query.clone(),
                status,
                kind_counts,
            });
        }
        if let Some(client) = self.client.upgrade() {
            client
                .window_ready
                .store(self.request.id, Ordering::Release);
        }
        self.last_window = Instant::now();
        Ok(())
    }

    fn fetch_launcher(
        &mut self,
        explicit: bool,
        cancel: &Cancellation,
    ) -> foldex_index::Result<()> {
        let backend = self.backend.as_mut().expect("live backend");
        let generation = &backend.status().generation;
        let work = self.launcher.get_or_insert_with(|| LauncherWork {
            matcher: LauncherMatcher::new(&self.request.query.text),
            generation: generation.clone(),
            prefix: 0,
            hits: Vec::with_capacity(128),
        });
        if work.generation != *generation {
            work.generation = generation.clone();
            work.prefix = 0;
            work.hits.clear();
        }
        let root = &self.request.root;
        let matcher = &mut work.matcher;
        let mut scorer = |name: &str, parent: &str, is_dir, hidden| {
            if hidden {
                return None;
            }
            // An absolute configured scope can contain install/build trees.
            // Join only after the name matched; rejected names allocate nothing.
            let score = providers::title_score(matcher, name)?;
            providers::candidate_score_with_rank(
                name,
                &root.join(parent),
                is_dir,
                explicit,
                matcher.query(),
                score,
            )
        };
        let batch = backend.candidate_batch(work.prefix, cancel, &mut scorer)?;
        let Some(batch) = batch else {
            if let Some(client) = self.client.upgrade() {
                client.launcher(&self.request, Vec::new(), backend.status().clone(), false);
            }
            self.query_dirty = false;
            self.count_dirty = false;
            return Ok(());
        };
        let status = batch.status.clone();
        work.merge(batch);
        let caught_up = work.prefix == status.entries;
        // Appended scan rows are ranked once. No old-prefix rescan at each
        // progress update; finished catalogs advance in bounded worker turns.
        self.query_dirty = !caught_up;
        self.count_dirty = false;
        if caught_up || self.last_window.elapsed() >= REFRESH_INTERVAL {
            let rows = backend.candidate_rows(&work.hits, cancel)?;
            let rows = rows
                .into_iter()
                .zip(&work.hits)
                .map(|(row, hit)| LauncherFile {
                    path: row.path,
                    is_dir: row.is_dir,
                    score: hit.score.unwrap_or_default(),
                })
                .collect();
            if cancel.is_cancelled() {
                return Err(foldex_index::Error::Cancelled);
            }
            if let Some(client) = self.client.upgrade() {
                client.launcher(&self.request, rows, status.clone(), work.complete(&status));
            }
            self.last_window = Instant::now();
        }
        Ok(())
    }

    fn fetch_count(&mut self, cancel: &Cancellation) -> foldex_index::Result<()> {
        if let Some(counts) = self.cached_counts() {
            self.publish_counts(self.window_status.as_ref().unwrap().clone(), Some(counts));
            self.count_dirty = false;
            return Ok(());
        }
        let mut spec = query_spec(&self.request.query, true);
        // A hidden group must retain its count so it can be shown again. The
        // backend already counts both groups in the same cancellable query.
        spec.kind = KindFilter::All;
        let page = self.backend.as_mut().expect("live backend").query(
            &spec,
            0,
            0,
            cancel,
            self.request.boundary.as_ref(),
        )?;
        let kind_counts = page
            .kind_counts
            .and_then(|(folders, files)| Some((to_usize(folders)?, to_usize(files)?)));
        if self.publish_counts(page.status.clone(), kind_counts) {
            if let Some(kind_counts) = kind_counts {
                self.counts = Some(CountCache {
                    query: self.request.query.clone(),
                    status: page.status,
                    kind_counts,
                });
            }
        } else {
            self.query_dirty = true;
        }
        self.count_dirty = false;
        Ok(())
    }

    fn cached_counts(&self) -> Option<(usize, usize)> {
        // Legacy readers can observe another writer between page/count calls.
        // Keep their existing validation query instead of relying on local state.
        if self.backend.as_ref().is_some_and(Backend::legacy) {
            return None;
        }
        let status = self.window_status.as_ref()?;
        let cache = self.counts.as_ref()?;
        cache
            .matches(&self.request.query, status)
            .then_some(cache.kind_counts)
    }

    fn publish_counts(&self, status: IndexStatus, kind_counts: Option<(usize, usize)>) -> bool {
        if let (Some(window), Some(window_status)) = (&self.window, &self.window_status) {
            if same_result_prefix(window_status, &status) {
                if let Some(client) = self.client.upgrade() {
                    let total =
                        kind_counts.map(|counts| filtered_total(&self.request.query, counts));
                    let more = total.map_or(self.window_has_more, |total| {
                        self.request.start.saturating_add(window.entries.len()) < total
                    });
                    client.window(
                        &self.request,
                        Arc::new(SearchWindow {
                            request: self.request.id,
                            start: self.request.start,
                            snapshot: window.clone(),
                            total,
                            kind_counts,
                            has_more: more,
                            status,
                            elapsed: self.request.started.elapsed(),
                        }),
                    );
                }
                return true;
            }
        }
        false
    }

    fn resolve_selection_step(&mut self, priority: &Cancellation) -> foldex_index::Result<bool> {
        let Some(client) = self.client.upgrade() else {
            self.selection = None;
            return Ok(false);
        };
        let pending = lock(&client.commands).selection.clone();
        let Some(pending) = pending else {
            self.selection = None;
            return Ok(false);
        };
        if pending.cancel.is_cancelled() {
            self.selection = None;
            return Ok(false);
        }
        if pending.scan_epoch != self.request.scan_epoch || pending.query != self.request.query {
            client.selection_result(&pending, Err(SelectionFailure::Changed));
            self.selection = None;
            return Ok(true);
        }
        if self
            .selection
            .as_ref()
            .is_none_or(|work| work.request.id != pending.id)
        {
            let count = match validate_ranges(&pending.ranges) {
                Ok(count) => count,
                Err(error) => {
                    client.selection_result(&pending, Err(error));
                    return Ok(true);
                }
            };
            let offset = pending.ranges.first().map_or(0, |range| range.0 as u64);
            self.selection = Some(SelectionWork {
                request: pending,
                range: 0,
                offset,
                paths: Vec::with_capacity(count),
                bytes: 0,
                cursor: None,
            });
        }
        let work = self.selection.as_mut().unwrap();
        if work.range == work.request.ranges.len() {
            let work = self.selection.take().unwrap();
            client.selection_result(&work.request, Ok(work.paths));
            return Ok(true);
        }
        let last = work.request.ranges[work.range].1 as u64;
        let size = (last - work.offset + 1).min(PAGE_ROWS as u64) as usize;
        let cancel = work.request.cancel.linked(priority);
        let spec = query_spec(&work.request.query, false);
        let backend = self.backend.as_mut().expect("live backend");
        let page = if work.cursor.is_some() {
            backend.query_after(
                &spec,
                work.cursor.as_ref(),
                size,
                &cancel,
                Some(&work.request.boundary),
            )
        } else {
            backend.query(
                &spec,
                work.offset,
                size,
                &cancel,
                Some(&work.request.boundary),
            )
        };
        let page = match page {
            Ok(page) => page,
            Err(foldex_index::Error::Cancelled) => return Err(foldex_index::Error::Cancelled),
            Err(error) => {
                let work = self.selection.take().unwrap();
                let reason = if matches!(error, foldex_index::Error::InvalidCursor) {
                    SelectionFailure::Changed
                } else {
                    SelectionFailure::Failed(error.to_string())
                };
                client.selection_result(&work.request, Err(reason));
                return Ok(true);
            }
        };
        if page.rows.len() != size {
            let work = self.selection.take().unwrap();
            client.selection_result(&work.request, Err(SelectionFailure::InvalidRange));
            return Ok(true);
        }
        let bytes = page
            .rows
            .iter()
            .map(|row| row.path.capacity() + std::mem::size_of::<PathBuf>())
            .sum::<usize>();
        if work.bytes.saturating_add(bytes) > MAX_ACTION_BYTES {
            let work = self.selection.take().unwrap();
            client.selection_result(&work.request, Err(SelectionFailure::MemoryBudget));
            return Ok(true);
        }
        work.bytes += bytes;
        work.offset += page.rows.len() as u64;
        work.paths.extend(page.rows.into_iter().map(|row| row.path));
        work.cursor = page.next;
        if work.offset > last {
            work.range += 1;
            work.cursor = None;
            if let Some(range) = work.request.ranges.get(work.range) {
                work.offset = range.0 as u64;
            }
        }
        if work.range == work.request.ranges.len() {
            let work = self.selection.take().unwrap();
            client.selection_result(&work.request, Ok(work.paths));
        }
        Ok(true)
    }
}

fn validate_ranges(ranges: &[(u32, u32)]) -> Result<usize, SelectionFailure> {
    let mut count = 0u64;
    let mut previous = None;
    for &(first, last) in ranges {
        if first > last || previous.is_some_and(|end| first <= end) {
            return Err(SelectionFailure::InvalidRange);
        }
        count = count.saturating_add(last as u64 - first as u64 + 1);
        previous = Some(last);
    }
    if count > MAX_ACTION_PATHS as u64 {
        return Err(SelectionFailure::TooMany { selected: count });
    }
    Ok(count as usize)
}

fn new_builder(
    cache_dir: &Path,
    cache_base: &Path,
    request: &Request,
) -> foldex_index::Result<IndexBuilder> {
    IndexBuilder::create_with_options(
        cache_dir,
        &request.root,
        &BuildOptions {
            excluded_root: Some(cache_base.to_path_buf()),
        },
        &request.scan_cancel,
    )
}

fn cleanup_cache(cache_dir: &Path, scope: &Path, active: &[PathBuf]) {
    let native: Vec<_> = active
        .iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("catalog-"))
        })
        .cloned()
        .collect();
    let legacy: Vec<_> = active
        .iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("index-"))
        })
        .cloned()
        .collect();
    let _ = NativeIndex::cleanup_owned_retaining(cache_dir, scope, &native);
    let _ = Index::cleanup_owned_retaining(cache_dir, scope, &legacy);
}

fn run(shared: Arc<Shared>, cache_base: PathBuf) {
    #[cfg(windows)]
    {
        // SAFETY: this lowers only the dedicated worker's scheduling priority. No handle is retained.
        unsafe {
            windows_sys::Win32::System::Threading::SetThreadPriority(
                windows_sys::Win32::System::Threading::GetCurrentThread(),
                windows_sys::Win32::System::Threading::THREAD_PRIORITY_BELOW_NORMAL,
            );
        }
    }
    let mut contexts = Vec::<Context>::with_capacity(8);
    let mut scopes = scopes::Scopes::new(shared.clone(), cache_base.clone());
    let mut cleanup = Vec::<(PathBuf, PathBuf)>::with_capacity(8);
    let mut reconciled = None;
    let mut next_context = 0usize;
    loop {
        if shared.stopped.load(Ordering::Acquire) {
            break;
        }
        let observed = *lock(&shared.changed);
        // Scan turns do not change the client registry or request priority.
        // Reconcile once per notification, rather than cloning/sorting every
        // active client's query after each bounded backend step.
        if reconciled != Some(observed) {
            reconciled = Some(observed);
            let mut inputs: Vec<_> = {
                let mut registry = lock(&shared.clients);
                registry.retain(|client| client.strong_count() > 0);
                registry
                    .iter()
                    .filter_map(Weak::upgrade)
                    .filter_map(|client| client.input().map(|request| (client, request)))
                    .collect()
            };
            inputs.sort_unstable_by(|a, b| b.1.serial.cmp(&a.1.serial));
            for (client, request) in inputs.iter().skip(MAX_SUBSCRIPTIONS) {
                client.failed(
                    request,
                    format!(
                        "At most {MAX_SUBSCRIPTIONS} visible search subscriptions can be active"
                    ),
                );
            }
            inputs.truncate(MAX_SUBSCRIPTIONS);
            let mut at = 0;
            while at < contexts.len() {
                let keep = contexts[at].client.upgrade().is_some_and(|client| {
                    inputs.iter().any(|(active, request)| {
                        Arc::ptr_eq(active, &client)
                            && request.scan_epoch == contexts[at].request.scan_epoch
                            && request.root == contexts[at].request.root
                    })
                });
                if keep {
                    at += 1;
                } else {
                    let context = contexts.remove(at);
                    let identity = (context.cache_dir.clone(), context.request.root.clone());
                    drop(context);
                    if cleanup.len() < MAX_CLIENTS && !cleanup.contains(&identity) {
                        cleanup.push(identity);
                    }
                }
            }
            for (client, request) in &inputs {
                if let Some(context) = contexts.iter_mut().find(|context| {
                    context
                        .client
                        .upgrade()
                        .is_some_and(|state| Arc::ptr_eq(&state, client))
                }) {
                    let shared_backend = matches!(context.backend, Some(Backend::Shared(_)));
                    if shared_backend == request.boundary.is_some() {
                        match Context::live(client, request.clone(), &cache_base, &mut scopes) {
                            Ok(replacement) => *context = replacement,
                            Err(error) => {
                                client.failed(request, error.to_string());
                                reconciled = None;
                            }
                        }
                    } else {
                        context.replace_request(request.clone());
                    }
                } else {
                    match std::fs::create_dir_all(&cache_base)
                        .map_err(foldex_index::Error::from)
                        .and_then(|_| {
                            Context::live(client, request.clone(), &cache_base, &mut scopes)
                        }) {
                        Ok(context) => {
                            let identity =
                                (context.cache_dir.clone(), context.request.root.clone());
                            if cleanup.len() < MAX_CLIENTS && !cleanup.contains(&identity) {
                                cleanup.push(identity);
                            }
                            contexts.push(context);
                        }
                        Err(foldex_index::Error::Cancelled) => {}
                        Err(error) => {
                            client.failed(request, error.to_string());
                            // Failure frees a connection for an older waiting client.
                            reconciled = None;
                        }
                    }
                }
            }
            contexts.sort_unstable_by(|a, b| b.request.serial.cmp(&a.request.serial));
        }
        let mut did_work = false;
        let mut turns = 0;
        for offset in 0..contexts.len() {
            let index = (next_context + offset) % contexts.len();
            let cleanup_allowed = contexts
                .iter()
                .filter(|context| context.cache_dir == contexts[index].cache_dir)
                .count()
                == 1;
            let priority_cancel = lock(&shared.interrupt).clone();
            let work_cancel = contexts[index]
                .request
                .query_cancel
                .linked(&priority_cancel);
            match contexts[index].step(&cache_base, cleanup_allowed, &work_cancel, &priority_cancel)
            {
                Ok(work) => {
                    if work {
                        turns += 1;
                    }
                    did_work |= work;
                }
                Err(foldex_index::Error::Cancelled) => {}
                Err(error) => {
                    did_work = true;
                    reconciled = None;
                    if let Some(client) = contexts[index].client.upgrade() {
                        client.failed(&contexts[index].request, error.to_string());
                    }
                }
            }
            if turns == QUERY_TURNS {
                next_context = (index + 1) % contexts.len();
                break;
            }
        }
        let active_roots = contexts
            .iter()
            .filter_map(|context| {
                context
                    .client
                    .upgrade()
                    .and_then(|client| client.input())
                    .map(|request| request.root)
            })
            .collect::<Vec<_>>();
        for update in scopes.advance(&active_roots) {
            did_work = true;
            if update.refetch
                && let Ok(status) = &update.status
                && matches!(
                    status.state,
                    foldex_index::BuildState::Complete | foldex_index::BuildState::Partial
                )
                && let Some(directory) = status.database_path.parent()
            {
                let identity = (directory.to_owned(), status.scope.clone());
                if !cleanup.contains(&identity) {
                    cleanup.push(identity);
                }
            }
            for context in &mut contexts {
                let matches = matches!(&context.backend,Some(Backend::Shared(catalog)) if Rc::ptr_eq(catalog,&update.catalog));
                if !matches {
                    continue;
                }
                match &update.status {
                    Ok(status) => {
                        if update.refetch {
                            context.query_dirty = true;
                            context.count_dirty = false;
                        }
                        if let Some(client) = context.client.upgrade() {
                            client.progress(&context.request, status.clone());
                        }
                    }
                    Err(error) => {
                        if let Some(client) = context.client.upgrade() {
                            client.failed(&context.request, error.to_string());
                        }
                        reconciled = None;
                    }
                }
            }
        }
        did_work |= scopes.busy();
        for (cache_dir, scope) in cleanup.drain(..) {
            let mut active: Vec<_> = contexts
                .iter()
                .filter(|context| context.cache_dir == cache_dir)
                .filter_map(|context| {
                    context
                        .backend
                        .as_ref()
                        .map(|backend| backend.database_path().to_path_buf())
                })
                .collect();
            active.extend(scopes.active_paths(&cache_dir));
            active.sort();
            active.dedup();
            cleanup_cache(&cache_dir, &scope, &active);
        }
        if !did_work {
            let changed = lock(&shared.changed);
            if *changed == observed
                && reconciled == Some(observed)
                && !shared.stopped.load(Ordering::Acquire)
            {
                if let Some(deadline) = scopes.deadline() {
                    drop(
                        shared
                            .ready
                            .wait_timeout(
                                changed,
                                deadline.saturating_duration_since(Instant::now()),
                            )
                            .unwrap_or_else(|poison| poison.into_inner()),
                    );
                } else {
                    drop(
                        shared
                            .ready
                            .wait(changed)
                            .unwrap_or_else(|poison| poison.into_inner()),
                    );
                }
            }
        }
    }
    drop(scopes);
    for context in contexts {
        let identity = (context.cache_dir.clone(), context.request.root.clone());
        drop(context);
        if !cleanup.contains(&identity) {
            cleanup.push(identity);
        }
    }
    for (cache_dir, scope) in cleanup {
        cleanup_cache(&cache_dir, &scope, &[]);
    }
}

fn filtered_total(query: &Query, (folders, files): (usize, usize)) -> usize {
    (if query.kind.includes(true) {
        folders
    } else {
        0
    })
    .saturating_add(if query.kind.includes(false) { files } else { 0 })
}

fn query_spec(query: &Query, include_total: bool) -> QuerySpec {
    QuerySpec {
        text: query.text.clone(),
        match_mode: match query.match_mode {
            foldex_core::MatchMode::Fuzzy => foldex_index::MatchMode::Fuzzy,
            foldex_core::MatchMode::Substring => foldex_index::MatchMode::Substring,
        },
        descending: query.descending,
        show_hidden: query.show_hidden,
        type_filter: query
            .type_filter
            .as_ref()
            .map(|filter| foldex_index::TypeFilter {
                include_folders: filter.include_folders,
                exclude: filter.exclude,
                extensions: filter.extensions.clone(),
            }),
        include_total,
        sort: match query.sort {
            foldex_core::SortColumn::Name => SortColumn::Name,
            foldex_core::SortColumn::Size => SortColumn::Size,
            foldex_core::SortColumn::Modified => SortColumn::Modified,
            foldex_core::SortColumn::Created | foldex_core::SortColumn::Accessed => {
                SortColumn::Name
            }
            foldex_core::SortColumn::Kind => SortColumn::Kind,
        },
        kind: match query.kind {
            foldex_core::KindFilter::All => KindFilter::All,
            foldex_core::KindFilter::Files => KindFilter::Files,
            foldex_core::KindFilter::Folders => KindFilter::Folders,
            foldex_core::KindFilter::None => KindFilter::None,
        },
    }
}

fn snapshot<'a>(
    root: &Path,
    rows: impl Iterator<Item = &'a IndexRow>,
    status: &IndexStatus,
    elapsed: Duration,
) -> Arc<Snapshot> {
    let entries = rows
        .map(|row| {
            let mut entry = Entry::from_name(
                row.raw_name.clone(),
                row.is_dir,
                row.size,
                row.modified_secs,
            );
            entry.relative_path = Some(row.relative_path.clone());
            entry.hidden = row.hidden;
            entry.symlink = row.symlink;
            entry
        })
        .collect();
    let mut snapshot = Snapshot::from_entries(root.to_path_buf(), entries);
    snapshot.default_order = Arc::from(
        (0..snapshot.entries.len())
            .map(|index| index as u32)
            .collect::<Vec<_>>(),
    );
    snapshot.recursive = true;
    snapshot.complete = status.complete && !status.from_cache;
    snapshot.skipped = status.skipped;
    snapshot.excluded_links = status.excluded_links;
    snapshot.elapsed = elapsed;
    Arc::new(snapshot)
}

fn to_usize(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::OnceLock,
        time::{SystemTime, UNIX_EPOCH},
    };
    static BASE: OnceLock<PathBuf> = OnceLock::new();
    static FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        path: PathBuf,
        base: PathBuf,
    }
    impl Fixture {
        fn new(entries: usize) -> Self {
            let base = BASE
                .get_or_init(|| {
                    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/search-fixtures");
                    fs::create_dir_all(&path).unwrap();
                    path
                })
                .clone();
            let path = base.join(format!(
                "owned-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            let scope = path.join("scope");
            fs::create_dir(&scope).unwrap();
            for index in 0..entries {
                fs::write(scope.join(format!("item{index}.txt")), []).unwrap();
            }
            Self { path, base }
        }
        fn scope(&self) -> PathBuf {
            self.path.join("scope")
        }
        fn cache(&self) -> PathBuf {
            self.path.join("cache")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let (Ok(path), Ok(base)) = (self.path.canonicalize(), self.base.canonicalize()) {
                if path.starts_with(&base) && path != base {
                    // Worker shutdown is deliberately detached in production; allow its final
                    // connection drop to finish before removing this verified owned fixture.
                    for _ in 0..100 {
                        if fs::remove_dir_all(&path).is_ok() || !path.exists() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            }
        }
    }
    fn complete(client: &SearchClient, request: u64, total: usize) -> Arc<SearchWindow> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            while let Some(event) = client.try_recv() {
                match event {
                    SearchEvent::Window(window)
                        if window.request == request
                            && window.status.complete
                            && !window.status.from_cache
                            && window.total == Some(total)
                            && window.snapshot.entries.len()
                                == total
                                    .saturating_sub(window.start)
                                    .min(WINDOW_PAGES * PAGE_ROWS) =>
                    {
                        return window;
                    }
                    SearchEvent::Failed { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("search did not complete within the fixture deadline")
    }

    fn launcher_complete(client: &SearchClient, request: u64) -> (Vec<LauncherFile>, IndexStatus) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            while let Some(event) = client.try_recv() {
                match event {
                    SearchEvent::Launcher {
                        request: id,
                        rows,
                        status,
                        complete: true,
                    } if id == request => return (rows, status),
                    SearchEvent::Launcher { request: id, .. } => {
                        assert_eq!(id, request, "Stale launcher result")
                    }
                    SearchEvent::Failed { message, .. } => panic!("{message}"),
                    _ => {}
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        panic!("launcher search did not complete within the fixture deadline")
    }

    fn unstarted_hub() -> SearchHub {
        SearchHub::unstarted_for_test()
    }

    #[test]
    fn native_middle_and_end_windows_publish_counts_without_a_followup_count_query() {
        let fixture = Fixture::new(0);
        fs::create_dir_all(fixture.cache()).unwrap();
        let cancel = Cancellation::new();
        let mut builder =
            IndexBuilder::create(&fixture.cache(), &fixture.scope(), &cancel).unwrap();
        let total = 2_048;
        for start in (0..total).step_by(256) {
            let records = (start..start + 256)
                .map(|row| {
                    foldex_index::IndexRecord::new(
                        PathBuf::from(format!("item{row:06}.txt")),
                        false,
                        row as u64,
                        0,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            builder.ingest_batch(&records, &cancel).unwrap();
        }
        let index = builder.finish(true).unwrap();
        let hub = unstarted_hub();
        let client = hub.client();
        client.request(fixture.scope(), Query::default(), 0, false);
        let mut context = Context::new(
            &client.state,
            client.state.input().unwrap(),
            &fixture.cache(),
        )
        .unwrap();
        context.backend = Some(Backend::Complete(index));
        for start in [0, total / 2, total - 80, 256] {
            let request = client.request(fixture.scope(), Query::default(), start, false);
            context.replace_request(client.state.input().unwrap());
            assert!(client.window_pending(request));
            context.fetch_window(&cancel).unwrap();
            assert!(!client.window_pending(request));
            let Some(SearchEvent::Window(window)) = client.try_recv() else {
                panic!("missing native window");
            };
            assert_eq!(window.start, start);
            assert_eq!(window.total, Some(total));
            assert_eq!(window.kind_counts, Some((0, total)));
            assert_eq!(
                window.snapshot.entries[0].name,
                format!("item{start:06}.txt")
            );
            assert_eq!(
                window.snapshot.entries.len(),
                WINDOW_ROWS.min(total - start)
            );
            assert!(
                !context.count_dirty,
                "native totals must be reused on the first publication"
            );
        }
    }

    #[test]
    fn superseded_and_cancelled_seek_windows_cannot_be_delivered_as_current_rows() {
        let hub = unstarted_hub();
        let client = hub.client();
        let root = PathBuf::from("virtual-seek-fixture");
        let window = |request, start| {
            Arc::new(SearchWindow {
                request,
                start,
                snapshot: Arc::new(Snapshot::from_entries(
                    root.clone(),
                    vec![Entry::from_name("only-loaded-row.txt", false, 0, 0)],
                )),
                total: Some(10_000_123),
                kind_counts: Some((0, 10_000_123)),
                has_more: true,
                status: test_status(root.clone(), PathBuf::from("virtual.sqlite3")),
                elapsed: Duration::ZERO,
            })
        };
        let middle = client.request(root.clone(), Query::default(), 5_000_000, false);
        let old = client.state.input().unwrap();
        assert!(client.window_pending(middle));
        client.state.window(&old, window(middle, 5_000_000));
        let end = client.request(root.clone(), Query::default(), 10_000_000, false);
        assert!(old.query_cancel.is_cancelled());
        assert!(
            !old.scan_cancel.is_cancelled(),
            "viewport requests must preserve traversal"
        );
        assert!(!client.window_pending(middle) && client.window_pending(end));
        assert!(
            client.try_recv().is_none(),
            "queued stale rows escaped after a new seek"
        );
        client.state.window(&old, window(middle, 5_000_000));
        assert!(
            client.try_recv().is_none(),
            "late cancelled rows were published"
        );
        let latest = client.state.input().unwrap();
        client.state.window(&latest, window(end, 10_000_000));
        assert!(
            client.window_pending(end),
            "the first page must not finish a multi-page window"
        );
        assert!(
            matches!(client.try_recv(), Some(SearchEvent::Window(window)) if window.request == end && window.start == 10_000_000)
        );
        client.state.window(&latest, window(end, 10_000_000));
        client.cancel();
        assert!(!client.window_pending(end));
        assert!(
            client.try_recv().is_none(),
            "cancellation must discard a queued current window"
        );
    }

    #[test]
    fn finished_duplicate_requests_replay_with_fresh_ids_and_keep_boundary_validation() {
        let fixture = Fixture::new(8);
        fs::create_dir_all(fixture.cache()).unwrap();
        let hub = unstarted_hub();
        let client = hub.client();
        let first = client.request(fixture.scope(), Query::default(), 0, false);
        let mut context = Context::new(
            &client.state,
            client.state.input().unwrap(),
            &fixture.cache(),
        )
        .unwrap();
        let cancel = Cancellation::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while context
            .step(&fixture.cache(), false, &cancel, &cancel)
            .unwrap()
        {
            assert!(Instant::now() < deadline);
        }
        let original = complete(&client, first, 8);
        let repeated = client.request(fixture.scope(), Query::default(), 0, false);
        assert!(repeated > first);
        context.replace_request(client.state.input().unwrap());
        assert!(!context.query_dirty && !context.count_dirty);
        let replayed = complete(&client, repeated, 8);
        assert!(Arc::ptr_eq(&original.snapshot, &replayed.snapshot));

        let folders = Query {
            kind: foldex_core::KindFilter::Folders,
            ..Query::default()
        };
        let filtered = client.request(fixture.scope(), folders, 0, false);
        context.replace_request(client.state.input().unwrap());
        context.fetch_window(&cancel).unwrap();
        context.fetch_count(&cancel).unwrap();
        let filtered = complete(&client, filtered, 0);
        assert_eq!(filtered.kind_counts, Some((0, 8)));
        // A kind toggle uses the existing pre-toggle counts rather than replacing them.
        assert_eq!(
            context.counts.as_ref().unwrap().query.kind,
            foldex_core::KindFilter::All
        );

        let mut invalid = original.status.clone();
        invalid.generation.push_str("-unrelated");
        client.request_as_of(
            fixture.scope(),
            context.request.query.clone(),
            0,
            false,
            Some(invalid),
        );
        context.replace_request(client.state.input().unwrap());
        assert!(
            context.query_dirty,
            "Pinned requests must reach backend validation"
        );
        assert!(matches!(
            context.fetch_window(&cancel),
            Err(foldex_index::Error::InvalidCursor)
        ));
        client.cancel();
        assert!(
            client.try_recv().is_none(),
            "Cancelled cached events must be discarded"
        );
    }

    fn test_status(scope: PathBuf, database_path: PathBuf) -> IndexStatus {
        IndexStatus {
            generation: "generation".into(),
            revision: 2,
            scope,
            database_path,
            state: foldex_index::BuildState::Complete,
            complete: true,
            entries: 8,
            files: 8,
            folders: 0,
            skipped: 0,
            excluded_links: 0,
            excluded_cache: 0,
            started_unix_ms: 1,
            updated_unix_ms: 2,
            source_root_modified_secs: None,
            from_cache: false,
            synthetic: false,
        }
    }

    #[test]
    fn count_cache_invalidates_on_filter_or_prefix_changes() {
        let status = test_status(PathBuf::from("scope"), PathBuf::from("catalog.sqlite3"));
        let cache = CountCache {
            query: Query::default(),
            status: status.clone(),
            kind_counts: (0, 8),
        };
        let reordered = Query {
            kind: foldex_core::KindFilter::Files,
            sort: foldex_core::SortColumn::Size,
            descending: true,
            ..Query::default()
        };
        assert!(cache.matches(&reordered, &status));

        for query in [
            Query {
                text: "item".into(),
                ..Query::default()
            },
            Query {
                match_mode: foldex_core::MatchMode::Substring,
                ..Query::default()
            },
            Query {
                show_hidden: true,
                ..Query::default()
            },
            Query {
                type_filter: Some(foldex_core::TypeFilter {
                    extensions: vec!["txt".into()],
                    ..foldex_core::TypeFilter::default()
                }),
                ..Query::default()
            },
        ] {
            assert!(!cache.matches(&query, &status));
        }
        let mut different = status.clone();
        different.generation.push('2');
        assert!(!cache.matches(&Query::default(), &different));
        different = status.clone();
        different.revision += 1;
        assert!(!cache.matches(&Query::default(), &different));
        different = status.clone();
        different.entries += 1;
        assert!(!cache.matches(&Query::default(), &different));
        different = status.clone();
        different.scope.push("other");
        assert!(!cache.matches(&Query::default(), &different));
        different = status;
        different.database_path.push("other");
        assert!(!cache.matches(&Query::default(), &different));
    }

    #[test]
    fn failed_newest_requests_free_capacity_for_an_older_waiting_client() {
        for fail_on_open in [true, false] {
            assert_waiting_client_survives_newer_failures(fail_on_open);
        }
    }

    fn assert_waiting_client_survives_newer_failures(fail_on_open: bool) {
        let fixture = Fixture::new(8);
        let mut hub = unstarted_hub();
        let waiting = hub.client();
        let request = waiting.request(fixture.scope(), Query::default(), 0, false);
        let failed: Vec<_> = (0..QUERY_TURNS)
            .map(|index| {
                let client = hub.client();
                if fail_on_open {
                    client.request_as_of(
                        fixture.scope(),
                        Query::default(),
                        0,
                        false,
                        Some(test_status(
                            fixture.scope(),
                            fixture.path.join(format!("missing-{index}.sqlite3")),
                        )),
                    );
                } else {
                    client.request(
                        fixture.scope(),
                        Query {
                            text: "q".repeat(128 * 1024 + 1),
                            ..Query::default()
                        },
                        0,
                        false,
                    );
                }
                client
            })
            .collect();
        let shared = hub.shared.clone();
        let cache = fixture.cache();
        hub.worker = Some(thread::spawn(move || run(shared, cache)));
        complete(&waiting, request, 8);
        for client in failed {
            assert!(matches!(
                client.try_recv(),
                Some(SearchEvent::Failed { .. })
            ));
        }
        hub.shutdown();
    }

    #[test]
    fn partial_file_generations_finish_after_candidate_ranking_catches_up() {
        let fixture = Fixture::new(0);
        let cancel = Cancellation::new();
        let mut builder =
            IndexBuilder::create(&fixture.cache(), &fixture.scope(), &cancel).unwrap();
        builder
            .ingest_batch(
                &[
                    foldex_index::IndexRecord::new(PathBuf::from("report.md"), false, 16, 0)
                        .unwrap(),
                ],
                &cancel,
            )
            .unwrap();
        let index = builder.finish(false).unwrap();
        let mut work = LauncherWork {
            matcher: LauncherMatcher::new("report"),
            generation: index.status().generation.clone(),
            prefix: 0,
            hits: Vec::with_capacity(128),
        };
        assert!(
            !index.status().complete,
            "This is a finished partial generation"
        );
        assert!(!work.complete(index.status()), "Ranking has not caught up");
        let batch = index
            .candidate_batch(0, &cancel, &mut |name, _, _, _| work.matcher.score(name))
            .unwrap();
        work.merge(batch);
        assert_eq!(work.hits.len(), 1);
        assert!(
            work.complete(index.status()),
            "Skipped entries must not stall later launcher scopes"
        );
        let mut cached = index.status().clone();
        cached.from_cache = true;
        assert!(
            !work.complete(&cached),
            "Cached results still require revalidation"
        );
    }

    #[test]
    fn launcher_retrieves_true_typos_and_omitted_words_before_explorer_prefilter() {
        let fixture = Fixture::new(30);
        fs::write(fixture.scope().join("Visual Studio Code.md"), []).unwrap();
        fs::write(fixture.scope().join("Visual Calendar.txt"), []).unwrap();
        fs::write(fixture.scope().join("Meeting Notes.md"), []).unwrap();
        let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
        let client = hub.client();
        for query in ["visual code", "visaul code", "visual cdoe"] {
            let request = client.request_launcher(fixture.scope(), query.into(), true);
            let (rows, _) = launcher_complete(&client, request);
            assert_eq!(rows.len(), 1, "{query}");
            assert_eq!(
                rows[0].path.file_name().unwrap(),
                "Visual Studio Code.md",
                "{query}"
            );
            assert!(rows.len() <= 64);
        }
        let request = client.request_launcher(fixture.scope(), "visual code".into(), true);
        let (_, status) = launcher_complete(&client, request);
        // Simulate an old result queued immediately before superseding input.
        lock(&client.state.slots).launcher = Some((request, Vec::new(), status.clone(), true));
        let fresh = client.request_launcher(fixture.scope(), "meeting notes".into(), true);
        let (rows, _) = launcher_complete(&client, fresh);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].path.file_name().unwrap(), "Meeting Notes.md");
        client.cancel();
        lock(&client.state.slots).launcher = Some((fresh, rows, status, true));
        assert!(client.try_recv().is_none());
        hub.shutdown();
    }

    #[test]
    fn disk_search_window_preserves_sql_order_and_absolute_offsets() {
        let fixture = Fixture::new(900);
        let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
        let client = hub.client();
        let request = client.request(
            fixture.scope(),
            Query {
                descending: true,
                ..Query::default()
            },
            256,
            false,
        );
        let window = complete(&client, request, 900);
        assert_eq!(window.start, 256);
        assert_eq!(window.snapshot.entries.len(), 644);
        assert_eq!(window.snapshot.entries[0].name, "item643.txt");
        assert_eq!(window.snapshot.entries[643].name, "item0.txt");
        assert_eq!(
            window.snapshot.entries[0].path(&window.snapshot),
            fixture.scope().join("item643.txt")
        );
        assert!(window.snapshot.recursive && window.snapshot.complete && !window.has_more);
        assert!(
            window
                .snapshot
                .default_order
                .iter()
                .enumerate()
                .all(|(at, &row)| at == row as usize)
        );
        client.close();
        drop(client);
        drop(hub);
    }

    #[test]
    fn global_selection_resolves_unloaded_rows_or_reports_the_whole_failure() {
        let fixture = Fixture::new(900);
        let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
        let client = hub.client();
        let request = client.request(fixture.scope(), Query::default(), 0, false);
        let window = complete(&client, request, 900);
        assert_eq!(window.snapshot.entries.len(), 768);
        let selection =
            client.resolve_selection(Query::default(), window.status.clone(), vec![(700, 899)]);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut resolved = None;
        while Instant::now() < deadline {
            if let Some(SearchEvent::Selection {
                selection: id,
                result,
            }) = client.try_recv()
            {
                if id == selection {
                    resolved = Some(result);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        let paths = resolved.expect("selection worker returned").unwrap();
        assert_eq!(paths.len(), 200);
        assert_eq!(paths[0], fixture.scope().join("item700.txt"));
        assert_eq!(paths[199], fixture.scope().join("item899.txt"));
        let selection =
            client.resolve_selection(Query::default(), window.status.clone(), vec![(0, 999_999)]);
        assert!(
            matches!(client.try_recv(), Some(SearchEvent::Selection { selection: id, result: Err(SelectionFailure::TooMany { selected: 1_000_000 }) }) if id == selection)
        );
        let mut wrong = window.status.clone();
        wrong.generation.push_str("-unrelated");
        let selection = client.resolve_selection(Query::default(), wrong, vec![(0, 1)]);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failed = false;
        while Instant::now() < deadline {
            if let Some(SearchEvent::Selection {
                selection: id,
                result,
            }) = client.try_recv()
            {
                if id == selection {
                    assert_eq!(result.unwrap_err(), SelectionFailure::Changed);
                    failed = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(failed);
        client.close();
        drop(client);
        drop(hub);
    }

    #[test]
    fn selection_ranges_are_disjoint_bounded_and_never_partially_accepted() {
        assert_eq!(validate_ranges(&[(0, 1), (768, 899)]), Ok(134));
        assert_eq!(
            validate_ranges(&[(0, 1), (1, 2)]),
            Err(SelectionFailure::InvalidRange)
        );
        assert_eq!(
            validate_ranges(&[(9, 2)]),
            Err(SelectionFailure::InvalidRange)
        );
        assert_eq!(
            validate_ranges(&[(0, 4096)]),
            Err(SelectionFailure::TooMany { selected: 4097 })
        );
    }

    #[test]
    fn repeated_unfinished_refreshes_do_not_accumulate_owned_generations() {
        let fixture = Fixture::new(900);
        let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
        let client = hub.client();
        for _ in 0..8 {
            let request = client.request(fixture.scope(), Query::default(), 0, true);
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut painted = false;
            while Instant::now() < deadline {
                if let Some(SearchEvent::Window(window)) = client.try_recv() {
                    if window.request == request {
                        painted = true;
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
            assert!(painted);
        }
        let request = client.request(fixture.scope(), Query::default(), 0, true);
        complete(&client, request, 900);
        client.set_active(false);
        let scope_cache = Index::cache_dir_for(&fixture.cache(), &fixture.scope()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let files = fs::read_dir(&scope_cache)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "sqlite3")
                })
                .count();
            if files <= 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "retained {files} cancelled generation files"
            );
            thread::sleep(Duration::from_millis(2));
        }
        client.close();
        drop(client);
        drop(hub);
    }

    #[test]
    fn disk_search_inactive_and_latest_requests_are_bounded_and_cancellable() {
        let fixture = Fixture::new(900);
        let hub = SearchHub::new(fixture.cache(), Arc::new(|| {})).unwrap();
        let client = hub.client();
        client.set_active(false);
        client.request(fixture.scope(), Query::default(), 0, false);
        thread::sleep(Duration::from_millis(30));
        assert!(!fixture.cache().exists());
        assert!(client.try_recv().is_none());
        client.set_active(true);
        client.request(
            fixture.scope(),
            Query {
                text: "absent".into(),
                ..Query::default()
            },
            0,
            false,
        );
        let request = client.request(
            fixture.scope(),
            Query {
                text: "item88".into(),
                ..Query::default()
            },
            0,
            false,
        );
        let window = complete(&client, request, 27);
        assert!(window.snapshot.entries.len() <= WINDOW_PAGES * PAGE_ROWS);
        assert!(
            window.snapshot.entries.iter().all(|entry| entry
                .name
                .chars()
                .filter(|letter| *letter == '8')
                .count()
                >= 2)
        );
        client.cancel();
        while let Some(event) = client.try_recv() {
            panic!("cancelled events must be discarded: {event:?}");
        }
        client.close();
        drop(client);
        drop(hub);
    }
}
