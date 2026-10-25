//! One shared coordinator owns the bounded folder service and visible metadata.
//! All filesystem and full-directory ordering work happens on these workers.
use super::{Column, Entry, EntryRevision, Snapshot, State, ValueKey, remap_rows};
use foldex_folder_stats::{
    FolderRequest, FolderSnapshot, FolderStatsService, MAX_REQUESTS_PER_VIEW,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, UNIX_EPOCH},
};

#[derive(Clone)]
pub(super) struct ViewInput {
    pub priority: bool,
    pub epoch: u64,
    pub revision: u64,
    pub refresh: bool,
    pub presented: Arc<ViewOutput>,
    pub data_revision: u64,
    pub changed_entries: Option<Arc<[OsString]>>,
    pub snapshot: Arc<Snapshot>,
    pub indices: Arc<[u32]>,
    pub visible: Vec<u32>,
    pub folders: bool,
    pub dates: bool,
    pub volume: bool,
    pub parent: bool,
    pub sort: Option<(Column, bool)>,
    pub selection_order: Arc<[u32]>,
    pub selection: Vec<(u32, u32)>,
    pub focused: Option<usize>,
}
impl ViewInput {
    fn same(&self, other: &Self) -> bool {
        self.priority == other.priority
            && self.epoch == other.epoch
            && self.revision == other.revision
            && self.refresh == other.refresh
            && self.data_revision == other.data_revision
            && self.snapshot.id == other.snapshot.id
            && self.visible == other.visible
            && self.folders == other.folders
            && self.dates == other.dates
            && self.volume == other.volume
            && self.parent == other.parent
            && self.sort == other.sort
            && self.selection == other.selection
            && self.focused == other.focused
            && Arc::ptr_eq(&self.selection_order, &other.selection_order)
    }
}
#[derive(Clone, Default)]
pub(super) struct RowValues {
    pub(super) key: Option<ValueKey>,
    pub folder: Option<Arc<FolderSnapshot>>,
    pub created: Option<i64>,
    pub accessed: Option<i64>,
    pub size: Option<u64>,
    pub modified: Option<i64>,
    pub metadata_done: bool,
}
#[derive(Clone, Default)]
pub(super) struct ViewOutput {
    pub epoch: u64,
    pub revision: u64,
    pub data_revision: u64,
    pub sort: Option<(Column, bool)>,
    pub rows: HashMap<u32, RowValues>,
    pub types: Arc<[(String, u64)]>,
    pub parent: Option<Arc<FolderSnapshot>>,
    pub volume_bytes: Option<u64>,
    pub volume_done: bool,
    pub order: Option<Arc<[u32]>>,
    pub remap: Option<SelectionRemap>,
    pub busy: bool,
    pub error: Option<String>,
}
#[derive(Clone)]
pub(super) struct SelectionRemap {
    pub source: Arc<[u32]>,
    pub selected: Vec<(u32, u32)>,
    pub focused: Option<usize>,
    pub mapped: Vec<(u32, u32)>,
    pub mapped_focus: Option<usize>,
}
#[derive(Default)]
struct Slots {
    inputs: HashMap<u64, ViewInput>,
    outputs: HashMap<u64, Arc<ViewOutput>>,
}
struct Shared {
    slots: Mutex<Slots>,
    leases: [AtomicU64; 2],
    priority: [AtomicBool; 2],
    stopped: AtomicBool,
    submissions: AtomicU64,
    invalidations: AtomicU64,
    walks_started: AtomicU64,
    publications: AtomicU64,
}
impl Shared {
    fn active(&self, id: u64) -> bool {
        self.leases
            .iter()
            .any(|slot| slot.load(AtomicOrdering::Acquire) == id.wrapping_add(1))
    }
}
pub(super) struct ColumnService {
    shared: Arc<Shared>,
    thread: Thread,
    _worker: JoinHandle<()>,
}
pub(super) enum Submission {
    Submitted,
    Busy,
    AtCapacity,
}
impl ColumnService {
    pub fn new(ctx: egui::Context) -> Self {
        let shared = Arc::new(Shared {
            slots: Mutex::new(Slots::default()),
            leases: [AtomicU64::new(0), AtomicU64::new(0)],
            priority: [AtomicBool::new(false), AtomicBool::new(false)],
            stopped: AtomicBool::new(false),
            submissions: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
            walks_started: AtomicU64::new(0),
            publications: AtomicU64::new(0),
        });
        let child = shared.clone();
        let worker = thread::Builder::new()
            .name("foldex-columns".into())
            .spawn(move || run(child, ctx))
            .expect("details coordinator thread");
        Self {
            shared,
            thread: worker.thread().clone(),
            _worker: worker,
        }
    }
    pub fn submit(&self, id: u64, input: ViewInput) -> Submission {
        let token = id.wrapping_add(1);
        let mut lease = self
            .shared
            .leases
            .iter()
            .position(|slot| slot.load(AtomicOrdering::Acquire) == token);
        if lease.is_none() {
            lease = self.shared.leases.iter().position(|slot| {
                slot.compare_exchange(0, token, AtomicOrdering::AcqRel, AtomicOrdering::Acquire)
                    .is_ok()
            });
        }
        if lease.is_none() && input.priority {
            for (index, slot) in self.shared.leases.iter().enumerate() {
                if self.shared.priority[index].load(AtomicOrdering::Acquire) {
                    continue;
                }
                let old = slot.load(AtomicOrdering::Acquire);
                if slot
                    .compare_exchange(old, token, AtomicOrdering::AcqRel, AtomicOrdering::Acquire)
                    .is_ok()
                {
                    lease = Some(index);
                    break;
                }
            }
        }
        let Some(lease) = lease else {
            return Submission::AtCapacity;
        };
        self.shared.priority[lease].store(input.priority, AtomicOrdering::Release);
        let Ok(mut slots) = self.shared.slots.try_lock() else {
            self.thread.unpark();
            return Submission::Busy;
        };
        if slots.inputs.get(&id).is_some_and(|old| old.same(&input)) {
            return Submission::Submitted;
        }
        slots.inputs.retain(|id, _| self.shared.active(*id));
        slots.outputs.retain(|id, _| self.shared.active(*id));
        slots.inputs.insert(id, input);
        self.shared
            .submissions
            .fetch_add(1, AtomicOrdering::Relaxed);
        drop(slots);
        self.thread.unpark();
        Submission::Submitted
    }
    pub fn output(&self, id: u64) -> Option<Arc<ViewOutput>> {
        self.shared.slots.try_lock().ok()?.outputs.get(&id).cloned()
    }
    pub(super) fn metrics(&self) -> (u64, u64, u64, u64) {
        (
            self.shared.submissions.load(AtomicOrdering::Relaxed),
            self.shared.invalidations.load(AtomicOrdering::Relaxed),
            self.shared.walks_started.load(AtomicOrdering::Relaxed),
            self.shared.publications.load(AtomicOrdering::Relaxed),
        )
    }
    pub fn cancel(&self, id: u64) -> bool {
        let mut changed = false;
        for slot in &self.shared.leases {
            changed |= slot
                .compare_exchange(
                    id.wrapping_add(1),
                    0,
                    AtomicOrdering::AcqRel,
                    AtomicOrdering::Acquire,
                )
                .is_ok();
        }
        if changed {
            self.thread.unpark();
        }
        true
    }
}
impl Drop for ColumnService {
    fn drop(&mut self) {
        self.shared.stopped.store(true, AtomicOrdering::Release);
        self.thread.unpark();
    }
}

struct Job {
    input: ViewInput,
    output: ViewOutput,
    folders: HashMap<u32, Arc<FolderSnapshot>>,
    requests: Vec<(Option<u32>, FolderRequest)>,
    /// Snapshot-entry coordinates survive filtering without retaining trees or
    /// paths for offscreen rows. Only current sort members count as pending.
    /// Outer None means pending. Inner None means unavailable/not applicable.
    keys: Vec<Option<Option<u64>>>,
    sort_members: Vec<bool>,
    pending_keys: usize,
    sort_next: usize,
    signature: u64,
    invalidate: Option<PathBuf>,
    changed_scopes: Vec<PathBuf>,
    dirty: bool,
}
impl Job {
    fn new(input: ViewInput) -> Self {
        Self::build(input, true, None, None)
    }
    fn build(
        input: ViewInput,
        restore_presented: bool,
        same_snapshot: Option<&Self>,
        retained_keys: Option<Vec<Option<Option<u64>>>>,
    ) -> Self {
        let signature =
            same_snapshot.map_or_else(|| source_signature(&input.snapshot), |job| job.signature);
        let invalidate = input.refresh.then(|| input.snapshot.path.clone());
        let types = same_snapshot.map_or_else(
            || {
                let mut types = BTreeMap::<String, u64>::new();
                // Menu discovery is bounded in retained memory; filtering still
                // applies to every row, including extensions outside this sample.
                for entry in input.snapshot.entries.iter() {
                    if entry.is_dir {
                        continue;
                    }
                    let extension = entry.extension();
                    if types.contains_key(extension) || types.len() < 128 {
                        *types.entry(extension.to_owned()).or_default() += 1;
                    }
                }
                Arc::from(types.into_iter().collect::<Vec<_>>())
            },
            |job| job.output.types.clone(),
        );
        let keys: Vec<Option<Option<u64>>> = if let Some((column, _)) = input.sort {
            retained_keys.unwrap_or_else(|| {
                input
                    .snapshot
                    .entries
                    .iter()
                    .map(|entry| {
                        if entry.is_dir {
                            None
                        } else {
                            Some(
                                if matches!(
                                    column,
                                    Column::Size | Column::VolumeUsage | Column::FolderUsage
                                ) {
                                    Some(entry.size)
                                } else {
                                    None
                                },
                            )
                        }
                    })
                    .collect()
            })
        } else {
            Vec::new()
        };
        let pending_keys = if input.sort.is_some() {
            input
                .indices
                .iter()
                .filter(|index| keys[**index as usize].is_none())
                .count()
        } else {
            0
        };
        let mut sort_members = if input.sort.is_some() {
            vec![false; input.snapshot.entries.len()]
        } else {
            Vec::new()
        };
        for index in input.indices.iter().filter(|_| input.sort.is_some()) {
            sort_members[*index as usize] = true;
        }
        let mut output = ViewOutput {
            epoch: input.epoch,
            revision: input.revision,
            data_revision: input.data_revision,
            sort: input.sort,
            types,
            ..Default::default()
        };
        let mut folders = HashMap::new();
        if !input.refresh && restore_presented {
            output.rows = input.presented.rows.clone();
            remap_rows(&mut output.rows, &input.snapshot, &input.visible);
            for (&index, values) in &output.rows {
                if let Some(stats) = &values.folder {
                    folders.insert(index, stats.clone());
                }
            }
            output.parent = input.presented.parent.clone();
            output.volume_bytes = input.presented.volume_bytes;
            output.volume_done = input.presented.volume_done;
        }
        let mut job = Self {
            input,
            output,
            folders,
            requests: Vec::new(),
            keys,
            sort_members,
            pending_keys,
            sort_next: 0,
            signature,
            invalidate,
            changed_scopes: Vec::new(),
            dirty: true,
        };
        if restore_presented {
            let acknowledged = job.input.presented.data_revision;
            job.apply_filesystem_changes(acknowledged);
        }
        job.restore_keys();
        job
    }
    fn add_changed_scope(&mut self, path: PathBuf) {
        if self.changed_scopes.contains(&path) {
            return;
        }
        if self.changed_scopes.len() < 64 {
            self.changed_scopes.push(path);
        } else if self.changed_scopes.len() == 64 {
            self.invalidate = Some(self.input.snapshot.path.clone());
            self.changed_scopes.clear();
        }
    }
    fn apply_filesystem_changes(&mut self, acknowledged: u64) {
        if self.input.data_revision <= acknowledged {
            return;
        }
        let scopes = match &self.input.changed_entries {
            Some(names) => names
                .iter()
                .take(64)
                .map(|name| self.input.snapshot.path.join(name))
                .collect::<Vec<_>>(),
            None => vec![self.input.snapshot.path.clone()],
        };
        let whole_scope = self.input.changed_entries.is_none();
        if whole_scope {
            self.invalidate = Some(self.input.snapshot.path.clone());
        } else {
            for scope in &scopes {
                self.add_changed_scope(scope.clone());
            }
        }
        self.invalidate_values(&scopes);
    }
    /// A child change also invalidates an ancestor's recursive total. Keep
    /// unrelated completed rows and sort keys; those trees need no new walk.
    fn invalidate_values(&mut self, scopes: &[PathBuf]) {
        if scopes.is_empty() {
            return;
        }
        let mut changed = false;
        self.folders.retain(|index, _| {
            let retain = self
                .input
                .snapshot
                .entries
                .get(*index as usize)
                .is_some_and(|entry| {
                    let path = entry.path(&self.input.snapshot);
                    !scopes.iter().any(|scope| tree_overlaps(&path, scope))
                });
            changed |= !retain;
            retain
        });
        for values in self.output.rows.values_mut() {
            if values
                .key
                .as_ref()
                .is_some_and(|key| scopes.iter().any(|scope| tree_overlaps(&key.path, scope)))
            {
                let key = values.key.take();
                *values = RowValues {
                    key,
                    ..Default::default()
                };
                changed = true;
            }
        }
        let mut invalidated_keys = false;
        for (index, entry) in self
            .input
            .snapshot
            .entries
            .iter()
            .enumerate()
            .filter(|_| self.input.sort.is_some())
        {
            if entry.is_dir && self.keys[index].is_some() {
                let path = entry.path(&self.input.snapshot);
                if scopes.iter().any(|scope| tree_overlaps(&path, scope)) {
                    self.keys[index] = None;
                    invalidated_keys = true;
                }
            }
        }
        if invalidated_keys {
            self.output.order = None;
            self.output.remap = None;
            self.sort_next = 0;
            self.restore_keys();
            changed = true;
        }
        if self.output.parent.is_some()
            && scopes
                .iter()
                .any(|scope| tree_overlaps(&self.input.snapshot.path, scope))
        {
            self.output.parent = None;
            changed = true;
        }
        self.dirty |= changed;
    }
    fn has_invalidations(&self) -> bool {
        self.invalidate.is_some() || !self.changed_scopes.is_empty()
    }
    fn restore_keys(&mut self) {
        let Some((column, _)) = self.input.sort else {
            return;
        };
        self.pending_keys = 0;
        for index in self.input.indices.iter() {
            if self.input.snapshot.entries[*index as usize].is_dir {
                if let Some(stats) = self
                    .folders
                    .get(index)
                    .filter(|stats| stats.state != State::Scanning)
                {
                    self.keys[*index as usize] = Some(statistic_key(column, stats));
                }
                self.pending_keys += usize::from(self.keys[*index as usize].is_none());
            }
        }
    }
    fn update(&mut self, input: ViewInput) {
        if self.input.epoch != input.epoch
            || self.input.revision != input.revision
            || self.input.snapshot.id != input.snapshot.id
            || self.input.sort != input.sort
            || self.input.data_revision != input.data_revision
        {
            let same_epoch = self.input.epoch == input.epoch;
            let same_snapshot = self.input.snapshot.id == input.snapshot.id;
            // Keep compact numeric keys for this snapshot even while a filter
            // hides their rows. Detailed tree snapshots remain viewport-bounded.
            let keep_keys = same_epoch
                && same_snapshot
                && self.input.sort.map(|sort| sort.0) == input.sort.map(|sort| sort.0);
            let retained_keys = keep_keys.then(|| std::mem::take(&mut self.keys));
            let mut next = Self::build(
                input,
                !same_epoch,
                same_snapshot.then_some(&*self),
                retained_keys,
            );
            if same_epoch {
                // Re-publication and query/order revisions are presentation
                // changes. They do not renew the folder service's freshness.
                next.invalidate = self.invalidate.take();
                for path in std::mem::take(&mut self.changed_scopes) {
                    next.add_changed_scope(path);
                }
                let previous_revisions = self
                    .output
                    .rows
                    .values()
                    .filter_map(|values| {
                        values
                            .key
                            .as_ref()
                            .map(|key| (key.path.as_path(), key.revision))
                    })
                    .collect::<HashMap<_, _>>();
                let changed_paths = next
                    .input
                    .visible
                    .iter()
                    .filter_map(|index| {
                        next.input
                            .snapshot
                            .entries
                            .get(*index as usize)
                            .and_then(|entry| {
                                let path = entry.path(&next.input.snapshot);
                                previous_revisions
                                    .get(path.as_path())
                                    .is_some_and(|revision| *revision != EntryRevision::of(entry))
                                    .then_some(path)
                            })
                    })
                    .collect::<Vec<_>>();
                let changed_source = !changed_paths.is_empty()
                    || (!same_snapshot
                        && !next.input.snapshot.recursive
                        && self.input.snapshot.complete
                        && next.input.snapshot.complete
                        && next.signature != self.signature);
                if changed_source {
                    for path in changed_paths {
                        next.add_changed_scope(path);
                    }
                    if next.input.parent {
                        next.invalidate = Some(next.input.snapshot.path.clone());
                    }
                }
                next.output.parent = self.output.parent.clone();
                if changed_source {
                    next.output.parent = None;
                }
                next.output.volume_bytes = self.output.volume_bytes;
                next.output.volume_done = self.output.volume_done;
                if same_snapshot {
                    next.folders = std::mem::take(&mut self.folders);
                    next.output.rows = std::mem::take(&mut self.output.rows);
                } else {
                    let previous = self
                        .folders
                        .iter()
                        .filter_map(|(index, stats)| {
                            self.input
                                .snapshot
                                .entries
                                .get(*index as usize)
                                .map(|entry| {
                                    (ValueKey::of(&self.input.snapshot, entry), stats.clone())
                                })
                        })
                        .collect::<HashMap<_, _>>();
                    for &index in &next.input.visible {
                        if let Some(entry) = next.input.snapshot.entries.get(index as usize) {
                            if let Some(stats) =
                                previous.get(&ValueKey::of(&next.input.snapshot, entry))
                            {
                                next.folders.insert(index, stats.clone());
                            }
                        }
                    }
                    remap_rows(
                        &mut self.output.rows,
                        &next.input.snapshot,
                        &next.input.visible,
                    );
                    next.output.rows = std::mem::take(&mut self.output.rows);
                }
                next.apply_filesystem_changes(self.input.data_revision);
                next.restore_keys();
                if same_snapshot
                    && keep_keys
                    && self.input.sort == next.input.sort
                    && Arc::ptr_eq(&self.input.indices, &next.input.indices)
                    && next.pending_keys == 0
                {
                    next.output.order = self.output.order.clone();
                    next.output.remap = self.output.remap.clone();
                }
            }
            *self = next;
            return;
        }
        if !self.input.same(&input) {
            self.input = input;
            self.dirty = true;
        }
    }
    fn prepare(&mut self) {
        if !self.dirty {
            return;
        }
        let visible: HashSet<u32> = self.input.visible.iter().copied().collect();
        self.output.rows.retain(|index, _| visible.contains(index));
        for &index in &self.input.visible {
            let Some(entry) = self.input.snapshot.entries.get(index as usize) else {
                continue;
            };
            let values = self.output.rows.entry(index).or_default();
            let key = ValueKey::of(&self.input.snapshot, entry);
            if values.key.as_ref() != Some(&key) {
                *values = RowValues {
                    key: Some(key),
                    ..Default::default()
                };
            }
            if (self.input.dates || !entry.metadata_known) && !values.metadata_done {
                // Only visible indexed rows need this fallback. Ordinary directory
                // enumeration already supplied these timestamps with its metadata.
                if !entry.metadata_known
                    || entry.created_secs.is_none()
                    || entry.accessed_secs.is_none()
                {
                    if let Ok(metadata) =
                        std::fs::symlink_metadata(entry.path(&self.input.snapshot))
                    {
                        values.created = metadata.created().ok().and_then(seconds);
                        values.accessed = metadata.accessed().ok().and_then(seconds);
                        values.size = Some(metadata.len());
                        values.modified = metadata.modified().ok().and_then(seconds);
                    }
                }
                values.metadata_done = true;
            }
            values.folder = self.folders.get(&index).cloned();
        }
        if self.input.volume && !self.output.volume_done {
            self.output.volume_bytes = volume_capacity(&self.input.snapshot.path);
            self.output.volume_done = true;
        }
        self.requests.clear();
        if self.input.parent
            && self
                .output
                .parent
                .as_ref()
                .is_none_or(|stats| stats.state == State::Scanning)
        {
            self.requests.push((
                None,
                FolderRequest {
                    path: self.input.snapshot.path.clone(),
                },
            ));
        }
        if self.input.folders {
            for &index in &self.input.visible {
                if self.requests.len() == MAX_REQUESTS_PER_VIEW {
                    break;
                }
                let Some(entry) = self.input.snapshot.entries.get(index as usize) else {
                    continue;
                };
                if self
                    .folders
                    .get(&index)
                    .is_some_and(|stats| stats.state != State::Scanning)
                {
                    continue;
                }
                if entry.is_dir {
                    self.requests.push((
                        Some(index),
                        FolderRequest {
                            path: entry.path(&self.input.snapshot),
                        },
                    ));
                }
            }
        }
        if self.input.sort.is_some() {
            while self.sort_next < self.input.indices.len()
                && self.keys[self.input.indices[self.sort_next] as usize].is_some()
            {
                self.sort_next += 1;
            }
            for index in self.input.indices.iter().skip(self.sort_next) {
                if self.requests.len() == MAX_REQUESTS_PER_VIEW {
                    break;
                }
                if self.keys[*index as usize].is_some()
                    || self
                        .requests
                        .iter()
                        .any(|(value, _)| *value == Some(*index))
                {
                    continue;
                }
                let entry = &self.input.snapshot.entries[*index as usize];
                self.requests.push((
                    Some(*index),
                    FolderRequest {
                        path: entry.path(&self.input.snapshot),
                    },
                ));
            }
        }
        let retained = self
            .requests
            .iter()
            .filter_map(|(index, _)| *index)
            .chain(self.input.visible.iter().copied())
            .collect::<HashSet<_>>();
        self.folders.retain(|index, _| retained.contains(index));
        self.output.busy = self.input.folders
            && self.input.visible.iter().any(|index| {
                self.input
                    .snapshot
                    .entries
                    .get(*index as usize)
                    .is_some_and(|entry| entry.is_dir)
                    && self
                        .folders
                        .get(index)
                        .is_none_or(|value| value.state == State::Scanning)
            })
            || self.requests.iter().any(|(index, _)| match index {
                None => self
                    .output
                    .parent
                    .as_ref()
                    .is_none_or(|value| value.state == State::Scanning),
                Some(index) => self
                    .folders
                    .get(index)
                    .is_none_or(|value| value.state == State::Scanning),
            })
            || self.pending_keys > 0;
    }
    fn event(&mut self, path: &Path, stats: Arc<FolderSnapshot>) {
        let mut received = false;
        for (index, request) in &self.requests {
            if request.path != path {
                continue;
            }
            received = true;
            if let Some(index) = index {
                self.folders.insert(*index, stats.clone());
                if let Some(values) = self.output.rows.get_mut(index) {
                    values.folder = Some(stats.clone());
                }
                if stats.state != State::Scanning {
                    if let Some((column, _)) = self.input.sort {
                        if let Some(key) = self.keys.get_mut(*index as usize) {
                            if key.is_none() && self.sort_members[*index as usize] {
                                self.pending_keys = self.pending_keys.saturating_sub(1);
                            }
                            *key = Some(statistic_key(column, &stats));
                        }
                    }
                }
            } else {
                self.output.parent = Some(stats.clone());
            }
        }
        if received {
            self.dirty = true;
        }
    }
    fn finish_sort(&mut self) {
        if self.output.order.is_some() {
            self.remap_selection();
            return;
        }
        if self.input.indices.is_empty() || self.keys.is_empty() || self.pending_keys > 0 {
            return;
        }
        let Some((_, descending)) = self.input.sort else {
            return;
        };
        let mut positions = (0..self.input.indices.len()).collect::<Vec<_>>();
        positions.sort_unstable_by(|&a, &b| {
            let left = &self.input.snapshot.entries[self.input.indices[a] as usize];
            let right = &self.input.snapshot.entries[self.input.indices[b] as usize];
            right
                .is_dir
                .cmp(&left.is_dir)
                .then_with(|| {
                    let l = self.keys[self.input.indices[a] as usize].unwrap();
                    let r = self.keys[self.input.indices[b] as usize].unwrap();
                    match (l, r) {
                        (Some(l), Some(r)) => {
                            if descending {
                                r.cmp(&l)
                            } else {
                                l.cmp(&r)
                            }
                        }
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        _ => Ordering::Equal,
                    }
                })
                .then_with(|| foldex_core::natural_cmp(left.lowercase(), right.lowercase()))
                .then_with(|| left.raw_name.cmp(&right.raw_name))
        });
        self.output.order = Some(Arc::from(
            positions
                .into_iter()
                .map(|at| self.input.indices[at])
                .collect::<Vec<_>>(),
        ));
        self.remap_selection();
        self.dirty = true;
    }
    fn remap_selection(&mut self) {
        if self.output.remap.as_ref().is_some_and(|remap| {
            remap.selected == self.input.selection
                && remap.focused == self.input.focused
                && Arc::ptr_eq(&remap.source, &self.input.selection_order)
        }) {
            return;
        }
        let Some(order) = &self.output.order else {
            return;
        };
        // Once the pane has applied this order, selections already use its row
        // coordinates. In particular Ctrl+A needs only its compact intervals,
        // not an inverse vector and one allocation per selected row.
        if Arc::ptr_eq(order, &self.input.selection_order) {
            let mapped = self
                .input
                .selection
                .iter()
                .filter_map(|&(first, last)| {
                    let maximum = order.len().checked_sub(1)? as u32;
                    let last = last.min(maximum);
                    (first <= last).then_some((first, last))
                })
                .collect();
            self.output.remap = Some(SelectionRemap {
                source: self.input.selection_order.clone(),
                selected: self.input.selection.clone(),
                focused: self.input.focused,
                mapped,
                mapped_focus: self.input.focused.filter(|row| *row < order.len()),
            });
            self.dirty = true;
            return;
        }
        let mut inverse = vec![u32::MAX; self.input.snapshot.entries.len()];
        for (row, index) in order.iter().enumerate() {
            inverse[*index as usize] = row as u32;
        }
        let mut selected = Vec::new();
        for &(first, last) in &self.input.selection {
            for row in first..=last.min(self.input.selection_order.len().saturating_sub(1) as u32) {
                if let Some(index) = self.input.selection_order.get(row as usize) {
                    let mapped = inverse[*index as usize];
                    if mapped != u32::MAX {
                        selected.push(mapped);
                    }
                }
            }
        }
        selected.sort_unstable();
        let mut mapped: Vec<(u32, u32)> = Vec::new();
        for row in selected {
            if let Some((_, end)) = mapped
                .last_mut()
                .filter(|(_, end)| end.saturating_add(1) == row)
            {
                *end = row;
            } else {
                mapped.push((row, row));
            }
        }
        let mapped_focus = self
            .input
            .focused
            .and_then(|row| self.input.selection_order.get(row))
            .map(|index| inverse[*index as usize])
            .filter(|row| *row != u32::MAX)
            .map(|row| row as usize);
        self.output.remap = Some(SelectionRemap {
            source: self.input.selection_order.clone(),
            selected: self.input.selection.clone(),
            focused: self.input.focused,
            mapped,
            mapped_focus,
        });
        self.dirty = true;
    }
}

fn tree_overlaps(root: &Path, changed: &Path) -> bool {
    root.starts_with(changed) || changed.starts_with(root)
}

/// Enumeration order, snapshot IDs and access-time changes are not filesystem
/// content revisions. Compute this only on the background coordinator.
fn source_signature(snapshot: &Snapshot) -> u64 {
    let mut sum = 0u64;
    let mut mixed = 0u64;
    for entry in snapshot.entries.iter() {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        entry.raw_name.hash(&mut hash);
        entry.relative_path.hash(&mut hash);
        EntryRevision::of(entry).hash(&mut hash);
        let value = hash.finish();
        sum = sum.wrapping_add(value);
        mixed ^= value.rotate_left((value & 63) as u32);
    }
    sum ^ mixed.rotate_left(17) ^ snapshot.entries.len() as u64
}

fn statistic_key(column: Column, stats: &FolderSnapshot) -> Option<u64> {
    if stats.state == State::Unavailable {
        return None;
    }
    Some(match column {
        Column::Folders => stats.folders,
        Column::Files => stats.files,
        Column::TotalItems => stats.folders.saturating_add(stats.files),
        _ => stats.bytes,
    })
}

/// Companion identities for the service's 64 path-keyed cached trees. A new
/// lease must not consume an older cached tree under a changed entry revision.
/// Unknown/evicted identities are refreshed conservatively. Storage is bounded
/// to 64 paths and 256 KiB of encoded names (at most 512 KiB of UTF-16 paths).
#[derive(Default)]
struct KnownRevisions {
    paths: HashMap<PathBuf, (u64, u64)>,
    bytes: usize,
    tick: u64,
}
impl KnownRevisions {
    fn matches(&self, path: &Path, revision: u64) -> bool {
        self.paths
            .get(path)
            .is_some_and(|(old, _)| *old == revision)
    }
    fn remember(&mut self, path: &Path, revision: u64) {
        self.tick = self.tick.wrapping_add(1);
        if let Some(value) = self.paths.get_mut(path) {
            *value = (revision, self.tick);
            return;
        }
        let weight = path.as_os_str().as_encoded_bytes().len();
        if weight > 256 * 1024 {
            return;
        }
        while self.paths.len() >= 64 || self.bytes.saturating_add(weight) > 256 * 1024 {
            let Some(oldest) = self
                .paths
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            self.paths.remove(&oldest);
            self.bytes = self
                .bytes
                .saturating_sub(oldest.as_os_str().as_encoded_bytes().len());
        }
        self.bytes += weight;
        self.paths.insert(path.to_path_buf(), (revision, self.tick));
    }
}

fn request_revision(job: &Job, index: Option<u32>) -> u64 {
    if let Some(entry) = index.and_then(|index| job.input.snapshot.entries.get(index as usize)) {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        EntryRevision::of(entry).hash(&mut hash);
        hash.finish()
    } else {
        job.signature
    }
}

fn run(shared: Arc<Shared>, ctx: egui::Context) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL,
        };
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
    let current = thread::current();
    let wake_ctx = ctx.clone();
    let service = FolderStatsService::new(Arc::new(move || {
        current.unpark();
        wake_ctx.request_repaint();
    }));
    let service_error = service.as_ref().err().map(ToString::to_string);
    let mut jobs = HashMap::<u64, Job>::new();
    let mut known_revisions = KnownRevisions::default();
    while !shared.stopped.load(AtomicOrdering::Acquire) {
        let mut retry = false;
        let inputs = {
            let mut slots = shared.slots.lock().unwrap_or_else(|p| p.into_inner());
            slots.inputs.retain(|id, _| shared.active(*id));
            slots.outputs.retain(|id, _| shared.active(*id));
            slots.inputs.clone()
        };
        let removed = jobs
            .keys()
            .filter(|id| !inputs.contains_key(id))
            .copied()
            .collect::<Vec<_>>();
        for id in removed {
            if service
                .as_ref()
                .is_ok_and(|service| service.cancel_view(id).is_err())
            {
                retry = true;
                continue;
            }
            jobs.remove(&id);
        }
        for (id, input) in inputs {
            if !shared.active(id) {
                continue;
            }
            match jobs.get_mut(&id) {
                Some(job) => job.update(input),
                None => {
                    jobs.insert(id, Job::new(input));
                }
            }
        }
        // Both panes share the cache, but also retain their own visible totals
        // and completed numeric sort keys. Invalidate those copies together so
        // a child edit observed by one pane cannot leave another pane's parent
        // total or settled order stale.
        let changed_scopes = jobs
            .iter()
            .filter(|(id, _)| shared.active(**id))
            .flat_map(|(_, job)| job.invalidate.iter().chain(&job.changed_scopes))
            .cloned()
            .collect::<Vec<_>>();
        if !changed_scopes.is_empty() {
            for job in jobs.values_mut() {
                job.invalidate_values(&changed_scopes);
            }
        }
        // New revisions must install their path-to-row request mapping before
        // consuming retained same-epoch events. Otherwise a completed event
        // could be dropped just before an unchanged set_requests submission,
        // which intentionally does not send that same completion twice.
        for (&id, job) in &mut jobs {
            if shared.active(id) {
                job.prepare();
                if let (Ok(service), Some(scope)) = (&service, &job.invalidate) {
                    match service.invalidate_scope(scope) {
                        Ok(()) => {
                            job.invalidate = None;
                            job.changed_scopes.clear();
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            retry = true
                        }
                        Err(error) => {
                            job.output.error = Some(error.to_string());
                            job.output.busy = false;
                            job.invalidate = None;
                        }
                    }
                }
                if job.invalidate.is_none() {
                    if let Ok(service) = &service {
                        while let Some(scope) = job.changed_scopes.last() {
                            match service.invalidate_scope(scope) {
                                Ok(()) => {
                                    job.changed_scopes.pop();
                                }
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    retry = true;
                                    break;
                                }
                                Err(error) => {
                                    job.output.error = Some(error.to_string());
                                    job.changed_scopes.clear();
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Ok(service) = &service {
            for _ in 0..64 {
                let Some(event) = service.try_recv() else {
                    break;
                };
                if let Some(job) = jobs
                    .get_mut(&event.view)
                    .filter(|job| job.input.epoch == event.epoch && !job.has_invalidations())
                {
                    job.event(&event.path, event.stats);
                }
            }
        }
        let mut changed = false;
        for (&id, job) in &mut jobs {
            if !shared.active(id) {
                continue;
            }
            job.prepare();
            job.finish_sort();
            if let Ok(service) = &service {
                if job.has_invalidations() {
                    retry = true;
                    continue;
                }
                if job.dirty {
                    let mut fresh = true;
                    for (index, request) in &job.requests {
                        let revision = request_revision(job, *index);
                        if !known_revisions.matches(&request.path, revision) {
                            match service.invalidate_tree(&request.path) {
                                Ok(()) => {}
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                                    retry = true;
                                    fresh = false;
                                    break;
                                }
                                Err(error) => {
                                    job.output.error = Some(error.to_string());
                                    job.output.busy = false;
                                    break;
                                }
                            }
                        }
                        known_revisions.remember(&request.path, revision);
                    }
                    if !fresh {
                        continue;
                    }
                    let requests = job
                        .requests
                        .iter()
                        .map(|(_, request)| request.clone())
                        .collect::<Vec<_>>();
                    match service.set_requests(id, job.input.epoch, &requests) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            retry = true;
                            continue;
                        }
                        Err(error) => {
                            job.output.error = Some(error.to_string());
                            job.output.busy = false;
                        }
                    }
                }
            } else {
                job.output.error = service_error.clone();
                job.output.busy = false;
            }
            if job.dirty {
                shared
                    .slots
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .outputs
                    .insert(id, Arc::new(job.output.clone()));
                shared.publications.fetch_add(1, AtomicOrdering::Relaxed);
                job.dirty = false;
                changed = true;
            }
        }
        if let Ok(service) = &service {
            let metrics = service.metrics();
            shared
                .walks_started
                .store(metrics.walks_started, AtomicOrdering::Relaxed);
            shared
                .invalidations
                .store(metrics.invalidations, AtomicOrdering::Relaxed);
        }
        if changed {
            ctx.request_repaint();
        }
        if retry {
            thread::park_timeout(Duration::from_millis(16));
        } else {
            thread::park();
        }
    }
    if let Ok(service) = service {
        service.shutdown();
    }
}
fn seconds(time: std::time::SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
}
#[cfg(windows)]
fn volume_capacity(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut total = 0;
    // SAFETY: path is NUL terminated; this worker owns the output and performs
    // the potentially blocking volume query away from frame-facing code.
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            path.as_ptr(),
            std::ptr::null_mut(),
            &mut total,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(total)
}
#[cfg(not(windows))]
fn volume_capacity(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(snapshot: Arc<Snapshot>) -> ViewInput {
        ViewInput {
            priority: true,
            epoch: 1,
            revision: 1,
            refresh: false,
            presented: Arc::default(),
            data_revision: 0,
            changed_entries: Some(Arc::from([])),
            visible: (0..snapshot.entries.len().min(128) as u32).collect(),
            indices: snapshot.default_order.clone(),
            snapshot,
            folders: true,
            dates: false,
            volume: false,
            parent: false,
            sort: None,
            selection_order: Arc::from([]),
            selection: Vec::new(),
            focused: None,
        }
    }
    fn folders(entries: Vec<Entry>, recursive: bool) -> Arc<Snapshot> {
        let mut snapshot = Snapshot::from_entries(PathBuf::from("fixture"), entries);
        snapshot.recursive = recursive;
        Arc::new(snapshot)
    }
    fn complete(job: &mut Job) {
        job.prepare();
        let paths = job
            .requests
            .iter()
            .map(|(_, request)| request.path.clone())
            .collect::<Vec<_>>();
        for (at, path) in paths.into_iter().enumerate() {
            job.event(
                &path,
                Arc::new(FolderSnapshot {
                    files: at as u64 + 3,
                    bytes: at as u64 + 39,
                    state: State::Complete,
                    ..Default::default()
                }),
            );
        }
        job.prepare();
    }
    #[test]
    fn identical_snapshot_publications_and_selection_do_not_restart_statistics() {
        let entries = vec![
            Entry::from_name("a", true, 0, 1),
            Entry::from_name("b", true, 0, 1),
        ];
        let mut job = Job::new(input(folders(entries.clone(), false)));
        complete(&mut job);
        let original = job.folders[&0].clone();
        let mut next = job.input.clone();
        next.snapshot = folders(entries, false);
        next.indices = next.snapshot.default_order.clone();
        next.revision += 1;
        assert_ne!(job.input.snapshot.id, next.snapshot.id);
        job.update(next);
        job.prepare();
        assert!(job.invalidate.is_none());
        assert!(job.requests.is_empty());
        assert!(!job.output.busy);
        assert!(Arc::ptr_eq(
            &original,
            job.output.rows[&0].folder.as_ref().unwrap()
        ));
        let mut selected = job.input.clone();
        selected.selection = vec![(0, 0)];
        selected.focused = Some(0);
        job.update(selected);
        job.prepare();
        assert!(job.requests.is_empty());
        assert!(job.invalidate.is_none());
    }
    #[test]
    fn streamed_reordering_retains_exact_path_values_and_metadata_without_row_aliasing() {
        let mut a = Entry::from_name("same", true, 0, 1);
        a.relative_path = Some(PathBuf::from("left/same"));
        let mut b = a.clone();
        b.relative_path = Some(PathBuf::from("right/same"));
        let mut job = Job::new(input(folders(vec![a.clone(), b.clone()], true)));
        complete(&mut job);
        let left = job.folders[&0].clone();
        let right = job.folders[&1].clone();
        let values = job.output.rows.get_mut(&0).unwrap();
        values.created = Some(123);
        values.accessed = Some(456);
        values.metadata_done = true;
        let mut next = job.input.clone();
        let c = Entry::from_name("genuinely-new", true, 0, 1);
        next.snapshot = folders(vec![b, a, c], true);
        next.visible = vec![0, 1, 2];
        next.revision += 1;
        job.update(next);
        job.prepare();
        assert!(
            job.invalidate.is_none(),
            "appending search results is not a filesystem refresh"
        );
        assert!(Arc::ptr_eq(
            &right,
            job.output.rows[&0].folder.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            &left,
            job.output.rows[&1].folder.as_ref().unwrap()
        ));
        assert_eq!(job.output.rows[&1].created, Some(123));
        assert_eq!(job.output.rows[&1].accessed, Some(456));
        assert!(job.output.rows[&2].folder.is_none());
        assert_eq!(job.requests.len(), 1);
        assert_eq!(job.requests[0].0, Some(2));
    }
    #[test]
    fn changed_streamed_path_revision_rejects_old_values_and_invalidates_in_flight_work() {
        let mut a = Entry::from_name("same", true, 0, 1);
        a.relative_path = Some(PathBuf::from("left/same"));
        let mut job = Job::new(input(folders(vec![a.clone()], true)));
        complete(&mut job);
        a.modified_secs += 1;
        let mut next = job.input.clone();
        next.snapshot = folders(vec![a], true);
        next.revision += 1;
        job.update(next);
        job.prepare();
        assert_eq!(job.changed_scopes, vec![PathBuf::from("fixture/left/same")]);
        assert!(job.output.rows[&0].folder.is_none());
        assert_eq!(job.requests.len(), 1);
    }
    #[test]
    fn real_source_revision_invalidates_scope_and_only_reuses_unchanged_rows() {
        let a = Entry::from_name("a", true, 0, 1);
        let b = Entry::from_name("b", true, 0, 1);
        let mut job = Job::new(input(folders(vec![a.clone(), b.clone()], false)));
        complete(&mut job);
        let retained = job.folders[&1].clone();
        let mut changed = a;
        changed.modified_secs += 1;
        let mut next = job.input.clone();
        next.snapshot = folders(vec![changed, b], false);
        next.revision += 1;
        job.update(next);
        job.prepare();
        assert_eq!(job.changed_scopes, vec![PathBuf::from("fixture/a")]);
        assert!(job.output.rows[&0].folder.is_none());
        assert!(Arc::ptr_eq(
            &retained,
            job.output.rows[&1].folder.as_ref().unwrap()
        ));
        assert_eq!(job.requests.len(), 1);
    }
    #[test]
    fn lease_recreation_preserves_completed_values_and_sort_keys_until_explicit_refresh() {
        let mut job = Job::new(input(folders(
            vec![Entry::from_name("a", true, 0, 1)],
            false,
        )));
        complete(&mut job);
        let mut next = job.input.clone();
        next.presented = Arc::new(job.output.clone());
        next.sort = Some((Column::Files, false));
        let mut recreated = Job::new(next.clone());
        recreated.prepare();
        recreated.finish_sort();
        assert!(recreated.requests.is_empty());
        assert!(recreated.invalidate.is_none());
        assert_eq!(recreated.pending_keys, 0);
        assert!(recreated.output.order.is_some());
        next.refresh = true;
        next.epoch += 1;
        let mut refreshed = Job::new(next);
        refreshed.prepare();
        assert_eq!(refreshed.invalidate.as_deref(), Some(Path::new("fixture")));
        assert!(refreshed.output.rows[&0].folder.is_none());
        assert_eq!(refreshed.requests.len(), 1);
    }
    #[test]
    fn source_revision_ignores_enumeration_order_and_access_time_but_detects_content_changes() {
        let a = Entry::from_name("a", true, 0, 1);
        let b = Entry::from_name("b", false, 39, 2);
        let original = folders(vec![a.clone(), b.clone()], false);
        let mut accessed = a.clone();
        accessed.accessed_secs = Some(123);
        let reordered = folders(vec![b.clone(), accessed], false);
        assert_eq!(source_signature(&original), source_signature(&reordered));
        let mut changed = b;
        changed.size += 1;
        assert_ne!(
            source_signature(&original),
            source_signature(&folders(vec![a, changed], false))
        );
    }
    #[test]
    fn root_file_notifications_preserve_unrelated_completed_and_in_flight_folder_values() {
        let mut job = Job::new(input(folders(
            vec![
                Entry::from_name("folder", true, 0, 1),
                Entry::from_name("NTUSER.DAT", false, 39, 1),
            ],
            false,
        )));
        complete(&mut job);
        let completed = job.folders[&0].clone();
        let mut changed = job.input.clone();
        changed.data_revision = 1;
        changed.changed_entries = Some(Arc::from([OsString::from("NTUSER.DAT")]));
        job.update(changed);
        job.prepare();
        assert!(job.invalidate.is_none());
        assert_eq!(
            job.changed_scopes,
            vec![PathBuf::from("fixture/NTUSER.DAT")]
        );
        assert!(job.requests.is_empty());
        assert!(Arc::ptr_eq(
            &completed,
            job.output.rows[&0].folder.as_ref().unwrap()
        ));
        let scanning = Arc::new(FolderSnapshot {
            files: 12,
            state: State::Scanning,
            ..Default::default()
        });
        job.folders.insert(0, scanning.clone());
        job.output.rows.get_mut(&0).unwrap().folder = Some(scanning.clone());
        let mut changed = job.input.clone();
        changed.data_revision += 1;
        job.update(changed);
        job.prepare();
        assert!(Arc::ptr_eq(
            &scanning,
            job.output.rows[&0].folder.as_ref().unwrap()
        ));
        assert_eq!(job.requests.len(), 1);
        assert!(
            !job.changed_scopes
                .contains(&PathBuf::from("fixture/folder"))
        );
    }
    #[test]
    fn true_child_notification_refreshes_same_model_same_second_without_row_or_layout_revision() {
        let mut job = Job::new(input(folders(
            vec![Entry::from_name("folder", true, 0, 1)],
            false,
        )));
        complete(&mut job);
        let original_model = job.input.snapshot.id;
        let original_layout = job.input.revision;
        let mut changed = job.input.clone();
        changed.data_revision = 1;
        changed.changed_entries = Some(Arc::from([OsString::from("folder")]));
        job.update(changed);
        job.prepare();
        assert_eq!(job.input.snapshot.id, original_model);
        assert_eq!(job.input.revision, original_layout);
        assert_eq!(job.changed_scopes, vec![PathBuf::from("fixture/folder")]);
        assert!(job.output.rows[&0].folder.is_none());
        assert_eq!(job.requests.len(), 1);
        assert_eq!(job.output.data_revision, 1);
    }
    #[test]
    fn lost_watch_coverage_invalidates_the_bounded_scope_conservatively() {
        let mut job = Job::new(input(folders(
            vec![Entry::from_name("folder", true, 0, 1)],
            false,
        )));
        complete(&mut job);
        let mut changed = job.input.clone();
        changed.data_revision = 1;
        changed.changed_entries = None;
        job.update(changed);
        job.prepare();
        assert_eq!(job.invalidate.as_deref(), Some(Path::new("fixture")));
        assert!(job.output.rows[&0].folder.is_none());
        assert_eq!(job.requests.len(), 1);
    }
    #[test]
    fn cached_tree_identity_rejects_new_metadata_revisions_and_stays_bounded() {
        let mut job = Job::new(input(folders(
            vec![Entry::from_name("folder", true, 0, 1)],
            false,
        )));
        job.prepare();
        let path = job.requests[0].1.path.clone();
        let original = request_revision(&job, Some(0));
        let mut known = KnownRevisions::default();
        known.remember(&path, original);
        assert!(known.matches(&path, original));
        let mut changed = job.input.clone();
        changed.snapshot = folders(vec![Entry::from_name("folder", true, 0, 2)], false);
        let revised = Job::new(changed);
        assert!(
            !known.matches(&path, request_revision(&revised, Some(0))),
            "a fresh lease cannot consume a path-only cached tree from older metadata"
        );
        for index in 0..256 {
            known.remember(Path::new(&format!("fixture/folder{index}")), index);
        }
        assert!(known.paths.len() <= 64);
        assert!(known.bytes <= 256 * 1024);
    }
    #[test]
    fn query_publications_do_not_reapply_already_processed_watch_changes_from_an_older_ui_output() {
        let mut request = input(folders(vec![Entry::from_name("folder", true, 0, 1)], false));
        request.data_revision = 1;
        request.changed_entries = Some(Arc::from([OsString::from("folder")]));
        // The UI can still hold revision zero while query/model requests arrive.
        let mut job = Job::new(request);
        job.changed_scopes.clear();
        complete(&mut job);
        let mut next = job.input.clone();
        next.revision += 1;
        job.update(next);
        job.prepare();
        assert!(!job.has_invalidations());
        assert!(job.requests.is_empty());
        assert!(!job.output.busy);
    }
    #[test]
    fn unchanged_submissions_and_cancelling_an_unleased_view_do_no_coordinator_work() {
        let mut job = Job::new(input(folders(
            vec![Entry::from_name("a", true, 0, 1)],
            false,
        )));
        complete(&mut job);
        let mut request = job.input.clone();
        request.presented = Arc::new(job.output.clone());
        let service = ColumnService::new(egui::Context::default());
        assert!(matches!(
            service.submit(1, request.clone()),
            Submission::Submitted
        ));
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while service.output(1).is_none() {
            assert!(std::time::Instant::now() < until);
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(30));
        let before = service.metrics();
        for _ in 0..100 {
            assert!(matches!(
                service.submit(1, request.clone()),
                Submission::Submitted
            ));
            assert!(service.cancel(999));
        }
        thread::sleep(Duration::from_millis(30));
        assert_eq!(service.metrics(), before);
        assert_eq!(
            before.2, 0,
            "completed presented values need no filesystem walks"
        );
    }
    #[test]
    fn completed_visible_values_release_request_slots_for_the_next_wave() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::from("fixture"),
            (0..80)
                .map(|index| Entry::from_name(format!("folder{index}"), true, 0, 0))
                .collect(),
        ));
        let mut job = Job::new(ViewInput {
            priority: true,
            epoch: 1,
            revision: 1,
            refresh: false,
            presented: Arc::default(),
            data_revision: 0,
            changed_entries: Some(Arc::from([])),
            snapshot,
            indices: Arc::from((0..80).collect::<Vec<u32>>()),
            visible: (0..80).collect(),
            folders: true,
            dates: false,
            volume: false,
            parent: false,
            sort: None,
            selection_order: Arc::from([]),
            selection: Vec::new(),
            focused: None,
        });
        for remaining in [80, 48, 16] {
            job.prepare();
            assert_eq!(job.requests.len(), remaining.min(MAX_REQUESTS_PER_VIEW));
            assert!(job.output.busy);
            let paths = job
                .requests
                .iter()
                .map(|(_, request)| request.path.clone())
                .collect::<Vec<_>>();
            for path in paths {
                job.event(
                    &path,
                    Arc::new(FolderSnapshot {
                        files: 3,
                        state: State::Complete,
                        ..Default::default()
                    }),
                );
            }
        }
        job.prepare();
        assert!(job.requests.is_empty());
        assert!(!job.output.busy);
        assert_eq!(job.output.rows.len(), 80);
        assert!(
            job.output
                .rows
                .values()
                .all(|row| row.folder.as_ref().unwrap().files == 3)
        );
        // A new filter/order revision must not await a second completion from
        // an unchanged service subscription: final values were already sent.
        let mut revised = job.input.clone();
        revised.revision += 1;
        revised.sort = Some((Column::Files, true));
        job.update(revised);
        job.prepare();
        assert!(job.requests.is_empty());
        assert_eq!(job.pending_keys, 0);
        job.finish_sort();
        assert!(job.output.order.is_some());
    }
    #[test]
    fn computed_order_uses_all_rows_and_keeps_unavailable_values_last() {
        let snapshot = Arc::new(Snapshot::from_entries(
            PathBuf::from("fixture"),
            vec![
                Entry::from_name("folder2", true, 0, 0),
                Entry::from_name("folder10", true, 0, 0),
                Entry::from_name("unavailable", true, 0, 0),
                Entry::from_name("file", false, 8, 0),
            ],
        ));
        let mut job = Job::new(ViewInput {
            priority: true,
            epoch: 1,
            revision: 1,
            refresh: false,
            presented: Arc::default(),
            data_revision: 0,
            changed_entries: Some(Arc::from([])),
            snapshot,
            indices: Arc::from([0, 1, 2, 3]),
            visible: vec![0],
            folders: true,
            dates: false,
            volume: false,
            parent: false,
            sort: Some((Column::Files, true)),
            selection_order: Arc::from([0, 1, 2, 3]),
            selection: vec![(0, 0)],
            focused: Some(0),
        });
        job.keys = vec![Some(Some(2)), Some(Some(10)), Some(None), Some(None)];
        job.pending_keys = 0;
        job.finish_sort();
        assert_eq!(job.output.order.as_deref().unwrap(), &[1, 0, 2, 3]);
        assert_eq!(job.output.remap.as_ref().unwrap().mapped, vec![(1, 1)]);
    }

    fn settled_offscreen_sort() -> Job {
        let mut request = input(folders(
            (0..96)
                .map(|index| Entry::from_name(format!("folder{index}"), true, 0, 1))
                .collect(),
            false,
        ));
        request.visible = vec![0];
        request.sort = Some((Column::Files, false));
        let mut job = Job::new(request);
        for _ in 0..4 {
            if job.pending_keys == 0 {
                break;
            }
            complete(&mut job);
        }
        assert_eq!(job.pending_keys, 0);
        assert_eq!(
            job.folders.len(),
            1,
            "completed offscreen snapshots stay bounded"
        );
        job.finish_sort();
        job
    }

    #[test]
    fn same_snapshot_filter_and_direction_changes_reuse_offscreen_keys_and_type_discovery() {
        let mut job = settled_offscreen_sort();
        let types = job.output.types.clone();
        let signature = job.signature;
        let original = job.keys.clone();
        let mut filtered = job.input.clone();
        filtered.revision += 1;
        filtered.indices = Arc::from([95, 64, 0]);
        filtered.sort = Some((Column::Files, true));
        job.update(filtered);
        job.prepare();
        assert!(Arc::ptr_eq(&types, &job.output.types));
        assert_eq!(job.signature, signature);
        assert_eq!(job.keys, original);
        assert_eq!(job.pending_keys, 0);
        assert!(
            job.requests.is_empty(),
            "filtering must not repeat the 96 tree walks"
        );
        assert!(!job.output.busy);
        job.finish_sort();
        assert_eq!(job.output.order.as_ref().unwrap().len(), 3);
        let mut unfiltered = job.input.clone();
        unfiltered.revision += 1;
        unfiltered.indices = unfiltered.snapshot.default_order.clone();
        job.update(unfiltered);
        job.prepare();
        assert_eq!(job.pending_keys, 0);
        assert!(
            job.requests.is_empty(),
            "clearing a filter also reuses the hidden rows' keys"
        );
        assert_eq!(job.keys, original);
    }

    #[test]
    fn unrelated_file_changes_preserve_settled_sort_order_and_child_changes_refresh_one_key() {
        let mut job = settled_offscreen_sort();
        let order = job.output.order.clone().unwrap();
        let original = job.keys.clone();
        let mut changed = job.input.clone();
        changed.data_revision += 1;
        changed.changed_entries = Some(Arc::from([OsString::from("session.log")]));
        job.update(changed);
        job.prepare();
        assert!(Arc::ptr_eq(&order, job.output.order.as_ref().unwrap()));
        assert_eq!(job.keys, original);
        assert!(job.requests.is_empty());
        assert!(!job.output.busy);
        job.changed_scopes.clear();

        let mut changed = job.input.clone();
        changed.data_revision += 1;
        changed.changed_entries = Some(Arc::from([OsString::from("folder70")]));
        job.update(changed);
        job.prepare();
        assert_eq!(job.pending_keys, 1);
        assert_eq!(job.requests.len(), 1);
        assert_eq!(job.requests[0].0, Some(70));
        for (index, key) in job.keys.iter().enumerate() {
            if index != 70 {
                assert_eq!(*key, original[index]);
            }
        }
        job.finish_sort();
        assert!(
            job.output.order.is_none(),
            "wait for settled keys before reordering rows"
        );
        let path = job.requests[0].1.path.clone();
        job.event(
            &path,
            Arc::new(FolderSnapshot {
                files: 500,
                state: State::Scanning,
                ..Default::default()
            }),
        );
        job.finish_sort();
        assert_eq!(job.pending_keys, 1);
        assert!(job.output.order.is_none());
        job.event(
            &path,
            Arc::new(FolderSnapshot {
                files: 501,
                state: State::Partial,
                ..Default::default()
            }),
        );
        job.prepare();
        job.finish_sort();
        assert_eq!(job.pending_keys, 0);
        assert_eq!(job.keys[70], Some(Some(501)));
        assert_eq!(job.output.order.as_ref().unwrap().last(), Some(&70));
    }

    #[test]
    fn a_child_edit_invalidates_another_panes_ancestor_values_and_keeps_its_sibling() {
        let mut request = input(folders(
            vec![
                Entry::from_name("ancestor", true, 0, 1),
                Entry::from_name("sibling", true, 0, 1),
            ],
            false,
        ));
        request.parent = true;
        request.sort = Some((Column::Files, false));
        let mut job = Job::new(request);
        complete(&mut job);
        job.finish_sort();
        let sibling = job.output.rows[&1].folder.clone().unwrap();
        job.dirty = false;
        job.invalidate_values(&[PathBuf::from("fixture/ancestor/deep/changed-file")]);
        job.prepare();
        assert!(job.dirty);
        assert!(job.output.parent.is_none());
        assert!(job.output.rows[&0].folder.is_none());
        assert!(Arc::ptr_eq(
            &sibling,
            job.output.rows[&1].folder.as_ref().unwrap()
        ));
        assert_eq!(job.pending_keys, 1);
        assert!(job.output.order.is_none());
        assert_eq!(
            job.requests.len(),
            2,
            "refresh the ancestor and denominator, not its sibling"
        );
        assert!(job.requests.iter().any(|(index, _)| *index == Some(0)));
        assert!(job.requests.iter().any(|(index, _)| index.is_none()));
    }

    #[test]
    fn selections_in_the_applied_sort_order_keep_compact_intervals_and_focus() {
        let mut job = settled_offscreen_sort();
        let order = job.output.order.clone().unwrap();
        let mut selection = job.input.clone();
        selection.selection_order = order.clone();
        selection.selection = vec![(0, 95)];
        selection.focused = Some(70);
        job.update(selection);
        job.finish_sort();
        let remap = job.output.remap.as_ref().unwrap();
        assert!(Arc::ptr_eq(&order, &remap.source));
        assert_eq!(remap.mapped, vec![(0, 95)]);
        assert_eq!(remap.mapped_focus, Some(70));
        assert!(Arc::ptr_eq(&order, job.output.order.as_ref().unwrap()));
        let mut selection = job.input.clone();
        selection.selection = vec![(0, 200), (250, 300)];
        selection.focused = Some(200);
        job.update(selection);
        job.finish_sort();
        let remap = job.output.remap.as_ref().unwrap();
        assert_eq!(remap.mapped, vec![(0, 95)]);
        assert_eq!(remap.mapped_focus, None);
    }

    #[test]
    fn hidden_sort_keys_are_invalidated_before_a_filter_is_cleared() {
        let mut job = settled_offscreen_sort();
        let mut filtered = job.input.clone();
        filtered.revision += 1;
        filtered.indices = Arc::from([0, 1]);
        job.update(filtered);
        job.prepare();
        assert_eq!(job.pending_keys, 0);
        let mut changed = job.input.clone();
        changed.data_revision += 1;
        changed.changed_entries = Some(Arc::from([OsString::from("folder70")]));
        job.update(changed);
        job.prepare();
        assert_eq!(job.keys[70], None);
        assert_eq!(
            job.pending_keys, 0,
            "hidden rows do not block the current sort"
        );
        assert!(job.requests.is_empty());
        let mut unfiltered = job.input.clone();
        unfiltered.revision += 1;
        unfiltered.indices = unfiltered.snapshot.default_order.clone();
        job.update(unfiltered);
        job.prepare();
        assert_eq!(job.pending_keys, 1);
        assert_eq!(job.requests.len(), 1);
        assert_eq!(job.requests[0].0, Some(70));
    }

    #[test]
    fn type_discovery_reuses_immutable_snapshots_and_rebuilds_for_new_contents() {
        let a = Entry::from_name("a.txt", false, 1, 1);
        let b = Entry::from_name("b.rs", false, 1, 1);
        let mut job = Job::new(input(folders(vec![a.clone(), b.clone()], false)));
        let types = job.output.types.clone();
        let mut presented = job.input.clone();
        presented.revision += 1;
        job.update(presented);
        assert!(Arc::ptr_eq(&types, &job.output.types));
        let mut changed = job.input.clone();
        changed.snapshot = folders(vec![a, b, Entry::from_name("c.txt", false, 1, 1)], false);
        changed.indices = changed.snapshot.default_order.clone();
        job.update(changed);
        assert!(!Arc::ptr_eq(&types, &job.output.types));
        assert_eq!(
            job.output.types.as_ref(),
            &[("rs".into(), 1), ("txt".into(), 2)]
        );
    }
}
