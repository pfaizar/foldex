//! Selection restoration on the query worker, using raw identities rather than old row numbers.
use crate::{Entry, Query, Snapshot, memory::MemoryBudget, natural_cmp};
use std::{cmp::Ordering, ffi::OsString, sync::Arc};

const SMALL_SELECTION: usize = 64;
/// Bounds the latest event payload and its short UI-thread clone to 32 KiB.
const MAX_RESTORED_RANGES: usize = 4096;

/// The previously rendered view. Inclusive, disjoint row intervals stay compact for Select All.
/// Callers should request restoration only when the directory and query remain unchanged.
#[derive(Clone, Debug)]
pub struct ViewRestore {
    pub snapshot: Arc<Snapshot>,
    pub indices: Arc<[u32]>,
    pub selected: Vec<(u32, u32)>,
    pub focused: Option<usize>,
    /// An explicit ordinary-directory filename overrides old selection, including at startup.
    /// Recursive basename matches are ambiguous and are deliberately not used as targets.
    pub target: Option<OsString>,
}

/// New view rows, matched only by exact raw filename and relative-path identity.
#[derive(Clone, Debug, Default)]
pub struct RestoredView {
    pub selected: Vec<(u32, u32)>,
    pub focused: Option<usize>,
    /// Workspace/reply limits retained only the primary selection instead of silently guessing.
    pub truncated: bool,
}

fn same_identity(a: &Entry, b: &Entry) -> bool {
    a.raw_name == b.raw_name && a.relative_path == b.relative_path
}

fn identity_cmp(a: &Entry, b: &Entry) -> Ordering {
    natural_cmp(a.lowercase(), b.lowercase())
        .then_with(|| a.raw_name.cmp(&b.raw_name))
        .then_with(|| a.relative_path.cmp(&b.relative_path))
}

fn find_identity(snapshot: &Snapshot, wanted: &Entry) -> Option<u32> {
    let split = snapshot
        .default_order
        .partition_point(|&index| snapshot.entries[index as usize].is_dir);
    // Identity survives an in-place kind/metadata change, so check both folders-first groups.
    for group in [
        &snapshot.default_order[..split],
        &snapshot.default_order[split..],
    ] {
        if let Ok(position) =
            group.binary_search_by(|&index| identity_cmp(&snapshot.entries[index as usize], wanted))
        {
            let index = group[position];
            if same_identity(&snapshot.entries[index as usize], wanted) {
                return Some(index);
            }
        }
    }
    None
}

fn find_row(indices: &[u32], wanted: u32, cancelled: &dyn Fn() -> bool) -> Option<usize> {
    // A filtered view may be ranked by fuzzy relevance rather than its display sort column.
    for (row, &index) in indices.iter().enumerate() {
        if row % 256 == 0 && cancelled() {
            return None;
        }
        if index == wanted {
            return Some(row);
        }
    }
    None
}

fn selected_contains(selected: &[(u32, u32)], row: usize) -> bool {
    let Ok(row) = u32::try_from(row) else {
        return false;
    };
    let position = selected.partition_point(|&(_, end)| end < row);
    selected
        .get(position)
        .is_some_and(|&(start, end)| start <= row && row <= end)
}

fn old_row(restore: &ViewRestore, row: usize) -> Option<&Entry> {
    restore
        .snapshot
        .entries
        .get(*restore.indices.get(row)? as usize)
}

fn primary_only(
    snapshot: &Snapshot,
    indices: &[u32],
    restore: &ViewRestore,
    cancelled: &dyn Fn() -> bool,
) -> Option<RestoredView> {
    let focused_index = restore
        .focused
        .and_then(|row| old_row(restore, row))
        .and_then(|old| find_identity(snapshot, old));
    let first_selected = restore.selected.first().map(|&(start, _)| start as usize);
    let primary = restore
        .focused
        .filter(|&row| selected_contains(&restore.selected, row))
        .or(first_selected);
    let primary_index = primary
        .and_then(|row| old_row(restore, row))
        .and_then(|old| find_identity(snapshot, old));
    let fallback_index = first_selected
        .filter(|row| Some(*row) != primary)
        .and_then(|row| old_row(restore, row))
        .and_then(|old| find_identity(snapshot, old));
    let (mut focused, mut primary_row, mut fallback_row) = (None, None, None);
    for (row, &index) in indices.iter().enumerate() {
        if row % 256 == 0 && cancelled() {
            return None;
        }
        if Some(index) == focused_index {
            focused = Some(row);
        }
        if Some(index) == primary_index {
            primary_row = Some(row);
        }
        if Some(index) == fallback_index {
            fallback_row = Some(row);
        }
        if (focused_index.is_none() || focused.is_some())
            && (primary_index.is_none() || primary_row.is_some())
            && (fallback_index.is_none() || fallback_row.is_some())
        {
            break;
        }
    }
    if cancelled() {
        return None;
    }
    let selected = primary_row
        .or(fallback_row)
        .and_then(|row| u32::try_from(row).ok())
        .map(|row| vec![(row, row)])
        .unwrap_or_default();
    Some(RestoredView {
        selected,
        focused,
        truncated: true,
    })
}

/// Two naturally ordered folder/file runs can be merged without allocating a new order.
struct IdentityOrder<'a> {
    snapshot: &'a Snapshot,
    folder: usize,
    file: usize,
    split: usize,
}
impl<'a> IdentityOrder<'a> {
    fn new(snapshot: &'a Snapshot) -> Self {
        let split = snapshot
            .default_order
            .partition_point(|&index| snapshot.entries[index as usize].is_dir);
        Self {
            snapshot,
            folder: 0,
            file: split,
            split,
        }
    }
    fn next(&mut self) -> Option<u32> {
        let folders = (self.folder < self.split).then(|| self.snapshot.default_order[self.folder]);
        let files = self.snapshot.default_order.get(self.file).copied();
        let take_folder = match (folders, files) {
            (Some(a), Some(b)) => {
                identity_cmp(
                    &self.snapshot.entries[a as usize],
                    &self.snapshot.entries[b as usize],
                ) != Ordering::Greater
            }
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return None,
        };
        if take_folder {
            self.folder += 1;
            folders
        } else {
            self.file += 1;
            files
        }
    }
}

struct Bits(Vec<u64>);
impl Bits {
    fn new(len: usize) -> Self {
        Self(vec![0; len.div_ceil(64)])
    }
    fn bytes(len: usize) -> usize {
        len.div_ceil(64).saturating_mul(8)
    }
    fn insert(&mut self, index: usize) {
        self.0[index / 64] |= 1_u64 << (index % 64);
    }
    fn contains(&self, index: usize) -> bool {
        self.0
            .get(index / 64)
            .is_some_and(|word| word & (1_u64 << (index % 64)) != 0)
    }
}

fn push_row(ranges: &mut Vec<(u32, u32)>, row: usize) -> bool {
    let Ok(row) = u32::try_from(row) else {
        return false;
    };
    if let Some((_, end)) = ranges.last_mut() {
        if *end == row {
            return true;
        }
        if end.checked_add(1) == Some(row) {
            *end = row;
            return true;
        }
    }
    if ranges.len() >= MAX_RESTORED_RANGES {
        return false;
    }
    ranges.push((row, row));
    true
}

pub(crate) fn restore_cancelled(
    snapshot: &Snapshot,
    indices: &[u32],
    _query: &Query,
    restore: ViewRestore,
    memory: &Arc<MemoryBudget>,
    cancelled: &dyn Fn() -> bool,
) -> Option<RestoredView> {
    if cancelled() {
        return None;
    }
    if let Some(target) = restore.target.as_ref() {
        let wanted = Entry::from_name(target.clone(), false, 0, 0);
        let row =
            find_identity(snapshot, &wanted).and_then(|index| find_row(indices, index, cancelled));
        if cancelled() {
            return None;
        }
        return Some(RestoredView {
            selected: row
                .and_then(|r| u32::try_from(r).ok())
                .map(|r| vec![(r, r)])
                .unwrap_or_default(),
            focused: row,
            truncated: false,
        });
    }
    if restore.snapshot.path != snapshot.path || restore.snapshot.recursive != snapshot.recursive {
        return Some(RestoredView::default());
    }
    let mut count = 0_usize;
    let mut previous = None;
    for (position, &(start, end)) in restore.selected.iter().enumerate() {
        if position % 256 == 0 && cancelled() {
            return None;
        }
        if start > end
            || end as usize >= restore.indices.len()
            || previous.is_some_and(|previous| start <= previous)
        {
            return primary_only(snapshot, indices, &restore, cancelled);
        }
        count = count.saturating_add((end - start) as usize + 1);
        previous = Some(end);
    }
    let focused_index = restore
        .focused
        .and_then(|row| old_row(&restore, row))
        .and_then(|old| find_identity(snapshot, old));
    if count == 0 {
        let focused = focused_index.and_then(|index| find_row(indices, index, cancelled));
        if cancelled() {
            return None;
        }
        return Some(RestoredView {
            focused,
            ..RestoredView::default()
        });
    }
    let workspace = memory.reservation();
    if count <= SMALL_SELECTION {
        // Fixed stack workspace for common single/few-file changes. The bounded reply's
        // heap capacity is reserved against the same shared budget while constructing it.
        if !workspace.grow_to(count * std::mem::size_of::<(u32, u32)>()) {
            return primary_only(snapshot, indices, &restore, cancelled);
        }
        let mut targets = [u32::MAX; SMALL_SELECTION];
        let mut used = 0;
        for &(start, end) in &restore.selected {
            for row in start..=end {
                if cancelled() {
                    return None;
                }
                let Some(old) = old_row(&restore, row as usize) else {
                    return primary_only(snapshot, indices, &restore, cancelled);
                };
                if let Some(index) = find_identity(snapshot, old) {
                    targets[used] = index;
                    used += 1;
                }
            }
        }
        targets[..used].sort_unstable();
        if used == 0 && focused_index.is_none() {
            return Some(RestoredView::default());
        }
        let mut selected = Vec::with_capacity(count);
        let mut focused = None;
        for (row, &index) in indices.iter().enumerate() {
            if row % 256 == 0 && cancelled() {
                return None;
            }
            if Some(index) == focused_index {
                focused = Some(row);
            }
            if targets[..used].binary_search(&index).is_ok() && !push_row(&mut selected, row) {
                return primary_only(snapshot, indices, &restore, cancelled);
            }
        }
        if cancelled() {
            return None;
        }
        return Some(RestoredView {
            selected,
            focused,
            truncated: false,
        });
    }

    let old_len = restore.snapshot.entries.len();
    let new_len = snapshot.entries.len();
    let reply_capacity = count.min(MAX_RESTORED_RANGES);
    let bytes = Bits::bytes(old_len)
        .saturating_add(Bits::bytes(new_len))
        .saturating_add(reply_capacity * std::mem::size_of::<(u32, u32)>());
    if !workspace.grow_to(bytes) {
        return primary_only(snapshot, indices, &restore, cancelled);
    }
    let mut old_selected = Bits::new(old_len);
    let mut new_selected = Bits::new(new_len);
    let mut visited = 0_usize;
    for &(start, end) in &restore.selected {
        for row in start..=end {
            if visited % 256 == 0 && cancelled() {
                return None;
            }
            visited += 1;
            let index = restore.indices[row as usize] as usize;
            if index >= old_len {
                return primary_only(snapshot, indices, &restore, cancelled);
            }
            old_selected.insert(index);
        }
    }
    let mut old_order = IdentityOrder::new(&restore.snapshot);
    let mut new_order = IdentityOrder::new(snapshot);
    let mut old_index = old_order.next();
    let mut new_index = new_order.next();
    visited = 0;
    while let (Some(old), Some(new)) = (old_index, new_index) {
        if visited % 256 == 0 && cancelled() {
            return None;
        }
        visited += 1;
        let old_entry = &restore.snapshot.entries[old as usize];
        let new_entry = &snapshot.entries[new as usize];
        match identity_cmp(old_entry, new_entry) {
            Ordering::Less => old_index = old_order.next(),
            Ordering::Greater => new_index = new_order.next(),
            Ordering::Equal => {
                if same_identity(old_entry, new_entry) && old_selected.contains(old as usize) {
                    new_selected.insert(new as usize);
                }
                old_index = old_order.next();
                new_index = new_order.next();
            }
        }
    }
    let mut selected = Vec::with_capacity(reply_capacity);
    let mut focused = None;
    for (row, &index) in indices.iter().enumerate() {
        if row % 256 == 0 && cancelled() {
            return None;
        }
        if Some(index) == focused_index {
            focused = Some(row);
        }
        if new_selected.contains(index as usize) && !push_row(&mut selected, row) {
            return primary_only(snapshot, indices, &restore, cancelled);
        }
    }
    if cancelled() {
        return None;
    }
    Some(RestoredView {
        selected,
        focused,
        truncated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter_snapshot;
    use std::{cell::Cell, collections::BTreeSet, path::PathBuf};

    fn model(entries: Vec<Entry>) -> Arc<Snapshot> {
        Arc::new(Snapshot::from_entries(
            PathBuf::from("owned-restore-fixture"),
            entries,
        ))
    }
    fn names(names: &[&str]) -> Arc<Snapshot> {
        model(
            names
                .iter()
                .map(|name| Entry::from_name(*name, false, 0, 0))
                .collect(),
        )
    }
    fn query() -> Query {
        Query {
            show_hidden: true,
            ..Query::default()
        }
    }
    fn request(
        old: &Arc<Snapshot>,
        query: &Query,
        selected: Vec<(u32, u32)>,
        focused: Option<usize>,
    ) -> ViewRestore {
        ViewRestore {
            snapshot: old.clone(),
            indices: filter_snapshot(old.clone(), query).indices,
            selected,
            focused,
            target: None,
        }
    }
    fn selected_names(
        snapshot: &Snapshot,
        indices: &[u32],
        result: &RestoredView,
    ) -> BTreeSet<String> {
        result
            .selected
            .iter()
            .flat_map(|&(start, end)| start..=end)
            .map(|row| {
                snapshot.entries[indices[row as usize] as usize]
                    .name
                    .clone()
            })
            .collect()
    }
    fn apply(
        new: Arc<Snapshot>,
        query: &Query,
        restore: ViewRestore,
    ) -> (Arc<[u32]>, RestoredView) {
        let view = filter_snapshot(new.clone(), query);
        let result = restore_cancelled(
            &new,
            &view.indices,
            query,
            restore,
            &MemoryBudget::new(8 * 1024 * 1024),
            &|| false,
        )
        .unwrap();
        (view.indices, result)
    }

    #[test]
    fn insertion_deletion_and_rename_restore_identities_instead_of_old_rows() {
        let old = names(&["alpha", "beta", "charlie"]);
        let new = names(&["aardvark", "alpha", "charlie", "renamed-beta"]);
        let (indices, result) = apply(
            new.clone(),
            &query(),
            request(&old, &query(), vec![(0, 2)], Some(1)),
        );
        assert_eq!(
            selected_names(&new, &indices, &result),
            BTreeSet::from(["alpha".into(), "charlie".into()])
        );
        assert_eq!(result.focused, None);
        assert!(!result.truncated);
        let (_, result) = apply(
            new,
            &query(),
            request(&old, &query(), vec![(2, 2)], Some(2)),
        );
        assert_eq!(result.selected, vec![(2, 2)]);
        assert_eq!(result.focused, Some(2));
    }

    #[test]
    fn size_sort_and_filter_restore_against_new_values_and_visible_rows() {
        let old = model(vec![
            Entry::from_name("keep-a", false, 1, 0),
            Entry::from_name("keep-b", false, 2, 0),
            Entry::from_name("keep-c", false, 3, 0),
        ]);
        let new = model(vec![
            Entry::from_name("keep-a", false, 20, 0),
            Entry::from_name("keep-b", false, 2, 0),
            Entry::from_name("keep-c", false, 3, 0),
            Entry::from_name("keep-new", false, 100, 0),
            Entry::from_name("other", false, 200, 0),
        ]);
        let query = Query {
            text: "keep".into(),
            sort: crate::SortColumn::Size,
            descending: true,
            ..query()
        };
        let (indices, result) = apply(
            new.clone(),
            &query,
            request(&old, &query, vec![(0, 1)], Some(0)),
        );
        assert_eq!(
            selected_names(&new, &indices, &result),
            BTreeSet::from(["keep-b".into(), "keep-c".into()])
        );
        assert_eq!(
            new.entries[indices[result.focused.unwrap()] as usize].name,
            "keep-c"
        );
    }

    #[test]
    fn small_target_and_primary_restore_do_not_assume_view_sort_order() {
        let old = names(&["a", "b", "c", "d"]);
        let new = names(&["a", "b", "c", "d", "new"]);
        // Deliberately unrelated to Query.sort, as with fuzzy relevance ranking.
        let permutation = [3, 1, 4, 0, 2];
        let result = restore_cancelled(
            &new,
            &permutation,
            &query(),
            request(&old, &query(), vec![(0, 1)], Some(0)),
            &MemoryBudget::new(1024 * 1024),
            &|| false,
        )
        .unwrap();
        assert_eq!(result.selected, vec![(1, 1), (3, 3)]);
        assert_eq!(result.focused, Some(3));
        let mut target = request(&old, &query(), Vec::new(), None);
        target.target = Some("c".into());
        let result = restore_cancelled(
            &new,
            &permutation,
            &query(),
            target,
            &MemoryBudget::new(0),
            &|| false,
        )
        .unwrap();
        assert_eq!(result.selected, vec![(4, 4)]);
        assert_eq!(result.focused, Some(4));
        let result = restore_cancelled(
            &new,
            &permutation,
            &query(),
            request(&old, &query(), vec![(0, 3)], Some(1)),
            &MemoryBudget::new(0),
            &|| false,
        )
        .unwrap();
        assert_eq!(result.selected, vec![(1, 1)]);
        assert_eq!(result.focused, Some(1));
        assert!(result.truncated);
    }

    #[test]
    fn large_restore_is_independent_of_ranked_view_permutation() {
        let old = model(
            (0..100)
                .map(|n| Entry::from_name(format!("item{n}"), false, 0, 0))
                .collect(),
        );
        let new = model(
            (0..101)
                .map(|n| Entry::from_name(format!("item{n}"), false, 0, 0))
                .collect(),
        );
        let permutation: Vec<u32> = (0..101).map(|n| (n * 37) % 101).collect();
        let result = restore_cancelled(
            &new,
            &permutation,
            &query(),
            request(&old, &query(), vec![(0, 99)], Some(70)),
            &MemoryBudget::new(1024 * 1024),
            &|| false,
        )
        .unwrap();
        let selected = selected_names(&new, &permutation, &result);
        assert_eq!(selected.len(), 100);
        assert!(!selected.contains("item100"));
        assert_eq!(
            result.focused,
            permutation.iter().position(|&index| index == 70)
        );
        assert!(!result.truncated);
    }

    #[test]
    fn large_merge_handles_folder_group_changes_and_excludes_new_files() {
        let old = model(
            (0..200)
                .map(|n| Entry::from_name(format!("item{n}"), n % 3 == 0, n, n as i64))
                .collect(),
        );
        let new = model(
            (0..200)
                .filter(|n| *n != 50)
                .map(|n| {
                    Entry::from_name(
                        format!("item{n}"),
                        if n == 69 { false } else { n % 3 == 0 },
                        500 - n,
                        n as i64,
                    )
                })
                .chain([Entry::from_name("item-new", false, 0, 0)])
                .collect(),
        );
        let query = query();
        let old_view = filter_snapshot(old.clone(), &query);
        let focus = old_view
            .indices
            .iter()
            .position(|&i| old.entries[i as usize].name == "item69")
            .unwrap();
        let (indices, result) = apply(
            new.clone(),
            &query,
            request(&old, &query, vec![(0, 199)], Some(focus)),
        );
        assert!(!result.truncated);
        let selected = selected_names(&new, &indices, &result);
        assert_eq!(selected.len(), 199);
        assert!(!selected.contains("item-new"));
        assert!(!selected.contains("item50"));
        assert_eq!(
            new.entries[indices[result.focused.unwrap()] as usize].name,
            "item69"
        );
    }

    #[test]
    fn recursive_equal_basenames_require_exact_relative_path_identity() {
        let entry = |relative: &str| {
            let mut entry = Entry::from_name("same.txt", false, 1, 0);
            entry.relative_path = Some(PathBuf::from(relative));
            entry
        };
        let mut old_model = Snapshot::from_entries(
            PathBuf::from("owned-restore-fixture"),
            vec![entry("a/same.txt"), entry("b/same.txt")],
        );
        old_model.recursive = true;
        let old = Arc::new(old_model);
        let mut new_model = Snapshot::from_entries(
            old.path.clone(),
            vec![entry("new/same.txt"), entry("b/same.txt")],
        );
        new_model.recursive = true;
        let new = Arc::new(new_model);
        let (indices, result) = apply(
            new.clone(),
            &query(),
            request(&old, &query(), vec![(0, 1)], Some(0)),
        );
        assert_eq!(result.selected.len(), 1);
        let selected_row = result.selected[0].0 as usize;
        assert_eq!(
            new.entries[indices[selected_row] as usize]
                .relative_path
                .as_deref(),
            Some(std::path::Path::new("b/same.txt"))
        );
        assert_eq!(result.focused, None);
        let mut target = request(&old, &query(), Vec::new(), None);
        target.target = Some("same.txt".into());
        let (_, result) = apply(new, &query(), target);
        assert!(
            result.selected.is_empty(),
            "a recursive basename is not a startup identity"
        );
    }

    #[test]
    fn startup_target_resolves_unicode_without_a_previous_view() {
        let new = names(&["before", "日本語-😀.txt", "z-last"]);
        let restore = ViewRestore {
            snapshot: new.clone(),
            indices: Arc::from([]),
            selected: Vec::new(),
            focused: None,
            target: Some("日本語-😀.txt".into()),
        };
        let (indices, result) = apply(new.clone(), &query(), restore);
        assert_eq!(
            selected_names(&new, &indices, &result),
            BTreeSet::from(["日本語-😀.txt".into()])
        );
        assert_eq!(
            result.focused,
            result.selected.first().map(|&(row, _)| row as usize)
        );
    }

    #[cfg(windows)]
    #[test]
    fn startup_target_matches_raw_wide_filename_without_lossy_collisions() {
        use std::os::windows::ffi::OsStringExt;
        let raw = OsString::from_wide(&[b'a' as u16, 0xD800, b'x' as u16]);
        let other = OsString::from_wide(&[b'a' as u16, 0xD801, b'x' as u16]);
        let new = model(vec![
            Entry::from_name(raw.clone(), false, 0, 0),
            Entry::from_name(other, false, 0, 0),
        ]);
        let restore = ViewRestore {
            snapshot: new.clone(),
            indices: Arc::from([]),
            selected: Vec::new(),
            focused: None,
            target: Some(raw.clone()),
        };
        let (indices, result) = apply(new.clone(), &query(), restore);
        assert_eq!(
            new.entries[indices[result.focused.unwrap()] as usize].raw_name,
            raw
        );
        assert_eq!(result.selected.len(), 1);
    }

    #[test]
    fn exhausted_budget_preserves_primary_and_reports_truncation() {
        let old = names(&["a", "b", "c"]);
        let new = names(&["0-new", "a", "b", "c"]);
        let query = query();
        let view = filter_snapshot(new.clone(), &query);
        let result = restore_cancelled(
            &new,
            &view.indices,
            &query,
            request(&old, &query, vec![(0, 2)], Some(1)),
            &MemoryBudget::new(0),
            &|| false,
        )
        .unwrap();
        assert!(result.truncated);
        assert_eq!(result.selected, vec![(2, 2)]);
        assert_eq!(result.focused, Some(2));
    }

    #[test]
    fn million_row_select_all_uses_compact_masks_and_one_interval() {
        let entries = 1_000_000;
        let mut bits = Bits::new(entries);
        assert_eq!(bits.0.len() * 8, 125_000);
        for row in 0..entries {
            bits.insert(row);
        }
        let mut ranges = Vec::with_capacity(1);
        for row in 0..entries {
            if bits.contains(row) {
                assert!(push_row(&mut ranges, row));
            }
        }
        assert_eq!(ranges, vec![(0, 999_999)]);
        assert_eq!(ranges.capacity(), 1);
        assert_eq!(Bits::bytes(entries) * 2, 250_000);
    }

    #[test]
    fn excessive_fragmentation_returns_primary_instead_of_a_huge_ui_reply() {
        let old = model(
            (0..5000)
                .map(|n| Entry::from_name(format!("item{}", n * 2), false, 0, 0))
                .collect(),
        );
        let new = model(
            (0..10000)
                .map(|n| Entry::from_name(format!("item{n}"), false, 0, 0))
                .collect(),
        );
        let (_, result) = apply(
            new,
            &query(),
            request(&old, &query(), vec![(0, 4999)], Some(2222)),
        );
        assert!(result.truncated);
        assert_eq!(result.selected, vec![(4444, 4444)]);
        assert_eq!(result.focused, Some(4444));
    }

    #[test]
    fn cancellation_during_large_restore_releases_charged_workspace() {
        let old = model(
            (0..5000)
                .map(|n| Entry::from_name(format!("item{n}"), false, 0, 0))
                .collect(),
        );
        let new = model(
            (0..5000)
                .map(|n| Entry::from_name(format!("item{n}"), false, 0, 0))
                .collect(),
        );
        let view = filter_snapshot(new.clone(), &query());
        let memory = MemoryBudget::new(8 * 1024 * 1024);
        let calls = Cell::new(0);
        let peak = Cell::new(0);
        let result = restore_cancelled(
            &new,
            &view.indices,
            &query(),
            request(&old, &query(), vec![(0, 4999)], None),
            &memory,
            &|| {
                let count = calls.get() + 1;
                calls.set(count);
                peak.set(peak.get().max(memory.used()));
                count > 25
            },
        );
        assert!(result.is_none());
        assert!(peak.get() > 0);
        assert!(peak.get() < 128 * 1024);
        assert_eq!(memory.used(), 0);
    }

    #[test]
    fn cancellation_during_ranked_row_lookup_releases_workspace() {
        let old = names(&["item0"]);
        let new = model(
            (0..5000)
                .map(|n| Entry::from_name(format!("item{n}"), false, 0, 0))
                .collect(),
        );
        let reversed: Vec<u32> = (0..5000).rev().collect();
        let memory = MemoryBudget::new(1024 * 1024);
        let calls = Cell::new(0);
        let result = restore_cancelled(
            &new,
            &reversed,
            &query(),
            request(&old, &query(), vec![(0, 0)], None),
            &memory,
            &|| {
                calls.set(calls.get() + 1);
                calls.get() > 5
            },
        );
        assert!(result.is_none());
        assert_eq!(memory.used(), 0);
    }
}
