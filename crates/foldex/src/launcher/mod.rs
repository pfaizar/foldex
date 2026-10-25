//! Focused native launcher with bounded results and worker-owned slow providers.
pub(crate) mod applications;
pub mod calculator;
pub mod instance;
pub(crate) mod matching;
pub mod native;
#[cfg(all(test, windows))]
mod native_probe;
pub(crate) mod providers;
mod ui;
use crate::search::{SearchClient, SearchEvent, SearchHub};
use egui::{Id, Key, Modifiers, Rect, Sense, Ui, UiBuilder, pos2, vec2};
use foldex_ui::{
    Icon, Theme, icon,
    motion::{LayoutMotion, MotionProfile},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant},
};

const MAX_FILE_PROVIDERS: usize = 2;
const MAX_FILE_ROWS: usize = 32;
const MAX_FAVORITES: usize = 32;
const MAX_RECENTS: usize = 10;

#[derive(Clone, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub background: bool,
    pub clipboard_history: bool,
    pub hotkey: String,
    pub file_roots: Vec<PathBuf>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            background: true,
            clipboard_history: true,
            hotkey: "Ctrl+Alt+Space".into(),
            file_roots: vec![],
        }
    }
}
pub struct Runtime {
    pub isolate: bool,
    pub auxiliary: bool,
    pub tray_only: bool,
    pub open_launcher: bool,
    pub open_settings: bool,
    pub channel: Option<String>,
    pub secondary: Option<crate::performance::monitor::Monitor>,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    All,
    Apps,
    Files,
    Windows,
    Calculator,
    Clipboard,
    Actions,
    Settings,
}
#[derive(Clone, Copy)]
pub enum Builtin {
    Explorer,
    Settings,
    Files,
    Apps,
    Windows,
    Clipboard,
    Calculator,
    Exit,
    Dismiss,
}
#[derive(Clone)]
pub enum ResultAction {
    Open(PathBuf),
    Browse(PathBuf),
    /// Show the entry's location and selection inside the main Foldex explorer.
    Reveal(PathBuf),
    Application(Arc<[u8]>),
    Window(isize),
    Copy(String),
    Builtin(Builtin),
}
impl ResultAction {
    fn reveal_path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Open(path) | Self::Browse(path) | Self::Reveal(path) if path.is_absolute() => {
                Some(path)
            }
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultGroup {
    Favorites,
    Recents,
    Apps,
    Files,
    Windows,
    Calculator,
    Clipboard,
    Actions,
}
impl ResultGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::Favorites => "Favorites",
            Self::Recents => "Recents",
            Self::Apps => "Apps",
            Self::Files => "Files",
            Self::Windows => "Windows",
            Self::Calculator => "Calculator",
            Self::Clipboard => "Clipboard",
            Self::Actions => "Actions",
        }
    }
}
pub struct ResultRow {
    pub id: u64,
    pub title: String,
    pub detail: String,
    pub icon: Icon,
    pub action: ResultAction,
    pub score: i32,
    pub group: ResultGroup,
}
pub fn row(
    title: String,
    detail: String,
    icon: Icon,
    action: ResultAction,
    score: i32,
) -> ResultRow {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    title.hash(&mut hasher);
    detail.hash(&mut hasher);
    match &action {
        ResultAction::Open(path) => {
            0.hash(&mut hasher);
            path.hash(&mut hasher);
        }
        ResultAction::Window(hwnd) => {
            1.hash(&mut hasher);
            hwnd.hash(&mut hasher);
        }
        ResultAction::Copy(text) => {
            2.hash(&mut hasher);
            text.hash(&mut hasher);
        }
        ResultAction::Builtin(b) => {
            3.hash(&mut hasher);
            (*b as u8).hash(&mut hasher);
        }
        ResultAction::Browse(path) => {
            4.hash(&mut hasher);
            path.hash(&mut hasher);
        }
        ResultAction::Application(id) => {
            5.hash(&mut hasher);
            id.hash(&mut hasher);
        }
        ResultAction::Reveal(path) => {
            6.hash(&mut hasher);
            path.hash(&mut hasher);
        }
    }
    let group = match &action {
        ResultAction::Open(_) | ResultAction::Browse(_) | ResultAction::Reveal(_) => {
            ResultGroup::Files
        }
        ResultAction::Application(_) => ResultGroup::Apps,
        ResultAction::Window(_) => ResultGroup::Windows,
        ResultAction::Copy(_) => ResultGroup::Clipboard,
        ResultAction::Builtin(_) => ResultGroup::Actions,
    };
    ResultRow {
        id: hasher.finish(),
        title,
        detail,
        icon,
        action,
        score,
        group,
    }
}
#[derive(Clone)]
struct Request {
    epoch: u64,
    query: String,
    mode: Mode,
    default_file_roots: bool,
    current_folder: PathBuf,
    favorites: Arc<[PathBuf]>,
    recents: Arc<[PathBuf]>,
}
struct Response {
    epoch: u64,
    rows: Vec<Arc<ResultRow>>,
    calculator: Option<Arc<ResultRow>>,
    default_roots: Option<Vec<PathBuf>>,
    complete: bool,
}

/// A valid edit can retain the painted answer while its provider generation is
/// pending. Only a reply for this generation makes the answer actionable.
#[derive(Default)]
struct CalculatorResult {
    display: Option<Arc<ResultRow>>,
    epoch: u64,
    pending: bool,
}
impl CalculatorResult {
    fn begin(&mut self, epoch: u64, candidate: Option<Arc<ResultRow>>) {
        self.epoch = epoch;
        self.pending = candidate.is_some();
        // An invalid expression or a different provider must remove the old
        // answer immediately. The first valid answer needs no entry animation.
        if candidate.is_none() || self.display.is_none() {
            self.display = candidate;
        }
    }

    fn finish(&mut self, epoch: u64, answer: Option<Arc<ResultRow>>) {
        if self.epoch == epoch {
            self.display = answer;
            self.pending = false;
        }
    }

    fn ready(&self, epoch: u64, query: &str) -> bool {
        self.epoch == epoch
            && !self.pending
            && self
                .display
                .as_ref()
                .is_some_and(|row| row.detail.trim() == query.trim())
    }
}

fn calculator_row(mode: Mode, query: &str) -> Option<Arc<ResultRow>> {
    let answer = match mode {
        Mode::All => calculator::automatic(query),
        Mode::Calculator => calculator::calculate(query),
        _ => None,
    }?;
    let mut result = row(
        answer.clone(),
        query.to_owned(),
        Icon::Calculator,
        ResultAction::Copy(answer),
        i32::MAX,
    );
    // Selection and animation belong to this calculator slot, not its value.
    result.id = Id::new("launcher-calculator-slot").value();
    result.group = ResultGroup::Calculator;
    Some(Arc::new(result))
}
struct CopyJob {
    id: u64,
    text: String,
}
struct WorkerState {
    request: Mutex<Option<Request>>,
    result: Mutex<Option<Response>>,
    copy: Mutex<Option<CopyJob>>,
    copied: Mutex<Option<(u64, Result<(), String>)>>,
    epoch: AtomicU64,
    stop: AtomicBool,
    clear_history: AtomicBool,
    signal: SyncSender<()>,
    wake: Arc<dyn Fn() + Send + Sync>,
}
struct FileProvider {
    root: PathBuf,
    client: SearchClient,
    request: u64,
    rows: Vec<Arc<ResultRow>>,
    scanned: u64,
    building: bool,
    generation: String,
    completed_scanned: u64,
    advance: bool,
}
pub struct Launcher {
    pub native: native::Native,
    pub settings: Settings,
    pub mode: Mode,
    pub query: String,
    pub settings_changed: bool,
    pub rows: Vec<Arc<ResultRow>>,
    pub selected: usize,
    pub status: String,
    worker: Arc<WorkerState>,
    provider_rows: Vec<Arc<ResultRow>>,
    files: Vec<FileProvider>,
    file_roots: Vec<PathBuf>,
    remaining_roots: VecDeque<PathBuf>,
    favorites: Arc<[PathBuf]>,
    recents: Arc<[PathBuf]>,
    focus: bool,
    dirty: bool,
    scroll_to: bool,
    motion: LayoutMotion,
    copy_id: u64,
    pending_copy: Option<u64>,
    provider_pending: bool,
    calculator: CalculatorResult,
    calculator_numbers: foldex_ui::rolling::RollingNumbers,
    calculator_paint: ui::CalculatorPaint,
    hotkey_edit: String,
    roots_edit: String,
    settings_error: Option<String>,
    current_folder: PathBuf,
}
impl Launcher {
    pub fn new(
        mut settings: Settings,
        hub: &SearchHub,
        wake: Arc<dyn Fn() + Send + Sync>,
        isolate: bool,
        channel: Option<String>,
    ) -> std::io::Result<Self> {
        if settings.hotkey.len() > 64
            || !settings.hotkey.is_empty() && native::parse_hotkey(&settings.hotkey).is_none()
        {
            settings.hotkey = Settings::default().hotkey;
        }
        settings.file_roots.truncate(2);
        let (signal, receiver) = mpsc::sync_channel(1);
        let native = native::Native::new(
            !isolate && settings.background,
            !isolate && settings.clipboard_history,
            settings.hotkey.clone(),
            wake.clone(),
            signal.clone(),
            isolate,
            channel,
        )?;
        let worker = Arc::new(WorkerState {
            request: Mutex::new(None),
            result: Mutex::new(None),
            copy: Mutex::new(None),
            copied: Mutex::new(None),
            epoch: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            clear_history: AtomicBool::new(false),
            signal,
            wake,
        });
        let state = worker.clone();
        let native_state = native.state.clone();
        std::thread::Builder::new()
            .name("foldex-launcher".into())
            .stack_size(512 * 1024)
            .spawn(move || worker_loop(state, native_state, receiver, isolate))?;
        let files = (0..MAX_FILE_PROVIDERS)
            .map(|_| FileProvider {
                root: PathBuf::new(),
                client: hub.client(),
                request: 0,
                rows: vec![],
                scanned: 0,
                building: false,
                generation: String::new(),
                completed_scanned: 0,
                advance: false,
            })
            .collect();
        Ok(Self {
            hotkey_edit: settings.hotkey.clone(),
            roots_edit: settings
                .file_roots
                .iter()
                .map(|p| crate::pane::friendly_path(p))
                .collect::<Vec<_>>()
                .join("; "),
            native,
            settings,
            mode: Mode::All,
            query: String::new(),
            settings_changed: false,
            rows: vec![],
            selected: 0,
            status: String::new(),
            worker,
            provider_rows: vec![],
            files,
            file_roots: vec![],
            remaining_roots: VecDeque::new(),
            favorites: Arc::from([]),
            recents: Arc::from([]),
            focus: true,
            dirty: true,
            scroll_to: true,
            motion: LayoutMotion::new(128),
            copy_id: 0,
            pending_copy: None,
            provider_pending: false,
            calculator: CalculatorResult::default(),
            calculator_numbers: foldex_ui::rolling::RollingNumbers::new(2),
            calculator_paint: ui::CalculatorPaint::default(),
            settings_error: None,
            current_folder: PathBuf::from("C:/"),
        })
    }
    pub fn open(&mut self, current: PathBuf) {
        self.native.capture_previous();
        self.native.set_opened(true);
        self.focus = true;
        self.query.clear();
        self.mode = Mode::All;
        self.selected = 0;
        self.scroll_to = true;
        self.current_folder = current;
        self.update_roots();
        self.request();
    }
    fn update_roots(&mut self) {
        self.file_roots = if self.settings.file_roots.is_empty() {
            // Resolve and validate Windows known folders on the provider worker.
            vec![]
        } else {
            providers::distinct_roots(self.settings.file_roots.iter().take(2).cloned())
        };
    }
    /// Feed the persisted explorer session before opening the launcher. Existing
    /// paths are checked on the provider worker, never while painting the UI.
    pub fn set_discovery_paths(&mut self, favorites: Vec<PathBuf>, recents: Vec<PathBuf>) {
        self.favorites = favorites
            .into_iter()
            .take(MAX_FAVORITES)
            .collect::<Vec<_>>()
            .into();
        self.recents = recents
            .into_iter()
            .take(MAX_RECENTS)
            .collect::<Vec<_>>()
            .into();
        if self.native.state.opened.load(Ordering::Acquire) {
            self.request();
        }
    }
    pub fn close(&mut self) {
        self.native.set_opened(false);
        self.provider_pending = false;
        self.pending_copy = None;
        self.worker.epoch.fetch_add(1, Ordering::AcqRel);
        *self
            .worker
            .request
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let _ = self.worker.signal.try_send(());
        self.provider_rows.clear();
        self.rows.clear();
        self.calculator = CalculatorResult::default();
        self.calculator_numbers.clear();
        self.calculator_paint = ui::CalculatorPaint::default();
        self.remaining_roots.clear();
        for file in &mut self.files {
            file.client.cancel();
            file.client.set_active(false);
            file.rows.clear();
            file.building = false;
            file.request = 0;
            file.advance = false;
        }
        self.motion.clear();
    }
    pub fn previous(&self) -> isize {
        self.native.previous()
    }
    pub fn activate_window(target: isize) -> bool {
        providers::activate_window(target)
    }
    pub fn status_error(&self) -> Option<String> {
        self.native
            .state
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn request(&mut self) {
        let epoch = self.worker.epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.provider_pending = true;
        self.pending_copy = None;
        self.provider_rows.clear();
        self.calculator
            .begin(epoch, calculator_row(self.mode, &self.query));
        if self.calculator.display.is_none() {
            self.calculator_numbers.clear();
            self.calculator_paint = ui::CalculatorPaint::default();
        }
        *self
            .worker
            .request
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(Request {
            epoch,
            query: self.query.clone(),
            mode: self.mode,
            default_file_roots: self.settings.file_roots.is_empty(),
            current_folder: self.current_folder.clone(),
            favorites: self.favorites.clone(),
            recents: self.recents.clone(),
        });
        let _ = self.worker.signal.try_send(());
        self.selected = 0;
        self.scroll_to = true;
        self.dirty = true;
        self.request_files();
        // Keep the calculator scene painted in the input frame. A valid edit
        // holds its last answer until the current worker generation commits;
        // `dirty` still guards activation in this input frame.
        self.rebuild();
    }
    fn request_files(&mut self) {
        let search = matches!(self.mode, Mode::All | Mode::Files)
            && self.query.trim().chars().nth(1).is_some()
            && !providers::path_intent(&self.query)
            && !(self.mode == Mode::All && calculator::automatic(&self.query).is_some());
        self.remaining_roots = if search {
            self.file_roots.iter().cloned().collect()
        } else {
            VecDeque::new()
        };
        for file in &mut self.files {
            file.rows.clear();
            file.scanned = 0;
            file.completed_scanned = 0;
            file.advance = false;
            file.generation.clear();
            if let Some(root) = self.remaining_roots.pop_front() {
                file.building = true;
                file.root = root;
                file.client.set_active(true);
                file.request = file.client.request_launcher(
                    file.root.clone(),
                    self.query.clone(),
                    !self.settings.file_roots.is_empty(),
                );
            } else {
                file.building = false;
                file.request = 0;
                file.client.cancel();
                file.client.set_active(false);
            }
        }
    }
    pub fn poll(&mut self) -> Option<bool> {
        let response = self
            .worker
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(response) = response {
            self.accept_response(response);
        }
        for file in &mut self.files {
            for _ in 0..4 {
                match file.client.try_recv() {
                    Some(SearchEvent::Launcher {
                        request,
                        rows,
                        status,
                        complete,
                    }) if request == file.request => {
                        if file.generation != status.generation {
                            file.generation.clone_from(&status.generation);
                            file.rows.retain(|row| match &row.action {
                                ResultAction::Open(path) | ResultAction::Browse(path) => {
                                    !contains_path(&file.root, path)
                                }
                                _ => false,
                            });
                        }
                        file.scanned = file.completed_scanned.saturating_add(status.entries);
                        file.building = !complete;
                        file.advance = complete;
                        file.rows.extend(rows.into_iter().map(|result| {
                            Arc::new(providers::scored_file_row(
                                result.path,
                                result.is_dir,
                                result.score,
                            ))
                        }));
                        sort_rows(&mut file.rows);
                        let mut seen = std::collections::HashSet::new();
                        file.rows.retain(|row| seen.insert(row.id));
                        file.rows.truncate(MAX_FILE_ROWS);
                        self.dirty = true;
                    }
                    Some(SearchEvent::Progress { request, status }) if request == file.request => {
                        file.scanned = file.completed_scanned.saturating_add(status.entries);
                        // Physical traversal completion is not matching completion.
                        // The launcher event alone may advance the scope queue.
                    }
                    Some(SearchEvent::Failed { request, message }) if request == file.request => {
                        file.building = false;
                        file.advance = true;
                        self.status = message;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
        self.advance_files();
        if self.dirty {
            self.rebuild();
            self.dirty = false;
        }
        let copied = self
            .worker
            .copied
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some((id, result)) = copied
            && self.pending_copy == Some(id)
        {
            self.pending_copy = None;
            match result {
                Ok(()) => return Some(true),
                Err(error) => {
                    self.status = error;
                    return Some(false);
                }
            }
        }
        None
    }
    fn accept_response(&mut self, response: Response) {
        if response.epoch != self.worker.epoch.load(Ordering::Acquire) {
            return;
        }
        self.provider_rows = response.rows;
        self.provider_pending = !response.complete;
        if response.complete {
            self.calculator.finish(response.epoch, response.calculator);
        }
        if self.settings.file_roots.is_empty()
            && let Some(roots) = response.default_roots
            && self.file_roots != roots
        {
            self.file_roots = roots;
            self.request_files();
        }
        self.dirty = true;
    }
    fn advance_files(&mut self) {
        for file in &mut self.files {
            if file.advance {
                file.advance = false;
                if let Some(root) = self.remaining_roots.pop_front() {
                    file.completed_scanned = file.scanned;
                    file.root = root;
                    file.generation.clear();
                    file.building = true;
                    file.request = file.client.request_launcher(
                        file.root.clone(),
                        self.query.clone(),
                        !self.settings.file_roots.is_empty(),
                    );
                }
            }
        }
    }
    fn rebuild(&mut self) {
        let previous = self.rows.get(self.selected).map(|row| row.id);
        self.rows.clone_from(&self.provider_rows);
        if let Some(answer) = &self.calculator.display {
            self.rows.insert(0, answer.clone());
        }
        if matches!(self.mode, Mode::All | Mode::Files) {
            for file in &self.files {
                self.rows.extend(file.rows.iter().cloned());
            }
        }
        sort_rows(&mut self.rows);
        let mut seen = std::collections::HashSet::new();
        self.rows.retain(|row| seen.insert(row.id));
        self.rows.truncate(96);
        if let Some(at) = previous.and_then(|id| self.rows.iter().position(|row| row.id == id)) {
            self.selected = at;
        } else {
            self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        }
    }
    fn row_action_ready(&self, at: usize) -> bool {
        !self.dirty
            && self.rows.get(at).is_some_and(|row| {
                row.group != ResultGroup::Calculator
                    || self
                        .calculator
                        .ready(self.worker.epoch.load(Ordering::Acquire), &self.query)
                        && self
                            .calculator
                            .display
                            .as_ref()
                            .is_some_and(|answer| Arc::ptr_eq(answer, row))
            })
    }
    pub fn copy(&mut self, text: String) {
        self.copy_id = self.copy_id.wrapping_add(1);
        self.pending_copy = Some(self.copy_id);
        *self.worker.copy.lock().unwrap_or_else(|e| e.into_inner()) = Some(CopyJob {
            id: self.copy_id,
            text,
        });
        let _ = self.worker.signal.try_send(());
    }
    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.focus = true;
            self.request();
        }
    }
    pub fn set_query(&mut self, query: &str) {
        if self.query != query {
            self.query = query.chars().take(512).collect();
            self.status.clear();
            self.request();
        }
    }
    pub fn pending(&self) -> bool {
        self.provider_pending
    }
    pub fn searching_files(&self) -> bool {
        !self.remaining_roots.is_empty() || self.files.iter().any(|file| file.building)
    }
}

fn contains_path(root: &std::path::Path, path: &std::path::Path) -> bool {
    // Verbatim prefixes and ASCII case do not create overlapping default scans.
    // This compares raw Windows units; it never converts identities to lossy text.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let normal = |path: &std::path::Path| {
            let mut units: Vec<_> = path
                .as_os_str()
                .encode_wide()
                .map(|unit| if unit == 47 { 92 } else { unit })
                .collect();
            if units.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
                units.splice(0..8, [92, 92]);
            } else if units.starts_with(&[92, 92, 63, 92]) {
                units.drain(0..4);
            }
            for unit in &mut units {
                if (65..=90).contains(unit) {
                    *unit += 32;
                }
            }
            while units.last() == Some(&92) {
                units.pop();
            }
            units
        };
        let root = normal(root);
        let path = normal(path);
        path == root || path.starts_with(&root) && path.get(root.len()) == Some(&92)
    }
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
}
impl Drop for Launcher {
    fn drop(&mut self) {
        self.worker.stop.store(true, Ordering::Release);
        let _ = self.worker.signal.try_send(());
    }
}

fn sort_rows(rows: &mut [Arc<ResultRow>]) {
    rows.sort_unstable_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.title.cmp(&b.title))
            .then_with(|| a.id.cmp(&b.id))
    });
}

// Rank borrowed candidates, copying only the best 64. An empty query over an
// 8,192-app catalog does not allocate 8,192 temporary result rows.
fn ranked(
    candidates: &[Arc<ResultRow>],
    matcher: &mut matching::Matcher,
    bias: i32,
    current: &dyn Fn() -> bool,
) -> Vec<Arc<ResultRow>> {
    let mut best: Vec<(usize, i32)> = Vec::with_capacity(64);
    for (index, candidate) in candidates.iter().enumerate() {
        if !current() {
            return Vec::new();
        }
        if matcher.is_empty() && candidate.score < 0 {
            continue;
        }
        let Some(score) = (if candidate.group == ResultGroup::Apps {
            matcher.app_score(&candidate.title)
        } else {
            matcher.score(&candidate.title)
        })
        .map(|score| score.saturating_add(bias).saturating_add(candidate.score)) else {
            continue;
        };
        let at = best.partition_point(|(old, old_score)| {
            *old_score > score
                || *old_score == score
                    && (candidates[*old].title < candidate.title
                        || candidates[*old].title == candidate.title
                            && candidates[*old].id <= candidate.id)
        });
        if at < 64 {
            if best.len() == 64 {
                best.pop();
            }
            best.insert(at, (index, score));
        }
    }
    best.into_iter()
        .map(|(index, score)| {
            let candidate = &candidates[index];
            Arc::new(ResultRow {
                id: candidate.id,
                title: candidate.title.clone(),
                detail: candidate.detail.clone(),
                icon: candidate.icon,
                action: candidate.action.clone(),
                score,
                group: candidate.group,
            })
        })
        .collect()
}

/// The empty-query order depends only on the catalog's existing utility scores.
/// Keep its bounded result window until the five-minute catalog refresh, so a
/// warm open only clones Arcs instead of rescoring and copying app titles/paths.
#[derive(Default)]
struct AppCatalog {
    candidates: Vec<Arc<ResultRow>>,
    discovery: Option<Vec<Arc<ResultRow>>>,
}
impl AppCatalog {
    fn replace(&mut self, candidates: Vec<Arc<ResultRow>>) {
        self.candidates = candidates;
        self.discovery = None;
    }

    fn ranked(
        &mut self,
        matcher: &mut matching::Matcher,
        discovery_limit: usize,
        current: &dyn Fn() -> bool,
    ) -> Vec<Arc<ResultRow>> {
        if !current() {
            return Vec::new();
        }
        if !matcher.is_empty() {
            return ranked(&self.candidates, matcher, 700, current);
        }
        if self.discovery.is_none() {
            let rows = ranked(&self.candidates, matcher, 100_000, current);
            if !current() {
                return Vec::new();
            }
            self.discovery = Some(rows);
        }
        self.discovery
            .as_ref()
            .unwrap()
            .iter()
            .take(discovery_limit)
            .cloned()
            .collect()
    }
}

#[derive(Default)]
struct History {
    rows: VecDeque<String>,
    bytes: usize,
}
impl History {
    fn clear(&mut self) {
        self.rows.clear();
        self.bytes = 0;
    }
    fn insert(&mut self, text: String) {
        if text.len() > 64 * 1024 || text.trim().is_empty() {
            return;
        }
        if let Some(at) = self.rows.iter().position(|old| *old == text) {
            self.bytes -= self.rows.remove(at).unwrap().len();
        }
        self.bytes += text.len();
        self.rows.push_front(text);
        while self.rows.len() > 100 || self.bytes > 2 * 1024 * 1024 {
            if let Some(old) = self.rows.pop_back() {
                self.bytes -= old.len();
            }
        }
    }
}

fn worker_loop(
    shared: Arc<WorkerState>,
    native: Arc<native::State>,
    receiver: mpsc::Receiver<()>,
    isolate: bool,
) {
    let mut apps = AppCatalog::default();
    let mut app_time: Option<Instant> = None;
    let mut windows = vec![];
    let mut window_time = Instant::now() - Duration::from_secs(5);
    let mut clipboard_version = 0;
    let mut history = History::default();
    let mut previous: Option<Request> = None;
    loop {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let mut history_changed = shared.clear_history.swap(false, Ordering::AcqRel);
        if history_changed || !native.remembers_clipboard() {
            history_changed |= !history.rows.is_empty();
            history.clear();
        }
        let version = native.clipboard_version.load(Ordering::Acquire);
        if !isolate && native.remembers_clipboard() && clipboard_version != version {
            clipboard_version = version;
            if let Some(text) = providers::clipboard_text() {
                history.insert(text);
                history_changed = true;
            }
        }
        let copy = shared.copy.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(job) = copy {
            let result = if isolate {
                Err("Clipboard writes are disabled in isolated benchmarks".into())
            } else {
                providers::write_clipboard(&job.text, native.window())
                    .map_err(|error| error.to_string())
            };
            *shared.copied.lock().unwrap_or_else(|e| e.into_inner()) = Some((job.id, result));
            (shared.wake)();
        }
        let mut request = shared
            .request
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if request.is_none() && history_changed {
            request = previous
                .as_ref()
                .filter(|request| {
                    request.mode == Mode::Clipboard
                        && request.epoch == shared.epoch.load(Ordering::Acquire)
                })
                .cloned();
        }
        if let Some(request) = request {
            let current = || {
                !shared.stop.load(Ordering::Acquire)
                    && shared.epoch.load(Ordering::Acquire) == request.epoch
            };
            if current() {
                let mut matcher = matching::Matcher::new(request.query.trim());
                let empty = matcher.is_empty();
                let calculator = calculator_row(request.mode, &request.query);
                let math = request.mode == Mode::All && calculator.is_some();
                let path = !math && providers::path_intent(&request.query);
                // Only file-name searches use these roots. In particular, app,
                // calculator and clipboard queries need no known-folder calls
                // or directory validation, even on redirected network folders.
                let default_roots = (request.default_file_roots
                    && !math
                    && !path
                    && matches!(request.mode, Mode::All | Mode::Files)
                    && request.query.trim().chars().nth(1).is_some())
                .then(|| providers::default_roots(&request.current_folder, true));
                let mut rows = Vec::with_capacity(128);
                if current() && !math && !path && matches!(request.mode, Mode::All | Mode::Files) {
                    rows.extend(providers::discovery(
                        &request.favorites,
                        &request.recents,
                        &mut matcher,
                        &current,
                    ));
                }
                if current() && path && matches!(request.mode, Mode::All | Mode::Files) {
                    rows.extend(providers::path_rows(
                        &request.query,
                        &request.current_folder,
                        &current,
                    ));
                }
                if current() && !math && !path && matches!(request.mode, Mode::All | Mode::Files) {
                    // Let saved locations and file scopes reach the UI before
                    // a cold installed-app enumeration finishes.
                    sort_rows(&mut rows);
                    *shared.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Response {
                        epoch: request.epoch,
                        rows: rows.clone(),
                        calculator: None,
                        default_roots: default_roots.clone(),
                        complete: false,
                    });
                    (shared.wake)();
                }
                if current() && !math && !path && matches!(request.mode, Mode::All | Mode::Apps) {
                    if app_time.is_none_or(|at| at.elapsed() > Duration::from_secs(300)) {
                        // The catalog is independent of query revisions. Finish
                        // it while typing, but abandon it between native calls
                        // once the launcher closes; never cache a partial scan.
                        let catalog_current = || {
                            !shared.stop.load(Ordering::Acquire)
                                && native.opened.load(Ordering::Acquire)
                        };
                        if let Some(candidates) = providers::applications(&catalog_current) {
                            apps.replace(candidates);
                            app_time = Some(Instant::now());
                        }
                    }
                    if current() {
                        rows.extend(apps.ranked(
                            &mut matcher,
                            if request.mode == Mode::All { 12 } else { 64 },
                            &current,
                        ));
                    }
                }
                if current()
                    && !math
                    && !path
                    && (request.mode == Mode::Windows || request.mode == Mode::All && !empty)
                {
                    if window_time.elapsed() > Duration::from_secs(1) {
                        windows = providers::windows();
                        window_time = Instant::now();
                    }
                    if current() {
                        rows.extend(ranked(&windows, &mut matcher, 100, &current));
                    }
                }
                if current() && request.mode == Mode::Clipboard {
                    for (at, text) in history.rows.iter().enumerate() {
                        if !current() {
                            break;
                        }
                        if let Some(score) = matcher.score(text) {
                            let title = text
                                .lines()
                                .next()
                                .unwrap_or(text)
                                .chars()
                                .take(120)
                                .collect();
                            rows.push(Arc::new(row(
                                title,
                                format!("Text · {} characters · copy again", text.chars().count()),
                                Icon::Copy,
                                ResultAction::Copy(text.clone()),
                                score.saturating_add(1000 - at as i32),
                            )));
                        }
                    }
                }
                if current()
                    && !math
                    && !path
                    && (request.mode == Mode::Actions || request.mode == Mode::All && !empty)
                {
                    for (title, detail, glyph, action) in [
                        (
                            "Open Foldex",
                            "Show your explorer workspaces",
                            Icon::Folder,
                            Builtin::Explorer,
                        ),
                        (
                            "Search files",
                            "Find files in your user folders and selected search scopes",
                            Icon::Search,
                            Builtin::Files,
                        ),
                        (
                            "Search applications",
                            "Find installed desktop and Store apps",
                            Icon::Window,
                            Builtin::Apps,
                        ),
                        (
                            "Clipboard history",
                            "Find and copy recent text",
                            Icon::Copy,
                            Builtin::Clipboard,
                        ),
                        (
                            "Switch window",
                            "Focus an open window",
                            Icon::Window,
                            Builtin::Windows,
                        ),
                        (
                            "Calculator",
                            "Arithmetic, functions, and unit conversions",
                            Icon::Calculator,
                            Builtin::Calculator,
                        ),
                        (
                            "Launcher settings",
                            "Shortcut, tray mode, and file search folders",
                            Icon::Settings,
                            Builtin::Settings,
                        ),
                        (
                            "Exit Foldex",
                            "Close the explorer and background launcher",
                            Icon::Close,
                            Builtin::Exit,
                        ),
                    ] {
                        if !current() {
                            break;
                        }
                        if let Some(score) = providers::title_score(&mut matcher, title) {
                            rows.push(Arc::new(row(
                                title.into(),
                                detail.into(),
                                glyph,
                                ResultAction::Builtin(action),
                                score.saturating_add(if request.query.is_empty() {
                                    500
                                } else {
                                    10
                                }),
                            )));
                        }
                    }
                }
                sort_rows(&mut rows);
                rows.truncate(64);
                if current() {
                    *shared.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Response {
                        epoch: request.epoch,
                        rows,
                        calculator,
                        default_roots,
                        complete: true,
                    });
                    (shared.wake)();
                    previous = Some(request);
                }
            }
        }
        if previous
            .as_ref()
            .is_some_and(|request| request.epoch != shared.epoch.load(Ordering::Acquire))
        {
            previous = None;
        }
        if receiver.recv().is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn focused_query_keeps_arrow_navigation_and_enter_off_stale_results() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let hub = SearchHub::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/launcher-input-fixtures"),
            wake.clone(),
        )
        .unwrap();
        let mut launcher = Launcher::new(Settings::default(), &hub, wake, true, None).unwrap();
        launcher.mode = Mode::Actions;
        launcher.rows = vec![
            Arc::new(row(
                "Explorer".into(),
                "Open explorer".into(),
                Icon::Folder,
                ResultAction::Builtin(Builtin::Explorer),
                1,
            )),
            Arc::new(row(
                "Calculator".into(),
                "Calculate".into(),
                Icon::Calculator,
                ResultAction::Builtin(Builtin::Calculator),
                1,
            )),
        ];
        launcher.dirty = false;
        let ctx = egui::Context::default();
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let mut pump = |frame: u32, events| {
            let mut action = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(720.0, 510.0))),
                    time: Some(frame as f64 / 60.0),
                    events,
                    ..Default::default()
                },
                |ui| {
                    if let Some(next) = launcher.ui(ui, &Theme::default()) {
                        action = Some(next);
                    }
                },
            );
            output.textures_delta.clear();
            action
        };
        assert!(pump(0, vec![]).is_none());
        assert!(pump(1, vec![key(Key::ArrowDown)]).is_none());
        assert!(matches!(
            pump(2, vec![key(Key::Enter)]),
            Some(ResultAction::Builtin(Builtin::Calculator))
        ));
        let edited = pump(
            3,
            vec![egui::Event::Text("fresh query".into()), key(Key::Enter)],
        );
        drop(pump);
        assert!(
            edited.is_none(),
            "Query {:?}, focused {:?}",
            launcher.query,
            ctx.memory(|memory| memory.focused())
        );
        assert_eq!(launcher.query, "fresh query");
        assert!(launcher.dirty);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(Id::new("launcher-query"))
        );
    }
    #[test]
    fn ranked_apps_match_full_order_with_only_a_small_result_window() {
        let candidates: Vec<_> = (0..8192)
            .rev()
            .map(|at| {
                Arc::new(row(
                    format!("Example application {at:04}"),
                    "Installed app".into(),
                    Icon::Window,
                    ResultAction::Open(PathBuf::from(format!("app-{at}.exe"))),
                    0,
                ))
            })
            .collect();
        for query in ["", "exa", "57", "application 123"] {
            let mut matcher = matching::Matcher::new(query);
            let limited = ranked(&candidates, &mut matcher, 200, &|| true);
            let mut complete: Vec<_> = candidates
                .iter()
                .filter_map(|candidate| {
                    matcher
                        .score(&candidate.title)
                        .map(|score| (candidate.id, score + 200, candidate.title.as_str()))
                })
                .collect();
            complete.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(b.2)));
            complete.truncate(64);
            assert_eq!(
                limited
                    .iter()
                    .map(|row| (row.id, row.score, row.title.as_str()))
                    .collect::<Vec<_>>(),
                complete
            );
            assert!(limited.len() <= 64);
        }
    }
    #[test]
    fn warm_app_discovery_reuses_ranked_rows_and_refresh_invalidates_them() {
        let candidates: Vec<_> = (0..8192)
            .rev()
            .map(|at| {
                let mut result = row(
                    format!("Example application {at:04}"),
                    "Installed app".into(),
                    Icon::Window,
                    ResultAction::Open(PathBuf::from(format!("app-{at}.lnk"))),
                    at % 5 - 1,
                );
                result.group = ResultGroup::Apps;
                Arc::new(result)
            })
            .collect();
        let expected = ranked(
            &candidates,
            &mut matching::Matcher::new(""),
            100_000,
            &|| true,
        );
        let mut catalog = AppCatalog::default();
        catalog.replace(candidates.clone());
        let cold = catalog.ranked(&mut matching::Matcher::new(""), 64, &|| true);
        assert_eq!(
            cold.iter()
                .map(|row| (row.id, row.score))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|row| (row.id, row.score))
                .collect::<Vec<_>>()
        );
        let visits = std::cell::Cell::new(0);
        let warm = catalog.ranked(&mut matching::Matcher::new(""), 12, &|| {
            visits.set(visits.get() + 1);
            true
        });
        assert_eq!(visits.get(), 1, "Warm opens must not walk the catalog");
        assert_eq!(warm.len(), 12);
        assert!(warm.iter().zip(&cold).all(|(a, b)| Arc::ptr_eq(a, b)));

        // Typed queries retain the full matcher and 64-result window, even
        // when the All-mode discovery preview is limited to twelve apps.
        let query = "application 123";
        let typed = catalog.ranked(&mut matching::Matcher::new(query), 12, &|| true);
        let expected = ranked(
            &candidates,
            &mut matching::Matcher::new(query),
            700,
            &|| true,
        );
        assert_eq!(
            typed
                .iter()
                .map(|row| (row.id, row.score))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|row| (row.id, row.score))
                .collect::<Vec<_>>()
        );

        let replacement = vec![Arc::new(row(
            "Newly installed application".into(),
            "Installed app".into(),
            Icon::Window,
            ResultAction::Open(PathBuf::from("new-app.lnk")),
            0,
        ))];
        catalog.replace(replacement);
        assert!(catalog.discovery.is_none());
        let refreshed = catalog.ranked(&mut matching::Matcher::new(""), 64, &|| true);
        assert_eq!(refreshed.len(), 1);
        assert_eq!(refreshed[0].title, "Newly installed application");
    }

    #[test]
    fn cancelled_app_discovery_is_not_cached_or_returned_on_a_warm_open() {
        let mut catalog = AppCatalog::default();
        catalog.replace(
            (0..100)
                .map(|at| {
                    Arc::new(row(
                        format!("Application {at}"),
                        "Installed app".into(),
                        Icon::Window,
                        ResultAction::Open(PathBuf::from(format!("app-{at}.lnk"))),
                        0,
                    ))
                })
                .collect(),
        );
        let visits = std::cell::Cell::new(0);
        let cancelled = catalog.ranked(&mut matching::Matcher::new(""), 64, &|| {
            visits.set(visits.get() + 1);
            visits.get() < 5
        });
        assert!(cancelled.is_empty());
        assert!(catalog.discovery.is_none());
        assert_eq!(
            catalog
                .ranked(&mut matching::Matcher::new(""), 64, &|| true)
                .len(),
            64
        );
        assert!(
            catalog
                .ranked(&mut matching::Matcher::new(""), 64, &|| false)
                .is_empty()
        );
    }

    #[test]
    fn useful_apps_lead_discovery_but_helpers_remain_explicitly_searchable() {
        let candidates: Vec<_> = ["A Crash Updater", "Notes", "Calculator", "Windows Terminal"]
            .into_iter()
            .map(|title| {
                let mut result = row(
                    title.into(),
                    "Installed app".into(),
                    Icon::Window,
                    ResultAction::Open(PathBuf::from(format!("{title}.lnk"))),
                    applications::default_priority(title),
                );
                result.group = ResultGroup::Apps;
                Arc::new(result)
            })
            .collect();
        let empty = ranked(
            &candidates,
            &mut matching::Matcher::new(""),
            100_000,
            &|| true,
        );
        assert_eq!(
            empty
                .iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["Calculator", "Windows Terminal", "Notes"]
        );
        let helper = ranked(
            &candidates,
            &mut matching::Matcher::new("crash updater"),
            700,
            &|| true,
        );
        assert_eq!(helper.len(), 1);
        assert_eq!(helper[0].id, candidates[0].id);
        assert_eq!(helper[0].group, ResultGroup::Apps);
    }
    #[test]
    fn substantive_queries_exclude_scattered_app_and_window_matches() {
        let mut candidates = vec![];
        for (index, title) in [
            "Notepad",
            "Windows Software Development Kit",
            "D:\\Workspace\\Project\\QuanTrading\\Code\\terminal-rs — Windows PowerShell - Otty",
        ]
        .into_iter()
        .enumerate()
        {
            let mut result = row(
                title.into(),
                "Provider result".into(),
                Icon::Window,
                if index == 2 {
                    ResultAction::Window(1)
                } else {
                    ResultAction::Open(PathBuf::from(format!("{title}.lnk")))
                },
                0,
            );
            if index != 2 {
                result.group = ResultGroup::Apps;
            }
            candidates.push(Arc::new(result));
        }
        let results = ranked(
            &candidates,
            &mut matching::Matcher::new("notep"),
            700,
            &|| true,
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Notepad");
        assert_eq!(results[0].id, candidates[0].id);
        assert_eq!(results[0].group, ResultGroup::Apps);
    }
    #[test]
    fn mature_launcher_matching_ranks_omitted_words_typos_and_initialisms() {
        let candidates: Vec<_> = [
            "Visual Calendar",
            "Visual Studio Code",
            "Code Visualizer",
            "Windows Software Development Kit",
        ]
        .into_iter()
        .map(|title| {
            let mut result = row(
                title.into(),
                "Installed app".into(),
                Icon::Window,
                ResultAction::Open(PathBuf::from(format!("{title}.lnk"))),
                applications::default_priority(title),
            );
            result.group = ResultGroup::Apps;
            Arc::new(result)
        })
        .collect();
        for query in ["visual code", "visaul code", "visual cdoe", "vsc"] {
            let result = ranked(
                &candidates,
                &mut matching::Matcher::new(query),
                700,
                &|| true,
            );
            assert_eq!(
                result.first().map(|row| row.title.as_str()),
                Some("Visual Studio Code"),
                "{query}"
            );
            assert!(result.iter().all(|row| row.group == ResultGroup::Apps));
        }
    }
    #[test]
    fn cancel_mid_app_ranking_discards_partial_results() {
        let candidates: Vec<_> = (0..100)
            .map(|at| {
                Arc::new(row(
                    format!("Visual Studio Code {at}"),
                    "Installed app".into(),
                    Icon::Window,
                    ResultAction::Open(PathBuf::from(format!("app-{at}.lnk"))),
                    0,
                ))
            })
            .collect();
        let visited = std::cell::Cell::new(0);
        let result = ranked(
            &candidates,
            &mut matching::Matcher::new("visual code"),
            700,
            &|| {
                visited.set(visited.get() + 1);
                visited.get() < 5
            },
        );
        assert!(result.is_empty());
        assert_eq!(visited.get(), 5);
    }
    #[test]
    fn all_calculator_is_immediate_primary_and_keeps_copy_activation() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let hub = SearchHub::new(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/launcher-calculator-fixtures"),
            wake.clone(),
        )
        .unwrap();
        let mut launcher = Launcher::new(Settings::default(), &hub, wake, true, None).unwrap();
        launcher.set_query("2+3*4");
        assert_eq!(launcher.mode as u8, Mode::All as u8);
        assert_eq!(launcher.rows[0].title, "14");
        assert_eq!(launcher.rows[0].group, ResultGroup::Calculator);
        assert!(matches!(&launcher.rows[0].action, ResultAction::Copy(answer) if answer == "14"));
        assert!(
            launcher.dirty,
            "The input frame must still guard stale Enter activation"
        );
        assert!(launcher.files.iter().all(|file| !file.building));
        launcher.set_query("2026-10-23");
        assert!(
            !launcher
                .rows
                .iter()
                .any(|row| row.group == ResultGroup::Calculator)
        );
        launcher.set_query("2026");
        launcher.set_mode(Mode::Calculator);
        assert!(matches!(&launcher.rows[0].action, ResultAction::Copy(answer) if answer == "2026"));
        launcher.close();
    }
    #[test]
    fn calculator_generations_retain_valid_edits_and_reject_reordered_replies() {
        let mut result = CalculatorResult::default();
        let initial = calculator_row(Mode::All, "2+2");
        result.begin(1, initial.clone());
        assert_eq!(result.display.as_ref().unwrap().title, "4");
        assert!(!result.ready(1, "2+2"));
        result.finish(1, initial);
        assert!(result.ready(1, "2+2"));
        let slot = result.display.as_ref().unwrap().id;

        let second = calculator_row(Mode::All, "2+20");
        result.begin(2, second.clone());
        let latest = calculator_row(Mode::All, "2+200");
        result.begin(3, latest.clone());
        result.finish(2, second);
        assert_eq!(result.display.as_ref().unwrap().title, "4");
        assert!(result.pending);
        assert!(!result.ready(3, "2+200"));

        result.finish(3, latest);
        assert_eq!(result.display.as_ref().unwrap().title, "202");
        assert_eq!(result.display.as_ref().unwrap().id, slot);
        assert!(result.ready(3, "2+200"));
        result.finish(1, calculator_row(Mode::All, "2+2"));
        assert_eq!(result.display.as_ref().unwrap().title, "202");

        result.begin(4, calculator_row(Mode::All, "2+"));
        assert!(result.display.is_none());
        assert!(!result.pending);
        result.finish(3, calculator_row(Mode::All, "2+200"));
        assert!(result.display.is_none());
        result.begin(5, calculator_row(Mode::Apps, "2+2"));
        assert!(result.display.is_none());
        result.begin(6, calculator_row(Mode::All, "notepad"));
        assert!(result.display.is_none());
    }
    #[test]
    fn default_scope_queue_uses_two_clients_and_close_cancels_every_scope() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let root =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/launcher-scope-fixtures");
        let hub = SearchHub::new(root.join("cache"), wake.clone()).unwrap();
        let mut launcher = Launcher::new(Settings::default(), &hub, wake, true, None).unwrap();
        let scopes: Vec<_> = ["Documents", "Desktop", "Downloads", "Current"]
            .into_iter()
            .map(|name| root.join(name))
            .collect();
        for scope in &scopes {
            std::fs::create_dir_all(scope).unwrap();
        }
        launcher.file_roots = scopes.clone();
        launcher.query = "report".into();
        launcher.request_files();
        assert_eq!(launcher.files.len(), 2);
        assert_eq!(launcher.files[0].root, scopes[0]);
        assert_eq!(launcher.files[1].root, scopes[1]);
        assert_eq!(launcher.remaining_roots.len(), 2);
        launcher.files[0].scanned = 17;
        launcher.files[0].advance = true;
        launcher.advance_files();
        assert_eq!(launcher.files[0].root, scopes[2]);
        assert_eq!(launcher.files[0].completed_scanned, 17);
        assert_eq!(launcher.remaining_roots.len(), 1);
        launcher.files[1].advance = true;
        launcher.advance_files();
        assert_eq!(launcher.files[1].root, scopes[3]);
        assert!(launcher.remaining_roots.is_empty());
        launcher.provider_pending = true;
        launcher.close();
        assert!(!launcher.pending());
        assert!(
            launcher
                .files
                .iter()
                .all(|file| !file.building && file.request == 0 && file.rows.is_empty())
        );
        assert!(launcher.remaining_roots.is_empty());
    }
    #[test]
    fn clipboard_history_deduplicates_and_obeys_both_budgets() {
        let mut history = History::default();
        for at in 0..150 {
            history.insert(format!("clip-{at}"));
        }
        assert_eq!(history.rows.len(), 100);
        history.insert("clip-75".into());
        assert_eq!(history.rows.front().unwrap(), "clip-75");
        assert_eq!(history.rows.len(), 100);
        for at in 0..50 {
            history.insert(format!("{at:04}{}", "a".repeat(65_532)));
        }
        assert!(history.rows.len() <= 100 && history.bytes <= 2 * 1024 * 1024);
        assert_eq!(
            history.bytes,
            history.rows.iter().map(String::len).sum::<usize>()
        );
        let count = history.rows.len();
        history.insert("x".repeat(65_537));
        assert_eq!(history.rows.len(), count);
        history.clear();
        assert!(history.rows.is_empty());
        assert_eq!(history.bytes, 0);
    }
    #[test]
    fn default_roots_do_not_scan_verbatim_home_twice() {
        assert!(contains_path(
            std::path::Path::new("C:/Users/Name"),
            std::path::Path::new("\\\\?\\c:\\users\\name\\Documents")
        ));
        assert!(!contains_path(
            std::path::Path::new("C:/Users/Name"),
            std::path::Path::new("C:/Users/Namesake")
        ));
        assert!(contains_path(
            std::path::Path::new("\\\\server\\share\\Home"),
            std::path::Path::new("\\\\?\\UNC\\SERVER\\share\\home\\Documents")
        ));
    }
}
