//! Coalesced atomic workspace writes and independently merged shared preferences.
pub(crate) mod shared;
use serde_json::Value;
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
    time::{Duration, SystemTime},
};

pub const MAX_SESSION_BYTES: usize = shared::MAX_BYTES;
type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub enum SubmitError {
    Busy,
    TooLarge,
    Unavailable,
    Invalid(String),
}
impl SubmitError {
    pub fn retryable(&self) -> bool {
        matches!(self, Self::Busy)
    }
}
impl std::fmt::Display for SubmitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Busy => "Workspace writer is busy",
            Self::TooLarge => "Workspace exceeds 4 MiB; close unused tabs or remove saved layouts before saving again",
            Self::Unavailable => "Workspace writer is unavailable",
            Self::Invalid(error) => error,
        })
    }
}
struct Request {
    revision: u64,
    path: PathBuf,
    bytes: Vec<u8>,
    shared: Option<(PathBuf, Arc<Value>)>,
}
#[derive(Default)]
struct Mailbox {
    request: Mutex<Option<Request>>,
    result: Mutex<Option<(u64, Result<(), String>)>>,
    preferences: Mutex<Option<Result<Value, String>>>,
    sequence: AtomicU64,
}
pub struct Persistence {
    mailbox: Arc<Mailbox>,
    signal: SyncSender<()>,
    shared_path: Option<PathBuf>,
    baseline: Arc<Value>,
}

impl Persistence {
    #[cfg(test)]
    pub fn new(wake: Wake) -> io::Result<Self> {
        Self::start(wake, None, Value::Null)
    }
    pub fn new_session(wake: Wake, shared_path: PathBuf, session: &Value) -> io::Result<Self> {
        Self::start(wake, Some(shared_path), shared::preferences(session))
    }
    fn start(wake: Wake, shared_path: Option<PathBuf>, baseline: Value) -> io::Result<Self> {
        let mailbox = Arc::new(Mailbox::default());
        let (signal, rx) = mpsc::sync_channel(1);
        let worker = mailbox.clone();
        let watched = shared_path.clone();
        thread::Builder::new()
            .name("foldex-session".into())
            .spawn(move || {
                let mut stamp = None;
                loop {
                    match rx.recv_timeout(Duration::from_secs(2)) {
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        _ => {}
                    }
                    loop {
                        let request = worker.request.lock().ok().and_then(|mut slot| slot.take());
                        let Some(request) = request else { break };
                        let result = save_request(&request, |preferences| {
                            if let Ok(mut slot) = worker.preferences.lock() {
                                *slot = Some(Ok(preferences));
                            }
                            wake();
                        })
                        .map_err(|error| error.to_string());
                        if let Ok(mut slot) = worker.result.lock() {
                            *slot = Some((request.revision, result));
                        }
                        // Re-read under a stamp captured before reading: another process
                        // may commit immediately after our cross-process lock is released.
                        stamp = None;
                        wake();
                    }
                    if let Some(path) = watched.as_deref() {
                        let next = file_stamp(path);
                        if next != stamp {
                            stamp = next;
                            let result = shared::current(path).map_err(|error| error.to_string());
                            if let Some(result) = result.transpose() {
                                if let Ok(mut slot) = worker.preferences.lock() {
                                    *slot = Some(result);
                                }
                                wake();
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            mailbox,
            signal,
            shared_path,
            baseline: Arc::new(baseline),
        })
    }
    pub fn save(&self, path: PathBuf, bytes: Vec<u8>) -> Result<u64, SubmitError> {
        if bytes.len() > MAX_SESSION_BYTES {
            return Err(SubmitError::TooLarge);
        }
        let mut slot = self
            .mailbox
            .request
            .try_lock()
            .map_err(|_| SubmitError::Busy)?;
        let revision = self.mailbox.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        *slot = Some(Request {
            revision,
            path,
            bytes,
            shared: self
                .shared_path
                .clone()
                .map(|path| (path, self.baseline.clone())),
        });
        drop(slot);
        match self.signal.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(revision),
            Err(TrySendError::Disconnected(())) => Err(SubmitError::Unavailable),
        }
    }
    pub fn poll(&self) -> Option<(u64, Result<(), String>)> {
        self.mailbox.result.try_lock().ok()?.take()
    }
    pub fn preferences_pending(&self) -> bool {
        self.mailbox
            .preferences
            .try_lock()
            .is_ok_and(|slot| slot.is_some())
    }
    /// Apply remote deltas around unsaved local edits. Each queued save captures
    /// its own immutable UI baseline, so coalescing cannot erase another writer.
    pub fn poll_preferences(&mut self, local: &Value) -> Option<Result<Value, String>> {
        let remote = match self.mailbox.preferences.try_lock().ok()?.take()? {
            Ok(remote) => remote,
            Err(error) => return Some(Err(error)),
        };
        let merged = shared::merge(&self.baseline, &remote, &shared::preferences(local));
        self.baseline = Arc::new(remote);
        let mut session = local.clone();
        shared::overlay(&mut session, &merged);
        Some(Ok(session))
    }
}

fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Auxiliary saves never replace the primary window's workspace. Names contain
/// no user-controlled data, and each auxiliary process gets independent state.
pub fn window_path(directory: &Path, auxiliary: bool) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    if auxiliary {
        let time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        directory.join("workspaces").join(format!(
            "window-{}-{time}-{}.json",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    } else {
        directory.join("foldex-session.json")
    }
}

fn save_request(request: &Request, publish: impl FnOnce(Value)) -> io::Result<()> {
    let bytes = if let Some((path, baseline)) = &request.shared {
        let mut session: Value = serde_json::from_slice(&request.bytes)?;
        let merged = shared::save(path, baseline, &shared::preferences(&session))?;
        // Acknowledging preferences before the independent workspace write also
        // makes retries safe when a workspace disk error follows a successful merge.
        publish(merged);
        shared::strip(&mut session);
        serde_json::to_vec(&session)?
    } else {
        request.bytes.clone()
    };
    if let Some(parent) = request.path.parent() {
        fs::create_dir_all(parent)?;
    }
    crate::platform::atomic_save(&request.path, &bytes)
}

#[cfg(test)]
mod tests;
