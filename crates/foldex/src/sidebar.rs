//! Sidebar preferences and one bounded metadata worker. Painting and preferences
//! never query a disk or a Known Folder. Unavailable locations remain usable.
use serde::{Deserialize, Serialize};
pub mod network;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

pub const ICON_VIEW: u64 = u64::MAX - 4;
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// Stable discriminants preserve the original sidebar_collapsed bit mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Recents,
    Bookmarks,
    Storage,
    Places,
    Network,
}
impl Section {
    pub const COUNT: usize = 5;
    pub const LEGACY_COUNT: usize = 4;
    pub const DEFAULT_ORDER: [Self; Self::COUNT] = [
        Self::Bookmarks,
        Self::Storage,
        Self::Places,
        Self::Network,
        Self::Recents,
    ];
    pub fn index(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Recents => "Recents",
            Self::Bookmarks => "Bookmarks",
            Self::Storage => "Storage",
            Self::Places => "Places",
            Self::Network => "Network",
        }
    }
}

/// Section visibility is independent of ordering and expansion. Missing fields
/// in older or partially specified sessions keep their native visible defaults.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct SectionVisibility {
    pub bookmarks: bool,
    pub storage: bool,
    pub places: bool,
    pub network: bool,
    pub recents: bool,
}
impl Default for SectionVisibility {
    fn default() -> Self {
        Self {
            bookmarks: true,
            storage: true,
            places: true,
            network: true,
            recents: true,
        }
    }
}
impl SectionVisibility {
    pub fn visible(&self, section: Section) -> bool {
        match section {
            Section::Bookmarks => self.bookmarks,
            Section::Storage => self.storage,
            Section::Places => self.places,
            Section::Network => self.network,
            Section::Recents => self.recents,
        }
    }
    pub fn toggle(&mut self, section: Section) {
        let value = match section {
            Section::Bookmarks => &mut self.bookmarks,
            Section::Storage => &mut self.storage,
            Section::Places => &mut self.places,
            Section::Network => &mut self.network,
            Section::Recents => &mut self.recents,
        };
        *value = !*value;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaceKind {
    User,
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
    RecycleBin,
}
impl PlaceKind {
    pub const ALL: [Self; 8] = [
        Self::User,
        Self::Desktop,
        Self::Downloads,
        Self::Documents,
        Self::Pictures,
        Self::Music,
        Self::Videos,
        Self::RecycleBin,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::User => "User",
            Self::Desktop => "Desktop",
            Self::Downloads => "Downloads",
            Self::Documents => "Documents",
            Self::Pictures => "Pictures",
            Self::Music => "Music",
            Self::Videos => "Videos",
            Self::RecycleBin => "Recycle Bin",
        }
    }
    pub fn icon_key(self, size: u16) -> foldex_assets::AssetKey {
        if self == Self::RecycleBin {
            return foldex_assets::AssetKey::stock_icon(foldex_assets::StockIcon::RecycleBin, size);
        }
        use foldex_assets::KnownFolderIcon as Native;
        foldex_assets::AssetKey::known_folder_icon(
            match self {
                Self::User => Native::Profile,
                Self::Desktop => Native::Desktop,
                Self::Downloads => Native::Downloads,
                Self::Documents => Native::Documents,
                Self::Pictures => Native::Pictures,
                Self::Music => Native::Music,
                Self::Videos => Native::Videos,
                Self::RecycleBin => unreachable!(),
            },
            size,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct PlacesVisibility {
    pub user: bool,
    pub desktop: bool,
    pub downloads: bool,
    pub documents: bool,
    pub pictures: bool,
    pub music: bool,
    pub videos: bool,
    pub recycle_bin: bool,
}
impl Default for PlacesVisibility {
    fn default() -> Self {
        Self {
            user: true,
            desktop: true,
            downloads: true,
            documents: true,
            pictures: true,
            music: true,
            videos: true,
            recycle_bin: true,
        }
    }
}
impl PlacesVisibility {
    pub fn visible(&self, kind: PlaceKind) -> bool {
        match kind {
            PlaceKind::User => self.user,
            PlaceKind::Desktop => self.desktop,
            PlaceKind::Downloads => self.downloads,
            PlaceKind::Documents => self.documents,
            PlaceKind::Pictures => self.pictures,
            PlaceKind::Music => self.music,
            PlaceKind::Videos => self.videos,
            PlaceKind::RecycleBin => self.recycle_bin,
        }
    }
    pub fn toggle(&mut self, kind: PlaceKind) {
        let value = match kind {
            PlaceKind::User => &mut self.user,
            PlaceKind::Desktop => &mut self.desktop,
            PlaceKind::Downloads => &mut self.downloads,
            PlaceKind::Documents => &mut self.documents,
            PlaceKind::Pictures => &mut self.pictures,
            PlaceKind::Music => &mut self.music,
            PlaceKind::Videos => &mut self.videos,
            PlaceKind::RecycleBin => &mut self.recycle_bin,
        };
        *value = !*value;
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub order: Vec<Section>,
    pub sections: SectionVisibility,
    pub places: PlacesVisibility,
    pub storage_usage: bool,
    /// Kept separately so the original four sidebar_collapsed bits stay stable.
    pub network_expanded: bool,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            order: Section::DEFAULT_ORDER.to_vec(),
            sections: Default::default(),
            places: Default::default(),
            storage_usage: true,
            network_expanded: false,
        }
    }
}
impl Preferences {
    pub fn normalize(&mut self) {
        let mut order = Vec::with_capacity(Section::COUNT);
        for section in self.order.iter().copied() {
            if !order.contains(&section) {
                order.push(section);
            }
        }
        // Legacy complete orders with Recents last retain that convention when
        // Network is introduced. Existing custom Network positions and the
        // relative order of every saved category remain unchanged.
        if order.len() == Section::LEGACY_COUNT
            && order.last() == Some(&Section::Recents)
            && !order.contains(&Section::Network)
        {
            order.insert(order.len() - 1, Section::Network);
        }
        for section in Section::DEFAULT_ORDER {
            if !order.contains(&section) {
                order.push(section);
            }
        }
        self.order = order;
    }
    pub fn move_by(&mut self, section: Section, offset: isize) -> bool {
        self.normalize();
        let Some(index) = self.order.iter().position(|item| *item == section) else {
            return false;
        };
        let Some(next) = index
            .checked_add_signed(offset)
            .filter(|next| *next < self.order.len())
        else {
            return false;
        };
        self.order.swap(index, next);
        true
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub kind: PlaceKind,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    pub total: u64,
    pub free: u64,
}
impl Usage {
    pub fn from_bytes(total: u64, free: u64) -> Option<Self> {
        (total > 0).then_some(Self {
            total,
            free: free.min(total),
        })
    }
    pub fn fraction(self) -> f32 {
        ((self.total - self.free) as f64 / self.total as f64) as f32
    }
    pub fn near_full(self) -> bool {
        u128::from(self.total - self.free) * 10 >= u128::from(self.total) * 9
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub path: PathBuf,
    pub label: String,
    pub usage: Option<Usage>,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Snapshot {
    places: Vec<Place>,
    volumes: Vec<Volume>,
}
struct Publication {
    snapshot: Snapshot,
    complete: bool,
}

fn publish(output: &Mutex<Option<Publication>>, publication: Publication, wake: &dyn Fn()) {
    let needs_wake = {
        let mut slot = output.lock().unwrap_or_else(|error| error.into_inner());
        let needs_wake = slot.is_none();
        *slot = Some(publication);
        needs_wake
    };
    // One latest-result slot also needs just one queued UI wake. Keep the
    // callback outside the lock so the UI can drain it immediately.
    if needs_wake {
        wake();
    }
}

#[cfg(any(windows, test))]
fn discovered_volume(path: PathBuf, fallback: String, previous: &[Volume]) -> Volume {
    let old = previous.iter().find(|volume| volume.path == path);
    Volume {
        label: old.map_or(fallback, |volume| volume.label.clone()),
        usage: old.and_then(|volume| volume.usage),
        path,
    }
}

pub struct Data {
    pub network: network::Data,
    signal: SyncSender<()>,
    result: Arc<Mutex<Option<Publication>>>,
    snapshot: Snapshot,
    pending: bool,
    next_refresh: Instant,
    revision: u64,
    // Native capture harness only; at most 26 cloned records. The live worker
    // continues updating snapshot while a deterministic capture is displayed.
    benchmark_volumes: Option<Vec<Volume>>,
}
impl Data {
    pub fn new(wake: Arc<dyn Fn() + Send + Sync>) -> io::Result<Self> {
        let network = network::Data::new(wake.clone());
        let (signal, requests) = mpsc::sync_channel(1);
        let result = Arc::new(Mutex::new(None));
        let output = result.clone();
        thread::Builder::new()
            .name("foldex-locations".into())
            .spawn(move || {
                #[cfg(windows)]
                let _apartment = native::Apartment::new();
                let mut previous = Snapshot::default();
                while requests.recv().is_ok() {
                    let mut completed = None;
                    snapshot(&previous, |snapshot, complete| {
                        if complete {
                            completed = Some(snapshot.clone());
                        }
                        publish(&output, Publication { snapshot, complete }, wake.as_ref());
                    });
                    if let Some(snapshot) = completed {
                        previous = snapshot;
                    }
                }
            })?;
        let data = Self {
            network,
            signal,
            result,
            snapshot: Snapshot {
                places: fallback_places(),
                volumes: Vec::new(),
            },
            pending: false,
            next_refresh: Instant::now(),
            revision: 1,
            benchmark_volumes: None,
        };
        Ok(data)
    }
    /// Returns true when cached paths/drive metadata are replaced. try_lock and
    /// try_send keep the UI independent of slow/unavailable media. One request,
    /// one result, at most seven places and twenty-six drive records are retained.
    pub fn service(&mut self, ctx: &egui::Context, active: bool) -> bool {
        let snapshot = self.result.try_lock().ok().and_then(|mut slot| slot.take());
        let mut changed = false;
        if let Some(publication) = snapshot {
            if self.snapshot != publication.snapshot {
                self.snapshot = publication.snapshot;
                self.revision = self.revision.wrapping_add(1);
                changed = true;
            }
            self.pending = !publication.complete;
            if publication.complete {
                self.next_refresh = Instant::now() + REFRESH_INTERVAL;
            }
        }
        // Hidden explorer/sidebar frames still drain a completed cached result,
        // but never start work or schedule their own wakeup.
        if active && !self.pending {
            if Instant::now() >= self.next_refresh {
                self.request_refresh();
            } else {
                let remaining = self.next_refresh.saturating_duration_since(Instant::now());
                // egui subtracts predicted_dt from delayed repaint requests.
                // Compensate so a refresh wake cannot turn into an immediate
                // repaint loop just before the native sample deadline.
                let predicted = Duration::try_from_secs_f32(ctx.input(|input| input.predicted_dt))
                    .unwrap_or_default();
                ctx.request_repaint_after(remaining.saturating_add(predicted));
            }
        }
        changed
    }
    fn request_refresh(&mut self) {
        if self.pending {
            return;
        }
        if self.signal.try_send(()).is_ok() {
            self.pending = true;
        } else {
            self.next_refresh = Instant::now() + REFRESH_INTERVAL;
        }
    }
    pub fn places(&self) -> &[Place] {
        &self.snapshot.places
    }
    pub fn volumes(&self) -> &[Volume] {
        self.benchmark_volumes
            .as_deref()
            .unwrap_or(&self.snapshot.volumes)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn usage(&self, path: &Path) -> Option<Usage> {
        self.volumes()
            .iter()
            .find(|volume| crate::bookmarks::path_eq(&volume.path, path))
            .and_then(|volume| volume.usage)
    }
    /// Capture harness only: override cached drive usage in display order.
    /// None means unknown capacity; unspecified remaining drives stay native.
    /// Returns the number of overrides applied, or zero before drive discovery.
    /// No disk call, refresh request, worker or timer is started by this helper.
    pub(crate) fn benchmark_override_usage(&mut self, usage: &[Option<Usage>]) -> usize {
        if usage.is_empty() || self.snapshot.volumes.is_empty() {
            return 0;
        }
        let mut volumes: Vec<_> = self.snapshot.volumes.iter().take(26).cloned().collect();
        let count = volumes.len().min(usage.len());
        for (volume, usage) in volumes.iter_mut().zip(usage.iter().take(26)) {
            volume.usage = *usage;
        }
        self.benchmark_volumes = Some(volumes);
        self.revision = self.revision.wrapping_add(1);
        count
    }
    /// Restores the newest real cached sample, including worker publications
    /// received during the capture. Use at the beginning of the next scene.
    pub(crate) fn benchmark_restore_usage(&mut self) -> bool {
        if self.benchmark_volumes.take().is_none() {
            return false;
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

fn fallback_places() -> Vec<Place> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .map(|path| {
            vec![Place {
                kind: PlaceKind::User,
                path,
            }]
        })
        .unwrap_or_default()
}

fn resolve_places(mut resolve: impl FnMut(PlaceKind) -> Option<PathBuf>) -> Vec<Place> {
    PlaceKind::ALL
        .into_iter()
        .filter_map(|kind| {
            if kind == PlaceKind::RecycleBin {
                return None;
            }
            let path = resolve(kind)?;
            path.is_absolute().then_some(Place { kind, path })
        })
        .collect()
}

fn snapshot(_previous: &Snapshot, mut publish: impl FnMut(Snapshot, bool)) {
    #[cfg(windows)]
    {
        let places = resolve_places(native::known_folder);
        native::volumes(&_previous.volumes, |volumes, complete| {
            publish(
                Snapshot {
                    places: places.clone(),
                    volumes,
                },
                complete,
            )
        });
    }
    #[cfg(not(windows))]
    {
        publish(
            Snapshot {
                places: fallback_places(),
                volumes: Vec::new(),
            },
            true,
        );
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::{
        Storage::FileSystem::{
            GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        },
        System::{
            Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize},
            Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode},
        },
        UI::Shell::{
            FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
            FOLDERID_Pictures, FOLDERID_Profile, FOLDERID_Videos, KF_FLAG_DONT_VERIFY,
            SHGetKnownFolderPath,
        },
    };
    pub(super) struct Apartment(bool);
    impl Apartment {
        pub(super) fn new() -> Self {
            unsafe {
                // This is a dedicated worker; prevent removable-media dialogs
                // without changing error mode on the UI or other workers.
                SetThreadErrorMode(SEM_FAILCRITICALERRORS, std::ptr::null_mut());
                Self(CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) >= 0)
            }
        }
    }
    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }
    pub(super) fn known_folder(kind: PlaceKind) -> Option<PathBuf> {
        let id = match kind {
            PlaceKind::User => FOLDERID_Profile,
            PlaceKind::Desktop => FOLDERID_Desktop,
            PlaceKind::Downloads => FOLDERID_Downloads,
            PlaceKind::Documents => FOLDERID_Documents,
            PlaceKind::Pictures => FOLDERID_Pictures,
            PlaceKind::Music => FOLDERID_Music,
            PlaceKind::Videos => FOLDERID_Videos,
            PlaceKind::RecycleBin => return None,
        };
        let mut output = std::ptr::null_mut();
        unsafe {
            // No CREATE or DEFAULT_PATH: honor OneDrive/redirection without
            // verifying an offline share or creating a missing directory.
            let result = SHGetKnownFolderPath(
                &id,
                KF_FLAG_DONT_VERIFY as u32,
                std::ptr::null_mut(),
                &mut output,
            );
            if result < 0 || output.is_null() {
                return None;
            }
            let mut length = 0;
            while length < 32_768 && *output.add(length) != 0 {
                length += 1;
            }
            let path = (length < 32_768).then(|| {
                PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(
                    output, length,
                )))
            });
            CoTaskMemFree(output.cast());
            path
        }
    }
    pub(super) fn volumes(previous: &[Volume], mut publish: impl FnMut(Vec<Volume>, bool)) {
        let mask = unsafe { GetLogicalDrives() };
        let mut volumes = Vec::with_capacity(mask.count_ones().min(26) as usize);
        let mut order = Vec::with_capacity(volumes.capacity());
        for letter in 0..26 {
            if mask & (1 << letter) == 0 {
                continue;
            }
            let path = PathBuf::from(format!("{}:\\", (b'A' + letter as u8) as char));
            let root: Vec<_> = path.as_os_str().encode_wide().chain([0]).collect();
            let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
            let fallback = match kind {
                2 => "Removable disk",
                4 => "Network drive",
                5 => "Optical drive",
                6 => "RAM disk",
                _ => "Local disk",
            };
            let fallback = format!("{fallback} ({}:)", (b'A' + letter as u8) as char);
            order.push((matches!(kind, 4 | 5), volumes.len(), fallback.clone()));
            // Retain the last sample while refreshing an already-known drive.
            // A failed fresh query below still replaces it with unavailable.
            volumes.push(discovered_volume(path, fallback, previous));
        }
        // Publish redirected folders and all drive roots before querying any
        // media. A slow mapped drive cannot hide the already-measured local ones.
        publish(volumes.clone(), volumes.is_empty());
        order.sort_by_key(|(slow, _, _)| *slow);
        let count = order.len();
        for (progress, (_, index, fallback)) in order.into_iter().enumerate() {
            let volume = &mut volumes[index];
            let root: Vec<_> = volume.path.as_os_str().encode_wide().chain([0]).collect();
            let mut label = [0u16; 261];
            let mut available = 0;
            let mut total = 0;
            let mut free = 0;
            // All calls run on this one worker. Mapped/offline volumes can be
            // slow, but can never stall a frame or accumulate more workers.
            let named = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    label.as_mut_ptr(),
                    label.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                )
            } != 0;
            let measured = unsafe {
                GetDiskFreeSpaceExW(root.as_ptr(), &mut available, &mut total, &mut free)
            } != 0;
            let name = if named {
                let end = label
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(label.len());
                String::from_utf16_lossy(&label[..end])
            } else {
                String::new()
            };
            volume.label = if !name.is_empty() {
                format!(
                    "{} ({})",
                    name,
                    volume.path.to_string_lossy().trim_end_matches('\\')
                )
            } else {
                fallback
            };
            // The denominator and free bytes use the caller-visible quota
            // pair. These are u64 throughout; no false 32-bit capacities.
            volume.usage = measured
                .then(|| Usage::from_bytes(total, available))
                .flatten();
            publish(volumes.clone(), progress + 1 == count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox_fixture() -> (Data, mpsc::Receiver<()>) {
        let (signal, receiver) = mpsc::sync_channel(1);
        (
            Data {
                network: network::Data::new(Arc::new(|| {})),
                signal,
                result: Arc::new(Mutex::new(None)),
                snapshot: Snapshot::default(),
                pending: false,
                next_refresh: Instant::now(),
                revision: 0,
                benchmark_volumes: None,
            },
            receiver,
        )
    }

    #[test]
    fn multiple_publications_share_one_queued_wake_and_keep_the_latest_sample() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let output = Mutex::new(None);
        let wakes = AtomicUsize::new(0);
        let wake = || {
            wakes.fetch_add(1, Ordering::Relaxed);
        };
        for count in 0..27 {
            publish(
                &output,
                Publication {
                    snapshot: Snapshot {
                        places: vec![],
                        volumes: vec![Volume {
                            path: "C:/".into(),
                            label: count.to_string(),
                            usage: Usage::from_bytes(100, count),
                        }],
                    },
                    complete: count == 26,
                },
                &wake,
            );
        }
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        let latest = output.lock().unwrap().take().unwrap();
        assert_eq!(latest.snapshot.volumes[0].label, "26");
        assert!(latest.complete);
        publish(
            &output,
            Publication {
                snapshot: Snapshot::default(),
                complete: true,
            },
            &wake,
        );
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn unchanged_complete_sample_clears_pending_without_invalidating_the_display() {
        let (mut data, receiver) = mailbox_fixture();
        data.pending = true;
        *data.result.lock().unwrap() = Some(Publication {
            snapshot: data.snapshot.clone(),
            complete: true,
        });
        assert!(!data.service(&egui::Context::default(), true));
        assert!(!data.pending);
        assert_eq!(data.revision(), 0);
        assert!(data.next_refresh > Instant::now());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn discovered_drives_keep_the_last_capacity_until_the_new_query_finishes() {
        let previous = vec![Volume {
            path: "C:/".into(),
            label: "Work (C:)".into(),
            usage: Usage::from_bytes(100, 30),
        }];
        let existing = discovered_volume("C:/".into(), "Local disk (C:)".into(), &previous);
        assert_eq!(existing, previous[0]);
        let new = discovered_volume("D:/".into(), "Local disk (D:)".into(), &previous);
        assert_eq!(new.label, "Local disk (D:)");
        assert_eq!(new.usage, None);
        assert_eq!(previous[0].usage.unwrap().free, 30);
    }

    #[test]
    fn repeated_refreshes_have_one_pending_request_and_no_worker_per_interaction() {
        let (mut data, receiver) = mailbox_fixture();
        for _ in 0..100 {
            data.request_refresh();
        }
        assert!(data.pending);
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
        let ctx = egui::Context::default();
        for _ in 0..100 {
            data.service(&ctx, true);
        }
        assert!(receiver.try_recv().is_err());
    }

    fn metadata_frame(
        data: &mut Data,
        ctx: &egui::Context,
        active: bool,
        frame: usize,
    ) -> Duration {
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(frame as f64 / 60.0),
                ..Default::default()
            },
            |ui| {
                data.service(ui.ctx(), active);
            },
        );
        let delay = output.viewport_output[&egui::ViewportId::ROOT].repaint_delay;
        // No renderer in these scheduling-only checks.
        output.textures_delta.clear();
        delay
    }

    #[test]
    fn visible_idle_metadata_schedules_only_a_distant_wake_then_one_due_request() {
        let (mut data, receiver) = mailbox_fixture();
        let ctx = egui::Context::default();
        data.next_refresh = Instant::now() + REFRESH_INTERVAL;
        for frame in 0..8 {
            let delay = metadata_frame(&mut data, &ctx, true, frame);
            if frame >= 3 {
                assert!(delay >= Duration::from_secs(55) && delay <= REFRESH_INTERVAL);
                assert!(!ctx.requested_repaint_last_pass());
            }
        }
        assert!(!data.pending);
        assert!(receiver.try_recv().is_err());
        // A short remaining deadline must stay delayed after egui prediction,
        // instead of being rounded into a busy immediate-repaint loop.
        data.next_refresh = Instant::now() + Duration::from_millis(250);
        let delay = metadata_frame(&mut data, &ctx, true, 8);
        assert!(delay > Duration::from_millis(100) && delay <= Duration::from_millis(250));
        data.next_refresh = Instant::now();
        metadata_frame(&mut data, &ctx, true, 9);
        assert!(data.pending);
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn hidden_metadata_has_no_refresh_work_or_timed_repaint_and_still_drains_results() {
        let (mut data, receiver) = mailbox_fixture();
        let ctx = egui::Context::default();
        for frame in 0..8 {
            let delay = metadata_frame(&mut data, &ctx, false, frame);
            if frame >= 3 {
                assert_eq!(delay, Duration::MAX);
            }
        }
        assert!(!data.pending);
        assert!(receiver.try_recv().is_err());
        *data.result.lock().unwrap() = Some(Publication {
            snapshot: Snapshot {
                places: vec![Place {
                    kind: PlaceKind::User,
                    path: std::env::temp_dir(),
                }],
                volumes: vec![],
            },
            complete: true,
        });
        assert_eq!(metadata_frame(&mut data, &ctx, false, 8), Duration::MAX);
        assert_eq!(data.revision, 1);
        assert!(data.next_refresh > Instant::now());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn partial_drive_publications_keep_refresh_pending_and_latest_result_wins() {
        let (mut data, receiver) = mailbox_fixture();
        data.pending = true;
        let root = std::env::temp_dir();
        for count in [1, 2, 3] {
            *data.result.lock().unwrap() = Some(Publication {
                snapshot: Snapshot {
                    places: vec![Place {
                        kind: PlaceKind::User,
                        path: root.clone(),
                    }],
                    volumes: (0..count)
                        .map(|i| Volume {
                            path: root.join(i.to_string()),
                            label: i.to_string(),
                            usage: None,
                        })
                        .collect(),
                },
                complete: false,
            });
        }
        let ctx = egui::Context::default();
        assert!(data.service(&ctx, true));
        assert_eq!(data.volumes().len(), 3);
        assert!(data.pending);
        assert!(receiver.try_recv().is_err());
        *data.result.lock().unwrap() = Some(Publication {
            snapshot: Snapshot::default(),
            complete: true,
        });
        assert!(data.service(&ctx, true));
        assert!(!data.pending);
        assert!(data.next_refresh > Instant::now());
        assert!(!data.service(&ctx, true));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn benchmark_usage_override_survives_publication_and_restores_latest_native_cache() {
        let (mut data, receiver) = mailbox_fixture();
        let root = std::env::temp_dir();
        data.snapshot.volumes = (0..2)
            .map(|index| Volume {
                path: root.join(index.to_string()),
                label: index.to_string(),
                usage: Usage::from_bytes(100, 20),
            })
            .collect();
        let next_refresh = data.next_refresh;
        assert_eq!(
            data.benchmark_override_usage(&[Usage::from_bytes(100, 5), None]),
            2
        );
        assert!(data.volumes()[0].usage.unwrap().near_full());
        assert!(data.volumes()[1].usage.is_none());
        assert_eq!(data.next_refresh, next_refresh);
        assert!(receiver.try_recv().is_err());
        let mut native = data.snapshot.clone();
        native.volumes[0].usage = Usage::from_bytes(100, 30);
        *data.result.lock().unwrap() = Some(Publication {
            snapshot: native,
            complete: true,
        });
        assert!(data.service(&egui::Context::default(), true));
        assert!(data.volumes()[0].usage.unwrap().near_full());
        assert!(data.benchmark_restore_usage());
        assert_eq!(data.volumes()[0].usage.unwrap().free, 30);
        assert_eq!(data.volumes()[1].usage.unwrap().free, 20);
        assert!(!data.benchmark_restore_usage());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn old_sessions_get_native_defaults_and_explicit_order_survives() {
        let defaults: Preferences = serde_json::from_str("{}").unwrap();
        assert_eq!(defaults.order, Section::DEFAULT_ORDER);
        assert!(defaults.storage_usage && defaults.places.pictures);
        assert!(
            Section::DEFAULT_ORDER
                .into_iter()
                .all(|section| defaults.sections.visible(section))
        );
        assert!(
            PlaceKind::ALL
                .into_iter()
                .all(|kind| defaults.places.visible(kind))
        );
        assert_eq!(
            PlaceKind::ALL.map(PlaceKind::label),
            [
                "User",
                "Desktop",
                "Downloads",
                "Documents",
                "Pictures",
                "Music",
                "Videos",
                "Recycle Bin"
            ]
        );
        let mut explicit: Preferences = serde_json::from_str(r#"{"order":["places","bookmarks","places"],"places":{"desktop":false,"music":false,"videos":false},"storage_usage":false}"#).unwrap();
        explicit.normalize();
        assert_eq!(
            explicit.order,
            [
                Section::Places,
                Section::Bookmarks,
                Section::Storage,
                Section::Network,
                Section::Recents
            ]
        );
        assert!(!explicit.places.desktop && explicit.places.user);
        assert!(!explicit.places.music && !explicit.places.videos);
        assert!(explicit.move_by(Section::Recents, -1));
        let restored: Preferences =
            serde_json::from_str(&serde_json::to_string(&explicit).unwrap()).unwrap();
        assert_eq!(restored, explicit);
        assert!(!explicit.move_by(Section::Places, -1));
    }
    #[test]
    fn section_visibility_migrates_partial_sessions_and_preserves_other_preferences() {
        let mut preferences: Preferences = serde_json::from_str(
            r#"{"order":["places","recents","bookmarks","storage"],"sections":{"network":false,"recents":false},"places":{"music":false},"storage_usage":false,"network_expanded":true}"#,
        )
        .unwrap();
        preferences.normalize();
        assert!(!preferences.sections.visible(Section::Network));
        assert!(!preferences.sections.visible(Section::Recents));
        for section in [Section::Bookmarks, Section::Storage, Section::Places] {
            assert!(preferences.sections.visible(section));
        }
        let original = preferences.clone();
        for section in Section::DEFAULT_ORDER {
            preferences.sections.toggle(section);
        }
        assert_eq!(preferences.order, original.order);
        assert_eq!(preferences.places, original.places);
        assert_eq!(preferences.storage_usage, original.storage_usage);
        assert_eq!(preferences.network_expanded, original.network_expanded);
        let restored: Preferences =
            serde_json::from_str(&serde_json::to_string(&preferences).unwrap()).unwrap();
        assert_eq!(restored, preferences);
        for section in Section::DEFAULT_ORDER {
            preferences.sections.toggle(section);
        }
        assert_eq!(preferences, original);

        let mut hidden = Preferences::default();
        for section in Section::DEFAULT_ORDER {
            hidden.sections.toggle(section);
        }
        hidden.normalize();
        let restored: Preferences =
            serde_json::from_str(&serde_json::to_string(&hidden).unwrap()).unwrap();
        assert!(
            Section::DEFAULT_ORDER
                .into_iter()
                .all(|section| !restored.sections.visible(section))
        );
        assert_eq!(restored.order, Section::DEFAULT_ORDER);
    }
    #[test]
    fn network_migration_keeps_recents_last_by_default_and_retains_custom_order() {
        assert_eq!(Section::Recents.index(), 0);
        assert_eq!(Section::Bookmarks.index(), 1);
        assert_eq!(Section::Storage.index(), 2);
        assert_eq!(Section::Places.index(), 3);
        assert_eq!(Section::Network.index(), 4);
        assert_eq!(Section::DEFAULT_ORDER.last(), Some(&Section::Recents));
        let mut legacy: Preferences =
            serde_json::from_str(r#"{"order":["bookmarks","storage","places","recents"]}"#)
                .unwrap();
        legacy.normalize();
        assert_eq!(legacy.order, Section::DEFAULT_ORDER);
        let mut custom: Preferences =
            serde_json::from_str(r#"{"order":["places","recents","bookmarks","storage"]}"#)
                .unwrap();
        custom.normalize();
        assert_eq!(
            custom.order,
            [
                Section::Places,
                Section::Recents,
                Section::Bookmarks,
                Section::Storage,
                Section::Network
            ]
        );
        let order = vec![
            Section::Network,
            Section::Places,
            Section::Recents,
            Section::Bookmarks,
            Section::Storage,
        ];
        custom.order = order.clone();
        custom.normalize();
        assert_eq!(custom.order, order);
    }
    #[test]
    fn known_folder_snapshot_preserves_redirected_paths_without_inventing_children() {
        let root = std::env::temp_dir();
        let redirected = root.join("OneDrive").join("Redirected Documents");
        let places = resolve_places(|kind| match kind {
            PlaceKind::User => Some(root.clone()),
            PlaceKind::Documents => Some(redirected.clone()),
            PlaceKind::Desktop => Some(PathBuf::from("relative-rejected")),
            _ => None,
        });
        assert_eq!(places.len(), 2);
        assert_eq!(places[1].kind, PlaceKind::Documents);
        assert_eq!(places[1].path, redirected);
    }
    #[test]
    fn volume_usage_handles_large_disks_empty_media_and_bad_free_counts() {
        assert_eq!(Usage::from_bytes(0, 0), None);
        let large = Usage::from_bytes(16 * 1024u64.pow(4), 1024u64.pow(4)).unwrap();
        assert!(large.near_full());
        assert_eq!(large.fraction(), 0.9375);
        assert_eq!(Usage::from_bytes(100, 150).unwrap().fraction(), 0.0);
        assert!(!Usage::from_bytes(100, 11).unwrap().near_full());
        assert!(Usage::from_bytes(100, 10).unwrap().near_full());
        assert!(
            !Usage::from_bytes(u64::MAX, u64::MAX / 10 + 1)
                .unwrap()
                .near_full()
        );
    }

    #[test]
    fn every_place_can_be_hidden_and_restored_without_changing_its_identity() {
        let mut visibility = PlacesVisibility::default();
        for kind in PlaceKind::ALL {
            let original = visibility.visible(kind);
            visibility.toggle(kind);
            assert_ne!(visibility.visible(kind), original);
            visibility.toggle(kind);
            assert_eq!(visibility.visible(kind), original);
        }
        let keys: Vec<_> = PlaceKind::ALL
            .into_iter()
            .map(|kind| kind.icon_key(20))
            .collect();
        for (index, key) in keys.iter().enumerate() {
            assert!(!keys[..index].contains(key));
        }
    }
}
