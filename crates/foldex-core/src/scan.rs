use crate::model::{CHUNK_LEN, Entry, EntryStore, LimitReason, Snapshot};
use std::{
    fs, io,
    mem::size_of,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) const DEFAULT_DIRECTORY_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const DEFAULT_SCAN_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const DEFAULT_SCAN_ENTRIES: usize = 250_000;

#[derive(Clone, Copy)]
pub(crate) struct CollectOptions {
    pub recursive: bool,
    pub max_entries: usize,
    pub max_bytes: usize,
    pub max_depth: usize,
}

/// Blocking directory enumeration for benchmarks/non-UI callers.
pub fn enumerate_directory(path: &Path) -> io::Result<Arc<Snapshot>> {
    collect(
        path.to_path_buf(),
        CollectOptions {
            recursive: false,
            max_entries: u32::MAX as usize,
            max_bytes: DEFAULT_DIRECTORY_BYTES,
            max_depth: 1,
        },
        &|| false,
        None,
        &mut |_| {},
    )?
    .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "enumeration cancelled"))
}

/// Blocking, bounded recursive name scan. UI code should call `Browser::scan`.
pub fn scan_recursive(path: &Path) -> io::Result<Arc<Snapshot>> {
    collect(
        path.to_path_buf(),
        CollectOptions {
            recursive: true,
            max_entries: DEFAULT_SCAN_ENTRIES,
            max_bytes: DEFAULT_SCAN_BYTES,
            max_depth: 128,
        },
        &|| false,
        None,
        &mut |_| {},
    )?
    .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "scan cancelled"))
}

struct Builder {
    entries: EntryStore,
    pending: Vec<Entry>,
    /// Conservative allowance includes ordering/sort scratch and one pending chunk.
    admission_bytes: usize,
    workspace_peak: usize,
    shared_memory_pressure: bool,
}

impl Builder {
    fn new(path: &Path, reservation: Option<Arc<crate::memory::MemoryReservation>>) -> Self {
        Self {
            entries: EntryStore {
                reservation,
                ..EntryStore::default()
            },
            pending: Vec::new(),
            admission_bytes: size_of::<Snapshot>()
                + path.as_os_str().len()
                + CHUNK_LEN * size_of::<Entry>(),
            workspace_peak: 0,
            shared_memory_pressure: false,
        }
    }
    fn workspace(&mut self, bytes: usize, options: CollectOptions) -> bool {
        if bytes <= self.workspace_peak {
            return true;
        }
        let total = self
            .admission_bytes
            .saturating_add(bytes - self.workspace_peak);
        if total > options.max_bytes {
            return false;
        }
        if self
            .entries
            .reservation
            .as_ref()
            .is_some_and(|reservation| !reservation.grow_to(total))
        {
            self.shared_memory_pressure = true;
            return false;
        }
        self.admission_bytes = total;
        self.workspace_peak = bytes;
        true
    }
    fn len(&self) -> usize {
        self.entries.len + self.pending.len()
    }
    fn admit(&mut self, entry: &Entry, options: CollectOptions) -> Result<usize, LimitReason> {
        if self.len() >= options.max_entries.min(u32::MAX as usize) {
            return Err(LimitReason::EntryCount);
        }
        // Ordering/progress scratch, the prepared hidden-file permutation, and
        // bounded current/pending query indices. The extra four bytes per visible
        // row fit inside this existing allowance; no model budget is increased.
        // This is deliberately conservative; reserved bytes can exceed logical snapshot bytes.
        let bytes = size_of::<Entry>() + entry.heap_bytes() + 128 + 2 * size_of::<usize>();
        if self.admission_bytes.saturating_add(bytes) > options.max_bytes {
            return Err(LimitReason::MemoryBudget);
        }
        if self
            .entries
            .reservation
            .as_ref()
            .is_some_and(|reservation| {
                !reservation.grow_to(self.admission_bytes.saturating_add(bytes))
            })
        {
            self.shared_memory_pressure = true;
            return Err(LimitReason::MemoryBudget);
        }
        Ok(bytes)
    }
    fn push(&mut self, entry: Entry, bytes: usize) {
        self.admission_bytes = self.admission_bytes.saturating_add(bytes);
        if self.pending.capacity() == 0 {
            self.pending.reserve_exact(CHUNK_LEN);
        }
        self.pending.push(entry);
        if self.pending.len() == CHUNK_LEN {
            self.flush();
        }
    }
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let chunk = std::mem::replace(&mut self.pending, Vec::with_capacity(CHUNK_LEN));
        self.entries.len += chunk.len();
        self.entries.chunks.push(Arc::from(chunk));
    }
}

struct Frame {
    relative: PathBuf,
    entries: fs::ReadDir,
    workspace_bytes: usize,
}

fn frame_workspace(root: &Path, relative: &Path) -> usize {
    // ReadDir retains an owned root; path construction/Windows UTF-16 conversion also need
    // temporary buffers. Reserve conservatively for deep/long paths and iterator state.
    root.as_os_str()
        .len()
        .saturating_add(relative.as_os_str().len())
        .saturating_mul(8)
        .saturating_add(4096)
}

pub(crate) fn collect(
    path: PathBuf,
    options: CollectOptions,
    cancelled: &dyn Fn() -> bool,
    reservation: Option<Arc<crate::memory::MemoryReservation>>,
    progress: &mut dyn FnMut(Arc<Snapshot>),
) -> io::Result<Option<Arc<Snapshot>>> {
    if cancelled() {
        return Ok(None);
    }
    let start = Instant::now();
    let first = fs::read_dir(&path)?;
    let root_workspace = frame_workspace(&path, Path::new(""));
    let mut stack = vec![Frame {
        relative: PathBuf::new(),
        entries: first,
        workspace_bytes: root_workspace,
    }];
    let mut builder = Builder::new(&path, reservation);
    let (mut skipped, mut excluded_links) = (0, 0);
    let mut limit = None;
    let mut checkpoint = CHUNK_LEN;
    let mut published = false;
    let mut last_publish = Instant::now();
    let mut visits = 0usize;
    let mut workspace_bytes = root_workspace;
    if !builder.workspace(workspace_bytes, options) {
        limit = Some(LimitReason::MemoryBudget);
    }

    while !stack.is_empty() {
        if matches!(
            limit,
            Some(LimitReason::MemoryBudget | LimitReason::EntryCount)
        ) {
            break;
        }
        if visits % 256 == 0 && cancelled() {
            return Ok(None);
        }
        let frame = stack.last_mut().expect("nonempty stack");
        let item = match frame.entries.next() {
            Some(Ok(entry)) => entry,
            Some(Err(_)) => {
                skipped += 1;
                visits += 1;
                continue;
            }
            None => {
                if let Some(frame) = stack.pop() {
                    workspace_bytes = workspace_bytes.saturating_sub(frame.workspace_bytes);
                }
                continue;
            }
        };
        visits += 1;
        let raw_name = item.file_name();
        let metadata = item.metadata();
        let file_type = item.file_type().ok();
        let mut entry =
            Entry::from_name(raw_name, file_type.is_some_and(|kind| kind.is_dir()), 0, 0);
        entry.symlink = file_type.is_some_and(|kind| kind.is_symlink());
        if let Ok(metadata) = metadata {
            if file_type.is_none() {
                entry.is_dir = metadata.is_dir();
                entry.symlink = metadata.file_type().is_symlink();
            }
            entry.size = if entry.is_dir { 0 } else { metadata.len() };
            entry.modified_secs = metadata.modified().map(system_time_secs).unwrap_or(0);
            entry.created_secs = metadata.created().ok().map(system_time_secs);
            entry.accessed_secs = metadata.accessed().ok().map(system_time_secs);
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                let attributes = metadata.file_attributes();
                entry.hidden = attributes & 0x2 != 0;
                entry.is_dir = attributes & 0x10 != 0;
                // Directory reparse points include junctions as well as symbolic links.
                entry.symlink |= attributes & 0x400 != 0;
            }
            if entry.is_dir {
                entry.size = 0;
            }
        } else {
            entry.metadata_known = false;
            skipped += 1;
        }
        if options.recursive {
            entry.relative_path = Some(frame.relative.join(&entry.raw_name));
        }
        // Ordinary listings need no relative path. Recursive files move their
        // one path into the entry; only traversed directories need a second owner.
        let descend = entry
            .relative_path
            .as_ref()
            .filter(|_| entry.is_dir && !entry.symlink)
            .cloned();
        if options.recursive && entry.symlink {
            excluded_links += 1;
        }
        let bytes = match builder.admit(&entry, options) {
            Ok(bytes) => bytes,
            Err(reason) => {
                limit = Some(reason);
                break;
            }
        };
        builder.push(entry, bytes);

        if builder.entries.len >= checkpoint
            && (!published || last_publish.elapsed() >= Duration::from_millis(100))
        {
            if let Some(snapshot) = Snapshot::build(
                path.clone(),
                builder.entries.clone(),
                false,
                skipped,
                excluded_links,
                options.recursive,
                limit,
                start,
                cancelled,
            ) {
                progress(Arc::new(snapshot));
            } else {
                return Ok(None);
            }
            published = true;
            last_publish = Instant::now();
            checkpoint = builder
                .len()
                .saturating_mul(2)
                .max(checkpoint.saturating_mul(2));
        }
        if let Some(relative) = descend {
            if stack.len() >= options.max_depth {
                skipped += 1;
                limit = Some(LimitReason::Depth);
                continue;
            }
            if cancelled() {
                return Ok(None);
            }
            let child_workspace = frame_workspace(&path, &relative);
            if !builder.workspace(workspace_bytes.saturating_add(child_workspace), options) {
                limit = Some(LimitReason::MemoryBudget);
                break;
            }
            match fs::read_dir(path.join(&relative)) {
                Ok(entries) => {
                    workspace_bytes = workspace_bytes.saturating_add(child_workspace);
                    stack.push(Frame {
                        relative,
                        entries,
                        workspace_bytes: child_workspace,
                    });
                }
                Err(_) => skipped += 1,
            }
        }
    }
    if cancelled() {
        return Ok(None);
    }
    drop(stack);
    if let Some(reservation) = &builder.entries.reservation {
        reservation.shrink_to(
            builder
                .admission_bytes
                .saturating_sub(builder.workspace_peak),
        );
    }
    builder.flush();
    let shared_memory_pressure = builder.shared_memory_pressure;
    Ok(Snapshot::build(
        path,
        builder.entries,
        skipped == 0 && limit.is_none(),
        skipped,
        excluded_links,
        options.recursive,
        limit,
        start,
        cancelled,
    )
    .map(|mut snapshot| {
        snapshot.shared_memory_pressure = shared_memory_pressure;
        Arc::new(snapshot)
    }))
}

fn system_time_secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(value) => value.as_secs().min(i64::MAX as u64) as i64,
        Err(error) => {
            -(error
                .duration()
                .as_secs()
                .saturating_add(u64::from(error.duration().subsec_nanos() != 0))
                .min(i64::MAX as u64) as i64)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    pub(crate) struct Fixture {
        pub path: PathBuf,
        base: PathBuf,
    }
    impl Fixture {
        pub fn new() -> Self {
            let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/test-fixtures");
            fs::create_dir_all(&base).unwrap();
            let path = base.join(format!(
                "owned-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self { path, base }
        }
        pub fn file(&self, name: &str) {
            fs::write(self.path.join(name), b"fixture").unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let (Ok(path), Ok(base)) = (self.path.canonicalize(), self.base.canonicalize()) {
                if path.starts_with(&base) && path != base {
                    let _ = fs::remove_dir_all(path);
                }
            }
        }
    }

    #[test]
    fn ordinary_and_recursive_listings_preserve_exact_paths() {
        let fixture = Fixture::new();
        fixture.file("duplicate.txt");
        fs::create_dir(fixture.path.join("nested")).unwrap();
        fs::write(fixture.path.join("nested/duplicate.txt"), b"nested").unwrap();
        #[cfg(windows)]
        let raw = {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[b'x' as u16, 0xd800, b'y' as u16])
        };
        #[cfg(unix)]
        let raw = {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(vec![b'x', 0xff, b'y'])
        };
        let raw_relative = PathBuf::from("nested").join(&raw);
        fs::write(fixture.path.join(&raw_relative), b"raw").unwrap();

        let ordinary = enumerate_directory(&fixture.path).unwrap();
        assert!(ordinary.complete);
        assert_eq!(ordinary.entries.len(), 2);
        assert!(
            ordinary
                .entries
                .iter()
                .all(|entry| entry.relative_path.is_none())
        );
        assert!(
            ordinary
                .entries
                .iter()
                .all(|entry| entry.path(&ordinary).exists())
        );

        let recursive = scan_recursive(&fixture.path).unwrap();
        assert!(recursive.complete);
        assert_eq!(recursive.entries.len(), 4);
        for relative in [
            PathBuf::from("duplicate.txt"),
            PathBuf::from("nested"),
            PathBuf::from("nested/duplicate.txt"),
            raw_relative,
        ] {
            let entry = recursive
                .entries
                .iter()
                .find(|entry| entry.relative_path.as_deref() == Some(relative.as_path()))
                .expect("every traversed relative identity is retained exactly");
            assert_eq!(entry.path(&recursive), fixture.path.join(relative));
        }
    }

    #[test]
    fn scan_cap_is_explicit_and_keeps_exact_relative_paths() {
        let fixture = Fixture::new();
        for i in 0..10 {
            fixture.file(&format!("item{i}.txt"));
        }
        let result = collect(
            fixture.path.clone(),
            CollectOptions {
                recursive: true,
                max_entries: 3,
                max_bytes: 1024 * 1024,
                max_depth: 128,
            },
            &|| false,
            None,
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.limit, Some(LimitReason::EntryCount));
        assert!(!result.complete);
        assert!(
            result
                .entries
                .iter()
                .all(|entry| entry.relative_path.is_some() && entry.path(&result).exists())
        );
    }

    #[test]
    fn byte_limit_stops_independently_of_entry_count() {
        let fixture = Fixture::new();
        for i in 0..100 {
            fixture.file(&format!("quite_a_long_filename_number_{i:03}.txt"));
        }
        let result = collect(
            fixture.path.clone(),
            CollectOptions {
                recursive: true,
                max_entries: 1000,
                max_bytes: CHUNK_LEN * size_of::<Entry>() + 4096,
                max_depth: 128,
            },
            &|| false,
            None,
            &mut |_| {},
        )
        .unwrap()
        .unwrap();
        assert!(result.entries.len() < 100);
        assert_eq!(result.limit, Some(LimitReason::MemoryBudget));
        assert!(!result.complete);
    }

    #[test]
    fn cancelled_scan_does_not_open_a_missing_target() {
        let result = collect(
            PathBuf::from("missing-cancelled-fixture"),
            CollectOptions {
                recursive: true,
                max_entries: 10,
                max_bytes: 1024 * 1024,
                max_depth: 128,
            },
            &|| true,
            None,
            &mut |_| {},
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn recursive_scan_does_not_follow_directory_links() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        outside.file("must_not_be_visited.txt");
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&outside.path, fixture.path.join("linked"));
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&outside.path, fixture.path.join("linked"));
        if let Err(error) = linked {
            // Windows symlink creation requires developer mode or privilege; ordinary traversal still runs.
            fixture.file("ordinary.txt");
            eprintln!("directory-link fixture unavailable on this host: {error}");
            assert_eq!(scan_recursive(&fixture.path).unwrap().entries.len(), 1);
            return;
        }
        let result = scan_recursive(&fixture.path).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert!(result.entries[0].symlink);
        assert_eq!(result.excluded_links, 1);
        assert!(
            !result
                .entries
                .iter()
                .any(|entry| entry.name == "must_not_be_visited.txt")
        );
    }

    #[test]
    fn progressive_snapshots_share_full_entry_chunks() {
        let fixture = Fixture::new();
        for i in 0..300 {
            fixture.file(&format!("item{i}"));
        }
        let mut progress = Vec::new();
        let result = collect(
            fixture.path.clone(),
            CollectOptions {
                recursive: false,
                max_entries: 1000,
                max_bytes: 1024 * 1024,
                max_depth: 1,
            },
            &|| false,
            None,
            &mut |snapshot| progress.push(snapshot),
        )
        .unwrap()
        .unwrap();
        assert!(!progress.is_empty());
        assert!(Arc::ptr_eq(
            &progress[0].entries.chunks[0],
            &result.entries.chunks[0]
        ));
        assert!(result.complete);
    }
}
