//! A coalesced atomic session writer independent of slow shell/file operations.
use std::{
    borrow::Cow,
    io::{self, Read},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender, TrySendError},
    },
    thread,
};

struct Request {
    revision: u64,
    path: PathBuf,
    bytes: Vec<u8>,
    preserve_launcher: bool,
}
#[derive(Default)]
struct Mailbox {
    request: Mutex<Option<Request>>,
    result: Mutex<Option<(u64, Result<(), String>)>>,
    sequence: AtomicU64,
}
pub struct Persistence {
    mailbox: Arc<Mailbox>,
    signal: SyncSender<()>,
    preserve_launcher: bool,
}

impl Persistence {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>, preserve_launcher: bool) -> io::Result<Self> {
        let mailbox = Arc::new(Mailbox::default());
        let (signal, rx) = mpsc::sync_channel(1);
        let worker = mailbox.clone();
        thread::Builder::new()
            .name("foldex-session".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    loop {
                        let request = worker.request.lock().ok().and_then(|mut s| s.take());
                        let Some(request) = request else { break };
                        let result = save_request(&request).map_err(|e| e.to_string());
                        if let Ok(mut slot) = worker.result.lock() {
                            *slot = Some((request.revision, result));
                        }
                        wake();
                    }
                }
            })?;
        Ok(Self {
            mailbox,
            signal,
            preserve_launcher,
        })
    }
    pub fn save(&self, path: PathBuf, bytes: Vec<u8>) -> Result<u64, String> {
        if bytes.len() > 256 * 1024 {
            return Err("Workspace state exceeds its 256 KiB budget".into());
        }
        let mut slot = self
            .mailbox
            .request
            .try_lock()
            .map_err(|_| "Workspace writer is busy".to_owned())?;
        let revision = self.mailbox.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        *slot = Some(Request {
            revision,
            path,
            bytes,
            preserve_launcher: self.preserve_launcher,
        });
        drop(slot);
        match self.signal.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(revision),
            Err(TrySendError::Disconnected(())) => Err("Workspace writer is unavailable".into()),
        }
    }
    pub fn poll(&self) -> Option<(u64, Result<(), String>)> {
        self.mailbox.result.try_lock().ok()?.take()
    }
}

fn save_request(request: &Request) -> io::Result<()> {
    // Serialize portable-session writers across processes. Auxiliary windows
    // can save their workspace without overwriting the owner's launcher settings.
    // This lock is held only on the sleeping persistence worker, never the UI.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(request.path.with_extension("lock"))?;
    lock.lock()?;
    let mut bytes = Cow::Borrowed(request.bytes.as_slice());
    let mut previous = None;
    if request.preserve_launcher {
        match std::fs::File::open(&request.path) {
            Ok(file) => {
                let mut current = Vec::new();
                file.take(256 * 1024 + 1).read_to_end(&mut current)?;
                if current.len() > 256 * 1024 {
                    return Err(io::Error::other("Existing workspace exceeds 256 KiB"));
                }
                let state: serde_json::Value = serde_json::from_slice(&current)?;
                if let Some(launcher) = state.get("launcher") {
                    let mut next: serde_json::Value = serde_json::from_slice(&bytes)?;
                    if next.get("launcher") != Some(launcher) {
                        next["launcher"] = launcher.clone();
                        bytes = Cow::Owned(serde_json::to_vec_pretty(&next)?);
                    }
                }
                previous = Some(current);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if bytes.len() > 256 * 1024 {
        return Err(io::Error::other("Workspace state exceeds 256 KiB"));
    }
    // The existing auxiliary-session read already supplies an exact comparison.
    // Check under the cross-process lock so a no-op never hides an owner's edit.
    if previous.as_deref() == Some(bytes.as_ref()) {
        return Ok(());
    }
    crate::platform::atomic_save(&request.path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn identical_auxiliary_save_keeps_the_existing_file_and_modification_time() {
        let base =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/session-test-fixtures");
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join(format!("owned-aux-noop-{}.json", std::process::id()));
        let bytes = br#"{"launcher":{"background":false},"active":3}"#;
        std::fs::write(&path, bytes).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        let modified = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let modified = file.metadata().unwrap().modified().unwrap();
        drop(file);
        save_request(&Request {
            revision: 1,
            path: path.clone(),
            preserve_launcher: true,
            bytes: bytes.to_vec(),
        })
        .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn latest_session_is_saved_atomically_without_a_window() {
        let base =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/session-test-fixtures");
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join(format!("owned-{}.json", std::process::id()));
        let worker = Persistence::new(Arc::new(|| {}), false).unwrap();
        let mut revision = 0;
        for n in 0..40 {
            loop {
                match worker.save(path.clone(), format!("{{\"revision\":{n}}}").into_bytes()) {
                    Ok(r) => {
                        revision = r;
                        break;
                    }
                    Err(_) => thread::yield_now(),
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut saved = false;
        while Instant::now() < deadline {
            if let Some((r, result)) = worker.poll() {
                result.unwrap();
                if r == revision {
                    saved = true;
                    break;
                }
            }
            thread::yield_now();
        }
        assert!(saved);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"revision\":39}");
        let resolved = path.canonicalize().unwrap();
        let root = base.canonicalize().unwrap();
        assert!(resolved.starts_with(root));
        std::fs::remove_file(resolved).unwrap();
    }

    #[test]
    fn auxiliary_session_preserves_the_owners_latest_launcher_settings() {
        let base =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/session-test-fixtures");
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join(format!("owned-aux-{}.json", std::process::id()));
        std::fs::write(
            &path,
            br#"{"launcher":{"background":false,"hotkey":"Ctrl+Shift+K"},"active":1}"#,
        )
        .unwrap();
        save_request(&Request {
            revision: 1,
            path: path.clone(),
            preserve_launcher: true,
            bytes: br#"{"launcher":{"background":true,"hotkey":"Ctrl+Alt+Space"},"active":3}"#
                .to_vec(),
        })
        .unwrap();
        let state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(state["launcher"]["background"], false);
        assert_eq!(state["launcher"]["hotkey"], "Ctrl+Shift+K");
        assert_eq!(state["active"], 3);
        std::fs::remove_file(path).unwrap();
    }
}
