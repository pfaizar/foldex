//! Bounded deep-seek workers. Each owns at most one generation reader/cache.
use super::*;
use foldex_index::{PageCursor, QueryPage};
use std::sync::mpsc::{self, SyncSender, TrySendError};

pub(super) struct Page {
    pub rows: Vec<IndexRow>,
    pub cursors: Vec<PageCursor>,
    pub total: Option<u64>,
    pub kind_counts: Option<(u64, u64)>,
    pub has_more: bool,
    pub status: IndexStatus,
}
/// The nearest retained cursor is at most one bounded window behind the target.
pub(super) fn collect(
    start: usize,
    mut cursor: Option<PageCursor>,
    mut query: impl FnMut(u64, Option<&PageCursor>, usize) -> foldex_index::Result<QueryPage>,
) -> foldex_index::Result<Page> {
    while let Some(point) = &cursor {
        let offset = point.next_offset();
        if offset >= start as u64 {
            break;
        }
        let skip = query(
            offset,
            Some(point),
            (start as u64 - offset).min(PAGE_ROWS as u64) as usize,
        )?;
        if !skip.has_more {
            return Ok(Page {
                rows: Vec::new(),
                cursors: Vec::new(),
                total: skip.total,
                kind_counts: skip.kind_counts,
                has_more: false,
                status: skip.status,
            });
        }
        let next = skip.next.ok_or_else(|| {
            foldex_index::Error::InvalidIndex("nonfinal seek page has no cursor".into())
        })?;
        if next.next_offset() <= offset {
            return Err(foldex_index::Error::InvalidCursor);
        }
        cursor = Some(next);
    }
    let first = query(start as u64, cursor.as_ref(), PAGE_ROWS)?;
    let mut page = Page {
        rows: first.rows,
        cursors: Vec::with_capacity(WINDOW_PAGES),
        total: first.total,
        kind_counts: first.kind_counts,
        has_more: first.has_more,
        status: first.status,
    };
    cursor = first.next;
    if let Some(point) = &cursor {
        page.cursors.push(point.clone());
    }
    while page.has_more && page.rows.len() < WINDOW_ROWS {
        let point = cursor.as_ref().ok_or_else(|| {
            foldex_index::Error::InvalidIndex("nonfinal result page has no cursor".into())
        })?;
        let next = query(
            point.next_offset(),
            Some(point),
            PAGE_ROWS.min(WINDOW_ROWS - page.rows.len()),
        )?;
        if !same_result_prefix(&page.status, &next.status) {
            return Err(foldex_index::Error::InvalidCursor);
        }
        if next.rows.is_empty() {
            return Err(foldex_index::Error::InvalidIndex(
                "nonfinal result page made no progress".into(),
            ));
        }
        page.rows.extend(next.rows);
        page.has_more = next.has_more;
        cursor = next.next;
        if let Some(point) = &cursor {
            page.cursors.push(point.clone());
            if page.cursors.len() > WINDOW_PAGES {
                page.cursors.remove(0);
            }
        }
    }
    Ok(page)
}

type ResultSlot = Arc<Mutex<Option<foldex_index::Result<Page>>>>;
struct Job {
    source: IndexStatus,
    spec: QuerySpec,
    start: usize,
    cursor: Option<PageCursor>,
    kind_counts: Option<(u64, u64)>,
    cancel: Cancellation,
    result: ResultSlot,
}
pub(super) struct Work {
    cancel: Cancellation,
    result: ResultSlot,
}
impl Work {
    pub fn take(&self) -> Option<foldex_index::Result<Page>> {
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
        Self::with_workers(shared, 2)
    }
    fn with_workers(shared: Arc<Shared>, workers: usize) -> io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(4);
        let receiver = Arc::new(Mutex::new(receiver));
        let stop = Arc::new(AtomicBool::new(false));
        let mut pool = Self {
            sender: Some(sender),
            stop,
            threads: Vec::with_capacity(2),
        };
        for number in 0..workers.clamp(1, 2) {
            let receiver = receiver.clone();
            let stop = pool.stop.clone();
            let shared = shared.clone();
            pool.threads.push(
                thread::Builder::new()
                    .name(format!("foldex-seek-{number}"))
                    .spawn(move || {
                        #[cfg(windows)]
                        unsafe {
                            use windows_sys::Win32::System::Threading::{
                                GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
                            };
                            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                        }
                        let mut reader: Option<NativeIndex> = None;
                        loop {
                            let job = { lock(&receiver).recv() };
                            let Ok(job) = job else { break };
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            if job.cancel.is_cancelled() {
                                shared.work_ready();
                                continue;
                            }
                            let result = (|| {
                                let reopen = reader.as_ref().is_none_or(|index| {
                                    index.status().database_path != job.source.database_path
                                        || index.status().entries < job.source.entries
                                        || index.status().revision < job.source.revision
                                });
                                if reopen {
                                    reader = Some(NativeIndex::open_database(
                                        &job.source.database_path,
                                    )?);
                                }
                                let index = reader.as_mut().unwrap();
                                let kind_counts = if job.spec.kind != KindFilter::All
                                    && job.kind_counts.is_none()
                                {
                                    let mut counts = job.spec.clone();
                                    counts.kind = KindFilter::All;
                                    counts.include_total = true;
                                    index
                                        .query_as_of(&counts, &job.source, 0, 0, &job.cancel)?
                                        .kind_counts
                                } else {
                                    job.kind_counts
                                };
                                let mut page =
                                    collect(job.start, job.cursor, |offset, cursor, size| {
                                        if cursor.is_some() {
                                            index.query_after_as_of(
                                                &job.spec,
                                                &job.source,
                                                cursor,
                                                size,
                                                &job.cancel,
                                            )
                                        } else {
                                            index.query_as_of(
                                                &job.spec,
                                                &job.source,
                                                offset,
                                                size,
                                                &job.cancel,
                                            )
                                        }
                                    })?;
                                if job.spec.kind != KindFilter::All {
                                    page.kind_counts = kind_counts;
                                }
                                Ok(page)
                            })();
                            if !job.cancel.is_cancelled() {
                                *lock(&job.result) = Some(result);
                            }
                            shared.work_ready();
                        }
                    })?,
            );
        }
        Ok(pool)
    }
    /// Full queues retain no additional row payload or pending unbounded work.
    pub fn submit(
        &self,
        source: IndexStatus,
        spec: QuerySpec,
        start: usize,
        cursor: Option<PageCursor>,
        kind_counts: Option<(u64, u64)>,
        parent: &Cancellation,
    ) -> foldex_index::Result<Option<Work>> {
        let cancel = Cancellation::new().linked(parent);
        let result = Arc::new(Mutex::new(None));
        match self.sender.as_ref().unwrap().try_send(Job {
            source,
            spec,
            start,
            cursor,
            kind_counts,
            cancel: cancel.clone(),
            result: result.clone(),
        }) {
            Ok(()) => Ok(Some(Work { cancel, result })),
            Err(TrySendError::Full(_)) => Ok(None),
            Err(TrySendError::Disconnected(_)) => {
                Err(io::Error::other("Deep-seek workers are unavailable").into())
            }
        }
    }
}
impl Drop for Pool {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.sender.take();
        // This owner lives on the coordinator, never the frame thread. Clients
        // cancel their tokens before coordinator shutdown and final lease cleanup.
        for worker in self.threads.drain(..) {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
#[path = "paging_tests.rs"]
mod tests;
