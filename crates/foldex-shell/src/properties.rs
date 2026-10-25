//! Native Properties has no dependency on a context-menu enumeration/session.
//! One on-demand STA bounds concurrent requests and never queues behind handlers.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

pub struct Properties {
    busy: Arc<AtomicBool>,
    result: Arc<Mutex<Option<Result<(), String>>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl Properties {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            busy: Arc::new(AtomicBool::new(false)),
            result: Arc::new(Mutex::new(None)),
            wake,
        }
    }

    pub fn show(&self, paths: Vec<PathBuf>) -> Result<(), String> {
        self.show_with_owner(0, paths)
    }

    /// `owner` is Foldex's HWND, captured on the UI thread. Zero is allowed for
    /// callers without a window; never substitute an unrelated foreground HWND.
    pub fn show_with_owner(&self, owner: isize, paths: Vec<PathBuf>) -> Result<(), String> {
        super::validate_paths(&paths, false)?;
        self.spawn(move || {
            #[cfg(windows)]
            {
                super::windows::properties(owner, &paths)
            }
            #[cfg(not(windows))]
            {
                let _ = (owner, paths);
                Err("Native properties require Windows".into())
            }
        })
    }

    pub fn try_recv(&self) -> Option<Result<(), String>> {
        self.result.try_lock().ok()?.take()
    }
    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    pub(super) fn spawn(
        &self,
        action: impl FnOnce() -> Result<(), String> + Send + 'static,
    ) -> Result<(), String> {
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("A Properties window is already open".into());
        }
        let busy = self.busy.clone();
        let result = self.result.clone();
        let wake = self.wake.clone();
        if let Err(error) = thread::Builder::new()
            .name("foldex-properties".into())
            .spawn(move || {
                let value = action();
                if let Ok(mut slot) = result.lock() {
                    *slot = Some(value);
                }
                busy.store(false, Ordering::Release);
                wake();
            })
        {
            self.busy.store(false, Ordering::Release);
            return Err(format!("Cannot open Properties: {error}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    #[test]
    fn independent_dialog_work_is_bounded_and_submission_never_waits() {
        let properties = Properties::new(Arc::new(|| {}));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        properties
            .spawn(move || {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            })
            .unwrap();
        started_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let start = Instant::now();
        assert!(properties.spawn(|| Ok(())).is_err());
        assert!(properties.try_recv().is_none());
        assert!(start.elapsed() < Duration::from_millis(50));
        release_tx.send(()).unwrap();
        while properties.try_recv().is_none() {
            assert!(start.elapsed() < Duration::from_secs(3));
            thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn invalid_properties_selection_does_not_start_a_worker() {
        let properties = Properties::new(Arc::new(|| panic!("invalid request woke the UI")));
        for paths in [vec![], vec![PathBuf::from("relative.txt")]] {
            assert!(properties.show_with_owner(0, paths).is_err());
            assert!(!properties.busy.load(Ordering::Acquire));
            assert!(properties.try_recv().is_none());
        }
    }

    #[test]
    fn failed_properties_request_releases_the_independent_lane() {
        let (wake_tx, wake_rx) = mpsc::channel();
        let properties = Properties::new(Arc::new(move || wake_tx.send(()).unwrap()));
        properties
            .spawn(|| Err("fixture shell failure".into()))
            .unwrap();
        wake_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(
            properties.try_recv(),
            Some(Err("fixture shell failure".into()))
        );
        properties.spawn(|| Ok(())).unwrap();
        wake_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert_eq!(properties.try_recv(), Some(Ok(())));
        assert!(properties.try_recv().is_none());
    }
}
