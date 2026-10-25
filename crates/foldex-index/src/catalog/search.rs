use super::{Catalog, format};
use crate::{Cancellation, Error, MatchMode, QuerySpec, Result, SortColumn, codec};
use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::BinaryHeap,
    sync::OnceLock,
    time::{Duration, Instant},
};

const KEY_BYTES: usize = 1024 * 1024;

fn query_pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: OnceLock<Option<rayon::ThreadPool>> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(std::thread::available_parallelism().map_or(1, |count| count.get().min(8)))
            .stack_size(256 * 1024)
            .thread_name(|index| format!("foldex-rank-{index}"))
            .start_handler(|_| {
                #[cfg(windows)]
                {
                    use windows_sys::Win32::System::Threading::{
                        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
                    };
                    // SAFETY: this lowers only the newly started scoring worker.
                    unsafe {
                        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                    }
                }
            })
            .build()
            .ok()
    })
    .as_ref()
}

fn trim_unused(bytes: &mut Vec<u8>) {
    if bytes.capacity() > 256 && bytes.capacity() > bytes.len().saturating_mul(4) {
        bytes.shrink_to(bytes.len().max(64));
    }
}

struct MatchPlan {
    needle: String,
    points: Vec<u32>,
    matcher: foldex_match::Matcher,
    mode: MatchMode,
    ranked: bool,
    type_filter: Option<crate::TypeFilter>,
    signature: Option<format::Signature>,
    literal_ascii: bool,
}
impl MatchPlan {
    fn new(query: &CatalogQuery) -> Result<Self> {
        if query.spec.type_filter.as_ref().is_some_and(|filter| {
            filter.extensions.len() > 256
                || filter.extensions.iter().any(|value| value.len() > 1024)
        }) {
            return Err(Error::InvalidPath(
                "type filter exceeds its bounded extension list".into(),
            ));
        }
        if query.spec.text.len() > 128 * 1024 {
            return Err(Error::InvalidPath(
                "catalog query exceeds path-size limit".into(),
            ));
        }
        let needle = match query.spec.match_mode {
            MatchMode::Fuzzy => foldex_match::fold_key(&query.spec.text),
            MatchMode::Substring => query.spec.text.to_lowercase(),
        };
        let literal_ascii = query.spec.match_mode == MatchMode::Substring && needle.is_ascii();
        let signature = (query.prefilter
            && !needle.is_empty()
            && (query.spec.match_mode == MatchMode::Fuzzy || literal_ascii))
            .then(|| format::Signature::of(&needle));
        Ok(Self {
            points: needle.chars().map(u32::from).collect(),
            matcher: foldex_match::Matcher::new(&needle),
            needle,
            mode: query.spec.match_mode,
            ranked: query.spec.match_mode == MatchMode::Fuzzy
                && query.spec.sort == SortColumn::Name
                && query.limit > 0,
            type_filter: query.spec.type_filter.clone(),
            signature,
            literal_ascii,
        })
    }
    /// Outer None is no match; inner None is a matching unranked query.
    fn evaluate(
        &self,
        record: &format::Record<'_>,
        scorer: &mut crate::score_cache::ScoreMemo,
        rank: bool,
    ) -> Result<Option<Option<i32>>> {
        if let Some(filter) = &self.type_filter {
            if record.flags & 1 != 0 {
                if !filter.include_folders {
                    return Ok(None);
                }
            } else {
                let lower = std::str::from_utf8(record.lower())
                    .map_err(|_| Error::InvalidIndex("catalog lowercase is not UTF-8".into()))?;
                let extension = crate::model::extension(lower, false);
                if filter.extensions.iter().any(|value| value == extension) == filter.exclude {
                    return Ok(None);
                }
            }
        }
        if self.needle.is_empty() {
            return Ok(Some(None));
        }
        match self.mode {
            MatchMode::Substring => {
                let lower = std::str::from_utf8(record.lower())
                    .map_err(|_| Error::InvalidIndex("catalog lowercase is not UTF-8".into()))?;
                Ok(lower.contains(&self.needle).then_some(None))
            }
            MatchMode::Fuzzy => {
                let name = match std::str::from_utf8(record.name()) {
                    Ok(name) => Cow::Borrowed(name),
                    Err(_) => {
                        Cow::Owned(codec::decode(record.name())?.to_string_lossy().into_owned())
                    }
                };
                if self.ranked && rank {
                    Ok(scorer.score(&self.matcher, &name).map(Some))
                } else {
                    Ok(self.matcher.is_match(&name).then_some(None))
                }
            }
        }
    }
}

#[derive(Clone)]
pub struct CatalogQuery {
    pub spec: QuerySpec,
    pub upper_id: Option<u64>,
    pub limit: usize,
    pub workers: usize,
    pub prefilter: bool,
    pub rank_pruning: bool,
    pub bitmap_prefilter: bool,
    pub lower_id: u64,
    pub after: Option<Hit>,
}

impl Default for CatalogQuery {
    fn default() -> Self {
        Self {
            spec: QuerySpec::default(),
            upper_id: None,
            lower_id: 0,
            limit: 256,
            workers: 8,
            prefilter: true,
            rank_pruning: true,
            bitmap_prefilter: true,
            after: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub id: u64,
    pub shard: usize,
    pub row: usize,
    pub score: Option<i32>,
    pub is_dir: bool,
    pub size: u64,
    pub modified_secs: i64,
    pub created_secs: Option<i64>,
    pub accessed_secs: Option<i64>,
    pub(super) natural: Vec<u8>,
    pub(super) raw: Vec<u8>,
    pub(super) extension: String,
    sort: SortColumn,
    descending: bool,
}
impl PartialEq for Hit {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Hit {}
impl PartialOrd for Hit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Hit {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.sort as u8)
            .cmp(&(other.sort as u8))
            .then_with(|| self.descending.cmp(&other.descending))
            .then_with(|| self.key().compare(other.key(), self.sort, self.descending))
    }
}
impl Hit {
    pub(super) fn location(
        id: u64,
        shard: usize,
        row: usize,
        score: Option<i32>,
        spec: &QuerySpec,
    ) -> Self {
        Self {
            id,
            shard,
            row,
            score,
            is_dir: false,
            size: 0,
            modified_secs: 0,
            created_secs: None,
            accessed_secs: None,
            natural: Vec::new(),
            raw: Vec::new(),
            extension: String::new(),
            sort: spec.sort,
            descending: spec.descending,
        }
    }
    pub(super) fn from_row(
        row: &crate::IndexRow,
        spec: &QuerySpec,
        shard: usize,
        ordinal: usize,
    ) -> Self {
        Self {
            id: row.id,
            shard,
            row: ordinal,
            score: row.fuzzy_score,
            is_dir: row.is_dir,
            size: row.size,
            modified_secs: row.modified_secs,
            created_secs: row.created_secs,
            accessed_secs: row.accessed_secs,
            natural: row.natural_key.clone(),
            raw: row.raw_sort.clone(),
            extension: row.extension.clone(),
            sort: spec.sort,
            descending: spec.descending,
        }
    }
    pub(super) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.natural.capacity()
            + self.raw.capacity()
            + self.extension.capacity()
    }

    fn key(&self) -> OrderKey<'_> {
        OrderKey {
            is_dir: self.is_dir,
            score: self.score,
            size: self.size,
            modified: self.modified_secs,
            created: self.created_secs,
            accessed: self.accessed_secs,
            extension: &self.extension,
            natural: &self.natural,
            raw: &self.raw,
            id: self.id,
        }
    }
}

#[derive(Clone, Copy)]
struct OrderKey<'a> {
    is_dir: bool,
    score: Option<i32>,
    size: u64,
    modified: i64,
    created: Option<i64>,
    accessed: Option<i64>,
    extension: &'a str,
    natural: &'a [u8],
    raw: &'a [u8],
    id: u64,
}
impl OrderKey<'_> {
    fn compare(self, other: Self, sort: SortColumn, descending: bool) -> Ordering {
        other
            .is_dir
            .cmp(&self.is_dir)
            .then_with(|| other.score.cmp(&self.score))
            .then_with(|| {
                let primary = match sort {
                    SortColumn::Name => Ordering::Equal,
                    SortColumn::Size => self.size.cmp(&other.size),
                    SortColumn::Modified => self.modified.cmp(&other.modified),
                    SortColumn::Created => self
                        .created
                        .unwrap_or(if descending { i64::MIN } else { i64::MAX })
                        .cmp(&other.created.unwrap_or(if descending {
                            i64::MIN
                        } else {
                            i64::MAX
                        })),
                    SortColumn::Accessed => self
                        .accessed
                        .unwrap_or(if descending { i64::MIN } else { i64::MAX })
                        .cmp(&other.accessed.unwrap_or(if descending {
                            i64::MIN
                        } else {
                            i64::MAX
                        })),
                    SortColumn::Kind => self.extension.cmp(other.extension),
                };
                let tie = primary
                    .then_with(|| self.natural.cmp(other.natural))
                    .then_with(|| self.raw.cmp(other.raw))
                    .then_with(|| self.id.cmp(&other.id));
                if descending { tie.reverse() } else { tie }
            })
    }
}

struct Top {
    heap: BinaryHeap<Hit>,
    limit: usize,
    bytes: usize,
    folders: u64,
    files: u64,
    candidates: u64,
    rejected: u64,
    eligible: u64,
    rank_pruned: u64,
}
impl Top {
    fn new(limit: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(limit),
            limit,
            bytes: limit * std::mem::size_of::<Hit>(),
            folders: 0,
            files: 0,
            candidates: 0,
            rejected: 0,
            eligible: 0,
            rank_pruned: 0,
        }
    }
    /// Skip only scoring, never membership/counting. Folder order and this
    /// conservative source-score ceiling prove a candidate cannot enter the
    /// full heap. Cursor queries keep exact scores for boundary comparisons.
    fn skip_rank(
        &self,
        query: &CatalogQuery,
        plan: &MatchPlan,
        record: &format::Record<'_>,
    ) -> bool {
        if !query.rank_pruning
            || !plan.ranked
            || plan.points.is_empty()
            || query.after.is_some()
            || self.heap.len() != self.limit
        {
            return false;
        }
        let Some(worst) = self.heap.peek() else {
            return false;
        };
        let is_dir = record.flags & 1 != 0;
        if is_dir != worst.is_dir {
            return worst.is_dir;
        }
        let Some(score) = worst.score else {
            return false;
        };
        if !record.name().is_ascii() {
            return false;
        }
        let length = record.name().len().min(foldex_match::MAX_TEXT_BYTES) as i32;
        let pattern = plan.points.len() as i32;
        // Source: base=100-3*(length-pattern), first boundary <=50,
        // each subsequent boundary <=30 and adjacency <=200. Index/gap
        // penalties never increase the score. Strict inequality preserves ties.
        let ceiling = 100 - 3 * (length - pattern) + 50 + (pattern - 1) * 230;
        ceiling < score
    }
    fn consider(
        &mut self,
        shard: usize,
        row: usize,
        id: u64,
        record: &format::Record<'_>,
        score: Option<i32>,
        query: &CatalogQuery,
        extension: &str,
    ) {
        if self.limit == 0 {
            return;
        }
        let is_dir = record.flags & 1 != 0;
        let key = OrderKey {
            is_dir,
            score,
            size: record.size,
            modified: record.modified,
            created: record.created,
            accessed: record.accessed,
            extension,
            natural: record.natural(),
            raw: record.name(),
            id,
        };
        if query.after.as_ref().is_some_and(|after| {
            key.compare(after.key(), query.spec.sort, query.spec.descending) != Ordering::Greater
        }) {
            return;
        }
        self.eligible += 1;
        if self.heap.len() == self.limit
            && self.heap.peek().is_some_and(|other| {
                key.compare(other.key(), query.spec.sort, query.spec.descending) != Ordering::Less
            })
        {
            return;
        }
        if self.heap.len() == self.limit {
            let mut worst = self.heap.peek_mut().unwrap();
            self.bytes -=
                worst.natural.capacity() + worst.raw.capacity() + worst.extension.capacity();
            worst.id = id;
            worst.shard = shard;
            worst.row = row;
            worst.score = score;
            worst.is_dir = is_dir;
            worst.size = record.size;
            worst.modified_secs = record.modified;
            worst.created_secs = record.created;
            worst.accessed_secs = record.accessed;
            worst.descending = query.spec.descending;
            worst.sort = query.spec.sort;
            worst.extension.clear();
            worst.extension.push_str(extension);
            worst.natural.clear();
            worst.natural.extend_from_slice(record.natural());
            worst.raw.clear();
            worst.raw.extend_from_slice(record.name());
            trim_unused(&mut worst.natural);
            trim_unused(&mut worst.raw);
            if worst.extension.capacity() > 256
                && worst.extension.capacity() > worst.extension.len().saturating_mul(4)
            {
                worst.extension.shrink_to_fit();
            }
            self.bytes +=
                worst.natural.capacity() + worst.raw.capacity() + worst.extension.capacity();
        } else {
            let hit = Hit {
                id,
                shard,
                row,
                score,
                is_dir,
                size: record.size,
                modified_secs: record.modified,
                created_secs: record.created,
                accessed_secs: record.accessed,
                natural: record.natural().to_vec(),
                raw: record.name().to_vec(),
                descending: query.spec.descending,
                sort: query.spec.sort,
                extension: extension.to_owned(),
            };
            self.bytes += hit.bytes() - std::mem::size_of::<Hit>();
            self.heap.push(hit);
        }
        while self.bytes > KEY_BYTES && !self.heap.is_empty() {
            let removed = self.heap.pop().unwrap();
            self.bytes -= removed.bytes() - std::mem::size_of::<Hit>();
            // A reduced prefix cannot be expanded later: discarded older ranks
            // might otherwise precede newly admitted worse candidates.
            self.limit = self.limit.min(self.heap.len());
        }
    }
}

pub struct SearchResult {
    pub hits: Vec<Hit>,
    pub folders: u64,
    pub files: u64,
    pub candidates: u64,
    pub prefilter_rejected: u64,
    pub elapsed: Duration,
    pub retained_key_bytes: usize,
    pub rank_pruned: u64,
}

impl Catalog {
    pub(super) fn visit_matches(
        &self,
        query: &CatalogQuery,
        cancel: &Cancellation,
        visitor: &mut impl FnMut(u64, &format::Record<'_>, Option<i32>) -> Result<()>,
    ) -> Result<u64> {
        cancel.check()?;
        let plan = MatchPlan::new(query)?;
        let upper = query.upper_id.unwrap_or_else(|| self.published_entries());
        if upper > self.published_entries() || query.lower_id > upper {
            return Err(Error::InvalidCursor);
        }
        let mut scorer = crate::score_cache::ScoreMemo::new();
        let mut matches = 0;
        for shard_index in self.shard_range(query.lower_id, upper) {
            cancel.check()?;
            let shard = &self.shards[shard_index];
            let view = format::View::open(shard)?;
            let first = query
                .lower_id
                .saturating_add(1)
                .saturating_sub(shard.first_id)
                .min(shard.count as u64) as usize;
            let end = upper
                .saturating_sub(shard.first_id)
                .saturating_add(1)
                .min(shard.count as u64) as usize;
            let selection = if query.bitmap_prefilter {
                view.select(
                    plan.signature.as_ref(),
                    plan.literal_ascii,
                    query.spec.show_hidden,
                    query.spec.kind,
                    first..end,
                )?
            } else {
                None
            };
            let rows = match selection {
                Some(selection) => super::bitmap::Rows::Selected(selection),
                None => super::bitmap::Rows::Range(first..end),
            };
            let deleted = self.deleted.get(&shard_index);
            for (ordinal, row) in rows.enumerate() {
                if super::masks::deleted(deleted, row) {
                    continue;
                }
                if ordinal % 128 == 0 {
                    cancel.check()?;
                }
                let (flags, admitted) =
                    view.filter(row, plan.signature.as_ref(), plan.literal_ascii)?;
                if !admitted
                    || !query.spec.show_hidden && flags & 2 != 0
                    || !query.spec.kind.includes(flags & 1 != 0)
                {
                    continue;
                }
                let record = view.record(row)?;
                let Some(score) = plan.evaluate(&record, &mut scorer, true)? else {
                    continue;
                };
                visitor(shard.first_id + row as u64, &record, score)?;
                matches += 1;
            }
        }
        Ok(matches)
    }

    pub fn search(&self, query: &CatalogQuery, cancel: &Cancellation) -> Result<SearchResult> {
        cancel.check()?;
        if query.spec.text.len() > 128 * 1024 {
            return Err(Error::InvalidPath(
                "catalog query exceeds path-size limit".into(),
            ));
        }
        if query.after.as_ref().is_some_and(|after| {
            after.sort != query.spec.sort || after.descending != query.spec.descending
        }) {
            return Err(Error::InvalidCursor);
        }
        let started = Instant::now();
        let plan = MatchPlan::new(query)?;
        let limit = query.limit.min(1025);
        // The low-level primitive honors its explicit worker budget. The app
        // facade chooses a single worker for small catalogs.
        let upper = query.upper_id.unwrap_or_else(|| self.published_entries());
        if upper > self.published_entries() || query.lower_id > upper {
            return Err(Error::InvalidCursor);
        }
        let shards = self.shard_range(query.lower_id, upper);
        let workers = query.workers.clamp(1, 8).min(shards.len().max(1));
        let run = |begin: usize, end: usize| -> Result<Top> {
            let mut top = Top::new(limit);
            let mut scorer = crate::score_cache::ScoreMemo::new();
            for shard_index in begin..end {
                cancel.check()?;
                let shard = &self.shards[shard_index];
                let view = format::View::open(shard)?;
                let first_row = query
                    .lower_id
                    .saturating_add(1)
                    .saturating_sub(shard.first_id)
                    .min(shard.count as u64) as usize;
                let last_row = upper
                    .saturating_sub(shard.first_id)
                    .saturating_add(1)
                    .min(shard.count as u64) as usize;
                let selection = if query.bitmap_prefilter {
                    view.select(
                        plan.signature.as_ref(),
                        plan.literal_ascii,
                        query.spec.show_hidden,
                        query.spec.kind,
                        first_row..last_row,
                    )?
                } else {
                    None
                };
                let rows = match selection {
                    Some(selection) => {
                        let rejected = selection.eligible - selection.selected;
                        top.candidates += rejected;
                        top.rejected += rejected;
                        super::bitmap::Rows::Selected(selection)
                    }
                    None => super::bitmap::Rows::Range(first_row..last_row),
                };
                let deleted = self.deleted.get(&shard_index);
                for (ordinal, row) in rows.enumerate() {
                    if super::masks::deleted(deleted, row) {
                        continue;
                    }
                    if ordinal % 128 == 0 {
                        cancel.check()?;
                    }
                    let (flags, admitted) =
                        view.filter(row, plan.signature.as_ref(), plan.literal_ascii)?;
                    let is_dir = flags & 1 != 0;
                    if !query.spec.show_hidden && flags & 2 != 0
                        || !query.spec.kind.includes(is_dir)
                    {
                        continue;
                    }
                    top.candidates += 1;
                    if !admitted {
                        top.rejected += 1;
                        continue;
                    }
                    let record = view.record(row)?;
                    let skip_rank = top.skip_rank(query, &plan, &record);
                    if skip_rank {
                        top.rank_pruned += 1;
                    }
                    let Some(score) = plan.evaluate(&record, &mut scorer, !skip_rank)? else {
                        continue;
                    };
                    if is_dir {
                        top.folders += 1;
                    } else {
                        top.files += 1;
                    }
                    let extension = if query.spec.sort == SortColumn::Kind {
                        crate::model::extension(
                            std::str::from_utf8(record.lower()).map_err(|_| {
                                Error::InvalidIndex("catalog lowercase is not UTF-8".into())
                            })?,
                            is_dir,
                        )
                    } else {
                        ""
                    };
                    top.consider(
                        shard_index,
                        row,
                        shard.first_id + row as u64,
                        &record,
                        score,
                        query,
                        extension,
                    );
                }
            }
            Ok(top)
        };
        let partials = if workers == 1 {
            vec![run(shards.start, shards.end)?]
        } else if let Some(pool) = query_pool() {
            use rayon::prelude::*;
            let chunk = shards.len().div_ceil(workers);
            let ranges = (shards.start..shards.end)
                .step_by(chunk)
                .map(|begin| (begin, (begin + chunk).min(shards.end)))
                .collect::<Vec<_>>();
            pool.install(|| {
                ranges
                    .par_iter()
                    .map(|&(begin, end)| run(begin, end))
                    .collect::<Result<Vec<_>>>()
            })?
        } else {
            // Resource exhaustion must preserve correctness and cancellation.
            vec![run(shards.start, shards.end)?]
        };
        let mut folders = 0;
        let mut files = 0;
        let mut candidates = 0;
        let mut prefilter_rejected = 0;
        let mut retained_key_bytes = 0usize;
        let mut rank_pruned = 0u64;
        let guaranteed = partials.iter().fold(limit, |limit, part| {
            if part.eligible > part.heap.len() as u64 {
                limit.min(part.heap.len())
            } else {
                limit
            }
        });
        let mut hits = Vec::with_capacity(workers * limit);
        for partial in partials {
            folders += partial.folders;
            files += partial.files;
            candidates += partial.candidates;
            prefilter_rejected += partial.rejected;
            retained_key_bytes += partial.bytes;
            rank_pruned += partial.rank_pruned;
            hits.extend(partial.heap.into_vec());
        }
        hits.sort_unstable();
        hits.truncate(guaranteed);
        hits.shrink_to_fit();
        cancel.check()?;
        Ok(SearchResult {
            hits,
            folders,
            files,
            candidates,
            prefilter_rejected,
            elapsed: started.elapsed(),
            retained_key_bytes,
            rank_pruned,
        })
    }
}
