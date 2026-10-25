//! Incremental metadata work cannot reserve the foreground traversal permits.
use super::*;
use foldex_index::catalog::Reconciled;
use std::sync::mpsc::{self, SyncSender, TrySendError};
type Slot = Arc<Mutex<Option<foldex_index::Result<Reconciled>>>>;
struct Job {
    source: PathBuf,
    paths: Vec<PathBuf>,
    options: BuildOptions,
    cancel: Cancellation,
    result: Slot,
}
pub(super) struct Work {
    cancel: Cancellation,
    result: Slot,
}
impl Work {
    pub fn take(&self) -> Option<foldex_index::Result<Reconciled>> {
        lock(&self.result).take()
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub(super) struct Pool {
    sender: Option<SyncSender<Job>>,
    stop: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
}
impl Pool {
    pub fn new(shared: Arc<Shared>) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(2);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut pool = Self {
            sender: Some(sender),
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::with_capacity(2),
        };
        for number in 0..2 {
            let receiver = receiver.clone();
            let stop = pool.stop.clone();
            let shared = shared.clone();
            pool.threads.push(
                thread::Builder::new()
                    .name(format!("foldex-reconcile-{number}"))
                    .spawn(move || {
                        #[cfg(windows)]
                        unsafe {
                            use windows_sys::Win32::System::Threading::{
                                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
                            };
                            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                        }
                        loop {
                            let job = { lock(&receiver).recv() };
                            let Ok(job) = job else { break };
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            if !job.cancel.is_cancelled() {
                                let result = NativeIndex::reconcile(
                                    &job.source,
                                    &job.paths,
                                    &job.options,
                                    &job.cancel,
                                );
                                if !job.cancel.is_cancelled() {
                                    *lock(&job.result) = Some(result);
                                }
                            }
                            shared.work_ready();
                        }
                    })?,
            );
        }
        Ok(pool)
    }
    pub fn submit(
        &self,
        source: PathBuf,
        paths: Vec<PathBuf>,
        options: BuildOptions,
        parent: &Cancellation,
    ) -> foldex_index::Result<Option<Work>> {
        let cancel = Cancellation::new().linked(parent);
        let result = Arc::new(Mutex::new(None));
        match self.sender.as_ref().unwrap().try_send(Job {
            source,
            paths,
            options,
            cancel: cancel.clone(),
            result: result.clone(),
        }) {
            Ok(()) => Ok(Some(Work { cancel, result })),
            Err(TrySendError::Full(_)) => Ok(None),
            Err(TrySendError::Disconnected(_)) => {
                Err(io::Error::other("Metadata refresh workers are unavailable").into())
            }
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        for worker in self.threads.drain(..) {
            let _ = worker.join();
        }
    }
}
