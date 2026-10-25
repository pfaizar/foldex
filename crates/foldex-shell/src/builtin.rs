//! Cached definitions for the system New/Compress menus. This lane never
//! enumerates the selection's arbitrary third-party context-menu handlers.
//! Definitions are inert: a committed command is resolved against a fresh
//! native menu for its captured target before it can execute.

use std::{
    collections::VecDeque,
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinKind {
    New,
    Compress,
}
impl BuiltinKind {
    fn index(self) -> usize {
        match self {
            Self::New => 0,
            Self::Compress => 1,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinItem {
    /// Stable definition identity, never an HMENU offset from an older menu.
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct BuiltinSnapshot {
    pub items: Arc<[BuiltinItem]>,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub enum BuiltinEvent {
    Ready {
        kind: BuiltinKind,
        snapshot: Arc<BuiltinSnapshot>,
    },
    DiscoveryFailed {
        kind: BuiltinKind,
        message: String,
    },
    Invoked {
        request: u64,
    },
    Failed {
        request: u64,
        message: String,
    },
}

#[derive(Clone)]
pub(super) struct Target {
    pub owner: isize,
    pub paths: Vec<PathBuf>,
    pub background: bool,
}
struct Discover {
    kind: BuiltinKind,
    target: Target,
}
struct Invoke {
    request: u64,
    kind: BuiltinKind,
    target: Target,
    key: String,
}
enum Job {
    Discover(Discover),
    Invoke(Invoke),
}

#[derive(Default)]
struct State {
    cache: [Option<Arc<BuiltinSnapshot>>; 2],
    discovering: [bool; 2],
    pending: [Option<Discover>; 2],
    committed: Option<Invoke>,
    invoking: bool,
    events: VecDeque<BuiltinEvent>,
}
struct Shared {
    state: Mutex<State>,
    closed: AtomicBool,
    signal: super::signal::Signal,
}

/// One lazy STA, two bounded definition caches and at most one committed action.
/// UI submissions only try-lock small mailboxes; COM never holds their lock.
pub struct BuiltinMenus {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl BuiltinMenus {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        Self::spawn(wake, Native::new)
    }

    /// Return the cached definitions immediately, or schedule their first load.
    /// Repeated opens do not rescan the registry or retain another native menu.
    pub fn discover(
        &self,
        kind: BuiltinKind,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
    ) -> Result<Option<Arc<BuiltinSnapshot>>, String> {
        super::validate_paths(&paths, background)?;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err("system menu worker closed".into());
        }
        let mut state = self.shared.state.try_lock().map_err(|_| super::busy())?;
        let at = kind.index();
        if let Some(snapshot) = &state.cache[at] {
            return Ok(Some(snapshot.clone()));
        }
        if !state.discovering[at] {
            state.discovering[at] = true;
            state.pending[at] = Some(Discover {
                kind,
                target: Target {
                    owner,
                    paths,
                    background,
                },
            });
        }
        drop(state);
        self.shared.signal.notify();
        Ok(None)
    }

    /// Capture the selected command and target. The worker reconstructs native
    /// definitions and rejects a missing/ambiguous key instead of reusing an ID.
    pub fn invoke(
        &self,
        request: u64,
        kind: BuiltinKind,
        owner: isize,
        paths: Vec<PathBuf>,
        background: bool,
        key: String,
    ) -> Result<(), String> {
        super::validate_paths(&paths, background)?;
        if key.is_empty() || key.len() > 2048 || key.contains('\0') {
            return Err("invalid system menu command identity".into());
        }
        if self.shared.closed.load(Ordering::Acquire) {
            return Err("system menu worker closed".into());
        }
        let mut state = self.shared.state.try_lock().map_err(|_| super::busy())?;
        if state.invoking {
            return Err("A system menu command is already running".into());
        }
        state.invoking = true;
        state.committed = Some(Invoke {
            request,
            kind,
            target: Target {
                owner,
                paths,
                background,
            },
            key,
        });
        drop(state);
        self.shared.signal.notify();
        Ok(())
    }

    pub fn try_recv(&self) -> Option<BuiltinEvent> {
        self.shared.state.try_lock().ok()?.events.pop_front()
    }
    pub fn shutdown(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.signal.notify();
    }
    pub fn is_worker_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
    fn spawn<B: Backend>(
        wake: Arc<dyn Fn() + Send + Sync>,
        factory: impl FnOnce() -> B + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            closed: AtomicBool::new(false),
            signal: super::signal::Signal::new()?,
        });
        let child = shared.clone();
        let worker = thread::Builder::new()
            .name("foldex-system-menus".into())
            .spawn(move || worker(child, wake, factory))?;
        shared.signal.attach(worker.thread().clone());
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
}
impl Drop for BuiltinMenus {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(super) trait Backend {
    fn discover(&mut self, kind: BuiltinKind, target: &Target) -> Result<BuiltinSnapshot, String>;
    fn invoke(&mut self, kind: BuiltinKind, target: &Target, key: &str) -> Result<(), String>;
    fn pump(&mut self) {}
}
#[cfg(windows)]
type Native = super::windows::builtin::Native;
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
    fn discover(&mut self, _: BuiltinKind, _: &Target) -> Result<BuiltinSnapshot, String> {
        Err("Windows system menus require Windows".into())
    }
    fn invoke(&mut self, _: BuiltinKind, _: &Target, _: &str) -> Result<(), String> {
        Err("Windows system menus require Windows".into())
    }
}

fn worker<B: Backend>(
    shared: Arc<Shared>,
    wake: Arc<dyn Fn() + Send + Sync>,
    factory: impl FnOnce() -> B,
) {
    let mut factory = Some(factory);
    let mut backend: Option<B> = None;
    loop {
        if let Some(backend) = &mut backend {
            backend.pump();
        }
        let job = {
            let Ok(mut state) = shared.state.lock() else {
                break;
            };
            if shared.closed.load(Ordering::Acquire) {
                break;
            }
            state.committed.take().map(Job::Invoke).or_else(|| {
                state
                    .pending
                    .iter_mut()
                    .find_map(Option::take)
                    .map(Job::Discover)
            })
        };
        let Some(job) = job else {
            if !shared.signal.wait() {
                break;
            }
            continue;
        };
        let backend = backend.get_or_insert_with(|| factory.take().unwrap()());
        let event = match job {
            Job::Discover(discover) => {
                let result = backend
                    .discover(discover.kind, &discover.target)
                    .and_then(bound_snapshot);
                let Ok(mut state) = shared.state.lock() else {
                    break;
                };
                state.discovering[discover.kind.index()] = false;
                match result {
                    Ok(snapshot) => {
                        let snapshot = Arc::new(snapshot);
                        state.cache[discover.kind.index()] = Some(snapshot.clone());
                        BuiltinEvent::Ready {
                            kind: discover.kind,
                            snapshot,
                        }
                    }
                    Err(message) => BuiltinEvent::DiscoveryFailed {
                        kind: discover.kind,
                        message,
                    },
                }
            }
            Job::Invoke(invoke) => {
                let result = backend.invoke(invoke.kind, &invoke.target, &invoke.key);
                let Ok(mut state) = shared.state.lock() else {
                    break;
                };
                state.invoking = false;
                match result {
                    Ok(()) => BuiltinEvent::Invoked {
                        request: invoke.request,
                    },
                    Err(message) => BuiltinEvent::Failed {
                        request: invoke.request,
                        message,
                    },
                }
            }
        };
        let Ok(mut state) = shared.state.lock() else {
            break;
        };
        if shared.closed.load(Ordering::Acquire) {
            break;
        }
        if state.events.len() == super::MAX_EVENTS {
            state.events.pop_front();
        }
        state.events.push_back(event);
        drop(state);
        wake();
    }
}

fn bound_snapshot(snapshot: BuiltinSnapshot) -> Result<BuiltinSnapshot, String> {
    let mut keys = std::collections::HashSet::new();
    let mut bytes = 0usize;
    let mut items = Vec::new();
    let mut truncated = snapshot.truncated;
    for item in snapshot.items.iter() {
        bytes = bytes
            .saturating_add(item.key.len())
            .saturating_add(item.label.len());
        if items.len() == super::MAX_ITEMS || bytes > super::MAX_TEXT_BYTES {
            truncated = true;
            break;
        }
        if item.key.is_empty()
            || item.key.len() > 2048
            || item.key.contains('\0')
            || item.label.is_empty()
        {
            return Err("invalid native system menu definition".into());
        }
        // Duplicate identities must not be selectable: an index is not a safe
        // substitute if two installed templates have the same native identity.
        if !keys.insert(item.key.clone()) {
            return Err("ambiguous native system menu definitions".into());
        }
        items.push(item.clone());
    }
    Ok(BuiltinSnapshot {
        items: items.into(),
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{atomic::AtomicUsize, mpsc},
        time::{Duration, Instant},
    };

    struct Fake {
        loads: Arc<AtomicUsize>,
        gate: Option<mpsc::Receiver<()>>,
        started: mpsc::Sender<()>,
    }
    impl Backend for Fake {
        fn discover(&mut self, _: BuiltinKind, _: &Target) -> Result<BuiltinSnapshot, String> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.started.send(()).unwrap();
            if let Some(gate) = self.gate.take() {
                gate.recv_timeout(Duration::from_secs(3)).unwrap();
            }
            Ok(BuiltinSnapshot {
                items: vec![BuiltinItem {
                    key: "fixture".into(),
                    label: "Fixture".into(),
                }]
                .into(),
                truncated: false,
            })
        }
        fn invoke(&mut self, _: BuiltinKind, target: &Target, key: &str) -> Result<(), String> {
            assert_eq!(target.paths, vec![std::env::temp_dir()]);
            assert_eq!(key, "fixture");
            Ok(())
        }
    }
    fn event(menu: &BuiltinMenus) -> BuiltinEvent {
        let start = Instant::now();
        loop {
            if let Some(event) = menu.try_recv() {
                return event;
            }
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn definition_load_is_coalesced_cached_and_does_not_hold_ui_mailbox() {
        let loads = Arc::new(AtomicUsize::new(0));
        let count = loads.clone();
        let (started, start) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let menu = BuiltinMenus::spawn(Arc::new(|| {}), move || Fake {
            loads: count,
            gate: Some(gate),
            started,
        })
        .unwrap();
        let paths = vec![std::env::temp_dir()];
        menu.discover(BuiltinKind::New, 0, paths.clone(), true)
            .unwrap();
        start.recv_timeout(Duration::from_secs(3)).unwrap();
        for _ in 0..256 {
            assert!(
                menu.discover(BuiltinKind::New, 0, paths.clone(), true)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        assert!(matches!(
            event(&menu),
            BuiltinEvent::Ready {
                kind: BuiltinKind::New,
                ..
            }
        ));
        let first = menu
            .discover(BuiltinKind::New, 0, paths.clone(), true)
            .unwrap()
            .unwrap();
        let second = menu
            .discover(BuiltinKind::New, 0, paths.clone(), true)
            .unwrap()
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(loads.load(Ordering::SeqCst), 1);
        menu.invoke(17, BuiltinKind::New, 0, paths, true, "fixture".into())
            .unwrap();
        assert!(matches!(
            event(&menu),
            BuiltinEvent::Invoked { request: 17 }
        ));
    }
    #[test]
    fn cache_bounds_reject_ambiguous_ids_and_truncate_large_catalogues() {
        let duplicate = BuiltinItem {
            key: "same".into(),
            label: "Template".into(),
        };
        assert!(
            bound_snapshot(BuiltinSnapshot {
                items: vec![duplicate.clone(), duplicate].into(),
                truncated: false
            })
            .is_err()
        );
        let items: Vec<_> = (0..super::super::MAX_ITEMS + 10)
            .map(|index| BuiltinItem {
                key: index.to_string(),
                label: "Template".into(),
            })
            .collect();
        let snapshot = bound_snapshot(BuiltinSnapshot {
            items: items.into(),
            truncated: false,
        })
        .unwrap();
        assert_eq!(snapshot.items.len(), super::super::MAX_ITEMS);
        assert!(snapshot.truncated);
    }

    #[test]
    fn committed_action_is_bounded_and_survives_later_definition_requests() {
        let loads = Arc::new(AtomicUsize::new(0));
        let (started, start) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let menu = BuiltinMenus::spawn(Arc::new(|| {}), move || Fake {
            loads,
            gate: Some(gate),
            started,
        })
        .unwrap();
        let paths = vec![std::env::temp_dir()];
        menu.discover(BuiltinKind::New, 0, paths.clone(), true)
            .unwrap();
        start.recv_timeout(Duration::from_secs(3)).unwrap();
        menu.invoke(
            44,
            BuiltinKind::New,
            0,
            paths.clone(),
            true,
            "fixture".into(),
        )
        .unwrap();
        assert!(
            menu.invoke(
                45,
                BuiltinKind::New,
                0,
                paths.clone(),
                true,
                "fixture".into()
            )
            .is_err()
        );
        menu.discover(BuiltinKind::Compress, 0, paths, false)
            .unwrap();
        release.send(()).unwrap();
        assert!(matches!(
            event(&menu),
            BuiltinEvent::Ready {
                kind: BuiltinKind::New,
                ..
            }
        ));
        assert!(matches!(
            event(&menu),
            BuiltinEvent::Invoked { request: 44 }
        ));
        assert!(matches!(
            event(&menu),
            BuiltinEvent::Ready {
                kind: BuiltinKind::Compress,
                ..
            }
        ));
    }
}
