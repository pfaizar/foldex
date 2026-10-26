//! Visible-folder recursive statistics. One low-priority worker multiplexes
//! bounded DFS walks; frame-facing methods never touch the filesystem.

use std::{
    collections::{HashMap, VecDeque},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

mod native;
mod scan;
use scan::Walk;

// View subscriptions are cheap; active DFS walks retain their independent bound.
pub const MAX_VIEWS: usize = 32;
pub const MAX_REQUESTS_PER_VIEW: usize = 32;
pub const MAX_ACTIVE_WALKS: usize = 4;
pub const MAX_DEPTH: usize = 32;
pub const MAX_ENTRIES: u64 = 1_000_000;
pub const REQUEST_PATH_BUDGET: usize = 512 * 1024;
pub const CACHE_PATH_BUDGET: usize = 512 * 1024;
pub const WALK_PATH_BUDGET: usize = 128 * 1024;
pub const MAX_CACHE_ENTRIES: usize = 64;
pub const MAX_EVENTS: usize = MAX_VIEWS * MAX_REQUESTS_PER_VIEW;
const PARTIAL_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FolderRequest {
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum State {
    #[default]
    Scanning,
    /// Finished the permitted tree without an observed error or resource limit.
    Complete,
    /// Finished with excluded branches, an error, overflow or a resource limit.
    Partial,
    /// Root could not be opened or was excluded by the default locality policy.
    Unavailable,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Issues {
    pub unreadable: u64,
    pub skipped_reparse: u64,
    pub skipped_offline: u64,
    pub skipped_nonlocal: u64,
    pub depth_limited: u64,
    pub path_limited: u64,
    pub entry_limited: u64,
    pub overflow: u64,
}
impl Issues {
    fn any(&self) -> bool {
        self.unreadable != 0
            || self.skipped_reparse != 0
            || self.skipped_offline != 0
            || self.skipped_nonlocal != 0
            || self.depth_limited != 0
            || self.path_limited != 0
            || self.entry_limited != 0
            || self.overflow != 0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FolderSnapshot {
    /// Descendant directories; excludes the requested root and excluded links.
    pub folders: u64,
    pub files: u64,
    /// Logical file lengths. Does not include directory allocation or link targets.
    pub bytes: u64,
    pub visited: u64,
    pub state: State,
    pub issues: Issues,
    /// A bounded root error/policy explanation. Descendant failures use counters.
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct FolderEvent {
    pub view: u64,
    pub epoch: u64,
    pub path: Arc<PathBuf>,
    pub stats: Arc<FolderSnapshot>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Metrics {
    pub walks_started: u64,
    pub invalidations: u64,
    pub active_walks: usize,
    pub open_handles: usize,
    pub traversal_path_bytes: usize,
    pub cached_entries: usize,
    pub cached_path_bytes: usize,
    pub event_slots: usize,
}

pub struct FolderStatsService {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl FolderStatsService {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let child = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("foldex-folder-stats".into())
            .spawn(move || worker(child, wake))?;
        let _ = shared.thread.set(worker.thread().clone());
        shared.notify();
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }

    /// Complete visible set, in visual priority order. At most 32 subscribed views and
    /// 32 folders/view. A higher epoch explicitly refreshes cached results for
    /// this view, including folders first requested later in that epoch.
    /// Same-epoch viewport changes retain matching in-flight work. Busy retries
    /// do not clone paths. A smaller epoch is ignored.
    pub fn set_requests(
        &self,
        view: u64,
        epoch: u64,
        requests: &[FolderRequest],
    ) -> io::Result<()> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "folder statistics worker closed",
            ));
        }
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        if requests.len() > MAX_REQUESTS_PER_VIEW {
            return Err(invalid("at most 32 visible folders per view"));
        }
        let existing = state.views.iter().position(|entry| entry.id == view);
        if let Some(index) = existing {
            let old = &state.views[index];
            if epoch < old.epoch
                || (epoch == old.epoch
                    && old.requests.len() == requests.len()
                    && old
                        .requests
                        .iter()
                        .zip(requests)
                        .all(|(old, new)| *old.path == new.path))
            {
                return Ok(());
            }
        } else if state.views.len() == MAX_VIEWS {
            return Err(busy());
        }
        let mut unique = Vec::<&PathBuf>::with_capacity(requests.len());
        let mut weight = 0usize;
        for request in requests {
            validate_path(&request.path)?;
            if !unique.contains(&&request.path) {
                weight = weight.saturating_add(path_bytes(&request.path));
                unique.push(&request.path);
            }
        }
        let other_weight: usize = state
            .views
            .iter()
            .filter(|entry| entry.id != view)
            .map(|entry| entry.weight)
            .sum();
        if weight.saturating_add(other_weight) > REQUEST_PATH_BUDGET {
            return Err(invalid("visible folder paths exceed 512 KiB"));
        }
        let same_epoch = existing.is_some_and(|index| state.views[index].epoch == epoch);
        if same_epoch {
            let old = &state.views[existing.unwrap()];
            if old.requests.len() == unique.len()
                && old
                    .requests
                    .iter()
                    .zip(&unique)
                    .all(|(old, path)| *old.path == **path)
            {
                return Ok(());
            }
        }
        let floor = if same_epoch {
            state.views[existing.unwrap()].floor
        } else if existing.is_some() {
            state.sequence()
        } else {
            0
        };
        let mut old = existing.map(|index| state.views.remove(index));
        let mut next = Vec::with_capacity(unique.len());
        for path in unique {
            let retained = if same_epoch {
                old.as_mut().and_then(|old| {
                    old.requests
                        .iter()
                        .position(|entry| *entry.path == *path)
                        .map(|index| old.requests.swap_remove(index))
                })
            } else {
                None
            };
            next.push(retained.unwrap_or_else(|| Request {
                path: Arc::new(path.clone()),
                token: state.sequence(),
                floor,
                stage: Stage::Queued,
            }));
        }
        if existing.is_some() && !same_epoch {
            if !next.is_empty() {
                self.shared
                    .metrics
                    .invalidations
                    .fetch_add(1, Ordering::Relaxed);
            }
            for request in &next {
                state.invalidate(Invalidation::scope(Arc::clone(&request.path)));
            }
        }
        state.views.push(View {
            id: view,
            epoch,
            floor,
            requests: next,
            weight,
        });
        drop(state);
        self.shared.changed();
        Ok(())
    }

    pub fn cancel_view(&self, view: u64) -> io::Result<()> {
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        if !state.views.iter().any(|entry| entry.id == view) {
            return Ok(());
        }
        state.views.retain(|entry| entry.id != view);
        drop(state);
        self.shared.changed();
        Ok(())
    }

    /// Invalidate a changed filesystem scope without inventing a new view epoch.
    /// The worker retires descendant trees and ancestor totals that include the
    /// changed scope. Use this only for real source changes or an explicit
    /// refresh, never for repaint,
    /// selection, result-window publication or acquiring a view lease.
    pub fn invalidate_scope(&self, scope: &PathBuf) -> io::Result<()> {
        self.invalidate(scope, true)
    }

    /// Renew a requested tree whose cached identity is unknown or changed.
    /// Unlike a filesystem change, acquiring a child subscription does not
    /// invalidate ancestor totals or restart an ancestor walk.
    pub fn invalidate_tree(&self, root: &PathBuf) -> io::Result<()> {
        self.invalidate(root, false)
    }

    fn invalidate(&self, path: &PathBuf, ancestors: bool) -> io::Result<()> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "folder statistics worker closed",
            ));
        }
        validate_path(path)?;
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        let invalidation = Invalidation {
            path: Arc::new(path.clone()),
            ancestors,
        };
        // Reject already queued old events immediately, before the coordinator
        // consumes them against its new path-to-row mapping.
        state.reset_paths(false, std::slice::from_ref(&invalidation));
        state.invalidate(invalidation);
        state.retire_events();
        self.shared
            .metrics
            .invalidations
            .fetch_add(1, Ordering::Relaxed);
        drop(state);
        self.shared.changed();
        Ok(())
    }

    /// Returns only current request generations. Stale event retirement and all
    /// traversal/handle destruction remain on the worker.
    pub fn try_recv(&self) -> Option<FolderEvent> {
        let mut state = self.shared.state.try_lock().ok()?;
        let index = state
            .events
            .iter()
            .position(|event| state.current(&event.subscriber, &event.event.path))?;
        let event = state.events.remove(index)?.event;
        self.shared
            .metrics
            .event_slots
            .store(state.events.len(), Ordering::Relaxed);
        Some(event)
    }

    pub fn metrics(&self) -> Metrics {
        self.shared.metrics.load()
    }
    pub fn shutdown(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.notify();
    }
    pub fn is_worker_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
}
impl Drop for FolderStatsService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Queued,
    Active,
    Done,
}
struct Request {
    path: Arc<PathBuf>,
    token: u64,
    floor: u64,
    stage: Stage,
}
struct View {
    id: u64,
    epoch: u64,
    floor: u64,
    requests: Vec<Request>,
    weight: usize,
}
#[derive(Clone, Copy)]
struct Subscriber {
    view: u64,
    epoch: u64,
    token: u64,
}
struct Envelope {
    subscriber: Subscriber,
    event: FolderEvent,
}
struct Invalidation {
    path: Arc<PathBuf>,
    ancestors: bool,
}
impl Invalidation {
    fn scope(path: Arc<PathBuf>) -> Self {
        Self {
            path,
            ancestors: true,
        }
    }
    fn matches(&self, path: &std::path::Path) -> bool {
        path.starts_with(self.path.as_path()) || (self.ancestors && self.path.starts_with(path))
    }
}
#[derive(Default)]
struct Mailbox {
    sequence: u64,
    views: Vec<View>,
    events: VecDeque<Envelope>,
    next_view: usize,
    idle: bool,
    invalidations: Vec<Invalidation>,
    invalidation_bytes: usize,
    clear_cache: bool,
}
impl Mailbox {
    fn invalidate(&mut self, invalidation: Invalidation) {
        if self.clear_cache {
            return;
        }
        if let Some(existing) = self
            .invalidations
            .iter_mut()
            .find(|old| old.path == invalidation.path)
        {
            existing.ancestors |= invalidation.ancestors;
            return;
        }
        let weight = path_bytes(&invalidation.path);
        if self.invalidations.len() == MAX_EVENTS
            || self.invalidation_bytes.saturating_add(weight) > REQUEST_PATH_BUDGET
        {
            self.invalidations.clear();
            self.invalidation_bytes = 0;
            self.clear_cache = true;
        } else {
            self.invalidation_bytes += weight;
            self.invalidations.push(invalidation);
        }
    }
    fn reset_paths(&mut self, all: bool, paths: &[Invalidation]) {
        if !all && paths.is_empty() {
            return;
        }
        let floor = self.sequence();
        for view in &mut self.views {
            for request in &mut view.requests {
                if all
                    || paths
                        .iter()
                        .any(|invalidation| invalidation.matches(&request.path))
                {
                    self.sequence = self.sequence.wrapping_add(1).max(1);
                    request.token = self.sequence;
                    request.floor = request.floor.max(floor);
                    request.stage = Stage::Queued;
                }
            }
        }
    }
    fn sequence(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1).max(1);
        self.sequence
    }
    fn current(&self, subscriber: &Subscriber, path: &PathBuf) -> bool {
        self.views.iter().any(|view| {
            view.id == subscriber.view
                && view.epoch == subscriber.epoch
                && view
                    .requests
                    .iter()
                    .any(|request| request.token == subscriber.token && **request.path == *path)
        })
    }
    fn retire_events(&mut self) {
        let mut events = std::mem::take(&mut self.events);
        events.retain(|event| self.current(&event.subscriber, &event.event.path));
        self.events = events;
    }
    fn candidate(&mut self) -> Option<(Arc<PathBuf>, u64)> {
        // A request can arrive after the worker took its invalidation batch.
        // Do not attach it to an old active/cache generation before that batch
        // is applied at the top of the next worker turn.
        if self.clear_cache || !self.invalidations.is_empty() {
            return None;
        }
        for offset in 0..self.views.len() {
            let index = (self.next_view + offset) % self.views.len();
            if let Some(request) = self.views[index]
                .requests
                .iter()
                .find(|request| request.stage == Stage::Queued)
            {
                let path = Arc::clone(&request.path);
                let floor = self
                    .views
                    .iter()
                    .flat_map(|view| &view.requests)
                    .filter(|request| request.path == path)
                    .map(|request| request.floor)
                    .max()
                    .unwrap_or(0);
                self.next_view = (index + 1) % self.views.len();
                return Some((path, floor));
            }
        }
        None
    }
    fn claim(&mut self, path: &Arc<PathBuf>, fresh: bool) -> Vec<Subscriber> {
        let mut subscribers = Vec::with_capacity(MAX_VIEWS);
        for view in &mut self.views {
            for request in &mut view.requests {
                if request.path == *path && (fresh || request.stage == Stage::Queued) {
                    if fresh {
                        self.sequence = self.sequence.wrapping_add(1).max(1);
                        request.token = self.sequence;
                    }
                    request.stage = Stage::Active;
                    subscribers.push(Subscriber {
                        view: view.id,
                        epoch: view.epoch,
                        token: request.token,
                    });
                }
            }
        }
        subscribers
    }
    fn requeue(&mut self, path: &Arc<PathBuf>, subscribers: &[Subscriber]) {
        for view in &mut self.views {
            for request in &mut view.requests {
                if request.path == *path
                    && subscribers.iter().any(|subscriber| {
                        subscriber.view == view.id
                            && subscriber.epoch == view.epoch
                            && subscriber.token == request.token
                    })
                {
                    request.stage = Stage::Queued;
                }
            }
        }
    }
    fn publish(
        &mut self,
        path: &Arc<PathBuf>,
        subscribers: &[Subscriber],
        stats: Arc<FolderSnapshot>,
        done: bool,
    ) -> bool {
        let mut published = false;
        for subscriber in subscribers {
            if !self.current(subscriber, path) {
                continue;
            }
            self.events.retain(|event| {
                !(event.subscriber.view == subscriber.view
                    && event.subscriber.token == subscriber.token)
            });
            if self.events.len() == MAX_EVENTS {
                self.events.pop_front();
            }
            self.events.push_back(Envelope {
                subscriber: *subscriber,
                event: FolderEvent {
                    view: subscriber.view,
                    epoch: subscriber.epoch,
                    path: Arc::clone(path),
                    stats: Arc::clone(&stats),
                },
            });
            if done {
                if let Some(request) = self
                    .views
                    .iter_mut()
                    .find(|view| view.id == subscriber.view)
                    .and_then(|view| {
                        view.requests
                            .iter_mut()
                            .find(|request| request.token == subscriber.token)
                    })
                {
                    request.stage = Stage::Done;
                }
            }
            published = true;
        }
        published
    }
}

#[derive(Default)]
struct MetricAtoms {
    walks_started: AtomicU64,
    invalidations: AtomicU64,
    active_walks: AtomicUsize,
    open_handles: AtomicUsize,
    traversal_path_bytes: AtomicUsize,
    cached_entries: AtomicUsize,
    cached_path_bytes: AtomicUsize,
    event_slots: AtomicUsize,
}
impl MetricAtoms {
    fn load(&self) -> Metrics {
        Metrics {
            walks_started: self.walks_started.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
            active_walks: self.active_walks.load(Ordering::Relaxed),
            open_handles: self.open_handles.load(Ordering::Relaxed),
            traversal_path_bytes: self.traversal_path_bytes.load(Ordering::Relaxed),
            cached_entries: self.cached_entries.load(Ordering::Relaxed),
            cached_path_bytes: self.cached_path_bytes.load(Ordering::Relaxed),
            event_slots: self.event_slots.load(Ordering::Relaxed),
        }
    }
}
#[derive(Default)]
struct Shared {
    state: Mutex<Mailbox>,
    revision: AtomicU64,
    closed: AtomicBool,
    thread: OnceLock<Thread>,
    metrics: MetricAtoms,
}
impl Shared {
    fn notify(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
    fn changed(&self) {
        self.revision.fetch_add(1, Ordering::Release);
        self.notify();
    }
}
struct Active {
    path: Arc<PathBuf>,
    subscribers: Vec<Subscriber>,
    revision: u64,
    walk: Walk,
    last_publish: Option<Instant>,
}
struct Cached {
    revision: u64,
    stats: Arc<FolderSnapshot>,
    used: u64,
    weight: usize,
}
#[derive(Default)]
struct Cache {
    values: HashMap<Arc<PathBuf>, Cached>,
    bytes: usize,
    tick: u64,
}
impl Cache {
    fn get(&mut self, path: &Arc<PathBuf>, revision: u64) -> Option<Arc<FolderSnapshot>> {
        let entry = self.values.get_mut(path)?;
        if entry.revision < revision {
            return None;
        }
        self.tick = self.tick.wrapping_add(1);
        entry.used = self.tick;
        Some(Arc::clone(&entry.stats))
    }
    fn insert(&mut self, path: Arc<PathBuf>, revision: u64, stats: Arc<FolderSnapshot>) {
        if let Some(old) = self.values.remove(&path) {
            self.bytes -= old.weight;
        }
        let weight = path_bytes(&path);
        if weight > CACHE_PATH_BUDGET {
            return;
        }
        while self.values.len() >= MAX_CACHE_ENTRIES
            || self.bytes.saturating_add(weight) > CACHE_PATH_BUDGET
        {
            let Some(oldest) = self
                .values
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(path, _)| Arc::clone(path))
            else {
                break;
            };
            self.bytes -= self.values.remove(&oldest).unwrap().weight;
        }
        self.tick = self.tick.wrapping_add(1);
        self.bytes += weight;
        self.values.insert(
            path,
            Cached {
                revision,
                stats,
                used: self.tick,
                weight,
            },
        );
    }
}

fn worker(shared: Arc<Shared>, wake: Arc<dyn Fn() + Send + Sync>) {
    native::background_priority();
    let mut active = Vec::<Active>::with_capacity(MAX_ACTIVE_WALKS);
    let mut cache = Cache::default();
    loop {
        if shared.closed.load(Ordering::Acquire) {
            break;
        }
        let mut notify = false;
        let mut retired = Vec::new();
        let invalidations;
        let clear_cache;
        {
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.retire_events();
            state.idle = false;
            invalidations = std::mem::take(&mut state.invalidations);
            state.invalidation_bytes = 0;
            clear_cache = std::mem::take(&mut state.clear_cache);
            state.reset_paths(clear_cache, &invalidations);
            let mut index = 0;
            while index < active.len() {
                let job = &mut active[index];
                job.subscribers
                    .retain(|subscriber| state.current(subscriber, &job.path));
                if job.subscribers.is_empty()
                    || clear_cache
                    || invalidations
                        .iter()
                        .any(|invalidation| invalidation.matches(&job.path))
                {
                    retired.push(active.swap_remove(index));
                } else {
                    index += 1;
                }
            }
        }
        drop(retired); // Close native directory handles outside the UI mailbox.
        if clear_cache {
            cache.values.clear();
            cache.bytes = 0;
        } else {
            cache.values.retain(|path, _| {
                !invalidations
                    .iter()
                    .any(|invalidation| invalidation.matches(path))
            });
            cache.bytes = cache.values.values().map(|entry| entry.weight).sum();
        }
        // Attach new matching subscribers to an existing walk, or supersede a
        // walk that began before a newer explicit refresh floor.
        for _ in 0..MAX_EVENTS {
            // Keep candidate freshness and its claim in one mailbox operation.
            // A refresh between them must not attach new subscriber tokens to
            // an older cache result or in-flight tree. Native IO stays outside.
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some((path, floor)) = state.candidate() else {
                break;
            };
            if let Some(index) = active.iter().position(|job| job.path == path) {
                if active[index].revision < floor {
                    let old = active.remove(index);
                    state.requeue(&old.path, &old.subscribers);
                    drop(state);
                    drop(old);
                } else {
                    active[index].subscribers.extend(state.claim(&path, false));
                }
                continue;
            }
            if let Some(stats) = cache.get(&path, floor) {
                let subscribers = state.claim(&path, false);
                notify |= state.publish(&path, &subscribers, stats, true);
                continue;
            }
            if active.len() == MAX_ACTIVE_WALKS {
                break;
            }
            let revision = state.sequence();
            let subscribers = state.claim(&path, true);
            drop(state);
            shared.metrics.walks_started.fetch_add(1, Ordering::Relaxed);
            let walk = Walk::new(&path);
            let done = walk.finished();
            let stats = Arc::new(walk.snapshot().clone());
            {
                let mut state = shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                notify |= state.publish(&path, &subscribers, Arc::clone(&stats), done);
            }
            if done {
                cache.insert(path, revision, stats);
            } else {
                active.push(Active {
                    path,
                    subscribers,
                    revision,
                    walk,
                    last_publish: None,
                });
            }
        }
        let observed_revision = shared.revision.load(Ordering::Acquire);
        let current = || {
            !shared.closed.load(Ordering::Acquire)
                && shared.revision.load(Ordering::Acquire) == observed_revision
        };
        for job in &mut active {
            if !current() {
                break;
            }
            job.walk.advance(&current);
            let done = job.walk.finished();
            if done
                || job
                    .last_publish
                    .is_none_or(|time| time.elapsed() >= PARTIAL_INTERVAL)
            {
                let stats = Arc::new(job.walk.snapshot().clone());
                notify |= shared
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .publish(&job.path, &job.subscribers, Arc::clone(&stats), done);
                job.last_publish = Some(Instant::now());
                if done {
                    cache.insert(Arc::clone(&job.path), job.revision, stats);
                }
            }
        }
        active.retain(|job| !job.walk.finished());
        shared
            .metrics
            .active_walks
            .store(active.len(), Ordering::Relaxed);
        shared.metrics.open_handles.store(
            active.iter().map(|job| job.walk.handles()).sum(),
            Ordering::Relaxed,
        );
        shared.metrics.traversal_path_bytes.store(
            active.iter().map(|job| job.walk.path_bytes()).sum(),
            Ordering::Relaxed,
        );
        shared
            .metrics
            .cached_entries
            .store(cache.values.len(), Ordering::Relaxed);
        shared
            .metrics
            .cached_path_bytes
            .store(cache.bytes, Ordering::Relaxed);
        let (has_queued, events) = {
            let state = shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (
                state.clear_cache
                    || !state.invalidations.is_empty()
                    || state.views.iter().any(|view| {
                        view.requests
                            .iter()
                            .any(|request| request.stage == Stage::Queued)
                    }),
                state.events.len(),
            )
        };
        shared.metrics.event_slots.store(events, Ordering::Relaxed);
        if notify {
            wake();
        }
        if active.is_empty() && !has_queued {
            shared
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .idle = true;
            thread::park();
        }
    }
    drop(active);
    drop(cache);
    let mut state = shared
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    state.events.clear();
    state.views.clear();
    for metric in [
        &shared.metrics.active_walks,
        &shared.metrics.open_handles,
        &shared.metrics.traversal_path_bytes,
        &shared.metrics.cached_entries,
        &shared.metrics.cached_path_bytes,
        &shared.metrics.event_slots,
    ] {
        metric.store(0, Ordering::Relaxed);
    }
}

fn path_bytes(path: &PathBuf) -> usize {
    native::path_weight(path)
}
fn validate_path(path: &PathBuf) -> io::Result<()> {
    if !path.is_absolute() || path.as_os_str().is_empty() || native::invalid_path(path) {
        return Err(invalid(
            "use an absolute path without NUL, up to 32766 UTF-16 units",
        ));
    }
    Ok(())
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn busy() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "folder statistics mailbox busy or visible-view subscription limit reached",
    )
}

#[cfg(test)]
mod tests;
