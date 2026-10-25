//! Retained Windows context menus for a native searchable popup. Enumeration,
//! expansion, native invocation and COM/HMENU teardown run on one STA worker.
//! Nothing is invoked during enumeration or search; `invoke` is explicit.

use std::{
    collections::{HashSet, VecDeque},
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

mod builtin;
#[cfg(any(windows, test))]
pub mod progress;
mod properties;
pub mod recycle;
mod signal;
pub use builtin::{BuiltinEvent, BuiltinItem, BuiltinKind, BuiltinMenus, BuiltinSnapshot};
pub use properties::Properties;
#[cfg(windows)]
mod windows;

pub const MAX_PATHS: usize = 256;
pub const MAX_ITEMS: usize = 512;
pub const MAX_DEPTH: usize = 6;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_PATH_BYTES: usize = 512 * 1024;
const MAX_ACTIONS: usize = 16;
const MAX_EVENTS: usize = 8;

#[derive(Clone, Debug, Default)]
pub struct ShellItem {
    /// Opaque session-local identifier. Never persist this as a pinned verb.
    pub id: u32,
    pub label: String,
    pub canonical_verb: Option<String>,
    pub help: String,
    pub enabled: bool,
    pub separator: bool,
    pub children: Vec<ShellItem>,
    pub has_submenu: bool,
    pub shortcut: Option<String>,
    pub checked: bool,
    pub default_item: bool,
    pub owner_draw: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MenuOptions {
    /// Shift+right-click extended verbs.
    pub extended: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    Ready {
        request: u64,
        items: Vec<ShellItem>,
        truncated: bool,
    },
    Updated {
        request: u64,
        items: Vec<ShellItem>,
        truncated: bool,
    },
    Failed {
        request: u64,
        message: String,
    },
    Invoked {
        request: u64,
        item_id: u32,
    },
}

pub struct ShellMenu {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}

impl ShellMenu {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::spawn(wake, Native::new)
    }

    pub fn open(
        &self,
        request: u64,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
    ) -> Result<(), String> {
        self.open_with_options(request, owner, paths, background, MenuOptions::default())
    }

    pub fn open_with_options(
        &self,
        request: u64,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
        options: MenuOptions,
    ) -> Result<(), String> {
        validate_paths(&paths, background)?;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err("shell menu worker closed".into());
        }
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        state.sequence = state.sequence.wrapping_add(1);
        let stamp = Stamp {
            request,
            serial: state.sequence,
        };
        state.desired = Some(stamp);
        state.ready = None;
        state.pending_open = Some(Open {
            stamp,
            owner,
            paths,
            background,
            options,
        });
        state.expands.clear();
        state.accepted_expands.clear();
        state.close_requested = false;
        drop(state);
        self.shared.signal.notify();
        Ok(())
    }

    /// Initializes a lazy native submenu, then returns a complete replacement
    /// snapshot. Search can request expansion without ever invoking a command.
    pub fn expand(&self, request: u64, item_id: u32) -> Result<(), String> {
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        let stamp = state
            .ready
            .filter(|stamp| stamp.request == request && state.desired == Some(*stamp))
            .ok_or_else(stale)?;
        if state.accepted_expands.contains(&item_id) {
            return Ok(());
        }
        if state.accepted_expands.len() >= MAX_ITEMS {
            return Err("shell submenu expansion limit reached".into());
        }
        if state.expands.len() == MAX_ACTIONS {
            return Err("shell submenu queue is full; retry".into());
        }
        state.accepted_expands.insert(item_id);
        state.expands.push_back(Action { stamp, item_id });
        drop(state);
        self.shared.signal.notify();
        Ok(())
    }

    /// Commits exactly one user-chosen command for the currently ready session.
    /// Accepted invocation survives closing/hiding the popup or a newer open.
    /// A stale call made after supersession is rejected. No implicit defaults.
    pub fn invoke(&self, request: u64, item_id: u32) -> Result<(), String> {
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        let stamp = state
            .ready
            .filter(|stamp| stamp.request == request && state.desired == Some(*stamp))
            .ok_or_else(stale)?;
        if state.committed.is_some() || state.invoking.is_some() {
            return Err("a shell command is already committed".into());
        }
        state.committed = Some(Action { stamp, item_id });
        state.invoking = Some(stamp);
        state.ready = None;
        state.expands.clear();
        drop(state);
        self.shared.signal.notify();
        Ok(())
    }

    pub fn close(&self, request: u64) -> Result<(), String> {
        let mut state = self.shared.state.try_lock().map_err(|_| busy())?;
        if state.desired.is_some_and(|stamp| stamp.request == request) {
            state.desired = None;
            state.ready = None;
            state.pending_open = None;
            state.expands.clear();
            state.accepted_expands.clear();
            state.close_requested = true;
        }
        drop(state);
        self.shared.signal.notify();
        Ok(())
    }

    pub fn try_recv(&self) -> Option<Event> {
        let mut state = self.shared.state.try_lock().ok()?;
        let index = state
            .events
            .iter()
            .position(|event| event.terminal || state.desired == Some(event.stamp))?;
        Some(state.events.remove(index)?.event)
    }

    pub fn shutdown(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.signal.notify();
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
            closed: AtomicBool::new(false),
            signal: signal::Signal::new()?,
        });
        let child = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("foldex-shell".into())
            .spawn(move || worker(child, wake, factory))?;
        shared.signal.attach(worker.thread().clone());
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
}

impl Drop for ShellMenu {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct Shared {
    state: Mutex<State>,
    closed: AtomicBool,
    signal: signal::Signal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stamp {
    request: u64,
    serial: u64,
}

struct Open {
    stamp: Stamp,
    owner: isize,
    paths: Vec<PathBuf>,
    background: bool,
    options: MenuOptions,
}
struct Action {
    stamp: Stamp,
    item_id: u32,
}
struct Envelope {
    stamp: Stamp,
    terminal: bool,
    event: Event,
}

#[derive(Default)]
struct State {
    sequence: u64,
    desired: Option<Stamp>,
    ready: Option<Stamp>,
    pending_open: Option<Open>,
    expands: VecDeque<Action>,
    accepted_expands: HashSet<u32>,
    committed: Option<Action>,
    invoking: Option<Stamp>,
    close_requested: bool,
    events: VecDeque<Envelope>,
}

impl State {
    fn publish(&mut self, stamp: Stamp, event: Event, terminal: bool) -> bool {
        self.events
            .retain(|old| old.terminal || (Some(old.stamp) == self.desired && old.stamp != stamp));
        if !terminal && self.desired != Some(stamp) {
            return false;
        }
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(Envelope {
            stamp,
            terminal,
            event,
        });
        true
    }
}

enum Job {
    Open(Open),
    Expand(Action),
    Invoke(Action),
    Close,
}
struct Snapshot {
    items: Vec<ShellItem>,
    truncated: bool,
}

trait Backend {
    fn open(&mut self, open: &Open) -> Result<Snapshot, String>;
    fn expand(&mut self, item_id: u32) -> Result<Snapshot, String>;
    fn invoke(&mut self, item_id: u32) -> Result<(), String>;
    fn close(&mut self);
    fn pump(&mut self) {}
}

#[cfg(windows)]
type Native = windows::Native;
#[cfg(not(windows))]
struct Native;
#[cfg(not(windows))]
impl Native {
    fn new() -> Self {
        Self
    }
}
#[cfg(not(windows))]
impl Backend for Native {
    fn open(&mut self, _: &Open) -> Result<Snapshot, String> {
        Err("Windows shell menus require Windows".into())
    }
    fn expand(&mut self, _: u32) -> Result<Snapshot, String> {
        Err(stale())
    }
    fn invoke(&mut self, _: u32) -> Result<(), String> {
        Err(stale())
    }
    fn close(&mut self) {}
}

fn worker<B: Backend>(
    shared: Arc<Shared>,
    wake: Arc<dyn Fn() + Send + Sync>,
    factory: impl FnOnce() -> B,
) {
    // Do not initialize COM or load Shell components until a menu is requested.
    let mut factory = Some(factory);
    let mut backend: Option<B> = None;
    let mut session = None;
    loop {
        if let Some(backend) = &mut backend {
            backend.pump();
        }
        let job = {
            let Ok(mut state) = shared.state.lock() else {
                break;
            };
            if shared.closed.load(Ordering::Acquire) {
                state.events.clear();
                break;
            }
            if let Some(action) = state.committed.take() {
                Some(Job::Invoke(action))
            } else if let Some(open) = state.pending_open.take() {
                Some(Job::Open(open))
            } else if state.close_requested {
                state.close_requested = false;
                Some(Job::Close)
            } else if let Some(action) = state.expands.pop_front() {
                Some(Job::Expand(action))
            } else {
                None
            }
        };
        let Some(job) = job else {
            if !shared.signal.wait() {
                break;
            }
            continue;
        };
        if backend.is_none() && !matches!(job, Job::Open(_)) {
            continue;
        }
        let backend = backend.get_or_insert_with(|| factory.take().unwrap()());
        match job {
            Job::Open(open) => {
                backend.close();
                let result = backend.open(&open);
                let stamp = open.stamp;
                let Ok(mut state) = shared.state.lock() else {
                    break;
                };
                if state.desired != Some(stamp) || shared.closed.load(Ordering::Acquire) {
                    drop(state);
                    backend.close();
                    session = None;
                    continue;
                }
                let event = match result {
                    Ok(snapshot) => {
                        session = Some(stamp);
                        state.ready = Some(stamp);
                        Event::Ready {
                            request: stamp.request,
                            items: snapshot.items,
                            truncated: snapshot.truncated,
                        }
                    }
                    Err(message) => {
                        session = None;
                        Event::Failed {
                            request: stamp.request,
                            message,
                        }
                    }
                };
                let published = state.publish(stamp, event, false);
                drop(state);
                if published {
                    wake();
                }
            }
            Job::Expand(action) => {
                let current = shared
                    .state
                    .lock()
                    .is_ok_and(|state| state.desired == Some(action.stamp));
                if !current || session != Some(action.stamp) {
                    continue;
                }
                let event = match backend.expand(action.item_id) {
                    Ok(snapshot) => Event::Updated {
                        request: action.stamp.request,
                        items: snapshot.items,
                        truncated: snapshot.truncated,
                    },
                    Err(message) => Event::Failed {
                        request: action.stamp.request,
                        message,
                    },
                };
                let published = shared
                    .state
                    .lock()
                    .is_ok_and(|mut state| state.publish(action.stamp, event, false));
                if published {
                    wake();
                }
            }
            Job::Invoke(action) => {
                let result = if session == Some(action.stamp) {
                    backend.invoke(action.item_id)
                } else {
                    Err(stale())
                };
                backend.close();
                session = None;
                let event = match result {
                    Ok(()) => Event::Invoked {
                        request: action.stamp.request,
                        item_id: action.item_id,
                    },
                    Err(message) => Event::Failed {
                        request: action.stamp.request,
                        message,
                    },
                };
                let Ok(mut state) = shared.state.lock() else {
                    break;
                };
                if state.invoking == Some(action.stamp) {
                    state.invoking = None;
                }
                let published = state.publish(action.stamp, event, true);
                drop(state);
                if published {
                    wake();
                }
            }
            Job::Close => {
                backend.close();
                session = None;
            }
        }
    }
    if let Some(backend) = &mut backend {
        backend.close();
    }
}

fn validate_paths(paths: &[PathBuf], background: bool) -> Result<(), String> {
    if paths.is_empty() || paths.len() > MAX_PATHS || (background && paths.len() != 1) {
        return Err("shell context needs 1–256 selected paths, or one background folder".into());
    }
    let mut bytes = 0usize;
    for path in paths {
        let encoded = path.as_os_str().as_encoded_bytes();
        if !path.is_absolute()
            || encoded.is_empty()
            || encoded.contains(&0)
            || encoded.len() > 64 * 1024
        {
            return Err("shell paths must be absolute, NUL-free and within 64 KiB".into());
        }
        bytes = bytes.saturating_add(encoded.len().saturating_mul(2));
    }
    if bytes > MAX_PATH_BYTES {
        return Err("shell selection paths exceed 512 KiB".into());
    }
    Ok(())
}

fn stale() -> String {
    "shell menu request is stale or not ready".into()
}
fn busy() -> String {
    "shell menu mailbox busy; retry on the next frame".into()
}

/// Removes accelerator markers while preserving literal &&; the tab-separated
/// native shortcut is returned separately for the UI's shortcut chip.
fn split_label(raw: &str) -> (String, Option<String>) {
    let (label, shortcut) = raw
        .split_once('\t')
        .map_or((raw, None), |(label, shortcut)| {
            (label, Some(shortcut.trim().to_owned()))
        });
    let mut result = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '&' {
            if chars.peek() == Some(&'&') {
                chars.next();
                result.push('&');
            }
        } else {
            result.push(character);
        }
    }
    (result, shortcut.filter(|text| !text.is_empty()))
}

#[cfg(test)]
mod tests;
