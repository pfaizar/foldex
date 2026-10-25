use std::{
    ffi::OsString,
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const MAX_PAGE_ROWS: usize = 256;
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;
pub const PAGE_CACHE_BUDGET: usize = 4 * 1024 * 1024;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Sqlite(rusqlite::Error),
    Cancelled,
    InvalidPath(String),
    InvalidIndex(String),
    InvalidCursor,
    WrongMode,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::Sqlite(error) => write!(f, "{error}"),
            Self::Cancelled => write!(f, "Index operation cancelled"),
            Self::InvalidPath(message) => write!(f, "Invalid index path: {message}"),
            Self::InvalidIndex(message) => write!(f, "Invalid filename index: {message}"),
            Self::InvalidCursor => write!(
                f,
                "Result cursor belongs to another query or index revision"
            ),
            Self::WrongMode => write!(f, "Manual records and filesystem traversal cannot be mixed"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        if value.sqlite_error_code() == Some(rusqlite::ErrorCode::OperationInterrupted) {
            Self::Cancelled
        } else {
            Self::Sqlite(value)
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Cancellation {
    flag: Arc<AtomicBool>,
    parents: Arc<[Cancellation]>,
}
impl Cancellation {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire) || self.parents.iter().any(Self::is_cancelled)
    }
    /// A fresh child token cancelled by either input. Cancelling the child never changes its parents.
    pub fn linked(&self, other: &Self) -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            parents: Arc::from([self.clone(), other.clone()]),
        }
    }
    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildState {
    Building,
    Complete,
    Partial,
    Cancelled,
}
impl BuildState {
    pub(crate) fn code(self) -> i64 {
        match self {
            Self::Building => 0,
            Self::Complete => 1,
            Self::Partial => 2,
            Self::Cancelled => 3,
        }
    }
    pub(crate) fn decode(value: i64) -> Result<Self> {
        match value {
            0 => Ok(Self::Building),
            1 => Ok(Self::Complete),
            2 => Ok(Self::Partial),
            3 => Ok(Self::Cancelled),
            _ => Err(Error::InvalidIndex("unknown build state".into())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct IndexStatus {
    pub directory_only: bool,
    pub generation: String,
    pub revision: u64,
    pub scope: PathBuf,
    pub database_path: PathBuf,
    pub state: BuildState,
    pub complete: bool,
    pub entries: u64,
    pub files: u64,
    pub folders: u64,
    pub skipped: u64,
    pub excluded_links: u64,
    pub excluded_cache: u64,
    pub started_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub source_root_modified_secs: Option<i64>,
    /// Cached results must be revalidated/rescanned before being called current.
    pub from_cache: bool,
    pub synthetic: bool,
}

#[derive(Clone, Debug)]
pub struct BuildProgress {
    pub status: IndexStatus,
    pub done: bool,
    pub elapsed: Duration,
}

#[derive(Clone, Debug)]
pub struct IndexRecord {
    pub relative_path: PathBuf,
    pub raw_name: OsString,
    pub name: String,
    pub lowercase: String,
    pub is_dir: bool,
    pub hidden: bool,
    pub symlink: bool,
    pub size: u64,
    pub modified_secs: i64,
    pub created_secs: Option<i64>,
    pub accessed_secs: Option<i64>,
}
impl IndexRecord {
    pub fn new(
        relative_path: PathBuf,
        is_dir: bool,
        size: u64,
        modified_secs: i64,
    ) -> Result<Self> {
        let relative_path = crate::codec::relative(&relative_path, false)?;
        let raw_name = relative_path
            .file_name()
            .ok_or_else(|| Error::InvalidPath("empty basename".into()))?
            .to_os_string();
        let name = raw_name.to_string_lossy().into_owned();
        let lowercase = name.to_lowercase();
        let hidden = name.starts_with('.');
        Ok(Self {
            relative_path,
            raw_name,
            name,
            lowercase,
            is_dir,
            hidden,
            symlink: false,
            size,
            modified_secs,
            created_secs: None,
            accessed_secs: None,
        })
    }
    pub(crate) fn heap_bytes(&self) -> usize {
        self.relative_path.capacity()
            + self.raw_name.capacity()
            + self.name.capacity()
            + self.lowercase.capacity()
    }
}

#[derive(Clone, Debug)]
pub struct IndexRow {
    pub id: u64,
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub raw_name: OsString,
    pub name: String,
    pub is_dir: bool,
    pub hidden: bool,
    pub symlink: bool,
    pub size: u64,
    pub modified_secs: i64,
    pub created_secs: Option<i64>,
    pub accessed_secs: Option<i64>,
    pub(crate) natural_key: Vec<u8>,
    pub(crate) raw_sort: Vec<u8>,
    pub(crate) extension: String,
    pub(crate) fuzzy_score: Option<i32>,
}
impl IndexRow {
    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.path.capacity()
            + self.relative_path.capacity()
            + self.raw_name.capacity()
            + self.name.capacity()
            + self.natural_key.capacity()
            + self.raw_sort.capacity()
            + self.extension.capacity()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SortColumn {
    #[default]
    Name,
    Size,
    Modified,
    Kind,
    Created,
    Accessed,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum MatchMode {
    #[default]
    Fuzzy,
    Substring,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TypeFilter {
    pub include_folders: bool,
    pub exclude: bool,
    pub extensions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QuerySpec {
    pub text: String,
    pub match_mode: MatchMode,
    pub sort: SortColumn,
    pub descending: bool,
    pub show_hidden: bool,
    pub kind: KindFilter,
    pub type_filter: Option<TypeFilter>,
    /// Disable for fast first paint; request a separate exact count afterwards.
    pub include_total: bool,
}
impl Default for QuerySpec {
    fn default() -> Self {
        Self {
            text: String::new(),
            match_mode: MatchMode::Fuzzy,
            sort: SortColumn::Name,
            descending: false,
            show_hidden: false,
            kind: KindFilter::All,
            type_filter: None,
            include_total: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PageCursor {
    pub(crate) generation: String,
    pub(crate) revision: u64,
    pub(crate) query_hash: u64,
    pub(crate) last: IndexRow,
    pub(crate) offset: u64,
}
impl PageCursor {
    pub fn next_offset(&self) -> u64 {
        self.offset
    }
}

#[derive(Clone, Debug)]
pub struct QueryPage {
    pub rows: Vec<IndexRow>,
    pub total: Option<u64>,
    /// Exact per-kind counts for this query when the total was requested.
    pub kind_counts: Option<(u64, u64)>,
    pub offset: u64,
    pub next_offset: u64,
    pub has_more: bool,
    pub next: Option<PageCursor>,
    pub status: IndexStatus,
    pub elapsed: Duration,
    pub used_trigram: bool,
    pub used_character_index: bool,
}

#[derive(Clone, Debug)]
pub struct CacheMetrics {
    pub page_cache_budget: usize,
    pub page_cache_used: usize,
    pub rank_cache_budget: usize,
    pub rank_cache_used: usize,
    pub sqlite_global_memory: usize,
    pub database_bytes: u64,
    pub sqlite_version: String,
    pub fts5: bool,
}

/// Opt-in benchmark instrumentation. Normal builds retain no callback counters or plans.
#[cfg(feature = "diagnostics")]
#[derive(Clone, Debug, Default)]
pub struct QueryDiagnostics {
    pub candidate_probes: u64,
    pub candidate_cache_hits: u64,
    pub membership_calls: u64,
    pub score_calls: u64,
    pub last_name_hits: u64,
    pub memo_hits: u64,
    pub source_scores: u64,
    pub groups: Vec<GroupDiagnostics>,
}

#[cfg(feature = "diagnostics")]
#[derive(Clone, Debug)]
pub struct GroupDiagnostics {
    pub is_dir: bool,
    pub sql: String,
    pub plan: Vec<String>,
    pub elapsed: Duration,
    pub payload_elapsed: Duration,
    pub vm_steps: u64,
    pub sorts: u64,
    pub full_scan_steps: u64,
    pub rows: usize,
}

#[derive(Clone, Debug, Default)]
pub struct IngestionStats {
    pub records: u64,
    pub entry_write: Duration,
    pub trigram_write: Duration,
    pub character_write: Duration,
    pub commit: Duration,
    pub commits: u64,
}

#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// Exclude the common index-cache ancestor, including caches for other scopes.
    /// It must already exist. This protects against indexing the index itself.
    pub excluded_root: Option<PathBuf>,
    /// Enumerate only direct children; never enqueue descendant directories.
    pub directory_only: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CleanupReport {
    pub removed_generations: u64,
    pub removed_bytes: u64,
    /// Unrecognized, invalid, busy, or mismatched files are deliberately retained.
    pub retained_unverified: u64,
}

pub(crate) fn extension(name: &str, is_dir: bool) -> &str {
    if is_dir {
        ""
    } else {
        name.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or("", |(_, suffix)| suffix)
    }
}

pub(crate) fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(std::path::absolute(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_cancellation_preempts_work_without_invalidating_parent_requests() {
        let query = Cancellation::new();
        let priority = Cancellation::new();
        let work = query.linked(&priority);
        priority.cancel();
        assert!(work.is_cancelled());
        assert!(!query.is_cancelled());
        let resumed = query.linked(&Cancellation::new());
        assert!(!resumed.is_cancelled());
        resumed.cancel();
        assert!(!query.is_cancelled());
        query.cancel();
        assert!(query.linked(&Cancellation::new()).is_cancelled());
    }
}
