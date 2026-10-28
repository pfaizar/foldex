//! Bounded background I/O. The frame loop only submits work and drains results.
//!
//! File mutations and interactive Shell handlers use independent lanes. Previews have one
//! replaceable request/result, so an unresponsive shell handler cannot delay a
//! preview and selecting many images cannot accumulate decoded buffers.

use std::{
    fs::{self, OpenOptions},
    io::{self, Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
};

use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
#[cfg(any(not(windows), test))]
use std::collections::HashSet;

#[cfg(windows)]
#[path = "platform/windows_ops.rs"]
pub(crate) mod windows_ops;
use crate::operation_history::{Direction, HistoryHandle, HistoryState, Mutation};
#[path = "platform/folder_options.rs"]
mod folder_options;
pub(crate) mod operations;

const JOB_CAPACITY: usize = 8;
const EVENT_CAPACITY: usize = 8;
const MAX_SELECTION: usize = 4_096;
#[cfg(any(not(windows), test))]
const MAX_OPERATION_ENTRIES: usize = 100_000;
#[cfg(any(not(windows), test))]
const MAX_OPERATION_PATH_BYTES: usize = 32 * 1024 * 1024;
const MAX_SAVE_BYTES: usize = 4 * 1024 * 1024;
const TEXT_LIMIT: usize = 256 * 1024;
const IMAGE_FILE_LIMIT: usize = 16 * 1024 * 1024;
const IMAGE_DECODE_LIMIT: u64 = 64 * 1024 * 1024;
const IMAGE_MAX_EDGE: u32 = 1_600;
#[cfg(any(not(windows), test))]
const COPY_BUFFER_SIZE: usize = 128 * 1024;

type Wake = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug)]
pub enum Job {
    ExplorerIntegration {
        id: u64,
        enabled: Option<bool>,
    },
    Open(PathBuf),
    /// Open Windows Folder Options only after an explicit pane-menu action.
    FolderOptions,
    Properties(Vec<PathBuf>),
    Launcher {
        settings: bool,
    },
    Application(Arc<[u8]>),
    Reveal(PathBuf),
    Explorer(PathBuf),
    Console(PathBuf),
    NewWindow(PathBuf),
    NewHomeWindow,
    NewFile {
        parent: PathBuf,
        name: String,
    },
    NewFolder {
        parent: PathBuf,
        name: String,
    },
    Rename {
        path: PathBuf,
        name: String,
    },
    Copy {
        sources: Vec<PathBuf>,
        destination: PathBuf,
        cut: bool,
    },
    /// Drop modifiers are captured in the UI; volume lookup stays on the worker.
    DropFiles {
        sources: Vec<PathBuf>,
        destination: PathBuf,
        control: bool,
        shift: bool,
    },
    Recycle(Vec<PathBuf>),
    /// The app has already obtained explicit permanent-delete confirmation.
    DeletePermanent(Vec<PathBuf>),
    Undo,
    Redo,
    WriteClipboard {
        sources: Vec<PathBuf>,
        cut: bool,
    },
    ReadClipboard {
        destination: PathBuf,
    },
    Resolve {
        path: PathBuf,
        id: u64,
    },
    Preview {
        path: PathBuf,
        id: u64,
    },
    Save {
        path: PathBuf,
        bytes: Vec<u8>,
    },
    /// Explicit diagnostics request; no timer or background sampling is started.
    Metrics {
        id: u64,
    },
}

impl Job {
    /// Plain-language start of the sentence shown when this job fails.
    pub fn failure_context(&self) -> &'static str {
        match self {
            Job::ExplorerIntegration { .. } => "Couldn't change the Windows Explorer setting",
            Job::Open(_) => "Couldn't open that item",
            Job::FolderOptions => "Couldn't open Folder Options",
            Job::Properties(_) => "Couldn't open Properties",
            Job::Launcher { .. } => "Couldn't open the launcher",
            Job::Application(_) => "Couldn't start that app",
            Job::Reveal(_) => "Couldn't show that item in Windows Explorer",
            Job::Explorer(_) => "Couldn't open Windows Explorer",
            Job::Console(_) => "Couldn't open the console",
            Job::NewWindow(_) | Job::NewHomeWindow => "Couldn't open a new window",
            Job::NewFile { .. } => "Couldn't create the file",
            Job::NewFolder { .. } => "Couldn't create the folder",
            Job::Rename { .. } => "Couldn't rename that item",
            Job::Copy { cut: true, .. } => "Couldn't move the selected items",
            Job::Copy { cut: false, .. } => "Couldn't copy the selected items",
            Job::DropFiles { .. } => "Couldn't move or copy the dropped items",
            Job::Recycle(_) => "Couldn't move the selected items to the Recycle Bin",
            Job::DeletePermanent(_) => "Couldn't delete the selected items",
            Job::Undo => "Couldn't undo that",
            Job::Redo => "Couldn't redo that",
            Job::WriteClipboard { cut: true, .. } => "Couldn't cut the selected items",
            Job::WriteClipboard { cut: false, .. } => "Couldn't copy the selected items",
            Job::ReadClipboard { .. } => "Couldn't paste the items",
            Job::Resolve { .. } => "Couldn't open that location",
            Job::Preview { .. } => "Couldn't read that file",
            Job::Save { .. } => "Couldn't save the file",
            Job::Metrics { .. } => "Couldn't read the performance details",
        }
    }
}

#[derive(Debug)]
pub enum PreviewContent {
    Text(String),
    Image { size: [usize; 2], rgba: Vec<u8> },
    Info { bytes: u64, message: String },
}

#[derive(Debug)]
pub enum Event {
    /// An associated application accepted the Open request successfully.
    Opened(PathBuf),
    ExplorerIntegration {
        id: u64,
        result: Result<crate::integration::Status, String>,
    },
    Done {
        message: String,
        refresh: bool,
    },
    /// `what` is the plain-language action; `detail` is the raw error for Developer Mode.
    Failed {
        what: &'static str,
        detail: String,
    },
    Resolved {
        id: u64,
        path: PathBuf,
        is_dir: bool,
    },
    ResolveFailed {
        id: u64,
        path: PathBuf,
        detail: String,
    },
    Preview {
        id: u64,
        path: PathBuf,
        content: PreviewContent,
    },
    Metrics {
        id: u64,
        private_bytes: u64,
        working_set_bytes: u64,
        handles: u32,
        /// Accumulated kernel + user CPU time, not a percentage.
        cpu_seconds: f64,
    },
}

struct ProcessMetrics {
    private_bytes: u64,
    working_set_bytes: u64,
    handles: u32,
    cpu_seconds: f64,
}

struct PathRequest {
    path: PathBuf,
    id: u64,
    generation: u64,
}

#[derive(Default)]
struct PathMailbox {
    request: Mutex<Option<PathRequest>>,
    result: Mutex<Option<Event>>,
    generation: AtomicU64,
}

pub struct Platform {
    history: HistoryHandle,
    properties: foldex_shell::Properties,
    jobs: SyncSender<OperationJob>,
    shell_jobs: SyncSender<Job>,
    events: Receiver<Event>,
    preview_signal: SyncSender<()>,
    preview: Arc<PathMailbox>,
    resolve_signal: SyncSender<()>,
    resolve: Arc<PathMailbox>,
    stop: Arc<AtomicBool>,
    owner: Arc<AtomicIsize>,
    integration_events: SyncSender<Event>,
    integration_busy: Arc<AtomicBool>,
    pending_jobs: Arc<AtomicU64>,
    launcher_busy: Arc<AtomicBool>,
    integration_wake: Wake,
    operations: Arc<operations::Operations>,
}

struct OperationJob {
    job: Job,
    operation: Option<Arc<operations::Operation>>,
}

impl Platform {
    pub fn new(wake: Wake) -> io::Result<Self> {
        let properties = foldex_shell::Properties::new(wake.clone());
        let (jobs, job_rx) = mpsc::sync_channel(JOB_CAPACITY);
        let (shell_jobs, shell_rx) = mpsc::sync_channel(JOB_CAPACITY);
        let shell_rx = Arc::new(Mutex::new(shell_rx));
        let (event_tx, events) = mpsc::sync_channel(EVENT_CAPACITY);
        let integration_events = event_tx.clone();
        let integration_wake = wake.clone();
        let (preview_signal, preview_rx) = mpsc::sync_channel(1);
        let (resolve_signal, resolve_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let owner = Arc::new(AtomicIsize::new(0));
        let history = HistoryHandle::new(owner.clone(), stop.clone());
        let preview = Arc::new(PathMailbox::default());
        let resolve = Arc::new(PathMailbox::default());
        let worker_stop = stop.clone();
        let worker_wake = wake.clone();
        let worker_owner = owner.clone();
        let worker_history = history.clone();
        let pending_jobs = Arc::new(AtomicU64::new(0));
        let worker_pending = pending_jobs.clone();
        // Two independent STA-capable workers keep a modal Shell handler from
        // blocking a second interactive request. Mutations and clipboard jobs
        // retain their single FIFO, including immediate Copy -> Paste ordering.
        for index in 0..2 {
            let rx = shell_rx.clone();
            let events = event_tx.clone();
            let stop = stop.clone();
            let wake = wake.clone();
            let pending = pending_jobs.clone();
            thread::Builder::new()
                .name(format!("foldex-shell-{index}"))
                .spawn(move || {
                    loop {
                        let job = { rx.lock().unwrap_or_else(|e| e.into_inner()).recv() };
                        let Ok(job) = job else { break };
                        run_shell_job(job, &events, &stop, &wake);
                        pending.fetch_sub(1, Ordering::AcqRel);
                        wake();
                    }
                })?;
        }
        thread::Builder::new()
            .name("foldex-file-operations".into())
            .spawn(move || {
                operation_worker(
                    job_rx,
                    event_tx,
                    worker_stop,
                    worker_owner,
                    worker_wake,
                    worker_history,
                    worker_pending,
                );
            })?;
        let preview_stop = stop.clone();
        let mailbox = preview.clone();
        let preview_wake = wake.clone();
        if let Err(error) = thread::Builder::new()
            .name("foldex-preview".into())
            .spawn(move || {
                path_worker(preview_rx, mailbox, preview_stop, preview_wake, false);
            })
        {
            stop.store(true, Ordering::Release);
            return Err(error);
        }
        let resolve_stop = stop.clone();
        let mailbox = resolve.clone();
        if let Err(error) = thread::Builder::new()
            .name("foldex-path-resolver".into())
            .spawn(move || path_worker(resolve_rx, mailbox, resolve_stop, wake, true))
        {
            stop.store(true, Ordering::Release);
            return Err(error);
        }
        Ok(Self {
            history,
            properties,
            jobs,
            shell_jobs,
            events,
            preview_signal,
            preview,
            resolve_signal,
            resolve,
            stop,
            owner,
            integration_events,
            integration_busy: Arc::new(AtomicBool::new(false)),
            pending_jobs,
            launcher_busy: Arc::new(AtomicBool::new(false)),
            integration_wake,
            operations: Arc::new(operations::Operations::default()),
        })
    }

    /// The actual Foldex window, obtained from eframe's Win32 window handle.
    /// Keeping only its integer value does not move an HWND or COM object
    /// between apartments. Native dialogs validate it when each job starts.
    pub fn set_owner_window(&self, owner: isize) {
        self.owner.store(owner, Ordering::Release);
    }

    /// Cached labels only: this never waits for filesystem work or the journal.
    pub fn history_state(&self) -> HistoryState {
        self.history.state()
    }

    /// Count queued work too: a mutation waiting behind another operation
    /// must not disappear just because no history transaction has started yet.
    pub fn update_busy(&self) -> bool {
        self.pending_jobs.load(Ordering::Acquire) != 0
            || self.properties.is_busy()
            || self.integration_busy.load(Ordering::Acquire)
            || self.history.state().busy
    }

    /// The batch planner shares the same serialized mutation history.
    pub fn history_handle(&self) -> HistoryHandle {
        self.history.clone()
    }

    pub fn operations(&self) -> &operations::Operations {
        &self.operations
    }

    /// Never waits for disk access, an OS handler, or queue capacity.
    pub fn try_submit(&self, job: Job) -> Result<(), String> {
        if self.stop.load(Ordering::Acquire) {
            return Err("File worker is shutting down".into());
        }
        validate_job_size(&job).map_err(|error| error.to_string())?;
        let job = match job {
            Job::Launcher { settings } => {
                if self.launcher_busy.swap(true, Ordering::AcqRel) {
                    return Ok(());
                }
                let busy = self.launcher_busy.clone();
                let events = self.integration_events.clone();
                let wake = self.integration_wake.clone();
                if let Err(error) = thread::Builder::new()
                    .name("foldex-launcher-activation".into())
                    .spawn(move || {
                        let result = std::env::current_exe().and_then(|exe| {
                            let mut command = std::process::Command::new(exe);
                            command.arg(if settings {
                                "--launcher-settings"
                            } else {
                                "--launcher"
                            });
                            #[cfg(windows)]
                            {
                                use std::os::windows::process::CommandExt;
                                command.creation_flags(0x08000000);
                            }
                            command.spawn().map(|_| ())
                        });
                        if let Err(error) = result {
                            let _ = events.try_send(Event::Failed {
                                what: "Couldn't open the launcher",
                                detail: format!("Cannot open the launcher: {error}"),
                            });
                            wake();
                        }
                        busy.store(false, Ordering::Release);
                    })
                {
                    self.launcher_busy.store(false, Ordering::Release);
                    return Err(error.to_string());
                }
                return Ok(());
            }
            Job::ExplorerIntegration { id, enabled } => {
                if self.integration_busy.swap(true, Ordering::AcqRel) {
                    return Err("Explorer integration is busy. Try again.".into());
                }
                let events = self.integration_events.clone();
                let busy = self.integration_busy.clone();
                let wake = self.integration_wake.clone();
                let stop = self.stop.clone();
                // No resident watcher, startup work, or wait behind a long copy.
                let result = thread::Builder::new()
                    .name("foldex-integration".into())
                    .stack_size(256 * 1024)
                    .spawn(move || {
                        let result = crate::integration::run(enabled);
                        let delivered = !stop.load(Ordering::Acquire)
                            && events
                                .send(Event::ExplorerIntegration { id, result })
                                .is_ok();
                        busy.store(false, Ordering::Release);
                        if delivered {
                            wake();
                        }
                    });
                if let Err(error) = result {
                    self.integration_busy.store(false, Ordering::Release);
                    return Err(error.to_string());
                }
                return Ok(());
            }
            Job::Properties(paths) => {
                return self
                    .properties
                    .show_with_owner(self.owner.load(Ordering::Acquire), paths);
            }
            Job::Preview { path, id } => {
                return submit_path_request(
                    &self.preview,
                    &self.preview_signal,
                    path,
                    id,
                    "Preview",
                );
            }
            Job::Resolve { path, id } => {
                return submit_path_request(
                    &self.resolve,
                    &self.resolve_signal,
                    path,
                    id,
                    "Path lookup",
                );
            }
            job => job,
        };
        self.pending_jobs.fetch_add(1, Ordering::AcqRel);
        let result = if is_shell_job(&job) {
            self.shell_jobs.try_send(job).map_err(|error| match error {
                TrySendError::Full(_) => "Shell actions are busy; try again shortly".to_string(),
                TrySendError::Disconnected(_) => "Shell worker is unavailable".into(),
            })
        } else {
            match self.operations.track(&job, self.integration_wake.clone()) {
                Err(error) => Err(error),
                Ok(operation) => {
                    self.jobs
                        .try_send(OperationJob { job, operation })
                        .map_err(|error| {
                            let (full, envelope) = match error {
                                TrySendError::Full(job) => (true, job),
                                TrySendError::Disconnected(job) => (false, job),
                            };
                            if let Some(op) = envelope.operation {
                                self.operations.remove(op.id);
                            }
                            if full {
                                "File operations are busy; try again shortly".into()
                            } else {
                                "File worker is unavailable".into()
                            }
                        })
                }
            }
        };
        if result.is_err() {
            self.pending_jobs.fetch_sub(1, Ordering::AcqRel);
        }
        result
    }

    /// Preview results occupy one slot independently of operation messages.
    pub fn try_recv(&self) -> Option<Event> {
        self.events
            .try_recv()
            .ok()
            .or_else(|| self.resolve.result.try_lock().ok()?.take())
            .or_else(|| self.preview.result.try_lock().ok()?.take())
            .or_else(|| {
                self.properties.try_recv()?.err().map(|detail| Event::Failed {
                    what: "Couldn't open Properties",
                    detail,
                })
            })
    }
}

fn submit_path_request(
    mailbox: &PathMailbox,
    signal: &SyncSender<()>,
    path: PathBuf,
    id: u64,
    label: &str,
) -> Result<(), String> {
    let mut slot = mailbox
        .request
        .try_lock()
        .map_err(|_| format!("{label} is busy; try again shortly"))?;
    let generation = mailbox
        .generation
        .fetch_add(1, Ordering::AcqRel)
        .wrapping_add(1);
    *slot = Some(PathRequest {
        path,
        id,
        generation,
    });
    drop(slot);
    match signal.try_send(()) {
        Ok(()) | Err(TrySendError::Full(())) => Ok(()),
        Err(TrySendError::Disconnected(())) => Err(format!("{label} worker is unavailable")),
    }
}

impl Drop for Platform {
    fn drop(&mut self) {
        // Dropping channel senders wakes idle workers. An in-progress OS handler
        // must never make closing the window wait indefinitely for a thread join.
        self.stop.store(true, Ordering::Release);
        self.operations.cancel_all();
    }
}

fn validate_job_size(job: &Job) -> io::Result<()> {
    let paths = match job {
        Job::Copy { sources, .. }
        | Job::DropFiles { sources, .. }
        | Job::Recycle(sources)
        | Job::DeletePermanent(sources)
        | Job::WriteClipboard { sources, .. } => Some(sources),
        _ => None,
    };
    if let Some(paths) = paths {
        if paths.is_empty() {
            return Err(invalid("No items selected"));
        }
        if paths.len() > MAX_SELECTION {
            return Err(invalid(format!(
                "Select at most {MAX_SELECTION} items per operation"
            )));
        }
        if paths
            .iter()
            .try_fold(0usize, |bytes, path| {
                bytes.checked_add(path.as_os_str().len().saturating_mul(2))
            })
            .is_none_or(|bytes| bytes > 1024 * 1024)
        {
            return Err(invalid("Selected paths exceed the 1 MiB operation budget"));
        }
    }
    if let Job::Save { bytes, .. } = job {
        if bytes.len() > MAX_SAVE_BYTES {
            return Err(invalid("Settings exceed the 4 MiB save limit"));
        }
    }
    Ok(())
}

fn is_shell_job(job: &Job) -> bool {
    matches!(
        job,
        Job::Open(_)
            | Job::Application(_)
            | Job::Reveal(_)
            | Job::FolderOptions
            | Job::Explorer(_)
            | Job::Console(_)
            | Job::NewWindow(_)
            | Job::NewHomeWindow
            | Job::Metrics { .. }
    )
}

fn run_shell_job(job: Job, events: &SyncSender<Event>, stop: &AtomicBool, wake: &Wake) {
    if stop.load(Ordering::Acquire) {
        return;
    }
    let what = job.failure_context();
    let event = match job {
        Job::Open(path) => match native::open(&path) {
            Ok(()) => Event::Opened(path),
            Err(error) => Event::Failed {
                what,
                detail: error.to_string(),
            },
        },
        Job::Metrics { id } => match native::metrics() {
            Ok(metrics) => Event::Metrics {
                id,
                private_bytes: metrics.private_bytes,
                working_set_bytes: metrics.working_set_bytes,
                handles: metrics.handles,
                cpu_seconds: metrics.cpu_seconds,
            },
            Err(error) => Event::Failed {
                what,
                detail: format!("Process diagnostics: {error}"),
            },
        },
        job => {
            let result = match job {
                Job::Application(identifier) => native::application(&identifier),
                Job::Reveal(path) => native::reveal(&path),
                Job::FolderOptions => folder_options::open(),
                Job::Explorer(path) => std::process::Command::new("explorer.exe")
                    .arg(path)
                    .spawn()
                    .map(|_| ()),
                Job::Console(path) => open_console(&path),
                Job::NewWindow(path) => std::env::current_exe()
                    .and_then(|exe| {
                        std::process::Command::new(exe)
                            .arg("--new-window")
                            .arg(path)
                            .spawn()
                    })
                    .map(|_| ()),
                Job::NewHomeWindow => std::env::current_exe()
                    .and_then(|exe| {
                        std::process::Command::new(exe)
                            .args(["--explorer", "--home", "--new-window"])
                            .spawn()
                    })
                    .map(|_| ()),
                _ => unreachable!("Only interactive Shell jobs enter this lane"),
            };
            match result {
                Ok(()) => Event::Done {
                    message: String::new(),
                    refresh: false,
                },
                Err(error) => Event::Failed {
                    what,
                    detail: error.to_string(),
                },
            }
        }
    };
    if !stop.load(Ordering::Acquire) && events.send(event).is_ok() {
        wake();
    }
}

fn operation_worker(
    jobs: Receiver<OperationJob>,
    events: SyncSender<Event>,
    stop: Arc<AtomicBool>,
    owner: Arc<AtomicIsize>,
    wake: Wake,
    history: HistoryHandle,
    pending: Arc<AtomicU64>,
) {
    while let Ok(OperationJob { job, operation }) = jobs.recv() {
        struct Pending<'a> {
            count: &'a AtomicU64,
            wake: &'a Wake,
            tracked: bool,
        }
        impl Drop for Pending<'_> {
            fn drop(&mut self) {
                if self.tracked {
                    self.count.fetch_sub(1, Ordering::AcqRel);
                    (self.wake)();
                }
            }
        }
        let _pending = Pending {
            count: &pending,
            wake: &wake,
            tracked: true,
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        let _current = operations::Current::enter(operation.clone());
        if let Some(op) = &operation {
            if op.cancel.load(Ordering::Acquire) {
                op.finish(&Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Cancelled before starting",
                )));
                continue;
            }
            op.start();
        }
        let what = job.failure_context();
        let (result, refresh) = match job {
            Job::ExplorerIntegration { .. } | Job::Properties(_) | Job::Launcher { .. } => {
                unreachable!("Independent on-demand shell work")
            }
            Job::Metrics { .. }
            | Job::Open(_)
            | Job::Application(_)
            | Job::Reveal(_)
            | Job::FolderOptions
            | Job::Explorer(_)
            | Job::Console(_)
            | Job::NewWindow(_)
            | Job::NewHomeWindow => unreachable!("Interactive Shell lane"),
            Job::NewFile { parent, name } => (
                history.mutate("New File", || {
                    Mutation::created(new_file(&parent, &name), &parent.join(&name))
                }),
                true,
            ),
            Job::NewFolder { parent, name } => (
                history.mutate("New Folder", || {
                    Mutation::created(new_folder(&parent, &name), &parent.join(&name))
                }),
                true,
            ),
            Job::Rename { path, name } => (
                history.mutate("Rename", || {
                    rename_tracked_owned(&path, &name, owner.load(Ordering::Acquire), &stop)
                }),
                true,
            ),
            Job::Copy {
                sources,
                destination,
                cut,
            } => (
                history.mutate(if cut { "Move" } else { "Copy" }, || {
                    copy_tracked_owned(
                        &sources,
                        &destination,
                        cut,
                        owner.load(Ordering::Acquire),
                        &stop,
                    )
                }),
                true,
            ),
            Job::DropFiles {
                sources,
                destination,
                control,
                shift,
            } => (
                history.mutate("Transfer", || {
                    drop_files_tracked_owned(
                        &sources,
                        &destination,
                        control,
                        shift,
                        owner.load(Ordering::Acquire),
                        &stop,
                    )
                }),
                true,
            ),
            Job::Recycle(paths) => (
                history.mutate("Delete", || {
                    delete_tracked_owned(&paths, false, owner.load(Ordering::Acquire), &stop)
                }),
                true,
            ),
            Job::DeletePermanent(paths) => (
                history.mutate("Delete", || {
                    delete_tracked_owned(&paths, true, owner.load(Ordering::Acquire), &stop)
                }),
                true,
            ),
            Job::Undo => (history.replay(Direction::Undo), true),
            Job::Redo => (history.replay(Direction::Redo), true),
            Job::WriteClipboard { sources, cut } => (write_file_clipboard(&sources, cut), false),
            Job::ReadClipboard { destination } => (
                history.mutate("Paste", || {
                    paste_file_clipboard_tracked(&destination, owner.load(Ordering::Acquire), &stop)
                }),
                true,
            ),
            Job::Save { path, bytes } => {
                (atomic_save(&path, &bytes).map(|()| String::new()), false)
            }
            Job::Preview { .. } | Job::Resolve { .. } => {
                unreachable!("Path work uses its own lane")
            }
        };
        if let Some(op) = &operation {
            op.finish(&result);
        }
        if stop.load(Ordering::Acquire) {
            break;
        }
        let event = match result {
            Ok(message) => Event::Done { message, refresh },
            Err(error) => {
                // A recursive operation can leave a partial destination. Refresh
                // before reporting failure so the visible filesystem stays true.
                if refresh
                    && events
                        .send(Event::Done {
                            message: String::new(),
                            refresh: true,
                        })
                        .is_err()
                {
                    break;
                }
                if refresh {
                    wake();
                }
                Event::Failed {
                    what,
                    detail: error.to_string(),
                }
            }
        };
        if events.send(event).is_err() {
            break;
        }
        wake();
    }
}

fn path_worker(
    signal: Receiver<()>,
    mailbox: Arc<PathMailbox>,
    stop: Arc<AtomicBool>,
    wake: Wake,
    resolve: bool,
) {
    while signal.recv().is_ok() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let request = match mailbox.request.lock() {
            Ok(mut slot) => slot.take(),
            Err(_) => break,
        };
        let Some(request) = request else { continue };
        let event = if resolve {
            match resolve_path(&request.path) {
                Ok((path, is_dir)) => Event::Resolved {
                    id: request.id,
                    path,
                    is_dir,
                },
                Err(error) => Event::ResolveFailed {
                    id: request.id,
                    path: request.path.clone(),
                    detail: error.to_string(),
                },
            }
        } else {
            let content =
                read_preview(&request.path).unwrap_or_else(|error| PreviewContent::Info {
                    bytes: 0,
                    message: format!("Cannot preview this item: {error}"),
                });
            Event::Preview {
                id: request.id,
                path: request.path,
                content,
            }
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        if mailbox.generation.load(Ordering::Acquire) != request.generation {
            continue;
        }
        if let Ok(mut result) = mailbox.result.lock() {
            *result = Some(event);
        } else {
            break;
        }
        wake();
    }
}

fn resolve_path(path: &Path) -> io::Result<(PathBuf, bool)> {
    let path = std::path::absolute(path)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(invalid("This path is not an ordinary file or folder"));
    }
    Ok((path, metadata.is_dir()))
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

fn contextual(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.ends_with([' ', '.'])
        || name.encode_utf16().count() > 255
        || name.chars().any(|c| {
            c <= '\u{1f}' || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return Err(invalid(
            "Use a nonempty name up to 255 characters, without / \\ : * ? \" < > | or a final space/dot",
        ));
    }
    let stem = name.split('.').next().unwrap_or("").to_uppercase();
    let reserved = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    if reserved {
        return Err(invalid("This name is reserved by Windows"));
    }
    Ok(())
}

fn reject_reparse(path: &Path, metadata: &fs::Metadata) -> io::Result<()> {
    let caps = foldex_fs::capabilities::probe(path)?;
    if caps.redirects() || caps.unknown_reparse() {
        return Err(invalid(
            "Symbolic links, junctions and unknown reparse items are not traversed by file operations",
        ));
    }
    if !metadata.is_file() && !metadata.is_dir() {
        return Err(invalid("Only ordinary files and folders are supported"));
    }
    Ok(())
}

fn ordinary_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|error| contextual(path, error))?;
    reject_reparse(path, &metadata).map_err(|error| contextual(path, error))?;
    Ok(metadata)
}

fn existing_directory(path: &Path) -> io::Result<PathBuf> {
    if !ordinary_metadata(path)?.is_dir() {
        return Err(invalid(format!("{} is not a folder", path.display())));
    }
    fs::canonicalize(path).map_err(|error| contextual(path, error))
}

#[cfg(any(not(windows), test))]
fn ensure_missing(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already exists; nothing was overwritten", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(contextual(path, error)),
    }
}

fn new_folder(parent: &Path, name: &str) -> io::Result<String> {
    validate_name(name)?;
    let path = existing_directory(parent)?.join(name);
    fs::create_dir(&path).map_err(|error| contextual(&path, error))?;
    Ok(format!("Created {name}"))
}

fn new_file(parent: &Path, name: &str) -> io::Result<String> {
    validate_name(name)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(parent.join(name))?;
    Ok(format!("Created {name}"))
}

fn open_console(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd.exe")
            .current_dir(path)
            .creation_flags(0x0000_0010)
            .spawn()?;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Console opening requires Windows",
        ))
    }
}

#[cfg(not(windows))]
fn rename_owned(
    path: &Path,
    name: &str,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> io::Result<String> {
    validate_name(name)?;
    #[cfg(windows)]
    {
        windows_ops::rename(path, name, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
        check_stop(stop)?;
        rename_item(path, name)
    }
}

fn rename_tracked_owned(path: &Path, name: &str, owner: isize, stop: &Arc<AtomicBool>) -> Mutation {
    #[cfg(windows)]
    {
        windows_ops::rename_tracked(path, name, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        untracked(rename_owned(path, name, owner, stop))
    }
}

fn copy_tracked_owned(
    sources: &[PathBuf],
    destination: &Path,
    cut: bool,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> Mutation {
    #[cfg(windows)]
    {
        windows_ops::transfer_tracked(sources, destination, cut, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        untracked(copy_owned(sources, destination, cut, owner, stop))
    }
}

fn delete_tracked_owned(
    paths: &[PathBuf],
    permanent: bool,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> Mutation {
    #[cfg(windows)]
    {
        windows_ops::delete_tracked(paths, permanent, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        if permanent {
            Mutation::failed(io::Error::new(
                io::ErrorKind::Unsupported,
                "Permanent deletion requires Windows",
            ))
        } else {
            untracked(recycle_owned(paths, owner, stop))
        }
    }
}

fn drop_files_tracked_owned(
    sources: &[PathBuf],
    destination: &Path,
    control: bool,
    shift: bool,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> Mutation {
    #[cfg(windows)]
    {
        windows_ops::drop_files_tracked(sources, destination, control, shift, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        untracked(drop_files_owned(
            sources,
            destination,
            control,
            shift,
            owner,
            stop,
        ))
    }
}

fn paste_file_clipboard_tracked(
    destination: &Path,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> Mutation {
    #[cfg(windows)]
    {
        windows_ops::paste_tracked(destination, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        untracked(paste_file_clipboard(destination, owner, stop))
    }
}

#[cfg(not(windows))]
fn untracked(result: io::Result<String>) -> Mutation {
    Mutation {
        changed: result.is_ok(),
        result,
        steps: vec![],
    }
}

#[cfg(not(windows))]
fn copy_owned(
    sources: &[PathBuf],
    destination: &Path,
    cut: bool,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> io::Result<String> {
    #[cfg(windows)]
    {
        windows_ops::transfer(sources, destination, cut, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
        copy_items(sources, destination, cut, stop)
    }
}

#[cfg(not(windows))]
fn recycle_owned(paths: &[PathBuf], owner: isize, stop: &Arc<AtomicBool>) -> io::Result<String> {
    #[cfg(windows)]
    {
        windows_ops::recycle(paths, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        let _ = (paths, owner, stop);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Recycle is available on Windows; permanent deletion is not substituted",
        ))
    }
}

#[cfg(not(windows))]
fn drop_files_owned(
    sources: &[PathBuf],
    destination: &Path,
    control: bool,
    shift: bool,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> io::Result<String> {
    #[cfg(windows)]
    {
        windows_ops::drop_files(sources, destination, control, shift, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        let _ = owner;
        if control && shift {
            return Err(invalid(
                "Creating shortcuts with Ctrl+Shift drag is not supported",
            ));
        }
        copy_items(sources, destination, shift, stop)
    }
}

fn write_file_clipboard(sources: &[PathBuf], cut: bool) -> io::Result<String> {
    #[cfg(windows)]
    {
        windows_ops::write_clipboard(sources, cut)
    }
    #[cfg(not(windows))]
    {
        let _ = (sources, cut);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "File clipboard is available on Windows",
        ))
    }
}

#[cfg(not(windows))]
fn paste_file_clipboard(
    destination: &Path,
    owner: isize,
    stop: &Arc<AtomicBool>,
) -> io::Result<String> {
    #[cfg(windows)]
    {
        windows_ops::paste(destination, owner, stop.clone())
    }
    #[cfg(not(windows))]
    {
        let _ = (destination, owner, stop);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "File clipboard is available on Windows",
        ))
    }
}

// Retained for the non-Windows fallback and isolated filesystem regression
// tests. User-requested Windows mutations always take windows_ops above.
#[cfg(any(not(windows), test))]
fn rename_item(path: &Path, name: &str) -> io::Result<String> {
    validate_name(name)?;
    ordinary_metadata(path)?;
    let source = fs::canonicalize(path).map_err(|error| contextual(path, error))?;
    let parent = source
        .parent()
        .ok_or_else(|| invalid("A drive root cannot be renamed"))?;
    let target = parent.join(name);
    if source == target {
        return Ok("Name unchanged".into());
    }
    // Windows supports case-only renames. MoveFileEx without REPLACE_EXISTING
    // handles that case while refusing a genuinely different existing item.
    native::move_no_replace(&source, &target).map_err(|error| contextual(path, error))?;
    Ok(format!("Renamed to {name}"))
}

#[cfg(any(not(windows), test))]
#[derive(Default)]
struct CopyStats {
    entries: usize,
    bytes: u64,
    path_bytes: usize,
}

#[cfg(any(not(windows), test))]
#[derive(Clone)]
struct CopiedEntry {
    path: PathBuf,
    is_dir: bool,
    len: u64,
    modified: Option<std::time::SystemTime>,
}

fn check_stop(stop: &AtomicBool) -> io::Result<()> {
    if stop.load(Ordering::Acquire) || operations::cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Operation stopped; any partial destination is preserved",
        ))
    } else {
        Ok(())
    }
}

#[cfg(any(not(windows), test))]
fn claim_entry(path: &Path, stats: &mut CopyStats) -> io::Result<()> {
    let bytes = path.as_os_str().len().saturating_mul(2);
    if stats.entries >= MAX_OPERATION_ENTRIES
        || stats.path_bytes.saturating_add(bytes) > MAX_OPERATION_PATH_BYTES
    {
        return Err(invalid(
            "Operation reached the 100,000-entry / 32 MiB path budget; split it into smaller selections. Any partial copy is preserved",
        ));
    }
    stats.entries += 1;
    stats.path_bytes += bytes;
    Ok(())
}

#[cfg(any(not(windows), test))]
fn copy_items(
    sources: &[PathBuf],
    destination: &Path,
    cut: bool,
    stop: &AtomicBool,
) -> io::Result<String> {
    if sources.is_empty() || sources.len() > MAX_SELECTION {
        return Err(invalid("Select between 1 and 4,096 items"));
    }
    let destination = existing_directory(destination)?;
    let mut unique = HashSet::with_capacity(sources.len());
    let mut failures = Vec::new();
    let mut failure_count = 0;
    let mut completed = 0;
    let mut stats = CopyStats::default();
    let mut buffer = vec![0; COPY_BUFFER_SIZE];
    for source in sources {
        check_stop(stop)?;
        let result = (|| {
            ordinary_metadata(source)?;
            let source = fs::canonicalize(source).map_err(|error| contextual(source, error))?;
            if !unique.insert(source.clone()) {
                return Err(invalid("The same source was selected more than once"));
            }
            let name = source
                .file_name()
                .ok_or_else(|| invalid("A drive root cannot be copied or moved"))?;
            let target = destination.join(name);
            if destination.starts_with(&source) {
                return Err(invalid(
                    "The destination cannot be the source folder or a folder inside it",
                ));
            }
            ensure_missing(&target)?;
            if cut {
                if stats.entries >= MAX_OPERATION_ENTRIES
                    || stats
                        .path_bytes
                        .saturating_add(source.as_os_str().len().saturating_mul(2))
                        > MAX_OPERATION_PATH_BYTES
                {
                    return Err(invalid(
                        "Operation reached its entry/path budget; remaining sources were kept",
                    ));
                }
                match native::move_no_replace(&source, &target) {
                    Ok(()) => {
                        operations::changed();
                        claim_entry(&source, &mut stats)?;
                        return Ok(());
                    }
                    Err(error) if native::is_cross_device(&error) => {}
                    Err(error) => return Err(contextual(&source, error)),
                }
            }
            let mut copied = Vec::new();
            copy_tree(
                &source,
                &target,
                &mut buffer,
                &mut stats,
                &mut copied,
                cut,
                stop,
            )?;
            if cut {
                remove_copied_source(&copied, stop)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => completed += 1,
            Err(error) => {
                failure_count += 1;
                if failures.len() < 5 {
                    failures.push(format!("{}: {error}", source.display()));
                }
            }
        }
    }
    let action = if cut { "Moved" } else { "Copied" };
    if failure_count > 0 {
        let mut message = format!(
            "{action} {completed}/{} selected items ({} entries, {} bytes). {failure_count} failed; existing files were not overwritten. {}",
            sources.len(),
            stats.entries,
            stats.bytes,
            failures.join("; ")
        );
        if failure_count > failures.len() {
            message.push_str(&format!(
                "; {} additional failures",
                failure_count - failures.len()
            ));
        }
        return Err(io::Error::other(message));
    }
    Ok(format!(
        "{action} {completed} item{}",
        if completed == 1 { "" } else { "s" }
    ))
}

#[cfg(any(not(windows), test))]
fn copy_tree(
    source: &Path,
    target: &Path,
    buffer: &mut [u8],
    stats: &mut CopyStats,
    copied: &mut Vec<CopiedEntry>,
    record_sources: bool,
    stop: &AtomicBool,
) -> io::Result<()> {
    // Explicit DFS avoids a recursion-stack overflow on deeply nested trees.
    let mut pending = vec![(source.to_owned(), target.to_owned(), 0usize)];
    let path_cost = |source: &Path, target: &Path| {
        source
            .as_os_str()
            .len()
            .saturating_add(target.as_os_str().len())
            .saturating_mul(2)
    };
    let mut pending_path_bytes = path_cost(source, target);
    while let Some((source, target, depth)) = pending.pop() {
        pending_path_bytes = pending_path_bytes.saturating_sub(path_cost(&source, &target));
        check_stop(stop)?;
        if depth > 512 {
            return Err(invalid("Folder nesting exceeds the 512-level copy limit"));
        }
        let metadata = ordinary_metadata(&source)?;
        claim_entry(&source, stats)?;
        if metadata.is_dir() {
            fs::create_dir(&target).map_err(|error| contextual(&target, error))?;
            operations::changed();
            if record_sources {
                copied.push(CopiedEntry {
                    path: source.clone(),
                    is_dir: true,
                    len: 0,
                    modified: None,
                });
            }
            let iterator = fs::read_dir(&source).map_err(|error| contextual(&source, error))?;
            for child in iterator {
                let child = child.map_err(|error| contextual(&source, error))?;
                if pending.len().saturating_add(stats.entries) >= MAX_OPERATION_ENTRIES {
                    return Err(invalid(
                        "Folder exceeds the 100,000-entry copy budget; any partial destination is preserved",
                    ));
                }
                let child_source = child.path();
                let child_target = target.join(child.file_name());
                let cost = path_cost(&child_source, &child_target);
                if pending_path_bytes
                    .saturating_add(stats.path_bytes)
                    .saturating_add(cost)
                    > MAX_OPERATION_PATH_BYTES
                {
                    return Err(invalid(
                        "Folder exceeds the 32 MiB path budget; any partial destination is preserved",
                    ));
                }
                pending_path_bytes += cost;
                pending.push((child_source, child_target, depth + 1));
            }
        } else {
            let mut read_options = OpenOptions::new();
            read_options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                // Prevent the source being changed/deleted while it is copied.
                read_options.share_mode(1);
            }
            let mut input = read_options
                .open(&source)
                .map_err(|error| contextual(&source, error))?;
            let fingerprint = input.metadata()?;
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|error| contextual(&target, error))?;
            operations::changed();
            loop {
                check_stop(stop)?;
                let count = input
                    .read(buffer)
                    .map_err(|error| contextual(&source, error))?;
                if count == 0 {
                    break;
                }
                output
                    .write_all(&buffer[..count])
                    .map_err(|error| contextual(&target, error))?;
                stats.bytes = stats.bytes.saturating_add(count as u64);
            }
            output.flush().map_err(|error| contextual(&target, error))?;
            let mut times = fs::FileTimes::new();
            if let Ok(modified) = fingerprint.modified() {
                times = times.set_modified(modified);
            }
            if let Ok(accessed) = fingerprint.accessed() {
                times = times.set_accessed(accessed);
            }
            output
                .set_times(times)
                .map_err(|error| contextual(&target, error))?;
            output
                .set_permissions(fingerprint.permissions())
                .map_err(|error| contextual(&target, error))?;
            if record_sources {
                copied.push(CopiedEntry {
                    path: source,
                    is_dir: false,
                    len: fingerprint.len(),
                    modified: fingerprint.modified().ok(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(any(not(windows), test))]
fn remove_copied_source(copied: &[CopiedEntry], stop: &AtomicBool) -> io::Result<()> {
    // Delete only the exact successfully copied entries, deepest first. A new
    // child makes remove_dir fail rather than deleting content created mid-copy.
    for entry in copied.iter().rev() {
        check_stop(stop)?;
        let metadata = ordinary_metadata(&entry.path)?;
        if metadata.is_dir() != entry.is_dir
            || (!entry.is_dir
                && (metadata.len() != entry.len || metadata.modified().ok() != entry.modified))
        {
            return Err(io::Error::other(format!(
                "{} changed while copying. The destination is preserved and the changed source was kept",
                entry.path.display()
            )));
        }
        let result = if entry.is_dir {
            fs::remove_dir(&entry.path)
        } else {
            fs::remove_file(&entry.path)
        };
        result.map_err(|error| contextual(&entry.path, error))?;
    }
    Ok(())
}

static SAVE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn atomic_save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_SAVE_BYTES {
        return Err(invalid("Settings exceed the 4 MiB save limit"));
    }
    if !path.is_absolute() {
        return Err(invalid("Use an absolute settings path"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Save path has no parent folder"))?;
    if parent.as_os_str().is_empty() {
        return Err(invalid("Use an absolute settings path"));
    }
    fs::create_dir_all(parent).map_err(|error| contextual(parent, error))?;
    let parent = existing_directory(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("Save path has no file name"))?;
    let target = parent.join(name);
    if let Ok(metadata) = fs::symlink_metadata(&target) {
        reject_reparse(&target, &metadata)?;
    }
    let mut temporary = None;
    for _ in 0..8 {
        let sequence = SAVE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".foldex-save-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(contextual(&candidate, error)),
        }
    }
    let (temporary, mut file) = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Cannot allocate an atomic settings file",
        )
    })?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &target)
    })();
    if result.is_err() {
        // This exact temporary path was created by this invocation.
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| contextual(path, error))
}

pub(crate) fn read_preview(path: &Path) -> io::Result<PreviewContent> {
    let metadata = fs::symlink_metadata(path).map_err(|error| contextual(path, error))?;
    let info = |message: String| PreviewContent::Info {
        bytes: metadata.len(),
        message,
    };
    if (!metadata.is_file() && !metadata.is_dir())
        || foldex_fs::capabilities::background_path(path).is_err()
    {
        return Ok(info(
            "Open this item with its associated app to view its contents.".into(),
        ));
    }
    if metadata.is_dir() {
        return Ok(info("Folder. Open to browse its contents.".into()));
    }
    let caps = foldex_fs::capabilities::background_path(path)?;
    if !caps.can_read_content() {
        return Ok(info(
            "Online-only or unavailable content. Open explicitly to download or view it.".into(),
        ));
    }
    let mut file = foldex_fs::capabilities::open_background_content(path)
        .map_err(|error| contextual(path, error))?;
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(TEXT_LIMIT + 4));
    Read::by_ref(&mut file)
        .take((TEXT_LIMIT + 4) as u64)
        .read_to_end(&mut bytes)?;
    let format = image::guess_format(&bytes).ok();
    if matches!(
        format,
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP)
    ) {
        if metadata.len() > IMAGE_FILE_LIMIT as u64 {
            return Ok(info(
                "Image exceeds the 16 MiB preview limit. Open it to view the original.".into(),
            ));
        }
        file.take((IMAGE_FILE_LIMIT + 1 - bytes.len()) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > IMAGE_FILE_LIMIT {
            return Ok(info("Image exceeds the 16 MiB preview limit.".into()));
        }
        return Ok(match decode_image(&bytes, format.unwrap()) {
            Ok(image) => image,
            Err(error) => info(format!("Image preview unavailable: {error}")),
        });
    }
    let truncated = metadata.len() > TEXT_LIMIT as u64 || bytes.len() > TEXT_LIMIT;
    bytes.truncate(TEXT_LIMIT);
    match decode_text(&bytes, truncated) {
        Some(mut text) => {
            if truncated {
                text.push_str("\n\n… Preview limited to the first 256 KiB.");
            }
            Ok(PreviewContent::Text(text))
        }
        None => Ok(info(
            "Binary file or unsupported text encoding. Open with its associated app.".into(),
        )),
    }
}

fn image_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(IMAGE_DECODE_LIMIT);
    limits
}

fn decode_image(bytes: &[u8], format: ImageFormat) -> Result<PreviewContent, String> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(image_limits());
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    if decoder.total_bytes() > IMAGE_DECODE_LIMIT {
        return Err("Decoded image exceeds the 64 MiB preview budget".into());
    }
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let image = DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    let mut image = if image.width() > IMAGE_MAX_EDGE || image.height() > IMAGE_MAX_EDGE {
        image.thumbnail(IMAGE_MAX_EDGE, IMAGE_MAX_EDGE)
    } else {
        image
    };
    image.apply_orientation(orientation);
    let rgba = image.into_rgba8();
    Ok(PreviewContent::Image {
        size: [rgba.width() as usize, rgba.height() as usize],
        rgba: rgba.into_raw(),
    })
}

fn decode_text(bytes: &[u8], truncated: bool) -> Option<String> {
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little = bytes[0] == 0xff;
        let mut words: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|pair| {
                if little {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        if truncated
            && words
                .last()
                .is_some_and(|word| (0xd800..=0xdbff).contains(word))
        {
            words.pop();
        }
        String::from_utf16(&words).ok()?
    } else {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(error) if truncated && error.error_len().is_none() => {
                std::str::from_utf8(&bytes[..error.valid_up_to()])
                    .ok()?
                    .to_owned()
            }
            Err(_) => return None,
        }
    };
    if text.contains('\0') {
        return None;
    }
    let controls = text
        .chars()
        .filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{000c}'))
        .count();
    if controls > 0 && controls.saturating_mul(100) > text.len().max(1) {
        return None;
    }
    Some(text)
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::{os::windows::ffi::OsStrExt, ptr};
    #[cfg(test)]
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
            ProcessStatus::{
                GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
            },
            Threading::{GetCurrentProcess, GetProcessHandleCount, GetProcessTimes},
        },
        UI::{
            Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems, ShellExecuteW},
            WindowsAndMessaging::SW_SHOWNORMAL,
        },
    };

    pub(super) fn metrics() -> io::Result<ProcessMetrics> {
        // The current-process pseudo-handle requires no OpenProcess, access to
        // other processes, enumeration, or CloseHandle.
        let process = unsafe { GetCurrentProcess() };
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        let mut memory = PROCESS_MEMORY_COUNTERS_EX {
            cb: size,
            ..Default::default()
        };
        let api_error =
            |api: &str| io::Error::other(format!("{api}: {}", io::Error::last_os_error()));
        if unsafe {
            GetProcessMemoryInfo(
                process,
                (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
                size,
            )
        } == 0
        {
            return Err(api_error("GetProcessMemoryInfo"));
        }
        let mut handles = 0;
        if unsafe { GetProcessHandleCount(process, &mut handles) } == 0 {
            return Err(api_error("GetProcessHandleCount"));
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        if unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }
            == 0
        {
            return Err(api_error("GetProcessTimes"));
        }
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Ok(ProcessMetrics {
            private_bytes: memory.PrivateUsage as u64,
            working_set_bytes: memory.WorkingSetSize as u64,
            handles,
            cpu_seconds: (ticks(kernel) as f64 + ticks(user) as f64) / 10_000_000.0,
        })
    }

    struct Apartment;
    impl Apartment {
        fn new() -> io::Result<Self> {
            // This is a dedicated background thread; no UI-thread COM state is changed.
            hresult(
                unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) },
                "Initialize Windows shell",
            )?;
            Ok(Self)
        }
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }

    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut value: Vec<u16> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(invalid("Path contains a null character"));
        }
        value.push(0);
        Ok(value)
    }

    fn shell_wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut value = wide(path)?;
        // Shell parsing accepts ordinary absolute paths more consistently than
        // the verbatim prefix returned by std::fs::canonicalize on Windows.
        if value.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
            value.drain(..8);
            value.splice(..0, [92, 92]);
        } else if value.starts_with(&[92, 92, 63, 92])
            && value.len() >= 7
            && matches!(value[4], 65..=90 | 97..=122)
            && value[5] == 58
        {
            value.drain(..4);
        }
        Ok(value)
    }

    fn absolute_existing(path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path).map_err(|error| contextual(path, error))
    }

    fn hresult(result: i32, operation: &str) -> io::Result<()> {
        if result >= 0 {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{operation} failed (Windows HRESULT 0x{:08X})",
                result as u32
            )))
        }
    }

    pub(super) fn open(path: &Path) -> io::Result<()> {
        let _apartment = Apartment::new()?;
        let path = shell_wide(&absolute_existing(path)?)?;
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let result = unsafe {
            ShellExecuteW(
                ptr::null_mut(),
                verb.as_ptr(),
                path.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if result > 32 {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "Windows could not open this item (shell code {result})"
            )))
        }
    }
    pub(super) fn application(identifier: &[u8]) -> io::Result<()> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Com::{CoTaskMemAlloc, CoTaskMemFree},
            UI::Shell::{
                SEE_MASK_FLAG_NO_UI, SEE_MASK_INVOKEIDLIST, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
                ShellExecuteExW,
            },
        };
        if !crate::launcher::applications::valid_target(identifier) {
            return Err(io::Error::other("Invalid installed application identifier"));
        }
        let _apartment = Apartment::new()?;
        struct Memory(*mut std::ffi::c_void);
        impl Drop for Memory {
            fn drop(&mut self) {
                unsafe {
                    CoTaskMemFree(self.0);
                }
            }
        }
        let pointer = unsafe { CoTaskMemAlloc(identifier.len()) };
        if pointer.is_null() {
            return Err(io::Error::other(
                "Unable to allocate a Windows application identifier",
            ));
        }
        let memory = Memory(pointer);
        unsafe {
            std::ptr::copy_nonoverlapping(
                identifier.as_ptr(),
                memory.0.cast::<u8>(),
                identifier.len(),
            );
        }
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpIDList: memory.0,
            nShow: SW_SHOWNORMAL,
            ..Default::default()
        };
        let success = unsafe { ShellExecuteExW(&mut info) } != 0;
        let error = (!success).then(io::Error::last_os_error);
        if !info.hProcess.is_null() {
            unsafe {
                CloseHandle(info.hProcess);
            }
        }
        if let Some(error) = error {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub(super) fn reveal(path: &Path) -> io::Result<()> {
        let _apartment = Apartment::new()?;
        let path = shell_wide(&absolute_existing(path)?)?;
        let id = unsafe { ILCreateFromPathW(path.as_ptr()) };
        if id.is_null() {
            return Err(io::Error::other("Windows could not locate this item"));
        }
        let result = unsafe { SHOpenFolderAndSelectItems(id, 0, ptr::null(), 0) };
        unsafe {
            ILFree(id);
        }
        hresult(result, "Reveal item in Windows Explorer")
    }

    #[cfg(test)]
    pub(super) fn move_no_replace(source: &Path, target: &Path) -> io::Result<()> {
        let source = wide(source)?;
        let target = wide(target)?;
        // Flags zero: never REPLACE_EXISTING or COPY_ALLOWED. A cross-volume
        // move returns ERROR_NOT_SAME_DEVICE and uses the checked copy path.
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0) } != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(test)]
    pub(super) fn is_cross_device(error: &io::Error) -> bool {
        error.raw_os_error() == Some(17)
    }

    #[cfg(test)]
    pub(super) fn clipboard_encoding_for_test(paths: &[PathBuf]) -> io::Result<Vec<u8>> {
        windows_ops::clipboard_encoding_for_test(paths)
    }
}

#[cfg(not(windows))]
mod native {
    use super::*;
    pub(super) fn application(_: &[u8]) -> io::Result<()> {
        Err(io::Error::other("Installed applications require Windows"))
    }
    pub(super) fn metrics() -> io::Result<ProcessMetrics> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Current-process metrics are available on Windows",
        ))
    }
    pub(super) fn open(_: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Shell open is available on Windows",
        ))
    }
    pub(super) fn reveal(_: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Shell reveal is available on Windows",
        ))
    }
    pub(super) fn move_no_replace(source: &Path, target: &Path) -> io::Result<()> {
        ensure_missing(target)?;
        fs::rename(source, target)
    }
    pub(super) fn is_cross_device(error: &io::Error) -> bool {
        error.kind() == io::ErrorKind::CrossesDevices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn cancellation_skips_queued_jobs_and_jobs_waiting_for_the_journal() {
        use std::time::{Duration, Instant};
        let dir = TestDirectory::new("cancel-queue");
        let platform = Platform::new(Arc::new(|| {})).unwrap();
        let history = platform.history_handle();
        let (entered, ready) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let holder = thread::spawn(move || {
            history.mutate("Gate", || {
                entered.send(()).unwrap();
                gate.recv().unwrap();
                Mutation {
                    result: Ok(String::new()),
                    steps: vec![],
                    changed: false,
                }
            })
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        for name in ["first", "second"] {
            platform
                .try_submit(Job::NewFolder {
                    parent: dir.0.clone(),
                    name: name.into(),
                })
                .unwrap();
        }
        let until = Instant::now() + Duration::from_secs(5);
        let records = platform.operations().snapshot();
        while Instant::now() < until
            && !records
                .iter()
                .any(|op| op.state() == operations::State::Running)
        {
            thread::sleep(Duration::from_millis(1));
        }
        platform.operations().cancel_all();
        release.send(()).unwrap();
        holder.join().unwrap().unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until && records.iter().any(|op| op.state().active()) {
            let _ = platform.try_recv();
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            records
                .iter()
                .all(|op| op.state() == operations::State::Cancelled)
        );
        assert!(!dir.0.join("first").exists() && !dir.0.join("second").exists());
        assert!(!platform.update_busy());
    }

    #[test]
    fn failed_queue_submission_does_not_leave_an_operation_record() {
        let (jobs, _rx) = mpsc::sync_channel(JOB_CAPACITY);
        let mut platform = Platform::new(Arc::new(|| {})).unwrap();
        platform.jobs = jobs; // A deterministic queue without a consumer.
        for _ in 0..JOB_CAPACITY {
            platform.try_submit(Job::Undo).unwrap();
        }
        assert!(platform.try_submit(Job::Undo).is_err());
        assert_eq!(platform.operations().snapshot().len(), JOB_CAPACITY);
        assert_eq!(
            platform.pending_jobs.load(Ordering::Acquire),
            JOB_CAPACITY as u64
        );
    }

    #[test]
    fn interactive_open_completes_while_mutation_history_is_locked() {
        use std::time::{Duration, Instant};
        let dir = TestDirectory::new("independent-shell");
        let platform = Platform::new(Arc::new(|| {})).unwrap();
        let history = platform.history_handle();
        let (entered, ready) = mpsc::sync_channel(1);
        let (release, gate) = mpsc::sync_channel(1);
        let holder = thread::spawn(move || {
            history.mutate("Test gate", || {
                entered.send(()).unwrap();
                gate.recv().unwrap();
                Mutation {
                    result: Ok(String::new()),
                    steps: vec![],
                    changed: false,
                }
            })
        });
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        platform
            .try_submit(Job::NewFolder {
                parent: dir.0.clone(),
                name: "queued".into(),
            })
            .unwrap();
        let begin = Instant::now();
        platform
            .try_submit(Job::Open(dir.0.join("missing-file.txt")))
            .unwrap();
        let until = begin + Duration::from_secs(5);
        let mut event = None;
        while Instant::now() < until && event.is_none() {
            event = platform.try_recv();
            thread::sleep(Duration::from_millis(1));
        }
        // Always release the journal before asserting, including failure paths.
        assert!(!dir.0.join("queued").exists());
        release.send(()).unwrap();
        holder.join().unwrap().unwrap();
        assert!(
            matches!(event, Some(Event::Failed { .. })),
            "Open must complete before the mutation is released: {event:?}"
        );
        println!(
            "independent_shell_open_ms={:.3}",
            begin.elapsed().as_secs_f64() * 1000.0
        );
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until && !dir.0.join("queued").exists() {
            let _ = platform.try_recv();
            thread::sleep(Duration::from_millis(1));
        }
        assert!(dir.0.join("queued").exists());
    }

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct TestDirectory(PathBuf);
    impl TestDirectory {
        fn new(name: &str) -> Self {
            // All fixtures stay below this app's workspace target directory.
            let base =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/platform-test-fixtures");
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = base.join(format!("{name}-{}-{sequence}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let base = fs::canonicalize(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/platform-test-fixtures"),
            )
            .unwrap();
            assert!(self.0.starts_with(&base) && self.0 != base);
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn windows_names_cannot_escape_or_use_devices() {
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "x\\y",
            "C:foo",
            "a\0b",
            "bad?",
            "trailing.",
            "trailing ",
            "con",
            "NUL.txt",
            "COM1",
            "LPT9.log",
            "COM¹",
        ] {
            assert!(validate_name(name).is_err(), "accepted {name:?}");
        }
        for name in [
            "hello",
            "my folder",
            "résumé.txt",
            "COM10.txt",
            ".gitignore",
        ] {
            assert!(validate_name(name).is_ok(), "rejected {name:?}");
        }
    }

    #[test]
    fn copy_nested_files_preserves_data_and_never_overwrites() {
        let fixture = TestDirectory::new("copy");
        let source = fixture.0.join("source");
        let destination = fixture.0.join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("one.txt"), b"first source").unwrap();
        fs::write(source.join("nested/two.txt"), b"nested contents").unwrap();
        let stop = AtomicBool::new(false);
        copy_items(&[source.clone()], &destination, false, &stop).unwrap();
        assert_eq!(
            fs::read(destination.join("source/nested/two.txt")).unwrap(),
            b"nested contents"
        );
        fs::write(destination.join("source/one.txt"), b"destination edit").unwrap();
        assert!(copy_items(&[source], &destination, false, &stop).is_err());
        assert_eq!(
            fs::read(destination.join("source/one.txt")).unwrap(),
            b"destination edit"
        );
    }

    #[test]
    fn copy_rejects_its_own_descendant_before_creating_anything() {
        let fixture = TestDirectory::new("descendant");
        let source = fixture.0.join("source");
        let child = source.join("child");
        fs::create_dir_all(&child).unwrap();
        let error = copy_items(&[source], &child, false, &AtomicBool::new(false)).unwrap_err();
        assert!(error.to_string().contains("inside it"));
        assert!(!child.join("source").exists());
    }

    #[test]
    fn mutation_failure_reports_partial_success_and_keeps_conflicts() {
        let fixture = TestDirectory::new("partial");
        let destination = fixture.0.join("destination");
        fs::create_dir(&destination).unwrap();
        let a = fixture.0.join("a.txt");
        let b = fixture.0.join("b.txt");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        fs::write(destination.join("b.txt"), b"existing").unwrap();
        let error = copy_items(&[a, b], &destination, false, &AtomicBool::new(false)).unwrap_err();
        assert!(error.to_string().contains("1/2"));
        assert_eq!(fs::read(destination.join("a.txt")).unwrap(), b"a");
        assert_eq!(fs::read(destination.join("b.txt")).unwrap(), b"existing");
    }

    #[test]
    fn rename_and_move_refuse_existing_targets() {
        let fixture = TestDirectory::new("rename");
        let a = fixture.0.join("a.txt");
        let b = fixture.0.join("b.txt");
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        assert!(rename_item(&a, "b.txt").is_err());
        assert_eq!(fs::read(&a).unwrap(), b"a");
        assert_eq!(fs::read(&b).unwrap(), b"b");
        rename_item(&a, "renamed.txt").unwrap();
        assert!(!a.exists());
        rename_item(&b, "B.txt").unwrap();
        assert_eq!(fs::read(fixture.0.join("B.txt")).unwrap(), b"b");
        let destination = fixture.0.join("destination");
        fs::create_dir(&destination).unwrap();
        copy_items(
            &[fixture.0.join("renamed.txt")],
            &destination,
            true,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!fixture.0.join("renamed.txt").exists());
        assert_eq!(fs::read(destination.join("renamed.txt")).unwrap(), b"a");
    }

    #[test]
    fn cross_volume_cleanup_keeps_a_source_changed_after_copy() {
        let fixture = TestDirectory::new("changed-source");
        let source = fixture.0.join("source.txt");
        let target = fixture.0.join("copy.txt");
        fs::write(&source, b"original").unwrap();
        let mut copied = Vec::new();
        copy_tree(
            &source,
            &target,
            &mut [0; 64],
            &mut CopyStats::default(),
            &mut copied,
            true,
            &AtomicBool::new(false),
        )
        .unwrap();
        fs::write(&source, b"new contents, longer").unwrap();
        assert!(remove_copied_source(&copied, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"new contents, longer");
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    #[test]
    fn preview_detects_binary_and_handles_utf16_and_partial_utf8() {
        assert!(decode_text(b"\0MZ\x01\x02", false).is_none());
        assert!(decode_text(b"\x01\x02\x03\x04", false).is_none());
        assert_eq!(decode_text(b"plain\ntext", false).unwrap(), "plain\ntext");
        assert_eq!(
            decode_text(&[0xff, 0xfe, b'H', 0, b'i', 0], false).unwrap(),
            "Hi"
        );
        assert_eq!(
            decode_text(&[0xfe, 0xff, 0, b'H', 0, b'i'], false).unwrap(),
            "Hi"
        );
        assert_eq!(decode_text(&[b'a', 0xe2, 0x82], true).unwrap(), "a");
        assert!(decode_text(&[b'a', 0xe2, 0x82], false).is_none());
    }

    #[test]
    fn preview_text_and_images_are_bounded() {
        let fixture = TestDirectory::new("preview");
        let text = fixture.0.join("large.txt");
        fs::write(&text, vec![b'x'; TEXT_LIMIT + 1_000]).unwrap();
        match read_preview(&text).unwrap() {
            PreviewContent::Text(value) => {
                assert!(value.len() < TEXT_LIMIT + 100);
                assert!(value.contains("256 KiB"));
            }
            _ => panic!("text not previewed"),
        }
        let image = fixture.0.join("image.png");
        image::RgbaImage::from_pixel(2_000, 1_000, image::Rgba([12, 34, 56, 255]))
            .save(&image)
            .unwrap();
        match read_preview(&image).unwrap() {
            PreviewContent::Image { size, rgba } => {
                assert_eq!(size, [1_600, 800]);
                assert_eq!(rgba.len(), size[0] * size[1] * 4);
            }
            other => panic!("image not previewed: {other:?}"),
        }
    }

    #[test]
    fn atomic_settings_save_replaces_without_leaving_temporary_files() {
        let fixture = TestDirectory::new("save");
        let path = fixture.0.join("settings.json");
        atomic_save(&path, b"old").unwrap();
        atomic_save(&path, b"new state").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new state");
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 1);
    }

    #[test]
    fn worker_returns_results_and_wakes_without_an_app_window() {
        let fixture = TestDirectory::new("worker");
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        let platform = Platform::new(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }))
        .unwrap();
        platform
            .try_submit(Job::NewFolder {
                parent: fixture.0.clone(),
                name: "created".into(),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let event = loop {
            if let Some(event) = platform.try_recv() {
                break event;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker did not respond"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(matches!(event, Event::Done { refresh: true, .. }));
        assert!(fixture.0.join("created").is_dir());
        while wakes.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(wakes.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn preview_worker_eventually_delivers_the_latest_selection() {
        let fixture = TestDirectory::new("preview-worker");
        let path = fixture.0.join("preview.txt");
        fs::write(&path, b"latest selection").unwrap();
        let platform = Platform::new(Arc::new(|| {})).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for id in 0..32 {
            loop {
                if platform
                    .try_submit(Job::Preview {
                        path: path.clone(),
                        id,
                    })
                    .is_ok()
                {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "preview request remained busy"
                );
                thread::yield_now();
            }
        }
        loop {
            if let Some(Event::Preview {
                id: 31, content, ..
            }) = platform.try_recv()
            {
                assert!(
                    matches!(content, PreviewContent::Text(ref text) if text == "latest selection")
                );
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "latest preview did not arrive"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn oversized_jobs_are_rejected_before_entering_the_queue() {
        assert!(
            validate_job_size(&Job::Copy {
                sources: Vec::new(),
                destination: PathBuf::new(),
                cut: false
            })
            .is_err()
        );
        assert!(
            validate_job_size(&Job::Save {
                path: PathBuf::new(),
                bytes: vec![0; MAX_SAVE_BYTES + 1]
            })
            .is_err()
        );
    }

    #[test]
    fn operation_budget_and_shutdown_are_enforced() {
        let mut stats = CopyStats {
            entries: MAX_OPERATION_ENTRIES,
            ..CopyStats::default()
        };
        assert!(claim_entry(Path::new("test"), &mut stats).is_err());
        assert!(check_stop(&AtomicBool::new(true)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn metrics_sample_only_the_current_process_and_keep_request_identity() {
        let before = native::metrics().unwrap();
        assert!(before.private_bytes > 0);
        assert!(before.working_set_bytes > 0);
        assert!(before.handles > 0);
        assert!(before.cpu_seconds.is_finite() && before.cpu_seconds >= 0.0);
        let platform = Platform::new(Arc::new(|| {})).unwrap();
        platform.try_submit(Job::Metrics { id: 73 }).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(event) = platform.try_recv() {
                match event {
                    Event::Metrics {
                        id,
                        private_bytes,
                        working_set_bytes,
                        handles,
                        cpu_seconds,
                    } => {
                        assert_eq!(id, 73);
                        assert!(private_bytes > 0 && working_set_bytes > 0 && handles > 0);
                        assert!(cpu_seconds.is_finite() && cpu_seconds >= before.cpu_seconds);
                        break;
                    }
                    other => panic!("Unexpected metrics response: {other:?}"),
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "metrics worker did not respond"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn resolver_reports_file_folder_and_missing_path_without_ui_io() {
        let fixture = TestDirectory::new("resolver");
        let file = fixture.0.join("file.txt");
        fs::write(&file, b"resolved").unwrap();
        assert!(resolve_path(&fixture.0).unwrap().1);
        assert!(!resolve_path(&file).unwrap().1);
        assert!(resolve_path(&fixture.0.join("missing")).is_err());
        let platform = Platform::new(Arc::new(|| {})).unwrap();
        platform
            .try_submit(Job::Resolve {
                path: file.clone(),
                id: 42,
            })
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Some(event) = platform.try_recv() {
                match event {
                    Event::Resolved { id, path, is_dir } => {
                        assert_eq!(id, 42);
                        assert_eq!(path, file);
                        assert!(!is_dir);
                        break;
                    }
                    other => panic!("Unexpected resolution: {other:?}"),
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "resolver did not respond"
            );
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[cfg(windows)]
    #[test]
    fn file_clipboard_encodes_unicode_and_double_null_without_changing_system_clipboard() {
        use std::os::windows::ffi::OsStrExt;
        let paths = [
            PathBuf::from("C:\\example\\résumé.txt"),
            PathBuf::from("D:\\my folder\\two.bin"),
        ];
        let bytes = native::clipboard_encoding_for_test(&paths).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 20);
        assert_eq!(i32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
        let encoded: Vec<u16> = bytes[20..]
            .chunks_exact(2)
            .map(|value| u16::from_le_bytes([value[0], value[1]]))
            .collect();
        let mut expected = Vec::new();
        for path in &paths {
            expected.extend(path.as_os_str().encode_wide());
            expected.push(0);
        }
        expected.push(0);
        assert_eq!(encoded, expected);
    }
}
