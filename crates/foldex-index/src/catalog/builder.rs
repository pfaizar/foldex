use super::{NativeIndex, manifest::Manifest};
use crate::{model::*, store::Mode};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

struct Pending {
    parent: Arc<PathBuf>,
    entries: Vec<foldex_fs::Entry>,
    next: usize,
}

pub struct NativeIndexBuilder {
    pub(super) index: Option<NativeIndex>,
    mode: Mode,
    walker: Option<foldex_fs::Walk>,
    pending: Option<Pending>,
    excluded_relative: Option<PathBuf>,
    excluded_fallback: Option<PathBuf>,
    done: bool,
    started: Instant,
    last_commit: Instant,
    committed_entries: u64,
    workers: usize,
    directory_only: bool,
}

impl NativeIndexBuilder {
    pub fn create(cache_dir: &Path, scope: &Path, cancel: &Cancellation) -> Result<Self> {
        Self::create_with_options(cache_dir, scope, &BuildOptions::default(), cancel)
    }
    pub fn create_with_options(
        cache_dir: &Path,
        scope: &Path,
        options: &BuildOptions,
        cancel: &Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        fs::create_dir_all(cache_dir)?;
        let cache_dir = fs::canonicalize(cache_dir)?;
        let excluded_root =
            fs::canonicalize(options.excluded_root.as_deref().unwrap_or(&cache_dir))?;
        let canonical_scope = fs::canonicalize(scope).ok();
        let excluded_relative = canonical_scope.as_ref().and_then(|scope| {
            excluded_root
                .strip_prefix(scope)
                .ok()
                .map(Path::to_path_buf)
        });
        let excluded_fallback = canonical_scope.is_none().then_some(excluded_root);
        let (manifest, data) = Manifest::create(
            &cache_dir,
            scope,
            options.directory_only,
            &options.excluded_subtrees,
            cancel,
        )?;
        Ok(Self {
            index: Some(NativeIndex::new(manifest, data)),
            mode: Mode::None,
            walker: None,
            pending: None,
            excluded_relative,
            excluded_fallback,
            done: false,
            started: Instant::now(),
            last_commit: Instant::now(),
            committed_entries: 0,
            directory_only: options.directory_only,
            workers: if options.directory_only {
                1
            } else {
                std::thread::available_parallelism().map_or(1, |count| count.get().min(8))
            },
        })
    }
    pub fn status(&self) -> &IndexStatus {
        self.index.as_ref().unwrap().status()
    }
    pub fn ingestion_buffer_bytes(&self) -> usize {
        self.index.as_ref().unwrap().ingestion_buffer_bytes()
    }
    pub fn retained_query_bytes(&self) -> usize {
        self.index.as_ref().unwrap().retained_query_bytes()
    }
    pub fn cached_window(&self, spec: &QuerySpec, prefix: u64, offset: usize, size: usize) -> bool {
        self.index
            .as_ref()
            .unwrap()
            .cached_window(spec, prefix, offset, size)
    }
    pub fn release_query_cache(&mut self) {
        self.index.as_mut().unwrap().release_query_cache();
    }
    pub fn set_scan_workers(&mut self, workers: usize) -> Result<()> {
        if self.mode != Mode::None {
            return Err(Error::WrongMode);
        }
        self.workers = workers.clamp(1, 16);
        Ok(())
    }

    pub fn ingest_batch(&mut self, records: &[IndexRecord], cancel: &Cancellation) -> Result<()> {
        if self.mode == Mode::Filesystem {
            return Err(Error::WrongMode);
        }
        if records.len() > 512
            || records.iter().map(IndexRecord::heap_bytes).sum::<usize>() > MAX_PAGE_BYTES
        {
            return Err(Error::InvalidPath(
                "native ingestion batch exceeds 512 records or 1MiB".into(),
            ));
        }
        self.mode = Mode::Manual;
        let index = self.index.as_mut().unwrap();
        index.manifest.status.synthetic = true;
        let before = index.data.entries();
        let result = index.data.append_batch(records, cancel);
        let added = (index.data.entries() - before) as usize;
        let status = &mut index.manifest.status;
        status.entries = index.data.entries();
        for record in &records[..added] {
            status.folders += u64::from(record.is_dir);
            status.files += u64::from(!record.is_dir);
            status.excluded_links += u64::from(record.symlink);
        }
        result
    }

    pub fn checkpoint(&mut self) -> Result<BuildProgress> {
        let index = self.index.as_mut().unwrap();
        index.manifest.commit(&mut index.data)?;
        self.committed_entries = index.status().entries;
        self.last_commit = Instant::now();
        Ok(BuildProgress {
            status: self.status().clone(),
            done: self.done,
            elapsed: self.started.elapsed(),
        })
    }

    fn stop_walk(&mut self) {
        self.pending = None;
        if let Some(mut walker) = self.walker.take() {
            walker.finish();
        }
    }

    pub fn scan_step(
        &mut self,
        max_records: usize,
        cancel: &Cancellation,
        foreground_yield: &dyn Fn(),
    ) -> Result<BuildProgress> {
        if self.mode == Mode::Manual {
            return Err(Error::WrongMode);
        }
        if self.done {
            return self.checkpoint();
        }
        if self.mode == Mode::None && !cancel.is_cancelled() {
            self.mode = Mode::Filesystem;
            let index = self.index.as_ref().unwrap();
            self.walker = Some(foldex_fs::Walk::start(
                index.status().scope.clone(),
                foldex_fs::WalkOptions {
                    excluded_subtrees: index.status().excluded_subtrees.clone(),
                    recursive: !self.directory_only,
                    workers: self.workers,
                    spool_root: index.data.root.join("queues"),
                    excluded_relative: self.excluded_relative.clone(),
                    excluded_root: self.excluded_fallback.clone(),
                },
            )?);
        }
        let mut remaining = max_records.clamp(1, 512);
        while remaining > 0 {
            if cancel.is_cancelled() {
                self.stop_walk();
                self.done = true;
                self.index.as_mut().unwrap().manifest.status.state = BuildState::Cancelled;
                break;
            }
            foreground_yield();
            if self.pending.is_none() {
                match self
                    .walker
                    .as_ref()
                    .unwrap()
                    .next(Duration::from_millis(4))?
                {
                    foldex_fs::WalkEvent::Pending => break,
                    foldex_fs::WalkEvent::Finished => {
                        self.done = true;
                        self.stop_walk();
                        break;
                    }
                    foldex_fs::WalkEvent::Batch(batch) => {
                        let status = &mut self.index.as_mut().unwrap().manifest.status;
                        status.skipped += batch.skipped;
                        status.excluded_cache += batch.excluded;
                        self.pending = Some(Pending {
                            parent: batch.parent,
                            entries: batch.entries,
                            next: 0,
                        });
                    }
                }
            }
            let mut pending = self.pending.take().unwrap();
            let count = remaining.min(pending.entries.len() - pending.next);
            if count == 0 {
                remaining = remaining.saturating_sub(1);
                continue;
            }
            let entries = &pending.entries[pending.next..pending.next + count];
            let index = self.index.as_mut().unwrap();
            let before = index.data.entries();
            let result = index
                .data
                .append_entries(pending.parent.as_ref(), entries, cancel);
            let added = (index.data.entries() - before) as usize;
            let status = &mut index.manifest.status;
            status.entries = index.data.entries();
            for entry in &entries[..added] {
                status.folders += u64::from(entry.is_dir());
                status.files += u64::from(!entry.is_dir());
                status.excluded_links += u64::from(entry.is_reparse());
            }
            pending.next += added;
            remaining = remaining.saturating_sub(added);
            if pending.next < pending.entries.len() {
                self.pending = Some(pending);
            }
            match result {
                Ok(()) => {}
                Err(Error::Cancelled) => {
                    self.stop_walk();
                    self.done = true;
                    self.index.as_mut().unwrap().manifest.status.state = BuildState::Cancelled;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if self.done && self.status().state != BuildState::Cancelled {
            let status = &mut self.index.as_mut().unwrap().manifest.status;
            status.complete = status.skipped == 0;
            status.state = if status.complete {
                BuildState::Complete
            } else {
                BuildState::Partial
            };
        }
        if self.done
            || self.last_commit.elapsed() >= Duration::from_millis(100)
            || self.status().entries.saturating_sub(self.committed_entries) >= 8192
        {
            return self.checkpoint();
        }
        Ok(BuildProgress {
            status: self.status().clone(),
            done: false,
            elapsed: self.started.elapsed(),
        })
    }

    pub fn query(
        &mut self,
        spec: &QuerySpec,
        offset: u64,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.checkpoint()?;
        self.index
            .as_mut()
            .unwrap()
            .query(spec, offset, size, cancel)
    }
    pub fn query_after(
        &mut self,
        spec: &QuerySpec,
        cursor: Option<&PageCursor>,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.checkpoint()?;
        self.index
            .as_mut()
            .unwrap()
            .query_after(spec, cursor, size, cancel)
    }
    pub fn query_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        offset: u64,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.checkpoint()?;
        self.index
            .as_mut()
            .unwrap()
            .query_as_of(spec, boundary, offset, size, cancel)
    }
    pub fn query_after_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        cursor: Option<&PageCursor>,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.checkpoint()?;
        self.index
            .as_mut()
            .unwrap()
            .query_after_as_of(spec, boundary, cursor, size, cancel)
    }
    pub fn metrics(&self) -> Result<CacheMetrics> {
        self.index.as_ref().unwrap().metrics()
    }

    pub fn finish(mut self, complete: bool) -> Result<NativeIndex> {
        self.stop_walk();
        let status = &mut self.index.as_mut().unwrap().manifest.status;
        if status.state != BuildState::Cancelled {
            status.complete =
                complete && (self.mode != Mode::Filesystem || self.done) && status.skipped == 0;
            status.state = if status.complete {
                BuildState::Complete
            } else {
                BuildState::Partial
            };
        }
        self.checkpoint()?;
        let mut index = self.index.take().unwrap();
        index.data.release_ingestion_buffers();
        if index.status().state != BuildState::Cancelled {
            index.manifest.publish_current()?;
        }
        index
            .manifest
            .connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE); PRAGMA shrink_memory;")?;
        Ok(index)
    }
    pub fn finish_cancelled(mut self) -> Result<NativeIndex> {
        self.index.as_mut().unwrap().manifest.status.state = BuildState::Cancelled;
        self.finish(false)
    }
}
impl Drop for NativeIndexBuilder {
    fn drop(&mut self) {
        self.stop_walk();
    }
}
