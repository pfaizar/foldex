use super::{
    FolderSnapshot, Issues, MAX_DEPTH, MAX_ENTRIES, State, WALK_PATH_BUDGET, native, path_bytes,
};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SLICE_ENTRIES: usize = 256;
const SLICE_TIME: Duration = Duration::from_millis(2);
struct Frame {
    path: PathBuf,
    reader: native::Reader,
    weight: usize,
}
pub(super) struct Walk {
    stack: Vec<Frame>,
    stats: FolderSnapshot,
    path_bytes: usize,
    finished: bool,
}
impl Walk {
    pub fn new(path: &Path) -> Self {
        let mut walk = Self {
            stack: Vec::with_capacity(MAX_DEPTH),
            stats: FolderSnapshot::default(),
            path_bytes: 0,
            finished: false,
        };
        let path = path.to_path_buf();
        let weight = path_bytes(&path);
        if weight > WALK_PATH_BUDGET {
            walk.root_error(
                "Root path exceeds the traversal path budget".into(),
                Issues {
                    path_limited: 1,
                    ..Default::default()
                },
            );
            return walk;
        }
        match native::check_root(&path) {
            Ok(()) => match native::Reader::open(&path) {
                Ok(reader) => {
                    walk.stack.push(Frame {
                        path,
                        reader,
                        weight,
                    });
                    walk.path_bytes = weight;
                }
                Err(error) => {
                    let mut issues = Issues::default();
                    open_error(&error, &mut issues);
                    walk.root_error(error.to_string(), issues);
                }
            },
            Err(error) => walk.root_error(error.message, error.issues),
        }
        walk
    }
    fn root_error(&mut self, error: String, issues: Issues) {
        self.stats.state = State::Unavailable;
        self.stats.issues = issues;
        self.stats.error = Some(error.chars().take(256).collect());
        self.finished = true;
    }
    pub fn advance(&mut self, current: &dyn Fn() -> bool) {
        let start = Instant::now();
        for iteration in 0..SLICE_ENTRIES {
            if self.finished || !current() {
                break;
            }
            if iteration > 0 && start.elapsed() >= SLICE_TIME {
                break;
            }
            if self.stats.visited >= MAX_ENTRIES {
                self.stats.issues.entry_limited += 1;
                self.finish();
                break;
            }
            let Some(frame) = self.stack.last_mut() else {
                self.finish();
                break;
            };
            let entry = match frame.reader.next() {
                Ok(Some(entry)) => entry,
                Ok(None) => {
                    let frame = self.stack.pop().unwrap();
                    self.path_bytes -= frame.weight;
                    continue;
                }
                Err(_) => {
                    self.stats.issues.unreadable += 1;
                    let frame = self.stack.pop().unwrap();
                    self.path_bytes -= frame.weight;
                    continue;
                }
            };
            self.stats.visited += 1;
            if entry.reparse {
                self.stats.issues.skipped_reparse += 1;
                continue;
            }
            if entry.offline {
                self.stats.issues.skipped_offline += 1;
                continue;
            }
            if entry.directory {
                add(&mut self.stats.folders, 1, &mut self.stats.issues);
                if self.stack.len() >= MAX_DEPTH {
                    self.stats.issues.depth_limited += 1;
                    continue;
                }
                let path = self.stack.last().unwrap().path.join(entry.name);
                let weight = path_bytes(&path);
                if weight > WALK_PATH_BUDGET
                    || self.path_bytes.saturating_add(weight) > WALK_PATH_BUDGET
                    || native::invalid_path(&path)
                {
                    self.stats.issues.path_limited += 1;
                    continue;
                }
                match native::Reader::open(&path) {
                    Ok(reader) => {
                        self.stack.push(Frame {
                            path,
                            reader,
                            weight,
                        });
                        self.path_bytes += weight;
                    }
                    Err(error) => open_error(&error, &mut self.stats.issues),
                }
            } else {
                add(&mut self.stats.files, 1, &mut self.stats.issues);
                add(&mut self.stats.bytes, entry.bytes, &mut self.stats.issues);
            }
            if start.elapsed() >= SLICE_TIME {
                break;
            }
        }
        if self.stack.is_empty() && !self.finished {
            self.finish();
        }
    }
    fn finish(&mut self) {
        self.stack.clear();
        self.path_bytes = 0;
        self.finished = true;
        self.stats.state = if self.stats.issues.any() {
            State::Partial
        } else {
            State::Complete
        };
    }
    pub fn finished(&self) -> bool {
        self.finished
    }
    pub fn snapshot(&self) -> &FolderSnapshot {
        &self.stats
    }
    pub fn handles(&self) -> usize {
        self.stack.iter().map(|frame| frame.reader.handles()).sum()
    }
    pub fn path_bytes(&self) -> usize {
        self.path_bytes
    }
}
fn open_error(error: &std::io::Error, issues: &mut Issues) {
    match native::exclusion(error) {
        Some(native::Excluded::Reparse) => issues.skipped_reparse += 1,
        Some(native::Excluded::Offline) => issues.skipped_offline += 1,
        None => issues.unreadable += 1,
    }
}
fn add(value: &mut u64, amount: u64, issues: &mut Issues) {
    match value.checked_add(amount) {
        Some(next) => *value = next,
        None => {
            *value = u64::MAX;
            issues.overflow += 1;
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::cell::Cell;
    #[test]
    fn a_pause_after_reading_a_directory_does_not_drop_its_unscanned_branch() {
        let fixture = crate::tests::Fixture::new();
        let root = fixture.dir("root");
        fixture.dir("root/child");
        fixture.file("root/child/file", 39);
        let mut walk = Walk::new(&root);
        let calls = Cell::new(0);
        walk.advance(&|| {
            let call = calls.get();
            calls.set(call + 1);
            call == 0
        });
        for _ in 0..100 {
            if walk.finished() {
                break;
            }
            walk.advance(&|| true);
        }
        assert!(walk.finished());
        assert_eq!(walk.snapshot().state, State::Complete);
        assert_eq!(
            (
                walk.snapshot().folders,
                walk.snapshot().files,
                walk.snapshot().bytes
            ),
            (1, 1, 39)
        );
    }
    #[test]
    fn exhausted_entry_budget_closes_handles_and_reports_partial() {
        let fixture = crate::tests::Fixture::new();
        let root = fixture.dir("root");
        fixture.file("root/file", 39);
        let mut walk = Walk::new(&root);
        walk.stats.visited = MAX_ENTRIES;
        walk.advance(&|| true);
        assert!(walk.finished());
        assert_eq!(walk.snapshot().state, State::Partial);
        assert_eq!(walk.snapshot().issues.entry_limited, 1);
        assert_eq!(walk.handles(), 0);
        assert_eq!(walk.path_bytes(), 0);
    }
    #[test]
    fn logical_byte_overflow_saturates_and_cannot_be_reported_complete() {
        let mut bytes = u64::MAX - 3;
        let mut issues = Issues::default();
        add(&mut bytes, 39, &mut issues);
        assert_eq!(bytes, u64::MAX);
        assert_eq!(issues.overflow, 1);
        let fixture = crate::tests::Fixture::new();
        let root = fixture.dir("root");
        let mut walk = Walk::new(&root);
        walk.stats.bytes = bytes;
        walk.stats.issues = issues;
        walk.finish();
        assert_eq!(walk.snapshot().state, State::Partial);
    }
    #[test]
    fn tagged_reparse_entry_is_not_descended_even_when_its_named_target_is_readable() {
        let fixture = crate::tests::Fixture::new();
        let root = fixture.dir("root");
        fixture.dir("root/linked");
        fixture.file("root/linked/must-not-be-counted", 4096);
        let mut walk = Walk::new(&root);
        walk.stack[0].reader.inject_reparse_fixture("linked");
        for _ in 0..100 {
            if walk.finished() {
                break;
            }
            walk.advance(&|| true);
        }
        assert!(walk.finished());
        assert_eq!(walk.snapshot().state, State::Partial);
        assert_eq!(walk.snapshot().issues.skipped_reparse, 1);
        assert_eq!(
            (
                walk.snapshot().folders,
                walk.snapshot().files,
                walk.snapshot().bytes
            ),
            (0, 0, 0)
        );
    }
}
