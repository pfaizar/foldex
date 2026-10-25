//! Opt-in generated-file Undo/Redo checks on a parent-owned non-input desktop.
//!
//! Include as a test-only child of `windows_ops.rs`:
//! `#[cfg(test)] #[path = "windows_undo_probe.rs"] mod undo_probe;`
//! The parent supplies a subprocess hard timeout, FOLDEX_NATIVE_UNDO_PROBE=1,
//! and an absolute, fresh FOLDEX_UNDO_PROBE_OUTPUT JSON file. No clipboard,
//! input injection, global Undo verb, trash enumeration, or Empty Recycle Bin.
//!
//! The entire test runs inside the thread-scoped `without_session_undo` helper.
//! Forward fixtures keep normal collision policy and default replay=false; the
//! helper removes only ALLOWUNDO/ADDUNDORECORD. Normal scenarios call the real
//! transfer_tracked/rename_tracked/delete_tracked facades. The partial-cancel
//! scenario also exercises the native operation/sink/mutation core directly.
//! Undo/Redo use HistoryHandle and exact-PIDL replay. This does not test the
//! normal worker queue or native Cancel buttons.
//! An inactive FoldexProbe_ desktop on WinSta0 is accepted only after checking
//! the input desktop is different; its clipboard remains shared and untouched.
//! Flag contract: https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ifileoperation-setoperationflags

use super::*;
use crate::operation_history::{Direction, HistoryHandle};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Component,
    sync::atomic::AtomicIsize,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    System::{
        StationsAndDesktops::{
            CloseDesktop, DESKTOP_READOBJECTS, GetProcessWindowStation, GetThreadDesktop,
            GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
        },
        Threading::GetCurrentThreadId,
    },
    UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow},
};

const MAX_CLEANUP_STEPS: usize = 128;
const MAX_FIXTURE_ENTRIES: usize = 512;
const MAX_DIAGNOSTIC_OPERATIONS: usize = 64;
const MAX_DIAGNOSTIC_BYTES: usize = 128 * 1024;
const MAX_DIAGNOSTIC_PATHS: usize = 32;

fn bounded(text: impl AsRef<str>, limit: usize) -> String {
    let text = text.as_ref();
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = text[..end].to_owned();
    if end < text.len() {
        output.push_str(" [truncated]");
    }
    output
}

fn error_diagnostic(error: &io::Error) -> serde_json::Value {
    let message = bounded(error.to_string(), 2048);
    let hresults: Vec<_> = message
        .as_bytes()
        .windows(10)
        .filter(|token| token.starts_with(b"0x") && token[2..].iter().all(u8::is_ascii_hexdigit))
        .take(8)
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .collect();
    serde_json::json!({"ok": false, "message": message, "kind": format!("{:?}", error.kind()),
        "raw_os_error": error.raw_os_error(), "hresult_tokens": hresults})
}

fn result_diagnostic(result: &io::Result<String>) -> serde_json::Value {
    match result {
        Ok(message) => serde_json::json!({"ok": true, "message": bounded(message, 2048)}),
        Err(error) => error_diagnostic(error),
    }
}

fn path_diagnostic(path: &Path) -> serde_json::Value {
    let mut output = serde_json::json!({"path": bounded(path.to_string_lossy(), 1024)});
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => {
            output["exists"] = false.into();
            output["metadata_error"] = error_diagnostic(&error);
            return output;
        }
    };
    use std::os::windows::fs::MetadataExt;
    let reparse = metadata.file_attributes() & 0x400 != 0;
    output["exists"] = true.into();
    output["directory"] = metadata.is_dir().into();
    output["bytes"] = metadata.len().into();
    output["reparse"] = reparse.into();
    if reparse {
        return output;
    }
    output["identity_capture"] = match Item::capture(path, false) {
        Ok(item) => {
            serde_json::json!({"ok": true, "stamp": bounded(format!("{:?}", item.stamp), 512)})
        }
        Err(error) => error_diagnostic(&error),
    };
    if metadata.is_file() {
        output["protected_history_capture"] = match Item::capture(path, true) {
            Ok(_) => serde_json::json!({"ok": true}),
            Err(error) => error_diagnostic(&error),
        };
        let mut preview = Vec::new();
        output["content_preview"] =
            match fs::File::open(path).and_then(|file| file.take(96).read_to_end(&mut preview)) {
                Ok(_) => serde_json::json!(String::from_utf8_lossy(&preview)),
                Err(error) => error_diagnostic(&error),
            };
    }
    output
}

fn fixture_diagnostic(fixture: &Fixture) -> serde_json::Value {
    let mut pending = vec![fixture.root.clone()];
    let mut entries = Vec::new();
    while let Some(path) = pending.pop() {
        if entries.len() >= MAX_DIAGNOSTIC_PATHS {
            break;
        }
        let entry = path_diagnostic(&path);
        let descend = entry["directory"] == true && entry["reparse"] == false;
        entries.push(entry);
        if descend {
            match fs::read_dir(&path) {
                Ok(children) => {
                    for child in children.take(MAX_DIAGNOSTIC_PATHS) {
                        match child {
                            Ok(child) if child.path().starts_with(&fixture.root) => {
                                pending.push(child.path())
                            }
                            Ok(_) => {}
                            Err(error) => entries.push(
                                serde_json::json!({"read_dir_error": error_diagnostic(&error)}),
                            ),
                        }
                    }
                }
                Err(error) => {
                    entries.push(serde_json::json!({"read_dir_error": error_diagnostic(&error)}))
                }
            }
        }
    }
    serde_json::json!({"entries": entries, "truncated": !pending.is_empty()})
}

fn mutation_diagnostic(mutation: &Mutation) -> serde_json::Value {
    let steps: Vec<_> = mutation.steps.iter().take(16).map(|step| match step {
        Step::Move { item, target } => serde_json::json!({"kind": "Move", "item": bounded(item.location.path.to_string_lossy(), 1024),
            "target": bounded(target.path.to_string_lossy(), 1024), "stamp": bounded(format!("{:?}", item.stamp), 512)}),
        Step::Recycle { item } => serde_json::json!({"kind": "Recycle", "item": bounded(item.location.path.to_string_lossy(), 1024),
            "protect_contents": item.protects_contents(), "stamp": bounded(format!("{:?}", item.stamp), 512)}),
        Step::Restore { item, token, target, .. } => serde_json::json!({"kind": "Restore", "item": bounded(item.location.path.to_string_lossy(), 1024),
            "target": bounded(target.path.to_string_lossy(), 1024), "pidl_bytes": token.pidl.len(), "stamp": bounded(format!("{:?}", item.stamp), 512)}),
        Step::Batch(_) => serde_json::json!({"kind": "Batch"}),
    }).collect();
    serde_json::json!({"result": result_diagnostic(&mutation.result), "changed": mutation.changed,
        "steps_len": mutation.steps.len(), "steps": steps})
}

fn history_diagnostic(handle: &HistoryHandle) -> serde_json::Value {
    let state = handle.state();
    serde_json::json!({"undo": state.undo, "redo": state.redo, "busy": state.busy})
}

fn push_diagnostic(
    operations: &mut Vec<serde_json::Value>,
    bytes: &mut usize,
    omitted: &mut usize,
    operation: serde_json::Value,
) {
    let size = serde_json::to_vec(&operation).map_or(MAX_DIAGNOSTIC_BYTES, |encoded| encoded.len());
    if operations.len() >= MAX_DIAGNOSTIC_OPERATIONS
        || bytes.saturating_add(size) > MAX_DIAGNOSTIC_BYTES
    {
        *omitted += 1;
    } else {
        *bytes += size;
        operations.push(operation);
    }
}

fn replay_diagnostic(
    fixture: &Fixture,
    handle: &HistoryHandle,
    direction: Direction,
    stop: &AtomicBool,
    phase: &str,
) -> (io::Result<String>, serde_json::Value) {
    let before = fixture_diagnostic(fixture);
    let history_before = history_diagnostic(handle);
    let result = handle.replay(direction);
    let evidence = serde_json::json!({"phase": phase, "direction": format!("{direction:?}"),
        "stop_requested": stop.load(Ordering::Acquire), "result": result_diagnostic(&result),
        "history_before": history_before, "history_after": history_diagnostic(handle),
        "files_before": before, "files_after": fixture_diagnostic(fixture)});
    (result, evidence)
}

fn require(condition: bool, message: impl Into<String>) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.into()))
    }
}

fn object_name(object: *mut c_void) -> io::Result<String> {
    let mut units = [0u16; 256];
    let mut needed = 0;
    if unsafe {
        GetUserObjectInformationW(
            object,
            UOI_NAME,
            units.as_mut_ptr().cast(),
            std::mem::size_of_val(&units) as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let length = units.iter().position(|unit| *unit == 0).ok_or_else(|| {
        io::Error::other("Native object name exceeded the probe's bounded buffer")
    })?;
    Ok(String::from_utf16_lossy(&units[..length]))
}

struct Isolation {
    station: String,
    desktop: String,
    output: PathBuf,
    input_desktop: Option<String>,
}
impl Isolation {
    fn require() -> io::Result<Self> {
        require(
            std::env::var("FOLDEX_NATIVE_UNDO_PROBE").as_deref() == Ok("1"),
            "Requires explicit FOLDEX_NATIVE_UNDO_PROBE=1 in the parent's isolated driver",
        )?;
        let station = object_name(unsafe { GetProcessWindowStation() })?;
        let desktop = object_name(unsafe { GetThreadDesktop(GetCurrentThreadId()) })?;
        let private_station = station.starts_with("FoldexProbe_");
        require(
            private_station
                || (station.eq_ignore_ascii_case("WinSta0") && desktop.starts_with("FoldexProbe_")),
            format!("Refusing native Undo/Redo checks on station {station}, desktop {desktop}"),
        )?;
        let input_desktop = if private_station {
            None
        } else {
            let input = unsafe { OpenInputDesktop(0, 0, DESKTOP_READOBJECTS) };
            if input.is_null() {
                return Err(io::Error::last_os_error());
            }
            let name = object_name(input);
            unsafe { CloseDesktop(input) };
            let name = name?;
            require(
                !desktop.eq_ignore_ascii_case(&name),
                "Refusing to run Undo/Redo fixtures on the input desktop",
            )?;
            Some(name)
        };
        let output = PathBuf::from(
            std::env::var_os("FOLDEX_UNDO_PROBE_OUTPUT")
                .or_else(|| std::env::var_os("FOLDEX_NATIVE_PROBE_OUTPUT"))
                .ok_or_else(|| io::Error::other("A fresh probe output file is required"))?,
        );
        require(output.is_absolute(), "Probe output must be absolute")?;
        require(!output.exists(), "Probe output already exists")?;
        let parent = fs::canonicalize(
            output
                .parent()
                .ok_or_else(|| io::Error::other("Missing output parent"))?,
        )?;
        require(parent.is_dir(), "Probe output parent must already exist")?;
        Ok(Self {
            station,
            desktop,
            output,
            input_desktop,
        })
    }
}

struct Owner(HWND);
impl Owner {
    fn new() -> io::Result<Self> {
        let class: Vec<_> = "STATIC\0".encode_utf16().collect();
        let title: Vec<_> = "Foldex owned Undo probe\0".encode_utf16().collect();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if window.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(window))
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

struct Fixture {
    root: PathBuf,
    temp: PathBuf,
    identity: crate::operation_history::Identity,
    marker: Vec<u8>,
}
impl Fixture {
    fn new(name: &str) -> io::Result<Self> {
        let temp = fs::canonicalize(std::env::temp_dir())?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let root = temp.join(format!(
            "FoldexUndoProbe_{}_{nonce}_{name}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        let root = fs::canonicalize(root)?;
        require(
            root.parent() == Some(temp.as_path()),
            "Fixture escaped the owned Temp boundary",
        )?;
        let marker = format!(
            "Foldex generated Undo fixture {nonce}\n{}\n",
            root.display()
        )
        .into_bytes();
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(".foldex-probe-owner"))?
            .write_all(&marker)?;
        let identity = Item::capture(&root, false)?.stamp.identity;
        Ok(Self {
            root,
            temp,
            identity,
            marker,
        })
    }

    fn path(&self, relative: &str) -> io::Result<PathBuf> {
        require(
            !relative.is_empty()
                && Path::new(relative)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "Fixture paths must contain only ordinary relative components",
        )?;
        Ok(self.root.join(relative))
    }

    fn file(&self, relative: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        let path = self.path(relative)?;
        self.inside(path.parent().unwrap())?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?
            .write_all(bytes)?;
        Ok(path)
    }

    fn folder(&self, relative: &str) -> io::Result<PathBuf> {
        let path = self.path(relative)?;
        self.inside(path.parent().unwrap())?;
        fs::create_dir(&path)?;
        Ok(path)
    }

    fn inside(&self, path: &Path) -> io::Result<()> {
        Item::capture(path, false)?; // Refuse the original reparse node before canonicalization.
        let resolved = fs::canonicalize(path)?;
        require(
            resolved.starts_with(&self.root),
            format!(
                "Native probe target escaped its generated fixture: {}",
                path.display()
            ),
        )?;
        let mut current = Some(resolved.as_path());
        while let Some(path) = current {
            Item::capture(path, false)?; // Reject reparse points without traversing them.
            if path == self.root {
                return Ok(());
            }
            current = path.parent();
        }
        Err(io::Error::other(
            "Native probe target has no owned fixture ancestor",
        ))
    }

    fn remove(&self) -> io::Result<()> {
        // Verify the resolved absolute boundary, root identity and ownership
        // marker before any recursive delete. Refuse unexpected reparse nodes.
        let resolved = fs::canonicalize(&self.root)?;
        require(
            resolved == self.root
                && resolved != self.temp
                && resolved.parent() == Some(self.temp.as_path()),
            "Refusing fixture cleanup outside its exact Temp child",
        )?;
        require(
            Item::capture(&resolved, false)?.stamp.identity == self.identity,
            "Fixture root was replaced; cleanup refused",
        )?;
        require(
            fs::read(resolved.join(".foldex-probe-owner"))? == self.marker,
            "Fixture ownership marker changed; cleanup refused",
        )?;
        let mut pending = vec![resolved.clone()];
        let mut visited = 0;
        while let Some(path) = pending.pop() {
            visited += 1;
            require(
                visited + pending.len() <= MAX_FIXTURE_ENTRIES,
                "Fixture cleanup entry budget exceeded",
            )?;
            self.inside(&path)?;
            if fs::symlink_metadata(&path)?.is_dir() {
                for entry in fs::read_dir(&path)? {
                    pending.push(entry?.path());
                }
            }
        }
        fs::remove_dir_all(resolved)
    }
}

#[derive(Clone, Copy)]
enum CleanupDirection {
    None,
    Undo,
    Redo,
}
struct Cleanup {
    handle: HistoryHandle,
    direction: CleanupDirection,
    live_paths: Vec<PathBuf>,
}

struct Case {
    fixture: Fixture,
    _apartment: Apartment,
    owner: Owner,
    stop: Arc<AtomicBool>,
    cleanup: Vec<Cleanup>,
    checks: Vec<serde_json::Value>,
    native_batches: usize,
    operations: Vec<serde_json::Value>,
    diagnostic_bytes: usize,
    omitted_operations: usize,
}
impl Case {
    fn new(name: &str) -> io::Result<Self> {
        let apartment = Apartment::new()?;
        let owner = Owner::new()?;
        Ok(Self {
            fixture: Fixture::new(name)?,
            _apartment: apartment,
            owner,
            stop: Arc::new(AtomicBool::new(false)),
            cleanup: Vec::new(),
            checks: Vec::new(),
            native_batches: 0,
            operations: Vec::new(),
            diagnostic_bytes: 0,
            omitted_operations: 0,
        })
    }

    fn check(&mut self, name: &str, passed: bool) -> io::Result<()> {
        self.checks
            .push(serde_json::json!({ "check": name, "passed": passed }));
        require(passed, name)
    }

    fn trace(&mut self, operation: serde_json::Value) {
        push_diagnostic(
            &mut self.operations,
            &mut self.diagnostic_bytes,
            &mut self.omitted_operations,
            operation,
        );
    }

    fn replay(&mut self, handle: &HistoryHandle, direction: Direction) -> io::Result<String> {
        let (result, evidence) = replay_diagnostic(
            &self.fixture,
            handle,
            direction,
            &self.stop,
            "journal_replay",
        );
        self.trace(evidence);
        result
    }

    fn record(
        &mut self,
        label: &'static str,
        mutation: Mutation,
        direction: CleanupDirection,
    ) -> (HistoryHandle, io::Result<String>) {
        self.trace(serde_json::json!({"phase": "journal_record", "label": label,
            "mutation": mutation_diagnostic(&mutation), "files": fixture_diagnostic(&self.fixture)}));
        let live_paths = mutation
            .steps
            .iter()
            .filter_map(|step| match (direction, step) {
                (CleanupDirection::Redo, Step::Recycle { item }) => {
                    Some(item.location.path.clone())
                }
                (CleanupDirection::Undo, Step::Restore { target, .. }) => Some(target.path.clone()),
                _ => None,
            })
            .collect();
        let handle = HistoryHandle::new(
            Arc::new(AtomicIsize::new(self.owner.0 as isize)),
            self.stop.clone(),
        );
        let result = handle.mutate(label, || mutation);
        self.trace(
            serde_json::json!({"phase": "journal_record_result", "label": label,
            "result": result_diagnostic(&result), "history": history_diagnostic(&handle)}),
        );
        self.cleanup.push(Cleanup {
            handle: handle.clone(),
            direction,
            live_paths,
        });
        (handle, result)
    }

    fn state(&self, count: usize) -> Arc<ProgressState> {
        ProgressState::new(count, self.stop.clone())
    }

    fn native_group(
        &mut self,
        items: &[(usize, &ComPointer)],
        destination: Option<&ComPointer>,
        name: Option<&[u16]>,
        mode: Mode,
        state: &Arc<ProgressState>,
    ) -> io::Result<(i32, bool)> {
        let before = fixture_diagnostic(&self.fixture);
        self.native_batches += 1;
        let result = perform_batch(items, destination, name, mode, self.owner.0 as isize, state);
        let native_result = match &result {
            Ok((hresult, aborted)) => {
                serde_json::json!({"ok": true, "hresult": format!("0x{:08X}", *hresult as u32),
                "hresult_signed": hresult, "aborted": aborted})
            }
            Err(error) => error_diagnostic(error),
        };
        let outcomes: Vec<_> = state
            .outcomes
            .iter()
            .take(16)
            .map(|value| value.load(Ordering::Acquire))
            .collect();
        let item_hresults: Vec<_> = state
            .errors
            .iter()
            .take(16)
            .map(|value| format!("0x{:08X}", value.load(Ordering::Acquire) as u32))
            .collect();
        let capture_errors: Vec<_> = state
            .capture_errors
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .take(16)
            .map(|error| error.as_ref().map(|error| bounded(error, 2048)))
            .collect();
        let created: Vec<_> = state
            .created
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .take(16)
            .map(|created| {
                created.as_ref().map(|created| {
                    serde_json::json!({"path": bounded(created.path.to_string_lossy(), 1024),
                "pidl_bytes": created.recycle.as_ref().map(|token| token.pidl.len())})
                })
            })
            .collect();
        self.trace(serde_json::json!({"phase": "native_group", "mode": format!("{mode:?}"),
            "effective_flags": format!("0x{:08X}", effective_operation_flags(mode, state.replay)),
            "native_result": native_result, "outcomes": outcomes, "item_hresults": item_hresults,
            "finish_hresult": format!("0x{:08X}", state.finish_result.load(Ordering::Acquire) as u32),
            "skipped": state.skipped.load(Ordering::Acquire), "rejected_delete": state.rejected_delete.load(Ordering::Acquire),
            "capture_created_errors": capture_errors, "created": created,
            "files_before": before, "files_after": fixture_diagnostic(&self.fixture)}));
        result
    }

    fn forward(
        &mut self,
        mode: Mode,
        sources: &[PathBuf],
        destination: Option<&Path>,
        name: Option<&str>,
    ) -> io::Result<Mutation> {
        validate_paths(sources)?;
        for path in sources {
            self.fixture.inside(path)?;
        }
        if let Some(path) = destination {
            self.fixture.inside(path)?;
        }
        if let Some(name) = name {
            validate_name(name)?;
        }
        let before = fixture_diagnostic(&self.fixture);
        let expected: Vec<_> = sources.iter().map(|source| {
            let output = match mode {
                Mode::Copy | Mode::Duplicate | Mode::Move => destination.map(|target| target.join(source.file_name().unwrap())),
                Mode::Rename => name.map(|name| source.with_file_name(name)),
                _ => Some(source.clone()),
            };
            serde_json::json!({"source": bounded(source.to_string_lossy(), 1024),
                "requested_output": output.map(|path| bounded(path.to_string_lossy(), 1024)),
                "expect_source_absent": matches!(mode, Mode::Move | Mode::Rename | Mode::Recycle | Mode::PermanentDelete),
                "shell_assigns_collision_name": mode == Mode::Duplicate})
        }).collect();
        self.native_batches += 1;
        let owner = self.owner.0 as isize;
        let mutation = match mode {
            Mode::Copy | Mode::Duplicate | Mode::Move => transfer_tracked(
                sources,
                destination.ok_or_else(|| invalid("Missing generated transfer destination"))?,
                mode == Mode::Move,
                owner,
                self.stop.clone(),
            ),
            Mode::Rename => {
                require(
                    sources.len() == 1,
                    "Rename facade requires one generated source",
                )?;
                rename_tracked(
                    &sources[0],
                    name.ok_or_else(|| invalid("Missing generated rename name"))?,
                    owner,
                    self.stop.clone(),
                )
            }
            Mode::Recycle | Mode::PermanentDelete => delete_tracked(
                sources,
                mode == Mode::PermanentDelete,
                owner,
                self.stop.clone(),
            ),
        };
        self.trace(
            serde_json::json!({"phase": "tracked_forward", "mode": format!("{mode:?}"),
            "effective_flags": format!("0x{:08X}", effective_operation_flags(mode, false)),
            "expected": expected, "mutation": mutation_diagnostic(&mutation),
            "files_before": before, "files_after": fixture_diagnostic(&self.fixture)}),
        );
        Ok(mutation)
    }

    fn cleanup(&mut self, remove_fixture: bool) -> Vec<String> {
        self.stop.store(false, Ordering::Release);
        let mut errors = Vec::new();
        let mut stash = 0usize;
        for cleanup in &self.cleanup {
            let direction = match cleanup.direction {
                CleanupDirection::None => continue,
                CleanupDirection::Undo => Direction::Undo,
                CleanupDirection::Redo => Direction::Redo,
            };
            for attempt in 0..MAX_CLEANUP_STEPS {
                let state = cleanup.handle.state();
                let available = match direction {
                    Direction::Undo => state.can_undo(),
                    Direction::Redo => state.can_redo(),
                };
                if !available {
                    break;
                }
                // Only generated collision occupants may be relocated. The
                // journal then restores its own exact PIDL, never a trash scan.
                let mut ready = true;
                for path in &cleanup.live_paths {
                    if path.exists() {
                        let target = self
                            .fixture
                            .root
                            .join(format!(".cleanup-preserved-{stash}"));
                        stash += 1;
                        if let Err(error) = self
                            .fixture
                            .inside(path)
                            .and_then(|()| fs::rename(path, target))
                        {
                            errors.push(format!("Preserve generated cleanup occupant: {error}"));
                            ready = false;
                            break;
                        }
                    }
                }
                if !ready {
                    break;
                }
                let (result, evidence) = replay_diagnostic(
                    &self.fixture,
                    &cleanup.handle,
                    direction,
                    &self.stop,
                    "cleanup_replay",
                );
                push_diagnostic(
                    &mut self.operations,
                    &mut self.diagnostic_bytes,
                    &mut self.omitted_operations,
                    evidence,
                );
                if let Err(error) = result {
                    errors.push(format!("Restore exact generated recycled item: {error}"));
                    break;
                }
                if attempt + 1 == MAX_CLEANUP_STEPS {
                    errors.push("History cleanup exceeded its bounded step count".into());
                }
            }
        }
        if errors.is_empty() && remove_fixture {
            if let Err(error) = self.fixture.remove() {
                errors.push(error.to_string());
            }
        }
        errors
    }
}

fn one_created_path(mutation: &Mutation) -> io::Result<PathBuf> {
    require(
        mutation.steps.len() == 1,
        "Native operation must retain exactly one confirmed inverse",
    )?;
    match &mutation.steps[0] {
        Step::Recycle { item } | Step::Move { item, .. } => Ok(item.location.path.clone()),
        _ => Err(io::Error::other("Expected a live created/moved item")),
    }
}

fn bytes(path: &Path, expected: &[u8]) -> bool {
    fs::read(path).is_ok_and(|actual| actual == expected)
}

fn copy_roundtrip(case: &mut Case) -> io::Result<()> {
    case.fixture.folder("source")?;
    let destination = case.fixture.folder("destination")?;
    let source = case
        .fixture
        .file("source/résumé.txt", b"generated copy payload")?;
    let folder = case.fixture.folder("source/tree")?;
    case.fixture
        .file("source/tree/nested.txt", b"generated nested payload")?;
    let mutation = case.forward(
        Mode::Copy,
        &[source.clone(), folder.clone()],
        Some(&destination),
        None,
    )?;
    case.check(
        "copy post-events retain both actual destination identities",
        mutation.steps.len() == 2,
    )?;
    let copied = destination.join("résumé.txt");
    let copied_tree = destination.join("tree");
    let file_identity = Item::capture(&copied, false)?.stamp.identity;
    let folder_identity = Item::capture(&copied_tree, false)?.stamp.identity;
    let (handle, result) = case.record("Copy", mutation, CleanupDirection::Redo);
    result?;
    case.replay(&handle, Direction::Undo)?;
    case.check(
        "copy Undo removes only its generated destinations",
        !copied.exists()
            && !copied_tree.exists()
            && bytes(&source, b"generated copy payload")
            && bytes(&folder.join("nested.txt"), b"generated nested payload"),
    )?;
    case.replay(&handle, Direction::Redo)?;
    case.check(
        "copy Redo restores exact files and directory content",
        bytes(&copied, b"generated copy payload")
            && bytes(&copied_tree.join("nested.txt"), b"generated nested payload")
            && Item::capture(&copied, false)?.stamp.identity == file_identity
            && Item::capture(&copied_tree, false)?.stamp.identity == folder_identity,
    )?;
    Ok(())
}

fn duplicate_roundtrip(case: &mut Case) -> io::Result<()> {
    let source = case
        .fixture
        .file("résumé original.txt", b"generated duplicate payload")?;
    let root = case.fixture.root.clone();
    let mutation = case.forward(Mode::Duplicate, &[source.clone()], Some(&root), None)?;
    let actual = one_created_path(&mutation)?;
    case.fixture.inside(&actual)?;
    case.check(
        "duplicate uses the actual Shell collision-renamed path",
        actual.file_name() != source.file_name()
            && actual.extension() == source.extension()
            && bytes(&actual, b"generated duplicate payload"),
    )?;
    let (handle, result) = case.record("Duplicate", mutation, CleanupDirection::Redo);
    result?;
    case.replay(&handle, Direction::Undo)?;
    case.check(
        "duplicate Undo preserves its original",
        !actual.exists() && bytes(&source, b"generated duplicate payload"),
    )?;
    case.replay(&handle, Direction::Redo)?;
    case.check(
        "duplicate Redo restores its exact collision name",
        bytes(&actual, b"generated duplicate payload")
            && bytes(&source, b"generated duplicate payload"),
    )
}

fn move_rename_roundtrip(case: &mut Case) -> io::Result<()> {
    let destination = case.fixture.folder("destination")?;
    let source = case.fixture.file("source.txt", b"generated move payload")?;
    let identity = Item::capture(&source, false)?.stamp.identity;
    let mutation = case.forward(Mode::Move, &[source.clone()], Some(&destination), None)?;
    let moved = one_created_path(&mutation)?;
    let (movement, result) = case.record("Move", mutation, CleanupDirection::None);
    result?;
    case.replay(&movement, Direction::Undo)?;
    case.check(
        "Move Undo restores the original object",
        bytes(&source, b"generated move payload")
            && !moved.exists()
            && Item::capture(&source, false)?.stamp.identity == identity,
    )?;
    case.replay(&movement, Direction::Redo)?;
    let mutation = case.forward(Mode::Rename, &[moved.clone()], None, Some("renamed Ω.txt"))?;
    let renamed = one_created_path(&mutation)?;
    let (rename, result) = case.record("Rename", mutation, CleanupDirection::None);
    result?;
    case.replay(&rename, Direction::Undo)?;
    case.check(
        "Rename Undo retains object identity",
        bytes(&moved, b"generated move payload") && !renamed.exists(),
    )?;
    case.replay(&rename, Direction::Redo)?;
    case.check(
        "Rename Redo retains object identity",
        !moved.exists()
            && bytes(&renamed, b"generated move payload")
            && Item::capture(&renamed, false)?.stamp.identity == identity,
    )
}

fn recycle_exact_roundtrip(case: &mut Case) -> io::Result<()> {
    let original = case
        .fixture
        .file("same original name.txt", b"FIRST generated recycled object")?;
    let first_identity = Item::capture(&original, false)?.stamp.identity;
    let first = case.forward(Mode::Recycle, &[original.clone()], None, None)?;
    let first_token = match first.steps.first() {
        Some(Step::Restore { token, .. }) => token.pidl.clone(),
        _ => {
            return Err(io::Error::other(
                "Recycle did not retain an exact first PIDL",
            ));
        }
    };
    let (first, result) = case.record("Recycle first", first, CleanupDirection::Undo);
    result?;
    case.check(
        "first recycle is not permanent deletion",
        !original.exists() && !first_token.is_empty(),
    )?;
    case.fixture.file(
        "same original name.txt",
        b"SECOND generated recycled object",
    )?;
    let second_identity = Item::capture(&original, false)?.stamp.identity;
    let second = case.forward(Mode::Recycle, &[original.clone()], None, None)?;
    let second_token = match second.steps.first() {
        Some(Step::Restore { token, .. }) => token.pidl.clone(),
        _ => {
            return Err(io::Error::other(
                "Recycle did not retain an exact second PIDL",
            ));
        }
    };
    let (second, result) = case.record("Recycle second", second, CleanupDirection::Undo);
    result?;
    case.check(
        "same-name recycled items retain distinct exact PIDLs",
        first_token != second_token && first_identity != second_identity,
    )?;
    case.replay(&first, Direction::Undo)?;
    case.check(
        "Undo restores first exact item, not the newest same-name item",
        bytes(&original, b"FIRST generated recycled object")
            && Item::capture(&original, false)?.stamp.identity == first_identity,
    )?;
    case.replay(&first, Direction::Redo)?;
    case.check(
        "Recycle Redo removes the restored exact first object",
        !original.exists(),
    )?;
    case.replay(&second, Direction::Undo)?;
    case.check(
        "second exact PIDL restores second object's bytes and identity",
        bytes(&original, b"SECOND generated recycled object")
            && Item::capture(&original, false)?.stamp.identity == second_identity,
    )?;
    let preserved = case.fixture.path("second restored and preserved.txt")?;
    fs::rename(&original, &preserved)?;
    case.replay(&first, Direction::Undo)?;
    case.check(
        "all generated recycled items are restored before fixture cleanup",
        bytes(&original, b"FIRST generated recycled object")
            && bytes(&preserved, b"SECOND generated recycled object"),
    )
}

fn new_items_roundtrip(case: &mut Case) -> io::Result<()> {
    let file = case.fixture.path("new generated file.txt")?;
    let result = super::super::new_file(&case.fixture.root, "new generated file.txt");
    let mutation = Mutation::created(result, &file);
    let (history, result) = case.record("New file", mutation, CleanupDirection::Redo);
    result?;
    case.replay(&history, Direction::Undo)?;
    case.check("new file Undo removes only the new file", !file.exists())?;
    case.replay(&history, Direction::Redo)?;
    case.check(
        "new file Redo restores the generated empty file",
        bytes(&file, b""),
    )?;
    let folder = case.fixture.path("new generated folder")?;
    let result = super::super::new_folder(&case.fixture.root, "new generated folder");
    let mutation = Mutation::created(result, &folder);
    let (history, result) = case.record("New folder", mutation, CleanupDirection::Redo);
    result?;
    case.replay(&history, Direction::Undo)?;
    case.check("new folder Undo removes its empty folder", !folder.exists())?;
    case.replay(&history, Direction::Redo)?;
    case.check(
        "new folder Redo restores its empty folder",
        folder.is_dir() && fs::read_dir(folder)?.next().is_none(),
    )
}

fn collision_retry(case: &mut Case) -> io::Result<()> {
    let destination = case.fixture.folder("destination")?;
    let source = case
        .fixture
        .file("source.txt", b"generated movable object")?;
    let mutation = case.forward(Mode::Move, &[source.clone()], Some(&destination), None)?;
    let moved = one_created_path(&mutation)?;
    let (history, result) = case.record("Move", mutation, CleanupDirection::None);
    result?;
    case.fixture
        .file("source.txt", b"generated unrelated collision occupant")?;
    let occupant = Item::capture(&source, false)?.stamp.identity;
    let result = case.replay(&history, Direction::Undo);
    case.check(
        "Undo refuses a destination collision and retains a retry",
        result.is_err()
            && history.state().can_undo()
            && !history.state().can_redo()
            && bytes(&source, b"generated unrelated collision occupant")
            && Item::capture(&source, false)?.stamp.identity == occupant
            && bytes(&moved, b"generated movable object"),
    )?;
    fs::remove_file(&source)?;
    case.replay(&history, Direction::Undo)?;
    case.check(
        "collision refusal can be retried after generated blocker is removed",
        !moved.exists() && bytes(&source, b"generated movable object"),
    )?;
    case.replay(&history, Direction::Redo)?;
    case.check(
        "retry preserves Redo",
        !source.exists() && bytes(&moved, b"generated movable object"),
    )
}

fn stale_and_edited_refusal(case: &mut Case) -> io::Result<()> {
    let destination = case.fixture.folder("destination")?;
    let source = case
        .fixture
        .file("source.txt", b"generated original identity")?;
    let mutation = case.forward(Mode::Move, &[source], Some(&destination), None)?;
    let moved = one_created_path(&mutation)?;
    let (history, result) = case.record("Move", mutation, CleanupDirection::None);
    result?;
    let displaced = case.fixture.path("displaced original.txt")?;
    fs::rename(&moved, &displaced)?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&moved)?
        .write_all(b"generated replacement identity")?;
    let replacement = Item::capture(&moved, false)?.stamp.identity;
    let replay = case.replay(&history, Direction::Undo);
    case.check(
        "Undo refuses a replaced source object without touching either identity",
        replay.is_err()
            && history.state().can_undo()
            && Item::capture(&moved, false)?.stamp.identity == replacement
            && bytes(&moved, b"generated replacement identity")
            && bytes(&displaced, b"generated original identity"),
    )?;

    let created = case
        .fixture
        .file("new then edited.txt", b"initial generated text")?;
    let mutation = Mutation::created(Ok("Created generated fixture".into()), &created);
    let (history, result) = case.record("New file", mutation, CleanupDirection::Redo);
    result?;
    fs::write(
        &created,
        b"edited generated text must survive Undo with a changed byte count",
    )?;
    let replay = case.replay(&history, Direction::Undo);
    case.check(
        "Undo refuses removal of edited newly-created content",
        replay.is_err()
            && history.state().can_undo()
            && bytes(
                &created,
                b"edited generated text must survive Undo with a changed byte count",
            ),
    )
}

fn permanent_exclusion(case: &mut Case) -> io::Result<()> {
    let source = case.fixture.file(
        "generated permanent target.txt",
        b"generated permanent payload",
    )?;
    let mutation = case.forward(Mode::PermanentDelete, &[source.clone()], None, None)?;
    case.check(
        "permanent delete produces no recoverable history steps",
        mutation.changed && mutation.steps.is_empty(),
    )?;
    let (history, result) = case.record("Delete permanently", mutation, CleanupDirection::None);
    result?;
    case.check(
        "permanent deletion is excluded from both journal directions",
        !source.exists() && !history.state().can_undo() && !history.state().can_redo(),
    )
}

fn partial_cancel(case: &mut Case) -> io::Result<()> {
    let destination = case.fixture.folder("destination")?;
    let duplicate = case
        .fixture
        .file("destination/duplicate.txt", b"first native group completes")?;
    let later = case
        .fixture
        .file("later.txt", b"second native group is canceled")?;
    let sources = [duplicate.clone(), later.clone()];
    let inputs = history::transfer_inputs(&sources, &destination, &[false, false]);
    let first = shell_item(&duplicate)?;
    let second = shell_item(&later)?;
    let folder = shell_item(&destination)?;
    let state = case.state(2);
    let first_result =
        case.native_group(&[(0, &first)], Some(&folder), None, Mode::Duplicate, &state)?;
    require(
        first_result.0 >= 0 && !first_result.1,
        "First native group unexpectedly failed",
    )?;
    case.stop.store(true, Ordering::Release);
    let canceled = case.native_group(&[(1, &second)], Some(&folder), None, Mode::Copy, &state);
    case.check(
        "cancellation is observed between real native operation groups",
        canceled
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::Interrupted),
    )?;
    let mutation = history::mutation(completed_report(&state, canceled), inputs, "Copied", false);
    case.trace(serde_json::json!({"phase": "partial_native_mutation", "mutation": mutation_diagnostic(&mutation),
        "files": fixture_diagnostic(&case.fixture)}));
    let actual = one_created_path(&mutation)?;
    case.check(
        "partial error retains only its actually completed inverse",
        mutation.changed
            && mutation.result.is_err()
            && !destination.join("later.txt").exists()
            && bytes(&later, b"second native group is canceled"),
    )?;
    let (history, result) = case.record("Partial copy", mutation, CleanupDirection::Redo);
    case.check(
        "partial completion is journaled even when the overall operation reports failure",
        result.is_err() && history.state().can_undo(),
    )?;
    let replay = case.replay(&history, Direction::Undo);
    case.check(
        "a pre-canceled Undo preserves its retry and generated output",
        replay.is_err() && history.state().can_undo() && actual.exists(),
    )?;
    case.stop.store(false, Ordering::Release);
    case.replay(&history, Direction::Undo)?;
    case.check(
        "partial Undo removes only the confirmed completed destination",
        !actual.exists()
            && bytes(&duplicate, b"first native group completes")
            && bytes(&later, b"second native group is canceled"),
    )?;
    case.replay(&history, Direction::Redo)?;
    case.check(
        "partial Redo restores only that completed destination",
        bytes(&actual, b"first native group completes") && !destination.join("later.txt").exists(),
    )
}

fn partial_replay_retry(case: &mut Case) -> io::Result<()> {
    let destination = case.fixture.folder("destination")?;
    let first = case.fixture.file("first.txt", b"first generated move")?;
    let second = case.fixture.file("second.txt", b"second generated move")?;
    let mutation = case.forward(
        Mode::Move,
        &[first.clone(), second.clone()],
        Some(&destination),
        None,
    )?;
    case.check(
        "two completed native moves retain two inverses",
        mutation.steps.len() == 2,
    )?;
    let (history, result) = case.record("Move pair", mutation, CleanupDirection::None);
    result?;
    case.fixture.file("first.txt", b"generated retry blocker")?;
    let result = case.replay(&history, Direction::Undo);
    case.check(
        "partial replay retains failed Undo and completed Redo independently",
        result.is_err()
            && history.state().can_undo()
            && history.state().can_redo()
            && bytes(&first, b"generated retry blocker")
            && bytes(&second, b"second generated move")
            && bytes(&destination.join("first.txt"), b"first generated move")
            && !destination.join("second.txt").exists(),
    )?;
    fs::remove_file(&first)?;
    case.replay(&history, Direction::Undo)?;
    case.check(
        "retry completes only the previously failed step",
        bytes(&first, b"first generated move") && bytes(&second, b"second generated move"),
    )?;
    case.replay(&history, Direction::Redo)?;
    case.replay(&history, Direction::Redo)?;
    case.check(
        "both completed partial records remain redoable",
        !first.exists()
            && !second.exists()
            && bytes(&destination.join("first.txt"), b"first generated move")
            && bytes(&destination.join("second.txt"), b"second generated move")
            && !history.state().can_redo(),
    )
}

#[test]
#[ignore = "requires parent-owned FoldexProbe_ non-input desktop/station, generated Temp fixtures and hard subprocess timeout"]
fn native_undo_redo_generated_files() {
    without_session_undo(run_generated_file_probe);
}

fn run_generated_file_probe() {
    let isolation = Isolation::require().expect("parent-owned non-input desktop Undo probe guard");
    for mode in [
        Mode::Copy,
        Mode::Duplicate,
        Mode::Move,
        Mode::Rename,
        Mode::Recycle,
        Mode::PermanentDelete,
    ] {
        assert_eq!(
            effective_operation_flags(mode, false),
            operation_flags(mode) & !(FOF_ALLOWUNDO | FOFX_ADDUNDORECORD),
            "Probe scope must suppress only Shell undo records for forward {mode:?}"
        );
    }
    let started = Instant::now();
    let scenarios: [(&str, fn(&mut Case) -> io::Result<()>); 10] = [
        ("copy_file_and_tree", copy_roundtrip),
        ("shell_duplicate_actual_name", duplicate_roundtrip),
        ("move_and_rename", move_rename_roundtrip),
        ("same_name_exact_recycle_pidl", recycle_exact_roundtrip),
        ("new_file_and_folder", new_items_roundtrip),
        ("collision_refusal_and_retry", collision_retry),
        ("stale_and_edited_refusal", stale_and_edited_refusal),
        ("permanent_delete_exclusion", permanent_exclusion),
        ("partial_native_group_cancel", partial_cancel),
        ("partial_journal_replay_retry", partial_replay_retry),
    ];
    let mut reports = Vec::new();
    for (name, scenario) in scenarios {
        let began = Instant::now();
        let report = match Case::new(name) {
            Ok(mut case) => {
                let result = scenario(&mut case);
                let scenario_passed = result.is_ok();
                let cleanup_errors = case.cleanup(scenario_passed);
                let fixture_retained = !scenario_passed || !cleanup_errors.is_empty();
                serde_json::json!({
                    "scenario": name, "passed": scenario_passed && cleanup_errors.is_empty(),
                    "checks": case.checks, "error": result.err().map(|error| error.to_string()),
                    "operations": case.operations, "diagnostic_bytes": case.diagnostic_bytes,
                    "omitted_operations": case.omitted_operations,
                    "cleanup_errors": cleanup_errors, "fixture": case.fixture.root,
                    "fixture_retained": fixture_retained,
                    "native_forward_calls_attempted": case.native_batches,
                    "elapsed_ms": began.elapsed().as_millis(),
                })
            }
            Err(error) => {
                serde_json::json!({"scenario": name, "passed": false, "setup_error": error.to_string()})
            }
        };
        reports.push(report);
    }
    let passed = reports.iter().all(|report| report["passed"] == true);
    let report = serde_json::json!({
        "schema": 1, "workload": "native-generated-file-undo-redo", "passed": passed,
        "station": isolation.station, "desktop": isolation.desktop,
        "input_desktop": isolation.input_desktop,
        "isolation": if isolation.station.starts_with("FoldexProbe_") { "private_window_station" } else { "inactive_desktop_on_shared_WinSta0" },
        "scenarios": reports, "elapsed_ms": started.elapsed().as_millis(),
        "clipboard_accessed": false, "desktop_input_sent": false,
        "global_shell_undo_record_flags": false, "global_shell_undo_verb": false,
        "forward_collision_policy": "normal production flags; only ALLOWUNDO/ADDUNDORECORD suppressed by without_session_undo",
        "tracked_facades_exercised": ["transfer_tracked", "rename_tracked", "delete_tracked"],
        "native_callback_log": "FOLDEX_NATIVE_POST / FOLDEX_NATIVE_CAPTURE_ERROR in captured subprocess stdout/stderr",
        "diagnostic_limits": {"operations_per_scenario": MAX_DIAGNOSTIC_OPERATIONS, "bytes_per_scenario": MAX_DIAGNOSTIC_BYTES,
            "paths_per_snapshot": MAX_DIAGNOSTIC_PATHS, "file_preview_bytes": 96},
        "recycle_lookup": "only exact journal-owned PIDLs; no trash enumeration",
        "cancellation_scope": "deterministic cancellation between real Shell operation groups and pre-canceled journal replay",
        "limits": ["parent must enforce hard subprocess timeout", "does not click native Cancel/retry dialogs", "does not exercise normal file-worker queue or clipboard paste"],
    });
    let encoded = serde_json::to_vec_pretty(&report).expect("bounded native Undo report");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(isolation.output)
        .expect("fresh native Undo report");
    output
        .write_all(&encoded)
        .expect("write native Undo evidence");
    output.sync_all().expect("flush native Undo evidence");
    println!(
        "FOLDEX_NATIVE_UNDO_EVIDENCE={}",
        serde_json::to_string(&report).unwrap()
    );
    assert!(
        passed,
        "Native Undo/Redo probe failed; inspect its generated-file report"
    );
}
