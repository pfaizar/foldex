//! Directory-level overlap with bounded paths, output batches and worker stacks.
use crate::{Entry, ReadDir, queue::Queue};
use std::{
    fs, io,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const BATCH_ROWS: usize = 128;
const BATCH_BYTES: usize = 128 * 1024;
const OUTPUT_BATCHES: usize = 4;

#[derive(Clone, Copy, Debug)]
pub enum ReaderKind {
    Standard,
    Native,
    LargeFetch,
}

pub struct WalkOptions {
    pub workers: usize,
    /// An existing private directory. Overflow is deleted when workers stop.
    pub spool_root: PathBuf,
    /// Relative canonical cache ancestor, when it belongs to the scope.
    pub excluded_relative: Option<PathBuf>,
    /// Canonical cache ancestor, used only if the scope could not be canonicalized.
    pub excluded_root: Option<PathBuf>,
}

pub struct Batch {
    pub parent: Arc<PathBuf>,
    pub entries: Vec<Entry>,
    pub skipped: u64,
    pub excluded: u64,
}

pub enum WalkEvent {
    Batch(Batch),
    Pending,
    Finished,
}

struct Shared {
    scope: PathBuf,
    options: WalkOptions,
    reader: ReaderKind,
    queue: Mutex<Queue>,
    ready: Condvar,
    cancel: AtomicBool,
}

pub struct Walk {
    shared: Arc<Shared>,
    receiver: Option<Receiver<io::Result<Batch>>>,
    workers: Vec<JoinHandle<()>>,
}

impl Walk {
    pub fn start(scope: PathBuf, options: WalkOptions) -> io::Result<Self> {
        Self::start_with_reader(scope, options, ReaderKind::Standard)
    }
    pub fn start_with_reader(
        scope: PathBuf,
        mut options: WalkOptions,
        reader: ReaderKind,
    ) -> io::Result<Self> {
        options.workers = options.workers.clamp(1, 16);
        if !scope.is_absolute() || !options.spool_root.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Walk paths must be absolute",
            ));
        }
        fs::create_dir_all(&options.spool_root)?;
        let workers = options.workers;
        let shared = Arc::new(Shared {
            scope,
            options,
            reader,
            queue: Mutex::new(Queue::new()),
            ready: Condvar::new(),
            cancel: AtomicBool::new(false),
        });
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_BATCHES);
        let mut walk = Self {
            shared,
            receiver: Some(receiver),
            workers: Vec::with_capacity(workers),
        };
        for _ in 0..workers {
            let shared = walk.shared.clone();
            let sender = sender.clone();
            walk.workers.push(
                thread::Builder::new()
                    .name("foldex-walk".into())
                    .stack_size(256 * 1024)
                    .spawn(move || worker(shared, sender))?,
            );
        }
        Ok(walk)
    }

    /// The caller waits at most this long for producer work, never for an entire tree.
    pub fn next(&self, timeout: Duration) -> io::Result<WalkEvent> {
        match self
            .receiver
            .as_ref()
            .map(|receiver| receiver.recv_timeout(timeout))
        {
            Some(Ok(Ok(batch))) => Ok(WalkEvent::Batch(batch)),
            Some(Ok(Err(error))) => Err(error),
            Some(Err(RecvTimeoutError::Timeout)) => Ok(WalkEvent::Pending),
            _ => Ok(WalkEvent::Finished),
        }
    }

    pub fn stop(&mut self) {
        self.shared.cancel.store(true, Ordering::Release);
        // Drop the receiver before joining: a backpressured sender must wake on cancellation.
        self.receiver.take();
        self.shared.ready.notify_all();
    }

    /// Worker-only teardown; no detached enumeration pool remains after completion.
    pub fn finish(&mut self) {
        self.stop();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for Walk {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker(shared: Arc<Shared>, sender: SyncSender<io::Result<Batch>>) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        // SAFETY: the pseudo-handle identifies only this newly created worker.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
        }
    }
    loop {
        let job = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if queue.done || shared.cancel.load(Ordering::Acquire) {
                    break None;
                }
                match queue.pop() {
                    Ok(Some(path)) => {
                        queue.active += 1;
                        break Some(path);
                    }
                    Ok(None) if queue.active == 0 => {
                        queue.done = true;
                        shared.ready.notify_all();
                        break None;
                    }
                    Ok(None) => {
                        queue = shared.ready.wait(queue).unwrap();
                    }
                    Err(error) => {
                        shared.cancel.store(true, Ordering::Release);
                        shared.ready.notify_all();
                        drop(queue);
                        let _ = sender.send(Err(error));
                        return;
                    }
                }
            }
        };
        let Some(parent) = job else {
            break;
        };
        let result = read_directory(&shared, &sender, Arc::new(parent));
        let mut queue = shared.queue.lock().unwrap();
        queue.active -= 1;
        if queue.active == 0 && queue.empty() {
            queue.done = true;
        }
        if result.is_err() {
            shared.cancel.store(true, Ordering::Release);
        }
        shared.ready.notify_all();
        drop(queue);
        if let Err(error) = result {
            let _ = sender.send(Err(error));
            break;
        }
    }
}

fn read_directory(
    shared: &Shared,
    sender: &SyncSender<io::Result<Batch>>,
    parent: Arc<PathBuf>,
) -> io::Result<()> {
    let absolute = shared.scope.join(parent.as_ref());
    let mut batch = Batch {
        parent,
        entries: Vec::with_capacity(BATCH_ROWS),
        skipped: 0,
        excluded: 0,
    };
    let excluded = shared
        .options
        .excluded_relative
        .as_ref()
        .is_some_and(|path| batch.parent.starts_with(path))
        || shared.options.excluded_root.as_ref().is_some_and(|root| {
            fs::canonicalize(&absolute)
                .ok()
                .is_some_and(|path| path.starts_with(root))
        });
    if excluded {
        batch.excluded = 1;
    } else if let Ok(reader) = match shared.reader {
        ReaderKind::Standard => ReadDir::standard(&absolute),
        ReaderKind::Native => ReadDir::open_no_follow(&absolute),
        ReaderKind::LargeFetch => ReadDir::large_fetch(&absolute),
    } {
        let mut bytes = BATCH_ROWS * std::mem::size_of::<Entry>();
        for entry in reader {
            if shared.cancel.load(Ordering::Acquire) {
                return Ok(());
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    batch.skipped += 1;
                    continue;
                }
            };
            if !entry.metadata_known {
                batch.skipped += 1;
            }
            if entry.is_dir() && !entry.is_reparse() {
                let child = batch.parent.join(&entry.name);
                shared
                    .queue
                    .lock()
                    .unwrap()
                    .push(child, &shared.options.spool_root)?;
                shared.ready.notify_one();
            }
            let payload = entry.name.capacity();
            if !batch.entries.is_empty()
                && (batch.entries.len() >= BATCH_ROWS
                    || bytes.saturating_add(payload) > BATCH_BYTES)
            {
                let parent = batch.parent.clone();
                sender.send(Ok(batch)).map_err(|_| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "Walk consumer closed")
                })?;
                batch = Batch {
                    parent,
                    entries: Vec::with_capacity(BATCH_ROWS),
                    skipped: 0,
                    excluded: 0,
                };
                bytes = BATCH_ROWS * std::mem::size_of::<Entry>();
            }
            bytes += payload;
            batch.entries.push(entry);
        }
    } else {
        batch.skipped = 1;
    }
    if !batch.entries.is_empty() || batch.skipped > 0 || batch.excluded > 0 {
        sender
            .send(Ok(batch))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Walk consumer closed"))?;
    }
    Ok(())
}
