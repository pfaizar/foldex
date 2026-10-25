//! Current Shell cut selection, independent of the opt-in launcher history.
//!
//! A clipboard event invalidates the previous generation immediately. One
//! replaceable worker result holds at most 4,096 exact paths / 1 MiB of UTF-16;
//! no clipboard data or filesystem metadata is read by a painter. Visible path
//! keys are prepared only when a viewport/source changes, then hash lookups do
//! not allocate. Cutting a folder marks that folder, never its descendants.
use std::{
    collections::{HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
};

#[cfg(windows)]
mod native;

pub const CUT_OPACITY: f32 = 0.5;
const MAX_PATHS: usize = 4_096;
const MAX_PATH_BYTES: usize = 1024 * 1024;
const MAX_PATH_UNITS: usize = 32_767;
type Wake = Arc<dyn Fn() + Send + Sync>;

pub fn content_opacity(cut: bool) -> f32 {
    if cut { CUT_OPACITY } else { 1.0 }
}

/// Identity of the currently published immutable selection. Changes also cover
/// invalidation-before-read, a completed read, fixture replacement and Escape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    generation: u64,
    snapshot: usize,
    suppressed: bool,
}

/// Prepared lexical identity. Original units verify equality after hashing:
/// Unicode mappings must never make distinct filesystem names compare equal.
#[derive(Clone, Debug)]
pub struct PathKey {
    units: Box<[u16]>,
    hash: u64,
}
impl PathKey {
    pub fn new(path: &Path) -> Option<Self> {
        if !path.is_absolute() {
            return None;
        }
        #[cfg(windows)]
        let units: Vec<_> = {
            use std::os::windows::ffi::OsStrExt;
            path.as_os_str().encode_wide().collect()
        };
        #[cfg(not(windows))]
        let units: Vec<_> = path.to_str()?.encode_utf16().collect();
        Self::from_units(&units)
    }

    fn from_units(units: &[u16]) -> Option<Self> {
        if units.is_empty() || units.len() > MAX_PATH_UNITS || units.contains(&0) {
            return None;
        }
        #[cfg(any(windows, test))]
        let units = crate::bookmarks::windows_lexical(units);
        #[cfg(not(any(windows, test)))]
        let units = units.to_vec();
        let mut hasher = DefaultHasher::new();
        #[cfg(windows)]
        {
            use windows_sys::Win32::Globalization::{LCMAP_UPPERCASE, LCMapStringEx};
            // An empty locale name selects LOCALE_NAME_INVARIANT. Without
            // LCMAP_LINGUISTIC_CASING, Windows uses filesystem casing rules.
            // https://learn.microsoft.com/windows/win32/api/winnls/nf-winnls-lcmapstringex
            let locale = [0u16];
            let length = unsafe {
                LCMapStringEx(
                    locale.as_ptr(),
                    LCMAP_UPPERCASE,
                    units.as_ptr(),
                    units.len() as i32,
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            };
            if length <= 0 || length as usize > MAX_PATH_UNITS * 2 {
                return None;
            }
            let mut folded = vec![0u16; length as usize];
            if unsafe {
                LCMapStringEx(
                    locale.as_ptr(),
                    LCMAP_UPPERCASE,
                    units.as_ptr(),
                    units.len() as i32,
                    folded.as_mut_ptr(),
                    length,
                    std::ptr::null(),
                    std::ptr::null(),
                    0,
                )
            } != length
            {
                return None;
            }
            folded.hash(&mut hasher);
        }
        #[cfg(not(windows))]
        units.hash(&mut hasher);
        Some(Self {
            units: units.into_boxed_slice(),
            hash: hasher.finish(),
        })
    }
}
impl Hash for PathKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.hash.hash(state);
    }
}
impl PartialEq for PathKey {
    fn eq(&self, other: &Self) -> bool {
        if self.units == other.units {
            return true;
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
            unsafe {
                CompareStringOrdinal(
                    self.units.as_ptr(),
                    self.units.len() as i32,
                    other.units.as_ptr(),
                    other.units.len() as i32,
                    1,
                ) == CSTR_EQUAL
            }
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}
impl Eq for PathKey {}

#[derive(Default)]
struct CutSnapshot {
    generation: u64,
    sequence: u32,
    paths: HashSet<PathKey>,
    path_bytes: usize,
}
impl CutSnapshot {
    fn insert(&mut self, path: PathKey) -> io::Result<()> {
        // Count duplicates against the decoded input budget too. A malicious
        // CF_HDROP cannot force unbounded processing with repeated names.
        let bytes = path.units.len().saturating_mul(2);
        if self.paths.len() >= MAX_PATHS || self.path_bytes.saturating_add(bytes) > MAX_PATH_BYTES {
            return Err(invalid("Cut selection exceeds the bounded path budget"));
        }
        self.path_bytes += bytes;
        self.paths.insert(path);
        Ok(())
    }

    fn from_paths(generation: u64, sequence: u32, paths: &[PathBuf]) -> io::Result<Self> {
        if paths.len() > MAX_PATHS {
            return Err(invalid("Cut selection exceeds 4,096 paths"));
        }
        let mut snapshot = Self {
            generation,
            sequence,
            ..Self::default()
        };
        for path in paths {
            snapshot.insert(PathKey::new(path).ok_or_else(|| invalid("Invalid cut path"))?)?;
        }
        Ok(snapshot)
    }
}

struct Shared {
    generation: AtomicU64,
    stopped: AtomicBool,
    // sequence + 1 preserves the full u32 sequence range, including wraparound.
    cancel: AtomicU64,
    result: Mutex<Option<Arc<CutSnapshot>>>,
    signal: SyncSender<()>,
    wake: Wake,
}
impl Shared {
    fn changed(&self) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let _ = self.signal.try_send(());
        (self.wake)();
        generation
    }

    fn publish(&self, snapshot: CutSnapshot) {
        if self.stopped.load(Ordering::Acquire)
            || self.generation.load(Ordering::Acquire) != snapshot.generation
        {
            return;
        }
        let mut result = self
            .result
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.generation.load(Ordering::Acquire) != snapshot.generation {
            return;
        }
        *result = Some(Arc::new(snapshot));
        drop(result);
        (self.wake)();
    }
}

pub struct Service {
    shared: Arc<Shared>,
    snapshot: Arc<CutSnapshot>,
    suppressed: Option<u64>,
    #[cfg(windows)]
    observer: Option<native::Observer>,
}
impl Service {
    fn channel(wake: Wake) -> (Self, Receiver<()>) {
        let (signal, receiver) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            cancel: AtomicU64::new(0),
            result: Mutex::new(None),
            signal,
            wake,
        });
        (
            Self {
                shared,
                snapshot: Arc::default(),
                suppressed: None,
                #[cfg(windows)]
                observer: None,
            },
            receiver,
        )
    }

    /// No native threads or clipboard calls; also used by normal unit fixtures.
    pub fn disabled() -> Self {
        Self::channel(Arc::new(|| {})).0
    }

    pub fn new(wake: Wake) -> io::Result<Self> {
        let (mut service, receiver) = Self::channel(wake);
        #[cfg(windows)]
        {
            service.observer = Some(native::Observer::start(service.shared.clone(), receiver)?);
        }
        #[cfg(not(windows))]
        {
            let _ = (&mut service, receiver);
        }
        Ok(service)
    }

    /// Try-lock only. Old generations are unusable even before this is drained.
    pub fn begin_frame(&mut self) {
        let generation = self.shared.generation.load(Ordering::Acquire);
        if self.snapshot.generation != generation {
            self.snapshot = Arc::new(CutSnapshot {
                generation,
                ..Default::default()
            });
        }
        if self.suppressed.is_some_and(|old| old != generation) {
            self.suppressed = None;
        }
        if let Ok(mut result) = self.shared.result.try_lock()
            && let Some(snapshot) = result.take()
            && snapshot.generation == generation
        {
            self.snapshot = snapshot;
        }
    }

    pub fn contains(&self, key: Option<&PathKey>) -> bool {
        self.snapshot.generation == self.shared.generation.load(Ordering::Acquire)
            && self.suppressed != Some(self.snapshot.generation)
            && key.is_some_and(|key| self.snapshot.paths.contains(key))
    }

    pub fn version(&self) -> Version {
        Version {
            generation: self.shared.generation.load(Ordering::Acquire),
            snapshot: Arc::as_ptr(&self.snapshot) as usize,
            suppressed: self.suppressed == Some(self.snapshot.generation),
        }
    }

    pub fn has_paths(&self) -> bool {
        !self.snapshot.paths.is_empty()
            && self.snapshot.generation == self.shared.generation.load(Ordering::Acquire)
            && self.suppressed != Some(self.snapshot.generation)
    }

    /// Escape only cancels the still-current file cut. The worker checks the
    /// native sequence and MOVE preference before clearing it; a replacement
    /// clipboard (including a new copy) is never modified by this request.
    pub fn cancel(&mut self) -> bool {
        let generation = self.shared.generation.load(Ordering::Acquire);
        if self.snapshot.generation != generation
            || self.snapshot.paths.is_empty()
            || self.suppressed == Some(generation)
        {
            return false;
        }
        self.suppressed = Some(generation);
        self.shared
            .cancel
            .store(u64::from(self.snapshot.sequence) + 1, Ordering::Release);
        let _ = self.shared.signal.try_send(());
        (self.shared.wake)();
        true
    }

    /// Immutable fixture snapshot. This constructor performs no native calls.
    pub fn fixture(paths: &[PathBuf]) -> io::Result<Self> {
        let mut service = Self::disabled();
        service.snapshot = Arc::new(CutSnapshot::from_paths(0, 0, paths)?);
        Ok(service)
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        let _ = self.shared.signal.try_send(());
        #[cfg(windows)]
        if let Some(observer) = &self.observer {
            observer.stop();
        }
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/cut-fixture")
            .join(name)
    }
    fn publish(service: &Service, generation: u64, names: &[&str]) {
        let paths: Vec<_> = names.iter().map(|name| path(name)).collect();
        service
            .shared
            .publish(CutSnapshot::from_paths(generation, 42, &paths).unwrap());
    }

    #[test]
    fn clipboard_change_invalidates_old_paths_before_worker_finishes() {
        let (mut service, _) = Service::channel(Arc::new(|| {}));
        let first = service.shared.changed();
        publish(&service, first, &["one.txt", "folder"]);
        service.begin_frame();
        let one = PathKey::new(&path("one.txt")).unwrap();
        assert!(service.contains(Some(&one)));
        let next = service.shared.changed();
        assert!(
            !service.contains(Some(&one)),
            "No stale cut while a new clipboard is read"
        );
        publish(&service, first, &["one.txt"]);
        service.begin_frame();
        assert!(!service.contains(Some(&one)));
        publish(&service, next, &["two.txt"]);
        service.begin_frame();
        assert!(!service.contains(Some(&one)));
        assert!(service.contains(PathKey::new(&path("two.txt")).as_ref()));
    }

    #[test]
    fn copy_empty_or_failed_snapshot_clears_cut_and_results_replace_instead_of_growing() {
        let (mut service, _) = Service::channel(Arc::new(|| {}));
        let generation = service.shared.changed();
        publish(&service, generation, &["one.txt"]);
        publish(&service, generation, &["two.txt"]);
        service.begin_frame();
        assert!(!service.contains(PathKey::new(&path("one.txt")).as_ref()));
        assert!(service.contains(PathKey::new(&path("two.txt")).as_ref()));
        let generation = service.shared.changed();
        service.shared.publish(CutSnapshot {
            generation,
            sequence: 43,
            ..Default::default()
        });
        service.begin_frame();
        assert!(!service.contains(PathKey::new(&path("two.txt")).as_ref()));
        assert!(service.snapshot.paths.is_empty());
    }

    #[test]
    fn cancel_suppresses_only_the_current_clipboard_generation() {
        let (mut service, receiver) = Service::channel(Arc::new(|| {}));
        let generation = service.shared.changed();
        publish(&service, generation, &["one.txt"]);
        service.begin_frame();
        assert!(service.cancel());
        assert!(!service.cancel());
        assert_eq!(service.shared.cancel.load(Ordering::Acquire), 43);
        assert!(receiver.try_recv().is_ok());
        publish(&service, generation, &["one.txt"]);
        service.begin_frame();
        assert!(!service.contains(PathKey::new(&path("one.txt")).as_ref()));
        let next = service.shared.changed();
        publish(&service, next, &["one.txt"]);
        service.begin_frame();
        assert!(service.contains(PathKey::new(&path("one.txt")).as_ref()));
    }

    #[test]
    fn exact_paths_do_not_dim_homonyms_or_children_and_budget_rejects_overflow() {
        let service = Service::fixture(&[path("folder"), path("one.txt")]).unwrap();
        for name in [
            "folder/child.txt",
            "folderish",
            "other/one.txt",
            "one.txt.bak",
        ] {
            assert!(
                !service.contains(PathKey::new(&path(name)).as_ref()),
                "{name}"
            );
        }
        assert!(service.contains(PathKey::new(&path("folder")).as_ref()));
        assert!(Service::fixture(&vec![path("one.txt"); MAX_PATHS + 1]).is_err());
        let long = path(&"x".repeat(32_000));
        assert!(Service::fixture(&vec![long; 17]).is_err());
        assert!(PathKey::new(Path::new("relative.txt")).is_none());
        assert!(PathKey::from_units(&[65, 0, 66]).is_none());
    }

    #[test]
    fn a_contended_result_never_blocks_the_frame_or_reenables_a_stale_generation() {
        let (mut service, _) = Service::channel(Arc::new(|| {}));
        let generation = service.shared.changed();
        publish(&service, generation, &["one.txt"]);
        service.begin_frame();
        let shared = service.shared.clone();
        let _held = shared.result.lock().unwrap();
        shared.changed();
        service.begin_frame();
        assert!(!service.contains(PathKey::new(&path("one.txt")).as_ref()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_case_and_long_prefix_share_a_hash_but_distinct_unicode_names_do_not_match() {
        let key = PathKey::new(Path::new(r"C:\Cut\Ärger.txt")).unwrap();
        let alternate = PathKey::new(Path::new(r"\\?\c:\cut\ärger.txt")).unwrap();
        assert_eq!(key, alternate);
        assert_eq!(key.hash, alternate.hash);
        let set = HashSet::from([key]);
        assert!(set.contains(&alternate));
        assert_ne!(
            PathKey::new(Path::new(r"C:\cut\ß.txt")),
            PathKey::new(Path::new(r"C:\cut\SS.txt"))
        );
        assert_ne!(
            PathKey::new(Path::new(r"\\?\C:\cut\file. ")),
            PathKey::new(Path::new(r"C:\cut\file"))
        );
    }
}
