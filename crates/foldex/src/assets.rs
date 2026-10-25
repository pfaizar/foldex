//! Visible-only shell assets. Native extraction lives in `foldex-assets`; this module
//! performs at most two texture uploads per frame and retains at most 8 MiB of
//! logical RGBA texture payload (renderer heaps/font atlases are separate).
//!
//! Call `retain_views` before adding visible pane/sidebar/launcher lanes, then `set_requests`
//! with a lazy builder. A matching signature does not run that builder or clone
//! paths. `image` addresses its cached request by display row, so painting does
//! not construct paths. Local thumbnail extraction is explicitly enabled.
//!
//! Model revisions must increase monotonically, e.g. `Snapshot::id`. File stamps
//! detect size/time/kind changes without refreshing every icon after a folder
//! notification. Timestamp precision is that of the supplied model; call
//! `invalidate_path` after a known write, including same-size/same-time writes.
use egui::{ColorImage, Context, TextureHandle, TextureId, TextureOptions, Vec2, vec2};
use foldex_assets::{
    AssetKey, AssetLoader, AssetMetrics, AssetOptions, AssetRequest, MAX_IMAGE_EDGE,
    MAX_REQUESTS_PER_VIEW, MAX_VIEWS, PixelImage,
};
use foldex_ui::motion::{LEGACY_REFERENCE_DURATION, MotionGains, MotionProfile};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

pub const GPU_PIXEL_BUDGET: usize = 8 * 1024 * 1024;
pub const MAX_GPU_ENTRIES: usize = 256;
pub const MAX_UPLOADS_PER_FRAME: usize = 2;
const MAX_EVENTS_PER_FRAME: usize = 16;
const MAX_HASH_BYTES_PER_FRAME: usize = MAX_UPLOADS_PER_FRAME * MAX_IMAGE_EDGE * MAX_IMAGE_EDGE * 4;
const PATH_BUDGET: usize = 512 * 1024;
const MAX_PATH_BYTES: usize = 64 * 1024;
const MAX_OBSERVATIONS: usize = MAX_REQUESTS_PER_VIEW * MAX_VIEWS;
const RETRY_DELAY: Duration = Duration::from_millis(16);

/// The complete viewport signature. `epoch` identifies the query/navigation
/// output; `revision` identifies its immutable model. `[first, end)` is bounded
/// to 128 requests. Physical-pixel size and thumbnail policy participate too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportSignature {
    pub epoch: u64,
    pub revision: u64,
    pub first: usize,
    pub end: usize,
    pub size: u16,
    pub thumbnails: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AssetStamp {
    pub bytes: u64,
    pub modified_secs: i64,
    pub is_dir: bool,
}
impl AssetStamp {
    /// Identity-only icon request, with no filesystem metadata on the UI thread.
    /// Does not invalidate a newer listing's observed bytes/mtime.
    pub const UNOBSERVED: Self = Self {
        bytes: 0,
        modified_secs: i64::MIN,
        is_dir: false,
    };
}

#[derive(Clone, Debug)]
pub struct VisibleAsset {
    pub key: AssetKey,
    pub stamp: AssetStamp,
}

#[derive(Clone, Copy, Debug)]
pub struct AssetImage {
    pub texture: TextureId,
    /// Physical source dimensions. Fit this aspect ratio into the logical icon rect.
    pub size: Vec2,
    pub loaded_at: Instant,
    /// Source rate-50 crossfade, with a .01 normalized snap. No hidden fade timer.
    pub opacity: f32,
    pub previous: Option<AssetLayer>,
}

#[derive(Clone, Copy, Debug)]
pub struct AssetLayer {
    pub texture: TextureId,
    pub size: Vec2,
    pub opacity: f32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ManagerMetrics {
    pub enabled: bool,
    pub gpu_pixel_bytes: usize,
    pub gpu_entries: usize,
    pub uploads_this_frame: usize,
    pub pending_controls: usize,
    pub errors: u64,
    pub budget_fallbacks: u64,
    pub backend: AssetMetrics,
}

struct View {
    id: u64,
    signature: ViewportSignature,
    worker_epoch: u64,
    requests: Vec<AssetRequest>,
    stamps: Vec<AssetStamp>,
    cut_keys: Vec<Option<crate::cut_state::PathKey>>,
    // Per-view representation changes retain IDs only, within the global cache budget.
    transitions: Vec<AssetTransition>,
    path_bytes: usize,
    pending: bool,
    registered: bool,
}

#[derive(Clone, Copy)]
struct AssetTransition {
    previous: Option<TextureId>,
    blend: f32,
    frame: u64,
}

struct GpuAsset {
    texture: TextureHandle,
    size: Vec2,
    bytes: usize,
    path_bytes: usize,
    touched: u64,
    pinned: bool,
    stamp: AssetStamp,
    revision: u64,
    // A Weak retains no pixels or CPU pixel-budget lease. It identifies a
    // backend cache hit without hashing/copying pixel data or re-uploading it.
    source: Option<Weak<PixelImage>>,
    loaded_at: Instant,
    opacity: f32,
    fade_frame: u64,
    pixel_hash: u64,
}

struct Observation {
    path: PathBuf,
    stamp: AssetStamp,
    revision: u64,
    touched: u64,
    bytes: usize,
    dirty: bool,
}

pub struct AssetManager {
    loader: Option<AssetLoader>,
    cut: crate::cut_state::Service,
    ctx: Context,
    views: Vec<View>,
    cancels: Vec<u64>,
    invalidations: Vec<PathBuf>,
    invalidation_bytes: usize,
    observations: Vec<Observation>,
    observation_bytes: usize,
    cache: HashMap<AssetKey, GpuAsset>,
    pins_dirty: bool,
    pixel_bytes: usize,
    cache_path_bytes: usize,
    serial: u64,
    tick: u64,
    frame: u64,
    now: Instant,
    gains: MotionGains,
    motion_duration: f32,
    uploads: usize,
    hash_bytes: usize,
    errors: u64,
    budget_fallbacks: u64,
    last_error: Option<String>,
}

impl AssetManager {
    pub fn new(ctx: &Context) -> io::Result<Self> {
        let repaint = ctx.clone();
        let loader = AssetLoader::with_options(
            AssetOptions {
                extract_local_thumbnails: true,
            },
            Arc::new(move || repaint.request_repaint()),
        )?;
        let mut manager = Self::disabled(ctx);
        manager.loader = Some(loader);
        Ok(manager)
    }

    /// Vector-icon fallback when worker creation fails. No native worker is started.
    pub fn disabled(ctx: &Context) -> Self {
        Self {
            loader: None,
            cut: crate::cut_state::Service::disabled(),
            ctx: ctx.clone(),
            views: Vec::with_capacity(MAX_VIEWS),
            cancels: Vec::with_capacity(MAX_VIEWS),
            invalidations: Vec::new(),
            invalidation_bytes: 0,
            observations: Vec::new(),
            observation_bytes: 0,
            cache: HashMap::new(),
            pins_dirty: false,
            pixel_bytes: 0,
            cache_path_bytes: 0,
            serial: 0,
            tick: 0,
            frame: 0,
            now: Instant::now(),
            gains: MotionGains::new(0.0, LEGACY_REFERENCE_DURATION),
            motion_duration: LEGACY_REFERENCE_DURATION,
            uploads: 0,
            hash_bytes: 0,
            errors: 0,
            budget_fallbacks: 0,
            last_error: None,
        }
    }

    /// Set before `begin_frame`; zero follows the app's reduced-motion preference.
    pub fn set_motion_duration(&mut self, seconds: f32) {
        self.motion_duration = seconds;
    }

    /// Start the current-cut observer independently of launcher history. The app
    /// calls this only outside benchmark mode; ordinary unit fixtures stay inert.
    pub fn enable_cut_tracking(&mut self) {
        if cfg!(test) {
            return;
        }
        let repaint = self.ctx.clone();
        match crate::cut_state::Service::new(Arc::new(move || repaint.request_repaint())) {
            Ok(service) => self.cut = service,
            Err(error) => self.record_error(&format!("Cut highlighting: {error}")),
        }
    }

    /// Isolated native/GPU benchmark seed. Installs an immutable fixture and
    /// stops any observer; performs no clipboard reads or writes.
    pub(crate) fn benchmark_set_cut_paths(&mut self, paths: &[PathBuf]) -> io::Result<()> {
        self.cut = crate::cut_state::Service::fixture(paths)?;
        self.ctx.request_repaint();
        Ok(())
    }

    /// Exact prepared path lookup, used by sidebar rows whose icon uses a stock
    /// Shell key. Preparation belongs to source changes, never to painting.
    pub fn is_cut_key(&self, key: Option<&crate::cut_state::PathKey>) -> bool {
        self.cut.contains(key)
    }

    pub fn cancel_cut(&mut self) -> bool {
        self.cut.cancel()
    }

    pub fn cut_version(&self) -> crate::cut_state::Version {
        self.cut.version()
    }

    /// A wide grid can expose more than the 128 native-asset requests. Its
    /// remaining rows still need cut presentation. The listing memoizes this
    /// result per entry/version; the lazy path runs only after a model,
    /// viewport, or clipboard change, and never for an empty cut selection.
    pub fn is_cut_row(
        &self,
        view: u64,
        epoch: u64,
        row: usize,
        path: impl FnOnce() -> PathBuf,
    ) -> bool {
        if !self.cut.has_paths() {
            return false;
        }
        let key = self
            .views
            .iter()
            .find(|entry| entry.id == view && entry.signature.epoch == epoch)
            .and_then(|entry| {
                row.checked_sub(entry.signature.first)
                    .and_then(|index| entry.cut_keys.get(index))
            });
        if let Some(key) = key {
            return self.cut.contains(key.as_ref());
        }
        self.cut
            .contains(crate::cut_state::PathKey::new(&path()).as_ref())
    }

    /// Run once per UI frame. This performs no filesystem, COM, GDI or blocking
    /// mailbox operations. Pixel Arcs are never retained after this call.
    pub fn begin_frame(&mut self, ctx: &Context) {
        self.ctx = ctx.clone();
        self.cut.begin_frame();
        self.frame = self.frame.wrapping_add(1);
        self.now = Instant::now();
        self.uploads = 0;
        self.hash_bytes = 0;
        self.gains = MotionGains::new(ctx.input(|input| input.stable_dt), self.motion_duration);
        self.flush_controls();
        let mut examined = 0;
        while examined < MAX_EVENTS_PER_FRAME
            && self.uploads < MAX_UPLOADS_PER_FRAME
            && self.hash_bytes < MAX_HASH_BYTES_PER_FRAME
        {
            let Some(event) = self.loader.as_ref().and_then(AssetLoader::try_recv) else {
                break;
            };
            examined += 1;
            let Some(view) = self.views.iter().find(|view| {
                view.id == event.view && view.worker_epoch == event.epoch && !view.pending
            }) else {
                continue;
            };
            if self
                .invalidations
                .iter()
                .any(|path| path == &event.key.path)
            {
                continue;
            }
            let Some(index) = view
                .requests
                .iter()
                .position(|request| request.key == event.key)
            else {
                continue;
            };
            let stamp = view.stamps[index];
            let revision = view.signature.revision;
            match event.pixels {
                Ok(pixels) => {
                    let source = Arc::downgrade(&pixels);
                    self.upload(
                        event.key,
                        stamp,
                        revision,
                        pixels.size,
                        &pixels.rgba,
                        Some(source),
                    );
                    // The Arc and its pixel-budget lease end here; only a Weak is cached.
                }
                Err(error) => self.record_error(&error),
            }
        }
        // More published events might already be queued under one coalesced wake.
        // A bounded drain needs a follow-up frame, but never a permanent timer.
        if examined == MAX_EVENTS_PER_FRAME
            || self.uploads == MAX_UPLOADS_PER_FRAME
            || self.hash_bytes >= MAX_HASH_BYTES_PER_FRAME
        {
            ctx.request_repaint();
        }
    }

    /// Cancel hidden/closed panes before submitting requests for newly shown panes.
    /// A contended cancellation is retained and retried before any new submissions.
    pub fn retain_views(&mut self, visible: &[u64]) {
        let mut index = 0;
        let mut changed = false;
        while index < self.views.len() {
            if visible
                .iter()
                .take(MAX_VIEWS)
                .any(|id| *id == self.views[index].id)
            {
                index += 1;
            } else {
                let view = self.views.swap_remove(index);
                changed = true;
                if view.registered && !self.cancels.contains(&view.id) {
                    self.cancels.push(view.id);
                }
            }
        }
        if changed {
            self.refresh_pins();
            self.flush_controls();
        }
    }

    /// Returns true only when the request set changed. The builder is called only
    /// then, and must return one request for each row in `[first, end)`.
    /// WouldBlock is internally retried without rerunning the builder.
    pub fn set_requests(
        &mut self,
        view: u64,
        signature: ViewportSignature,
        build: impl FnOnce() -> Vec<VisibleAsset>,
    ) -> io::Result<bool> {
        validate_signature(signature)?;
        let existing = self.views.iter().position(|entry| entry.id == view);
        if existing.is_some_and(|index| self.views[index].signature == signature) {
            return Ok(false);
        }
        if existing.is_none() && self.views.len() == MAX_VIEWS {
            return Err(invalid("visible asset view limit reached"));
        }
        let assets = build();
        if assets.len() != signature.end - signature.first {
            return Err(invalid(
                "asset requests must correspond to every row in the viewport range",
            ));
        }
        let mut path_bytes = 0usize;
        for asset in &assets {
            validate_key(&asset.key)?;
            path_bytes = path_bytes
                .saturating_add(path_weight(&asset.key.path))
                .saturating_add(std::mem::size_of::<AssetKey>());
        }
        let other_bytes: usize = self
            .views
            .iter()
            .filter(|entry| entry.id != view)
            .map(|entry| entry.path_bytes)
            .sum();
        if path_bytes.saturating_add(other_bytes) > PATH_BUDGET {
            return Err(invalid("visible asset paths exceed 512 KiB"));
        }
        // Observations are bounded independently of the GPU cache. This also
        // invalidates a CPU-only cached thumbnail when its model metadata changes.
        let mut invalidated = false;
        for asset in &assets {
            if asset.stamp == AssetStamp::UNOBSERVED {
                continue;
            }
            let mut changed = self.observe(&asset.key.path, asset.stamp, signature.revision);
            for (key, cached) in &mut self.cache {
                if key.path != asset.key.path {
                    continue;
                }
                if cached.stamp == asset.stamp {
                    // A newer model confirmed this resident image's stamp.
                    // Keep that fact even when no new native event is needed.
                    cached.revision = cached.revision.max(signature.revision);
                } else if cached.stamp == AssetStamp::UNOBSERVED
                    || cached.revision <= signature.revision
                {
                    changed = true;
                }
            }
            if changed {
                invalidated = true;
                self.invalidate_path(&asset.key.path)?;
            }
        }
        if !invalidated {
            if let Some(index) = existing {
                let old = &mut self.views[index];
                if old.requests.len() == assets.len()
                    && assets
                        .iter()
                        .all(|asset| self.cache.contains_key(&asset.key))
                    && old.requests.iter().zip(&old.stamps).zip(&assets).all(
                        |((request, stamp), asset)| {
                            request.key == asset.key && *stamp == asset.stamp
                        },
                    )
                {
                    // Navigation/model generations may change while all resident
                    // assets remain identical. Preserve their worker generation
                    // and fades rather than replaying the mailbox. Missing or
                    // failed assets still get the usual new-generation retry.
                    old.signature = signature;
                    return Ok(true);
                }
            }
        }
        let mut requests = Vec::with_capacity(assets.len());
        let mut stamps = Vec::with_capacity(assets.len());
        let mut cut_keys = Vec::with_capacity(assets.len());
        let mut transitions = Vec::with_capacity(assets.len());
        for asset in assets {
            let prior = existing.and_then(|index| {
                let view = &self.views[index];
                view.requests
                    .iter()
                    .enumerate()
                    .find(|(row, request)| {
                        request.key.path == asset.key.path && view.stamps[*row] == asset.stamp
                    })
                    .map(|(row, request)| (&request.key, view.transitions[row]))
            });
            let transition = if let Some((_, transition)) =
                prior.filter(|(key, _)| *key == &asset.key)
            {
                transition
            } else {
                // On a rapid reversal, retain the dominant displayed layer.
                let displayed = prior.and_then(|(key, transition)| {
                    let current = self.cache.get(key);
                    let previous = transition.previous.filter(|texture| {
                        self.cache.iter().any(|(key, cached)| {
                            cached.texture.id() == *texture
                                && key.path == asset.key.path
                                && compatible_stamp(asset.stamp, cached.stamp)
                        })
                    });
                    if current.is_some_and(|cached| cached.opacity * transition.blend >= 0.5) {
                        current.map(|cached| cached.texture.id())
                    } else {
                        previous.or_else(|| current.map(|cached| cached.texture.id()))
                    }
                });
                let current = self.cache.get(&asset.key);
                let fallback = if current.is_some_and(|cached| cached.opacity >= 1.0) {
                    None
                } else {
                    displayed.or_else(|| {
                        // A warm exact image is already the desired presentation.
                        // Re-entering a folder must not fade it from another size.
                        if current.is_some() {
                            return None;
                        }
                        self.cache
                            .iter()
                            .filter_map(|(key, cached)| {
                                (key != &asset.key
                                    && key.path == asset.key.path
                                    && compatible_stamp(asset.stamp, cached.stamp))
                                .then(|| {
                                    (
                                        u32::from(key.thumbnail != asset.key.thumbnail) * 1024
                                            + key.size.abs_diff(asset.key.size) as u32,
                                        cached.texture.id(),
                                    )
                                })
                            })
                            .min_by_key(|(score, _)| *score)
                            .map(|(_, texture)| texture)
                    })
                };
                let previous = fallback
                    .filter(|texture| current.is_none_or(|cached| cached.texture.id() != *texture));
                AssetTransition {
                    previous,
                    // Only cold pixels need an upload fade. Already displayed
                    // cached variants must not start another view-local fade.
                    blend: 1.0,
                    frame: self.frame.wrapping_sub(1),
                }
            };
            transitions.push(transition);
            cut_keys.push(crate::cut_state::PathKey::new(&asset.key.path));
            requests.push(AssetRequest { key: asset.key });
            stamps.push(asset.stamp);
        }
        self.serial = self.serial.wrapping_add(1).max(1);
        let registered = existing.is_some_and(|index| self.views[index].registered);
        let state = View {
            id: view,
            signature,
            worker_epoch: self.serial,
            requests,
            stamps,
            cut_keys,
            transitions,
            path_bytes,
            pending: self.loader.is_some(),
            registered,
        };
        if let Some(index) = existing {
            self.views[index] = state;
        } else {
            self.views.push(state);
        }
        self.refresh_pins();
        self.flush_controls();
        Ok(true)
    }

    /// Cut membership uses the submitted immutable viewport; old epochs and rows
    /// outside that viewport cannot reuse another row's path. No path is cloned.
    pub fn is_cut(&self, view: u64, epoch: u64, row: usize) -> bool {
        let key = self
            .views
            .iter()
            .find(|entry| entry.id == view && entry.signature.epoch == epoch)
            .and_then(|entry| {
                row.checked_sub(entry.signature.first)
                    .and_then(|index| entry.cut_keys.get(index))
            })
            .and_then(Option::as_ref);
        self.cut.contains(key)
    }

    /// Borrow the already-built path/key for a display row. Old query epochs and
    /// rows outside the submitted viewport return the app's vector-icon fallback.
    pub fn image(&mut self, view: u64, epoch: u64, row: usize) -> Option<AssetImage> {
        let view = self
            .views
            .iter_mut()
            .find(|entry| entry.id == view && entry.signature.epoch == epoch)?;
        let index = row.checked_sub(view.signature.first)?;
        let key = &view.requests.get(index)?.key;
        let transition = &mut view.transitions[index];
        let previous_key = transition.previous.and_then(|texture| {
            self.cache.iter().find_map(|(old_key, cached)| {
                (cached.texture.id() == texture
                    && old_key.path == key.path
                    && compatible_stamp(view.stamps[index], cached.stamp))
                .then_some(old_key)
            })
        });
        // A different resolution of the same representation replaces the
        // retained pixels atomically. Replaying the cold upload fade would dim
        // a fully visible icon on every zoom/DPI bucket boundary.
        let resolution_change = previous_key.is_some_and(|old| old.thumbnail == key.thumbnail);
        let identical_pixels = previous_key
            .and_then(|old| self.cache.get(old))
            .zip(self.cache.get(key))
            .is_some_and(|(old, current)| {
                old.pixel_hash == current.pixel_hash
                    && old.bytes == current.bytes
                    && old.size == current.size
            });
        let mut current = self.cache.get_mut(key).map(|cached| {
            if resolution_change || identical_pixels {
                cached.opacity = 1.0;
                transition.blend = 1.0;
            }
            present(cached, self.frame, self.gains, &self.ctx, &mut self.tick)
        });
        if transition.blend >= 1.0 && current.as_ref().is_some_and(|image| image.opacity >= 1.0) {
            self.pins_dirty |= transition.previous.take().is_some();
            return current;
        }
        let previous = if let Some(texture) = transition.previous {
            self.cache
                .iter_mut()
                .find(|(old_key, cached)| {
                    cached.texture.id() == texture
                        && old_key.path == key.path
                        && compatible_stamp(view.stamps[index], cached.stamp)
                })
                .map(|(_, cached)| {
                    present(cached, self.frame, self.gains, &self.ctx, &mut self.tick)
                })
        } else {
            None
        };
        if let Some(image) = &mut current {
            if previous.is_none() {
                transition.blend = 1.0;
            } else if transition.frame != self.frame {
                transition.frame = self.frame;
                if self
                    .gains
                    .step_normalized(&mut transition.blend, 1.0, MotionProfile::Standard)
                {
                    self.ctx.request_repaint();
                }
            }
            image.opacity *= transition.blend;
            if image.opacity >= 1.0 {
                self.pins_dirty |= transition.previous.take().is_some();
            } else {
                image.previous = previous.map(|old| AssetLayer {
                    texture: old.texture,
                    size: old.size,
                    opacity: old.opacity,
                });
            }
            current
        } else {
            previous
        }
    }

    /// Read-only native QA/diagnostics; does not advance fades or request frames.
    pub fn view_ready(&self, view: u64) -> (usize, usize) {
        self.views
            .iter()
            .find(|entry| entry.id == view)
            .map_or((0, 0), |entry| {
                (
                    entry.requests.len(),
                    entry
                        .requests
                        .iter()
                        .filter(|request| self.cache.contains_key(&request.key))
                        .count(),
                )
            })
    }

    /// Read-only continuity probe: (requested rows, rows with native pixels,
    /// rows retaining a compatible older resolution while the exact key loads).
    /// Does not advance animation, touch the LRU, or request another frame.
    pub fn view_presented(&self, view: u64) -> (usize, usize, usize) {
        let Some(view) = self.views.iter().find(|entry| entry.id == view) else {
            return (0, 0, 0);
        };
        let mut presented = 0;
        let mut retained = 0;
        for ((request, stamp), transition) in view
            .requests
            .iter()
            .zip(&view.stamps)
            .zip(&view.transitions)
        {
            if self.cache.contains_key(&request.key) {
                presented += 1;
            } else if transition.previous.is_some_and(|texture| {
                self.cache.iter().any(|(key, cached)| {
                    cached.texture.id() == texture
                        && key.path == request.key.path
                        && compatible_stamp(*stamp, cached.stamp)
                })
            }) {
                presented += 1;
                retained += 1;
            }
        }
        (view.requests.len(), presented, retained)
    }

    /// Evict all GPU variants immediately; defer a contended native cache
    /// invalidation. The bounded queue returns WouldBlock rather than growing.
    pub fn invalidate_path(&mut self, path: &Path) -> io::Result<()> {
        validate_path(path)?;
        if let Some(observed) = self.observations.iter_mut().find(|old| old.path == path) {
            observed.dirty = true;
        }
        self.remove_path(path);
        if self.loader.is_none() || self.invalidations.iter().any(|queued| queued == path) {
            if let Some(observed) = self.observations.iter_mut().find(|old| old.path == path) {
                observed.dirty = false;
            }
            return Ok(());
        }
        let bytes = path_weight(path);
        if self.invalidations.len() == MAX_OBSERVATIONS
            || self.invalidation_bytes.saturating_add(bytes) > PATH_BUDGET
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "asset invalidation queue full; retry",
            ));
        }
        self.invalidations.push(path.to_path_buf());
        self.invalidation_bytes += bytes;
        if let Some(observed) = self.observations.iter_mut().find(|old| old.path == path) {
            observed.dirty = false;
        }
        self.flush_controls();
        Ok(())
    }

    /// Explicit refresh of at most 256 current visible assets. This is for F5
    /// or a known write, not each native directory-reconciliation notification.
    /// Paths are collected once because invalidation may update the mailboxes.
    pub fn invalidate_visible(&mut self) -> io::Result<()> {
        let mut paths = Vec::new();
        for view in &self.views {
            for request in &view.requests {
                if !paths.contains(&request.key.path) {
                    paths.push(request.key.path.clone());
                }
            }
        }
        for path in paths {
            self.invalidate_path(&path)?;
        }
        Ok(())
    }

    pub fn metrics(&self) -> ManagerMetrics {
        ManagerMetrics {
            enabled: self.loader.is_some(),
            gpu_pixel_bytes: self.pixel_bytes,
            gpu_entries: self.cache.len(),
            uploads_this_frame: self.uploads,
            pending_controls: self.cancels.len()
                + self.invalidations.len()
                + self.views.iter().filter(|view| view.pending).count(),
            errors: self.errors,
            budget_fallbacks: self.budget_fallbacks,
            backend: self
                .loader
                .as_ref()
                .map_or(AssetMetrics::default(), AssetLoader::metrics),
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    fn record_error(&mut self, message: &str) {
        self.errors = self.errors.saturating_add(1);
        self.last_error = Some(message.chars().take(256).collect());
    }

    fn flush_controls(&mut self) {
        let Some(loader) = self.loader.as_ref() else {
            return;
        };
        let mut failed = None;
        while let Some(view) = self.cancels.last().copied() {
            match loader.cancel_view(view) {
                Ok(()) => {
                    self.cancels.pop();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.cancels.pop();
                    failed = Some(error.to_string());
                }
            }
        }
        while let Some(path) = self.invalidations.last() {
            match loader.invalidate_path(path) {
                Ok(()) => {
                    self.invalidation_bytes -= path_weight(path);
                    self.invalidations.pop();
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    self.invalidation_bytes -= path_weight(path);
                    self.invalidations.pop();
                    failed = Some(error.to_string());
                }
            }
        }
        if self.cancels.is_empty() && self.invalidations.is_empty() {
            for view in &mut self.views {
                if !view.pending {
                    continue;
                }
                match loader.set_requests_ref(view.id, view.worker_epoch, &view.requests) {
                    Ok(()) => {
                        view.pending = false;
                        view.registered = true;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        view.pending = false;
                        failed = Some(error.to_string());
                    }
                }
            }
        }
        if !self.cancels.is_empty()
            || !self.invalidations.is_empty()
            || self.views.iter().any(|view| view.pending)
        {
            self.ctx.request_repaint_after(RETRY_DELAY);
        }
        if let Some(error) = failed {
            self.record_error(&error);
        }
    }

    fn refresh_pins(&mut self) {
        for (key, cached) in &mut self.cache {
            cached.pinned = self.views.iter().any(|view| {
                view.requests
                    .iter()
                    .zip(&view.stamps)
                    .zip(&view.transitions)
                    .any(|((request, stamp), transition)| {
                        &request.key == key
                            || (transition.previous == Some(cached.texture.id())
                                && request.key.path == key.path
                                && compatible_stamp(*stamp, cached.stamp))
                    })
            });
        }
        self.pins_dirty = false;
    }

    fn observe(&mut self, path: &Path, stamp: AssetStamp, revision: u64) -> bool {
        self.tick = self.tick.wrapping_add(1);
        if let Some(old) = self.observations.iter_mut().find(|old| old.path == path) {
            old.touched = self.tick;
            if revision < old.revision {
                return old.dirty;
            }
            let changed = old.stamp != stamp;
            old.stamp = stamp;
            old.revision = revision;
            old.dirty |= changed;
            return old.dirty;
        }
        let bytes = path_weight(path);
        while self.observations.len() >= MAX_OBSERVATIONS
            || self.observation_bytes.saturating_add(bytes) > PATH_BUDGET
        {
            let Some(index) = self
                .observations
                .iter()
                .enumerate()
                .min_by_key(|(_, old)| old.touched)
                .map(|(index, _)| index)
            else {
                break;
            };
            self.observation_bytes -= self.observations.swap_remove(index).bytes;
        }
        self.observation_bytes += bytes;
        self.observations.push(Observation {
            path: path.to_path_buf(),
            stamp,
            revision,
            touched: self.tick,
            bytes,
            dirty: false,
        });
        false
    }

    fn remove_path(&mut self, path: &Path) {
        let mut removed_pixels = 0;
        let mut removed_paths = 0;
        self.cache.retain(|key, cached| {
            if key.path == path {
                removed_pixels += cached.bytes;
                removed_paths += cached.path_bytes;
                false
            } else {
                true
            }
        });
        self.pixel_bytes -= removed_pixels;
        self.cache_path_bytes -= removed_paths;
    }

    fn remove_key(&mut self, key: &AssetKey) {
        if let Some(cached) = self.cache.remove(key) {
            self.pixel_bytes -= cached.bytes;
            self.cache_path_bytes -= cached.path_bytes;
        }
    }

    /// A retained source can fund its own replacement only when every visible
    /// reference will receive that replacement. Another pane's current source
    /// or differently sized pending request must remain resident.
    fn replaceable_previous(
        &self,
        old_key: &AssetKey,
        cached: &GpuAsset,
        incoming: &AssetKey,
        stamp: AssetStamp,
    ) -> bool {
        if old_key.path != incoming.path || !compatible_stamp(stamp, cached.stamp) {
            return false;
        }
        let mut retained = false;
        for view in &self.views {
            for ((request, stamp), transition) in view
                .requests
                .iter()
                .zip(&view.stamps)
                .zip(&view.transitions)
            {
                if &request.key == old_key {
                    return false;
                }
                if transition.previous == Some(cached.texture.id()) {
                    if &request.key != incoming || !compatible_stamp(*stamp, cached.stamp) {
                        return false;
                    }
                    retained = true;
                }
            }
        }
        retained
    }

    fn upload(
        &mut self,
        key: AssetKey,
        stamp: AssetStamp,
        revision: u64,
        size: [usize; 2],
        rgba: &[u8],
        source: Option<Weak<PixelImage>>,
    ) -> bool {
        if self.uploads == MAX_UPLOADS_PER_FRAME {
            return false;
        }
        let Some(bytes) = size[0]
            .checked_mul(size[1])
            .and_then(|area| area.checked_mul(4))
        else {
            self.record_error("invalid shell asset dimensions");
            return false;
        };
        if size.contains(&0)
            || size.iter().any(|edge| *edge > MAX_IMAGE_EDGE)
            || bytes != rgba.len()
        {
            self.record_error("invalid shell asset pixel buffer");
            return false;
        }
        if let Some(cached) = self.cache.get_mut(&key) {
            // An identity-only launcher/sidebar request must never replace a
            // file listing's observed metadata with an incomparable revision.
            if stamp == AssetStamp::UNOBSERVED && cached.stamp != AssetStamp::UNOBSERVED {
                return false;
            }
            if cached.revision > revision && cached.stamp != stamp {
                return false;
            }
            if source
                .as_ref()
                .zip(cached.source.as_ref())
                .is_some_and(|(new, old)| Weak::ptr_eq(new, old))
            {
                if cached.stamp == stamp {
                    cached.revision = cached.revision.max(revision);
                }
                return false;
            }
        }
        // A freshly extracted native object may still contain identical pixels.
        // Keep its resident texture and fade progress rather than flashing again.
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.hash_bytes = self.hash_bytes.saturating_add(rgba.len());
        rgba.hash(&mut hash);
        let pixel_hash = hash.finish();
        if let Some(cached) = self.cache.get_mut(&key) {
            if cached.stamp == stamp
                && cached.bytes == bytes
                && cached.size == vec2(size[0] as f32, size[1] as f32)
                && cached.pixel_hash == pixel_hash
            {
                cached.source = source;
                cached.revision = cached.revision.max(revision);
                return false;
            }
        }
        let path_bytes = path_weight(&key.path);
        let old_bytes = self.cache.get(&key).map_or(0, |old| old.bytes);
        let old_paths = self.cache.get(&key).map_or(0, |old| old.path_bytes);
        if self.pins_dirty {
            self.refresh_pins();
        }
        let mut replaced_previous = false;
        while self.pixel_bytes - old_bytes + bytes > GPU_PIXEL_BUDGET
            || self.cache_path_bytes - old_paths + path_bytes > PATH_BUDGET
            || (!self.cache.contains_key(&key) && self.cache.len() >= MAX_GPU_ENTRIES)
        {
            let evict = self
                .cache
                .iter()
                .filter(|(old_key, old)| !old.pinned && *old_key != &key)
                .min_by_key(|(_, old)| old.touched)
                .map(|(key, _)| key.clone());
            if let Some(evict) = evict {
                self.remove_key(&evict);
                continue;
            }
            // Keep a visible old source until the complete replacement fits.
            // A full cache can then swap that source in the same UI frame,
            // without exceeding the budget or introducing a blank frame.
            let mut pixels = self.pixel_bytes - old_bytes + bytes;
            let mut paths = self.cache_path_bytes - old_paths + path_bytes;
            let mut entries = self.cache.len() + usize::from(!self.cache.contains_key(&key));
            let mut retire = Vec::new();
            for (old_key, cached) in &self.cache {
                if old_key != &key && self.replaceable_previous(old_key, cached, &key, stamp) {
                    pixels -= cached.bytes;
                    paths -= cached.path_bytes;
                    entries -= 1;
                    retire.push(old_key.clone());
                    if pixels <= GPU_PIXEL_BUDGET
                        && paths <= PATH_BUDGET
                        && entries <= MAX_GPU_ENTRIES
                    {
                        break;
                    }
                }
            }
            if pixels > GPU_PIXEL_BUDGET || paths > PATH_BUDGET || entries > MAX_GPU_ENTRIES {
                self.budget_fallbacks = self.budget_fallbacks.saturating_add(1);
                return false;
            }
            for retired in retire {
                self.remove_key(&retired);
            }
            replaced_previous = true;
        }
        self.remove_key(&key);
        self.tick = self.tick.wrapping_add(1);
        let image = ColorImage::from_rgba_unmultiplied(size, rgba);
        let texture = self.ctx.load_texture(
            format!("shell-asset-{}", self.tick),
            image,
            TextureOptions::LINEAR,
        );
        self.cache.insert(
            key,
            GpuAsset {
                texture,
                size: vec2(size[0] as f32, size[1] as f32),
                bytes,
                path_bytes,
                touched: self.tick,
                pinned: true,
                stamp,
                revision,
                source,
                loaded_at: self.now,
                opacity: if self.gains.is_immediate() || replaced_previous {
                    1.0
                } else {
                    0.0
                },
                fade_frame: self.frame.wrapping_sub(1),
                pixel_hash,
            },
        );
        self.pixel_bytes += bytes;
        self.cache_path_bytes += path_bytes;
        self.uploads += 1;
        true
    }
}

fn present(
    cached: &mut GpuAsset,
    frame: u64,
    gains: MotionGains,
    ctx: &Context,
    tick: &mut u64,
) -> AssetImage {
    *tick = tick.wrapping_add(1);
    cached.touched = *tick;
    if cached.fade_frame != frame || gains.is_immediate() {
        cached.fade_frame = frame;
        if gains.step_normalized(&mut cached.opacity, 1.0, MotionProfile::Standard) {
            ctx.request_repaint();
        }
    }
    AssetImage {
        texture: cached.texture.id(),
        size: cached.size,
        loaded_at: cached.loaded_at,
        opacity: cached.opacity,
        previous: None,
    }
}

fn compatible_stamp(requested: AssetStamp, cached: AssetStamp) -> bool {
    requested == AssetStamp::UNOBSERVED || requested == cached
}

fn validate_signature(signature: ViewportSignature) -> io::Result<()> {
    if signature.end < signature.first || signature.end - signature.first > MAX_REQUESTS_PER_VIEW {
        return Err(invalid("asset viewport must contain at most 128 rows"));
    }
    if signature.size == 0 || signature.size as usize > MAX_IMAGE_EDGE {
        return Err(invalid(
            "asset size must be between 1 and 256 physical pixels",
        ));
    }
    Ok(())
}

fn validate_key(key: &AssetKey) -> io::Result<()> {
    validate_path(&key.path)?;
    if key.size == 0 || key.size as usize > MAX_IMAGE_EDGE {
        return Err(invalid(
            "asset size must be between 1 and 256 physical pixels",
        ));
    }
    Ok(())
}

fn validate_path(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path_weight(path) > MAX_PATH_BYTES
        || (!path.is_absolute() && !foldex_assets::is_native_icon_path(path))
        || path.as_os_str().as_encoded_bytes().contains(&0)
    {
        return Err(invalid(
            "asset path must be absolute, nonempty, NUL-free and within 64 KiB",
        ));
    }
    Ok(())
}

fn path_weight(path: &Path) -> usize {
    path.as_os_str().len().saturating_mul(2)
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn signature(epoch: u64, revision: u64, first: usize, end: usize) -> ViewportSignature {
        ViewportSignature {
            epoch,
            revision,
            first,
            end,
            size: 48,
            thumbnails: true,
        }
    }
    fn asset(name: &str) -> VisibleAsset {
        VisibleAsset {
            key: AssetKey {
                path: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/assets-unit")
                    .join(name),
                size: 48,
                thumbnail: true,
            },
            stamp: AssetStamp {
                bytes: 10,
                modified_secs: 42,
                is_dir: false,
            },
        }
    }
    fn inject(manager: &mut AssetManager, name: &str, side: usize) -> bool {
        manager.upload(
            asset(name).key,
            asset(name).stamp,
            1,
            [side, side],
            &vec![255; side * side * 4],
            None,
        )
    }

    fn variant(name: &str, size: u16) -> VisibleAsset {
        let mut asset = asset(name);
        asset.key.size = size;
        asset
    }

    fn inject_variant(manager: &mut AssetManager, asset: &VisibleAsset) -> bool {
        let side = usize::from(asset.key.size);
        manager.upload(
            asset.key.clone(),
            asset.stamp,
            1,
            [side, side],
            &vec![255; side * side * 4],
            None,
        )
    }

    #[test]
    fn repeated_size_changes_keep_native_pixels_and_never_refade_a_known_representation() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let initial = asset("zoom.txt");
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![initial.clone()])
            .unwrap();
        assert!(inject_variant(&mut manager, &initial));
        manager.cache.get_mut(&initial.key).unwrap().opacity = 1.0;
        let mut displayed = manager.image(1, 1, 0).unwrap().texture;
        for (at, size) in [16, 96, 152, 256, 48, 16, 256, 96, 48]
            .into_iter()
            .enumerate()
        {
            manager.begin_frame(&ctx);
            let next = variant("zoom.txt", size);
            let mut sig = signature(at as u64 + 2, 1, at, at + 1);
            sig.size = size;
            let resident = manager.cache.contains_key(&next.key);
            manager.set_requests(1, sig, || vec![next.clone()]).unwrap();
            assert_eq!(manager.view_presented(1), (1, 1, usize::from(!resident)));
            if !resident {
                let pending = manager.image(1, sig.epoch, at).unwrap();
                assert_eq!(pending.texture, displayed);
                assert_eq!(pending.opacity, 1.0);
                assert!(inject_variant(&mut manager, &next));
            }
            let ready = manager.image(1, sig.epoch, at).unwrap();
            assert_eq!(ready.texture, manager.cache[&next.key].texture.id());
            assert_eq!(ready.opacity, 1.0, "size {size} replayed a cold fade");
            assert!(ready.previous.is_none());
            assert_eq!(manager.view_presented(1), (1, 1, 0));
            assert!(manager.views[0].transitions[0].previous.is_none());
            displayed = ready.texture;
        }
    }

    #[test]
    fn cached_navigation_and_model_revisions_present_the_exact_warm_texture_immediately() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        for value in [
            asset("warm.txt"),
            variant("warm.txt", 96),
            asset("other.txt"),
        ] {
            manager.begin_frame(&ctx);
            assert!(inject_variant(&mut manager, &value));
            manager.cache.get_mut(&value.key).unwrap().opacity = 1.0;
        }
        for epoch in 1..=20 {
            let value = asset(if epoch % 2 == 0 {
                "warm.txt"
            } else {
                "other.txt"
            });
            let expected = manager.cache[&value.key].texture.id();
            manager
                .set_requests(1, signature(epoch, epoch, 20, 21), || vec![value])
                .unwrap();
            let image = manager.image(1, epoch, 20).unwrap();
            assert_eq!(image.texture, expected);
            assert_eq!(image.opacity, 1.0);
            assert!(
                image.previous.is_none(),
                "warm navigation began a new crossfade"
            );
        }
    }

    #[test]
    fn retained_resolution_survives_unrelated_texture_pressure_then_releases_its_pin() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let old = variant("retained.txt", 256);
        assert!(inject_variant(&mut manager, &old));
        manager.cache.get_mut(&old.key).unwrap().opacity = 1.0;
        let texture = manager.cache[&old.key].texture.id();
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![old.clone()])
            .unwrap();
        let next = variant("retained.txt", 96);
        let mut sig = signature(2, 1, 0, 1);
        sig.size = 96;
        manager.set_requests(1, sig, || vec![next.clone()]).unwrap();
        assert!(manager.cache[&old.key].pinned);
        for index in 0..40 {
            manager.begin_frame(&ctx);
            manager.refresh_pins();
            assert!(inject_variant(
                &mut manager,
                &variant(&format!("pressure-{index}"), 256)
            ));
            assert!(manager.metrics().gpu_pixel_bytes <= GPU_PIXEL_BUDGET);
            assert!(manager.metrics().gpu_entries <= MAX_GPU_ENTRIES);
        }
        assert_eq!(manager.view_presented(1), (1, 1, 1));
        assert_eq!(manager.image(1, 2, 0).unwrap().texture, texture);
        manager.begin_frame(&ctx);
        assert!(inject_variant(&mut manager, &next));
        assert_eq!(manager.image(1, 2, 0).unwrap().opacity, 1.0);
        manager.refresh_pins();
        assert!(!manager.cache[&old.key].pinned);
        assert!(manager.cache[&next.key].pinned);
        manager.retain_views(&[]);
        assert!(!manager.cache[&next.key].pinned);
    }

    #[test]
    fn full_pinned_cache_atomically_funds_a_resolution_replacement_from_its_own_old_pixels() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let old: Vec<_> = (0..32)
            .map(|at| variant(&format!("full-{at}"), 256))
            .collect();
        manager
            .set_requests(1, signature(1, 1, 0, old.len()), || old.clone())
            .unwrap();
        for value in &old {
            manager.begin_frame(&ctx);
            assert!(inject_variant(&mut manager, value));
            manager.cache.get_mut(&value.key).unwrap().opacity = 1.0;
        }
        assert_eq!(manager.pixel_bytes, GPU_PIXEL_BUDGET);
        let next: Vec<_> = old
            .iter()
            .cloned()
            .map(|mut value| {
                value.key.size = 152;
                value
            })
            .collect();
        let mut sig = signature(2, 1, 0, next.len());
        sig.size = 152;
        manager.set_requests(1, sig, || next.clone()).unwrap();
        assert!(manager.cache.values().all(|cached| cached.pinned));
        assert_eq!(manager.view_presented(1), (32, 32, 32));
        manager.begin_frame(&ctx);
        assert!(inject_variant(&mut manager, &next[0]));
        assert!(!manager.cache.contains_key(&old[0].key));
        assert_eq!(manager.view_presented(1), (32, 32, 31));
        let image = manager.image(1, 2, 0).unwrap();
        assert_eq!(image.texture, manager.cache[&next[0].key].texture.id());
        assert_eq!(image.opacity, 1.0);
        assert!(image.previous.is_none());
        assert!(manager.pixel_bytes <= GPU_PIXEL_BUDGET);
        assert_eq!(manager.uploads, 1);
        assert_eq!(manager.budget_fallbacks, 0);
    }

    #[test]
    fn another_panes_current_texture_cannot_fund_a_resolution_replacement() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let old: Vec<_> = (0..32)
            .map(|at| variant(&format!("shared-full-{at}"), 256))
            .collect();
        manager
            .set_requests(1, signature(1, 1, 0, old.len()), || old.clone())
            .unwrap();
        for value in &old {
            manager.begin_frame(&ctx);
            assert!(inject_variant(&mut manager, value));
            manager.cache.get_mut(&value.key).unwrap().opacity = 1.0;
        }
        let next = variant("shared-full-0", 152);
        let mut sig = signature(2, 1, 0, 1);
        sig.size = 152;
        manager.set_requests(2, sig, || vec![next.clone()]).unwrap();
        manager.begin_frame(&ctx);
        assert!(!inject_variant(&mut manager, &next));
        assert_eq!(manager.view_presented(2), (1, 1, 1));
        assert_eq!(
            manager.image(2, 2, 0).unwrap().texture,
            manager.cache[&old[0].key].texture.id()
        );
        assert_eq!(manager.pixel_bytes, GPU_PIXEL_BUDGET);
        assert_eq!(manager.budget_fallbacks, 1);
    }

    #[test]
    fn insufficient_replacement_room_keeps_the_old_source_instead_of_evicting_it_early() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let mut old = vec![variant("small-retained.txt", 16)];
        old.extend((0..32).map(|at| variant(&format!("large-pinned-{at}"), 256)));
        manager
            .set_requests(1, signature(1, 1, 0, old.len()), || old.clone())
            .unwrap();
        for (at, value) in old.iter().enumerate() {
            manager.begin_frame(&ctx);
            let side = usize::from(value.key.size);
            // 31 full textures, one 255x256 texture and the 16x16 source
            // occupy exactly 8 MiB. Replacing only 16x16 cannot fund 96x96.
            let size = [if at == 32 { 255 } else { side }, side];
            assert!(manager.upload(
                value.key.clone(),
                value.stamp,
                1,
                size,
                &vec![255; size[0] * size[1] * 4],
                None,
            ));
            manager.cache.get_mut(&value.key).unwrap().opacity = 1.0;
        }
        assert_eq!(manager.pixel_bytes, GPU_PIXEL_BUDGET);
        let next = variant("small-retained.txt", 96);
        let mut requests = old.clone();
        requests[0] = next.clone();
        manager
            .set_requests(1, signature(2, 1, 0, requests.len()), || requests)
            .unwrap();
        manager.begin_frame(&ctx);
        assert!(!inject_variant(&mut manager, &next));
        assert!(manager.cache.contains_key(&old[0].key));
        assert_eq!(manager.view_presented(1), (33, 33, 1));
        assert_eq!(manager.image(1, 2, 0).unwrap().opacity, 1.0);
        assert_eq!(manager.pixel_bytes, GPU_PIXEL_BUDGET);
    }

    #[test]
    fn warm_icon_thumbnail_switch_and_identical_cold_pixels_do_not_start_a_new_fade() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let mut icon = asset("representation.txt");
        icon.key.thumbnail = false;
        let thumbnail = asset("representation.txt");
        assert!(inject_variant(&mut manager, &icon));
        manager.cache.get_mut(&icon.key).unwrap().opacity = 1.0;
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![icon.clone()])
            .unwrap();
        manager
            .set_requests(1, signature(2, 1, 0, 1), || vec![thumbnail.clone()])
            .unwrap();
        assert!(inject_variant(&mut manager, &thumbnail));
        let image = manager.image(1, 2, 0).unwrap();
        assert_eq!(
            image.opacity, 1.0,
            "identical native pixels must not crossfade"
        );
        assert!(image.previous.is_none());
        // Make the resident variants visually different to cover a warm switch
        // independently of the identical-pixel shortcut.
        manager.cache.get_mut(&thumbnail.key).unwrap().pixel_hash ^= 1;
        for (epoch, value) in [(3, icon), (4, thumbnail)] {
            manager
                .set_requests(1, signature(epoch, 1, 0, 1), || vec![value])
                .unwrap();
            let image = manager.image(1, epoch, 0).unwrap();
            assert_eq!(image.opacity, 1.0);
            assert!(image.previous.is_none());
        }
    }

    #[test]
    fn retained_pixels_require_the_exact_path_and_stamp_but_allow_metadata_free_requests() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let old = asset("one/same.txt");
        assert!(inject_variant(&mut manager, &old));
        manager.cache.get_mut(&old.key).unwrap().opacity = 1.0;
        let mut unobserved = variant("one/same.txt", 96);
        unobserved.stamp = AssetStamp::UNOBSERVED;
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![unobserved])
            .unwrap();
        assert_eq!(manager.view_presented(1), (1, 1, 1));
        assert_eq!(
            manager.image(1, 1, 0).unwrap().texture,
            manager.cache[&old.key].texture.id()
        );
        manager
            .set_requests(1, signature(2, 1, 0, 1), || {
                vec![variant("two/same.txt", 96)]
            })
            .unwrap();
        assert_eq!(manager.view_presented(1), (1, 0, 0));
        assert!(manager.image(1, 2, 0).is_none());
        let mut changed = variant("one/same.txt", 96);
        changed.stamp.modified_secs += 1;
        manager
            .set_requests(1, signature(3, 2, 0, 1), || vec![changed])
            .unwrap();
        assert_eq!(manager.view_presented(1), (1, 0, 0));
        assert!(manager.image(1, 3, 0).is_none());
    }

    #[test]
    fn cut_snapshot_changes_without_rebuilding_assets_and_rejects_stale_rows() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(7, 1, 20, 22), || {
                vec![asset("one.txt"), asset("folder")]
            })
            .unwrap();
        let key_pointer = manager.views[0].cut_keys[0].as_ref().unwrap() as *const _;
        manager
            .benchmark_set_cut_paths(&[asset("one.txt").key.path])
            .unwrap();
        assert!(manager.is_cut(1, 7, 20));
        assert!(!manager.is_cut(1, 7, 21));
        assert!(!manager.is_cut(1, 6, 20));
        assert!(!manager.is_cut(1, 7, 19));
        assert!(!manager.is_cut(1, 7, 22));
        assert!(!manager.is_cut(2, 7, 20));
        assert!(
            !manager
                .set_requests(1, signature(7, 1, 20, 22), || {
                    panic!("cut state must not rebuild the Shell viewport")
                })
                .unwrap()
        );
        assert_eq!(
            key_pointer,
            manager.views[0].cut_keys[0].as_ref().unwrap() as *const _
        );
        manager
            .benchmark_set_cut_paths(&[asset("folder").key.path])
            .unwrap();
        assert!(!manager.is_cut(1, 7, 20));
        assert!(manager.is_cut(1, 7, 21));
        assert!(manager.cancel_cut());
        assert!(!manager.is_cut(1, 7, 21));
        manager.benchmark_set_cut_paths(&[]).unwrap();
        assert!(!manager.is_cut(1, 7, 20));
        assert!(!manager.is_cut(1, 7, 21));
    }

    #[test]
    fn wide_grid_cut_lookup_reaches_rows_beyond_the_native_icon_lane_lazily() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(9, 1, 0, 1), || vec![asset("cached")])
            .unwrap();
        assert!(!manager.is_cut_row(1, 9, 200, || panic!(
            "empty cut state must not build a path"
        )));
        manager
            .benchmark_set_cut_paths(&[asset("beyond-icons").key.path])
            .unwrap();
        assert!(!manager.is_cut_row(1, 9, 0, || panic!(
            "cached rows already have a prepared path"
        )));
        assert!(manager.is_cut_row(1, 9, 200, || asset("beyond-icons").key.path));
        assert!(!manager.is_cut_row(1, 9, 201, || asset("other/beyond-icons").key.path));
    }

    #[test]
    fn unchanged_signature_never_rebuilds_paths_and_rows_reject_stale_epoch() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let builds = Cell::new(0);
        let sig = signature(9, 12, 20, 22);
        manager
            .set_requests(1, sig, || {
                builds.set(builds.get() + 1);
                vec![asset("a"), asset("b")]
            })
            .unwrap();
        assert!(
            !manager
                .set_requests(1, sig, || panic!("unchanged viewport rebuilt"))
                .unwrap()
        );
        assert_eq!(builds.get(), 1);
        assert!(inject(&mut manager, "a", 48));
        assert!(manager.image(1, 9, 20).is_some());
        assert!(manager.image(1, 8, 20).is_none());
        assert!(manager.image(1, 9, 19).is_none());
        assert!(manager.image(1, 9, 22).is_none());
    }

    #[test]
    fn identical_assets_preserve_worker_generation_and_fade_across_model_revisions() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 10, 0, 1), || vec![asset("a")])
            .unwrap();
        assert!(inject(&mut manager, "a", 48));
        let worker_epoch = manager.views[0].worker_epoch;
        let texture = manager.cache[&asset("a").key].texture.id();
        manager.views[0].transitions[0].blend = 0.4;
        manager.views[0].pending = true;
        manager.views[0].registered = true;
        for revision in 11..=100 {
            manager
                .set_requests(1, signature(2, revision, 20, 21), || vec![asset("a")])
                .unwrap();
            assert_eq!(manager.views[0].worker_epoch, worker_epoch);
            assert_eq!(manager.views[0].transitions[0].blend, 0.4);
            assert!(manager.views[0].pending);
            assert!(manager.views[0].registered);
        }
        assert_eq!(manager.serial, worker_epoch);
        assert_eq!(manager.cache[&asset("a").key].texture.id(), texture);
        assert_eq!(manager.cache[&asset("a").key].revision, 100);
        assert_eq!(manager.observations[0].revision, 100);
        assert!(manager.image(1, 1, 0).is_none());
        assert_eq!(manager.image(1, 2, 20).unwrap().texture, texture);

        let mut old_asset = asset("a");
        old_asset.stamp.bytes = 9;
        manager
            .set_requests(2, signature(3, 50, 0, 1), || vec![old_asset])
            .unwrap();
        assert_eq!(manager.cache[&asset("a").key].texture.id(), texture);
        assert_eq!(manager.observations[0].revision, 100);
    }

    #[test]
    fn changed_asset_keys_or_stamps_replace_worker_generation() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 10, 0, 1), || vec![asset("a")])
            .unwrap();
        let first_generation = manager.views[0].worker_epoch;
        manager
            .set_requests(1, signature(1, 11, 0, 1), || vec![asset("b")])
            .unwrap();
        let second_generation = manager.views[0].worker_epoch;
        assert_ne!(first_generation, second_generation);
        let mut changed = asset("b");
        changed.stamp.modified_secs += 1;
        manager
            .set_requests(1, signature(1, 12, 0, 1), || vec![changed])
            .unwrap();
        assert_ne!(second_generation, manager.views[0].worker_epoch);
    }

    #[test]
    fn missing_assets_retry_on_new_model_revision() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 10, 0, 1), || vec![asset("missing")])
            .unwrap();
        let first_generation = manager.views[0].worker_epoch;
        manager
            .set_requests(1, signature(1, 11, 0, 1), || vec![asset("missing")])
            .unwrap();
        assert_ne!(first_generation, manager.views[0].worker_epoch);
    }

    #[test]
    fn strict_request_and_view_bounds_run_before_builder() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        assert!(
            manager
                .set_requests(1, signature(1, 1, 0, MAX_REQUESTS_PER_VIEW + 1), || {
                    panic!("invalid row range ran the request builder")
                })
                .is_err()
        );
        assert!(manager.views.is_empty());
        let builds = Cell::new(0);
        for view in 1..=MAX_VIEWS as u64 {
            let rows = if view == 1 { MAX_REQUESTS_PER_VIEW } else { 0 };
            manager
                .set_requests(view, signature(1, 1, 0, rows), || {
                    builds.set(builds.get() + 1);
                    (0..rows)
                        .map(|row| asset(&format!("boundary-{row}")))
                        .collect()
                })
                .unwrap();
        }
        assert_eq!(builds.get(), MAX_VIEWS);
        assert_eq!(manager.views.len(), MAX_VIEWS);
        assert_eq!(manager.views[0].requests.len(), MAX_REQUESTS_PER_VIEW);
        let overflow_view = MAX_VIEWS as u64 + 1;
        assert!(
            manager
                .set_requests(overflow_view, signature(1, 1, 0, 0), || {
                    panic!("view overflow ran the request builder")
                })
                .is_err()
        );
        assert_eq!(manager.views.len(), MAX_VIEWS);
        // Updating an existing view at capacity must not be treated as a new view.
        manager
            .set_requests(1, signature(2, 2, 0, 0), Vec::new)
            .unwrap();
        assert_eq!(manager.views.len(), MAX_VIEWS);
        let retained: Vec<_> = (2..=MAX_VIEWS as u64).collect();
        manager.retain_views(&retained);
        assert_eq!(manager.views.len(), MAX_VIEWS - 1);
        assert!(!manager.views.iter().any(|view| view.id == 1));
        manager
            .set_requests(overflow_view, signature(1, 1, 0, 0), Vec::new)
            .unwrap();
        assert_eq!(manager.views.len(), MAX_VIEWS);
        assert!(manager.image(1, 1, 0).is_none());
        assert!(
            manager
                .set_requests(overflow_view, signature(1, 1, 0, 1), Vec::new)
                .is_err()
        );
        let unchanged = manager
            .views
            .iter()
            .find(|view| view.id == overflow_view)
            .unwrap();
        assert_eq!(unchanged.signature, signature(1, 1, 0, 0));
        assert!(unchanged.requests.is_empty());
    }

    #[test]
    fn reordered_view_uses_keys_and_new_epoch_instead_of_reused_row_positions() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(3, 1, 0, 2), || vec![asset("a"), asset("b")])
            .unwrap();
        assert!(inject(&mut manager, "a", 48));
        assert!(inject(&mut manager, "b", 48));
        let first = manager.image(1, 3, 0).unwrap().texture;
        let second = manager.image(1, 3, 1).unwrap().texture;
        manager
            .set_requests(1, signature(4, 1, 0, 2), || vec![asset("b"), asset("a")])
            .unwrap();
        assert_eq!(manager.image(1, 4, 0).unwrap().texture, second);
        assert_eq!(manager.image(1, 4, 1).unwrap().texture, first);
        assert!(manager.image(1, 3, 0).is_none());
    }

    #[test]
    fn two_upload_limit_and_pixel_validation_are_strict() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager.begin_frame(&ctx);
        assert!(inject(&mut manager, "a", 48));
        assert!(inject(&mut manager, "b", 48));
        assert!(!inject(&mut manager, "c", 48));
        assert_eq!(manager.metrics().uploads_this_frame, 2);
        manager.begin_frame(&ctx);
        assert!(!manager.upload(
            asset("c").key,
            AssetStamp::default(),
            1,
            [257, 1],
            &[0; 4],
            None
        ));
        assert!(!manager.upload(
            asset("c").key,
            AssetStamp::default(),
            1,
            [1, 1],
            &[0; 3],
            None
        ));
        assert_eq!(manager.metrics().gpu_entries, 2);
    }

    #[test]
    fn lru_is_bounded_and_visible_textures_are_pinned() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        for index in 0..40 {
            manager.begin_frame(&ctx);
            manager.refresh_pins();
            assert!(inject(&mut manager, &format!("file-{index}"), 256));
        }
        assert_eq!(manager.metrics().gpu_pixel_bytes, GPU_PIXEL_BUDGET);
        assert_eq!(manager.metrics().gpu_entries, 32);
        assert!(!manager.cache.contains_key(&asset("file-0").key));
        manager
            .set_requests(1, signature(1, 2, 0, 1), || vec![asset("file-8")])
            .unwrap();
        for index in 40..45 {
            manager.begin_frame(&ctx);
            manager.refresh_pins();
            assert!(inject(&mut manager, &format!("file-{index}"), 256));
        }
        assert!(manager.cache.contains_key(&asset("file-8").key));
        assert!(manager.metrics().gpu_pixel_bytes <= GPU_PIXEL_BUDGET);
    }

    #[test]
    fn stamp_change_evicts_all_sizes_but_old_model_cannot_invalidate_newer_observation() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 10, 0, 1), || vec![asset("a")])
            .unwrap();
        assert!(inject(&mut manager, "a", 48));
        let mut changed = asset("a");
        changed.stamp.bytes = 11;
        manager
            .set_requests(1, signature(1, 11, 0, 1), || vec![changed.clone()])
            .unwrap();
        assert_eq!(manager.metrics().gpu_entries, 0);
        assert!(!manager.observe(&changed.key.path, asset("a").stamp, 9));
        assert_eq!(manager.observations[0].stamp.bytes, 11);
    }

    #[test]
    fn metadata_free_launcher_icons_do_not_invalidate_a_listing_observation() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 10, 0, 1), || vec![asset("a")])
            .unwrap();
        assert!(inject(&mut manager, "a", 48));
        let mut icon = asset("a");
        icon.stamp = AssetStamp::UNOBSERVED;
        manager
            .set_requests(2, signature(2, 999, 0, 1), || vec![icon])
            .unwrap();
        assert_eq!(manager.metrics().gpu_entries, 1);
        assert_eq!(manager.observations[0].stamp, asset("a").stamp);
        assert_eq!(manager.observations[0].revision, 10);
    }

    #[test]
    fn observed_metadata_supersedes_an_identity_only_icon_at_any_revision() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        let key = asset("a").key;
        assert!(manager.upload(
            key,
            AssetStamp::UNOBSERVED,
            u64::MAX,
            [48, 48],
            &vec![255; 48 * 48 * 4],
            None
        ));
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![asset("a")])
            .unwrap();
        assert_eq!(manager.metrics().gpu_entries, 0);
        assert_eq!(manager.observations[0].stamp, asset("a").stamp);
    }

    #[test]
    fn observations_and_path_memory_are_bounded() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        // Short unique paths isolate the count ceiling from the byte ceiling.
        let largest_short = format!("{MAX_OBSERVATIONS:x}");
        assert!(path_weight(Path::new(&largest_short)) * MAX_OBSERVATIONS <= PATH_BUDGET);
        for index in 0..=MAX_OBSERVATIONS {
            manager.observe(Path::new(&format!("{index:x}")), AssetStamp::default(), 1);
            assert!(manager.observations.len() <= MAX_OBSERVATIONS);
            assert!(manager.observation_bytes <= PATH_BUDGET);
        }
        assert_eq!(manager.observations.len(), MAX_OBSERVATIONS);
        assert!(
            !manager
                .observations
                .iter()
                .any(|old| old.path == Path::new("0"))
        );
        assert!(
            manager
                .observations
                .iter()
                .any(|old| old.path == Path::new(&largest_short))
        );
        assert_eq!(
            manager.observation_bytes,
            manager
                .observations
                .iter()
                .map(|old| old.bytes)
                .sum::<usize>()
        );

        // Long, individually valid paths hit the byte ceiling before the count.
        let mut path_manager = AssetManager::disabled(&ctx);
        let padding = "x".repeat(MAX_PATH_BYTES / 4);
        let first_path = asset(&format!("memory-{:08x}-{padding}", 0)).key.path;
        validate_path(&first_path).unwrap();
        let weight = path_weight(&first_path);
        let path_capacity = PATH_BUDGET / weight;
        assert!(path_capacity > 0 && path_capacity < MAX_OBSERVATIONS);
        for index in 0..=path_capacity {
            let path = asset(&format!("memory-{index:08x}-{padding}")).key.path;
            validate_path(&path).unwrap();
            assert_eq!(path_weight(&path), weight);
            path_manager.observe(&path, AssetStamp::default(), 1);
            assert!(path_manager.observation_bytes <= PATH_BUDGET);
            assert!(path_manager.observations.len() <= MAX_OBSERVATIONS);
        }
        assert_eq!(path_manager.observations.len(), path_capacity);
        assert_eq!(path_manager.observation_bytes, path_capacity * weight);
        assert!(
            !path_manager
                .observations
                .iter()
                .any(|old| old.path == first_path)
        );
        let last_path = asset(&format!("memory-{path_capacity:08x}-{padding}"))
            .key
            .path;
        assert!(
            path_manager
                .observations
                .iter()
                .any(|old| old.path == last_path)
        );
        assert_eq!(
            path_manager.observation_bytes,
            path_manager
                .observations
                .iter()
                .map(|old| old.bytes)
                .sum::<usize>()
        );

        // Accept the exact per-path boundary and reject the next code unit.
        let prefix_weight = path_weight(&asset("x").key.path);
        let boundary_chars = (MAX_PATH_BYTES - prefix_weight) / 2 + 1;
        let boundary = asset(&"x".repeat(boundary_chars));
        assert_eq!(path_weight(&boundary.key.path), MAX_PATH_BYTES);
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![boundary.clone()])
            .unwrap();
        let oversized = asset(&"x".repeat(boundary_chars + 1));
        assert!(path_weight(&oversized.key.path) > MAX_PATH_BYTES);
        assert!(
            manager
                .set_requests(1, signature(1, 2, 0, 1), || vec![oversized])
                .is_err()
        );
        assert_eq!(manager.views[0].signature, signature(1, 1, 0, 1));
        assert_eq!(manager.views[0].requests[0].key.path, boundary.key.path);
    }

    #[test]
    fn explicit_refresh_invalidates_visible_assets_and_preserves_unrelated_lru() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![asset("a")])
            .unwrap();
        assert!(inject(&mut manager, "a", 48));
        assert!(inject(&mut manager, "b", 48));
        manager.invalidate_visible().unwrap();
        assert!(!manager.cache.contains_key(&asset("a").key));
        assert!(manager.cache.contains_key(&asset("b").key));
        assert_eq!(manager.metrics().gpu_entries, 1);
    }

    #[test]
    fn tiny_textures_are_entry_bounded_and_fully_pinned_cache_uses_vector_fallback() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        for index in 0..300 {
            manager.begin_frame(&ctx);
            manager.refresh_pins();
            assert!(inject(&mut manager, &format!("tiny-{index}"), 1));
        }
        assert_eq!(manager.metrics().gpu_entries, MAX_GPU_ENTRIES);
        manager.cache.clear();
        manager.pixel_bytes = 0;
        manager.cache_path_bytes = 0;
        for index in 0..32 {
            manager.begin_frame(&ctx);
            assert!(inject(&mut manager, &format!("pinned-{index}"), 256));
        }
        manager.begin_frame(&ctx);
        assert!(!inject(&mut manager, "fallback", 256));
        assert_eq!(manager.metrics().gpu_pixel_bytes, GPU_PIXEL_BUDGET);
        assert_eq!(manager.metrics().budget_fallbacks, 1);
    }

    #[test]
    fn reduced_motion_snaps_existing_fade_and_settled_image_requests_no_frames() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![asset("a")])
            .unwrap();
        assert!(inject(&mut manager, "a", 48));
        manager.set_motion_duration(0.0);
        for _ in 0..5 {
            let mut output = ctx.run_ui(Default::default(), |ui| {
                manager.begin_frame(ui.ctx());
                assert_eq!(manager.image(1, 1, 0).unwrap().opacity, 1.0);
            });
            output.textures_delta.clear(); // Headless test intentionally has no renderer.
            assert_eq!(manager.metrics().pending_controls, 0);
            if !ctx.has_requested_repaint() {
                assert_eq!(
                    output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                    Duration::MAX
                );
                return;
            }
        }
        panic!("settled asset sustained idle redraws");
    }

    #[test]
    fn normal_crossfade_is_monotonic_then_stops_repainting() {
        let ctx = Context::default();
        let mut manager = AssetManager::disabled(&ctx);
        manager
            .set_requests(1, signature(1, 1, 0, 1), || vec![asset("a")])
            .unwrap();
        assert!(inject(&mut manager, "a", 48));
        let mut previous = 0.0;
        for frame in 0..60 {
            let input = egui::RawInput {
                time: Some(frame as f64 / 60.0),
                predicted_dt: 1.0 / 60.0,
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| {
                manager.begin_frame(ui.ctx());
                let opacity = manager.image(1, 1, 0).unwrap().opacity;
                assert!(opacity >= previous && opacity <= 1.0);
                previous = opacity;
            });
            output.textures_delta.clear(); // Headless test intentionally has no renderer.
            if previous == 1.0 && !ctx.has_requested_repaint() {
                assert_eq!(
                    output.viewport_output[&egui::ViewportId::ROOT].repaint_delay,
                    Duration::MAX
                );
                return;
            }
        }
        panic!("completed native-image crossfade sustained idle redraws");
    }
}
