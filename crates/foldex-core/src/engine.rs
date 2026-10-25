use crate::{
    RestoredView, ViewRestore,
    model::{Query, Snapshot, filter_cancelled},
    scan::{self, CollectOptions},
};
use std::{
    any::Any,
    collections::VecDeque,
    ffi::OsString,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};
use tokio::{
    runtime::{Builder, Handle},
    sync::{Semaphore, mpsc, watch},
};

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Debug)]
pub struct EngineOptions {
    pub managed_bytes: usize,
    pub cache_bytes: usize,
    pub cache_directories: usize,
    pub directory_bytes: usize,
    pub scan_bytes: usize,
    pub scan_entries: usize,
    pub scan_depth: usize,
    /// Use nonrecursive ReadDirectoryChangesW on Windows for visible directories.
    /// Notifications cause a fresh enumeration rather than applying untrusted path deltas.
    pub native_watch: bool,
    /// Fallback for unavailable native watches. Parent-directory mtime cannot detect all
    /// in-place writes or metadata changes. None disables this fallback, not native watching.
    pub poll_interval: Option<Duration>,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            managed_bytes: 64 * 1024 * 1024,
            cache_bytes: 16 * 1024 * 1024,
            cache_directories: 8,
            directory_bytes: scan::DEFAULT_DIRECTORY_BYTES,
            scan_bytes: scan::DEFAULT_SCAN_BYTES,
            scan_entries: scan::DEFAULT_SCAN_ENTRIES,
            scan_depth: 128,
            native_watch: true,
            poll_interval: Some(Duration::from_secs(2)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Navigate,
    Filter,
    Scan,
    Poll,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WatchCoverage {
    ManualRefresh,
    /// A nonrecursive native watch invalidates the listing on names, size, write-time,
    /// creation-time, attributes and security changes. Fresh enumeration reconciles it.
    /// Overflow counts persist for the current watch and trigger full cache invalidation.
    NativeDirectoryChanges {
        buffer_bytes: usize,
        overflow_count: u64,
    },
    /// Detects parent-directory modified-time changes. In-place writes/metadata-only changes
    /// and some filesystems require explicit refresh; this is not ReadDirectoryChangesW.
    DirectoryMtimePolling {
        interval: Duration,
    },
}

#[derive(Clone, Debug)]
pub enum Event {
    Directory {
        generation: u64,
        snapshot: Arc<Snapshot>,
        cached: bool,
        done: bool,
    },
    View {
        generation: u64,
        snapshot: Arc<Snapshot>,
        indices: Arc<[u32]>,
        elapsed: Duration,
        kind_counts: (usize, usize),
        restored: Option<RestoredView>,
    },
    ScanProgress {
        generation: u64,
        snapshot: Arc<Snapshot>,
        visited: u64,
        done: bool,
    },
    WatchStatus {
        generation: u64,
        coverage: WatchCoverage,
    },
    Error {
        generation: u64,
        path: PathBuf,
        message: String,
        operation: Operation,
    },
}

#[derive(Clone)]
struct Navigation {
    generation: u64,
    path: PathBuf,
    refresh: bool,
}
#[derive(Clone)]
struct Filtering {
    generation: u64,
    navigation: u64,
    snapshot: Arc<Snapshot>,
    query: Query,
    // Consume-once payload: the latest request slot must not pin the previous model
    // indefinitely after restoration finishes. Cloning a request only clones this Arc.
    restore: Option<Arc<Mutex<Option<ViewRestore>>>>,
}
#[derive(Clone)]
struct Scanning {
    generation: u64,
    root: PathBuf,
}

struct BrowserState {
    navigation: AtomicU64,
    query: AtomicU64,
    scan: AtomicU64,
    closed: AtomicBool,
    active: AtomicBool,
    scoped_search: AtomicBool,
    data_revision: AtomicU64,
    data_changes: Mutex<DataChanges>,
}

/// Bounded invalidation hints, not a filesystem/model cache. A caller that falls
/// behind the retained names receives an explicit whole-scope invalidation.
#[derive(Default)]
struct DataChanges {
    generation: u64,
    revision: u64,
    all_after: u64,
    names: Vec<(u64, OsString)>,
    bytes: usize,
}
impl DataChanges {
    fn record(&mut self, generation: u64, revision: u64, names: Option<&[OsString]>) {
        if self.generation != generation {
            *self = Self {
                generation,
                ..Default::default()
            };
        }
        self.revision = revision;
        let Some(names) = names else {
            self.all_after = revision;
            self.names.clear();
            self.bytes = 0;
            return;
        };
        for name in names {
            if let Some((at, _)) = self.names.iter_mut().find(|(_, old)| old == name) {
                *at = revision;
                continue;
            }
            let bytes = name.as_encoded_bytes().len();
            if self.names.len() == 64 || self.bytes.saturating_add(bytes) > 64 * 1024 {
                self.all_after = revision;
                self.names.clear();
                self.bytes = 0;
                return;
            }
            self.bytes += bytes;
            self.names.push((revision, name.clone()));
        }
    }
    fn since(&self, generation: u64, revision: u64, since: u64) -> (u64, Option<Vec<OsString>>) {
        if self.generation != generation {
            return (revision, Some(Vec::new()));
        }
        (
            self.revision,
            if since < self.all_after {
                None
            } else {
                Some(
                    self.names
                        .iter()
                        .filter(|(at, _)| *at > since)
                        .map(|(_, name)| name.clone())
                        .collect(),
                )
            },
        )
    }
}
impl BrowserState {
    fn filesystem_changed(&self, generation: u64, names: Option<&[OsString]>) {
        if self.navigation.load(Ordering::Acquire) != generation {
            return;
        }
        let mut changes = self
            .data_changes
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.navigation.load(Ordering::Acquire) != generation {
            return;
        }
        let revision = self
            .data_revision
            .load(Ordering::Relaxed)
            .wrapping_add(1)
            .max(1);
        changes.record(generation, revision, names);
        self.data_revision.store(revision, Ordering::Release);
    }
    fn cancelled(&self, shared: &Shared, lane: Lane, generation: u64, navigation: u64) -> bool {
        self.closed.load(Ordering::Relaxed)
            || shared.stopped.load(Ordering::Relaxed)
            || match lane {
                Lane::Directory | Lane::Status => {
                    self.navigation.load(Ordering::Acquire) != generation
                }
                Lane::View => {
                    self.query.load(Ordering::Acquire) != generation
                        || self.navigation.load(Ordering::Acquire) != navigation
                }
                Lane::Scan => self.scan.load(Ordering::Acquire) != generation,
            }
    }
}

#[derive(Clone, Copy)]
enum Lane {
    Directory,
    View,
    Scan,
    Status,
}
#[derive(Clone)]
struct Stamped {
    lane: Lane,
    generation: u64,
    navigation: u64,
    event: Event,
}

#[derive(Clone)]
struct Outputs {
    directory: watch::Sender<Option<Stamped>>,
    view: watch::Sender<Option<Stamped>>,
    scan: watch::Sender<Option<Stamped>>,
    status: watch::Sender<Option<Stamped>>,
    errors: mpsc::Sender<Stamped>,
    wake: Wake,
}
impl Outputs {
    /// The lane's background receiver clears its old result before handling the
    /// next command, including cancellation. Otherwise an idle watch slot pins
    /// a cancelled directory indefinitely even after the UI released its model.
    fn clear(&self, lane: Lane, shared: &Shared) {
        let previous = match lane {
            Lane::Directory => self.directory.send_replace(None),
            Lane::View => self.view.send_replace(None),
            Lane::Scan => self.scan.send_replace(None),
            Lane::Status => self.status.send_replace(None),
        };
        if let Some(previous) = previous {
            // Saturation can destroy this value here on the background receiver,
            // never on the UI thread. Usually the retirement worker handles it.
            let _ = shared.defer(previous);
        }
    }
    fn publish(&self, lane: Lane, generation: u64, navigation: u64, event: Event) {
        let event = Some(Stamped {
            lane,
            generation,
            navigation,
            event,
        });
        match lane {
            Lane::Directory => {
                self.directory.send_replace(event);
            }
            Lane::View => {
                self.view.send_replace(event);
            }
            Lane::Scan => {
                self.scan.send_replace(event);
            }
            Lane::Status => {
                self.status.send_replace(event);
            }
        }
        (self.wake)();
    }
    fn error(
        &self,
        lane: Lane,
        generation: u64,
        navigation: u64,
        path: PathBuf,
        operation: Operation,
        message: String,
    ) {
        let event = Stamped {
            lane,
            generation,
            navigation,
            event: Event::Error {
                generation,
                path,
                message,
                operation,
            },
        };
        // Errors are small and bounded. A fault storm must not allocate an unbounded queue.
        if self.errors.try_send(event).is_ok() {
            (self.wake)();
        }
    }
}

struct CacheEntry {
    path: PathBuf,
    snapshot: Arc<Snapshot>,
    charged_bytes: usize,
}
struct DirectoryCache {
    entries: VecDeque<CacheEntry>,
    bytes: usize,
    max_bytes: usize,
    max_entries: usize,
}
impl DirectoryCache {
    fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_bytes,
            max_entries,
        }
    }
    fn get(&mut self, path: &PathBuf) -> Option<Arc<Snapshot>> {
        let position = self.entries.iter().position(|entry| &entry.path == path)?;
        let entry = self.entries.remove(position)?;
        let snapshot = entry.snapshot.clone();
        self.entries.push_back(entry);
        Some(snapshot)
    }
    fn invalidate(&mut self, path: &PathBuf) {
        if let Some(position) = self.entries.iter().position(|entry| &entry.path == path) {
            if let Some(entry) = self.entries.remove(position) {
                self.bytes = self.bytes.saturating_sub(entry.charged_bytes);
            }
        }
    }
    fn insert(&mut self, snapshot: Arc<Snapshot>) {
        if let Some(position) = self
            .entries
            .iter()
            .position(|entry| entry.path == snapshot.path)
        {
            if let Some(old) = self.entries.remove(position) {
                self.bytes = self.bytes.saturating_sub(old.charged_bytes);
            }
        }
        // Reserve for the snapshot's one alternate-order cache as well.
        let charged_bytes = snapshot
            .managed_bytes
            .saturating_add(snapshot.entries.len() * 4);
        if charged_bytes > self.max_bytes || self.max_entries == 0 {
            return;
        }
        while self.bytes.saturating_add(charged_bytes) > self.max_bytes
            || self.entries.len() >= self.max_entries
        {
            if let Some(old) = self.entries.pop_front() {
                self.bytes = self.bytes.saturating_sub(old.charged_bytes);
            } else {
                break;
            }
        }
        self.bytes += charged_bytes;
        self.entries.push_back(CacheEntry {
            path: snapshot.path.clone(),
            snapshot,
            charged_bytes,
        });
    }
}

struct Shared {
    sequence: AtomicU64,
    stopped: AtomicBool,
    cache: Mutex<DirectoryCache>,
    /// Weak identities let independent visible tabs share an unchanged model,
    /// even when freshness invalidation has removed the directory cache entry.
    models: Mutex<std::collections::HashMap<PathBuf, std::sync::Weak<Snapshot>>>,
    navigation_slots: Arc<Semaphore>,
    query_slots: Arc<Semaphore>,
    scan_slots: Arc<Semaphore>,
    poll_slots: Arc<Semaphore>,
    foreground_jobs: AtomicUsize,
    options: EngineOptions,
    memory: Arc<crate::memory::MemoryBudget>,
    retired: std::sync::mpsc::SyncSender<Box<dyn Any + Send>>,
}
impl Shared {
    fn next(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
    fn defer<T: Send + 'static>(&self, value: T) -> Result<(), T> {
        self.retired.try_send(Box::new(value)).map_err(|error| {
            // The failed send returns precisely the value boxed above.
            let (std::sync::mpsc::TrySendError::Full(value)
            | std::sync::mpsc::TrySendError::Disconnected(value)) = error;
            *value.downcast::<T>().expect("retirement type")
        })
    }
}

/// Owns a small Tokio runtime on a coordinator thread. Keep it alive while Browsers are used.
pub struct Engine {
    shared: Arc<Shared>,
    handle: Handle,
    shutdown: watch::Sender<bool>,
    coordinator: Option<thread::JoinHandle<()>>,
    wake: Wake,
}

impl Engine {
    pub fn new(wake: Wake) -> io::Result<Self> {
        Self::with_options(wake, EngineOptions::default())
    }

    pub fn with_options(wake: Wake, options: EngineOptions) -> io::Result<Self> {
        let runtime = Builder::new_current_thread()
            .enable_time()
            .max_blocking_threads(5)
            .thread_keep_alive(Duration::from_secs(3))
            .thread_name("foldex-worker")
            .build()?;
        let handle = runtime.handle().clone();
        let (retired, retirement) = std::sync::mpsc::sync_channel::<Box<dyn Any + Send>>(64);
        thread::Builder::new()
            .name("foldex-retirement".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                while let Ok(snapshot) = retirement.recv() {
                    drop(snapshot);
                }
            })?;
        let (shutdown, mut stop) = watch::channel(false);
        let coordinator = thread::Builder::new()
            .name("foldex-coordinator".into())
            .spawn(move || {
                runtime.block_on(async {
                    let _ = stop.changed().await;
                });
                // Blocking filesystem calls may be inside Windows. Never join them on the UI thread.
                runtime.shutdown_timeout(Duration::from_millis(100));
            })?;
        let shared = Arc::new(Shared {
            models: Mutex::new(std::collections::HashMap::new()),
            sequence: AtomicU64::new(1),
            stopped: AtomicBool::new(false),
            cache: Mutex::new(DirectoryCache::new(
                options.cache_bytes,
                options.cache_directories,
            )),
            navigation_slots: Arc::new(Semaphore::new(2)),
            query_slots: Arc::new(Semaphore::new(1)),
            scan_slots: Arc::new(Semaphore::new(1)),
            poll_slots: Arc::new(Semaphore::new(1)),
            foreground_jobs: AtomicUsize::new(0),
            memory: crate::memory::MemoryBudget::new(options.managed_bytes),
            retired,
            options,
        });
        Ok(Self {
            shared,
            handle,
            shutdown,
            coordinator: Some(coordinator),
            wake,
        })
    }

    pub fn browser(&self) -> Browser {
        let state = Arc::new(BrowserState {
            navigation: AtomicU64::new(0),
            query: AtomicU64::new(0),
            scan: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            active: AtomicBool::new(true),
            scoped_search: AtomicBool::new(false),
            data_revision: AtomicU64::new(0),
            data_changes: Mutex::new(DataChanges::default()),
        });
        let (navigation, navigation_rx) = watch::channel(None);
        let (filtering, filtering_rx) = watch::channel(None);
        let (scanning, scanning_rx) = watch::channel(None);
        let (freshness, freshness_rx) = watch::channel(());
        let (directory, directory_rx) = watch::channel(None);
        let (view, view_rx) = watch::channel(None);
        let (scan_output, scan_rx) = watch::channel(None);
        let (status, status_rx) = watch::channel(None);
        let (errors, errors_rx) = mpsc::channel(16);
        let outputs = Outputs {
            directory,
            view,
            scan: scan_output,
            status,
            errors,
            wake: self.wake.clone(),
        };
        self.handle.spawn(navigation_loop(
            self.shared.clone(),
            state.clone(),
            outputs.clone(),
            navigation_rx.clone(),
        ));
        self.handle.spawn(query_loop(
            self.shared.clone(),
            state.clone(),
            outputs.clone(),
            filtering_rx,
        ));
        self.handle.spawn(scan_loop(
            self.shared.clone(),
            state.clone(),
            outputs.clone(),
            scanning_rx,
        ));
        if self.shared.options.native_watch || self.shared.options.poll_interval.is_some() {
            self.handle.spawn(freshness_loop(
                self.shared.clone(),
                state.clone(),
                outputs,
                navigation_rx,
                navigation.clone(),
                freshness_rx,
            ));
        }
        Browser {
            shared: self.shared.clone(),
            state,
            navigation,
            filtering,
            scanning,
            freshness,
            directory: directory_rx,
            view: view_rx,
            scan: scan_rx,
            status: status_rx,
            errors: errors_rx,
        }
    }

    /// Conservative shared model reservations, including models pinned by UI tabs.
    pub fn managed_bytes(&self) -> usize {
        self.shared.memory.used()
    }

    /// Nonblocking destruction handoff. Retain and retry the returned snapshot if saturated.
    pub fn retire(&self, snapshot: Arc<Snapshot>) -> Result<(), Arc<Snapshot>> {
        self.shared.defer(snapshot)
    }

    /// Hand off a closed pane and all its pinned models without destroying them on the UI thread.
    /// Saturation returns ownership so the caller can retain it and retry on a later frame.
    pub fn defer<T: Send + 'static>(&self, value: T) -> Result<(), T> {
        self.shared.defer(value)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.shutdown.send_replace(true);
        // Dropping a JoinHandle detaches it. Closing an app must not wait on a disconnected share.
        let _ = self.coordinator.take();
    }
}

pub struct Browser {
    shared: Arc<Shared>,
    state: Arc<BrowserState>,
    navigation: watch::Sender<Option<Navigation>>,
    filtering: watch::Sender<Option<Filtering>>,
    scanning: watch::Sender<Option<Scanning>>,
    freshness: watch::Sender<()>,
    directory: watch::Receiver<Option<Stamped>>,
    view: watch::Receiver<Option<Stamped>>,
    scan: watch::Receiver<Option<Stamped>>,
    status: watch::Receiver<Option<Stamped>>,
    errors: mpsc::Receiver<Stamped>,
}

impl Browser {
    /// True filesystem invalidations only. Watching readiness/reactivation,
    /// filtering, selection and equivalent model publications do not advance it.
    pub fn data_revision(&self) -> u64 {
        self.state.data_revision.load(Ordering::Acquire)
    }
    /// Nonblocking bounded child-name hints since an acknowledged revision.
    /// Outer None means mailbox contention (retry); inner None means lost/overflow
    /// coverage and requires whole-scope invalidation. Names are never model deltas.
    pub fn filesystem_changes_since(&self, since: u64) -> Option<(u64, Option<Vec<OsString>>)> {
        let changes = self.state.data_changes.try_lock().ok()?;
        Some(changes.since(
            self.state.navigation.load(Ordering::Acquire),
            self.data_revision(),
            since,
        ))
    }
    /// Atomic model-pool counters. This performs no filesystem work or locking.
    pub fn model_memory_usage(&self) -> (usize, usize) {
        (self.shared.memory.used(), self.shared.options.managed_bytes)
    }
    pub fn navigate(&self, path: PathBuf, refresh: bool) -> u64 {
        let generation = self.shared.next();
        self.state.navigation.store(generation, Ordering::Release);
        self.state.query.store(generation, Ordering::Release);
        self.state.scan.store(generation, Ordering::Release);
        self.state.scoped_search.store(false, Ordering::Release);
        self.scanning.send_replace(None);
        if let Some(previous) = self.filtering.send_replace(None) {
            let _ = self.shared.defer(previous);
        }
        self.navigation.send_replace(Some(Navigation {
            generation,
            path,
            refresh,
        }));
        generation
    }
    pub fn filter(&self, snapshot: Arc<Snapshot>, query: Query) -> u64 {
        self.filter_with_restore(snapshot, query, None)
    }
    pub fn cancel_filter(&self) -> u64 {
        let generation = self.shared.next();
        self.state.query.store(generation, Ordering::Release);
        if let Some(previous) = self.filtering.send_replace(None) {
            let _ = self.shared.defer(previous);
        }
        generation
    }
    pub fn filter_with_restore(
        &self,
        snapshot: Arc<Snapshot>,
        query: Query,
        restore: Option<ViewRestore>,
    ) -> u64 {
        let generation = self.shared.next();
        self.state.query.store(generation, Ordering::Release);
        if let Some(previous) = self.filtering.send_replace(Some(Filtering {
            generation,
            navigation: self.state.navigation.load(Ordering::Acquire),
            snapshot,
            query,
            restore: restore.map(|restore| Arc::new(Mutex::new(Some(restore)))),
        })) {
            // Restore payloads may retain an older, large model too. Drop the entire
            // superseded request on the retirement worker, not just its new snapshot.
            let _ = self.shared.defer(previous);
        }
        generation
    }
    pub fn scan(&self, root: PathBuf) -> u64 {
        let generation = self.shared.next();
        self.state.scan.store(generation, Ordering::Release);
        self.state.scoped_search.store(true, Ordering::Release);
        self.freshness.send_replace(());
        self.scanning
            .send_replace(Some(Scanning { generation, root }));
        generation
    }
    pub fn cancel_scan(&self) {
        self.state.scan.store(self.shared.next(), Ordering::Release);
        self.scanning.send_replace(None);
        self.state.scoped_search.store(false, Ordering::Release);
        self.freshness.send_replace(());
    }
    /// Supersede pending path work without opening another directory.
    pub fn cancel_navigation(&self) {
        let generation = self.shared.next();
        self.state.navigation.store(generation, Ordering::Release);
        self.state.query.store(generation, Ordering::Release);
        self.state.scan.store(generation, Ordering::Release);
        self.navigation.send_replace(None);
        if let Some(previous) = self.filtering.send_replace(None) {
            let _ = self.shared.defer(previous);
        }
        self.scanning.send_replace(None);
        self.state.scoped_search.store(false, Ordering::Release);
        self.freshness.send_replace(());
    }
    /// Hidden tabs should be inactive so only visible directories are watched or polled.
    pub fn set_active(&self, active: bool) {
        if self.state.active.swap(active, Ordering::AcqRel) != active {
            self.freshness.send_replace(());
        }
    }

    /// Nonblocking destruction handoff. Retain and retry the returned snapshot if saturated.
    pub fn retire(&self, snapshot: Arc<Snapshot>) -> Result<(), Arc<Snapshot>> {
        self.shared.defer(snapshot)
    }
    pub fn defer<T: Send + 'static>(&self, value: T) -> Result<(), T> {
        self.shared.defer(value)
    }
    pub fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
        self.freshness.send_replace(());
    }
    /// Nonblocking and bounded: checks four latest-result slots and at most sixteen errors.
    pub fn try_recv(&mut self) -> Option<Event> {
        for receiver in [
            &mut self.directory,
            &mut self.view,
            &mut self.scan,
            &mut self.status,
        ] {
            if receiver.has_changed().unwrap_or(false) {
                let next = receiver.borrow_and_update().clone();
                if let Some(next) = next {
                    if !self.state.cancelled(
                        &self.shared,
                        next.lane,
                        next.generation,
                        next.navigation,
                    ) {
                        return Some(next.event);
                    }
                }
            }
        }
        for _ in 0..16 {
            let next = self.errors.try_recv().ok()?;
            if !self
                .state
                .cancelled(&self.shared, next.lane, next.generation, next.navigation)
            {
                return Some(next.event);
            }
        }
        None
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        self.close();
    }
}

struct Foreground(Arc<Shared>);
impl Drop for Foreground {
    fn drop(&mut self) {
        self.0.foreground_jobs.fetch_sub(1, Ordering::Release);
    }
}

async fn navigation_loop(
    shared: Arc<Shared>,
    state: Arc<BrowserState>,
    outputs: Outputs,
    mut requests: watch::Receiver<Option<Navigation>>,
) {
    while requests.changed().await.is_ok() {
        outputs.clear(Lane::Directory, &shared);
        let Some(request) = requests.borrow_and_update().clone() else {
            continue;
        };
        let generation = request.generation;
        if state.cancelled(&shared, Lane::Directory, generation, generation) {
            continue;
        }
        if !shared.options.native_watch && shared.options.poll_interval.is_none() {
            outputs.publish(
                Lane::Status,
                generation,
                generation,
                Event::WatchStatus {
                    generation,
                    coverage: WatchCoverage::ManualRefresh,
                },
            );
        }
        if !request.refresh {
            let cached = shared
                .cache
                .lock()
                .ok()
                .and_then(|mut cache| cache.get(&request.path))
                .or_else(|| {
                    shared
                        .models
                        .lock()
                        .ok()
                        .and_then(|models| {
                            models.get(&request.path).and_then(std::sync::Weak::upgrade)
                        })
                        .filter(|snapshot| snapshot.complete)
                });
            if let Some(snapshot) = cached {
                outputs.publish(
                    Lane::Directory,
                    generation,
                    generation,
                    Event::Directory {
                        generation,
                        snapshot,
                        cached: true,
                        done: false,
                    },
                );
            }
        }
        let Ok(permit) = shared.navigation_slots.clone().acquire_owned().await else {
            break;
        };
        if state.cancelled(&shared, Lane::Directory, generation, generation) {
            continue;
        }
        shared.foreground_jobs.fetch_add(1, Ordering::Release);
        if shared.memory.available() < 1024 * 1024 {
            if let Ok(mut cache) = shared.cache.lock() {
                cache.entries.clear();
                cache.bytes = 0;
            }
        }
        let worker_shared = shared.clone();
        let worker_state = state.clone();
        let worker_outputs = outputs.clone();
        let path = request.path.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _foreground = Foreground(worker_shared.clone());
            let cancelled =
                || worker_state.cancelled(&worker_shared, Lane::Directory, generation, generation);
            if cancelled() {
                return Ok(None);
            }
            let Some(snapshot) = scan::collect(
                path,
                CollectOptions {
                    recursive: false,
                    max_entries: u32::MAX as usize,
                    max_bytes: worker_shared.options.directory_bytes,
                    max_depth: 1,
                },
                &cancelled,
                Some(worker_shared.memory.reservation()),
                &mut |snapshot| {
                    if !cancelled() {
                        worker_outputs.publish(
                            Lane::Directory,
                            generation,
                            generation,
                            Event::Directory {
                                generation,
                                snapshot,
                                cached: false,
                                done: false,
                            },
                        );
                    }
                },
            )?
            else {
                return Ok(None);
            };
            // Equality can walk every filename, and reusing an earlier model
            // destroys the new duplicate. Keep both on this blocking job so the
            // coordinator remains free to dispatch queries and watch changes.
            let Some(snapshot) = share_unchanged_model(&worker_shared, snapshot, &cancelled) else {
                return Ok(None);
            };
            Ok::<_, io::Error>(Some(snapshot))
        })
        .await;
        if state.cancelled(&shared, Lane::Directory, generation, generation) {
            continue;
        }
        match result {
            Ok(Ok(Some(snapshot))) => {
                if let Ok(mut cache) = shared.cache.lock() {
                    cache.insert(snapshot.clone());
                }
                outputs.publish(
                    Lane::Directory,
                    generation,
                    generation,
                    Event::Directory {
                        generation,
                        snapshot,
                        cached: false,
                        done: true,
                    },
                );
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => outputs.error(
                Lane::Directory,
                generation,
                generation,
                request.path,
                Operation::Navigate,
                error.to_string(),
            ),
            Err(error) => outputs.error(
                Lane::Directory,
                generation,
                generation,
                request.path,
                Operation::Navigate,
                error.to_string(),
            ),
        }
    }
}

async fn query_loop(
    shared: Arc<Shared>,
    state: Arc<BrowserState>,
    outputs: Outputs,
    mut requests: watch::Receiver<Option<Filtering>>,
) {
    while requests.changed().await.is_ok() {
        outputs.clear(Lane::View, &shared);
        let Some(request) = requests.borrow_and_update().clone() else {
            continue;
        };
        let (generation, navigation) = (request.generation, request.navigation);
        let Ok(permit) = shared.query_slots.clone().acquire_owned().await else {
            break;
        };
        if state.cancelled(&shared, Lane::View, generation, navigation) {
            continue;
        }
        let worker_shared = shared.clone();
        let worker_state = state.clone();
        let path = request.snapshot.path.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let begin = Instant::now();
            let cancelled =
                || worker_state.cancelled(&worker_shared, Lane::View, generation, navigation);
            let mut view = filter_cancelled(request.snapshot, &request.query, &cancelled)?;
            let restore = request
                .restore
                .and_then(|slot| slot.lock().ok().and_then(|mut payload| payload.take()));
            let restored = if let Some(restore) = restore {
                Some(crate::restore::restore_cancelled(
                    &view.snapshot,
                    &view.indices,
                    &request.query,
                    restore,
                    &worker_shared.memory,
                    &cancelled,
                )?)
            } else {
                None
            };
            view.elapsed = begin.elapsed();
            Some((view, restored))
        })
        .await;
        if state.cancelled(&shared, Lane::View, generation, navigation) {
            continue;
        }
        match result {
            Ok(Some((view, restored))) => outputs.publish(
                Lane::View,
                generation,
                navigation,
                Event::View {
                    generation,
                    snapshot: view.snapshot,
                    indices: view.indices,
                    elapsed: view.elapsed,
                    kind_counts: view.kind_counts,
                    restored,
                },
            ),
            Ok(None) => {}
            Err(error) => outputs.error(
                Lane::View,
                generation,
                navigation,
                path,
                Operation::Filter,
                error.to_string(),
            ),
        }
    }
}

async fn scan_loop(
    shared: Arc<Shared>,
    state: Arc<BrowserState>,
    outputs: Outputs,
    mut requests: watch::Receiver<Option<Scanning>>,
) {
    while requests.changed().await.is_ok() {
        outputs.clear(Lane::Scan, &shared);
        let Some(request) = requests.borrow_and_update().clone() else {
            outputs.scan.send_replace(None);
            continue;
        };
        let generation = request.generation;
        let Ok(permit) = shared.scan_slots.clone().acquire_owned().await else {
            break;
        };
        if state.cancelled(&shared, Lane::Scan, generation, 0) {
            continue;
        }
        let worker_shared = shared.clone();
        let worker_state = state.clone();
        let worker_outputs = outputs.clone();
        let root = request.root.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let cancelled = || {
                // Foreground work owns the drive's next useful slice of work.
                while worker_shared.foreground_jobs.load(Ordering::Acquire) > 0 {
                    if worker_state.cancelled(&worker_shared, Lane::Scan, generation, 0) {
                        return true;
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                worker_state.cancelled(&worker_shared, Lane::Scan, generation, 0)
            };
            scan::collect(
                root,
                CollectOptions {
                    recursive: true,
                    max_entries: worker_shared.options.scan_entries,
                    max_bytes: worker_shared.options.scan_bytes,
                    max_depth: worker_shared.options.scan_depth,
                },
                &cancelled,
                Some(worker_shared.memory.reservation()),
                &mut |snapshot| {
                    if !worker_state.cancelled(&worker_shared, Lane::Scan, generation, 0) {
                        let visited = snapshot.entries.len() as u64;
                        worker_outputs.publish(
                            Lane::Scan,
                            generation,
                            0,
                            Event::ScanProgress {
                                generation,
                                snapshot,
                                visited,
                                done: false,
                            },
                        );
                    }
                },
            )
        })
        .await;
        if state.cancelled(&shared, Lane::Scan, generation, 0) {
            continue;
        }
        match result {
            Ok(Ok(Some(snapshot))) => {
                let visited = snapshot.entries.len() as u64;
                outputs.publish(
                    Lane::Scan,
                    generation,
                    0,
                    Event::ScanProgress {
                        generation,
                        snapshot,
                        visited,
                        done: true,
                    },
                );
            }
            Ok(Ok(None)) => {}
            Ok(Err(error)) => outputs.error(
                Lane::Scan,
                generation,
                0,
                request.root,
                Operation::Scan,
                error.to_string(),
            ),
            Err(error) => outputs.error(
                Lane::Scan,
                generation,
                0,
                request.root,
                Operation::Scan,
                error.to_string(),
            ),
        }
    }
}

fn fallback_coverage(interval: Option<Duration>) -> WatchCoverage {
    interval.map_or(WatchCoverage::ManualRefresh, |interval| {
        WatchCoverage::DirectoryMtimePolling { interval }
    })
}

fn publish_coverage(outputs: &Outputs, generation: u64, coverage: WatchCoverage) {
    outputs.publish(
        Lane::Status,
        generation,
        generation,
        Event::WatchStatus {
            generation,
            coverage,
        },
    );
}

fn invalidate_cached_directory(shared: &Shared, path: &PathBuf) {
    if let Ok(mut cache) = shared.cache.lock() {
        cache.invalidate(path);
    }
}

fn share_unchanged_model(
    shared: &Shared,
    snapshot: Arc<Snapshot>,
    cancelled: &dyn Fn() -> bool,
) -> Option<Arc<Snapshot>> {
    if cancelled() {
        return None;
    }
    if !snapshot.complete || snapshot.recursive {
        return Some(snapshot);
    }
    loop {
        if cancelled() {
            return None;
        }
        let previous = shared.models.lock().ok().and_then(|models| {
            models
                .get(&snapshot.path)
                .and_then(std::sync::Weak::upgrade)
        });
        if let Some(previous) = previous.as_ref() {
            if previous.complete
                && previous.entries.len() == snapshot.entries.len()
                && previous.skipped == snapshot.skipped
                && previous.excluded_links == snapshot.excluded_links
                && previous.limit == snapshot.limit
            {
                let mut same = true;
                for (position, (old, new)) in previous
                    .entries
                    .iter()
                    .zip(snapshot.entries.iter())
                    .enumerate()
                {
                    if position % 256 == 0 && cancelled() {
                        return None;
                    }
                    if old != new {
                        same = false;
                        break;
                    }
                }
                if same {
                    return (!cancelled()).then(|| previous.clone());
                }
            }
        }
        if cancelled() {
            return None;
        }
        let Ok(mut models) = shared.models.lock() else {
            return Some(snapshot);
        };
        let current = models
            .get(&snapshot.path)
            .and_then(std::sync::Weak::upgrade);
        if current.as_ref().is_some_and(|current| {
            previous
                .as_ref()
                .is_none_or(|previous| !Arc::ptr_eq(previous, current))
        }) {
            // Another navigation worker published during the comparison. Retry
            // against it before replacing the identity; never compare or destroy
            // a model while holding the registry lock needed by the coordinator.
            drop(models);
            drop(current);
            continue;
        }
        models.retain(|_, model| model.strong_count() > 0);
        if models.len() >= 128 && !models.contains_key(&snapshot.path) {
            if let Some(path) = models.keys().next().cloned() {
                models.remove(&path);
            }
        }
        models.insert(snapshot.path.clone(), Arc::downgrade(&snapshot));
        return Some(snapshot);
    }
}

async fn freshness_loop(
    shared: Arc<Shared>,
    state: Arc<BrowserState>,
    outputs: Outputs,
    mut requests: watch::Receiver<Option<Navigation>>,
    sender: watch::Sender<Option<Navigation>>,
    mut activity: watch::Receiver<()>,
) {
    // All native handle creation, watch setup and thread creation happen on this background
    // coordinator. UI methods only publish navigation/visibility requests to latest slots.
    // Dormant tabs own only an async task, not a watcher thread/kernel handles.
    let (mut native, mut notices): (
        Option<crate::watch::NativeWatcher>,
        Option<watch::Receiver<crate::watch::Notice>>,
    ) = (None, None);
    let interval = shared
        .options
        .poll_interval
        .map(|d| d.max(Duration::from_millis(250)));
    let mut watched: Option<crate::watch::Request> = None;
    let mut native_ready = false;
    let mut native_revision = 0u64;
    let mut coverage = None;
    let mut previous: Option<(u64, Option<SystemTime>)> = None;
    let mut reconcile_due: Option<Instant> = None;
    let mut last_refresh: Option<Instant> = None;
    let mut next_poll = interval.map(|d| Instant::now() + d);

    loop {
        if state.closed.load(Ordering::Relaxed) || shared.stopped.load(Ordering::Relaxed) {
            break;
        }
        let request = requests.borrow_and_update().clone();
        let desired = request
            .as_ref()
            .filter(|_| {
                state.active.load(Ordering::Acquire) && !state.scoped_search.load(Ordering::Acquire)
            })
            .map(|request| crate::watch::Request {
                generation: request.generation,
                path: request.path.clone(),
            });
        if desired != watched {
            if desired.is_none() {
                native = None;
                notices = None;
            } else if native.is_none() && shared.options.native_watch {
                if let Ok((watcher, receiver)) = crate::watch::NativeWatcher::new() {
                    native = Some(watcher);
                    notices = Some(receiver);
                }
            }
            watched = desired;
            if let Some(native) = native.as_ref() {
                native.set(watched.clone());
            }
            native_ready = false;
            native_revision = 0;
            coverage = None;
            previous = None;
            reconcile_due = None;
            last_refresh = None;
            next_poll = interval.map(|d| {
                if native.is_some() {
                    Instant::now() + d
                } else {
                    Instant::now()
                }
            });
            if let Some(watched) = watched.as_ref() {
                let fallback = fallback_coverage(interval);
                publish_coverage(&outputs, watched.generation, fallback.clone());
                coverage = Some(fallback);
            }
        }

        let poll_due = if watched.is_some() && !native_ready {
            next_poll
        } else {
            None
        };
        let deadline = match (reconcile_due, poll_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        enum Next {
            Navigation(bool),
            Activity(bool),
            Native(Option<crate::watch::Notice>),
            Deadline,
        }
        let next = tokio::select! {
            changed = requests.changed() => Next::Navigation(changed.is_ok()),
            changed = activity.changed() => Next::Activity(changed.is_ok()),
            notice = async {
                if let Some(receiver) = notices.as_mut() {
                    receiver.changed().await.ok()?;
                    Some(receiver.borrow_and_update().clone())
                } else { std::future::pending().await }
            } => Next::Native(notice),
            _ = async {
                if let Some(deadline) = deadline {
                    tokio::time::sleep_until(deadline.into()).await;
                } else { std::future::pending::<()>().await; }
            } => Next::Deadline,
        };
        match next {
            Next::Navigation(true) | Next::Activity(true) => continue,
            Next::Navigation(false) | Next::Activity(false) => break,
            Next::Native(None) => {
                // A worker failure degrades to explicit fallback; do not spin on a closed slot.
                notices = None;
                native = None;
                native_ready = false;
                next_poll = interval.map(|_| Instant::now());
                if let Some(watched) = watched.as_ref() {
                    let fallback = fallback_coverage(interval);
                    publish_coverage(&outputs, watched.generation, fallback.clone());
                    coverage = Some(fallback);
                    invalidate_cached_directory(&shared, &watched.path);
                    reconcile_due = Some(Instant::now() + Duration::from_millis(50));
                }
            }
            Next::Native(Some(notice)) => {
                let Some(watched) = watched
                    .as_ref()
                    .filter(|w| w.generation == notice.generation)
                else {
                    continue;
                };
                if notice.revision > native_revision {
                    // Per-name revisions survive latest-slot coalescing. Only
                    // overflow/history loss widens invalidation to the scope.
                    let names = notice
                        .names
                        .as_ref()
                        .filter(|_| native_revision >= notice.names_after)
                        .map(|names| {
                            names
                                .iter()
                                .filter(|(revision, _)| *revision > native_revision)
                                .map(|(_, name)| name.clone())
                                .collect::<Vec<_>>()
                        });
                    state.filesystem_changed(watched.generation, names.as_deref());
                    native_revision = notice.revision;
                }
                native_ready = notice.ready && notice.error.is_none();
                let next_coverage = if native_ready {
                    WatchCoverage::NativeDirectoryChanges {
                        buffer_bytes: crate::watch::BUFFER_BYTES,
                        overflow_count: notice.overflows,
                    }
                } else {
                    fallback_coverage(interval)
                };
                if coverage.as_ref() != Some(&next_coverage) {
                    publish_coverage(&outputs, watched.generation, next_coverage.clone());
                    coverage = Some(next_coverage);
                }
                // The ready notification also reconciles once: enumeration may have started
                // just before the watch was armed. All later changes invalidate before refresh.
                // Sticky overflow counts remain visible even if notices coalesce in this slot.
                invalidate_cached_directory(&shared, &watched.path);
                let now = Instant::now();
                let due = (now + Duration::from_millis(50))
                    .max(last_refresh.map_or(now, |at| at + Duration::from_millis(200)));
                reconcile_due.get_or_insert(due);
                if !native_ready {
                    // Establish the fallback baseline before the scheduled reconciliation.
                    // Waiting a whole interval first could miss changes during that gap.
                    next_poll = interval.map(|_| now);
                }
            }
            Next::Deadline => {
                let now = Instant::now();
                if reconcile_due.is_some_and(|due| due <= now) {
                    reconcile_due = None;
                    if let (Some(watched), Some(request)) = (watched.as_ref(), request.as_ref()) {
                        if !state.cancelled(
                            &shared,
                            Lane::Directory,
                            watched.generation,
                            watched.generation,
                        ) && state.active.load(Ordering::Acquire)
                            && !state.scoped_search.load(Ordering::Acquire)
                        {
                            invalidate_cached_directory(&shared, &watched.path);
                            sender.send_replace(Some(Navigation {
                                refresh: true,
                                ..request.clone()
                            }));
                            last_refresh = Some(now);
                        }
                    }
                }
                if !native_ready && watched.is_some() && next_poll.is_some_and(|due| due <= now) {
                    next_poll = interval.map(|d| now + d);
                    let Some(request) = request.as_ref() else {
                        continue;
                    };
                    let Ok(permit) = shared.poll_slots.clone().acquire_owned().await else {
                        break;
                    };
                    let path = request.path.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        std::fs::metadata(path)
                            .and_then(|metadata| metadata.modified())
                            .ok()
                    })
                    .await
                    .ok()
                    .flatten();
                    if state.cancelled(
                        &shared,
                        Lane::Directory,
                        request.generation,
                        request.generation,
                    ) || !state.active.load(Ordering::Acquire)
                        || state.scoped_search.load(Ordering::Acquire)
                    {
                        continue;
                    }
                    // Initial fallback enumeration covers the gap before its first mtime
                    // baseline, including reactivation after a hidden tab was suspended.
                    if previous.is_none() {
                        invalidate_cached_directory(&shared, &request.path);
                        sender.send_replace(Some(Navigation {
                            refresh: true,
                            ..request.clone()
                        }));
                    }
                    if let Some((generation, old)) = previous {
                        if generation == request.generation && old != result {
                            state.filesystem_changed(request.generation, None);
                            invalidate_cached_directory(&shared, &request.path);
                            sender.send_replace(Some(Navigation {
                                refresh: true,
                                ..request.clone()
                            }));
                        }
                    }
                    previous = Some((request.generation, result));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Entry, scan::tests::Fixture};
    use std::time::Instant;

    #[test]
    fn filesystem_hints_keep_unacknowledged_children_bounded_and_scoped_to_navigation() {
        let mut changes = DataChanges::default();
        changes.record(7, 1, Some(&[OsString::from("folder")]));
        changes.record(7, 2, Some(&[OsString::from("NTUSER.DAT")]));
        assert_eq!(changes.since(7, 2, 0).1.unwrap().len(), 2);
        assert_eq!(
            changes.since(7, 2, 1).1.unwrap(),
            vec![OsString::from("NTUSER.DAT")]
        );
        assert!(changes.since(8, 2, 0).1.unwrap().is_empty());
        for revision in 3..=67 {
            changes.record(
                7,
                revision,
                Some(&[OsString::from(format!("child{revision}"))]),
            );
        }
        assert!(changes.names.len() <= 64);
        assert!(changes.bytes <= 64 * 1024);
        assert!(
            changes.since(7, 67, 0).1.is_none(),
            "lost history must require full invalidation"
        );
        changes.record(8, 68, Some(&[OsString::from("other-scope")]));
        assert_eq!(
            changes.since(8, 68, 0).1.unwrap(),
            vec![OsString::from("other-scope")]
        );
    }
    #[test]
    fn stale_navigation_notifications_cannot_advance_current_filesystem_revision() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                native_watch: false,
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let browser = engine.browser();
        browser.state.navigation.store(7, Ordering::Release);
        browser
            .state
            .filesystem_changed(6, Some(&[OsString::from("old")]));
        assert_eq!(browser.data_revision(), 0);
        browser
            .state
            .filesystem_changed(7, Some(&[OsString::from("current")]));
        assert_eq!(browser.data_revision(), 1);
        assert_eq!(
            browser.filesystem_changes_since(0).unwrap().1.unwrap(),
            vec![OsString::from("current")]
        );
        browser.cancel_filter();
        browser.set_active(false);
        browser.set_active(true);
        assert_eq!(browser.data_revision(), 1);
        browser.close();
    }

    #[cfg(windows)]
    #[test]
    fn native_data_revision_ignores_ready_and_detects_same_size_preserved_timestamp_writes() {
        let fixture = Fixture::new();
        let path = fixture.path.join("same.txt");
        std::fs::write(&path, b"aaaa").unwrap();
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_792_454_400);
        let preserve_time = || {
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap()
        };
        preserve_time();
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                native_watch: true,
                poll_interval: None,
                ..Default::default()
            },
        )
        .unwrap();
        let mut browser = engine.browser();
        let generation = browser.navigate(fixture.path.clone(), true);
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut ready = false;
        let mut listed = false;
        while !(ready && listed) {
            assert!(Instant::now() < deadline);
            match browser.try_recv() {
                Some(Event::WatchStatus {
                    generation: g,
                    coverage: WatchCoverage::NativeDirectoryChanges { .. },
                }) if g == generation => ready = true,
                Some(Event::Directory {
                    generation: g,
                    done: true,
                    ..
                }) if g == generation => listed = true,
                _ => thread::sleep(Duration::from_millis(2)),
            }
        }
        assert_eq!(
            browser.data_revision(),
            0,
            "watch readiness is not a data change"
        );
        std::fs::write(&path, b"bbbb").unwrap();
        preserve_time();
        while browser.data_revision() == 0 {
            assert!(
                Instant::now() < deadline,
                "same-size same-second write was lost"
            );
            thread::sleep(Duration::from_millis(2));
        }
        let (_, names) = loop {
            if let Some(changes) = browser.filesystem_changes_since(0) {
                break changes;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        assert_eq!(names.unwrap(), vec![OsString::from("same.txt")]);
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.len(), 4);
        assert_eq!(metadata.modified().unwrap(), modified);
        browser.close();
    }

    fn snapshot(name: &str) -> Arc<Snapshot> {
        Arc::new(Snapshot::from_entries(
            PathBuf::from(name),
            vec![Entry::from_name("item", false, 0, 0)],
        ))
    }

    #[test]
    fn unchanged_models_share_identity_but_metadata_changes_do_not() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let original = snapshot("shared-model-fixture");
        let registered =
            share_unchanged_model(&engine.shared, original.clone(), &|| false).unwrap();
        assert!(Arc::ptr_eq(&registered, &original));
        let duplicate = snapshot("shared-model-fixture");
        let reused = share_unchanged_model(&engine.shared, duplicate, &|| false).unwrap();
        assert!(Arc::ptr_eq(&reused, &original));

        let mutations: [fn(&mut Entry); 6] = [
            |entry| entry.size = 1,
            |entry| entry.modified_secs = 1,
            |entry| entry.created_secs = Some(1),
            |entry| entry.accessed_secs = Some(1),
            |entry| entry.hidden = true,
            |entry| entry.metadata_known = false,
        ];
        for mutate in mutations {
            share_unchanged_model(&engine.shared, original.clone(), &|| false).unwrap();
            let mut entry = original.entries[0].clone();
            mutate(&mut entry);
            let changed = Arc::new(Snapshot::from_entries(original.path.clone(), vec![entry]));
            let result = share_unchanged_model(&engine.shared, changed.clone(), &|| false).unwrap();
            assert!(Arc::ptr_eq(&result, &changed));
            assert!(!Arc::ptr_eq(&result, &original));
        }
        share_unchanged_model(&engine.shared, original.clone(), &|| false).unwrap();
        for recursive in [false, true] {
            let mut partial =
                Snapshot::from_entries(original.path.clone(), vec![original.entries[0].clone()]);
            partial.complete = recursive;
            partial.recursive = recursive;
            let partial = Arc::new(partial);
            let result = share_unchanged_model(&engine.shared, partial.clone(), &|| false).unwrap();
            assert!(Arc::ptr_eq(&result, &partial));
        }
    }

    #[test]
    fn unchanged_model_comparison_cancels_before_replacing_identity() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let entries: Vec<_> = (0..1024)
            .map(|i| Entry::from_name(format!("file{i}"), false, i, 0))
            .collect();
        let original = Arc::new(Snapshot::from_entries(
            PathBuf::from("cancel-model-sharing-fixture"),
            entries.clone(),
        ));
        share_unchanged_model(&engine.shared, original.clone(), &|| false).unwrap();
        let duplicate = Arc::new(Snapshot::from_entries(original.path.clone(), entries));
        let checks = std::cell::Cell::new(0);
        let result = share_unchanged_model(&engine.shared, duplicate, &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 4
        });
        assert!(result.is_none());
        let registered = engine.shared.models.lock().unwrap()[&original.path]
            .upgrade()
            .unwrap();
        assert!(Arc::ptr_eq(&registered, &original));
    }

    #[test]
    fn model_sharing_rechecks_a_concurrent_replacement_before_inserting() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let original = snapshot("concurrent-model-sharing-fixture");
        share_unchanged_model(&engine.shared, original.clone(), &|| false).unwrap();
        let make_changed = || {
            Arc::new(Snapshot::from_entries(
                original.path.clone(),
                vec![Entry::from_name("item", false, 1, 0)],
            ))
        };
        let candidate = make_changed();
        let concurrent = make_changed();
        let checks = std::cell::Cell::new(0);
        let result = share_unchanged_model(&engine.shared, candidate, &|| {
            checks.set(checks.get() + 1);
            if checks.get() == 4 {
                // Publish after the failed entry comparison but before insertion,
                // without depending on OS thread scheduling to reproduce the race.
                let other =
                    share_unchanged_model(&engine.shared, concurrent.clone(), &|| false).unwrap();
                assert!(Arc::ptr_eq(&other, &concurrent));
            }
            false
        })
        .unwrap();
        assert!(Arc::ptr_eq(&result, &concurrent));
    }

    #[test]
    fn concurrent_navigation_workers_share_identical_complete_models() {
        let fixture = Fixture::new();
        for i in 0..8 {
            fixture.file(&format!("item{i}"));
        }
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                native_watch: false,
                poll_interval: None,
                ..EngineOptions::default()
            },
        )
        .unwrap();
        let mut first = engine.browser();
        let mut second = engine.browser();
        let first_generation = first.navigate(fixture.path.clone(), true);
        let second_generation = second.navigate(fixture.path.clone(), true);
        let mut snapshots = [None, None];
        let deadline = Instant::now() + Duration::from_secs(5);
        while snapshots.iter().any(Option::is_none) {
            assert!(Instant::now() < deadline, "shared navigation timed out");
            for (slot, (browser, generation)) in [
                (&mut first, first_generation),
                (&mut second, second_generation),
            ]
            .into_iter()
            .enumerate()
            {
                if let Some(Event::Directory {
                    generation: actual,
                    snapshot,
                    done: true,
                    ..
                }) = browser.try_recv()
                {
                    assert_eq!(actual, generation);
                    assert!(snapshot.complete);
                    snapshots[slot] = Some(snapshot);
                }
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(Arc::ptr_eq(
            snapshots[0].as_ref().unwrap(),
            snapshots[1].as_ref().unwrap()
        ));
    }

    #[test]
    fn deferred_pane_models_are_destroyed_on_the_retirement_thread() {
        struct Probe(std::sync::mpsc::Sender<String>);
        impl Drop for Probe {
            fn drop(&mut self) {
                let _ = self
                    .0
                    .send(thread::current().name().unwrap_or("unnamed").to_owned());
            }
        }
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(engine.defer(Probe(tx)).is_ok());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            "foldex-retirement"
        );
    }

    #[test]
    fn cache_evicts_by_bytes_and_access_order() {
        let a = snapshot("a");
        let b = snapshot("b");
        let c = snapshot("c");
        let charge = a.managed_bytes + 4;
        let mut cache = DirectoryCache::new(charge * 2 + 8, 8);
        cache.insert(a);
        cache.insert(b);
        assert!(cache.get(&PathBuf::from("a")).is_some());
        cache.insert(c);
        assert!(cache.get(&PathBuf::from("b")).is_none());
        assert!(cache.get(&PathBuf::from("a")).is_some());
        assert!(cache.bytes <= cache.max_bytes);
    }

    #[test]
    fn rapid_navigation_and_queries_never_deliver_stale_results() {
        let fixture_a = Fixture::new();
        fixture_a.file("old.txt");
        let fixture_b = Fixture::new();
        fixture_b.file("new.txt");
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                poll_interval: None,
                ..EngineOptions::default()
            },
        )
        .unwrap();
        let mut browser = engine.browser();
        browser.navigate(fixture_a.path.clone(), false);
        let latest = browser.navigate(fixture_b.path.clone(), false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let snapshot = loop {
            assert!(Instant::now() < deadline, "navigation timed out");
            if let Some(Event::Directory {
                generation,
                snapshot,
                ..
            }) = browser.try_recv()
            {
                assert_eq!(generation, latest);
                if snapshot.complete {
                    break snapshot;
                }
            }
            thread::sleep(Duration::from_millis(2));
        };
        browser.filter(
            snapshot.clone(),
            Query {
                text: "old".into(),
                ..Query::default()
            },
        );
        let last_query = browser.filter(
            snapshot,
            Query {
                text: "new".into(),
                ..Query::default()
            },
        );
        loop {
            assert!(Instant::now() < deadline, "query timed out");
            if let Some(Event::View {
                generation,
                indices,
                ..
            }) = browser.try_recv()
            {
                assert_eq!(generation, last_query);
                assert_eq!(indices.len(), 1);
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn cancelling_scan_rejects_already_published_progress() {
        let fixture = Fixture::new();
        for i in 0..300 {
            fixture.file(&format!("item{i}"));
        }
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                poll_interval: None,
                ..EngineOptions::default()
            },
        )
        .unwrap();
        let mut browser = engine.browser();
        browser.scan(fixture.path.clone());
        thread::sleep(Duration::from_millis(30));
        browser.cancel_scan();
        assert!(!matches!(
            browser.try_recv(),
            Some(Event::ScanProgress { .. })
        ));
    }

    #[test]
    fn global_budget_counts_snapshots_pinned_outside_cache() {
        const ENTRIES: usize = 1500;
        const DIRECTORY_BYTES: usize = 4 * 1024 * 1024;
        let first = Fixture::new();
        let second = Fixture::new();
        for i in 0..ENTRIES {
            first.file(&format!("first{i}"));
            second.file(&format!("second{i}"));
        }
        // Preserve the original fixture's headroom while charging the two new
        // optional timestamps for every entry and the reserved pending chunk.
        // Round the additional allowance to 64 KiB so reservation granularity
        // cannot erase that headroom. This needs no extra calibration scans.
        let timestamp_allowance =
            ((ENTRIES + crate::model::CHUNK_LEN) * 2 * std::mem::size_of::<Option<i64>>())
                .next_multiple_of(64 * 1024);
        let budget = 512 * 1024 + timestamp_allowance;
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                // Isolate external snapshot pins from cache ownership and
                // unrelated watcher reconciliation in this accounting test.
                native_watch: false,
                poll_interval: None,
                cache_bytes: 0,
                cache_directories: 0,
                managed_bytes: budget,
                directory_bytes: DIRECTORY_BYTES,
                ..EngineOptions::default()
            },
        )
        .unwrap();
        let mut browser = engine.browser();
        let first_generation = browser.navigate(first.path.clone(), true);
        let deadline = Instant::now() + Duration::from_secs(10);
        let first_snapshot = loop {
            assert!(Instant::now() < deadline);
            assert!(engine.managed_bytes() <= budget);
            if let Some(Event::Directory {
                generation,
                snapshot,
                done: true,
                ..
            }) = browser.try_recv()
            {
                assert_eq!(generation, first_generation);
                break snapshot;
            }
            thread::sleep(Duration::from_millis(2));
        };
        assert!(
            first_snapshot.complete,
            "one complete fixture must fit: budget={budget}, timestamp_allowance={timestamp_allowance}, entry_bytes={}",
            std::mem::size_of::<Entry>()
        );
        assert_eq!(first_snapshot.entries.len(), ENTRIES);
        let first_charge = engine.managed_bytes();
        assert!(first_charge > budget / 2);
        assert!(engine.shared.cache.lock().unwrap().entries.is_empty());
        let second_generation = browser.navigate(second.path.clone(), true);
        let deadline = Instant::now() + Duration::from_secs(10);
        let second_snapshot = loop {
            assert!(Instant::now() < deadline);
            assert!(engine.managed_bytes() <= budget);
            if let Some(Event::Directory {
                generation,
                snapshot,
                done: true,
                ..
            }) = browser.try_recv()
            {
                assert_eq!(generation, second_generation);
                break snapshot;
            }
            thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(
            second_snapshot.limit,
            Some(crate::LimitReason::MemoryBudget)
        );
        assert!(!second_snapshot.complete);
        assert!(second_snapshot.shared_memory_pressure);
        assert!(!first_snapshot.entries.is_empty());
        assert!(engine.managed_bytes() <= budget);

        // Releasing the external pins and the latest result must return their
        // reservations. Then the formerly blocked directory fits completely.
        let first_weak = Arc::downgrade(&first_snapshot);
        let second_weak = Arc::downgrade(&second_snapshot);
        drop(first_snapshot);
        drop(second_snapshot);
        browser.cancel_navigation();
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.managed_bytes() != 0 {
            assert!(
                Instant::now() < deadline,
                "unpinned reservations were retained"
            );
            assert!(engine.managed_bytes() <= budget);
            thread::sleep(Duration::from_millis(2));
        }
        assert!(first_weak.upgrade().is_none());
        assert!(second_weak.upgrade().is_none());
        let recovered_generation = browser.navigate(second.path.clone(), true);
        let deadline = Instant::now() + Duration::from_secs(10);
        let recovered = loop {
            assert!(
                Instant::now() < deadline,
                "unpinned directory did not recover"
            );
            assert!(engine.managed_bytes() <= budget);
            if let Some(Event::Directory {
                generation,
                snapshot,
                done: true,
                ..
            }) = browser.try_recv()
            {
                assert_eq!(generation, recovered_generation);
                break snapshot;
            }
            thread::sleep(Duration::from_millis(2));
        };
        assert!(recovered.complete);
        assert_eq!(recovered.entries.len(), ENTRIES);
        assert_eq!(recovered.limit, None);
        assert!(!recovered.shared_memory_pressure);
        assert!(engine.managed_bytes() <= budget);
        assert!(first_charge + engine.managed_bytes() > budget);
    }

    #[test]
    fn invalidation_removes_a_cached_directory_before_reconciliation() {
        let mut cache = DirectoryCache::new(1024 * 1024, 8);
        let snapshot = snapshot("owned-cache-fixture");
        cache.insert(snapshot.clone());
        assert!(cache.bytes > 0);
        assert!(cache.get(&snapshot.path).is_some());
        cache.invalidate(&snapshot.path);
        assert!(cache.get(&snapshot.path).is_none());
        assert_eq!(cache.bytes, 0);
    }

    #[test]
    fn view_event_restores_selection_without_retaining_previous_model_at_idle() {
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                native_watch: false,
                poll_interval: None,
                ..EngineOptions::default()
            },
        )
        .unwrap();
        let mut browser = engine.browser();
        let path = PathBuf::from("owned-restore-event-fixture");
        let old = Arc::new(Snapshot::from_entries(
            path.clone(),
            vec![
                Entry::from_name("a", false, 1, 0),
                Entry::from_name("c", false, 2, 0),
            ],
        ));
        let old_weak = Arc::downgrade(&old);
        let query = Query {
            show_hidden: true,
            ..Query::default()
        };
        let restore = ViewRestore {
            snapshot: old.clone(),
            indices: crate::filter_snapshot(old.clone(), &query).indices,
            selected: vec![(1, 1)],
            focused: Some(1),
            target: None,
        };
        drop(old);
        let new = Arc::new(Snapshot::from_entries(
            path,
            vec![
                Entry::from_name("0-new", false, 0, 0),
                Entry::from_name("a", false, 1, 0),
                Entry::from_name("c", false, 2, 0),
            ],
        ));
        let generation = browser.filter_with_restore(new, query, Some(restore));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "restored view event timed out");
            if let Some(Event::View {
                generation: actual,
                restored,
                ..
            }) = browser.try_recv()
            {
                assert_eq!(actual, generation);
                let restored = restored.expect("filter_with_restore must publish its restoration");
                assert_eq!(restored.selected, vec![(2, 2)]);
                assert_eq!(restored.focused, Some(2));
                assert!(!restored.truncated);
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            old_weak.upgrade().is_none(),
            "the latest request slot must consume its old model"
        );
        assert_eq!(engine.shared.memory.used(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn native_freshness_reconciles_in_place_size_change_without_polling() {
        let fixture = Fixture::new();
        fixture.file("existing.txt");
        let engine = Engine::with_options(
            Arc::new(|| {}),
            EngineOptions {
                native_watch: true,
                poll_interval: None,
                ..EngineOptions::default()
            },
        )
        .unwrap();
        let mut browser = engine.browser();
        let generation = browser.navigate(fixture.path.clone(), true);
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut ready = false;
        let mut listed = false;
        while !(ready && listed) {
            assert!(
                Instant::now() < deadline,
                "native watch/listing did not become ready"
            );
            match browser.try_recv() {
                Some(Event::WatchStatus {
                    generation: g,
                    coverage: WatchCoverage::NativeDirectoryChanges { .. },
                }) if g == generation => ready = true,
                Some(Event::Directory {
                    generation: g,
                    done: true,
                    ..
                }) if g == generation => listed = true,
                _ => thread::sleep(Duration::from_millis(3)),
            }
        }
        let contents = b"an in-place replacement longer than the original fixture";
        std::fs::write(fixture.path.join("existing.txt"), contents).unwrap();
        loop {
            assert!(
                Instant::now() < deadline,
                "native notification did not reconcile changed size"
            );
            if let Some(Event::Directory {
                generation: g,
                snapshot,
                done: true,
                ..
            }) = browser.try_recv()
            {
                assert_eq!(g, generation);
                if snapshot
                    .entries
                    .get(0)
                    .is_some_and(|entry| entry.size == contents.len() as u64)
                {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(3));
        }
        browser.close();
    }
}
