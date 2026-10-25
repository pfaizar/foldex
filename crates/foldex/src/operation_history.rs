//! Session-local, bounded file-operation history. Only background workers take
//! the journal lock; the frame reads a small, separate cached state.
//!
//! A record contains the inverse of confirmed changes, never guessed paths.
//! Replaying a record produces its inverse on the other stack. Failed steps
//! remain retryable, including a partially completed Shell operation.

use std::{
    collections::VecDeque,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicIsize, Ordering},
    },
};

#[path = "operation_history/identity.rs"]
mod identity;

const MAX_RECORDS: usize = 64;
pub(crate) const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_TREE_ENTRIES: usize = 16_384;
const MAX_TREE_PATH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HistoryState {
    pub undo: Option<&'static str>,
    pub redo: Option<&'static str>,
    pub busy: bool,
}
impl HistoryState {
    pub fn can_undo(self) -> bool {
        !self.busy && self.undo.is_some()
    }
    pub fn can_redo(self) -> bool {
        !self.busy && self.redo.is_some()
    }
    pub fn undo_label(self) -> String {
        self.undo
            .map_or_else(|| "Undo".into(), |label| format!("Undo {label}"))
    }
    pub fn redo_label(self) -> String {
        self.redo
            .map_or_else(|| "Redo".into(), |label| format!("Redo {label}"))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Direction {
    Undo,
    Redo,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    pub volume: u64,
    pub id: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Stamp {
    pub identity: Identity,
    pub directory: bool,
    pub bytes: u64,
    pub modified: u64,
    pub created: u64,
    /// Reparse tag and a hash of its bounded, opaque data (no target traversal).
    pub reparse: Option<(u32, u64)>,
}

#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub path: PathBuf,
    pub parent: Identity,
}
impl Target {
    pub fn capture(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() || path.file_name().is_none() {
            return Err(io::Error::other("Undo requires an absolute, non-root path"));
        }
        let parent = identity::read_parent(
            path.parent()
                .ok_or_else(|| io::Error::other("Missing parent"))?,
        )?;
        if !parent.directory {
            return Err(io::Error::other("The target parent is not a folder"));
        }
        Ok(Self {
            path: path.to_owned(),
            parent: parent.identity,
        })
    }
    pub fn validate(&self) -> io::Result<()> {
        let current = Self::capture(&self.path)?;
        if current.parent != self.parent {
            return Err(io::Error::other(format!(
                "{} changed; nothing was overwritten",
                self.path.parent().unwrap().display()
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub location: Target,
    pub stamp: Stamp,
    // Removal of a copied/new tree requires every descendant to remain the
    // same object and retain its metadata. Directory links are opaque.
    contents: Option<Vec<(PathBuf, Stamp)>>,
}
impl Item {
    pub fn protects_contents(&self) -> bool {
        self.contents.is_some()
    }
    pub fn capture(path: &Path, protect_contents: bool) -> io::Result<Self> {
        Ok(Self {
            location: Target::capture(path)?,
            stamp: identity::read(path)?,
            contents: if protect_contents {
                Some(snapshot(path)?)
            } else {
                None
            },
        })
    }
    pub fn validate(&self) -> io::Result<()> {
        self.location.validate()?;
        let current = identity::read(&self.location.path)?;
        let link_changed = (opaque_link(&current) || opaque_link(&self.stamp))
            && current.reparse != self.stamp.reparse;
        if current.identity != self.stamp.identity
            || current.directory != self.stamp.directory
            || link_changed
        {
            return Err(io::Error::other(format!(
                "{} was moved or replaced; the history item was kept for retry",
                self.location.path.display()
            )));
        }
        #[cfg(windows)]
        if current.created != self.stamp.created {
            return Err(io::Error::other(
                "The recorded file identity was reused; the replacement was left unchanged",
            ));
        }
        if let Some(contents) = &self.contents {
            if &snapshot(&self.location.path)? != contents {
                return Err(io::Error::other(format!(
                    "{} changed after it was created; Undo will not remove edited content",
                    self.location.path.display()
                )));
            }
        }
        Ok(())
    }
    pub(crate) fn bytes(&self) -> usize {
        self.location.path.as_os_str().len().saturating_mul(2)
            + 160
            + self.contents.as_ref().map_or(0, |entries| {
                entries
                    .iter()
                    .map(|(path, _)| path.as_os_str().len().saturating_mul(2) + 128)
                    .sum::<usize>()
            })
    }
}

fn snapshot(root: &Path) -> io::Result<Vec<(PathBuf, Stamp)>> {
    let mut pending = vec![PathBuf::new()];
    let mut entries = Vec::new();
    let mut bytes = 0usize;
    while let Some(relative) = pending.pop() {
        if entries.len().saturating_add(pending.len()) >= MAX_TREE_ENTRIES {
            return Err(io::Error::other(
                "This tree exceeds the bounded Undo entry limit",
            ));
        }
        // Path::join("") appends a separator. A regular-file snapshot must
        // inspect the file itself, rather than accidentally address it as a
        // directory (which fails on Windows).
        let path = if relative.as_os_str().is_empty() {
            root.to_owned()
        } else {
            root.join(&relative)
        };
        let mut stamp = identity::read(&path)?;
        if stamp.directory && !opaque_link(&stamp) {
            for child in fs::read_dir(&path)? {
                if entries.len().saturating_add(pending.len()) >= MAX_TREE_ENTRIES {
                    return Err(io::Error::other(
                        "This tree exceeds the bounded Undo entry limit",
                    ));
                }
                let next = relative.join(child?.file_name());
                bytes = bytes.saturating_add(next.as_os_str().len().saturating_mul(2));
                if bytes > MAX_TREE_PATH_BYTES {
                    return Err(io::Error::other(
                        "This tree exceeds the bounded Undo path limit",
                    ));
                }
                pending.push(next);
            }
        }
        // Hydrating a cloud placeholder alone does not alter file contents or
        // name redirection. Keep link-target metadata for opaque links only.
        if !opaque_link(&stamp) {
            stamp.reparse = None;
        }
        entries.push((relative, stamp));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

fn opaque_link(stamp: &Stamp) -> bool {
    // IO_REPARSE_TAG_NAME_SURROGATE: junction/symlink name redirection. Cloud
    // directories remain traversable trees even when they have a reparse tag.
    stamp.reparse.is_some_and(|(tag, _)| {
        foldex_fs::capabilities::Capabilities::new(foldex_fs::capabilities::REPARSE, Some(tag))
            .redirects()
    })
}

#[derive(Clone, Debug)]
pub(crate) struct RecycleToken {
    pub pidl: Vec<u8>,
}

#[derive(Clone)]
pub(crate) enum Step {
    Move {
        item: Item,
        target: Target,
    },
    Recycle {
        item: Item,
    },
    Restore {
        item: Item,
        token: RecycleToken,
        target: Target,
        protect_contents: bool,
    },
    Batch(crate::batch::HistoryPlan),
}
impl Step {
    pub(crate) fn bytes(&self) -> usize {
        match self {
            Self::Move { item, target } => item.bytes() + target.path.as_os_str().len() * 2 + 64,
            Self::Recycle { item } => item.bytes(),
            Self::Restore {
                item,
                token,
                target,
                ..
            } => item.bytes() + token.pidl.len() + target.path.as_os_str().len() * 2 + 64,
            Self::Batch(plan) => plan.bytes(),
        }
    }
    fn replay(self, owner: isize, stop: &Arc<AtomicBool>) -> Replay {
        match self {
            Self::Batch(plan) => plan.replay(),
            step => {
                #[cfg(windows)]
                {
                    crate::platform::windows_ops::replay_step(step, owner, stop.clone())
                }
                #[cfg(not(windows))]
                {
                    let _ = (owner, stop);
                    Replay::failed(step, "Native file Undo requires Windows".into())
                }
            }
        }
    }
}

pub(crate) struct Replay {
    pub inverse: Option<Step>,
    pub retry: Option<Step>,
    pub error: Option<String>,
}
impl Replay {
    #[cfg(test)]
    pub fn completed(inverse: Step) -> Self {
        Self {
            inverse: Some(inverse),
            retry: None,
            error: None,
        }
    }
    pub fn failed(step: Step, error: String) -> Self {
        Self {
            inverse: None,
            retry: Some(step),
            error: Some(error),
        }
    }
}

pub(crate) struct Mutation {
    pub result: io::Result<String>,
    pub steps: Vec<Step>,
    pub changed: bool,
}
impl Mutation {
    pub fn failed(error: io::Error) -> Self {
        Self {
            result: Err(error),
            steps: vec![],
            changed: false,
        }
    }
    pub fn created(result: io::Result<String>, path: &Path) -> Self {
        let changed = result.is_ok();
        let mut mutation = Self {
            result,
            steps: vec![],
            changed,
        };
        if changed {
            match Item::capture(path, true) {
                Ok(item) => mutation.steps.push(Step::Recycle { item }),
                Err(error) => mutation.note(format!("Undo is unavailable for this item: {error}")),
            }
        }
        mutation
    }
    pub fn note(&mut self, warning: String) {
        match &mut self.result {
            Ok(message) => {
                message.push_str(". ");
                message.push_str(&warning);
            }
            Err(error) => {
                self.result = Err(io::Error::other(format!("{error}. {warning}")));
            }
        }
    }
}

struct Record {
    label: &'static str,
    steps: Vec<Step>,
    bytes: usize,
}
impl Record {
    fn new(label: &'static str, steps: Vec<Step>) -> Self {
        let bytes = steps.iter().map(Step::bytes).sum();
        Self {
            label,
            steps,
            bytes,
        }
    }
}
#[derive(Default)]
struct Journal {
    undo: VecDeque<Record>,
    redo: VecDeque<Record>,
}
impl Journal {
    fn state(&self, busy: bool) -> HistoryState {
        HistoryState {
            undo: self.undo.back().map(|r| r.label),
            redo: self.redo.back().map(|r| r.label),
            busy,
        }
    }
    fn trim(&mut self, protected: Option<Direction>) {
        while self.undo.len() + self.redo.len() > MAX_RECORDS
            || self
                .undo
                .iter()
                .chain(&self.redo)
                .map(|r| r.bytes)
                .sum::<usize>()
                > MAX_BYTES
        {
            if !self.undo.is_empty()
                && !(matches!(protected, Some(Direction::Undo)) && self.undo.len() == 1)
            {
                self.undo.pop_front();
            } else if !self.redo.is_empty()
                && !(matches!(protected, Some(Direction::Redo)) && self.redo.len() == 1)
            {
                self.redo.pop_front();
            } else {
                break; // The protected retry record is individually bounded.
            }
        }
    }
    fn record(&mut self, label: &'static str, mutation: &mut Mutation) {
        if !mutation.changed {
            return;
        }
        self.redo.clear();
        if !mutation.steps.is_empty() {
            let label = if matches!(label, "Transfer" | "Paste") {
                if mutation
                    .steps
                    .iter()
                    .all(|step| matches!(step, Step::Move { .. }))
                {
                    "Move"
                } else if mutation
                    .steps
                    .iter()
                    .all(|step| matches!(step, Step::Recycle { .. }))
                {
                    "Copy"
                } else {
                    "Transfer"
                }
            } else {
                label
            };
            if mutation.steps.iter().map(Step::bytes).sum::<usize>() > MAX_BYTES {
                mutation.steps.clear();
                mutation.note(
                    "Undo is unavailable because this change exceeds the 16 MiB journal budget"
                        .into(),
                );
                return;
            }
            mutation.steps.reverse();
            self.undo
                .push_back(Record::new(label, std::mem::take(&mut mutation.steps)));
            self.trim(None);
        }
    }
    fn replay(
        &mut self,
        direction: Direction,
        mut replay: impl FnMut(Step) -> Replay,
    ) -> io::Result<String> {
        let (source, destination, verb) = match direction {
            Direction::Undo => (&mut self.undo, &mut self.redo, "Undid"),
            Direction::Redo => (&mut self.redo, &mut self.undo, "Redid"),
        };
        let Some(record) = source.pop_back() else {
            return Ok("Nothing to undo or redo".into());
        };
        let label = record.label;
        let mut remaining = record.steps.into_iter();
        let mut inverse = Vec::new();
        let mut error = None;
        let mut retry = Vec::new();
        while let Some(step) = remaining.next() {
            let result = replay(step);
            if result.inverse.is_some() {
                crate::platform::operations::changed();
            }
            if let Some(step) = result.inverse {
                inverse.push(step);
            }
            if let Some(step) = result.retry {
                retry.push(step);
            }
            if result.error.is_some() || !retry.is_empty() {
                error = result.error;
                retry.extend(remaining);
                break;
            }
        }
        let complete = inverse.len();
        let retry_count = retry.len();
        if !retry.is_empty() {
            source.push_back(Record::new(label, retry));
        }
        if !inverse.is_empty() {
            inverse.reverse();
            destination.push_back(Record::new(label, inverse));
        }
        self.trim((retry_count != 0).then_some(direction));
        match error {
            Some(error) => {
                let action = match direction {
                    Direction::Undo => "Undo",
                    Direction::Redo => "Redo",
                };
                let progress = if retry_count != 0 {
                    format!(
                        "{complete} completed; {retry_count} remaining change(s) were kept for retry"
                    )
                } else {
                    format!("{complete} reversible change(s) completed")
                };
                Err(io::Error::other(format!(
                    "{action} {label}: {progress}. {error}"
                )))
            }
            None => Ok(format!("{verb} {label}")),
        }
    }
}

struct Shared {
    journal: Mutex<Journal>,
    state: Mutex<HistoryState>,
    owner: Arc<AtomicIsize>,
    stop: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct HistoryHandle(Arc<Shared>);
impl Default for HistoryHandle {
    fn default() -> Self {
        Self::new(
            Arc::new(AtomicIsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        )
    }
}
impl HistoryHandle {
    pub(crate) fn new(owner: Arc<AtomicIsize>, stop: Arc<AtomicBool>) -> Self {
        Self(Arc::new(Shared {
            journal: Mutex::new(Journal::default()),
            state: Mutex::new(HistoryState::default()),
            owner,
            stop,
        }))
    }
    pub fn state(&self) -> HistoryState {
        self.0
            .state
            .try_lock()
            .map(|state| *state)
            .unwrap_or(HistoryState {
                busy: true,
                ..Default::default()
            })
    }
    pub(crate) fn begin(&self) -> Transaction<'_> {
        let journal = self
            .0
            .journal
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *self
            .0
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = journal.state(true);
        Transaction {
            shared: &self.0,
            journal,
        }
    }
    pub(crate) fn mutate(
        &self,
        label: &'static str,
        operation: impl FnOnce() -> Mutation,
    ) -> io::Result<String> {
        let mut transaction = self.begin();
        if crate::platform::operations::cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Cancelled before starting",
            ));
        }
        let mut mutation = operation();
        transaction.record(label, &mut mutation);
        mutation.result
    }
    pub(crate) fn replay(&self, direction: Direction) -> io::Result<String> {
        let mut transaction = self.begin();
        if crate::platform::operations::cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Cancelled before starting",
            ));
        }
        let owner = self.0.owner.load(Ordering::Acquire);
        transaction
            .journal
            .replay(direction, |step| step.replay(owner, &self.0.stop))
    }
}
pub(crate) struct Transaction<'a> {
    shared: &'a Shared,
    journal: MutexGuard<'a, Journal>,
}
impl Transaction<'_> {
    pub fn record(&mut self, label: &'static str, mutation: &mut Mutation) {
        if mutation.changed {
            crate::platform::operations::changed();
        }
        self.journal.record(label, mutation);
    }
}
impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        *self
            .shared
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = self.journal.state(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    fn fake_step(id: u8) -> Step {
        let path = std::env::temp_dir().join(format!("foldex-history-model-{id}"));
        Step::Recycle {
            item: Item {
                location: Target {
                    path,
                    parent: Identity {
                        volume: 1,
                        id: [0; 16],
                    },
                },
                stamp: Stamp {
                    identity: Identity {
                        volume: 1,
                        id: [id; 16],
                    },
                    directory: false,
                    bytes: 1,
                    modified: 1,
                    created: 1,
                    reparse: None,
                },
                contents: None,
            },
        }
    }
    fn mutation(steps: Vec<Step>) -> Mutation {
        Mutation {
            result: Ok(String::new()),
            steps,
            changed: true,
        }
    }

    #[test]
    fn undo_redo_reverse_records_and_new_changes_invalidate_only_redo() {
        let mut journal = Journal::default();
        journal.record("Copy", &mut mutation(vec![fake_step(1), fake_step(2)]));
        assert_eq!(journal.state(false).undo_label(), "Undo Copy");
        assert!(!journal.state(false).can_redo());
        let mut visited = Vec::new();
        journal
            .replay(Direction::Undo, |step| {
                if let Step::Recycle { item } = &step {
                    visited.push(item.stamp.identity.id[0]);
                }
                Replay::completed(step)
            })
            .unwrap();
        assert_eq!(visited, [2, 1]);
        assert!(!journal.state(false).can_undo());
        assert_eq!(journal.state(false).redo_label(), "Redo Copy");
        journal.record(
            "Rename",
            &mut Mutation::failed(io::Error::other("No files changed")),
        );
        assert!(journal.state(false).can_redo());
        journal.record("Rename", &mut mutation(vec![fake_step(3)]));
        assert_eq!(journal.state(false).undo, Some("Rename"));
        assert!(!journal.state(false).can_redo());
    }

    #[test]
    fn partial_undo_keeps_failed_and_unattempted_steps_and_exposes_completed_redo() {
        let mut journal = Journal::default();
        journal.record(
            "Move",
            &mut mutation(vec![fake_step(1), fake_step(2), fake_step(3)]),
        );
        let mut calls = 0;
        let error = journal
            .replay(Direction::Undo, |step| {
                calls += 1;
                if calls == 2 {
                    Replay::failed(step, "Destination occupied".into())
                } else {
                    Replay::completed(step)
                }
            })
            .unwrap_err();
        assert!(error.to_string().contains("kept for retry"));
        assert_eq!(calls, 2);
        assert_eq!(journal.undo.back().unwrap().steps.len(), 2);
        assert_eq!(journal.redo.back().unwrap().steps.len(), 1);
        assert!(journal.state(false).can_undo() && journal.state(false).can_redo());
        journal.replay(Direction::Undo, Replay::completed).unwrap();
        assert!(journal.undo.is_empty());
        assert_eq!(
            journal
                .redo
                .iter()
                .map(|record| record.steps.len())
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn history_limits_evict_old_records_without_retaining_an_oversized_record() {
        let mut journal = Journal::default();
        for id in 0..80 {
            journal.record("Copy", &mut mutation(vec![fake_step(id)]));
        }
        assert_eq!(journal.undo.len(), MAX_RECORDS);
        let Step::Recycle { item } = fake_step(1) else {
            unreachable!()
        };
        let target = item.location.clone();
        let mut oversized = mutation(vec![Step::Restore {
            item,
            token: RecycleToken {
                pidl: vec![0; MAX_BYTES],
            },
            target,
            protect_contents: false,
        }]);
        journal.record("Delete", &mut oversized);
        assert_eq!(journal.undo.len(), MAX_RECORDS);
        assert!(oversized.steps.is_empty());
        assert!(oversized.result.unwrap().contains("journal budget"));
    }

    #[test]
    fn cached_state_remains_available_while_a_worker_owns_the_history() {
        let history = HistoryHandle::default();
        let mut transaction = history.begin();
        assert!(history.state().busy);
        transaction.record("New File", &mut mutation(vec![fake_step(1)]));
        drop(transaction);
        assert!(history.state().can_undo());
        assert_eq!(history.state().undo_label(), "Undo New File");
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "foldex-history-fixture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn created_tree_undo_refuses_edited_or_new_descendants() {
        let fixture = Fixture::new();
        let root = fixture.0.join("copy");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join("nested/child.txt"), b"initial").unwrap();
        let item = Item::capture(&root, true).unwrap();
        assert!(item.validate().is_ok());
        fs::write(
            root.join("nested/child.txt"),
            b"edited content with a different length",
        )
        .unwrap();
        assert!(
            item.validate()
                .unwrap_err()
                .to_string()
                .contains("edited content")
        );
        let item = Item::capture(&root, true).unwrap();
        fs::write(root.join("nested/new.txt"), b"new user content").unwrap();
        assert!(item.validate().is_err());
        assert_eq!(
            fs::read(root.join("nested/new.txt")).unwrap(),
            b"new user content"
        );
    }

    #[test]
    fn created_file_snapshot_uses_the_file_path_and_refuses_subsequent_edits() {
        let fixture = Fixture::new();
        let path = fixture.0.join("created.txt");
        fs::write(&path, b"original").unwrap();
        let item = Item::capture(&path, true).unwrap();
        assert!(item.protects_contents());
        assert!(item.validate().is_ok());
        let entries = item.contents.as_ref().unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].0.as_os_str().is_empty());
        assert!(!entries[0].1.directory);
        fs::write(&path, b"edited content with a different length").unwrap();
        assert!(item.validate().is_err());
        assert_eq!(
            fs::read(&path).unwrap(),
            b"edited content with a different length"
        );
    }

    #[test]
    fn rename_history_allows_content_edits_but_refuses_replacements_and_parent_changes() {
        let fixture = Fixture::new();
        let parent = fixture.0.join("parent");
        fs::create_dir(&parent).unwrap();
        let path = parent.join("a.txt");
        fs::write(&path, b"A").unwrap();
        let item = Item::capture(&path, false).unwrap();
        fs::write(&path, b"edited but same object").unwrap();
        assert!(item.validate().is_ok());
        fs::rename(&path, parent.join("original.txt")).unwrap();
        fs::write(&path, b"replacement").unwrap();
        assert!(item.validate().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"replacement");
        let replacement = Item::capture(&path, false).unwrap();
        fs::rename(&parent, fixture.0.join("saved-parent")).unwrap();
        fs::create_dir(&parent).unwrap();
        fs::write(&path, b"other parent").unwrap();
        assert!(replacement.validate().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"other parent");
    }

    #[test]
    fn junctions_are_opaque_while_cloud_directories_remain_traversable() {
        let Step::Recycle { mut item } = fake_step(1) else {
            unreachable!()
        };
        item.stamp.directory = true;
        item.stamp.reparse = Some((0xa000_0003, 123)); // Mount-point junction.
        assert!(opaque_link(&item.stamp));
        item.stamp.reparse = Some((0x9000_001a, 0)); // Cloud placeholder.
        assert!(!opaque_link(&item.stamp));
    }
}
