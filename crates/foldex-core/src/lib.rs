//! Cached file browser models and a bounded background engine.
//!
//! UI code should use [`Browser`] requests, not the blocking convenience functions.
//! Paths for file actions must come from [`Entry::path`], never display text.

mod engine;
mod memory;
mod model;
mod restore;
mod scan;
mod watch;

pub use engine::{Browser, Engine, EngineOptions, Event, Operation, WatchCoverage};
pub use foldex_match::MatchMode;
pub use model::{
    Entry, EntryStore, KindFilter, LimitReason, Query, Snapshot, SortColumn, TypeFilter, View,
    filter_snapshot, natural_cmp,
};
pub use restore::{RestoredView, ViewRestore};
pub use scan::{enumerate_directory, scan_recursive};
