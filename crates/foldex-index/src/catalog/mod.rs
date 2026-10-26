//! Bounded persistent filename catalog used by the app's recursive-search worker.
mod bitmap;
mod candidates;
pub use candidates::CandidateBatch;
mod cleanup;
mod format;
mod index;
mod manifest;
mod masks;
mod reconcile;
pub use reconcile::{ReconcileMetrics, Reconciled};
mod rows;
mod spool;
pub use index::{NativeIndex, NativeQueryTiming};
mod builder;
pub use builder::NativeIndexBuilder;
mod search;
use crate::{Cancellation, Error, IndexRecord, IndexRow, Result, codec};
pub use search::{CatalogQuery, Hit, SearchResult};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const SHARD_ROWS: usize = 4096;
const SHARD_BYTES: usize = 1024 * 1024;

pub struct Catalog {
    root: PathBuf,
    shards: Vec<format::Shard>,
    pending: format::Buffer,
    entries: u64,
    bytes: u64,
    lower_scratch: String,
    natural_scratch: Vec<u8>,
    signature_scratch: format::Signature,
    substring_safe: bool,
    deleted: HashMap<usize, Vec<u8>>,
    masked: u64,
}

impl Catalog {
    /// The caller supplies a new, exclusively owned experiment directory.
    pub fn create(root: &Path) -> Result<Self> {
        if !root.is_absolute() {
            return Err(Error::InvalidPath("catalog root must be absolute".into()));
        }
        fs::create_dir(root)?;
        Ok(Self {
            root: root.to_owned(),
            shards: Vec::new(),
            pending: format::Buffer::new(),
            entries: 0,
            bytes: 0,
            lower_scratch: String::new(),
            natural_scratch: Vec::new(),
            signature_scratch: format::Signature::default(),
            substring_safe: false,
            deleted: HashMap::new(),
            masked: 0,
        })
    }

    pub fn append_batch(&mut self, records: &[IndexRecord], cancel: &Cancellation) -> Result<()> {
        for record in records {
            cancel.check()?;
            let relative = codec::relative(&record.relative_path, false)?;
            let raw = relative
                .file_name()
                .ok_or_else(|| Error::InvalidPath("missing catalog basename".into()))?;
            let name = raw.to_string_lossy();
            let parent = codec::encode(relative.parent().unwrap_or(Path::new("")).as_os_str());
            self.prepare_name(&name);
            self.push_named(record.into(), raw.as_encoded_bytes(), &parent)?;
        }
        Ok(())
    }

    /// Filesystem batches share an already validated parent. Avoid constructing,
    /// revalidating and allocating a complete IndexRecord for every child.
    pub fn append_entries(
        &mut self,
        parent: &Path,
        entries: &[foldex_fs::Entry],
        cancel: &Cancellation,
    ) -> Result<()> {
        let parent = codec::relative(parent, true)?;
        let parent = codec::encode(parent.as_os_str());
        for entry in entries {
            cancel.check()?;
            let mut components = Path::new(&entry.name).components();
            if !matches!(components.next(), Some(std::path::Component::Normal(name)) if name == entry.name.as_os_str())
                || components.next().is_some()
                || entry.name.as_encoded_bytes().contains(&0)
            {
                return Err(Error::InvalidPath("invalid filesystem basename".into()));
            }
            self.prepare_name(&entry.name.to_string_lossy());
            self.push_named(
                format::Metadata {
                    availability: entry.capabilities().availability(),
                    size: if entry.is_dir() { 0 } else { entry.size },
                    modified_secs: entry.modified_secs,
                    created_secs: entry.created_secs,
                    accessed_secs: entry.accessed_secs,
                    is_dir: entry.is_dir(),
                    hidden: entry.is_hidden(),
                    symlink: entry.is_opaque(),
                },
                entry.name.as_encoded_bytes(),
                &parent,
            )?;
        }
        Ok(())
    }

    fn prepare_name(&mut self, name: &str) {
        self.signature_scratch = format::Signature::of(name);
        if name.is_ascii() {
            self.lower_scratch.clear();
            self.lower_scratch.push_str(name);
            self.lower_scratch.make_ascii_lowercase();
        } else {
            // Preserve Rust's contextual Unicode lowercase (including final sigma).
            self.lower_scratch = name.to_lowercase();
        }
        codec::natural_key_into(&self.lower_scratch, &mut self.natural_scratch);
        self.substring_safe = self
            .signature_scratch
            .supports_ascii_substring(name, &self.lower_scratch);
    }

    fn push_named(&mut self, metadata: format::Metadata, raw: &[u8], parent: &[u8]) -> Result<()> {
        if parent.len().saturating_add(raw.len()).saturating_add(1) > 128 * 1024 {
            return Err(Error::InvalidPath("catalog path too long".into()));
        }
        let added = format::RECORD_BYTES
            + raw.len()
            + self.lower_scratch.len()
            + self.natural_scratch.len()
            + parent.len();
        if self.pending.count > 0
            && (self.pending.count >= SHARD_ROWS
                || self.pending.bytes().saturating_add(added) > SHARD_BYTES)
        {
            self.checkpoint()?;
        }
        self.pending.push(
            metadata,
            raw,
            self.lower_scratch.as_bytes(),
            &self.natural_scratch,
            parent,
            self.signature_scratch,
            self.substring_safe,
        )?;
        self.entries += 1;
        Ok(())
    }

    /// Close all writers before publishing a shard that readers may map.
    pub fn checkpoint(&mut self) -> Result<()> {
        if self.pending.count == 0 {
            return Ok(());
        }
        self.pending.prepare_filter();
        let first_id = self.entries - self.pending.count as u64 + 1;
        let path = self
            .root
            .join(format!("shard-{:08}.bin", self.shards.len()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let mut file = options.open(&path)?;
        let header = self.pending.header(first_id);
        let mut checksum = crc32fast::Hasher::new();
        checksum.update(&header);
        file.write_all(&header)?;
        for part in self.pending.parts() {
            checksum.update(part);
            file.write_all(part)?;
        }
        file.sync_all()?;
        let length = file.metadata()?.len();
        drop(file);
        // No process-lifetime mapping or file handle per shard is retained.
        self.shards.push(format::Shard {
            path,
            first_id,
            count: self.pending.count,
            bytes: length,
            checksum: checksum.finalize(),
            verified: std::sync::atomic::AtomicBool::new(true),
        });
        self.bytes += length;
        self.pending.clear();
        Ok(())
    }

    pub fn entries(&self) -> u64 {
        self.entries
    }
    pub(super) fn ingestion_buffer_bytes(&self) -> usize {
        self.pending.retained_bytes()
            + self.lower_scratch.capacity()
            + self.natural_scratch.capacity()
    }
    pub(super) fn release_ingestion_buffers(&mut self) {
        debug_assert_eq!(self.pending.count, 0);
        self.pending = format::Buffer::new();
        self.lower_scratch = String::new();
        self.natural_scratch = Vec::new();
    }
    pub fn published_entries(&self) -> u64 {
        self.shards
            .last()
            .map_or(0, |shard| shard.first_id + shard.count as u64 - 1)
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn shards(&self) -> usize {
        self.shards.len()
    }

    /// Shards are contiguous and ordered by immutable row ID. Restrict each
    /// append-only query to its interval before choosing workers or scanning.
    pub(super) fn shard_range(&self, lower: u64, upper: u64) -> std::ops::Range<usize> {
        let end = self.shards.partition_point(|shard| shard.first_id <= upper);
        if lower >= upper {
            return end..end;
        }
        let begin = self.shards[..end]
            .partition_point(|shard| shard.first_id + shard.count as u64 - 1 <= lower);
        begin..end
    }

    pub fn relative_path(&self, hit: &Hit) -> Result<PathBuf> {
        if self.masked(hit.shard, hit.row) {
            return Err(Error::InvalidCursor);
        }
        let shard = self.shards.get(hit.shard).ok_or(Error::InvalidCursor)?;
        let view = format::View::open(shard)?;
        let record = view.record(hit.row)?;
        let relative =
            PathBuf::from(codec::decode(record.path())?).join(codec::decode(record.name())?);
        codec::relative(&relative, false)
    }

    pub fn relative_paths(&self, hits: &[Hit]) -> Result<Vec<PathBuf>> {
        use std::collections::BTreeMap;
        let mut groups = BTreeMap::<usize, Vec<(usize, usize)>>::new();
        for (position, hit) in hits.iter().enumerate() {
            if self.masked(hit.shard, hit.row) {
                return Err(Error::InvalidCursor);
            }
            groups
                .entry(hit.shard)
                .or_default()
                .push((position, hit.row));
        }
        let mut output = vec![PathBuf::new(); hits.len()];
        for (shard, rows) in groups {
            let view = format::View::open(self.shards.get(shard).ok_or(Error::InvalidCursor)?)?;
            for (position, row) in rows {
                let record = view.record(row)?;
                let relative = PathBuf::from(codec::decode(record.path())?)
                    .join(codec::decode(record.name())?);
                output[position] = codec::relative(&relative, false)?;
            }
        }
        Ok(output)
    }

    /// Decode each needed shard once, then restore result order. A monotonically
    /// shrinking prefix keeps retained decoded rows within the page byte budget.
    pub fn rows(
        &self,
        hits: &[Hit],
        scope: &Path,
        byte_budget: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<IndexRow>> {
        self.rows_with(
            hits.len(),
            |position| &hits[position],
            scope,
            byte_budget,
            cancel,
        )
    }

    /// Decode cache misses by their positions without cloning owned Hit keys.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn rows_at(
        &self,
        hits: &[Hit],
        positions: &[usize],
        scope: &Path,
        byte_budget: usize,
        cancel: &Cancellation,
        ordered: bool,
    ) -> Result<Vec<IndexRow>> {
        let count = positions.len().min(crate::model::MAX_PAGE_ROWS);
        if positions[..count]
            .iter()
            .any(|position| *position >= hits.len())
        {
            return Err(Error::InvalidCursor);
        }
        let hit_at = |position| &hits[positions[position]];
        if ordered {
            self.rows_ordered_with(count, hit_at, scope, byte_budget, cancel)
        } else {
            self.rows_with(count, hit_at, scope, byte_budget, cancel)
        }
    }

    fn rows_with<'a>(
        &self,
        count: usize,
        hit_at: impl Fn(usize) -> &'a Hit,
        scope: &Path,
        byte_budget: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<IndexRow>> {
        let byte_budget = byte_budget.min(crate::model::MAX_PAGE_BYTES);
        let count = count.min(crate::model::MAX_PAGE_ROWS);
        let mut order = (0..count).collect::<Vec<_>>();
        order.sort_unstable_by_key(|index| hit_at(*index).shard);
        let mut decoded: Vec<Option<IndexRow>> = (0..count).map(|_| None).collect();
        let mut cutoff = count;
        let mut active_shard = usize::MAX;
        let mut view = None;
        let mut used = 0usize;
        for position in order {
            cancel.check()?;
            if position >= cutoff {
                continue;
            }
            let hit = hit_at(position);
            if self.masked(hit.shard, hit.row) {
                return Err(Error::InvalidCursor);
            }
            let shard = self.shards.get(hit.shard).ok_or(Error::InvalidCursor)?;
            if hit.id != shard.first_id.saturating_add(hit.row as u64) {
                return Err(Error::InvalidCursor);
            }
            if active_shard != hit.shard {
                drop(view.take());
                view = Some(format::View::open(shard)?);
                active_shard = hit.shard;
            }
            let record = view.as_ref().unwrap().record(hit.row)?;
            let row = Self::decode_row(&record, hit, scope)?;
            let bytes = row.bytes();
            if bytes > crate::model::MAX_PAGE_BYTES {
                return Err(Error::InvalidIndex(
                    "one catalog result exceeds page byte budget".into(),
                ));
            }
            used += bytes;
            decoded[position] = Some(row);
            while used > byte_budget {
                // Earlier rows have priority even when their shard is decoded
                // later. Removing only the greatest retained position proves
                // that the final prefix equals decoding in display order.
                let Some(last) = (1..cutoff).rev().find(|index| decoded[*index].is_some()) else {
                    break;
                };
                used -= decoded[last].take().unwrap().bytes();
                cutoff = last;
            }
        }
        decoded
            .into_iter()
            .take(cutoff)
            .map(|row| {
                row.ok_or_else(|| {
                    Error::InvalidIndex("grouped catalog page lost its prefix".into())
                })
            })
            .collect()
    }

    /// Historical eight-view decoder retained for controlled A/B measurements.
    fn rows_ordered_with<'a>(
        &self,
        count: usize,
        hit_at: impl Fn(usize) -> &'a Hit,
        scope: &Path,
        byte_budget: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<IndexRow>> {
        let byte_budget = byte_budget.min(crate::model::MAX_PAGE_BYTES);
        let mut views: Vec<(usize, usize, format::View)> = Vec::with_capacity(8);
        let count = count.min(crate::model::MAX_PAGE_ROWS);
        let mut output = Vec::with_capacity(count);
        let mut used = 0usize;
        for age in 0..count {
            let hit = hit_at(age);
            cancel.check()?;
            let shard = self.shards.get(hit.shard).ok_or(Error::InvalidCursor)?;
            if hit.id != shard.first_id.saturating_add(hit.row as u64) {
                return Err(Error::InvalidCursor);
            }
            let slot =
                if let Some(slot) = views.iter().position(|(index, _, _)| *index == hit.shard) {
                    slot
                } else {
                    if views.len() == 8 {
                        let oldest = views
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, (_, age, _))| *age)
                            .unwrap()
                            .0;
                        views.swap_remove(oldest);
                    }
                    views.push((hit.shard, age, format::View::open(shard)?));
                    views.len() - 1
                };
            views[slot].1 = age;
            let record = views[slot].2.record(hit.row)?;
            let row = Self::decode_row(&record, hit, scope)?;
            let bytes = row.bytes();
            if bytes > crate::model::MAX_PAGE_BYTES {
                return Err(Error::InvalidIndex(
                    "one catalog result exceeds page byte budget".into(),
                ));
            }
            if !output.is_empty() && used.saturating_add(bytes) > byte_budget {
                break;
            }
            used += bytes;
            output.push(row);
        }
        Ok(output)
    }

    fn decode_row(record: &format::Record<'_>, hit: &Hit, scope: &Path) -> Result<IndexRow> {
        let raw_name = codec::decode(record.name())?;
        let relative = PathBuf::from(codec::decode(record.path())?).join(&raw_name);
        let relative_path = codec::relative(&relative, false)?;
        let name = raw_name.to_string_lossy().into_owned();
        let lower = std::str::from_utf8(record.lower())
            .map_err(|_| Error::InvalidIndex("catalog lowercase is not UTF-8".into()))?;
        let is_dir = record.flags & 1 != 0;
        Ok(IndexRow {
            availability: foldex_fs::capabilities::Availability::from_code(
                ((record.flags >> 4) & 7) as u8,
            )
            .ok_or_else(|| Error::InvalidIndex("invalid availability".into()))?,
            id: hit.id,
            path: scope.join(&relative_path),
            relative_path,
            raw_name,
            name,
            is_dir,
            hidden: record.flags & 2 != 0,
            symlink: record.flags & 4 != 0,
            size: record.size,
            modified_secs: record.modified,
            created_secs: record.created,
            accessed_secs: record.accessed,
            natural_key: record.natural().to_vec(),
            raw_sort: record.name().to_vec(),
            extension: crate::model::extension(lower, is_dir).to_owned(),
            fuzzy_score: hit.score,
        })
    }
}
