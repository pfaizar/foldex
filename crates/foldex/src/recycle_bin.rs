//! The Recycle Bin is a shell namespace, never a directory. Native identities
//! remain opaque PIDLs in this model; ordinal keys below are presentation only.
//! A single lazy worker owns enumeration, model preparation, filtering and verbs.
pub(crate) mod state;
use crate::{navigation::RECYCLE_BIN_ADDRESS, pane::columns::Column};
use foldex_core::{Entry, Query, Snapshot};
use foldex_shell::recycle::Item;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread,
    time::Instant,
};

const MAX_VIEWS: usize = 16;

pub(crate) fn icon_key(entry: &Entry, size: u16) -> foldex_assets::AssetKey {
    use foldex_assets::{AssetKey, StockIcon};
    if entry.is_dir {
        AssetKey::stock_icon(StockIcon::Folder, size)
    } else {
        AssetKey::file_type_icon(entry.extension(), size)
            .unwrap_or_else(|| AssetKey::stock_icon(StockIcon::File, size))
    }
}

#[derive(Clone)]
pub(crate) enum Intent {
    Empty,
    RestoreAll,
    RestoreSelected,
    DeleteSelected,
    PropertiesSelected,
    Context(egui::Pos2),
    Retry,
}

pub(crate) struct Model {
    pub snapshot: Arc<Snapshot>,
    pub items: Vec<Item>,
    pub locations: Vec<String>,
}
impl Model {
    pub(crate) fn new(items: Vec<Item>) -> Arc<Self> {
        let mut entries = Vec::with_capacity(items.len());
        let mut locations = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let mut entry =
                Entry::from_name(item.name.as_str(), item.is_dir, item.size, item.deleted_at);
            // This key only disambiguates duplicate display names in egui. It
            // carries no PIDL, original path or recycled filesystem path.
            entry.relative_path = Some(PathBuf::from(format!("item-{index}")));
            entry.hidden = false;
            entries.push(entry);
            locations.push(
                item.original_path
                    .parent()
                    .unwrap_or(&item.original_path)
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        Arc::new(Self {
            snapshot: Arc::new(Snapshot::from_entries(RECYCLE_BIN_ADDRESS.into(), entries)),
            items,
            locations,
        })
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[derive(Clone)]
pub(crate) enum Target {
    All(Arc<Model>),
    Selection {
        model: Arc<Model>,
        order: Arc<[u32]>,
        ranges: Vec<(u32, u32)>,
    },
}
impl Target {
    pub(crate) fn count(&self) -> usize {
        match self {
            Self::All(model) => model.items.len(),
            Self::Selection { order, ranges, .. } => {
                ranges.iter().fold(0usize, |count, &(a, b)| {
                    count.saturating_add(
                        (b as usize + 1).min(order.len()).saturating_sub(a as usize),
                    )
                })
            }
        }
    }
    fn ids(&self) -> Vec<Vec<u8>> {
        match self {
            Self::All(model) => model.items.iter().map(|item| item.id.clone()).collect(),
            Self::Selection {
                model,
                order,
                ranges,
            } => ranges
                .iter()
                .flat_map(|&(a, b)| (a as usize)..(b as usize + 1).min(order.len()))
                .filter_map(|row| model.items.get(order[row] as usize))
                .map(|item| item.id.clone())
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Restore,
    Remove,
    Empty,
    Properties,
}

pub(crate) struct ViewRequest {
    pub pane: u64,
    pub navigation: u64,
    pub generation: u64,
    pub model: Arc<Model>,
    pub query: Query,
    pub sort: Option<(Column, bool)>,
}
pub(crate) struct PreparedView {
    pub request: ViewRequest,
    pub indices: Arc<[u32]>,
    pub counts: (usize, usize),
    pub elapsed: std::time::Duration,
}

fn prepare(request: ViewRequest) -> PreparedView {
    let begin = Instant::now();
    let view = foldex_core::filter_snapshot(request.model.snapshot.clone(), &request.query);
    let indices = if let Some((column, descending)) = request.sort {
        let model = &request.model;
        let mut indices = view.indices.to_vec();
        indices.sort_by(|&a, &b| {
            let a_item = &model.items[a as usize];
            let b_item = &model.items[b as usize];
            let comparison = match column {
                Column::OriginalLocation => {
                    model.locations[a as usize].cmp(&model.locations[b as usize])
                }
                Column::DateDeleted => a_item.deleted_at.cmp(&b_item.deleted_at),
                Column::Size => a_item.size.cmp(&b_item.size),
                _ => a_item.name.cmp(&b_item.name),
            };
            let comparison = if descending {
                comparison.reverse()
            } else {
                comparison
            };
            // Raw PIDLs distinguish same-name items without synthesizing paths.
            comparison.then_with(|| a_item.id.cmp(&b_item.id))
        });
        Arc::from(indices)
    } else {
        view.indices
    };
    PreparedView {
        request,
        indices,
        counts: view.kind_counts,
        elapsed: begin.elapsed(),
    }
}

trait Backend: Send + Sync {
    fn enumerate(&self, cancel: &AtomicBool) -> Result<Vec<Item>, String>;
    fn operate(&self, operation: Operation, ids: &[Vec<u8>], owner: isize) -> Result<(), String>;
}
struct Native;
impl Backend for Native {
    fn enumerate(&self, cancel: &AtomicBool) -> Result<Vec<Item>, String> {
        foldex_shell::recycle::enumerate_cancellable(cancel)
    }
    fn operate(&self, operation: Operation, ids: &[Vec<u8>], owner: isize) -> Result<(), String> {
        use foldex_shell::recycle;
        match operation {
            Operation::Restore => recycle::restore(ids, owner),
            Operation::Remove => recycle::remove(ids, owner),
            // This operation is queued only after the app's affirmative modal.
            Operation::Empty => recycle::empty_confirmed(owner),
            Operation::Properties => recycle::properties(ids, owner),
        }
    }
}

struct Load {
    generation: u64,
    cancel: Arc<AtomicBool>,
}
struct Verb {
    operation: Operation,
    target: Target,
    owner: isize,
}
#[derive(Default)]
struct Mailbox {
    load: Option<Load>,
    views: Vec<ViewRequest>,
    verb: Option<Verb>,
    stop: bool,
}
#[derive(Default)]
struct Replies {
    model: Option<(u64, Result<Arc<Model>, String>)>,
    views: Vec<PreparedView>,
    operation: Option<(Operation, Result<(), String>)>,
}
struct Worker {
    mailbox: Arc<Mutex<Mailbox>>,
    replies: Arc<Mutex<Replies>>,
    signal: SyncSender<()>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Ok(mut mailbox) = self.mailbox.lock() {
            mailbox.stop = true;
            mailbox.views.clear();
            mailbox.load = None;
        }
        let _ = self.signal.try_send(());
    }
}

pub(crate) struct Service {
    backend: Arc<dyn Backend>,
    wake: Arc<dyn Fn() + Send + Sync>,
    visible: Arc<AtomicBool>,
    worker: Option<Worker>,
    cancel: Arc<AtomicBool>,
    generation: u64,
    needs_refresh: bool,
    pub model: Option<Arc<Model>>,
    pub error: Option<String>,
    operation_error: Option<String>,
    pub loading: bool,
    pub pending: bool,
    /// Synthetic preview/testing never initializes a native worker.
    inert: bool,
}
impl Drop for Service {
    fn drop(&mut self) {
        // Native enumeration cooperatively stops between shell calls. Do not
        // join a COM worker or wake a closing viewport from this UI destructor.
        self.cancel.store(true, Ordering::Release);
        self.visible.store(false, Ordering::Release);
    }
}
impl Service {
    pub(crate) fn new(wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            backend: Arc::new(Native),
            wake,
            visible: Arc::new(AtomicBool::new(false)),
            worker: None,
            cancel: Arc::new(AtomicBool::new(false)),
            generation: 0,
            needs_refresh: true,
            model: None,
            error: None,
            operation_error: None,
            loading: false,
            pending: false,
            inert: false,
        }
    }
    fn start(&mut self) -> Result<(), String> {
        if self.worker.is_some() || self.inert {
            return Ok(());
        }
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let replies = Arc::new(Mutex::new(Replies::default()));
        let (signal, receive) = mpsc::sync_channel(1);
        let input = mailbox.clone();
        let output = replies.clone();
        let backend = self.backend.clone();
        let visible = self.visible.clone();
        let wake = self.wake.clone();
        thread::Builder::new()
            .name("foldex-recycle-bin".into())
            .spawn(move || {
                while receive.recv().is_ok() {
                    loop {
                        enum Job {
                            Load(Load),
                            View(ViewRequest),
                            Verb(Verb),
                        }
                        let job = {
                            let Ok(mut input) = input.lock() else {
                                return;
                            };
                            if input.stop {
                                return;
                            }
                            if let Some(verb) = input.verb.take() {
                                Some(Job::Verb(verb))
                            } else if let Some(load) = input.load.take() {
                                Some(Job::Load(load))
                            } else {
                                input.views.pop().map(Job::View)
                            }
                        };
                        let Some(job) = job else {
                            break;
                        };
                        match job {
                            Job::Load(load) => {
                                let result = backend.enumerate(&load.cancel).map(Model::new);
                                if load.cancel.load(Ordering::Acquire) {
                                    continue;
                                }
                                if let Ok(mut replies) = output.lock() {
                                    replies.model = Some((load.generation, result));
                                }
                            }
                            Job::View(request) => {
                                if !visible.load(Ordering::Acquire) {
                                    continue;
                                }
                                let view = prepare(request);
                                if let Ok(mut replies) = output.lock() {
                                    replies
                                        .views
                                        .retain(|v| v.request.pane != view.request.pane);
                                    if replies.views.len() < MAX_VIEWS {
                                        replies.views.push(view);
                                    }
                                }
                            }
                            Job::Verb(verb) => {
                                let ids = if verb.operation == Operation::Empty {
                                    Vec::new()
                                } else {
                                    verb.target.ids()
                                };
                                let result = if verb.operation != Operation::Empty && ids.is_empty()
                                {
                                    Err("The Recycle Bin selection is empty".into())
                                } else {
                                    backend.operate(verb.operation, &ids, verb.owner)
                                };
                                if let Ok(mut replies) = output.lock() {
                                    replies.operation = Some((verb.operation, result));
                                }
                            }
                        }
                        if visible.load(Ordering::Acquire) {
                            wake();
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        self.worker = Some(Worker {
            mailbox,
            replies,
            signal,
        });
        Ok(())
    }
    pub(crate) fn set_visible(&mut self, visible: bool) {
        let previous = self.visible.swap(visible, Ordering::AcqRel);
        if !visible {
            if previous {
                self.cancel.store(true, Ordering::Release);
                self.generation = self.generation.wrapping_add(1);
                self.loading = false;
                self.needs_refresh = true;
                if let Some(worker) = &self.worker
                    && let Ok(mut input) = worker.mailbox.lock()
                {
                    input.load = None;
                    input.views.clear();
                }
            }
            return;
        }
        if self.needs_refresh && !self.pending {
            self.refresh();
        }
    }
    pub(crate) fn refresh(&mut self) {
        if self.inert {
            return;
        }
        self.needs_refresh = true;
        if !self.visible.load(Ordering::Acquire) || self.pending {
            return;
        }
        if let Err(error) = self.start() {
            self.error = Some(error);
            self.needs_refresh = false;
            return;
        }
        self.cancel.store(true, Ordering::Release);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.generation = self.generation.wrapping_add(1);
        self.loading = true;
        self.error = None;
        self.operation_error = None;
        self.needs_refresh = false;
        let worker = self.worker.as_ref().unwrap();
        if let Ok(mut input) = worker.mailbox.lock() {
            input.load = Some(Load {
                generation: self.generation,
                cancel: self.cancel.clone(),
            });
            input.views.clear();
        }
        let _ = worker.signal.try_send(());
    }
    /// Latest request per pane; a fixed mailbox prevents typing from queuing work.
    pub(crate) fn request_view(&mut self, request: ViewRequest) -> bool {
        if self.inert {
            return false;
        }
        let Some(worker) = &self.worker else {
            return false;
        };
        let Ok(mut input) = worker.mailbox.lock() else {
            return false;
        };
        input.views.retain(|view| view.pane != request.pane);
        if input.views.len() >= MAX_VIEWS {
            return false;
        }
        input.views.push(request);
        drop(input);
        let _ = worker.signal.try_send(());
        true
    }
    pub(crate) fn submit(&mut self, operation: Operation, target: Target, owner: isize) -> bool {
        if self.pending || self.loading || self.inert || target.count() == 0 {
            return false;
        }
        let Some(worker) = &self.worker else {
            return false;
        };
        let Ok(mut input) = worker.mailbox.lock() else {
            return false;
        };
        if input.verb.is_some() {
            return false;
        }
        input.verb = Some(Verb {
            operation,
            target,
            owner,
        });
        self.pending = true;
        self.error = None;
        self.operation_error = None;
        drop(input);
        let _ = worker.signal.try_send(());
        true
    }
    /// Hidden routes do not poll, schedule timers, or request repaint.
    pub(crate) fn poll(&mut self) -> Vec<PreparedView> {
        if !self.visible.load(Ordering::Acquire) {
            return Vec::new();
        }
        let Some(worker) = &self.worker else {
            return Vec::new();
        };
        let replies = worker
            .replies
            .lock()
            .map(|mut replies| std::mem::take(&mut *replies))
            .unwrap_or_default();
        if let Some((generation, result)) = replies.model
            && generation == self.generation
        {
            self.loading = false;
            match result {
                Ok(model) => {
                    state::observe(model.is_empty());
                    self.model = Some(model);
                    self.error = self.operation_error.clone();
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some((operation, result)) = replies.operation {
            self.pending = false;
            let error = result.err();
            if operation != Operation::Properties {
                // A native operation may have made partial progress before a
                // cancel/error. Always renew its namespace view afterwards.
                self.refresh();
                self.operation_error = error.clone();
            }
            self.error = error;
        }
        replies.views
    }
    pub(crate) fn inject(&mut self, items: Vec<Item>) -> Arc<Model> {
        self.inert = true;
        self.cancel.store(true, Ordering::Release);
        self.worker = None;
        self.pending = false;
        self.loading = false;
        self.needs_refresh = false;
        self.error = None;
        self.operation_error = None;
        let model = Model::new(items);
        self.model = Some(model.clone());
        model
    }
    pub(crate) fn is_inert(&self) -> bool {
        self.inert
    }
    pub(crate) fn prepare_inert(request: ViewRequest) -> PreparedView {
        prepare(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recycle_row_assets_use_only_native_type_or_stock_keys_without_paths_or_thumbnails() {
        let model = Model::new(vec![
            item(1, "report.xlsx", "C:/private/report.xlsx", 1),
            item(2, "Folder", "C:/private/Folder", 2),
        ]);
        let file = icon_key(&model.snapshot.entries[0], 152);
        assert_eq!(file.path, PathBuf::from("foldex-icon:type/xlsx"));
        assert!(!file.thumbnail);
        assert!(foldex_assets::is_native_icon_path(&file.path));
        let folder = icon_key(&model.snapshot.entries[1], 152);
        assert_eq!(
            folder,
            foldex_assets::AssetKey::stock_icon(foldex_assets::StockIcon::Folder, 152)
        );
        assert!(!folder.thumbnail);
        assert_eq!(
            icon_key(&Entry::from_name("README", false, 0, 0), 152),
            foldex_assets::AssetKey::stock_icon(foldex_assets::StockIcon::File, 152)
        );
    }
    fn item(id: u8, name: &str, location: &str, deleted_at: i64) -> Item {
        Item {
            id: vec![id],
            name: name.into(),
            original_path: location.into(),
            deleted_at,
            size: id as u64 * 100,
            is_dir: name == "Folder",
            file_system_path: None,
        }
    }
    #[test]
    fn model_keeps_duplicate_name_pidls_separate_from_inert_row_keys() {
        let model = Model::new(vec![
            item(1, "same.txt", "C:/a/same.txt", 1),
            item(2, "same.txt", "C:/b/same.txt", 2),
        ]);
        assert_ne!(
            model.snapshot.entries[0].relative_path,
            model.snapshot.entries[1].relative_path
        );
        assert_eq!(model.items[0].id, [1]);
        assert_eq!(model.items[1].id, [2]);
        assert_eq!(model.locations, ["C:/a", "C:/b"]);
    }
    #[test]
    fn injected_views_filter_count_types_and_sort_deleted_metadata() {
        let model = Model::new(vec![
            item(1, "alpha.txt", "C:/b/alpha.txt", 10),
            item(2, "beta.txt", "C:/a/beta.txt", 20),
            item(3, "Folder", "C:/a/Folder", 30),
        ]);
        let view = prepare(ViewRequest {
            pane: 1,
            navigation: 1,
            generation: 1,
            model,
            query: Query {
                text: "txt".into(),
                ..Default::default()
            },
            sort: Some((Column::DateDeleted, true)),
        });
        assert_eq!(&*view.indices, &[1, 0]);
        assert_eq!(view.counts, (0, 2));
    }
    #[test]
    fn new_service_is_dormant_and_inert_preview_cannot_submit_native_work() {
        let mut service = Service::new(Arc::new(|| panic!("hidden service woke UI")));
        assert!(service.worker.is_none());
        service.set_visible(false);
        assert!(service.poll().is_empty());
        let model = service.inject(vec![item(1, "safe.txt", "C:/safe.txt", 1)]);
        assert!(!service.submit(Operation::Empty, Target::All(model), 0));
        assert!(service.worker.is_none());
    }

    #[test]
    fn dropping_service_cancels_enumeration_and_suppresses_worker_wakes() {
        let service = Service::new(Arc::new(|| panic!("closed service woke UI")));
        let cancel = service.cancel.clone();
        let visible = service.visible.clone();
        visible.store(true, Ordering::Release);
        drop(service);
        assert!(cancel.load(Ordering::Acquire));
        assert!(!visible.load(Ordering::Acquire));
    }

    #[derive(Default)]
    struct MockBackend {
        enumerations: std::sync::atomic::AtomicUsize,
        verbs: Mutex<Vec<(Operation, Vec<Vec<u8>>)>>,
    }
    impl Backend for MockBackend {
        fn enumerate(&self, cancel: &AtomicBool) -> Result<Vec<Item>, String> {
            self.enumerations.fetch_add(1, Ordering::SeqCst);
            if cancel.load(Ordering::Acquire) {
                return Err("cancelled mock".into());
            }
            Ok(vec![
                item(1, "same.txt", "C:/same.txt", 1),
                item(2, "same.txt", "C:/same.txt", 2),
            ])
        }
        fn operate(
            &self,
            operation: Operation,
            ids: &[Vec<u8>],
            _owner: isize,
        ) -> Result<(), String> {
            self.verbs.lock().unwrap().push((operation, ids.to_vec()));
            Ok(())
        }
    }
    fn await_service(service: &mut Service, ready: impl Fn(&Service) -> bool) {
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while !ready(service) && Instant::now() < deadline {
            service.poll();
            thread::yield_now();
        }
        assert!(ready(service), "mock service did not finish");
    }
    #[test]
    fn mocked_worker_is_lazy_pauses_hidden_routes_and_uses_exact_frozen_pidls() {
        let backend = Arc::new(MockBackend::default());
        let mut service = Service::new(Arc::new(|| {}));
        service.backend = backend.clone();
        assert!(service.worker.is_none());
        assert_eq!(backend.enumerations.load(Ordering::SeqCst), 0);
        service.set_visible(true);
        await_service(&mut service, |service| {
            service.model.is_some() && !service.loading
        });
        assert_eq!(backend.enumerations.load(Ordering::SeqCst), 1);
        let model = service.model.as_ref().unwrap().clone();
        let target = Target::Selection {
            model,
            order: Arc::from([1, 0]),
            ranges: vec![(0, 0)],
        };
        assert!(service.submit(Operation::Restore, target, 0));
        await_service(&mut service, |service| !service.pending && !service.loading);
        assert_eq!(
            *backend.verbs.lock().unwrap(),
            [(Operation::Restore, vec![vec![2]])]
        );
        assert_eq!(
            backend.enumerations.load(Ordering::SeqCst),
            2,
            "mutations renew the namespace view"
        );
        service.set_visible(false);
        for _ in 0..8 {
            assert!(service.poll().is_empty());
        }
        service.refresh();
        assert_eq!(backend.enumerations.load(Ordering::SeqCst), 2);
        assert!(!service.loading);
        service.set_visible(true);
        await_service(&mut service, |service| !service.loading);
        assert_eq!(backend.enumerations.load(Ordering::SeqCst), 3);
    }
}
