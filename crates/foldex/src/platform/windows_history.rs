//! Exact Shell post-event paths and Recycle Bin identities for the app journal.
//! No trash enumeration, guessed $R names, Shell automation or global Undo verb.

use super::*;
use std::{ffi::OsString, fs, os::windows::ffi::OsStringExt};
use windows_sys::Win32::UI::Shell::{ILGetSize, SHCreateItemFromIDList, SHGetIDListFromObject};

const MAX_PIDL_BYTES: usize = 64 * 1024;

struct TaskMem(*mut c_void);
impl Drop for TaskMem {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(self.0) };
    }
}

/// IShellItem::Compare may require the source to still exist. A post-move or
/// post-delete callback can contain a different IShellItem for that now-missing
/// source, so retain its canonical Shell name before queuing the mutation.
#[derive(Clone, Debug)]
pub(super) struct SourceKey(Vec<u16>);
impl SourceKey {
    pub fn new(path: &Path) -> io::Result<Self> {
        let mut units = wide(path)?;
        units.pop();
        for unit in &mut units {
            if *unit == 47 {
                *unit = 92;
            }
        }
        Ok(Self(units))
    }
    pub fn same_spelling(&self, other: &Self) -> bool {
        self.0 == other.0
    }
    #[cfg(test)]
    pub fn diagnostic(&self) -> String {
        String::from_utf16_lossy(&self.0[..self.0.len().min(512)])
    }
}
impl Ord for SourceKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use windows_sys::Win32::Globalization::CompareStringOrdinal;
        match unsafe {
            CompareStringOrdinal(
                self.0.as_ptr(),
                self.0.len() as i32,
                other.0.as_ptr(),
                other.0.len() as i32,
                1,
            )
        } {
            1 => std::cmp::Ordering::Less,
            2 => std::cmp::Ordering::Equal,
            3 => std::cmp::Ordering::Greater,
            _ => self.0.cmp(&other.0),
        }
    }
}
impl PartialOrd for SourceKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for SourceKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for SourceKey {}

pub(super) fn source_key(item: *mut c_void) -> io::Result<SourceKey> {
    SourceKey::new(&filesystem_path(item)?)
}

fn filesystem_path(item: *mut c_void) -> io::Result<PathBuf> {
    if item.is_null() {
        return Err(invalid("Windows did not return the resulting item"));
    }
    let vtable = unsafe { &**item.cast::<*const ShellItemVtable>() };
    let mut name = ptr::null_mut();
    let result = unsafe { (vtable.get_display_name)(item, SIGDN_FILESYSPATH, &mut name) };
    let _allocation = TaskMem(name.cast());
    hresult(result, "Read resulting Shell path")?;
    if name.is_null() {
        return Err(invalid("Windows returned an empty Shell path"));
    }
    let length = (0..32_767)
        .find(|offset| unsafe { *name.add(*offset) == 0 })
        .ok_or_else(|| invalid("Resulting Shell path is too long"))?;
    let path = PathBuf::from(OsString::from_wide(unsafe {
        std::slice::from_raw_parts(name, length)
    }));
    wide(&path)?;
    Ok(path)
}

#[derive(Clone, Debug)]
pub(super) struct Created {
    pub path: PathBuf,
    pub recycle: Option<RecycleToken>,
}

pub(super) fn capture_created(item: *mut c_void, recycle: bool) -> io::Result<Created> {
    let path = filesystem_path(item)?;
    let recycle = if recycle {
        let mut pidl = ptr::null_mut();
        let result = unsafe { SHGetIDListFromObject(item, &mut pidl) };
        let _allocation = TaskMem(pidl.cast());
        hresult(result, "Retain exact recycled Shell item")?;
        if pidl.is_null() {
            return Err(invalid("Windows did not return a recycled item identifier"));
        }
        let bytes = unsafe { ILGetSize(pidl) } as usize;
        if !(2..=MAX_PIDL_BYTES).contains(&bytes) {
            return Err(invalid("Recycled item identifier exceeds the Undo budget"));
        }
        let pidl = unsafe { std::slice::from_raw_parts(pidl.cast::<u8>(), bytes) }.to_vec();
        validate_pidl(&pidl)?;
        Some(RecycleToken { pidl })
    } else {
        None
    };
    Ok(Created { path, recycle })
}

fn validate_pidl(bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_PIDL_BYTES {
        return Err(invalid("Recycled item identifier is too large"));
    }
    let mut offset = 0usize;
    while offset + 2 <= bytes.len() {
        let count = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        if count == 0 {
            if offset + 2 == bytes.len() && offset != 0 {
                return Ok(());
            }
            break;
        }
        if count < 2 || count > bytes.len() - offset {
            break;
        }
        offset += count;
    }
    Err(invalid(
        "Recycled item identifier is invalid; no item was restored",
    ))
}

fn recycled_item(token: &RecycleToken, expected: &Item) -> io::Result<ComPointer> {
    validate_pidl(&token.pidl)?;
    // Give the ABI naturally aligned storage, independent of Vec<u8>'s layout.
    let mut aligned = vec![0usize; token.pidl.len().div_ceil(size_of::<usize>())];
    unsafe {
        ptr::copy_nonoverlapping(
            token.pidl.as_ptr(),
            aligned.as_mut_ptr().cast(),
            token.pidl.len(),
        )
    };
    let mut item = ptr::null_mut();
    hresult(
        unsafe { SHCreateItemFromIDList(aligned.as_ptr().cast(), &IID_SHELL_ITEM, &mut item) },
        "Locate exact recycled item",
    )?;
    let item = ComPointer::checked(item, "recycled item")?;
    let resolved = Item::capture(&filesystem_path(item.0)?, false)?;
    if resolved.stamp.identity != expected.stamp.identity
        || resolved.stamp.created != expected.stamp.created
    {
        return Err(invalid(
            "The recycled item was removed or replaced; nothing was restored",
        ));
    }
    Ok(item)
}

pub(super) struct Input {
    original: Option<Item>,
    candidate: PathBuf,
    candidate_existed: bool,
    candidate_is_source: bool,
    candidate_parent: Option<crate::operation_history::Identity>,
    moving: bool,
}
impl Input {
    pub fn new(source: &Path, candidate: PathBuf, moving: bool) -> Self {
        let original = Item::capture(source, false).ok();
        let candidate_stamp = Item::capture(&candidate, false).ok();
        let candidate_is_source = original
            .as_ref()
            .zip(candidate_stamp.as_ref())
            .is_some_and(|(source, target)| source.stamp.identity == target.stamp.identity);
        let candidate_parent = Target::capture(&candidate).ok().map(|target| target.parent);
        // Access-denied/unsupported targets are treated as existing: never
        // mistake an unreadable merge/replacement for a newly created item.
        let candidate_existed = !matches!(fs::symlink_metadata(&candidate), Err(error) if error.kind() == io::ErrorKind::NotFound);
        Self {
            original,
            candidate,
            candidate_existed,
            candidate_is_source,
            candidate_parent,
            moving,
        }
    }
}

pub(super) fn transfer_inputs(
    sources: &[PathBuf],
    destination: &Path,
    moves: &[bool],
) -> Vec<Input> {
    let mut bytes = 0usize;
    sources
        .iter()
        .zip(moves)
        .map(|(path, moving)| {
            bytes = bytes.saturating_add(
                (destination.as_os_str().len()
                    + path.as_os_str().len()
                    + path.file_name().map_or(0, |name| name.len()))
                .saturating_mul(2),
            );
            if bytes > 4 * 1024 * 1024 {
                return Input {
                    original: None,
                    candidate: PathBuf::new(),
                    candidate_existed: true,
                    candidate_is_source: false,
                    candidate_parent: None,
                    moving: *moving,
                };
            }
            Input::new(
                path,
                destination.join(path.file_name().unwrap_or_default()),
                *moving,
            )
        })
        .collect()
}

pub(super) fn mutation(
    report: Report,
    inputs: Vec<Input>,
    action: &str,
    recycling: bool,
) -> Mutation {
    let changed = report.outcomes.contains(&SUCCEEDED);
    let mut mutation = Mutation {
        result: report.message(action),
        steps: Vec::new(),
        changed,
    };
    let mut unavailable = 0;
    let mut first_unavailable = None;
    let mut retained_bytes = 0usize;
    for (index, input) in inputs.into_iter().enumerate() {
        if report.outcomes.get(index) != Some(&SUCCEEDED) {
            continue;
        }
        let build = || -> io::Result<Step> {
            if report.replaced {
                return Err(invalid(
                    "Windows replaced an existing destination during the operation",
                ));
            }
            let original = input
                .original
                .ok_or_else(|| invalid("The original item identity was unavailable"))?;
            let created = report
                .created
                .get(index)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    invalid(
                        report
                            .capture_errors
                            .get(index)
                            .and_then(Option::as_ref)
                            .cloned()
                            .unwrap_or_else(|| {
                                "Windows did not return the actual resulting path".into()
                            }),
                    )
                })?;
            if recycling {
                let token = created
                    .recycle
                    .clone()
                    .ok_or_else(|| invalid("Windows did not return a Recycle Bin identity"))?;
                return Ok(Step::Restore {
                    item: Item::capture(&created.path, false)?,
                    token,
                    target: original.location,
                    protect_contents: false,
                });
            }
            let actual = Item::capture(&created.path, !input.moving)?;
            // A replacement/merge cannot be reversed without the old bytes.
            // A collision-renamed output is a distinct item and is journaled.
            if input.candidate_existed && !input.candidate_is_source {
                let parent = input
                    .candidate_parent
                    .ok_or_else(|| invalid("The existing destination identity is unavailable"))?;
                if actual.location.parent == parent
                    && same_name(
                        created.path.file_name().unwrap_or_default(),
                        input.candidate.file_name().unwrap_or_default(),
                    )
                {
                    return Err(invalid(
                        "The operation replaced or merged an existing destination",
                    ));
                }
            }
            if input.moving {
                Ok(Step::Move {
                    item: actual,
                    target: original.location,
                })
            } else {
                Ok(Step::Recycle { item: actual })
            }
        };
        match build() {
            Ok(step) => {
                let bytes = step.bytes();
                if retained_bytes.saturating_add(bytes) <= crate::operation_history::MAX_BYTES {
                    retained_bytes += bytes;
                    mutation.steps.push(step);
                } else {
                    unavailable += 1;
                    first_unavailable.get_or_insert_with(|| {
                        "The retained Undo data exceeded the journal budget".to_owned()
                    });
                }
            }
            Err(error) => {
                unavailable += 1;
                first_unavailable.get_or_insert_with(|| error.to_string());
            }
        }
    }
    if unavailable != 0 {
        mutation.note(format!("Undo is unavailable for {unavailable} changed item(s) whose previous contents or exact identity could not be retained. {}", first_unavailable.unwrap_or_default()));
    }
    mutation
}

fn same_path(left: &Path, right: &Path) -> bool {
    use windows_sys::Win32::Globalization::CompareStringOrdinal;
    let (Ok(mut left), Ok(mut right)) = (wide(left), wide(right)) else {
        return false;
    };
    for unit in left.iter_mut().chain(right.iter_mut()) {
        if *unit == 47 {
            *unit = 92;
        }
    }
    unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32 - 1,
            right.as_ptr(),
            right.len() as i32 - 1,
            1,
        ) == 2
    }
}

fn same_name(left: &std::ffi::OsStr, right: &std::ffi::OsStr) -> bool {
    use windows_sys::Win32::Globalization::CompareStringOrdinal;
    let left: Vec<_> = left.encode_wide().collect();
    let right: Vec<_> = right.encode_wide().collect();
    unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        ) == 2
    }
}

fn available_destination(target: &Target, source: Option<&Item>) -> io::Result<()> {
    target.validate()?;
    match fs::symlink_metadata(&target.path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_)
            if source.is_some_and(|source| {
                same_path(&source.location.path, &target.path)
                    && Item::capture(&target.path, false)
                        .is_ok_and(|item| item.stamp.identity == source.stamp.identity)
            }) =>
        {
            Ok(())
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} already exists; nothing was overwritten",
                target.path.display()
            ),
        )),
    }
}

pub(super) struct ReplayGuard {
    item: Item,
    target: Option<Target>,
    restoring: bool,
}
impl ReplayGuard {
    pub fn validate(&self) -> io::Result<()> {
        self.item.validate()?;
        if let Some(target) = &self.target {
            available_destination(
                target,
                if self.restoring {
                    None
                } else {
                    Some(&self.item)
                },
            )?;
        }
        Ok(())
    }
}

fn replay_state(stop: Arc<AtomicBool>, guard: ReplayGuard) -> Arc<ProgressState> {
    let mut state = ProgressState::new(1, stop);
    let inner = Arc::get_mut(&mut state).expect("new progress state has one owner");
    inner.replay = true;
    inner.replay_guard = Some(guard);
    state
}

pub(crate) fn replay_step(step: Step, owner: isize, stop: Arc<AtomicBool>) -> Replay {
    let original = step.clone();
    let attempt = || -> io::Result<Replay> {
        check_stop(&stop)?;
        let _apartment = Apartment::new()?;
        let (item, target, protect, mode, token) = match step {
            Step::Move { item, target } => (item, Some(target), false, Mode::Move, None),
            Step::Recycle { item } => {
                let protect = item.protects_contents();
                (item, None, protect, Mode::Recycle, None)
            }
            Step::Restore {
                item,
                target,
                token,
                protect_contents,
            } => (
                item,
                Some(target),
                protect_contents,
                Mode::Move,
                Some(token),
            ),
            Step::Batch(_) => {
                return Err(invalid(
                    "Batch rename must use its identity-checked recovery journal",
                ));
            }
        };
        item.validate()?;
        if let Some(target) = &target {
            available_destination(target, if token.is_none() { Some(&item) } else { None })?;
        }
        let source = match &token {
            Some(token) => recycled_item(token, &item)?,
            None => shell_item(&item.location.path)?,
        };
        let destination = target
            .as_ref()
            .map(|target| shell_item(target.path.parent().unwrap()))
            .transpose()?;
        let name: Option<Vec<_>> = target.as_ref().map(|target| {
            target
                .path
                .file_name()
                .unwrap()
                .encode_wide()
                .chain(Some(0))
                .collect()
        });
        let mode = if mode == Mode::Move
            && token.is_none()
            && target
                .as_ref()
                .is_some_and(|target| target.parent == item.location.parent)
        {
            Mode::Rename
        } else {
            mode
        };
        let state = replay_state(
            stop.clone(),
            ReplayGuard {
                item: item.clone(),
                target: target.clone(),
                restoring: token.is_some(),
            },
        );
        // Re-check after Shell parsing, immediately before queuing mutation.
        item.validate()?;
        if let Some(target) = &target {
            available_destination(target, if token.is_none() { Some(&item) } else { None })?;
        }
        let result = perform_batch(
            &[(0, &source)],
            destination.as_ref(),
            name.as_deref(),
            mode,
            owner,
            &state,
        );
        let (result, aborted, setup_error) = match result {
            Ok((result, aborted)) => (result, aborted, None),
            Err(error) => (0, false, Some(error.to_string())),
        };
        let mut report = report(&state, result, aborted);
        if setup_error.is_some() {
            report.setup_error = setup_error;
        }
        if report.outcomes[0] != SUCCEEDED {
            return Ok(Replay::failed(
                original.clone(),
                report.message("Restored").err().map_or_else(
                    || {
                        "The operation was canceled or skipped; no history item was discarded"
                            .into()
                    },
                    |error| error.to_string(),
                ),
            ));
        }
        let inverse = (|| -> io::Result<Step> {
            let created = report.created[0].as_ref().ok_or_else(|| {
                invalid("Windows completed the change without a reusable resulting item")
            })?;
            let actual = Item::capture(
                &created.path,
                if mode == Mode::Recycle {
                    false
                } else {
                    protect
                },
            )?;
            if mode == Mode::Recycle {
                Ok(Step::Restore {
                    item: actual,
                    token: created.recycle.clone().ok_or_else(|| {
                        invalid("Windows did not provide the exact recycled item")
                    })?,
                    target: item.location.clone(),
                    protect_contents: protect,
                })
            } else if token.is_some() {
                Ok(Step::Recycle { item: actual })
            } else {
                Ok(Step::Move {
                    item: actual,
                    target: item.location.clone(),
                })
            }
        })();
        let inverse = match inverse {
            Ok(inverse) => inverse,
            Err(error) => {
                return Ok(Replay {
                    inverse: None,
                    retry: None,
                    error: Some(format!(
                        "The change completed, but its inverse is unavailable: {error}"
                    )),
                });
            }
        };
        let mut error = report
            .message("Restored")
            .err()
            .map(|error| error.to_string());
        if let Some(target) = target
            && let Some(created) = &report.created[0]
            && !Target::capture(&created.path).is_ok_and(|actual| {
                actual.parent == target.parent
                    && same_name(
                        created.path.file_name().unwrap_or_default(),
                        target.path.file_name().unwrap_or_default(),
                    )
            })
        {
            error = Some(format!(
                "The destination changed during Undo/Redo; Windows preserved the item at {}",
                created.path.display()
            ));
        }
        Ok(Replay {
            inverse: Some(inverse),
            retry: None,
            error,
        })
    };
    match attempt() {
        Ok(result) => result,
        Err(error) => Replay::failed(original, error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "foldex-history-post-events-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::create_dir(path.join("source")).unwrap();
            fs::create_dir(path.join("destination")).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn completed(path: PathBuf) -> Report {
        Report {
            outcomes: vec![SUCCEEDED],
            aborted: false,
            result: 0,
            skipped: false,
            rejected_delete: false,
            created: vec![Some(Created {
                path,
                recycle: None,
            })],
            setup_error: None,
            replaced: false,
            capture_errors: vec![],
        }
    }
    #[test]
    fn recycled_identifiers_require_a_bounded_complete_nonempty_pidl() {
        assert!(validate_pidl(&[4, 0, 9, 9, 0, 0]).is_ok());
        for bytes in [
            vec![],
            vec![0, 0],
            vec![1, 0, 0, 0],
            vec![8, 0, 9, 9, 0, 0],
            vec![4, 0, 9, 9],
            vec![4, 0, 9, 9, 0, 0, 1],
            vec![1; MAX_PIDL_BYTES + 1],
        ] {
            assert!(validate_pidl(&bytes).is_err());
        }
    }

    #[test]
    fn copy_history_records_the_actual_collision_name_and_refuses_replacements() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source/a.txt");
        let candidate = fixture.0.join("destination/a.txt");
        let actual = fixture.0.join("destination/a (2).txt");
        fs::write(&source, b"source").unwrap();
        fs::write(&candidate, b"existing").unwrap();
        let input = Input::new(&source, candidate.clone(), false);
        fs::copy(&source, &actual).unwrap();
        let result = mutation(completed(actual.clone()), vec![input], "Copied", false);
        let Some(Step::Recycle { item }) = result.steps.first() else {
            panic!(
                "The distinct confirmed output must be undoable: {:?}",
                result.result
            );
        };
        assert_eq!(item.location.path, actual);
        assert!(item.protects_contents());
        assert_eq!(fs::read(&candidate).unwrap(), b"existing");

        let input = Input::new(&source, candidate.clone(), false);
        fs::copy(&source, &candidate).unwrap();
        let result = mutation(completed(candidate.clone()), vec![input], "Copied", false);
        assert!(result.steps.is_empty());
        assert!(result.result.unwrap().contains("Undo is unavailable"));
        assert_eq!(fs::read(&candidate).unwrap(), b"source");
    }

    #[test]
    fn occupied_restore_target_is_refused_without_changing_its_contents() {
        let fixture = Fixture::new();
        let path = fixture.0.join("destination/a.txt");
        let target = Target::capture(&path).unwrap();
        fs::write(&path, b"new unrelated content").unwrap();
        assert_eq!(
            available_destination(&target, None).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&path).unwrap(), b"new unrelated content");
    }

    #[test]
    fn unconfirmed_and_late_replacement_events_cannot_create_an_undo_recipe() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source/a.txt");
        let actual = fixture.0.join("destination/a.txt");
        fs::write(&source, b"source").unwrap();
        let input = Input::new(&source, actual.clone(), false);
        fs::copy(&source, &actual).unwrap();
        let mut report = completed(actual.clone());
        report.replaced = true;
        assert!(
            mutation(report, vec![input], "Copied", false)
                .steps
                .is_empty()
        );
        let input = Input::new(&source, actual.clone(), false);
        let mut report = completed(actual);
        report.outcomes[0] = SKIPPED;
        let result = mutation(report, vec![input], "Copied", false);
        assert!(!result.changed && result.steps.is_empty());
    }
}
