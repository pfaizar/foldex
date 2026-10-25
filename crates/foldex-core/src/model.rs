use std::{
    cmp::Ordering,
    ffi::OsString,
    mem::size_of,
    ops::Index,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::{Duration, Instant},
};

pub(crate) const CHUNK_LEN: usize = 256;
static NEXT_SNAPSHOT: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Display-only Unicode text. Use `raw_name` or `path` for filesystem actions.
    pub name: String,
    pub raw_name: OsString,
    /// Exact path relative to the search root. Absent for ordinary directory views.
    pub relative_path: Option<PathBuf>,
    pub is_dir: bool,
    pub hidden: bool,
    pub symlink: bool,
    pub size: u64,
    pub modified_secs: i64,
    /// None when the source or filesystem does not supply this timestamp.
    pub created_secs: Option<i64>,
    pub accessed_secs: Option<i64>,
    /// Only retain a separate key when case conversion changes the display name.
    lowercase: Option<Box<str>>,
    pub metadata_known: bool,
}

impl Entry {
    pub fn from_name(
        name: impl Into<OsString>,
        is_dir: bool,
        size: u64,
        modified_secs: i64,
    ) -> Self {
        let raw_name = name.into();
        let name = raw_name.to_string_lossy().into_owned();
        let lowercase = if name
            .bytes()
            .all(|byte| byte.is_ascii() && !byte.is_ascii_uppercase())
        {
            None
        } else {
            // Keep Rust's contextual Unicode conversion, including final sigma
            // and lowercase expansions. Unchanged Unicode can share the name too.
            let lower = name.to_lowercase();
            (lower != name).then(|| lower.into_boxed_str())
        };
        Self {
            hidden: name.starts_with('.'),
            name,
            raw_name,
            relative_path: None,
            is_dir,
            symlink: false,
            size,
            modified_secs,
            created_secs: None,
            accessed_secs: None,
            lowercase,
            metadata_known: true,
        }
    }

    #[inline]
    pub fn lowercase(&self) -> &str {
        self.lowercase.as_deref().unwrap_or(&self.name)
    }

    /// Constructs the exact path, preserving non-Unicode Windows filenames.
    pub fn path(&self, snapshot: &Snapshot) -> PathBuf {
        snapshot.path.join(
            self.relative_path
                .as_deref()
                .unwrap_or_else(|| Path::new(&self.raw_name)),
        )
    }

    pub fn extension(&self) -> &str {
        if self.is_dir {
            return "";
        }
        self.lowercase()
            .rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or("", |(_, ext)| ext)
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.name
            .capacity()
            .saturating_add(self.lowercase.as_ref().map_or(0, |lower| lower.len()))
            .saturating_add(self.raw_name.capacity())
            .saturating_add(self.relative_path.as_ref().map_or(0, PathBuf::capacity))
    }
}

/// Immutable chunks let progressive snapshots share entries instead of copying strings.
#[derive(Clone, Debug, Default)]
pub struct EntryStore {
    pub(crate) chunks: Vec<Arc<[Entry]>>,
    pub(crate) len: usize,
    pub(crate) reservation: Option<Arc<crate::memory::MemoryReservation>>,
}

impl EntryStore {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn get(&self, index: usize) -> Option<&Entry> {
        if index >= self.len {
            return None;
        }
        self.chunks.get(index / CHUNK_LEN)?.get(index % CHUNK_LEN)
    }
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.chunks.iter().flat_map(|chunk| chunk.iter())
    }
    pub(crate) fn from_vec(entries: Vec<Entry>) -> Self {
        let len = entries.len();
        let mut source = entries.into_iter();
        let mut chunks = Vec::with_capacity(len.div_ceil(CHUNK_LEN));
        loop {
            let chunk: Vec<Entry> = source.by_ref().take(CHUNK_LEN).collect();
            if chunk.is_empty() {
                break;
            }
            chunks.push(Arc::from(chunk));
        }
        Self {
            chunks,
            len,
            reservation: None,
        }
    }
    pub(crate) fn managed_bytes(&self) -> usize {
        self.chunks.capacity() * size_of::<Arc<[Entry]>>()
            + self
                .chunks
                .iter()
                .map(|chunk| {
                    2 * size_of::<usize>()
                        + chunk.len() * size_of::<Entry>()
                        + chunk.iter().map(Entry::heap_bytes).sum::<usize>()
                })
                .sum::<usize>()
    }
}

impl Index<usize> for EntryStore {
    type Output = Entry;
    fn index(&self, index: usize) -> &Self::Output {
        self.get(index).expect("entry index out of range")
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum SortColumn {
    #[default]
    Name,
    Size,
    Modified,
    Created,
    Accessed,
    Kind,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum KindFilter {
    #[default]
    All,
    Files,
    Folders,
    None,
}

impl KindFilter {
    pub fn includes(self, is_dir: bool) -> bool {
        match self {
            Self::All => true,
            Self::Files => !is_dir,
            Self::Folders => is_dir,
            Self::None => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct TypeFilter {
    pub include_folders: bool,
    /// Exclusion mode preserves types that have not appeared in the bounded menu yet.
    pub exclude: bool,
    /// Exact lowercase extensions without a dot; empty selects extensionless files.
    pub extensions: Vec<String>,
}
impl TypeFilter {
    pub fn includes(&self, entry: &Entry) -> bool {
        if entry.is_dir {
            self.include_folders
        } else {
            self.extensions
                .iter()
                .any(|extension| extension == entry.extension())
                != self.exclude
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Query {
    pub conditions: foldex_query::Conditions,
    pub text: String,
    pub match_mode: foldex_match::MatchMode,
    pub sort: SortColumn,
    pub descending: bool,
    pub show_hidden: bool,
    pub kind: KindFilter,
    pub type_filter: Option<TypeFilter>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitReason {
    EntryCount,
    MemoryBudget,
    Depth,
}

#[derive(Debug)]
pub struct Snapshot {
    pub id: u64,
    pub path: PathBuf,
    pub entries: EntryStore,
    /// True only after traversal ended without unreadable entries or a limit.
    pub complete: bool,
    pub skipped: u64,
    pub excluded_links: u64,
    pub elapsed: Duration,
    /// Logical retained model bytes, including distinct prepared orders.
    /// Shared chunks and aliased order storage count once here.
    pub managed_bytes: usize,
    pub recursive: bool,
    pub limit: Option<LimitReason>,
    /// The shared model pool was exhausted, rather than this directory's own
    /// configured limit. A visible client can retry after other models release.
    pub shared_memory_pressure: bool,
    pub default_order: Arc<[u32]>,
    default_visible_order: Arc<[u32]>,
    alternate_order: Mutex<Option<(SortColumn, bool, Arc<[u32]>)>>,
}

impl Snapshot {
    /// Blocking model constructor, useful for fixture benchmarks and non-UI callers.
    pub fn from_entries(path: PathBuf, entries: Vec<Entry>) -> Self {
        let start = Instant::now();
        Self::build(
            path,
            EntryStore::from_vec(entries),
            true,
            0,
            0,
            false,
            None,
            start,
            &|| false,
        )
        .expect("an uncancelled snapshot constructor cannot cancel")
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build(
        path: PathBuf,
        entries: EntryStore,
        complete: bool,
        skipped: u64,
        excluded_links: u64,
        recursive: bool,
        limit: Option<LimitReason>,
        start: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Self> {
        let default_order = make_order(&entries, SortColumn::Name, false, cancelled)?;
        let default_visible_order = visible_order(&entries, &default_order, cancelled)?;
        let visible_bytes = if Arc::ptr_eq(&default_order, &default_visible_order) {
            0
        } else {
            default_visible_order.len() * size_of::<u32>()
        };
        let managed_bytes = size_of::<Self>()
            .saturating_add(path.capacity())
            .saturating_add(entries.managed_bytes())
            .saturating_add(default_order.len() * size_of::<u32>())
            .saturating_add(visible_bytes);
        Some(Self {
            id: NEXT_SNAPSHOT.fetch_add(1, AtomicOrdering::Relaxed),
            path,
            entries,
            complete,
            skipped,
            excluded_links,
            elapsed: start.elapsed(),
            managed_bytes,
            recursive,
            limit,
            shared_memory_pressure: false,
            default_order,
            default_visible_order,
            alternate_order: Mutex::new(None),
        })
    }

    pub fn resolve(&self, index: u32) -> Option<PathBuf> {
        self.entries
            .get(index as usize)
            .map(|entry| entry.path(self))
    }

    /// Both default views are prepared by the model worker. This clones only
    /// an Arc, regardless of directory size; it performs no filtering or I/O.
    pub fn default_indices(&self, show_hidden: bool) -> Arc<[u32]> {
        if show_hidden {
            self.default_order.clone()
        } else {
            self.default_visible_order.clone()
        }
    }

    /// Default orders keep folders first, so counts need no scan on the UI thread.
    pub fn kind_counts(&self, show_hidden: bool) -> (usize, usize) {
        let order = if show_hidden {
            &self.default_order
        } else {
            &self.default_visible_order
        };
        let folders = order.partition_point(|&index| self.entries[index as usize].is_dir);
        (folders, order.len() - folders)
    }

    fn order(&self, query: &Query, cancelled: &dyn Fn() -> bool) -> Option<Arc<[u32]>> {
        if query.sort == SortColumn::Name && !query.descending {
            return Some(self.default_order.clone());
        }
        if let Ok(cache) = self.alternate_order.lock() {
            if let Some((column, descending, order)) = &*cache {
                if *column == query.sort && *descending == query.descending {
                    return Some(order.clone());
                }
            }
        }
        let order = make_order(&self.entries, query.sort, query.descending, cancelled)?;
        if let Ok(mut cache) = self.alternate_order.lock() {
            *cache = Some((query.sort, query.descending, order.clone()));
        }
        Some(order)
    }
}

fn visible_order(
    entries: &EntryStore,
    order: &Arc<[u32]>,
    cancelled: &dyn Fn() -> bool,
) -> Option<Arc<[u32]>> {
    let mut hidden = 0;
    for (position, entry) in entries.iter().enumerate() {
        if position % 256 == 0 && cancelled() {
            return None;
        }
        hidden += usize::from(entry.hidden);
    }
    if hidden == 0 {
        return Some(order.clone());
    }
    let mut visible = Vec::with_capacity(order.len() - hidden);
    for chunk in order.chunks(256) {
        if cancelled() {
            return None;
        }
        visible.extend(
            chunk
                .iter()
                .copied()
                .filter(|&index| !entries[index as usize].hidden),
        );
    }
    Some(visible.into())
}

#[derive(Clone, Debug)]
pub struct View {
    pub snapshot: Arc<Snapshot>,
    pub indices: Arc<[u32]>,
    pub elapsed: Duration,
    /// Text/hidden-file matches before the folder/file visibility toggles.
    pub kind_counts: (usize, usize),
}

/// Blocking query helper for tests and benchmarks. UI code should call `Browser::filter`.
pub fn filter_snapshot(snapshot: Arc<Snapshot>, query: &Query) -> View {
    filter_cancelled(snapshot, query, &|| false).expect("an uncancelled filter cannot cancel")
}

pub(crate) fn filter_cancelled(
    snapshot: Arc<Snapshot>,
    query: &Query,
    cancelled: &dyn Fn() -> bool,
) -> Option<View> {
    let start = Instant::now();
    if cancelled() {
        return None;
    }
    if query.text.is_empty()
        && query.kind == KindFilter::All
        && query.type_filter.is_none()
        && query.conditions.is_empty()
        && query.sort == SortColumn::Name
        && !query.descending
    {
        return Some(View {
            indices: snapshot.default_indices(query.show_hidden),
            kind_counts: snapshot.kind_counts(query.show_hidden),
            snapshot,
            elapsed: start.elapsed(),
        });
    }
    let order = snapshot.order(query, cancelled)?;
    // Substring uses full Unicode lowercase and the entire query. Fuzzy uses
    // the matcher's own bounded simple fold; neither needs the other's buffers.
    let needle =
        (query.match_mode == foldex_match::MatchMode::Substring).then(|| query.text.to_lowercase());
    let matcher = (query.match_mode == foldex_match::MatchMode::Fuzzy)
        .then(|| foldex_match::Matcher::new(&query.text));
    let rank = matcher.as_ref().is_some_and(|matcher| !matcher.is_empty())
        && query.sort == SortColumn::Name;
    if (query.show_hidden || snapshot.default_visible_order.len() == snapshot.entries.len())
        && query.kind == KindFilter::All
        && query.type_filter.is_none()
        && query.conditions.is_empty()
        && query.text.is_empty()
    {
        return Some(View {
            kind_counts: snapshot.kind_counts(query.show_hidden),
            snapshot,
            indices: order,
            elapsed: start.elapsed(),
        });
    }
    let mut matches = if rank {
        Vec::new()
    } else {
        Vec::with_capacity(order.len().min(4096))
    };
    let mut ranked: Vec<(u32, i32)> = Vec::new();
    let mut kind_counts = (0, 0);
    for (position, &index) in order.iter().enumerate() {
        if position % 256 == 0 && cancelled() {
            return None;
        }
        let entry = &snapshot.entries[index as usize];
        if !query.show_hidden && entry.hidden {
            continue;
        }
        if query
            .type_filter
            .as_ref()
            .is_some_and(|filter| !filter.includes(entry))
        {
            continue;
        }
        let relative = entry
            .relative_path
            .as_deref()
            .unwrap_or_else(|| Path::new(&entry.raw_name));
        if !query
            .conditions
            .matches(relative, entry.is_dir, entry.size, entry.modified_secs)
        {
            continue;
        }
        let included = query.kind.includes(entry.is_dir);
        let score = if query.text.is_empty() {
            Some(0)
        } else if let Some(needle) = &needle {
            entry.lowercase().contains(needle.as_str()).then_some(0)
        } else {
            let matcher = matcher.as_ref().expect("fuzzy matcher prepared");
            if rank && included {
                matcher.score(&entry.name)
            } else {
                // Metadata sorts and kind counts consume membership only.
                // Skip alternate alignment scoring without changing matches.
                matcher.is_match(&entry.name).then_some(0)
            }
        };
        if let Some(score) = score {
            if entry.is_dir {
                kind_counts.0 += 1;
            } else {
                kind_counts.1 += 1;
            }
            if !included {
                continue;
            }
            if rank {
                ranked.push((index, score));
            } else {
                matches.push(index);
            }
        }
    }
    if rank {
        let compare = |a: &(u32, i32), b: &(u32, i32)| {
            snapshot.entries[b.0 as usize]
                .is_dir
                .cmp(&snapshot.entries[a.0 as usize].is_dir)
                .then_with(|| b.1.cmp(&a.1))
        };
        const RUN: usize = 2048;
        for run in ranked.chunks_mut(RUN) {
            if cancelled() {
                return None;
            }
            run.sort_unstable_by(compare);
        }
        if ranked.len() > RUN {
            let mut scratch = vec![(0, 0); ranked.len()];
            let mut width = RUN;
            while width < ranked.len() {
                for left in (0..ranked.len()).step_by(width.saturating_mul(2)) {
                    let mid = (left + width).min(ranked.len());
                    let end = (left + width * 2).min(ranked.len());
                    let (mut a, mut b) = (left, mid);
                    for out in left..end {
                        if out % 256 == 0 && cancelled() {
                            return None;
                        }
                        if a < mid
                            && (b >= end || compare(&ranked[a], &ranked[b]) != Ordering::Greater)
                        {
                            scratch[out] = ranked[a];
                            a += 1;
                        } else {
                            scratch[out] = ranked[b];
                            b += 1;
                        }
                    }
                }
                std::mem::swap(&mut ranked, &mut scratch);
                width = width.saturating_mul(2);
            }
        }
        matches = ranked.into_iter().map(|v| v.0).collect();
    }
    if cancelled() {
        return None;
    }
    Some(View {
        snapshot,
        indices: Arc::from(matches),
        elapsed: start.elapsed(),
        kind_counts,
    })
}

pub(crate) fn compare_entries(
    a: &Entry,
    b: &Entry,
    column: SortColumn,
    descending: bool,
) -> Ordering {
    let folders = b.is_dir.cmp(&a.is_dir);
    if folders != Ordering::Equal {
        return folders;
    }
    let name = || {
        natural_cmp(a.lowercase(), b.lowercase())
            .then_with(|| a.raw_name.cmp(&b.raw_name))
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    };
    let order = match column {
        SortColumn::Name => name(),
        SortColumn::Size => a.size.cmp(&b.size).then_with(name),
        SortColumn::Modified => a.modified_secs.cmp(&b.modified_secs).then_with(name),
        SortColumn::Created => a.created_secs.cmp(&b.created_secs).then_with(name),
        SortColumn::Accessed => a.accessed_secs.cmp(&b.accessed_secs).then_with(name),
        SortColumn::Kind => a.extension().cmp(b.extension()).then_with(name),
    };
    if descending { order.reverse() } else { order }
}

fn make_order(
    entries: &EntryStore,
    column: SortColumn,
    descending: bool,
    cancelled: &dyn Fn() -> bool,
) -> Option<Arc<[u32]>> {
    if cancelled() {
        return None;
    }
    let mut indices: Vec<u32> = (0..entries.len()).map(|index| index as u32).collect();
    let compare = |&a: &u32, &b: &u32| {
        compare_entries(
            &entries[a as usize],
            &entries[b as usize],
            column,
            descending,
        )
    };
    // Bounded runs allow superseding a large sort without waiting for one huge sort call.
    const RUN: usize = 2048;
    for run in indices.chunks_mut(RUN) {
        if cancelled() {
            return None;
        }
        run.sort_unstable_by(compare);
    }
    if indices.len() > RUN {
        let mut scratch = vec![0; indices.len()];
        let mut width = RUN;
        while width < indices.len() {
            for left in (0..indices.len()).step_by(width.saturating_mul(2)) {
                let mid = (left + width).min(indices.len());
                let end = (mid + width).min(indices.len());
                let (mut a, mut b) = (left, mid);
                for out in left..end {
                    if out % 256 == 0 && cancelled() {
                        return None;
                    }
                    if a < mid
                        && (b >= end || compare(&indices[a], &indices[b]) != Ordering::Greater)
                    {
                        scratch[out] = indices[a];
                        a += 1;
                    } else {
                        scratch[out] = indices[b];
                        b += 1;
                    }
                }
            }
            std::mem::swap(&mut indices, &mut scratch);
            width = width.saturating_mul(2);
        }
    }
    if cancelled() {
        return None;
    }
    Some(Arc::from(indices))
}

/// Allocation-free natural comparison of already-normalized names. Numeric runs do not overflow.
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let (a, b) = (left.as_bytes(), right.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (start_i, start_j) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let mut significant_i = start_i;
            let mut significant_j = start_j;
            while significant_i < i && a[significant_i] == b'0' {
                significant_i += 1;
            }
            while significant_j < j && b[significant_j] == b'0' {
                significant_j += 1;
            }
            let order = (i - significant_i)
                .cmp(&(j - significant_j))
                .then_with(|| a[significant_i..i].cmp(&b[significant_j..j]))
                .then_with(|| (i - start_i).cmp(&(j - start_j)));
            if order != Ordering::Equal {
                return order;
            }
        } else {
            let order = a[i].cmp(&b[j]);
            if order != Ordering::Equal {
                return order;
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_membership_preserves_metadata_sort_results_and_kind_counts() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::from("membership-fixture"),
            [
                "report-project.rs",
                "archive_report.txt",
                "a_a_a_a.txt",
                "aaaa.rs",
                "PrOjEcT.rs",
                ".report.txt",
                "東京_report.rs",
                "ΚΕΦΑΛΑΙΟ.txt",
                "κεφάλαιο.rs",
                "İstanbul.txt",
                "i\u{307}stanbul.rs",
                "unrelated",
            ]
            .into_iter()
            .enumerate()
            .map(|(i, name)| {
                let mut entry = Entry::from_name(name, i % 4 == 0, (i % 3) as u64, i as i64);
                entry.created_secs = (i % 2 == 0).then_some((12 - i) as i64);
                entry.accessed_secs = (i % 3 == 0).then_some(i as i64);
                entry
            })
            .collect(),
        ));
        for sort in [
            SortColumn::Size,
            SortColumn::Modified,
            SortColumn::Created,
            SortColumn::Accessed,
            SortColumn::Kind,
        ] {
            for descending in [false, true] {
                for show_hidden in [false, true] {
                    for kind in [
                        KindFilter::All,
                        KindFilter::Files,
                        KindFilter::Folders,
                        KindFilter::None,
                    ] {
                        for type_filter in [
                            None,
                            Some(TypeFilter {
                                include_folders: true,
                                exclude: true,
                                extensions: vec!["txt".into()],
                            }),
                        ] {
                            for text in ["rp", "aa", "東京", "κ", "i", "does-not-exist", ""] {
                                let query = Query {
                                    text: text.into(),
                                    sort,
                                    descending,
                                    show_hidden,
                                    kind,
                                    type_filter: type_filter.clone(),
                                    ..Query::default()
                                };
                                // Score-based membership is the previous behavior. Keep
                                // every matching index in its exact metadata/name order.
                                let matcher = foldex_match::Matcher::new(text);
                                let mut expected = Vec::new();
                                let mut counts = (0, 0);
                                for &index in snapshot.order(&query, &|| false).unwrap().iter() {
                                    let entry = &snapshot.entries[index as usize];
                                    if (!show_hidden && entry.hidden)
                                        || type_filter.as_ref().is_some_and(|f| !f.includes(entry))
                                        || matcher.score(&entry.name).is_none()
                                    {
                                        continue;
                                    }
                                    if entry.is_dir {
                                        counts.0 += 1;
                                    } else {
                                        counts.1 += 1;
                                    }
                                    if kind.includes(entry.is_dir) {
                                        expected.push(index);
                                    }
                                }
                                let view = filter_snapshot(snapshot.clone(), &query);
                                assert_eq!(view.indices.as_ref(), expected.as_slice(), "{query:?}");
                                assert_eq!(view.kind_counts, counts, "{query:?}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn substring_queries_keep_the_full_text_beyond_the_fuzzy_prefix_limit() {
        let prefix = "a".repeat(foldex_match::MAX_TEXT_BYTES);
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::new(),
            vec![
                Entry::from_name(format!("{prefix}z"), false, 0, 0),
                Entry::from_name(format!("{prefix}x"), false, 0, 0),
            ],
        ));
        for (match_mode, expected) in [
            (foldex_match::MatchMode::Substring, vec![0]),
            (foldex_match::MatchMode::Fuzzy, vec![1, 0]),
        ] {
            let query = Query {
                text: format!("{prefix}z"),
                match_mode,
                sort: SortColumn::Size,
                ..Query::default()
            };
            assert_eq!(
                filter_snapshot(snapshot.clone(), &query).indices.as_ref(),
                expected.as_slice()
            );
        }
    }

    #[test]
    fn membership_filter_cancels_after_a_partial_pass() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::new(),
            (0..1024)
                .map(|i| Entry::from_name(format!("project_{i}_report.rs"), false, i, 0))
                .collect(),
        ));
        let mut query = Query {
            sort: SortColumn::Size,
            ..Query::default()
        };
        // Prepare the alternate order so cancellation here exercises filtering.
        filter_snapshot(snapshot.clone(), &query);
        query.text = "pr".into();
        let checks = std::cell::Cell::new(0);
        assert!(
            filter_cancelled(snapshot, &query, &|| {
                checks.set(checks.get() + 1);
                checks.get() >= 3
            })
            .is_none()
        );
        assert_eq!(checks.get(), 3);
    }

    #[test]
    fn type_masks_intersect_visibility_and_preserve_unknown_types_in_exclusion_mode() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::from("fixture"),
            vec![
                Entry::from_name("report-folder", true, 0, 0),
                Entry::from_name("report.PNG", false, 10, 0),
                Entry::from_name("report.txt", false, 20, 0),
                Entry::from_name("report.new", false, 30, 0),
            ],
        ));
        let mut query = Query {
            text: "report".into(),
            show_hidden: true,
            kind: KindFilter::Files,
            type_filter: Some(TypeFilter {
                include_folders: true,
                exclude: true,
                extensions: vec!["png".into(), "txt".into()],
            }),
            ..Default::default()
        };
        let view = filter_snapshot(snapshot.clone(), &query);
        assert_eq!(view.indices.as_ref(), &[3]);
        assert_eq!(view.kind_counts, (1, 1));
        query.kind = KindFilter::None;
        assert!(filter_snapshot(snapshot.clone(), &query).indices.is_empty());
        query.kind = KindFilter::All;
        query.type_filter = Some(TypeFilter::default());
        assert!(filter_snapshot(snapshot.clone(), &query).indices.is_empty());
        query.type_filter = None;
        assert_eq!(filter_snapshot(snapshot, &query).indices.len(), 4);
    }

    #[test]
    fn created_and_accessed_sorts_use_independent_real_timestamps() {
        let mut a = Entry::from_name("a", false, 1, 100);
        a.created_secs = Some(20);
        a.accessed_secs = Some(10);
        let mut b = Entry::from_name("b", false, 1, 100);
        b.created_secs = Some(10);
        b.accessed_secs = Some(20);
        let snapshot = Arc::new(Snapshot::from_entries(PathBuf::from("fixture"), vec![a, b]));
        for (sort, expected) in [
            (SortColumn::Created, [1, 0]),
            (SortColumn::Accessed, [0, 1]),
        ] {
            let view = filter_snapshot(
                snapshot.clone(),
                &Query {
                    sort,
                    show_hidden: true,
                    ..Default::default()
                },
            );
            assert_eq!(view.indices.as_ref(), &expected);
        }
    }

    #[test]
    fn natural_numbers_leading_zeros_and_huge_numbers() {
        let mut names = vec![
            "file10",
            "file0002",
            "file2",
            "file02",
            "file1",
            "file999999999999999999999999999999",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            [
                "file1",
                "file2",
                "file02",
                "file0002",
                "file10",
                "file999999999999999999999999999999"
            ]
        );
        assert_eq!(natural_cmp("a000", "a00"), Ordering::Greater);
    }

    #[test]
    fn lowercase_keys_share_text_without_changing_unicode() {
        for (name, expected) in [
            ("", ""),
            ("file_0123.txt", "file_0123.txt"),
            ("File_0123.TXT", "file_0123.txt"),
            ("ΟΣ", "ος"),
            ("ΟΣΑ", "οσα"),
            ("İstanbul.TXT", "i\u{307}stanbul.txt"),
            ("KELVIN.TXT", "kelvin.txt"),
            ("StraẞE.rs", "straße.rs"),
            ("東京.rs", "東京.rs"),
            ("foo🙂.PNG", "foo🙂.png"),
        ] {
            let entry = Entry::from_name(name, false, 0, 0);
            assert_eq!(entry.lowercase(), expected, "{name}");
            assert_eq!(entry.raw_name, OsString::from(name));
            assert_eq!(entry.name, name);
            if name == expected {
                assert!(entry.lowercase.is_none());
                assert_eq!(entry.lowercase().as_ptr(), entry.name.as_ptr());
            }
            assert_eq!(entry.clone().lowercase(), expected);
        }
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::from("root"),
            vec![
                Entry::from_name("İstanbul.TXT", false, 0, 0),
                Entry::from_name("istanbul.txt", false, 0, 0),
                Entry::from_name("ΟΣ", false, 0, 0),
            ],
        ));
        for (text, name) in [("i\u{307}s", "İstanbul.TXT"), ("ος", "ΟΣ")] {
            let view = filter_snapshot(
                snapshot.clone(),
                &Query {
                    text: text.into(),
                    match_mode: foldex_match::MatchMode::Substring,
                    ..Query::default()
                },
            );
            assert_eq!(view.indices.len(), 1);
            assert_eq!(view.snapshot.entries[view.indices[0] as usize].name, name);
        }
    }

    #[test]
    fn unicode_filter_uses_cached_lowercase_and_keeps_raw_path() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::from("root"),
            vec![
                Entry::from_name("ÄPFEL2.txt", false, 0, 0),
                Entry::from_name("東京.rs", false, 0, 0),
                Entry::from_name("hidden", true, 0, 0),
            ],
        ));
        let view = filter_snapshot(
            snapshot.clone(),
            &Query {
                text: "äpf".into(),
                ..Query::default()
            },
        );
        assert_eq!(view.indices.len(), 1);
        assert_eq!(
            snapshot.resolve(view.indices[0]),
            Some(PathBuf::from("root").join("ÄPFEL2.txt"))
        );
        let view = filter_snapshot(
            snapshot,
            &Query {
                text: "東京".into(),
                kind: KindFilter::Files,
                ..Query::default()
            },
        );
        assert_eq!(view.indices.len(), 1);
    }

    #[test]
    fn descending_preserves_folders_first_and_hidden_filter() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::new(),
            vec![
                Entry::from_name("file2", false, 2, 0),
                Entry::from_name("file10", false, 10, 0),
                Entry::from_name("folder", true, 0, 0),
                Entry::from_name(".secret", false, 20, 0),
            ],
        ));
        let view = filter_snapshot(
            snapshot,
            &Query {
                descending: true,
                ..Query::default()
            },
        );
        let names: Vec<_> = view
            .indices
            .iter()
            .map(|&i| view.snapshot.entries[i as usize].name.as_str())
            .collect();
        assert_eq!(names, ["folder", "file10", "file2"]);
    }

    #[test]
    fn prepared_visibility_order_keeps_sort_and_reuses_storage() {
        let mut secret = Entry::from_name("secret", false, 0, 0);
        secret.hidden = true;
        let mut visible_dot = Entry::from_name(".visible", false, 0, 0);
        visible_dot.hidden = false;
        let model = Arc::new(Snapshot::from_entries(
            PathBuf::new(),
            vec![
                Entry::from_name("file10", false, 0, 0),
                Entry::from_name("folder", true, 0, 0),
                Entry::from_name("file2", false, 0, 0),
                secret,
                visible_dot,
            ],
        ));
        let visible = model.default_indices(false);
        let names: Vec<_> = visible
            .iter()
            .map(|&index| model.entries[index as usize].name.as_str())
            .collect();
        assert_eq!(names, ["folder", ".visible", "file2", "file10"]);
        assert!(Arc::ptr_eq(
            &model.default_order,
            &model.default_indices(true)
        ));
        assert!(Arc::ptr_eq(
            &visible,
            &filter_snapshot(model.clone(), &Query::default()).indices
        ));

        let all_visible =
            Snapshot::from_entries(PathBuf::new(), vec![Entry::from_name("plain", false, 0, 0)]);
        assert!(Arc::ptr_eq(
            &all_visible.default_order,
            &all_visible.default_indices(false)
        ));
        let all_hidden = Snapshot::from_entries(
            PathBuf::new(),
            vec![Entry::from_name(".hidden", false, 0, 0)],
        );
        assert!(all_hidden.default_indices(false).is_empty());
        assert_eq!(all_hidden.default_indices(true).len(), 1);
    }

    #[test]
    fn cancelled_filter_and_sort_do_not_publish_partial_result() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::new(),
            (0..5000)
                .map(|i| Entry::from_name(format!("item{i}"), false, i, 0))
                .collect(),
        ));
        assert!(filter_cancelled(snapshot.clone(), &Query::default(), &|| true).is_none());
        assert!(make_order(&snapshot.entries, SortColumn::Size, true, &|| true).is_none());
    }

    #[test]
    fn kind_counts_follow_text_and_hidden_settings_while_visibility_is_independent() {
        let model = Arc::new(Snapshot::from_entries(
            PathBuf::new(),
            vec![
                Entry::from_name("report-folder", true, 0, 0),
                Entry::from_name("report.txt", false, 0, 0),
                Entry::from_name(".report.txt", false, 0, 0),
                Entry::from_name("other.txt", false, 0, 0),
            ],
        ));
        for (kind, len) in [
            (KindFilter::All, 2),
            (KindFilter::Files, 1),
            (KindFilter::Folders, 1),
            (KindFilter::None, 0),
        ] {
            let mut query = Query {
                text: "report".into(),
                kind,
                ..Default::default()
            };
            let view = filter_snapshot(model.clone(), &query);
            assert_eq!(view.indices.len(), len);
            assert_eq!(view.kind_counts, (1, 1));
            query.show_hidden = true;
            assert_eq!(filter_snapshot(model.clone(), &query).kind_counts, (1, 2));
            query.text.clear();
            assert_eq!(filter_snapshot(model.clone(), &query).kind_counts, (1, 3));
        }
        assert_eq!(model.kind_counts(false), (1, 2));
    }

    #[test]
    fn chunked_entries_keep_random_access_and_iteration() {
        let entries = EntryStore::from_vec(
            (0..1000)
                .map(|i| Entry::from_name(format!("{i}"), false, i, 0))
                .collect(),
        );
        assert_eq!(entries.len(), 1000);
        assert_eq!(entries[256].name, "256");
        assert_eq!(entries[999].size, 999);
        assert!(entries.get(1000).is_none());
        assert_eq!(entries.iter().count(), 1000);
    }

    #[test]
    fn large_natural_order_crosses_all_merge_runs() {
        let snapshot = Snapshot::from_entries(
            PathBuf::new(),
            (0..10_000)
                .map(|i| Entry::from_name(format!("file{}", i * 9973 % 10_000), false, 0, 0))
                .collect(),
        );
        for (expected, &index) in snapshot.default_order.iter().enumerate() {
            assert_eq!(
                snapshot.entries[index as usize].name,
                format!("file{expected}")
            );
        }
    }

    #[test]
    fn non_unicode_names_are_preserved_for_actions() {
        #[cfg(windows)]
        let raw = {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[b'x' as u16, 0xd800, b'y' as u16])
        };
        #[cfg(unix)]
        let raw = {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'x', 0xff, b'y'])
        };
        let entry = Entry::from_name(raw.clone(), false, 0, 0);
        let snapshot = Snapshot::from_entries(PathBuf::from("root"), vec![entry]);
        assert_eq!(snapshot.entries[0].raw_name, raw);
        assert_eq!(
            snapshot.entries[0].path(&snapshot),
            PathBuf::from("root").join(raw)
        );
        assert!(snapshot.entries[0].name.contains('\u{fffd}'));
    }
}
