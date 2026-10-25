use crate::{
    codec,
    model::*,
    store::{IndexBuilder, Mode, unix_secs},
};
use rusqlite::OptionalExtension;
use std::{fs, path::PathBuf};

pub(crate) struct CurrentDirectory {
    id: i64,
    relative: PathBuf,
    entries: fs::ReadDir,
}

impl IndexBuilder {
    /// Advances a bounded number of records, committing at least every 100 ms/8192 records.
    /// Queries and explicit checkpoints always force durability. Pending directories
    /// live in SQLite rather than a depth/fanout-sized memory stack. No reparse point is followed.
    pub fn scan_step(
        &mut self,
        max_records: usize,
        cancel: &Cancellation,
        foreground_yield: &dyn Fn(),
    ) -> Result<BuildProgress> {
        if self.scan_workers > 0 {
            return self.scan_parallel_step(max_records, cancel, foreground_yield);
        }
        if self.mode == Mode::Manual {
            return Err(Error::WrongMode);
        }
        if self.walk_done {
            return self.checkpoint();
        }
        if self.mode == Mode::None {
            self.mode = Mode::Filesystem;
            self.ensure_transaction()?;
            self.index
                .as_ref()
                .unwrap()
                .connection
                .execute("INSERT INTO pending(relative) VALUES(?1)", [&[] as &[u8]])?;
        }
        let target = max_records.clamp(1, 512);
        let mut batch = Vec::with_capacity(target.min(128));
        let mut batch_bytes = 0usize;
        let mut visited = 0usize;
        while visited < target {
            if cancel.is_cancelled() {
                // Commit the bounded records already enumerated, then mark cancellation explicitly.
                if !batch.is_empty() {
                    self.store_batch(&batch, &Cancellation::new(), true)?;
                    batch.clear();
                }
                self.index.as_mut().unwrap().status.state = BuildState::Cancelled;
                self.current = None;
                self.walk_done = true;
                self.ensure_transaction()?;
                break;
            }
            if visited % 64 == 0 {
                foreground_yield();
            }
            if self.current.is_none() {
                // Flush discovered directories before selecting the next queue row.
                if !batch.is_empty() {
                    self.store_batch(&batch, &Cancellation::new(), true)?;
                    batch.clear();
                    batch_bytes = 0;
                }
                self.ensure_transaction()?;
                let next: Option<(i64, Vec<u8>)> = self
                    .index
                    .as_ref()
                    .unwrap()
                    .connection
                    .query_row(
                        "SELECT id,relative FROM pending ORDER BY id LIMIT 1",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((id, relative)) = next else {
                    self.walk_done = true;
                    let status = &mut self.index.as_mut().unwrap().status;
                    status.complete = status.skipped == 0;
                    status.state = if status.complete {
                        BuildState::Complete
                    } else {
                        BuildState::Partial
                    };
                    break;
                };
                let relative = codec::relative(&PathBuf::from(codec::decode(&relative)?), true)?;
                let scope = &self.index.as_ref().unwrap().status.scope;
                let absolute = scope.join(&relative);
                // Resolve the root once. Descendants never follow reparse points, so comparing
                // relative paths avoids a canonicalization syscall for every visited directory.
                if self
                    .excluded_relative
                    .as_ref()
                    .is_some_and(|excluded| relative.starts_with(excluded))
                    || (!self.canonical_scope_known
                        && fs::canonicalize(&absolute)
                            .ok()
                            .is_some_and(|path| path.starts_with(&self.excluded_root)))
                {
                    self.index.as_mut().unwrap().status.excluded_cache += 1;
                    self.index
                        .as_ref()
                        .unwrap()
                        .connection
                        .execute("DELETE FROM pending WHERE id=?1", [id])?;
                    visited += 1;
                    continue;
                }
                foreground_yield();
                visited += 1;
                match fs::read_dir(&absolute) {
                    Ok(entries) => {
                        self.current = Some(CurrentDirectory {
                            id,
                            relative,
                            entries,
                        })
                    }
                    Err(_) => {
                        self.index.as_mut().unwrap().status.skipped += 1;
                        self.index
                            .as_ref()
                            .unwrap()
                            .connection
                            .execute("DELETE FROM pending WHERE id=?1", [id])?;
                        continue;
                    }
                }
            }
            let current = self.current.as_mut().unwrap();
            let entry = match current.entries.next() {
                Some(Ok(entry)) => entry,
                Some(Err(_)) => {
                    self.index.as_mut().unwrap().status.skipped += 1;
                    visited += 1;
                    continue;
                }
                None => {
                    let id = current.id;
                    self.current = None;
                    self.index
                        .as_ref()
                        .unwrap()
                        .connection
                        .execute("DELETE FROM pending WHERE id=?1", [id])?;
                    continue;
                }
            };
            visited += 1;
            let relative = current.relative.join(entry.file_name());
            let metadata = entry.metadata();
            let file_type = entry.file_type().ok();
            let mut record =
                IndexRecord::new(relative, file_type.is_some_and(|kind| kind.is_dir()), 0, 0)?;
            record.symlink = file_type.is_some_and(|kind| kind.is_symlink());
            if let Ok(metadata) = metadata {
                record.size = metadata.len();
                record.modified_secs = metadata.modified().map(unix_secs).unwrap_or(0);
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    let flags = metadata.file_attributes();
                    record.is_dir = flags & 0x10 != 0;
                    record.hidden = flags & 0x2 != 0;
                    record.symlink |= flags & 0x400 != 0;
                }
                if record.is_dir {
                    record.size = 0;
                }
            } else {
                self.index.as_mut().unwrap().status.skipped += 1;
            }
            let bytes = record.heap_bytes();
            if !batch.is_empty() && batch_bytes.saturating_add(bytes) > MAX_PAGE_BYTES {
                self.store_batch(&batch, &Cancellation::new(), true)?;
                batch.clear();
                batch_bytes = 0;
            }
            batch_bytes += bytes;
            batch.push(record);
        }
        if !batch.is_empty() {
            self.store_batch(&batch, &Cancellation::new(), true)?;
        }
        self.progressive_checkpoint()
    }
}
