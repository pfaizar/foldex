//! Opt-in ranked name visitation. The application supplies the matcher; ordinary
//! explorer query planning, signatures, pages and score caches are unaffected.
use super::{Catalog, Hit, NativeIndex, NativeIndexBuilder, format};
use crate::{Cancellation, Error, IndexRow, IndexStatus, QuerySpec, Result, codec};
use std::{borrow::Cow, ops::Range};

const MAX_HITS: usize = 64;
const MAX_VISIT_ROWS: usize = 8192;

pub struct CandidateBatch {
    pub hits: Vec<Hit>,
    /// Count of the visited prefix, including rejected/hidden names.
    pub scanned_to: u64,
    pub status: IndexStatus,
}

impl Catalog {
    /// Visit the append-only prefix interval (start, end], with no subsequence/signature
    /// prefilter. The callback receives display name/parent and kind/hidden
    /// flags. Display strings must never be used as filesystem identities.
    /// Only bounded winning locations are retained; paths are decoded later.
    pub(super) fn candidate_hits(
        &self,
        range: Range<u64>,
        limit: usize,
        cancel: &Cancellation,
        score: &mut impl FnMut(&str, &str, bool, bool) -> Option<i32>,
    ) -> Result<Vec<Hit>> {
        cancel.check()?;
        if range.start > range.end || range.end > self.published_entries() {
            return Err(Error::InvalidCursor);
        }
        let limit = limit.min(MAX_HITS);
        let mut best = Vec::<Hit>::with_capacity(limit);
        if limit == 0 {
            return Ok(best);
        }
        let spec = QuerySpec::default();
        for shard_index in self.shard_range(range.start, range.end) {
            cancel.check()?;
            let shard = &self.shards[shard_index];
            // One mapped shard at a time; no process-lifetime mapping per shard.
            let view = format::View::open(shard)?;
            let first = range
                .start
                .saturating_add(1)
                .saturating_sub(shard.first_id)
                .min(shard.count as u64) as usize;
            let end = range
                .end
                .saturating_sub(shard.first_id)
                .saturating_add(1)
                .min(shard.count as u64) as usize;
            for row in first..end {
                if row % 128 == 0 {
                    cancel.check()?;
                }
                let record = view.record(row)?;
                let name = match std::str::from_utf8(record.name()) {
                    Ok(name) => Cow::Borrowed(name),
                    Err(_) => {
                        Cow::Owned(codec::decode(record.name())?.to_string_lossy().into_owned())
                    }
                };
                let parent = String::from_utf8_lossy(record.path());
                let Some(rank) =
                    score(&name, &parent, record.flags & 1 != 0, record.flags & 2 != 0)
                else {
                    continue;
                };
                let id = shard.first_id + row as u64;
                let at = best.partition_point(|old| {
                    old.score.unwrap_or(i32::MIN) > rank || old.score == Some(rank) && old.id < id
                });
                if at < limit {
                    if best.len() == limit {
                        best.pop();
                    }
                    best.insert(at, Hit::location(id, shard_index, row, Some(rank), &spec));
                }
            }
        }
        cancel.check()?;
        Ok(best)
    }
}

impl NativeIndex {
    /// Rank at most one bounded range. Resume at `scanned_to`; callers merge the
    /// returned best locations with their previous best 64 for this generation.
    pub fn candidate_batch(
        &self,
        lower_id: u64,
        cancel: &Cancellation,
        score: &mut impl FnMut(&str, &str, bool, bool) -> Option<i32>,
    ) -> Result<CandidateBatch> {
        let status = self.status().clone();
        if lower_id > status.entries || status.entries > self.data.published_entries() {
            return Err(Error::InvalidCursor);
        }
        let upper = lower_id
            .saturating_add(MAX_VISIT_ROWS as u64)
            .min(status.entries);
        let hits = self
            .data
            .candidate_hits(lower_id..upper, MAX_HITS, cancel, score)?;
        Ok(CandidateBatch {
            hits,
            scanned_to: upper,
            status,
        })
    }

    /// Decode the bounded winning locations using the catalog's normal raw path
    /// codec and page byte budget, preserving exact OS identities.
    pub fn candidate_rows(&self, hits: &[Hit], cancel: &Cancellation) -> Result<Vec<IndexRow>> {
        self.data.rows(
            &hits[..hits.len().min(MAX_HITS)],
            &self.status().scope,
            crate::model::MAX_PAGE_BYTES,
            cancel,
        )
    }
}

impl NativeIndexBuilder {
    pub fn candidate_batch(
        &mut self,
        lower_id: u64,
        cancel: &Cancellation,
        score: &mut impl FnMut(&str, &str, bool, bool) -> Option<i32>,
    ) -> Result<CandidateBatch> {
        self.checkpoint()?;
        self.index
            .as_ref()
            .unwrap()
            .candidate_batch(lower_id, cancel, score)
    }

    pub fn candidate_rows(&self, hits: &[Hit], cancel: &Cancellation) -> Result<Vec<IndexRow>> {
        self.index.as_ref().unwrap().candidate_rows(hits, cancel)
    }
}
