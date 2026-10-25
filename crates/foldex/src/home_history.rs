//! Bounded interaction history, with metadata checks on one demand-driven worker.
use serde::{Deserialize, Serialize};
mod native;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const MAX_HISTORY: usize = 512;
pub const MAX_RECOMMENDED: usize = 8;
const MAX_PATH_BYTES: usize = 32 * 1024;
const PATH_BUDGET: usize = 512 * 1024;
const RECHECK: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Visited,
    Opened,
    WindowsRecent,
}
impl Access {
    pub fn label(self) -> &'static str {
        match self {
            Self::Visited => "Folder visited",
            Self::Opened => "File opened",
            Self::WindowsRecent => "Windows recent item",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub access: Access,
    pub last_used: i64,
    pub uses: u32,
}
#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub entries: Vec<Entry>,
}
impl History {
    pub fn normalize(&mut self) {
        let mut bytes = 0;
        let mut entries: Vec<Entry> = Vec::with_capacity(self.entries.len().min(MAX_HISTORY));
        self.entries.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        for mut entry in self.entries.drain(..) {
            let weight = path_weight(&entry.path);
            if weight == 0
                || weight > MAX_PATH_BYTES
                || bytes + weight > PATH_BUDGET
                || entries.len() == MAX_HISTORY
            {
                continue;
            }
            if entries.iter().any(|old| same_path(&old.path, &entry.path)) {
                continue;
            }
            entry.uses = entry.uses.min(1_000_000);
            entry.last_used = entry.last_used.max(0);
            bytes += weight;
            entries.push(entry);
        }
        self.entries = entries;
    }
    /// Old sessions have genuine recent paths but no measured dates/counts.
    pub fn migrate_recent(&mut self, paths: impl Iterator<Item = PathBuf>) {
        for path in paths.take(MAX_HISTORY) {
            if !self
                .entries
                .iter()
                .any(|entry| same_path(&entry.path, &path))
            {
                self.entries.push(Entry {
                    path,
                    access: Access::Visited,
                    last_used: 0,
                    uses: 0,
                });
            }
        }
        self.normalize();
    }
    pub fn merge_windows_recent(&mut self, entries: Vec<Entry>) -> bool {
        self.merge_windows_recent_with(entries, |_| {})
    }
    fn merge_windows_recent_with(
        &mut self,
        entries: Vec<Entry>,
        mut imported: impl FnMut(&Entry),
    ) -> bool {
        let mut changed = false;
        for entry in entries.into_iter().take(256) {
            if let Some(old) = self
                .entries
                .iter_mut()
                .find(|old| same_path(&old.path, &entry.path))
            {
                if entry.last_used > old.last_used {
                    old.last_used = entry.last_used;
                    old.access = Access::WindowsRecent;
                    imported(old);
                    changed = true;
                }
            } else {
                self.entries.push(entry);
                imported(self.entries.last().unwrap());
                changed = true;
            }
        }
        if changed {
            self.normalize();
        }
        changed
    }
    pub fn record(&mut self, path: PathBuf, access: Access, at: i64) {
        let weight = path_weight(&path);
        if weight == 0 || weight > MAX_PATH_BYTES {
            return;
        }
        let old = self
            .entries
            .iter()
            .position(|entry| same_path(&entry.path, &path))
            .map(|at| self.entries.remove(at));
        self.entries.insert(
            0,
            Entry {
                path,
                access,
                last_used: at.max(0),
                uses: old.map_or(1, |old| old.uses.saturating_add(1).min(1_000_000)),
            },
        );
        self.entries.truncate(MAX_HISTORY);
        let mut bytes = 0usize;
        self.entries.retain(|entry| {
            bytes = bytes.saturating_add(path_weight(&entry.path));
            bytes <= PATH_BUDGET
        });
    }
}
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}
fn path_weight(path: &Path) -> usize {
    path.as_os_str().len().saturating_mul(2)
}
fn same_path(a: &Path, b: &Path) -> bool {
    crate::bookmarks::path_eq(a, b)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validated {
    pub entry: Entry,
    pub is_dir: bool,
    pub bytes: u64,
    pub modified: i64,
}
impl Validated {
    pub fn reason(&self) -> String {
        if self.entry.uses >= 3 {
            format!(
                "Frequently {} · {} times",
                if self.is_dir { "visited" } else { "opened" },
                self.entry.uses
            )
        } else if self.entry.access == Access::WindowsRecent {
            "Recently used in Windows".into()
        } else if self.entry.last_used == 0 {
            "From your previous visits".into()
        } else {
            format!(
                "Recently {}",
                if self.is_dir { "visited" } else { "opened" }
            )
        }
    }
}
pub fn recommended_at(rows: &[Validated], at: i64) -> Vec<usize> {
    // Frequency matters only within the active 30-day window. Old migrated rows
    // cannot claim a measured frequency or appear ahead of real current usage.
    let rank = |index: usize| {
        let entry = &rows[index].entry;
        let active = entry.last_used > 0 && at.saturating_sub(entry.last_used) <= 30 * 86400;
        (
            active && entry.uses >= 3,
            if active { entry.uses.min(100) } else { 0 },
            entry.last_used,
        )
    };
    // Keep only the exact best eight. The input index breaks ties in the same
    // order as the previous full stable sort, without an N-element allocation.
    let mut order = Vec::with_capacity(rows.len().min(MAX_RECOMMENDED));
    for index in 0..rows.len() {
        let candidate = rank(index);
        let position = order.partition_point(|&old| rank(old) >= candidate);
        if position < MAX_RECOMMENDED {
            if order.len() == MAX_RECOMMENDED {
                order.pop();
            }
            order.insert(position, index);
        }
    }
    order
}

/// First second at which a current frequency can leave the active window.
/// A backwards wall-clock adjustment also invalidates the UI ranking cache.
pub fn recommendation_deadline(rows: &[Validated], at: i64) -> i64 {
    rows.iter()
        .filter(|row| row.entry.last_used > 0)
        .map(|row| row.entry.last_used.saturating_add(30 * 86400 + 1))
        .filter(|deadline| *deadline > at)
        .min()
        .unwrap_or(i64::MAX)
}

struct Request {
    revision: u64,
    entries: Vec<Entry>,
    import_recent: bool,
}
struct Output {
    revision: u64,
    rows: Vec<Validated>,
    imported: Vec<Entry>,
    imported_recent: bool,
}
struct Shared {
    request: Mutex<Option<Request>>,
    output: Mutex<Option<Output>>,
    revision: AtomicU64,
    stop: AtomicBool,
}
pub struct Service {
    shared: Arc<Shared>,
    worker: std::thread::Thread,
    pub rows: Vec<Validated>,
    pub revision: u64,
    requested: u64,
    due: Instant,
    pending: bool,
    import_recent: bool,
    import_due: Instant,
    imported: Vec<Entry>,
}
impl Service {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>, import_recent: bool) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            request: Mutex::new(None),
            output: Mutex::new(None),
            revision: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        let background = shared.clone();
        let handle = std::thread::Builder::new()
            .name("foldex-home-history".into())
            .spawn(move || {
                loop {
                    if background.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let request = background
                        .request
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take();
                    let Some(request) = request else {
                        std::thread::park();
                        continue;
                    };
                    let mut history = History {
                        entries: request.entries,
                    };
                    let cancelled = || {
                        background.stop.load(Ordering::Relaxed)
                            || background.revision.load(Ordering::Relaxed) != request.revision
                    };
                    let mut validator = native::Validator::default();
                    let mut imported = Vec::new();
                    if request.import_recent {
                        let recent = native::recent(&cancelled, &mut validator);
                        if cancelled() {
                            continue;
                        }
                        history.merge_windows_recent_with(recent, |entry| {
                            imported.push(entry.clone());
                        });
                    }
                    let mut rows = Vec::with_capacity(history.entries.len());
                    for entry in history.entries {
                        if cancelled() {
                            break;
                        }
                        if !validator.can_validate(&entry.path) {
                            continue;
                        }
                        let Ok(metadata) = std::fs::metadata(&entry.path) else {
                            continue;
                        };
                        if !metadata.is_dir() && !metadata.is_file() {
                            continue;
                        }
                        let modified = metadata
                            .modified()
                            .ok()
                            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                            .map_or(0, |duration| duration.as_secs().min(i64::MAX as u64) as i64);
                        rows.push(Validated {
                            entry,
                            is_dir: metadata.is_dir(),
                            bytes: metadata.len(),
                            modified,
                        });
                    }
                    if !cancelled() {
                        // Send only new/newer imports back to persistence. Unchanged
                        // Windows history must not repeat its UI-side merge work.
                        imported
                            .retain(|entry| rows.iter().any(|row| row.entry.path == entry.path));
                        *background.output.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(Output {
                                revision: request.revision,
                                rows,
                                imported,
                                imported_recent: request.import_recent,
                            });
                        wake();
                    }
                }
            })?;
        Ok(Self {
            shared,
            worker: handle.thread().clone(),
            rows: vec![],
            revision: 0,
            requested: 0,
            due: Instant::now(),
            pending: false,
            import_recent,
            import_due: Instant::now(),
            imported: vec![],
        })
    }
    pub fn invalidate(&mut self) {
        self.due = Instant::now();
        // Cancel the active generation immediately; the next visible service
        // call replaces the one request slot with the newest bounded history.
        self.requested = self.requested.wrapping_add(1);
        self.shared
            .revision
            .store(self.requested, Ordering::Relaxed);
        self.pending = false;
    }
    pub fn refresh(&mut self) {
        self.invalidate();
        self.import_due = Instant::now();
    }
    pub fn set_import_windows_recent(&mut self, enabled: bool) {
        if self.import_recent != enabled {
            self.import_recent = enabled;
            self.imported.clear();
            self.import_due = Instant::now();
            self.invalidate();
        }
    }
    pub fn pending(&self) -> bool {
        self.pending
    }
    pub fn take_imported(&mut self) -> Vec<Entry> {
        std::mem::take(&mut self.imported)
    }
    /// Visible Home panes need one deadline wake even when the sidebar is hidden.
    /// In-flight IO wakes the UI on completion and needs no polling repaint.
    pub fn schedule_recheck(&self, ctx: &egui::Context) {
        if !self.pending {
            let remaining = self
                .due
                .saturating_duration_since(Instant::now())
                .max(Duration::from_millis(10));
            let predicted = Duration::try_from_secs_f32(ctx.input(|input| input.predicted_dt))
                .unwrap_or_default();
            ctx.request_repaint_after(remaining.saturating_add(predicted));
        }
    }
    /// No blocking lock acquisition and no filesystem calls on the UI thread.
    pub fn service(&mut self, history: &History, visible: bool) {
        if let Ok(mut output) = self.shared.output.try_lock() {
            if let Some(output) = output
                .take()
                .filter(|output| output.revision == self.requested)
            {
                if self.rows != output.rows {
                    self.rows = output.rows;
                    self.revision = self.revision.wrapping_add(1);
                }
                self.imported = output.imported;
                if output.imported_recent {
                    self.import_due = Instant::now() + Duration::from_secs(300);
                }
                self.pending = false;
                self.due = Instant::now() + RECHECK;
            }
        }
        if !visible || self.pending || Instant::now() < self.due {
            return;
        }
        if let Ok(mut request) = self.shared.request.try_lock() {
            self.requested = self.requested.wrapping_add(1);
            self.shared
                .revision
                .store(self.requested, Ordering::Relaxed);
            let import_recent = self.import_recent && Instant::now() >= self.import_due;
            *request = Some(Request {
                revision: self.requested,
                entries: history.entries.iter().take(MAX_HISTORY).cloned().collect(),
                import_recent,
            });
            self.pending = true;
            self.worker.unpark();
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.worker.unpark();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: usize, uses: u32, last_used: i64) -> Validated {
        Validated {
            entry: Entry {
                path: format!("C:/history/{index}.txt").into(),
                access: Access::Opened,
                uses,
                last_used,
            },
            is_dir: false,
            bytes: 1,
            modified: 1,
        }
    }

    // No worker, native calls or filesystem fixtures for generation/scheduling
    // checks. The current thread's unpark token is never waited on by a test.
    fn mailbox_fixture() -> Service {
        Service {
            shared: Arc::new(Shared {
                request: Mutex::new(None),
                output: Mutex::new(None),
                revision: AtomicU64::new(0),
                stop: AtomicBool::new(false),
            }),
            worker: std::thread::current(),
            rows: vec![],
            revision: 0,
            requested: 0,
            due: Instant::now() + RECHECK,
            pending: false,
            import_recent: false,
            import_due: Instant::now(),
            imported: vec![],
        }
    }

    #[test]
    fn bounded_recommendations_match_the_full_reference_sort_including_ties_and_expiry() {
        for at in [1, 2_592_001, 4_000_000, 8_000_000] {
            for length in [0, 1, 7, 8, 9, 64, MAX_HISTORY] {
                let dates = [
                    0,
                    1,
                    at - 2_592_001,
                    at - 2_592_000,
                    at - 2_591_999,
                    at,
                    at + 1,
                ];
                let uses = [0, 1, 2, 3, 4, 99, 100, 101, 1_000_000];
                let rows: Vec<_> = (0..length)
                    .map(|index| {
                        row(
                            index,
                            uses[index * 7 % uses.len()],
                            dates[index * 3 % dates.len()],
                        )
                    })
                    .collect();
                let rank = |index: usize| {
                    let entry = &rows[index].entry;
                    let active =
                        entry.last_used > 0 && at.saturating_sub(entry.last_used) <= 30 * 86400;
                    (
                        active && entry.uses >= 3,
                        if active { entry.uses.min(100) } else { 0 },
                        entry.last_used,
                    )
                };
                let mut expected: Vec<_> = (0..length).collect();
                expected.sort_by(|a, b| rank(*b).cmp(&rank(*a)).then(a.cmp(b)));
                expected.truncate(MAX_RECOMMENDED);
                assert_eq!(
                    recommended_at(&rows, at),
                    expected,
                    "length={length}, time={at}"
                );
            }
        }
    }

    #[test]
    fn invalidations_coalesce_to_latest_history_and_reject_the_old_generation() {
        let mut service = mailbox_fixture();
        service.rows = vec![row(0, 1, 1)];
        service.pending = true;
        *service.shared.output.lock().unwrap() = Some(Output {
            revision: 0,
            rows: vec![row(1, 1, 2)],
            imported: vec![row(1, 1, 2).entry],
            imported_recent: true,
        });
        for _ in 0..100 {
            service.invalidate();
        }
        assert!(!service.pending());
        let mut history = History::default();
        history.record("C:/latest.txt".into(), Access::Opened, 10);
        service.service(&history, true);
        assert_eq!(
            service.rows[0].entry.path,
            PathBuf::from("C:/history/0.txt")
        );
        assert!(service.take_imported().is_empty());
        assert!(service.pending());
        let request = service.shared.request.lock().unwrap().take().unwrap();
        assert_eq!(request.entries, history.entries);
        assert_eq!(
            request.revision,
            service.shared.revision.load(Ordering::Relaxed)
        );
        service.service(&history, true);
        assert!(service.shared.request.lock().unwrap().is_none());
    }

    #[test]
    fn unchanged_validation_keeps_presentation_revision_and_completed_import_sets_its_deadline() {
        let mut service = mailbox_fixture();
        service.rows = vec![row(0, 1, 1)];
        service.revision = 9;
        service.pending = true;
        *service.shared.output.lock().unwrap() = Some(Output {
            revision: service.requested,
            rows: service.rows.clone(),
            imported: vec![],
            imported_recent: true,
        });
        service.service(&History::default(), false);
        assert_eq!(service.revision, 9);
        assert!(!service.pending());
        assert!(
            service.import_due.saturating_duration_since(Instant::now()) > Duration::from_secs(295)
        );
        service.set_import_windows_recent(true);
        service.service(&History::default(), true);
        assert!(
            service
                .shared
                .request
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .import_recent
        );
        // Cancelling this scan must leave import due for its replacement.
        service.invalidate();
        service.service(&History::default(), true);
        assert!(
            service
                .shared
                .request
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .import_recent
        );
        service.set_import_windows_recent(false);
        assert!(!service.pending());
        service.service(&History::default(), true);
        assert!(
            !service
                .shared
                .request
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .import_recent
        );
    }

    #[test]
    fn slow_validation_gets_a_full_idle_interval_before_another_check() {
        let mut service = mailbox_fixture();
        service.pending = true;
        service.due = Instant::now() - RECHECK;
        *service.shared.output.lock().unwrap() = Some(Output {
            revision: service.requested,
            rows: vec![row(0, 1, 1)],
            imported: vec![],
            imported_recent: false,
        });
        service.service(&History::default(), true);
        assert!(!service.pending());
        assert!(service.shared.request.lock().unwrap().is_none());
        assert!(service.due.saturating_duration_since(Instant::now()) > Duration::from_secs(55));
    }

    #[test]
    fn visible_home_idle_has_one_distant_deadline_and_pending_io_has_no_polling_repaint() {
        let mut service = mailbox_fixture();
        let ctx = egui::Context::default();
        let draw = |service: &Service, frame: u64| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    time: Some(frame as f64 / 60.0),
                    ..Default::default()
                },
                |ui| service.schedule_recheck(ui.ctx()),
            );
            let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
            output.textures_delta.clear();
            delay
        };
        for frame in 0..8 {
            let delay = draw(&service, frame);
            if frame >= 3 {
                assert!(delay > Duration::from_secs(55) && delay <= RECHECK);
            }
        }
        service.pending = true;
        for frame in 8..12 {
            assert_eq!(draw(&service, frame), Duration::MAX);
        }
        service.pending = false;
        service.due = Instant::now() + Duration::from_millis(250);
        let delay = draw(&service, 12);
        assert!(delay > Duration::from_millis(100) && delay <= Duration::from_millis(250));
    }

    #[test]
    fn unchanged_windows_imports_produce_no_persistence_delta() {
        let mut history = History::default();
        history.record("C:/actual.txt".into(), Access::Opened, 10);
        let imported = Entry {
            path: "C:/actual.txt".into(),
            access: Access::WindowsRecent,
            uses: 0,
            last_used: 20,
        };
        let mut changes = Vec::new();
        assert!(
            history.merge_windows_recent_with(vec![imported.clone()], |entry| changes
                .push(entry.clone()))
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].uses, 1);
        assert!(!history.merge_windows_recent_with(vec![imported], |_| panic!("unchanged import")));
    }

    #[test]
    fn history_deduplicates_real_interactions_and_keeps_a_fixed_cap() {
        let mut history = History::default();
        history.record("C:/Users/Test/report.pdf".into(), Access::Opened, 10);
        #[cfg(windows)]
        history.record("c:\\users\\test\\REPORT.pdf".into(), Access::Opened, 20);
        #[cfg(not(windows))]
        history.record("C:/Users/Test/report.pdf".into(), Access::Opened, 20);
        assert_eq!(history.entries.len(), 1);
        assert_eq!(history.entries[0].uses, 2);
        for n in 0..700 {
            history.record(format!("C:/files/{n}").into(), Access::Opened, 100 + n);
        }
        assert_eq!(history.entries.len(), MAX_HISTORY);
        assert_eq!(history.entries[0].last_used, 799);
    }
    #[test]
    fn legacy_recent_migration_does_not_invent_usage_dates_or_frequency() {
        let mut history = History::default();
        history.migrate_recent([PathBuf::from("C:/old")].into_iter());
        assert_eq!(history.entries[0].last_used, 0);
        assert_eq!(history.entries[0].uses, 0);
        let restored: History =
            serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
        assert_eq!(restored.entries, history.entries);
    }
    #[test]
    fn stale_frequency_cannot_displace_current_recommendations() {
        let row = |uses, last_used| Validated {
            entry: Entry {
                path: "C:/real".into(),
                access: Access::Opened,
                uses,
                last_used,
            },
            is_dir: false,
            bytes: 0,
            modified: 0,
        };
        let rows = [row(100, 1), row(1, 4_000_000), row(3, 3_999_999)];
        assert_eq!(recommended_at(&rows, 4_000_000), [2, 1, 0]);
    }
    #[test]
    fn recent_import_preserves_measured_frequency_and_does_not_fabricate_counts() {
        let mut history = History::default();
        history.record("C:/actual.txt".into(), Access::Opened, 10);
        history.merge_windows_recent(vec![Entry {
            path: "C:/actual.txt".into(),
            access: Access::WindowsRecent,
            uses: 0,
            last_used: 20,
        }]);
        assert_eq!(history.entries[0].uses, 1);
        assert_eq!(history.entries[0].last_used, 20);
        assert_eq!(history.entries[0].access, Access::WindowsRecent);
        assert!(!history.merge_windows_recent(vec![history.entries[0].clone()]));
    }
    #[test]
    fn worker_publishes_only_existing_local_entries() {
        let root = std::env::temp_dir().join(format!(
            "foldex-home-history-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("real.txt");
        std::fs::write(&file, "real generated test file").unwrap();
        let file = std::fs::canonicalize(file).unwrap();
        let mut history = History::default();
        history.record(root.clone(), Access::Visited, 1);
        history.record(file.clone(), Access::Opened, 2);
        history.record(root.join("missing.txt"), Access::Opened, 3);
        let mut service = Service::new(Arc::new(|| {}), false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while service.revision == 0 {
            service.service(&history, true);
            assert!(
                Instant::now() < deadline,
                "Home validation worker timed out"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(service.rows.len(), 2);
        assert_eq!(service.rows[0].entry.path, file);
        assert!(!service.rows[0].is_dir);
        assert!(service.rows[1].is_dir);
        drop(service);
        std::fs::remove_file(root.join("real.txt")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
