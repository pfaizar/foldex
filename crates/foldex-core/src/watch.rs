//! A latest-request native directory watcher. No filesystem calls run on the caller's thread.
//!
//! Notifications invalidate a listing; their names are deliberately not applied as model deltas.
//! A full enumeration is the source of truth, including after a notification-buffer overflow.
use std::{ffi::OsString, io, path::PathBuf};
use tokio::sync::watch;

pub(crate) const BUFFER_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub generation: u64,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Notice {
    pub generation: u64,
    pub revision: u64,
    pub overflows: u64,
    pub ready: bool,
    pub error: Option<String>,
    /// Exact immediate child names from this completion. None means overflow or
    /// an untrusted/truncated batch and requires conservative scope invalidation.
    pub names: Option<Vec<(u64, OsString)>>,
    pub names_after: u64,
}
impl Notice {
    fn invalidate(&mut self, overflow: bool) {
        self.revision = self.revision.saturating_add(1);
        self.overflows = self.overflows.saturating_add(u64::from(overflow));
    }
    fn record_names(&mut self, names: Option<Vec<OsString>>) {
        self.invalidate(names.is_none());
        let Some(names) = names else {
            self.names = None;
            self.names_after = self.revision;
            return;
        };
        let retained = self.names.get_or_insert_with(Vec::new);
        let mut bytes = retained
            .iter()
            .map(|(_, name)| name.as_encoded_bytes().len())
            .sum::<usize>();
        for name in names {
            if let Some((at, _)) = retained.iter_mut().find(|(_, old)| old == &name) {
                *at = self.revision;
                continue;
            }
            bytes = bytes.saturating_add(name.as_encoded_bytes().len());
            if retained.len() == 64 || bytes > 64 * 1024 {
                self.names = None;
                self.names_after = self.revision;
                return;
            }
            retained.push((self.revision, name));
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::NativeWatcher;

#[cfg(not(windows))]
pub(crate) struct NativeWatcher;
#[cfg(not(windows))]
impl NativeWatcher {
    pub fn new() -> io::Result<(Self, watch::Receiver<Notice>)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native directory watching is Windows-only",
        ))
    }
    pub fn set(&self, _request: Option<Request>) {}
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::{
        os::windows::ffi::OsStrExt,
        ptr,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };
    use windows_sys::Win32::{
        Foundation::{
            CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NOTIFY_ENUM_DIR, HANDLE,
            INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0,
        },
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
            FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION,
            FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
            FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            ReadDirectoryChangesW,
        },
        System::{
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
            Threading::{
                CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects,
                WaitForSingleObject,
            },
        },
    };

    // HANDLE is an opaque pointer-sized value. All operations documented as thread-safe below
    // use an owned kernel handle; no Rust pointee is accessed through this numeric storage.
    struct OwnedHandle(usize);
    impl OwnedHandle {
        fn get(&self) -> HANDLE {
            self.0 as HANDLE
        }
        fn event() -> io::Result<Self> {
            // Unnamed, manual-reset event, initially nonsignaled, default security descriptor.
            let handle = unsafe { CreateEventW(ptr::null(), 1, 0, ptr::null()) };
            if handle.is_null() {
                Err(io::Error::last_os_error())
            } else {
                Ok(Self(handle as usize))
            }
        }
    }
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // The unique handle remains owned until all pending I/O has been drained.
            unsafe {
                CloseHandle(self.get());
            }
        }
    }

    struct Control {
        event: OwnedHandle,
        // None means no new command. Some(None) disables the current watch.
        command: Mutex<Option<Option<Request>>>,
        stopped: AtomicBool,
    }

    pub(crate) struct NativeWatcher {
        control: Arc<Control>,
    }
    impl NativeWatcher {
        /// Called by the background coordinator, never by a UI callback.
        pub fn new() -> io::Result<(Self, watch::Receiver<Notice>)> {
            let control = Arc::new(Control {
                event: OwnedHandle::event()?,
                command: Mutex::new(None),
                stopped: AtomicBool::new(false),
            });
            let worker_control = control.clone();
            let (sender, receiver) = watch::channel(Notice::default());
            thread::Builder::new()
                .name("foldex-directory-watch".into())
                .stack_size(256 * 1024)
                .spawn(move || run(worker_control, sender))?;
            Ok((Self { control }, receiver))
        }

        pub fn set(&self, request: Option<Request>) {
            if let Ok(mut command) = self.control.command.lock() {
                *command = Some(request);
            }
            // SetEvent never waits for the watcher or pending disk I/O.
            unsafe {
                SetEvent(self.control.event.get());
            }
        }
    }
    impl Drop for NativeWatcher {
        fn drop(&mut self) {
            self.control.stopped.store(true, Ordering::Release);
            unsafe {
                SetEvent(self.control.event.get());
            }
            // No join: cancellation/completion draining is owned by the worker.
        }
    }

    struct Active {
        directory: OwnedHandle,
        completion: OwnedHandle,
        // DWORD alignment is required by ReadDirectoryChangesW. Boxes keep these addresses
        // fixed while OVERLAPPED I/O is outstanding, including across moves of Active.
        buffer: Box<[u32]>,
        overlapped: Box<OVERLAPPED>,
        pending: bool,
    }
    impl Active {
        fn open(path: &std::path::Path) -> io::Result<Self> {
            let wide = native_path(path)?;
            // A directory is opened without mutation rights and without denying concurrent
            // readers, writers, renames or deletion. The handle is used only on this worker.
            let directory = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    ptr::null_mut(),
                )
            };
            if directory == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            let directory = OwnedHandle(directory as usize);
            let completion = OwnedHandle::event()?;
            let mut active = Self {
                directory,
                completion,
                buffer: vec![0_u32; BUFFER_BYTES / 4].into_boxed_slice(),
                overlapped: Box::new(OVERLAPPED::default()),
                pending: false,
            };
            active.arm()?;
            Ok(active)
        }

        fn arm(&mut self) -> io::Result<()> {
            debug_assert!(!self.pending);
            *self.overlapped = OVERLAPPED::default();
            self.overlapped.hEvent = self.completion.get();
            unsafe {
                ResetEvent(self.completion.get());
            }
            // Nonrecursive: exactly the visible directory, including in-place writes/metadata.
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_ATTRIBUTES
                | FILE_NOTIFY_CHANGE_SIZE
                | FILE_NOTIFY_CHANGE_LAST_WRITE
                | FILE_NOTIFY_CHANGE_CREATION
                | FILE_NOTIFY_CHANGE_SECURITY;
            let result = unsafe {
                ReadDirectoryChangesW(
                    self.directory.get(),
                    self.buffer.as_mut_ptr().cast(),
                    BUFFER_BYTES as u32,
                    0,
                    filter,
                    ptr::null_mut(),
                    self.overlapped.as_mut(),
                    None,
                )
            };
            if result == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(error);
                }
            }
            self.pending = true;
            Ok(())
        }

        /// Returns whether loss of individual notifications requires explicit invalidation.
        fn completed(&mut self) -> io::Result<Option<Vec<OsString>>> {
            let mut bytes = 0;
            let result = unsafe {
                GetOverlappedResult(
                    self.directory.get(),
                    self.overlapped.as_ref(),
                    &mut bytes,
                    0,
                )
            };
            if result == 0 {
                let error = io::Error::last_os_error();
                // Defensively preserve pending storage on a spurious completion signal.
                if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
                    return Err(error);
                }
                self.pending = false;
                if error.raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR as i32) {
                    return Ok(None);
                }
                return Err(error);
            }
            self.pending = false;
            // The documented overflow completion succeeds with zero bytes. No partial
            // filename list is trusted; a fresh enumeration reconciles the whole listing.
            Ok(child_names(&self.buffer, bytes as usize))
        }
    }

    fn native_path(path: &std::path::Path) -> io::Result<Vec<u16>> {
        // GetFullPathName-style lexical normalization happens only on this worker. The
        // verbatim prefix supports the same long drive/UNC paths as Rust's filesystem API.
        let absolute = std::path::absolute(path)?;
        let wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory path contains NUL",
            ));
        }
        let namespace = wide.len() >= 4
            && wide[0..2] == [92, 92]
            && (wide[2] == 63 || wide[2] == 46)
            && wide[3] == 92;
        let mut prefixed = if namespace {
            wide
        } else if wide.starts_with(&[92, 92]) {
            let mut prefixed: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
            prefixed.extend_from_slice(&wide[2..]);
            prefixed
        } else {
            let mut prefixed: Vec<u16> = "\\\\?\\".encode_utf16().collect();
            prefixed.extend_from_slice(&wide);
            prefixed
        };
        prefixed.push(0);
        Ok(prefixed)
    }
    impl Drop for Active {
        fn drop(&mut self) {
            if self.pending {
                unsafe {
                    CancelIoEx(self.directory.get(), self.overlapped.as_ref());
                    let mut ignored = 0;
                    // CancelIoEx requests cancellation; storage must stay alive until the
                    // completion (normal or ERROR_OPERATION_ABORTED) has actually arrived.
                    GetOverlappedResult(
                        self.directory.get(),
                        self.overlapped.as_ref(),
                        &mut ignored,
                        1,
                    );
                }
                self.pending = false;
            }
        }
    }

    fn run(control: Arc<Control>, sender: watch::Sender<Notice>) {
        let mut active: Option<Active> = None;
        let mut notice = Notice::default();
        loop {
            unsafe {
                ResetEvent(control.event.get());
            }
            if control.stopped.load(Ordering::Acquire) {
                break;
            }
            let command = control
                .command
                .lock()
                .ok()
                .and_then(|mut command| command.take());
            if let Some(request) = command {
                // Drop drains I/O before opening a replacement. Rapid navigation cannot
                // accumulate detached open/watch jobs or buffers for superseded paths.
                drop(active.take());
                notice = Notice::default();
                if let Some(request) = request {
                    notice.generation = request.generation;
                    match Active::open(&request.path) {
                        Ok(watcher) => {
                            active = Some(watcher);
                            notice.ready = true;
                        }
                        Err(error) => {
                            notice.error = Some(error.to_string());
                        }
                    }
                }
                sender.send_replace(notice.clone());
            }
            if control.stopped.load(Ordering::Acquire) {
                break;
            }
            let result = if let Some(watcher) = active.as_ref() {
                let handles = [control.event.get(), watcher.completion.get()];
                unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) }
            } else {
                unsafe { WaitForSingleObject(control.event.get(), INFINITE) }
            };
            if result == WAIT_OBJECT_0 {
                continue;
            }
            if result == WAIT_FAILED {
                notice.ready = false;
                notice.error = Some(io::Error::last_os_error().to_string());
                notice.names = None;
                sender.send_replace(notice);
                break;
            }
            let Some(watcher) = active.as_mut() else {
                continue;
            };
            match watcher.completed() {
                Ok(names) => {
                    notice.record_names(names);
                    if let Err(error) = watcher.arm() {
                        notice.ready = false;
                        notice.error = Some(error.to_string());
                        drop(active.take());
                    }
                }
                Err(error) => {
                    notice.ready = false;
                    notice.error = Some(error.to_string());
                    notice.names = None;
                    drop(active.take());
                }
            }
            sender.send_replace(notice.clone());
        }
        // Active is dropped before Control and its event, canceling and draining pending I/O.
        drop(active);
    }
}

/// Parse the bounded FILE_NOTIFY_INFORMATION buffer without casting unaligned
/// structs or trusting offsets/filenames as model deltas. Names are invalidation
/// hints only; enumeration remains the source of truth.
#[cfg(windows)]
fn child_names(words: &[u32], bytes: usize) -> Option<Vec<OsString>> {
    use std::os::windows::ffi::OsStringExt;
    if bytes < 12 || bytes > BUFFER_BYTES || bytes > words.len().checked_mul(4)? {
        return None;
    }
    let mut names = Vec::new();
    let mut offset = 0usize;
    loop {
        if offset % 4 != 0 || offset.checked_add(12)? > bytes {
            return None;
        }
        let first = offset / 4;
        let next = words[first] as usize;
        let action = words[first + 1];
        let length = words[first + 2] as usize;
        if !(1..=5).contains(&action)
            || length == 0
            || length % 2 != 0
            || offset.checked_add(12)?.checked_add(length)? > bytes
        {
            return None;
        }
        let mut raw = Vec::with_capacity(length / 2);
        for index in 0..length / 2 {
            let unit = offset / 2 + 6 + index;
            let value = ((words[unit / 2] >> ((unit % 2) * 16)) & 0xffff) as u16;
            if matches!(value, 0 | 47 | 92) {
                return None;
            }
            raw.push(value);
        }
        let name = OsString::from_wide(&raw);
        if name == "." || name == ".." {
            return None;
        }
        if !names.contains(&name) {
            if names.len() == 64 {
                return None;
            }
            names.push(name);
        }
        if next == 0 {
            return Some(names);
        }
        if next < 12 + length || next % 4 != 0 {
            return None;
        }
        offset = offset.checked_add(next)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn coalesced_notifications_preserve_exact_child_hints_and_overflow_floor() {
        let mut notice = Notice::default();
        for _ in 0..100 {
            notice.record_names(Some(vec![OsString::from("NTUSER.DAT")]));
        }
        assert_eq!(notice.revision, 100);
        assert_eq!(notice.names_after, 0);
        assert_eq!(
            notice.names.as_ref().unwrap(),
            &vec![(100, OsString::from("NTUSER.DAT"))]
        );
        notice.record_names(None);
        assert_eq!(notice.names_after, 101);
        notice.record_names(Some(vec![OsString::from("folder")]));
        assert_eq!(
            notice.names.as_ref().unwrap(),
            &vec![(102, OsString::from("folder"))]
        );
        assert_eq!(
            notice.names_after, 101,
            "older observers must still see the overflow"
        );
    }

    #[cfg(windows)]
    #[test]
    fn changed_child_parser_preserves_raw_utf16_and_rejects_untrusted_offsets() {
        use std::os::windows::ffi::OsStringExt;
        let raw = [b'a' as u16, 0xd800, b'x' as u16];
        let bytes = 12 + raw.len() * 2;
        let mut words = vec![0u32; bytes.div_ceil(4)];
        words[1] = 3;
        words[2] = (raw.len() * 2) as u32;
        for (index, unit) in raw.iter().enumerate() {
            words[3 + index / 2] |= u32::from(*unit) << ((index % 2) * 16);
        }
        assert_eq!(
            child_names(&words, bytes).unwrap(),
            vec![OsString::from_wide(&raw)]
        );
        words[0] = 4;
        assert!(child_names(&words, bytes).is_none());
        words[0] = 0;
        words[2] = u32::MAX;
        assert!(child_names(&words, bytes).is_none());
        assert!(child_names(&words, 0).is_none());
    }

    #[test]
    fn overflow_invalidation_survives_later_changes_in_latest_slot() {
        let mut notice = Notice {
            generation: 7,
            ready: true,
            ..Notice::default()
        };
        notice.invalidate(true);
        notice.invalidate(false);
        assert_eq!(notice.generation, 7);
        assert_eq!(notice.revision, 2);
        assert_eq!(notice.overflows, 1);
    }

    #[cfg(windows)]
    fn wait_for(
        receiver: &mut watch::Receiver<Notice>,
        predicate: impl Fn(&Notice) -> bool,
    ) -> Notice {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let notice = receiver.borrow_and_update().clone();
            if predicate(&notice) {
                return notice;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!(
            "watcher did not publish expected state: {:?}",
            *receiver.borrow()
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_watch_reports_in_place_write_and_closes_after_cancellation() {
        use std::io::Write;
        let fixture = crate::scan::tests::Fixture::new();
        let path = fixture.path.join("existing.txt");
        std::fs::write(&path, b"old").unwrap();
        let (watcher, mut events) = NativeWatcher::new().unwrap();
        watcher.set(Some(Request {
            generation: 11,
            path: fixture.path.clone(),
        }));
        let ready = wait_for(&mut events, |n| {
            n.generation == 11 && (n.ready || n.error.is_some())
        });
        assert!(ready.ready, "{}", ready.error.unwrap_or_default());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"more fixture bytes").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let changed = wait_for(&mut events, |n| n.revision > ready.revision);
        assert_eq!(changed.generation, 11);
        assert!(changed.ready);
        watcher.set(None);
        wait_for(&mut events, |n| n.generation == 0 && !n.ready);
        std::fs::write(&path, b"changed while disabled").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(80));
        assert!(
            !events.has_changed().unwrap(),
            "a disabled watch must not publish disk changes"
        );
        drop(watcher);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while events.has_changed().is_ok() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            events.has_changed().is_err(),
            "watch worker must exit and close its channel"
        );
    }

    #[cfg(windows)]
    #[test]
    fn unsupported_path_reports_failure_and_latest_path_replaces_it() {
        let fixture = crate::scan::tests::Fixture::new();
        let (watcher, mut events) = NativeWatcher::new().unwrap();
        watcher.set(Some(Request {
            generation: 1,
            path: fixture.path.join("absent-directory"),
        }));
        let failed = wait_for(&mut events, |n| n.generation == 1 && n.error.is_some());
        assert!(!failed.ready);
        watcher.set(Some(Request {
            generation: 2,
            path: fixture.path.clone(),
        }));
        let ready = wait_for(&mut events, |n| {
            n.generation == 2 && (n.ready || n.error.is_some())
        });
        assert!(ready.ready, "{}", ready.error.unwrap_or_default());
        drop(watcher);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while events.has_changed().is_ok() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            events.has_changed().is_err(),
            "pending read cancellation must release the worker"
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_watch_accepts_generated_paths_longer_than_max_path() {
        let fixture = crate::scan::tests::Fixture::new();
        let mut path = fixture.path.clone();
        for n in 0..7 {
            path.push(format!("part-{n}-{}", "x".repeat(38)));
        }
        std::fs::create_dir_all(&path).unwrap();
        assert!(path.as_os_str().len() > 260);
        let (watcher, mut events) = NativeWatcher::new().unwrap();
        watcher.set(Some(Request {
            generation: 91,
            path,
        }));
        let ready = wait_for(&mut events, |n| {
            n.generation == 91 && (n.ready || n.error.is_some())
        });
        assert!(ready.ready, "{}", ready.error.unwrap_or_default());
        drop(watcher);
    }
}
