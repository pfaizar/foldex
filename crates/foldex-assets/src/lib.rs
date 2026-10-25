//! Bounded, latest-only native shell assets. All filesystem/COM/GDI work belongs
//! to the one worker; callers may draw a generic vector icon while awaiting it.
//!
//! `epoch` protects view publication. Call `invalidate_path` after a filesystem
//! change to invalidate cached content at every size as well. Upload returned
//! pixels once and drop their Arc: the 8 MiB CPU lease follows caller-held Arcs.
//! Full event/pixel budgets pause extraction until the caller drains results;
//! capacity pressure is never cached as a native-provider failure.

use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, TryLockError,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

mod native_icon;
#[cfg(windows)]
mod windows;
pub use native_icon::{KnownFolderIcon, MAX_SHELL_ICON_ID_BYTES, StockIcon, is_native_icon_path};

pub const MAX_VIEWS: usize = 64;
pub const MAX_REQUESTS_PER_VIEW: usize = 128;
pub const MAX_IMAGE_EDGE: usize = 256;
pub const CPU_PIXEL_BUDGET: usize = 8 * 1024 * 1024;
const MAX_PIXEL_BYTES: usize = MAX_IMAGE_EDGE * MAX_IMAGE_EDGE * 4;
const MAX_PATH_BYTES: usize = 64 * 1024;
const PATH_QUEUE_BUDGET: usize = 512 * 1024;
const MAX_EVENTS: usize = 256;
const MAX_CACHE_ENTRIES: usize = 128;
const THUMBNAIL_TTL: Duration = Duration::from_secs(10);
const ICON_TTL: Duration = Duration::from_secs(60);
const ERROR_TTL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AssetKey {
    pub path: PathBuf,
    /// Requested bounding box in physical pixels, from 1 through 256.
    pub size: u16,
    pub thumbnail: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetRequest {
    pub key: AssetKey,
}

/// Thumbnail providers may read local file contents if extraction is enabled.
/// Reparse ancestors/offline placeholders receive a generic icon; remote or
/// unavailable paths retain the caller's own fallback. ShellExecute is never called.
#[derive(Clone, Copy, Debug, Default)]
pub struct AssetOptions {
    pub extract_local_thumbnails: bool,
}

#[derive(Debug)]
pub struct PixelImage {
    pub size: [usize; 2],
    /// Top-down, straight (unpremultiplied) sRGB RGBA8.
    pub rgba: Vec<u8>,
    _lease: PixelLease,
}

#[derive(Clone, Debug)]
pub struct AssetEvent {
    pub view: u64,
    pub epoch: u64,
    pub key: AssetKey,
    pub pixels: Result<Arc<PixelImage>, String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AssetMetrics {
    /// Includes cache/event pixels and Arcs still held by the caller.
    pub cpu_pixel_bytes: usize,
    pub cached_entries: usize,
    /// Cumulative backend load attempts, including provider failures.
    pub native_loads: usize,
    /// Cumulative successful and negative cache hits.
    pub cache_hits: usize,
    /// Publication wakes plus retries after a contended nonblocking receive.
    pub ui_wakeups: usize,
    /// Capacity waits, excluding ordinary idle/cache-expiry parking.
    pub backpressure_waits: usize,
}

pub struct AssetLoader {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl AssetLoader {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::with_options(AssetOptions::default(), wake)
    }

    pub fn with_options(
        options: AssetOptions,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> io::Result<Self> {
        Self::spawn(wake, move || Native::new(options))
    }

    /// Replaces the complete visible request set for a view. The newest view
    /// state wins; a smaller epoch is ignored. An exact key still visible in the
    /// same uncancelled/uninvalidated view keeps its in-flight pixels and follows
    /// the latest epoch. Identical sets do not restart work.
    /// Returns WouldBlock if the brief mailbox lock is busy or 64 other views
    /// are registered. Cancel hidden views before adding another one, and retry
    /// a busy mailbox on the next frame. No disk/COM calls happen here.
    pub fn set_requests(
        &self,
        view: u64,
        epoch: u64,
        requests: Vec<AssetRequest>,
    ) -> io::Result<()> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "asset worker closed",
            ));
        }
        if requests.len() > MAX_REQUESTS_PER_VIEW {
            return Err(invalid("at most 128 visible assets per view"));
        }
        let mut keys = Vec::with_capacity(requests.len());
        let mut weight = 0usize;
        for request in requests {
            validate_key(&request.key)?;
            if !keys.contains(&request.key) {
                weight = weight.saturating_add(key_weight(&request.key));
                keys.push(request.key);
            }
        }
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        let existing = state.views.iter().position(|entry| entry.view == view);
        if let Some(index) = existing {
            let old = &state.views[index];
            if epoch < old.epoch || (epoch == old.epoch && keys == old.keys) {
                return Ok(());
            }
        } else if state.views.len() == MAX_VIEWS {
            return Err(busy());
        }
        let other_weight: usize = state
            .views
            .iter()
            .filter(|entry| entry.view != view)
            .map(|entry| entry.weight)
            .sum();
        if weight.saturating_add(other_weight) > PATH_QUEUE_BUDGET {
            return Err(invalid("visible asset paths exceed 512 KiB"));
        }
        // A viewport replacement can keep the same file visible at a different
        // row or epoch. Its in-flight pixels still identify that exact key.
        // Cancellation and explicit invalidation start a new content lifetime.
        let serial = if let Some(index) = existing {
            state.views[index].serial
        } else {
            state.next_serial()
        };
        let entry = View {
            view,
            epoch,
            serial,
            keys,
            next: 0,
            weight,
        };
        if let Some(index) = existing {
            state.views[index] = entry;
        } else {
            state.views.push(entry);
        }
        drop(state);
        self.shared.notify();
        Ok(())
    }

    /// Borrowed retry path for a cached viewport request set. Acquires the
    /// mailbox before inspecting/cloning keys; an unchanged set and a busy
    /// mailbox perform no PathBuf clones. New keys are cloned only on acceptance.
    pub fn set_requests_ref(
        &self,
        view: u64,
        epoch: u64,
        requests: &[AssetRequest],
    ) -> io::Result<()> {
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "asset worker closed",
            ));
        }
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        let existing = state.views.iter().position(|entry| entry.view == view);
        if let Some(index) = existing {
            let old = &state.views[index];
            if epoch < old.epoch
                || (epoch == old.epoch
                    && old.keys.len() == requests.len()
                    && old
                        .keys
                        .iter()
                        .zip(requests)
                        .all(|(key, request)| key == &request.key))
            {
                return Ok(());
            }
        } else if state.views.len() == MAX_VIEWS {
            return Err(busy());
        }
        if requests.len() > MAX_REQUESTS_PER_VIEW {
            return Err(invalid("at most 128 visible assets per view"));
        }
        let mut unique: Vec<&AssetKey> = Vec::with_capacity(requests.len());
        let mut weight = 0usize;
        for request in requests {
            validate_key(&request.key)?;
            if !unique.contains(&&request.key) {
                weight = weight.saturating_add(key_weight(&request.key));
                unique.push(&request.key);
            }
        }
        let other_weight: usize = state
            .views
            .iter()
            .filter(|entry| entry.view != view)
            .map(|entry| entry.weight)
            .sum();
        if weight.saturating_add(other_weight) > PATH_QUEUE_BUDGET {
            return Err(invalid("visible asset paths exceed 512 KiB"));
        }
        if let Some(index) = existing {
            let old = &state.views[index];
            if epoch == old.epoch && old.keys.iter().eq(unique.iter().copied()) {
                return Ok(());
            }
        }
        let serial = if let Some(index) = existing {
            state.views[index].serial
        } else {
            state.next_serial()
        };
        let entry = View {
            view,
            epoch,
            serial,
            keys: unique.into_iter().cloned().collect(),
            next: 0,
            weight,
        };
        if let Some(index) = existing {
            state.views[index] = entry;
        } else {
            state.views.push(entry);
        }
        drop(state);
        self.shared.notify();
        Ok(())
    }

    /// Removes a hidden/closed view and rejects its in-flight results. Shell
    /// work already in a native call finishes on the worker, then is discarded.
    pub fn cancel_view(&self, view: u64) -> io::Result<()> {
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        state.views.retain(|entry| entry.view != view);
        drop(state);
        self.shared.notify();
        Ok(())
    }

    /// Invalidates all cached sizes/variants for this exact raw path and restarts
    /// matching visible requests. Use after rename/write/thumbnail freshness
    /// events, even when the visible request epoch is otherwise unchanged.
    pub fn invalidate_path(&self, path: &Path) -> io::Result<()> {
        validate_path(path)?;
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        let serial = state.next_serial();
        for entry in &mut state.views {
            if entry.keys.iter().any(|key| key.path == path) {
                entry.serial = serial;
                entry.next = 0;
            }
        }
        if !state.invalidations.iter().any(|queued| queued == path) {
            if state.invalidations.len() == MAX_REQUESTS_PER_VIEW
                || state.invalidation_bytes.saturating_add(path_weight(path)) > PATH_QUEUE_BUDGET
            {
                state.clear_cache = true;
                state.invalidations.clear();
                state.invalidation_bytes = 0;
            } else if !state.clear_cache {
                state.invalidation_bytes += path_weight(path);
                state.invalidations.push(path.to_path_buf());
            }
        }
        drop(state);
        self.shared.notify();
        Ok(())
    }

    pub fn invalidate_key(&self, key: &AssetKey) -> io::Result<()> {
        self.invalidate_path(&key.path)
    }

    /// Nonblocking. Stale event disposal remains on the worker, avoiding large
    /// pixel buffer destruction in the UI mailbox path.
    pub fn try_recv(&self) -> Option<AssetEvent> {
        let mut state = match self.shared.state.try_lock() {
            Ok(state) => state,
            Err(TryLockError::WouldBlock) => {
                // A coalesced publication wake may be consumed by this frame.
                // Retry on another frame if the worker briefly owns the mailbox.
                self.shared.wake_ui();
                return None;
            }
            Err(TryLockError::Poisoned(_)) => return None,
        };
        let (index, epoch) = state.events.iter().enumerate().find_map(|(index, event)| {
            state.current_epoch(&event.work).map(|epoch| (index, epoch))
        })?;
        let was_full = state.events.len() == MAX_EVENTS;
        let mut event = state.events.remove(index)?.event;
        event.epoch = epoch;
        drop(state);
        if was_full {
            self.shared.notify();
        }
        Some(event)
    }

    pub fn metrics(&self) -> AssetMetrics {
        AssetMetrics {
            cpu_pixel_bytes: self.shared.budget.used.load(Ordering::Relaxed),
            cached_entries: self.shared.cached_entries.load(Ordering::Relaxed),
            native_loads: self.shared.native_loads.load(Ordering::Relaxed),
            cache_hits: self.shared.cache_hits.load(Ordering::Relaxed),
            ui_wakeups: self.shared.ui_wakeups.load(Ordering::Relaxed),
            backpressure_waits: self.shared.backpressure_waits.load(Ordering::Relaxed),
        }
    }

    /// Signals shutdown without joining a possibly slow native provider call.
    /// All RAII resources are released on the same worker once that call returns.
    pub fn shutdown(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.notify();
    }

    pub fn is_worker_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn spawn<F, B>(wake: Arc<dyn Fn() + Send + Sync>, factory: F) -> io::Result<Self>
    where
        F: FnOnce() -> B + Send + 'static,
        B: Backend,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake,
            closed: AtomicBool::new(false),
            budget: Arc::new(PixelBudget {
                used: AtomicUsize::new(0),
                limit: CPU_PIXEL_BUDGET,
                worker_thread: OnceLock::new(),
            }),
            cached_entries: AtomicUsize::new(0),
            native_loads: AtomicUsize::new(0),
            cache_hits: AtomicUsize::new(0),
            ui_wakeups: AtomicUsize::new(0),
            backpressure_waits: AtomicUsize::new(0),
        });
        let worker_shared = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("foldex-assets".into())
            .stack_size(512 * 1024)
            .spawn(move || run_worker(worker_shared, factory()))?;
        let _ = shared.budget.worker_thread.set(worker.thread().clone());
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
}

impl Drop for AssetLoader {
    fn drop(&mut self) {
        self.shutdown();
        // Detach rather than block the UI while an installed provider is slow.
        // No raw COM/GDI object crosses this boundary or is abandoned early.
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Arc<dyn Fn() + Send + Sync>,
    closed: AtomicBool,
    budget: Arc<PixelBudget>,
    cached_entries: AtomicUsize,
    native_loads: AtomicUsize,
    cache_hits: AtomicUsize,
    ui_wakeups: AtomicUsize,
    backpressure_waits: AtomicUsize,
}

impl Shared {
    fn notify(&self) {
        if let Some(worker) = self.budget.worker_thread.get() {
            // unpark retains a permit even if shutdown/input happens just
            // before park. No lock-dependent/lost Condvar wake can stall exit.
            worker.unpark();
        }
    }

    fn wake_ui(&self) {
        self.ui_wakeups.fetch_add(1, Ordering::Relaxed);
        (self.wake)();
    }
}

#[derive(Default)]
struct State {
    views: Vec<View>,
    cursor: usize,
    sequence: u64,
    events: VecDeque<StampedEvent>,
    invalidations: Vec<PathBuf>,
    invalidation_bytes: usize,
    clear_cache: bool,
}

impl State {
    fn next_serial(&mut self) -> u64 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    fn current(&self, work: &Work) -> bool {
        self.current_epoch(work).is_some()
    }

    fn current_epoch(&self, work: &Work) -> Option<u64> {
        self.views
            .iter()
            .find(|view| {
                view.view == work.view
                    && view.serial == work.serial
                    && view.keys.contains(&work.key)
            })
            .map(|view| view.epoch)
    }

    fn next_work(&mut self) -> Option<Work> {
        for _ in 0..self.views.len() {
            self.cursor %= self.views.len();
            let entry = &mut self.views[self.cursor];
            self.cursor += 1;
            if let Some(key) = entry.keys.get(entry.next) {
                entry.next += 1;
                return Some(Work {
                    view: entry.view,
                    epoch: entry.epoch,
                    serial: entry.serial,
                    key: key.clone(),
                });
            }
        }
        None
    }

    fn retire_stale(&mut self) {
        let views = &self.views;
        self.events.retain_mut(|event| {
            let current = views.iter().find(|view| {
                view.view == event.work.view
                    && view.serial == event.work.serial
                    && view.keys.contains(&event.work.key)
            });
            if let Some(view) = current {
                event.work.epoch = view.epoch;
                event.event.epoch = view.epoch;
            }
            current.is_some()
        });
    }
}

struct View {
    view: u64,
    epoch: u64,
    serial: u64,
    keys: Vec<AssetKey>,
    next: usize,
    weight: usize,
}

struct Work {
    view: u64,
    epoch: u64,
    serial: u64,
    key: AssetKey,
}

struct StampedEvent {
    work: Work,
    event: AssetEvent,
}

#[derive(Debug)]
struct PixelBudget {
    used: AtomicUsize,
    limit: usize,
    worker_thread: OnceLock<thread::Thread>,
}

#[derive(Debug)]
struct PixelLease {
    budget: Arc<PixelBudget>,
    bytes: usize,
}

impl Drop for PixelLease {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(self.bytes, Ordering::AcqRel);
        // The final Arc can be released after try_recv returns. Wake capacity
        // waits here so callers need no polling timer or explicit acknowledgement.
        if let Some(worker) = self.budget.worker_thread.get() {
            worker.unpark();
        }
    }
}

struct RawPixels {
    size: [usize; 2],
    rgba: Vec<u8>,
}

impl RawPixels {
    fn finish(self, budget: &Arc<PixelBudget>) -> Result<Arc<PixelImage>, String> {
        let [width, height] = self.size;
        if width == 0
            || height == 0
            || width > MAX_IMAGE_EDGE
            || height > MAX_IMAGE_EDGE
            || self.rgba.len() != width * height * 4
            || self.rgba.capacity() > MAX_PIXEL_BYTES
        {
            return Err("invalid or oversized shell image dimensions".into());
        }
        let bytes = self.rgba.capacity();
        budget
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|next| *next <= budget.limit)
            })
            .map_err(|_| "asset CPU pixel budget is held by pending results".to_owned())?;
        Ok(Arc::new(PixelImage {
            size: self.size,
            rgba: self.rgba,
            _lease: PixelLease {
                budget: Arc::clone(budget),
                bytes,
            },
        }))
    }
}

trait Backend {
    fn load(&mut self, key: &AssetKey) -> Result<RawPixels, String>;
}

#[cfg(windows)]
type Native = windows::Native;

#[cfg(not(windows))]
struct Native;

#[cfg(not(windows))]
impl Native {
    fn new(_: AssetOptions) -> Self {
        Self
    }
}

#[cfg(not(windows))]
impl Backend for Native {
    fn load(&mut self, _: &AssetKey) -> Result<RawPixels, String> {
        Err("native shell assets require Windows".into())
    }
}

struct CacheEntry {
    key: AssetKey,
    pixels: Result<Arc<PixelImage>, String>,
    expires: Instant,
    weight: usize,
}

#[derive(Default)]
struct Cache {
    entries: VecDeque<CacheEntry>,
    path_bytes: usize,
}

impl Cache {
    fn expire(&mut self) {
        let now = Instant::now();
        self.entries.retain(|entry| entry.expires > now);
        self.path_bytes = self.entries.iter().map(|entry| entry.weight).sum();
    }

    fn remove_path(&mut self, path: &Path) {
        self.entries.retain(|entry| entry.key.path != path);
        self.path_bytes = self.entries.iter().map(|entry| entry.weight).sum();
    }

    fn get(&mut self, key: &AssetKey) -> Option<Result<Arc<PixelImage>, String>> {
        let position = self.entries.iter().position(|entry| entry.key == *key)?;
        let entry = self.entries.remove(position)?;
        let result = entry.pixels.clone();
        self.entries.push_back(entry);
        Some(result)
    }

    fn make_room(&mut self, budget: &PixelBudget, bytes: usize) {
        while budget.used.load(Ordering::Acquire).saturating_add(bytes) > budget.limit {
            // Negative entries hold no pixel lease. Evicting them cannot free
            // capacity and would invite repeated failing provider calls.
            let Some(index) = self.entries.iter().position(|entry| entry.pixels.is_ok()) else {
                break;
            };
            let entry = self
                .entries
                .remove(index)
                .expect("located pixel cache entry");
            self.path_bytes -= entry.weight;
        }
    }

    fn insert(&mut self, key: AssetKey, pixels: Result<Arc<PixelImage>, String>) {
        let weight = key_weight(&key);
        while !self.entries.is_empty()
            && (self.entries.len() >= MAX_CACHE_ENTRIES
                || self.path_bytes.saturating_add(weight) > PATH_QUEUE_BUDGET)
        {
            if let Some(entry) = self.entries.pop_front() {
                self.path_bytes -= entry.weight;
            }
        }
        let ttl = if pixels.is_err() {
            ERROR_TTL
        } else if key.thumbnail {
            THUMBNAIL_TTL
        } else {
            ICON_TTL
        };
        self.path_bytes += weight;
        self.entries.push_back(CacheEntry {
            key,
            pixels,
            expires: Instant::now() + ttl,
            weight,
        });
    }

    fn wait_duration(&self) -> Option<Duration> {
        self.entries
            .iter()
            .map(|entry| entry.expires.saturating_duration_since(Instant::now()))
            .min()
    }
}

fn run_worker<B: Backend>(shared: Arc<Shared>, mut backend: B) {
    let mut cache = Cache::default();
    let mut pending: Option<Work> = None;
    loop {
        cache.expire();
        shared
            .cached_entries
            .store(cache.entries.len(), Ordering::Relaxed);
        let mut work = {
            let Ok(mut state) = shared.state.lock() else {
                break;
            };
            if shared.closed.load(Ordering::Acquire) {
                state.events.clear();
                break;
            }
            state.retire_stale();
            if state.clear_cache {
                cache = Cache::default();
                state.clear_cache = false;
            }
            for path in state.invalidations.drain(..) {
                cache.remove_path(&path);
            }
            state.invalidation_bytes = 0;
            if pending.as_ref().is_some_and(|work| !state.current(work)) {
                pending = None;
            }
            // Never discard an undelivered current result to make queue space.
            // A UI dequeue or control change unparks this worker.
            if state.events.len() == MAX_EVENTS {
                drop(state);
                shared.backpressure_waits.fetch_add(1, Ordering::Relaxed);
                thread::park();
                continue;
            }
            if let Some(work) = pending.take().or_else(|| state.next_work()) {
                work
            } else {
                drop(state);
                if let Some(duration) = cache.wait_duration() {
                    thread::park_timeout(duration);
                } else {
                    thread::park();
                }
                continue;
            }
        };
        let pixels = if let Some(pixels) = cache.get(&work.key) {
            shared.cache_hits.fetch_add(1, Ordering::Relaxed);
            pixels
        } else {
            // This is the only pixel producer. Make room for the maximum
            // bounded backend allocation before entering a native provider;
            // caller-held Arcs can then only decrease the budget while loading.
            cache.make_room(&shared.budget, MAX_PIXEL_BYTES);
            if shared.budget.used.load(Ordering::Acquire) + MAX_PIXEL_BYTES > shared.budget.limit {
                pending = Some(work);
                shared
                    .cached_entries
                    .store(cache.entries.len(), Ordering::Relaxed);
                shared.backpressure_waits.fetch_add(1, Ordering::Relaxed);
                thread::park();
                continue;
            }
            shared.native_loads.fetch_add(1, Ordering::Relaxed);
            let pixels = backend
                .load(&work.key)
                .and_then(|raw| raw.finish(&shared.budget));
            // A cancelled/invalidated native result must not repopulate cache.
            let is_current = shared.state.lock().is_ok_and(|state| state.current(&work));
            if is_current && !shared.closed.load(Ordering::Acquire) {
                cache.insert(work.key.clone(), pixels.clone());
            }
            pixels
        };
        shared
            .cached_entries
            .store(cache.entries.len(), Ordering::Relaxed);
        let should_wake = {
            let Ok(mut state) = shared.state.lock() else {
                break;
            };
            state.retire_stale();
            if let Some(epoch) = state
                .current_epoch(&work)
                .filter(|_| !shared.closed.load(Ordering::Acquire))
            {
                work.epoch = epoch;
                let event = AssetEvent {
                    view: work.view,
                    epoch: work.epoch,
                    key: work.key.clone(),
                    pixels,
                };
                let was_empty = state.events.is_empty();
                debug_assert!(state.events.len() < MAX_EVENTS);
                state.events.push_back(StampedEvent { work, event });
                was_empty
            } else {
                false
            }
        };
        if should_wake {
            shared.wake_ui();
        }
    }
    shared.cached_entries.store(0, Ordering::Relaxed);
}

fn path_weight(path: &Path) -> usize {
    path.as_os_str().len().saturating_mul(2)
}

fn key_weight(key: &AssetKey) -> usize {
    path_weight(&key.path).saturating_add(std::mem::size_of::<AssetKey>())
}

fn validate_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path_weight(path) > MAX_PATH_BYTES
        || (!path.is_absolute() && !is_native_icon_path(path))
        || path.as_os_str().as_encoded_bytes().contains(&0)
    {
        Err(invalid(
            "asset path must be absolute, nonempty, NUL-free and within 64 KiB",
        ))
    } else {
        Ok(())
    }
}

fn validate_key(key: &AssetKey) -> io::Result<()> {
    validate_path(&key.path)?;
    if key.size == 0 || usize::from(key.size) > MAX_IMAGE_EDGE {
        Err(invalid("asset size must be between 1 and 256 pixels"))
    } else {
        Ok(())
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn busy() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "asset mailbox busy or visible-view limit reached",
    )
}

#[cfg(test)]
mod tests;
