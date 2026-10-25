use super::{Catalog, CatalogQuery, Hit, manifest::Manifest};
use crate::{model::*, query::fingerprint};
use std::{
    collections::VecDeque,
    fs,
    path::Path,
    time::{Duration, Instant},
};

const HEAD_ROWS: usize = 1025;
const HEAD_BYTES: usize = 2 * 1024 * 1024;
const ARCHIVED_HEAD_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Default, Debug)]
pub struct NativeQueryTiming {
    pub head: Duration,
    pub decode: Duration,
    pub rows: Duration,
    pub cache_hits: usize,
    pub cache_misses: usize,
    pub chunks: usize,
    pub head_reused: bool,
}

struct Head {
    hash: u64,
    prefix: u64,
    keys: Vec<Hit>,
    folders: u64,
    files: u64,
}
impl Head {
    fn bytes(&self) -> usize {
        self.keys.capacity() * std::mem::size_of::<Hit>()
            + self
                .keys
                .iter()
                .map(|key| key.bytes() - std::mem::size_of::<Hit>())
                .sum::<usize>()
    }
    fn complete(&self) -> bool {
        self.folders + self.files == self.keys.len() as u64
    }
}

pub struct NativeIndex {
    // Manifest drops last: its SQLite handle protects the generation while data is in use.
    pub(super) data: Catalog,
    head: Option<Head>,
    archived_heads: VecDeque<Head>,
    spool: Option<super::spool::Spool>,
    rows: super::rows::Rows,
    timing: NativeQueryTiming,
    pub(super) manifest: Manifest,
}

impl NativeIndex {
    pub(super) fn new(manifest: Manifest, data: Catalog) -> Self {
        Self {
            manifest,
            data,
            head: None,
            archived_heads: VecDeque::new(),
            spool: None,
            rows: super::rows::Rows::default(),
            timing: NativeQueryTiming::default(),
        }
    }
    pub fn open(cache_dir: &Path) -> Result<Self> {
        let pointer = if cache_dir.join("current-native.index").is_file() {
            "current-native.index"
        } else {
            "building-native.index"
        };
        Self::open_database(&super::cleanup::pointer(cache_dir, pointer)?)
    }
    pub fn open_database(path: &Path) -> Result<Self> {
        let (manifest, data) = Manifest::open(path)?;
        Ok(Self::new(manifest, data))
    }
    pub fn open_scope(cache_dir: &Path, scope: &Path) -> Result<Self> {
        let index = Self::open(cache_dir)?;
        if index.status().scope != absolute(scope)? {
            return Err(Error::InvalidIndex("native cached scope mismatch".into()));
        }
        Ok(index)
    }
    pub fn status(&self) -> &IndexStatus {
        &self.manifest.status
    }
    pub fn last_query_timing(&self) -> NativeQueryTiming {
        self.timing
    }
    /// Allocated capacity retained only for catalog ingestion, excluding rows/keys.
    pub fn ingestion_buffer_bytes(&self) -> usize {
        self.data.ingestion_buffer_bytes()
    }
    /// Controlled decoder comparison: preserve ranks but discard decoded rows.
    pub fn benchmark_set_grouped_payload(&mut self, grouped: bool) {
        self.rows = super::rows::Rows::default();
        self.rows.ordered_baseline = !grouped;
    }
    pub fn query(
        &mut self,
        spec: &QuerySpec,
        offset: u64,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run(spec, offset, None, size, cancel, None)
    }
    pub fn query_after(
        &mut self,
        spec: &QuerySpec,
        cursor: Option<&PageCursor>,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run(
            spec,
            cursor.map_or(0, |cursor| cursor.offset),
            cursor,
            size,
            cancel,
            None,
        )
    }
    pub fn query_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        offset: u64,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run(spec, offset, None, size, cancel, Some(boundary))
    }
    pub fn query_after_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        cursor: Option<&PageCursor>,
        size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run(
            spec,
            cursor.map_or(0, |cursor| cursor.offset),
            cursor,
            size,
            cancel,
            Some(boundary),
        )
    }

    fn refresh_head(
        &mut self,
        spec: &QuerySpec,
        prefix: u64,
        cancel: &Cancellation,
    ) -> Result<bool> {
        let hash = fingerprint(spec, None);
        if self.head.as_ref().is_none_or(|head| head.hash != hash) {
            let saved = self
                .archived_heads
                .iter()
                .position(|head| head.hash == hash)
                .and_then(|index| self.archived_heads.remove(index));
            if let Some(old) = self.head.take() {
                self.archived_heads.push_front(old);
            }
            self.head = saved;
            while self.archived_heads.len() > 4
                || self.archived_heads.iter().map(Head::bytes).sum::<usize>() > ARCHIVED_HEAD_BYTES
            {
                self.archived_heads.pop_back();
            }
        }
        if self
            .spool
            .as_ref()
            .is_some_and(|spool| spool.hash != hash || spool.prefix > prefix)
        {
            self.spool = None;
        }
        if self
            .head
            .as_ref()
            .is_some_and(|head| head.hash == hash && head.prefix == prefix)
        {
            return Ok(true);
        }
        let old = self
            .head
            .take()
            .filter(|head| head.hash == hash && head.prefix < prefix);
        let result = self.data.search(
            &CatalogQuery {
                spec: spec.clone(),
                upper_id: Some(prefix),
                lower_id: old.as_ref().map_or(0, |head| head.prefix),
                limit: HEAD_ROWS,
                workers: if prefix < 50_000 { 1 } else { 8 },
                ..CatalogQuery::default()
            },
            cancel,
        )?;
        let mut keys = result.hits;
        let (mut folders, mut files) = (result.folders, result.files);
        let mut guaranteed = if folders + files > keys.len() as u64 {
            keys.len()
        } else {
            HEAD_ROWS
        };
        if let Some(old) = old {
            if !old.complete() {
                guaranteed = guaranteed.min(old.keys.len());
            }
            folders += old.folders;
            files += old.files;
            keys.extend(old.keys);
            keys.sort_unstable();
        }
        keys.truncate(guaranteed.min(HEAD_ROWS));
        keys.shrink_to_fit();
        let mut head = Head {
            hash,
            prefix,
            keys,
            folders,
            files,
        };
        while head.bytes() > HEAD_BYTES && !head.keys.is_empty() {
            head.keys.pop();
        }
        head.keys.shrink_to_fit();
        self.head = Some(head);
        Ok(false)
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        &mut self,
        spec: &QuerySpec,
        offset: u64,
        cursor: Option<&PageCursor>,
        size: usize,
        cancel: &Cancellation,
        boundary: Option<&IndexStatus>,
    ) -> Result<QueryPage> {
        let started = Instant::now();
        cancel.check()?;
        if spec.text.len() > 128 * 1024 {
            return Err(Error::InvalidPath(
                "native query exceeds path-size limit".into(),
            ));
        }
        let source = boundary.unwrap_or(self.status()).clone();
        if source.generation != self.status().generation
            || source.scope != self.status().scope
            || source.entries > self.status().entries
            || source.revision > self.status().revision
        {
            return Err(Error::InvalidCursor);
        }
        let hash = fingerprint(spec, boundary.map(|boundary| boundary.entries));
        if cursor.is_some_and(|cursor| {
            cursor.generation != source.generation
                || cursor.revision != source.revision
                || cursor.query_hash != hash
        }) {
            return Err(Error::InvalidCursor);
        }
        self.rows.timing = super::rows::Timing::default();
        let head_started = Instant::now();
        let head_reused = self.refresh_head(spec, source.entries, cancel)?;
        let head_elapsed = head_started.elapsed();
        let head = self.head.as_ref().unwrap();
        let total = head.files + head.folders;
        let limit = size.min(MAX_PAGE_ROWS);
        let mut rows = Vec::new();
        if limit > 0 && offset < total {
            let cached_start = if let Some(cursor) = cursor {
                head.keys
                    .iter()
                    .position(|key| key.id == cursor.last.id)
                    .map(|position| position + 1)
            } else {
                usize::try_from(offset).ok()
            };
            if let Some(start) = cached_start.filter(|start| *start < head.keys.len()) {
                rows = self.rows.page(
                    &self.data,
                    &head.keys[start..head.keys.len().min(start + limit)],
                    &source.scope,
                    cancel,
                )?;
            } else if cursor.is_none() || self.spool.is_some() {
                let query_hash = fingerprint(spec, None);
                // Take ownership before appending: a failed mutation discards
                // the unjournaled cache. Once complete, keep it across cancelled
                // page reads/decodes so another drag does not rebuild the sort.
                if self
                    .spool
                    .as_ref()
                    .is_none_or(|spool| spool.prefix != source.entries)
                {
                    let mut spool = match self.spool.take() {
                        Some(spool) => spool,
                        None => super::spool::Spool::create(&self.data, spec, query_hash)?,
                    };
                    spool.append(&self.data, spec, source.entries, cancel)?;
                    if spool.total != total {
                        return Err(Error::InvalidIndex(
                            "native result count disagrees with ordered cache".into(),
                        ));
                    }
                    self.spool = Some(spool);
                }
                let hits = self
                    .spool
                    .as_mut()
                    .unwrap()
                    .page(&self.data, spec, offset, limit, cancel)?;
                rows = self.rows.page(&self.data, &hits, &source.scope, cancel)?;
            } else {
                let mut after = if let Some(cursor) = cursor {
                    Some(self.key_for_row(&cursor.last, spec)?)
                } else {
                    None
                };
                let mut skipped = if cursor.is_some() { offset } else { 0 };
                // Sequential scrolling uses keysets until an arbitrary seek
                // requests an ordered disk cache.
                if cursor.is_none() && !head.keys.is_empty() && offset >= head.keys.len() as u64 {
                    skipped = head.keys.len() as u64;
                    after = head.keys.last().cloned();
                }
                while skipped < offset {
                    let advance = (offset - skipped).min(HEAD_ROWS as u64) as usize;
                    let next = self.data.search(
                        &CatalogQuery {
                            spec: spec.clone(),
                            upper_id: Some(source.entries),
                            after: after.take(),
                            limit: advance,
                            workers: if source.entries < 50_000 { 1 } else { 8 },
                            ..CatalogQuery::default()
                        },
                        cancel,
                    )?;
                    if next.hits.is_empty() {
                        return Err(Error::InvalidIndex(
                            "native stream ended before requested offset".into(),
                        ));
                    }
                    skipped += next.hits.len() as u64;
                    after = next.hits.last().cloned();
                }
                let next = self.data.search(
                    &CatalogQuery {
                        spec: spec.clone(),
                        upper_id: Some(source.entries),
                        after,
                        limit,
                        workers: if source.entries < 50_000 { 1 } else { 8 },
                        ..CatalogQuery::default()
                    },
                    cancel,
                )?;
                rows = self
                    .rows
                    .page(&self.data, &next.hits, &source.scope, cancel)?;
            }
        }
        let next_offset = offset.saturating_add(rows.len() as u64);
        if limit > 0 && offset < total && rows.is_empty() {
            return Err(Error::InvalidIndex(
                "ordered results ended before the reported total".into(),
            ));
        }
        let has_more = next_offset < total;
        let next = if has_more {
            rows.last().cloned().map(|last| PageCursor {
                generation: source.generation.clone(),
                revision: source.revision,
                query_hash: hash,
                last,
                offset: next_offset,
            })
        } else {
            None
        };
        cancel.check()?;
        self.timing = NativeQueryTiming {
            head: head_elapsed,
            decode: self.rows.timing.decode,
            rows: self.rows.timing.elapsed,
            cache_hits: self.rows.timing.hits,
            cache_misses: self.rows.timing.misses,
            chunks: self.data.shards.len(),
            head_reused,
        };
        Ok(QueryPage {
            rows,
            total: spec.include_total.then_some(total),
            kind_counts: spec.include_total.then_some((head.folders, head.files)),
            offset,
            next_offset,
            has_more,
            next,
            status: source,
            elapsed: started.elapsed(),
            used_trigram: false,
            used_character_index: false,
        })
    }

    fn key_for_row(&self, row: &IndexRow, spec: &QuerySpec) -> Result<Hit> {
        let shard = self
            .data
            .shards
            .partition_point(|shard| shard.first_id <= row.id)
            .checked_sub(1)
            .ok_or(Error::InvalidCursor)?;
        let ordinal = usize::try_from(row.id - self.data.shards[shard].first_id)
            .map_err(|_| Error::InvalidCursor)?;
        if ordinal >= self.data.shards[shard].count {
            return Err(Error::InvalidCursor);
        }
        Ok(Hit::from_row(row, spec, shard, ordinal))
    }

    pub fn metrics(&self) -> Result<CacheMetrics> {
        let (mut used, mut highwater) = (0, 0);
        // SAFETY: the owning connection is alive, and the status call retains no pointers.
        let result = unsafe {
            rusqlite::ffi::sqlite3_db_status(
                self.manifest.connection.handle(),
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                &mut used,
                &mut highwater,
                0,
            )
        };
        if result != rusqlite::ffi::SQLITE_OK {
            return Err(
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(result), None).into(),
            );
        }
        // SAFETY: SQLite's read-only global accounting accessor takes no pointers.
        let sqlite_memory = unsafe { rusqlite::ffi::sqlite3_memory_used().max(0) as usize };
        Ok(CacheMetrics {
            page_cache_budget: 640 * 1024
                + self.spool.as_ref().map_or(0, |_| super::spool::CACHE_BYTES),
            page_cache_used: used.max(0) as usize
                + self
                    .spool
                    .as_ref()
                    .map_or(Ok(0), |spool| spool.cache_used())?,
            rank_cache_budget: HEAD_BYTES + ARCHIVED_HEAD_BYTES + super::rows::BUDGET,
            rank_cache_used: self.head.as_ref().map_or(0, Head::bytes)
                + self.archived_heads.iter().map(Head::bytes).sum::<usize>()
                + self.rows.bytes(),
            sqlite_global_memory: sqlite_memory,
            database_bytes: self.data.bytes()
                + fs::metadata(&self.status().database_path)?.len()
                + self.spool.as_ref().map_or(0, |spool| spool.bytes()),
            sqlite_version: rusqlite::version().into(),
            fts5: false,
        })
    }

    /// Query-key/decoded-row allocations, including a conservative spool-cache
    /// allowance. Immutable descriptors and ingestion have separate bounds.
    pub fn retained_query_bytes(&self) -> usize {
        self.head.as_ref().map_or(0, Head::bytes)
            + self.archived_heads.iter().map(Head::bytes).sum::<usize>()
            + self.rows.bytes()
            + self.spool.as_ref().map_or(0, |_| super::spool::CACHE_BYTES)
    }
    pub fn cached_window(&self, spec: &QuerySpec, prefix: u64, offset: usize, size: usize) -> bool {
        let hash = fingerprint(spec, None);
        self.head.iter().chain(&self.archived_heads).any(|head| {
            head.hash == hash
                && head.prefix == prefix
                && (head.complete() || offset.saturating_add(size) <= head.keys.len())
        })
    }
    pub fn release_query_cache(&mut self) {
        let baseline = self.rows.ordered_baseline;
        self.head = None;
        self.archived_heads.clear();
        self.spool = None;
        self.rows = super::rows::Rows::default();
        self.rows.ordered_baseline = baseline;
    }
}
