//! Small in-memory operation history. Native callbacks only touch atomics.
use super::{Job, Wake};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const RETAINED: usize = 64;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum State {
    Queued,
    Running,
    Complete,
    Partial,
    Failed,
    Cancelled,
}
impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Complete => "Complete",
            Self::Partial => "Completed with issues",
            Self::Failed => "Failed",
            Self::Cancelled => "Cancelled",
        }
    }
    pub fn active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}
pub struct Operation {
    pub id: u64,
    pub label: &'static str,
    pub description: String,
    state: AtomicU8,
    pub cancel: AtomicBool,
    changed: AtomicBool,
    incomplete: AtomicBool,
    aborted: AtomicBool,
    total: AtomicU64,
    done: AtomicU64,
    created: Instant,
    finished_ms: AtomicU64,
    wake_ms: AtomicU64,
    message: Mutex<String>,
    wake: Wake,
}
impl Operation {
    pub fn state(&self) -> State {
        match self.state.load(Ordering::Acquire) {
            0 => State::Queued,
            1 => State::Running,
            2 => State::Complete,
            3 => State::Partial,
            4 => State::Failed,
            _ => State::Cancelled,
        }
    }
    pub fn elapsed(&self) -> Duration {
        let end = self.finished_ms.load(Ordering::Acquire);
        if self.state().active() {
            self.created.elapsed()
        } else {
            Duration::from_millis(end)
        }
    }
    pub fn message(&self) -> String {
        self.message
            .try_lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }
    /// Windows reports estimated work units, not bytes or a file count.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total.load(Ordering::Acquire);
        (total != 0)
            .then(|| (self.done.load(Ordering::Acquire).min(total) as f64 / total as f64) as f32)
    }
    pub fn request_cancel(&self) {
        if self.state().active() {
            self.cancel.store(true, Ordering::Release);
            (self.wake)();
        }
    }
    pub fn mark_changed(&self) {
        self.changed.store(true, Ordering::Release);
    }
    pub fn progress(&self, total: u32, done: u32) {
        self.total.store(u64::from(total), Ordering::Release);
        self.done.store(u64::from(done), Ordering::Release);
        let now = self.created.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        if self
            .wake_ms
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |last| {
                (now.saturating_sub(last) >= 50).then_some(now)
            })
            .is_ok()
        {
            (self.wake)();
        }
    }
    pub fn start(&self) {
        self.state.store(State::Running as u8, Ordering::Release);
        (self.wake)();
    }
    pub fn finish(&self, result: &io::Result<String>) {
        let changed = self.changed.load(Ordering::Acquire);
        let interrupted = self.aborted.load(Ordering::Acquire)
            || (self.cancel.load(Ordering::Acquire) && result.is_err());
        let state = if interrupted {
            if changed {
                State::Partial
            } else {
                State::Cancelled
            }
        } else if self.incomplete.load(Ordering::Acquire) {
            State::Partial
        } else if result.is_err() {
            if changed {
                State::Partial
            } else {
                State::Failed
            }
        } else {
            State::Complete
        };
        let message = match result {
            Ok(s) => s.clone(),
            Err(e) => e.to_string(),
        };
        *self.message.lock().unwrap_or_else(|e| e.into_inner()) = bounded(&message, 4096);
        self.finished_ms.store(
            self.created.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            Ordering::Release,
        );
        self.state.store(state as u8, Ordering::Release);
        (self.wake)();
    }
}
#[derive(Default)]
pub struct Operations {
    next: AtomicU64,
    records: Mutex<VecDeque<Arc<Operation>>>,
}
impl Operations {
    pub fn track(&self, job: &Job, wake: Wake) -> Result<Option<Arc<Operation>>, String> {
        let (label, description) = match job {
            Job::NewFile { parent, name, .. } => {
                ("New file", parent.join(name).display().to_string())
            }
            Job::NewFolder { parent, name, .. } => {
                ("New folder", parent.join(name).display().to_string())
            }
            Job::Rename { path, name, .. } => ("Rename", format!("{} → {name}", path.display())),
            Job::Copy {
                sources,
                destination,
                cut,
            } => (
                if *cut { "Move" } else { "Copy" },
                format!("{} items → {}", sources.len(), destination.display()),
            ),
            Job::DropFiles {
                sources,
                destination,
                ..
            } => (
                "Transfer",
                format!("{} items → {}", sources.len(), destination.display()),
            ),
            Job::Recycle(paths) => ("Recycle", format!("{} selected items", paths.len())),
            Job::DeletePermanent(paths) => (
                "Permanent delete",
                format!("{} selected items", paths.len()),
            ),
            Job::ReadClipboard { destination } => ("Paste", destination.display().to_string()),
            Job::Undo => ("Undo", String::new()),
            Job::Redo => ("Redo", String::new()),
            _ => return Ok(None),
        };
        let mut records = self
            .records
            .try_lock()
            .map_err(|_| "Operation history is busy")?;
        while records.len() >= RETAINED {
            let Some(index) = records.iter().position(|r| !r.state().active()) else {
                return Err("Operation history is full".into());
            };
            records.remove(index);
        }
        let operation = Arc::new(Operation {
            id: self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1),
            label,
            description: bounded(&description, 1024),
            state: AtomicU8::new(State::Queued as u8),
            cancel: AtomicBool::new(false),
            changed: AtomicBool::new(false),
            incomplete: AtomicBool::new(false),
            aborted: AtomicBool::new(false),
            total: AtomicU64::new(0),
            done: AtomicU64::new(0),
            created: Instant::now(),
            finished_ms: AtomicU64::new(0),
            wake_ms: AtomicU64::new(0),
            message: Mutex::new(String::new()),
            wake,
        });
        records.push_back(operation.clone());
        Ok(Some(operation))
    }
    pub fn remove(&self, id: u64) {
        if let Ok(mut records) = self.records.lock() {
            records.retain(|r| r.id != id);
        }
    }
    pub fn snapshot(&self) -> Vec<Arc<Operation>> {
        self.records
            .try_lock()
            .map(|r| r.iter().rev().cloned().collect())
            .unwrap_or_default()
    }
    pub fn clear_completed(&self) {
        if let Ok(mut records) = self.records.try_lock() {
            records.retain(|r| r.state().active());
        }
    }
    pub fn cancel_all(&self) {
        for op in self.snapshot() {
            op.request_cancel();
        }
    }
}
fn bounded(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.into();
    }
    let mut end = limit.saturating_sub(3);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}
thread_local! { static CURRENT: RefCell<Option<Arc<Operation>>> = const { RefCell::new(None) }; }
pub struct Current(Option<Arc<Operation>>);
impl Current {
    pub fn enter(operation: Option<Arc<Operation>>) -> Self {
        Self(CURRENT.with(|c| c.replace(operation)))
    }
}
impl Drop for Current {
    fn drop(&mut self) {
        CURRENT.with(|c| c.replace(self.0.take()));
    }
}
pub fn current() -> Option<Arc<Operation>> {
    CURRENT.with(|c| c.borrow().clone())
}
pub fn cancelled() -> bool {
    current().is_some_and(|op| op.cancel.load(Ordering::Acquire))
}
pub fn changed() {
    if let Some(op) = current() {
        op.mark_changed();
    }
}
#[cfg(windows)]
pub fn native_outcome(incomplete: bool, aborted: bool) {
    if let Some(op) = current() {
        op.incomplete.fetch_or(incomplete, Ordering::AcqRel);
        op.aborted.fetch_or(aborted, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tracked(ops: &Operations) -> Arc<Operation> {
        ops.track(&Job::Undo, Arc::new(|| {})).unwrap().unwrap()
    }
    #[test]
    fn cancellation_and_partial_changes_are_distinct() {
        let ops = Operations::default();
        let op = tracked(&ops);
        op.request_cancel();
        op.finish(&Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Cancelled",
        )));
        assert_eq!(op.state(), State::Cancelled);
        let op = tracked(&ops);
        let _scope = Current::enter(Some(op.clone()));
        changed();
        op.request_cancel();
        op.finish(&Err(io::Error::other("Stopped after one item")));
        assert_eq!(op.state(), State::Partial);
        assert!(op.message().contains("one item"));
    }
    #[test]
    fn cancellation_racing_success_does_not_misreport_completed_work() {
        let ops = Operations::default();
        let op = tracked(&ops);
        op.start();
        op.request_cancel();
        op.finish(&Ok("Done".into()));
        assert_eq!(op.state(), State::Complete);
    }
    #[test]
    fn bounded_history_preserves_active_jobs_and_utf8_messages() {
        let ops = Operations::default();
        let active = tracked(&ops);
        for _ in 0..100 {
            let op = tracked(&ops);
            op.finish(&Ok("é".repeat(5000)));
            assert!(op.message().len() <= 4096);
        }
        assert_eq!(ops.snapshot().len(), RETAINED);
        assert!(ops.snapshot().iter().any(|op| op.id == active.id));
        ops.clear_completed();
        assert_eq!(ops.snapshot().len(), 1);
        ops.cancel_all();
        assert!(active.cancel.load(Ordering::Acquire));
    }
    #[test]
    fn native_work_units_are_clamped_and_unknown_progress_is_indeterminate() {
        let ops = Operations::default();
        let op = tracked(&ops);
        assert_eq!(op.fraction(), None);
        op.progress(100, 120);
        assert_eq!(op.fraction(), Some(1.0));
    }
}
