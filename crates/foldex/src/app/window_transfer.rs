//! One-tab handoff between independent browser processes. The launcher remains
//! owned by the original process; control files and auxiliary state live in TEMP.
use super::*;
use crate::workspace::{TabState, WorkspaceState};
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    hash::BuildHasher,
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command},
    sync::mpsc::{self, Receiver},
    time::{SystemTime, UNIX_EPOCH},
};

const VERSION: u32 = 1;
const MAX_BYTES: usize = crate::persistence::MAX_SESSION_BYTES;
const TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(30);
const TAB_MOVE_FAILED: &str = "Couldn't move the tab to a new window";

#[derive(Clone, Serialize, Deserialize)]
struct PaneTransfer {
    filter: String,
    recursive: bool,
    substring: bool,
    home_selected: Option<PathBuf>,
}
impl PaneTransfer {
    fn capture(pane: &Pane) -> Self {
        Self {
            filter: pane.filter.clone(),
            recursive: pane.recursive,
            substring: pane.query.match_mode == foldex_match::MatchMode::Substring,
            home_selected: pane.home_selected.clone(),
        }
    }
    fn apply(self, pane: &mut Pane) {
        pane.filter = self.filter;
        pane.filter_lower = pane.filter.to_lowercase();
        pane.query.text = pane.filter.clone();
        pane.query.match_mode = if self.substring {
            foldex_match::MatchMode::Substring
        } else {
            foldex_match::MatchMode::Fuzzy
        };
        // Dormant panes start work only after the destination has committed.
        // Virtual routes must never turn their fallback path into a scan.
        pane.recursive = self.recursive
            && matches!(
                pane.route(),
                crate::navigation::Route::Folder(_) | crate::navigation::Route::Search(_)
            );
        pane.needs_refresh = pane.recursive;
        pane.home_selected = self.home_selected;
    }
}

#[derive(Serialize, Deserialize)]
struct Payload {
    session: Session,
    panes: Vec<PaneTransfer>,
    theme: String,
    position: Option<[f32; 2]>,
    #[serde(default)]
    probe_report: Option<PathBuf>,
    #[serde(default)]
    probe_exit_after_commit: bool,
}
#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    token: String,
    expires_ms: u64,
    payload: Payload,
}
#[derive(Clone)]
struct Ticket {
    directory: PathBuf,
    token: String,
    expires_ms: u64,
}
#[derive(Serialize, Deserialize)]
struct Ready {
    token: String,
    process: u32,
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Commit,
    Cancel,
}
#[derive(Serialize, Deserialize)]
struct Outcome {
    token: String,
    decision: Decision,
}
enum Event {
    Ready(Ticket),
    Activated(Ticket),
    Failed(String),
}
struct Pending {
    source: u64,
    fingerprint: Vec<u8>,
    events: Receiver<Event>,
    committed: bool,
}
pub(crate) struct Incoming {
    ticket: Ticket,
    panes: Vec<PaneTransfer>,
    announced: bool,
    committed_frame: Option<u64>,
    activated: bool,
    deadline: Instant,
    expected: serde_json::Value,
    probe_report: Option<PathBuf>,
    probe_exit_after_commit: bool,
}
#[derive(Default)]
pub(super) struct Transfers {
    outgoing: Option<Pending>,
    incoming: Option<Incoming>,
    pub(super) completion: Option<Result<(), String>>,
    pub(super) ready_acknowledged: bool,
    child_probe: Option<ChildProbe>,
}
struct ChildProbe {
    path: PathBuf,
    expected: serde_json::Value,
    frames: usize,
}
impl Transfers {
    pub(super) fn busy(&self) -> bool {
        self.outgoing.is_some() || self.incoming.is_some()
    }
    pub(super) fn receiving(&self) -> bool {
        self.incoming.is_some()
    }
}

pub(crate) struct Startup {
    pub session: Session,
    pub theme: String,
    pub position: Option<[f32; 2]>,
    pub incoming: Incoming,
}
impl Startup {
    pub fn state_directory(&self) -> PathBuf {
        self.incoming.ticket.directory.join("state")
    }
    pub fn is_probe(&self) -> bool {
        self.incoming.probe_report.is_some() || self.incoming.probe_exit_after_commit
    }
    pub fn cleanup(&self) -> Cleanup {
        Cleanup(self.incoming.ticket.directory.clone())
    }
}
pub(crate) struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        // Only directories allocated by this protocol can be removed. The
        // guard outlives eframe and therefore every UI-owned session writer.
        if owned_directory(&self.0) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn base_directory() -> PathBuf {
    std::env::temp_dir().join("foldex-tab-transfers")
}
fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}
fn owned_directory(directory: &Path) -> bool {
    let base = base_directory();
    directory.is_absolute()
        && directory.parent() == Some(base.as_path())
        && directory
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(valid_token)
        && [directory, base.as_path()]
            .iter()
            .all(|path| fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_symlink()))
        && directory
            .canonicalize()
            .ok()
            .is_some_and(|resolved| base.canonicalize().ok().as_deref() == resolved.parent())
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn unique_token() -> String {
    let random = std::collections::hash_map::RandomState::new();
    let mut first = random.build_hasher();
    std::process::id().hash(&mut first);
    SystemTime::now().hash(&mut first);
    let mut second = std::collections::hash_map::RandomState::new().build_hasher();
    first.finish().hash(&mut second);
    format!("{:016x}{:016x}", first.finish(), second.finish())
}
fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("Tab transfer exceeds its size limit"));
    }
    Ok(bytes)
}
fn write_once(path: &Path, bytes: &[u8]) -> io::Result<()> {
    // Complete the record before publishing it without replacement, so the
    // timeout can never interpret a partially written commit as cancellation.
    let staging = path.with_extension(format!("{}.tmp", unique_token()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = fs::remove_file(&staging);
        return Err(error);
    }
    drop(file);
    #[cfg(windows)]
    let result = {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
        let from: Vec<_> = staging.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 0) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    #[cfg(not(windows))]
    let result = fs::hard_link(&staging, path);
    let _ = fs::remove_file(staging);
    result
}
impl Ticket {
    fn create(payload: Payload) -> io::Result<Self> {
        fs::create_dir_all(base_directory())?;
        for _ in 0..16 {
            let token = unique_token();
            let directory = base_directory().join(&token);
            match fs::create_dir(&directory) {
                Ok(()) => {
                    let ticket = Self {
                        directory,
                        token,
                        expires_ms: now_ms() + TIMEOUT.as_millis() as u64,
                    };
                    let envelope = Envelope {
                        version: VERSION,
                        token: ticket.token.clone(),
                        expires_ms: ticket.expires_ms,
                        payload,
                    };
                    let bytes = match serde_json::to_vec(&envelope) {
                        Ok(bytes) => bytes,
                        Err(error) => {
                            ticket.clean_control_files();
                            return Err(error.into());
                        }
                    };
                    if bytes.len() > MAX_BYTES {
                        ticket.clean_control_files();
                        return Err(io::Error::other(
                            "Tab state exceeds the 4 MiB transfer budget",
                        ));
                    }
                    if let Err(error) = write_once(&ticket.directory.join("request.json"), &bytes) {
                        ticket.clean_control_files();
                        return Err(error);
                    }
                    return Ok(ticket);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other(
            "A unique tab transfer could not be allocated",
        ))
    }
    fn ready(&self, process: u32) -> bool {
        self.acknowledged("ready.json", process)
    }
    fn acknowledged(&self, name: &str, process: u32) -> bool {
        read_bounded(&self.directory.join(name), 256)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Ready>(&bytes).ok())
            .is_some_and(|ready| ready.token == self.token && ready.process == process)
    }
    fn decision(&self) -> Option<Decision> {
        self.read_decision("outcome.json")
    }
    fn read_decision(&self, name: &str) -> Option<Decision> {
        let bytes = read_bounded(&self.directory.join(name), 256).ok()?;
        let outcome: Outcome = serde_json::from_slice(&bytes).ok()?;
        (outcome.token == self.token).then_some(outcome.decision)
    }
    fn decide(&self, decision: Decision) -> io::Result<()> {
        self.write_decision("outcome.json", decision)
    }
    fn release(&self, decision: Decision) -> io::Result<()> {
        self.write_decision("released.json", decision)
    }
    fn write_decision(&self, name: &str, decision: Decision) -> io::Result<()> {
        if decision == Decision::Commit && now_ms() >= self.expires_ms {
            return Err(io::Error::other(
                "The new window did not become ready in time",
            ));
        }
        let bytes = serde_json::to_vec(&Outcome {
            token: self.token.clone(),
            decision,
        })?;
        match write_once(&self.directory.join(name), &bytes) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if self.read_decision(name) == Some(decision) {
                    Ok(())
                } else {
                    Err(io::Error::other("The tab transfer was cancelled"))
                }
            }
            result => result,
        }
    }
    fn clean_control_files(&self) {
        if !owned_directory(&self.directory) {
            return;
        }
        for name in [
            "request.json",
            "claimed",
            "ready.json",
            "outcome.json",
            "activated.json",
            "released.json",
        ] {
            let _ = fs::remove_file(self.directory.join(name));
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

/// Consume a single request from the protocol's generated TEMP directory. A
/// malformed or repeated explicit transfer fails closed instead of activating
/// the primary instance or treating the transfer filename as a folder.
pub(crate) fn read_startup(args: &[OsString]) -> io::Result<Option<Startup>> {
    let requests = args.iter().filter(|arg| *arg == "--tab-transfer").count();
    let tokens = args
        .iter()
        .filter(|arg| *arg == "--tab-transfer-token")
        .count();
    if requests == 0 && tokens == 0 {
        return Ok(None);
    }
    if requests != 1 || tokens != 1 || !args.iter().any(|arg| arg == "--new-window") {
        return Err(io::Error::other(
            "Tab transfer requires one request, token and --new-window",
        ));
    }
    let value = |key: &str| {
        args.iter()
            .position(|arg| arg == key)
            .and_then(|at| args.get(at + 1))
            .filter(|value| !value.to_string_lossy().starts_with("--"))
    };
    let path = value("--tab-transfer")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("The tab transfer request is missing"))?;
    let token = value("--tab-transfer-token")
        .and_then(|value| value.to_str())
        .filter(|token| valid_token(token))
        .ok_or_else(|| io::Error::other("The tab transfer token is invalid"))?;
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid transfer path"))?;
    if !owned_directory(directory)
        || directory.file_name().and_then(|name| name.to_str()) != Some(token)
        || path.file_name().and_then(|name| name.to_str()) != Some("request.json")
    {
        return Err(io::Error::other(
            "The tab request is outside its owned TEMP directory",
        ));
    }
    write_once(
        &directory.join("claimed"),
        std::process::id().to_string().as_bytes(),
    )?;
    let bytes = read_bounded(&path, MAX_BYTES)?;
    let envelope: Envelope = serde_json::from_slice(&bytes)?;
    fs::remove_file(&path)?;
    let now = now_ms();
    if envelope.version != VERSION
        || envelope.token != token
        || envelope.expires_ms <= now
        || envelope.expires_ms > now.saturating_add(TIMEOUT.as_millis() as u64)
    {
        return Err(io::Error::other("The tab transfer is invalid or expired"));
    }
    let payload = envelope.payload;
    let workspace = payload
        .session
        .workspace
        .as_ref()
        .ok_or_else(|| io::Error::other("Tab state is missing"))?;
    if workspace.tabs.is_empty()
        || workspace.tabs.len() > 2
        || workspace.tabs.len() != payload.panes.len()
        || workspace.tabs.iter().any(|tab| tab.right.is_some())
        || payload.panes.iter().any(|pane| pane.filter.len() > 4096)
        || payload.theme.len() > 64 * 1024
        || foldex_ui::Theme::read(&payload.theme).is_err()
    {
        return Err(io::Error::other("The tab transfer state is malformed"));
    }
    let expected = serde_json::json!({"workspace": workspace, "panes": payload.panes});
    fs::create_dir(directory.join("state"))?;
    Ok(Some(Startup {
        session: payload.session,
        theme: payload.theme,
        position: payload
            .position
            .filter(|position| position.iter().all(|value| value.is_finite())),
        incoming: Incoming {
            ticket: Ticket {
                directory: directory.to_path_buf(),
                token: token.into(),
                expires_ms: envelope.expires_ms,
            },
            panes: payload.panes,
            announced: false,
            committed_frame: None,
            activated: false,
            deadline: Instant::now() + TIMEOUT,
            expected,
            probe_report: payload.probe_report,
            probe_exit_after_commit: payload.probe_exit_after_commit,
        },
    }))
}

fn spawn_window(executable: &Path, ticket: &Ticket) -> io::Result<Child> {
    let mut command = Command::new(executable);
    command
        .arg("--new-window")
        .arg("--tab-transfer")
        .arg(ticket.directory.join("request.json"))
        .arg("--tab-transfer-token")
        .arg(&ticket.token);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: no debug console.
    }
    command.spawn()
}
fn transfer_worker(
    payload: Payload,
    executable: PathBuf,
    events: mpsc::Sender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
) {
    let result = (|| -> io::Result<()> {
        let ticket = Ticket::create(payload)?;
        let mut child = match spawn_window(&executable, &ticket) {
            Ok(child) => child,
            Err(error) => {
                ticket.clean_control_files();
                return Err(error);
            }
        };
        let began = Instant::now();
        let mut announced = false;
        let mut activated = false;
        let result = loop {
            if ticket.read_decision("released.json") == Some(Decision::Commit) {
                break Ok(());
            }
            if ticket.decision() == Some(Decision::Cancel)
                || ticket.read_decision("released.json") == Some(Decision::Cancel)
            {
                break Err(io::Error::other("The tab transfer was cancelled"));
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    break Err(io::Error::other(format!(
                        "The new window exited before the transfer completed ({status})"
                    )));
                }
                Ok(None) => {}
                Err(error) => break Err(error),
            }
            if began.elapsed() >= TIMEOUT || now_ms() >= ticket.expires_ms {
                // The final source release and timeout have exactly one winner.
                let _ = ticket.decide(Decision::Cancel);
                let _ = ticket.release(Decision::Cancel);
                if ticket.read_decision("released.json") == Some(Decision::Commit) {
                    break Ok(());
                }
                break Err(io::Error::other(
                    "The new window did not become ready in time",
                ));
            }
            if !announced && ticket.ready(child.id()) {
                if events.send(Event::Ready(ticket.clone())).is_err() {
                    let _ = ticket.decide(Decision::Cancel);
                    break Err(io::Error::other(
                        "The source window closed during the transfer",
                    ));
                }
                announced = true;
                wake();
            }
            if announced
                && !activated
                && ticket.decision() == Some(Decision::Commit)
                && ticket.acknowledged("activated.json", child.id())
            {
                if events.send(Event::Activated(ticket.clone())).is_err() {
                    let _ = ticket.release(Decision::Cancel);
                    break Err(io::Error::other(
                        "The source window closed before completing the transfer",
                    ));
                }
                activated = true;
                wake();
            }
            std::thread::sleep(POLL);
        };
        if result.is_err() {
            let _ = ticket.decide(Decision::Cancel);
            let _ = ticket.release(Decision::Cancel);
            let _ = child.kill();
            let _ = child.wait();
            ticket.clean_control_files();
        }
        result
    })();
    if let Err(error) = result {
        let _ = events.send(Event::Failed(error.to_string()));
        wake();
    }
}

fn single_tab_workspace(
    mut tab: TabState,
    active_side: usize,
    source: &WorkspaceState,
) -> WorkspaceState {
    use crate::workspace_tree::{GroupState, Node, TreeState};
    let right = tab.right.take();
    let split = if tab.split.is_finite() {
        tab.split.clamp(0.15, 0.85)
    } else {
        0.5
    };
    let vertical = tab.vertical;
    let mut tabs = vec![tab];
    let mut groups = vec![GroupState {
        id: 1,
        tabs: vec![0],
        active: 0,
    }];
    let tree = if let Some(right) = right {
        tabs.push(TabState {
            left: right,
            right: None,
            vertical: false,
            split: 0.5,
        });
        groups.push(GroupState {
            id: 2,
            tabs: vec![1],
            active: 0,
        });
        Node::Split {
            id: 3,
            vertical,
            children: vec![Node::leaf(1), Node::leaf(2)],
            weights: vec![split, 1.0 - split],
        }
    } else {
        Node::leaf(1)
    };
    let active = usize::from(tabs.len() > 1 && active_side > 0);
    WorkspaceState {
        tree: Some(TreeState {
            root: tree,
            groups,
            focused: active as u64 + 1,
        }),
        tabs,
        active,
        active_pane: 0,
        sidebar: source.sidebar,
        sidebar_width: source.sidebar_width,
        inspector: source.inspector,
        inspector_width: source.inspector_width,
    }
}

impl Foldex {
    fn tab_fingerprint(&self, source: u64) -> io::Result<Vec<u8>> {
        let tab = self
            .tabs
            .iter()
            .find(|tab| tab.left.id == source)
            .ok_or_else(|| io::Error::other("The source tab was closed"))?;
        let panes: Vec<_> = std::iter::once(&tab.left)
            .chain(tab.right.iter())
            .map(PaneTransfer::capture)
            .collect();
        Ok(serde_json::to_vec(&(Self::tab_state(tab), panes))?)
    }
    fn transfer_payload(&self, source: u64, probe_report: Option<PathBuf>) -> io::Result<Payload> {
        let tab = self
            .tabs
            .iter()
            .find(|tab| tab.left.id == source)
            .ok_or_else(|| io::Error::other("The source tab was closed"))?;
        let active_side = if self.tabs[self.active].left.id == source {
            self.active_pane
        } else {
            0
        };
        let workspace =
            single_tab_workspace(Self::tab_state(tab), active_side, &self.workspace_state());
        let panes = std::iter::once(&tab.left)
            .chain(tab.right.iter())
            .map(PaneTransfer::capture)
            .collect();
        let mut session = self.session_snapshot();
        session.tabs = workspace
            .tabs
            .iter()
            .map(|tab| tab.left.path.clone())
            .collect();
        session.active = workspace.active;
        session.workspace = Some(workspace);
        let pointer = drag_pointer(&self.ui_context, self.window_owner);
        let position = self.ui_context.input(|input| {
            input.viewport().inner_rect.map(|rect| {
                let point = pointer.unwrap_or(egui::pos2(112.0, 48.0));
                [rect.left() + point.x - 80.0, rect.top() + point.y - 16.0]
            })
        });
        Ok(Payload {
            session,
            panes,
            theme: self.theme.write().map_err(io::Error::other)?,
            position,
            probe_report,
            probe_exit_after_commit: false,
        })
    }
    pub(super) fn move_tab_to_new_window(&mut self, source: u64) {
        let executable = std::env::current_exe();
        match executable.and_then(|executable| self.begin_tab_transfer(source, executable, None)) {
            Ok(()) => self.toast("Moving tab to a new window…"),
            Err(error) => self.toast_error(TAB_MOVE_FAILED, &error.to_string()),
        }
    }
    pub(super) fn begin_tab_transfer(
        &mut self,
        source: u64,
        executable: PathBuf,
        probe_report: Option<PathBuf>,
    ) -> io::Result<()> {
        self.start_tab_transfer(source, executable, probe_report, false)
    }
    pub(super) fn begin_tab_transfer_without_activation(
        &mut self,
        source: u64,
        executable: PathBuf,
    ) -> io::Result<()> {
        self.start_tab_transfer(source, executable, None, true)
    }
    fn start_tab_transfer(
        &mut self,
        source: u64,
        executable: PathBuf,
        probe_report: Option<PathBuf>,
        exit_after_commit: bool,
    ) -> io::Result<()> {
        if self.window_transfer.outgoing.is_some() {
            return Err(io::Error::other("A tab is already moving to a new window"));
        }
        let fingerprint = self.tab_fingerprint(source)?;
        let mut payload = self.transfer_payload(source, probe_report)?;
        payload.probe_exit_after_commit = exit_after_commit;
        let (events, receiver) = mpsc::channel();
        let ctx = self.ui_context.clone();
        let wake = Arc::new(move || ctx.request_repaint_of(egui::ViewportId::ROOT));
        std::thread::Builder::new()
            .name("foldex-tab-transfer".into())
            .spawn(move || transfer_worker(payload, executable, events, wake))?;
        self.window_transfer.completion = None;
        self.window_transfer.ready_acknowledged = false;
        self.window_transfer.outgoing = Some(Pending {
            source,
            fingerprint,
            events: receiver,
            committed: false,
        });
        self.ui_context.request_repaint_after(POLL);
        Ok(())
    }
    pub(crate) fn accept_window_transfer(&mut self, mut incoming: Incoming) {
        for (tab, extra) in self
            .tabs
            .iter_mut()
            .zip(std::mem::take(&mut incoming.panes))
        {
            extra.apply(&mut tab.left);
        }
        self.window_transfer.incoming = Some(incoming);
        self.explorer_visible = false;
        self.ui_context.request_repaint();
    }
    fn remove_moved_tab(&mut self, source: u64, ctx: &egui::Context) {
        let Some(at) = self.tabs.iter().position(|tab| tab.left.id == source) else {
            return;
        };
        let last = self.tabs.len() == 1;
        let previous = self.tabs[self.active].left.id;
        if last {
            // Keep a valid Home root for the primary IPC/tray/hotkey owner.
            let mut left =
                Pane::dormant(self.next_id, &self.engine, self.tabs[at].left.path.clone());
            self.next_id += 1;
            left.attach_search(self.search.client());
            left.restore_navigation(crate::navigation::Navigation::new(
                crate::navigation::Route::Home,
            ));
            let id = left.id;
            self.tabs.push(Tab {
                left,
                right: None,
                vertical: false,
                pending_axis: None,
                split: 0.5,
                reveal: 0.0,
                closing: false,
            });
            self.desk.add(id);
        }
        let closed = std::mem::take(&mut self.closed_tabs);
        self.close_tab(at);
        self.closed_tabs = closed;
        if previous != source {
            self.focus_pane(previous);
        }
        self.session_dirty = true;
        self.save_due = Some(Instant::now());
        if last {
            if self.launcher_auxiliary {
                self.request_explorer_close(ctx);
            } else {
                self.hide_explorer(ctx);
            }
        }
    }
    fn transfer_window_state(&self) -> serde_json::Value {
        let panes: Vec<_> = self
            .tabs
            .iter()
            .map(|tab| PaneTransfer::capture(&tab.left))
            .collect();
        serde_json::json!({"workspace": self.workspace_state(), "panes": panes})
    }
    pub(super) fn service_window_transfers(&mut self, ctx: &egui::Context) {
        if let Some(mut pending) = self.window_transfer.outgoing.take() {
            match pending.events.try_recv() {
                Ok(Event::Ready(ticket)) => {
                    self.window_transfer.ready_acknowledged = true;
                    let unchanged = self
                        .tab_fingerprint(pending.source)
                        .is_ok_and(|bytes| bytes == pending.fingerprint);
                    let result = if unchanged {
                        ticket.decide(Decision::Commit)
                    } else {
                        let _ = ticket.decide(Decision::Cancel);
                        Err(io::Error::other(
                            "The tab changed while the new window was opening. Try moving it again",
                        ))
                    };
                    match result {
                        Ok(()) => {
                            pending.committed = true;
                            self.window_transfer.outgoing = Some(pending);
                            ctx.request_repaint_after(POLL);
                        }
                        Err(error) => {
                            self.toast_error(TAB_MOVE_FAILED, &error.to_string());
                            self.window_transfer.completion = Some(Err(error.to_string()));
                        }
                    }
                }
                Ok(Event::Activated(ticket)) => {
                    // Readiness alone is insufficient: the destination now owns
                    // the restored state and has prepared a native browser frame.
                    let unchanged = pending.committed
                        && self
                            .tab_fingerprint(pending.source)
                            .is_ok_and(|bytes| bytes == pending.fingerprint);
                    let late_failure = pending.events.try_iter().find_map(|event| {
                        if let Event::Failed(error) = event {
                            Some(error)
                        } else {
                            None
                        }
                    });
                    let result = if let Some(error) = late_failure {
                        let _ = ticket.release(Decision::Cancel);
                        Err(io::Error::other(error))
                    } else if unchanged {
                        ticket.release(Decision::Commit)
                    } else {
                        let _ = ticket.release(Decision::Cancel);
                        Err(io::Error::other(
                            "The tab changed while the new window was opening. Try moving it again",
                        ))
                    };
                    match result {
                        Ok(()) => {
                            self.remove_moved_tab(pending.source, ctx);
                            self.toast("Tab moved to a new window");
                            self.window_transfer.completion = Some(Ok(()));
                        }
                        Err(error) => {
                            self.toast_error(TAB_MOVE_FAILED, &error.to_string());
                            self.window_transfer.completion = Some(Err(error.to_string()));
                        }
                    }
                }
                Ok(Event::Failed(error)) => {
                    self.toast_error(TAB_MOVE_FAILED, &error);
                    self.window_transfer.completion = Some(Err(error));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.toast_error(TAB_MOVE_FAILED, "The window worker stopped");
                    self.window_transfer.completion = Some(Err("The window worker stopped".into()));
                }
                Err(mpsc::TryRecvError::Empty) => {
                    self.window_transfer.outgoing = Some(pending);
                    ctx.request_repaint_after(POLL);
                }
            }
        }
        if let Some(mut incoming) = self.window_transfer.incoming.take() {
            if !incoming.announced {
                let ready = serde_json::to_vec(&Ready {
                    token: incoming.ticket.token.clone(),
                    process: std::process::id(),
                });
                let result = ready.map_err(io::Error::other).and_then(|bytes| {
                    write_once(&incoming.ticket.directory.join("ready.json"), &bytes)
                });
                if result.is_err() {
                    let _ = incoming.ticket.decide(Decision::Cancel);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    self.allow_close = true;
                    return;
                }
                incoming.announced = true;
            }
            let decision = incoming.ticket.decision();
            let release = incoming.ticket.read_decision("released.json");
            if decision == Some(Decision::Cancel)
                || release == Some(Decision::Cancel)
                || Instant::now() >= incoming.deadline
                || now_ms() >= incoming.ticket.expires_ms
            {
                let _ = incoming.ticket.release(Decision::Cancel);
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else if decision == Some(Decision::Commit) {
                if incoming.probe_exit_after_commit {
                    // Deliberate owned-child failure for the native smoke probe.
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                if incoming.committed_frame.is_none() {
                    incoming.committed_frame = Some(self.frame_count);
                    self.show_explorer(ctx);
                }
                if !incoming.activated
                    && incoming
                        .committed_frame
                        .is_some_and(|frame| self.frame_count > frame.saturating_add(1))
                {
                    let activated = serde_json::to_vec(&Ready {
                        token: incoming.ticket.token.clone(),
                        process: std::process::id(),
                    });
                    let result = activated.map_err(io::Error::other).and_then(|bytes| {
                        write_once(&incoming.ticket.directory.join("activated.json"), &bytes)
                    });
                    if result.is_err() {
                        let _ = incoming.ticket.release(Decision::Cancel);
                        self.allow_close = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    incoming.activated = true;
                }
                if incoming.activated && release == Some(Decision::Commit) {
                    // The source has now removed its tab. Keep control records
                    // until its worker has observed this final receipt.
                    if let Some(path) = incoming.probe_report {
                        self.window_transfer.child_probe = Some(ChildProbe {
                            path,
                            expected: incoming.expected,
                            frames: 0,
                        });
                    }
                } else {
                    self.window_transfer.incoming = Some(incoming);
                    ctx.request_repaint_after(POLL);
                }
            } else {
                self.window_transfer.incoming = Some(incoming);
                ctx.request_repaint_after(POLL);
            }
        }
        if let Some(mut probe) = self.window_transfer.child_probe.take() {
            probe.frames += 1;
            if probe.frames >= 4 {
                let actual = self.transfer_window_state();
                let report = serde_json::json!({"ok": actual == probe.expected, "expected": probe.expected, "actual": actual, "process": std::process::id(), "session_path": self.session_path, "isolated_launcher": !self.background_allowed});
                let _ = fs::write(
                    probe.path,
                    serde_json::to_vec_pretty(&report).unwrap_or_default(),
                );
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.window_transfer.child_probe = Some(probe);
                ctx.request_repaint_after(POLL);
            }
        }
    }
}

/// A strip drag detaches only on release beyond the whole client viewport.
/// Moving vertically out of the strip while still over the app is a reorder.
pub(super) fn detach_intent(
    press: egui::Pos2,
    point: egui::Pos2,
    viewport: Rect,
    active: bool,
) -> bool {
    active
        && viewport.contains(press)
        && point.is_finite()
        && !viewport.expand(12.0).contains(point)
}
pub(super) fn drag_pointer(ctx: &egui::Context, window: isize) -> Option<egui::Pos2> {
    #[cfg(windows)]
    if window != 0 {
        use windows_sys::Win32::{
            Foundation::POINT, Graphics::Gdi::ScreenToClient, UI::WindowsAndMessaging::GetCursorPos,
        };
        let mut point = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut point) } != 0
            && unsafe { ScreenToClient(window as _, &mut point) } != 0
        {
            return Some(egui::pos2(point.x as f32, point.y as f32) / ctx.pixels_per_point());
        }
    }
    #[cfg(not(windows))]
    let _ = window;
    ctx.input(|input| input.pointer.interact_pos())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        navigation::{Navigation, Route},
        workspace::{PaneState, ViewSettings},
    };

    fn payload() -> Payload {
        let mut navigation = Navigation::new(Route::Folder("C:/owned-transfer/previous".into()));
        navigation.visit(Route::Home);
        let left = PaneState {
            path: "C:/owned-transfer/fallback".into(),
            view: ViewSettings {
                zoom: 0.6,
                sort: 3,
                descending: true,
                hidden: true,
                ..Default::default()
            },
            route: Some(Route::Home),
            navigation: Some(navigation),
        };
        let right = PaneState {
            path: "C:/owned-transfer/right-fallback".into(),
            view: ViewSettings {
                zoom: 0.8,
                sort: 2,
                kind: 1,
                ..Default::default()
            },
            route: Some(Route::RecycleBin),
            navigation: Some(Navigation::new(Route::RecycleBin)),
        };
        let workspace = single_tab_workspace(
            TabState {
                left,
                right: Some(right),
                vertical: true,
                split: 0.375,
            },
            1,
            &WorkspaceState {
                sidebar: true,
                sidebar_width: 244.0,
                inspector: true,
                inspector_width: 321.0,
                ..Default::default()
            },
        );
        let mut session = Session::default();
        session.tabs = workspace
            .tabs
            .iter()
            .map(|tab| tab.left.path.clone())
            .collect();
        session.active = 1;
        session.workspace = Some(workspace);
        Payload {
            session,
            panes: vec![
                PaneTransfer {
                    filter: "notes".into(),
                    recursive: false,
                    substring: true,
                    home_selected: Some("C:/owned-transfer/note.txt".into()),
                },
                PaneTransfer {
                    filter: "*.txt".into(),
                    recursive: false,
                    substring: false,
                    home_selected: None,
                },
            ],
            theme: Theme::default().write().unwrap(),
            position: Some([100.0, 200.0]),
            probe_report: None,
            probe_exit_after_commit: false,
        }
    }
    fn args(ticket: &Ticket) -> Vec<OsString> {
        vec![
            "--new-window".into(),
            "--tab-transfer".into(),
            ticket.directory.join("request.json").into(),
            "--tab-transfer-token".into(),
            ticket.token.clone().into(),
        ]
    }
    fn queue(app: &mut Foldex, source: u64, fingerprint: Vec<u8>) -> mpsc::Sender<Event> {
        let (sender, events) = mpsc::channel();
        app.window_transfer.outgoing = Some(Pending {
            source,
            fingerprint,
            events,
            committed: false,
        });
        sender
    }

    #[test]
    fn split_routes_navigation_filters_views_and_active_side_round_trip_once() {
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let startup = read_startup(&args(&ticket)).unwrap().unwrap();
        assert!(!ticket.directory.join("request.json").exists());
        assert!(
            read_startup(&args(&ticket)).is_err(),
            "a consumed handoff must never replay"
        );
        let state = startup.session.workspace.as_ref().unwrap();
        assert_eq!(state.tabs.len(), 2);
        assert_eq!(state.active, 1);
        assert_eq!(state.tree.as_ref().unwrap().focused, 2);
        assert_eq!(state.tabs[0].left.route, Some(Route::Home));
        assert!(state.tabs[0].left.navigation.as_ref().unwrap().can_back());
        assert_eq!(state.tabs[1].left.route, Some(Route::RecycleBin));
        assert_eq!(state.tabs[0].left.view.zoom, 0.6);
        assert!(state.tabs[0].left.view.descending && state.tabs[0].left.view.hidden);
        assert_eq!(state.tabs[0].left.view.sort, 3);
        assert_eq!(startup.incoming.panes[0].filter, "notes");
        assert!(startup.incoming.panes[0].substring);
        assert_eq!(startup.incoming.panes[1].filter, "*.txt");
        match &state.tree.as_ref().unwrap().root {
            crate::workspace_tree::Node::Split {
                vertical, weights, ..
            } => {
                assert!(*vertical);
                assert_eq!(weights, &[0.375, 0.625]);
            }
            _ => panic!("the two-sided tab lost its split"),
        }
    }

    #[test]
    fn native_acknowledgement_requires_both_the_nonce_and_spawned_process() {
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let ready = ticket.directory.join("ready.json");
        assert!(!ticket.ready(42));
        fs::write(
            &ready,
            serde_json::to_vec(&Ready {
                token: "wrong".into(),
                process: 42,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(!ticket.ready(42));
        fs::write(
            &ready,
            serde_json::to_vec(&Ready {
                token: ticket.token.clone(),
                process: 43,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(!ticket.ready(42));
        fs::write(
            &ready,
            serde_json::to_vec(&Ready {
                token: ticket.token.clone(),
                process: 42,
            })
            .unwrap(),
        )
        .unwrap();
        assert!(ticket.ready(42));
    }

    #[test]
    fn cancellation_and_commit_have_one_winner_and_expired_readiness_cannot_commit() {
        let mut ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        ticket.expires_ms = now_ms();
        assert!(ticket.decide(Decision::Commit).is_err());
        ticket.decide(Decision::Cancel).unwrap();
        ticket.expires_ms = now_ms() + 1000;
        assert!(ticket.decide(Decision::Commit).is_err());
        assert!(ticket.decision() == Some(Decision::Cancel));
        let second = Ticket::create(payload()).unwrap();
        let _cleanup_second = Cleanup(second.directory.clone());
        second.decide(Decision::Commit).unwrap();
        assert!(second.decide(Decision::Cancel).is_err());
        assert!(second.decision() == Some(Decision::Commit));
    }

    #[test]
    fn large_saved_collection_preferences_survive_tab_handoff() {
        let mut payload = payload();
        for n in 0..32 {
            payload
                .session
                .saved_searches
                .push(crate::navigation::SavedSearch {
                    name: format!("collection{n}"),
                    location: crate::navigation::SearchLocation {
                        scope: std::env::temp_dir(),
                        directory_only: true,
                        query: foldex_core::Query {
                            conditions: foldex_query::Conditions {
                                excluded_subtrees: (0..16)
                                    .map(|i| {
                                        PathBuf::from(format!(
                                            "folder{i}/{}/{}",
                                            "x".repeat(245),
                                            "y".repeat(245)
                                        ))
                                    })
                                    .collect(),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    },
                });
        }
        let ticket = Ticket::create(payload).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        assert!(
            fs::metadata(ticket.directory.join("request.json"))
                .unwrap()
                .len()
                > 256 * 1024
        );
        assert_eq!(
            read_startup(&args(&ticket))
                .unwrap()
                .unwrap()
                .session
                .saved_searches
                .len(),
            32
        );
    }

    #[test]
    fn startup_rejects_missing_duplicate_unowned_expired_and_oversize_requests() {
        assert!(read_startup(&["--tab-transfer".into()]).is_err());
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let mut duplicate = args(&ticket);
        duplicate.push("--tab-transfer".into());
        assert!(read_startup(&duplicate).is_err());
        let mut unowned = args(&ticket);
        unowned[2] = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("request.json")
            .into();
        assert!(read_startup(&unowned).is_err());
        let path = ticket.directory.join("request.json");
        let mut envelope: Envelope = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        envelope.expires_ms = now_ms().saturating_sub(1);
        fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(read_startup(&args(&ticket)).is_err());
        let large = Ticket::create(payload()).unwrap();
        let _cleanup_large = Cleanup(large.directory.clone());
        OpenOptions::new()
            .write(true)
            .open(large.directory.join("request.json"))
            .unwrap()
            .set_len(MAX_BYTES as u64 + 1)
            .unwrap();
        assert!(read_startup(&args(&large)).is_err());
    }

    #[test]
    fn absent_acknowledgement_and_failed_spawn_leave_the_source_unchanged() {
        let (mut app, ctx, _) = super::super::tests::fixture_app();
        let source = app.tabs[app.active].left.id;
        let before = app.tab_fingerprint(source).unwrap();
        let sender = queue(&mut app, source, before.clone());
        app.service_window_transfers(&ctx);
        assert_eq!(app.tab_fingerprint(source).unwrap(), before);
        assert!(app.window_transfer.outgoing.is_some());
        sender
            .send(Event::Failed("synthetic spawn failure".into()))
            .unwrap();
        app.service_window_transfers(&ctx);
        assert_eq!(app.tab_fingerprint(source).unwrap(), before);
        assert!(app.toast.as_ref().unwrap().error);
        assert!(app.window_transfer.completion.as_ref().unwrap().is_err());
    }

    #[test]
    fn changed_source_cancels_a_ready_destination_without_losing_the_tab() {
        let (mut app, ctx, _) = super::super::tests::fixture_app();
        let source = app.tabs[app.active].left.id;
        let fingerprint = app.tab_fingerprint(source).unwrap();
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let sender = queue(&mut app, source, fingerprint);
        app.pane_mut().filter = "new query while the child starts".into();
        sender.send(Event::Ready(ticket.clone())).unwrap();
        app.service_window_transfers(&ctx);
        assert!(app.tabs.iter().any(|tab| tab.left.id == source));
        assert_eq!(app.pane().filter, "new query while the child starts");
        assert!(ticket.decision() == Some(Decision::Cancel));
    }

    #[test]
    fn acknowledged_transfer_removes_only_its_source_and_keeps_closed_tab_history() {
        let (mut app, ctx, _) = super::super::tests::fixture_app();
        let source = app.tabs[app.active].left.id;
        app.duplicate_tab();
        let spare = app.tabs[app.active].left.id;
        let fingerprint = app.tab_fingerprint(source).unwrap();
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let sender = queue(&mut app, source, fingerprint);
        sender.send(Event::Ready(ticket.clone())).unwrap();
        app.service_window_transfers(&ctx);
        assert!(app.tabs.iter().any(|tab| tab.left.id == source));
        assert!(app.window_transfer.completion.is_none());
        sender.send(Event::Activated(ticket.clone())).unwrap();
        app.service_window_transfers(&ctx);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].left.id, spare);
        assert!(app.closed_tabs.is_empty());
        assert!(ticket.decision() == Some(Decision::Commit));
        assert!(ticket.read_decision("released.json") == Some(Decision::Commit));
        assert!(app.window_transfer.completion.as_ref().unwrap().is_ok());
    }

    #[test]
    fn moving_the_last_primary_tab_preserves_a_dormant_home_and_the_launcher_owner() {
        let (mut app, ctx, _) = super::super::tests::fixture_app();
        let source = app.tabs[app.active].left.id;
        let fingerprint = app.tab_fingerprint(source).unwrap();
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let sender = queue(&mut app, source, fingerprint);
        sender.send(Event::Ready(ticket.clone())).unwrap();
        app.service_window_transfers(&ctx);
        assert_eq!(app.tabs[app.active].left.id, source);
        sender.send(Event::Activated(ticket)).unwrap();
        app.service_window_transfers(&ctx);
        assert_eq!(app.tabs.len(), 1);
        assert_ne!(app.tabs[0].left.id, source);
        assert!(app.tabs[0].left.is_home());
        assert!(app.tabs[0].left.snapshot.is_none());
        assert!(!app.explorer_visible && !app.allow_close);
        assert!(app.close_requested.is_none());
    }

    #[test]
    fn child_failure_after_readiness_keeps_the_original_tab_and_its_state() {
        let (mut app, ctx, _) = super::super::tests::fixture_app();
        let source = app.tabs[app.active].left.id;
        let fingerprint = app.tab_fingerprint(source).unwrap();
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let sender = queue(&mut app, source, fingerprint.clone());
        sender.send(Event::Ready(ticket)).unwrap();
        app.service_window_transfers(&ctx);
        assert!(app.window_transfer.outgoing.as_ref().unwrap().committed);
        app.service_window_transfers(&ctx);
        assert_eq!(app.tab_fingerprint(source).unwrap(), fingerprint);
        sender
            .send(Event::Failed("child exited before activation".into()))
            .unwrap();
        app.service_window_transfers(&ctx);
        assert_eq!(app.tab_fingerprint(source).unwrap(), fingerprint);
        assert!(app.window_transfer.completion.as_ref().unwrap().is_err());
    }

    #[test]
    fn source_changes_after_commit_abort_the_activated_child_without_removing_the_tab() {
        let (mut app, ctx, _) = super::super::tests::fixture_app();
        let source = app.tabs[app.active].left.id;
        let fingerprint = app.tab_fingerprint(source).unwrap();
        let ticket = Ticket::create(payload()).unwrap();
        let _cleanup = Cleanup(ticket.directory.clone());
        let sender = queue(&mut app, source, fingerprint);
        sender.send(Event::Ready(ticket.clone())).unwrap();
        app.service_window_transfers(&ctx);
        app.pane_mut().filter = "query changed after readiness".into();
        sender.send(Event::Activated(ticket.clone())).unwrap();
        app.service_window_transfers(&ctx);
        assert!(app.tabs.iter().any(|tab| tab.left.id == source));
        assert_eq!(app.pane().filter, "query changed after readiness");
        assert!(ticket.read_decision("released.json") == Some(Decision::Cancel));
    }

    #[test]
    fn drag_intent_distinguishes_reordering_jitter_and_an_actual_viewport_crossing() {
        let viewport = Rect::from_min_size(egui::Pos2::ZERO, vec2(1200.0, 760.0));
        let press = egui::pos2(200.0, 16.0);
        assert!(!detach_intent(
            press,
            egui::pos2(800.0, 16.0),
            viewport,
            true
        ));
        assert!(!detach_intent(
            press,
            egui::pos2(200.0, 110.0),
            viewport,
            true
        ));
        assert!(!detach_intent(
            press,
            egui::pos2(200.0, -4.0),
            viewport,
            true
        ));
        assert!(!detach_intent(
            press,
            egui::pos2(1250.0, 30.0),
            viewport,
            false
        ));
        assert!(detach_intent(
            press,
            egui::pos2(1250.0, 30.0),
            viewport,
            true
        ));
        assert!(detach_intent(
            press,
            egui::pos2(200.0, -30.0),
            viewport,
            true
        ));
        assert!(detach_intent(
            press,
            egui::pos2(-30.0, 200.0),
            viewport,
            true
        ));
        assert!(!detach_intent(
            egui::pos2(-50.0, 10.0),
            egui::pos2(-80.0, 200.0),
            viewport,
            true
        ));
    }
}
