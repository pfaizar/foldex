//! Worker-only disk-backed filename indexing. No user file is modified by this crate.
//!
//! One Index/IndexBuilder owns one SQLite connection. Interleave scan steps and queries
//! on one background worker to keep the aggregate page cache small.

#[cfg(feature = "catalog-experiment")]
pub mod catalog;
mod codec;
mod fuzzy;
mod model;
mod parallel_walk;
mod query;
mod rank_cache;
mod score_cache;
mod store;
mod walk;

pub use model::{
    BuildOptions, BuildProgress, BuildState, CacheMetrics, Cancellation, CleanupReport, Error,
    IndexRecord, IndexRow, IndexStatus, IngestionStats, KindFilter, MatchMode, PageCursor,
    QueryPage, QuerySpec, Result, SortColumn, TypeFilter,
};
#[cfg(feature = "diagnostics")]
pub use model::{GroupDiagnostics, QueryDiagnostics};
pub use store::{Index, IndexBuilder};
