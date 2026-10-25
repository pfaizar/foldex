use crate::{
    model::*,
    store::{IndexBuilder, Mode},
};
use foldex_fs::{Walk, WalkEvent, WalkOptions};
use std::{path::PathBuf, sync::Arc, time::Duration};

pub(crate) struct CurrentBatch {
    parent: Arc<PathBuf>,
    entries: std::vec::IntoIter<foldex_fs::Entry>,
}

impl IndexBuilder {
    pub(crate) fn scan_parallel_step(
        &mut self,
        max_records: usize,
        cancel: &Cancellation,
        foreground_yield: &dyn Fn(),
    ) -> Result<BuildProgress> {
        if self.mode == Mode::Manual {
            return Err(Error::WrongMode);
        }
        if self.walk_done {
            return self.checkpoint();
        }
        if self.mode == Mode::None {
            self.mode = Mode::Filesystem;
            self.walker = Some(Walk::start(
                self.status().scope.clone(),
                WalkOptions {
                    recursive: !self.directory_only,
                    workers: self.scan_workers,
                    spool_root: self.cache_dir.clone(),
                    excluded_relative: self.excluded_relative.clone(),
                    excluded_root: (!self.canonical_scope_known)
                        .then(|| self.excluded_root.clone()),
                },
            )?);
        }
        let target = max_records.clamp(1, 512);
        let mut batch = Vec::with_capacity(target.min(128));
        let mut bytes = 0usize;
        let mut visited = 0usize;
        while visited < target {
            if cancel.is_cancelled() {
                if !batch.is_empty() {
                    self.store_batch(&batch, &Cancellation::new(), false)?;
                    batch.clear();
                }
                self.walk_batch = None;
                if let Some(mut walker) = self.walker.take() {
                    walker.finish();
                }
                self.index.as_mut().unwrap().status.state = BuildState::Cancelled;
                self.walk_done = true;
                self.ensure_transaction()?;
                break;
            }
            if visited % 64 == 0 {
                foreground_yield();
            }
            if self.walk_batch.is_none() {
                match self
                    .walker
                    .as_ref()
                    .unwrap()
                    .next(Duration::from_millis(4))?
                {
                    WalkEvent::Pending => break,
                    WalkEvent::Finished => {
                        self.walk_done = true;
                        if let Some(mut walker) = self.walker.take() {
                            walker.finish();
                        }
                        let status = &mut self.index.as_mut().unwrap().status;
                        status.complete = status.skipped == 0;
                        status.state = if status.complete {
                            BuildState::Complete
                        } else {
                            BuildState::Partial
                        };
                        self.ensure_transaction()?;
                        break;
                    }
                    WalkEvent::Batch(ready) => {
                        let status = &mut self.index.as_mut().unwrap().status;
                        status.skipped += ready.skipped;
                        status.excluded_cache += ready.excluded;
                        self.walk_batch = Some(CurrentBatch {
                            parent: ready.parent,
                            entries: ready.entries.into_iter(),
                        });
                        visited += 1;
                    }
                }
            }
            let current = self.walk_batch.as_mut().unwrap();
            let Some(entry) = current.entries.next() else {
                self.walk_batch = None;
                continue;
            };
            visited += 1;
            let mut record = IndexRecord::new(
                current.parent.join(&entry.name),
                entry.is_dir(),
                if entry.is_dir() { 0 } else { entry.size },
                entry.modified_secs,
            )?;
            record.hidden = entry.is_hidden();
            record.symlink = entry.is_reparse();
            let added = record.heap_bytes();
            if !batch.is_empty() && bytes.saturating_add(added) > MAX_PAGE_BYTES {
                self.store_batch(&batch, &Cancellation::new(), false)?;
                batch.clear();
                bytes = 0;
            }
            bytes += added;
            batch.push(record);
        }
        if !batch.is_empty() {
            self.store_batch(&batch, &Cancellation::new(), false)?;
        }
        self.progressive_checkpoint()
    }
}
