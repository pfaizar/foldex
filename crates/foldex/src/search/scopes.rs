//! Catalog lifetime belongs to a scope, independently of any query subscriber.
//! Watch callbacks publish invalidation counters only; scanning stays on bounded
//! search turns and uses a cancellation token owned by the scope.
use super::*;
use notify::{Config, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};

const MAX_SCOPES: usize = MAX_SUBSCRIPTIONS;
const IDLE_SCOPE_CACHE: usize = 8;
const MAX_SCANS: usize = 2;
const CHANGE_QUIET: Duration = Duration::from_millis(120);
const WATCH_RETRY: Duration = Duration::from_secs(5);
const REBUILD_SPACING: Duration = Duration::from_secs(1);

struct Signal {
    revision: AtomicU64,
    healthy: AtomicBool,
}
enum Watch {
    Native { _watcher: RecommendedWatcher },
    Poll { _watcher: PollWatcher },
    Unavailable,
}
struct Monitor {
    signal: Arc<Signal>,
    _watch: Watch,
}
impl Monitor {
    fn native(&self) -> bool {
        matches!(self._watch, Watch::Native { .. })
    }
}
impl Monitor {
    fn new(root: &Path, excluded: &Path, shared: &Arc<Shared>, directory_only: bool) -> Self {
        let mode = if directory_only {
            RecursiveMode::NonRecursive
        } else {
            RecursiveMode::Recursive
        };
        let signal = Arc::new(Signal {
            revision: AtomicU64::new(0),
            healthy: AtomicBool::new(true),
        });
        let callback = || {
            let signal = signal.clone();
            let shared = shared.clone();
            let excluded = excluded.to_owned();
            let root = std::path::absolute(root).unwrap_or_else(|_| root.to_owned());
            move |event: notify::Result<notify::Event>| {
                match &event {
                    Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
                    Ok(event)
                        if !event.need_rescan()
                            && !event.paths.is_empty()
                            && event.paths.iter().all(|path| {
                                path.ancestors()
                                    .any(|parent| crate::bookmarks::path_eq(parent, &excluded))
                            }) =>
                    {
                        return;
                    }
                    Err(_) => signal.healthy.store(false, Ordering::Release),
                    Ok(event)
                        if matches!(
                            event.kind,
                            EventKind::Remove(_)
                                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                        ) && event
                            .paths
                            .iter()
                            .any(|path| crate::bookmarks::path_eq(path, &root)) =>
                    {
                        signal.healthy.store(false, Ordering::Release)
                    }
                    _ => {}
                }
                signal.revision.fetch_add(1, Ordering::AcqRel);
                shared.notify();
            }
        };
        let watch = match RecommendedWatcher::new(
            callback(),
            Config::default().with_follow_symlinks(false),
        )
        .and_then(|mut watcher| {
            watcher.watch(root, mode)?;
            Ok(watcher)
        }) {
            Ok(watcher) => Watch::Native { _watcher: watcher },
            Err(_) => match PollWatcher::new(
                callback(),
                Config::default()
                    .with_follow_symlinks(false)
                    .with_poll_interval(Duration::from_secs(2))
                    .with_compare_contents(false),
            )
            .and_then(|mut watcher| {
                watcher.watch(root, mode)?;
                Ok(watcher)
            }) {
                Ok(watcher) => Watch::Poll { _watcher: watcher },
                Err(_) => {
                    signal.healthy.store(false, Ordering::Release);
                    Watch::Unavailable
                }
            },
        };
        Self {
            signal,
            _watch: watch,
        }
    }
}

struct Scope {
    root: PathBuf,
    directory_only: bool,
    cache_dir: PathBuf,
    catalog: Rc<RefCell<SharedCatalog>>,
    monitor: Monitor,
    cancel: Cancellation,
    seen: u64,
    dirty: Option<Instant>,
    running: bool,
    active: bool,
    refresh_serial: u64,
    last_used: Instant,
    last_publication: Instant,
    last_progress: Instant,
    retried: Instant,
    scans: u64,
    last_rebuild: Option<Instant>,
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(Backend::Building(builder)) = self.catalog.borrow_mut().backend.take() {
            let _ = builder.finish_cancelled();
        }
    }
}

pub(super) struct Update {
    pub catalog: Rc<RefCell<SharedCatalog>>,
    pub status: foldex_index::Result<IndexStatus>,
    pub refetch: bool,
}
pub(super) struct Scopes {
    values: Vec<Scope>,
    shared: Arc<Shared>,
    base: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::super::live_tests::Fixture;
    use super::*;

    fn settle(scopes: &mut Scopes, root: &Path, backend: &Backend) {
        let until = Instant::now() + Duration::from_secs(15);
        while !backend.complete() || !backend.status().complete {
            for update in scopes.advance(&[(root.to_owned(), false)]) {
                update.status.unwrap();
            }
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn subscriber_cancellation_does_not_cancel_the_shared_scan() {
        let fixture = Fixture::new();
        let hub = SearchHub::unstarted_for_test();
        let client = hub.client();
        client.request(fixture.root(), Query::default(), 0, false);
        let request = client.state.input().unwrap();
        let mut scopes = Scopes::new(hub.shared.clone(), fixture.cache());
        let (_, backend) = scopes.backend(&request).unwrap();
        request.scan_cancel.cancel();
        settle(&mut scopes, &fixture.root(), &backend);
        assert_eq!(scopes.values[0].scans, 1);
        assert!(!scopes.values[0].cancel.is_cancelled());
    }
    #[test]
    fn watch_loss_marks_rows_stale_and_recovery_reconciles_the_scope() {
        let fixture = Fixture::new();
        let hub = SearchHub::unstarted_for_test();
        let client = hub.client();
        client.request(fixture.root(), Query::default(), 0, false);
        let mut scopes = Scopes::new(hub.shared.clone(), fixture.cache());
        let (_, backend) = scopes.backend(&client.state.input().unwrap()).unwrap();
        settle(&mut scopes, &fixture.root(), &backend);
        let generation = backend.status().generation;
        scopes.values[0]
            .monitor
            .signal
            .healthy
            .store(false, Ordering::Release);
        scopes.values[0]
            .monitor
            .signal
            .revision
            .fetch_add(1, Ordering::AcqRel);
        scopes.advance(&[(fixture.root(), false)]);
        assert!(backend.status().from_cache);
        assert!(scopes.deadline().is_some());
        scopes.values[0].retried = Instant::now() - WATCH_RETRY;
        scopes.advance(&[(fixture.root(), false)]);
        settle(&mut scopes, &fixture.root(), &backend);
        assert_ne!(backend.status().generation, generation);
        assert!(!backend.status().from_cache);
    }
    #[test]
    fn inactive_dirty_scopes_do_not_schedule_idle_timers_or_busy_work() {
        let fixture = Fixture::new();
        let hub = SearchHub::unstarted_for_test();
        let client = hub.client();
        client.request(fixture.root(), Query::default(), 0, false);
        let mut scopes = Scopes::new(hub.shared.clone(), fixture.cache());
        let (_, backend) = scopes.backend(&client.state.input().unwrap()).unwrap();
        settle(&mut scopes, &fixture.root(), &backend);
        scopes.values[0].dirty = Some(Instant::now());
        scopes.advance(&[]);
        assert!(scopes.deadline().is_none());
        assert!(!scopes.busy());
    }
    #[test]
    fn twelve_subscribed_scopes_finish_with_at_most_two_running_traversals() {
        let fixture = Fixture::new();
        let hub = SearchHub::unstarted_for_test();
        let client = hub.client();
        let mut scopes = Scopes::new(hub.shared.clone(), fixture.cache());
        let mut roots = Vec::new();
        let mut backends = Vec::new();
        for n in 0..12 {
            let root = fixture.root().join(format!("visible{n}"));
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("report.txt"), b"a").unwrap();
            client.request(root.clone(), Query::default(), 0, false);
            let (_, backend) = scopes.backend(&client.state.input().unwrap()).unwrap();
            roots.push((root, false));
            backends.push(backend);
        }
        let until = Instant::now() + Duration::from_secs(15);
        while backends.iter().any(|backend| !backend.complete()) {
            for update in scopes.advance(&roots) {
                update.status.unwrap();
            }
            assert!(scopes.values.iter().filter(|scope| scope.running).count() <= MAX_SCANS);
            assert!(
                Instant::now() < until,
                "waiting scope never acquired a traversal permit"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            scopes.values.iter().map(|scope| scope.scans).sum::<u64>(),
            12
        );
        assert!(backends.iter().all(|backend| backend.status().files == 1));
    }

    #[test]
    fn unused_catalogs_and_watchers_are_retained_within_a_fixed_limit() {
        let fixture = Fixture::new();
        let hub = SearchHub::unstarted_for_test();
        let client = hub.client();
        let mut scopes = Scopes::new(hub.shared.clone(), fixture.cache());
        for n in 0..MAX_SCOPES + 4 {
            let root = fixture.root().join(format!("root{n}"));
            std::fs::create_dir(&root).unwrap();
            client.request(root.clone(), Query::default(), 0, false);
            let (_, backend) = scopes.backend(&client.state.input().unwrap()).unwrap();
            settle(&mut scopes, &root, &backend);
            assert!(scopes.values.len() <= MAX_SCOPES);
        }
        assert_eq!(scopes.values.len(), IDLE_SCOPE_CACHE);
    }
}
impl Scopes {
    pub fn new(shared: Arc<Shared>, base: PathBuf) -> Self {
        Self {
            values: Vec::new(),
            shared,
            base,
        }
    }
    pub fn backend(&mut self, request: &Request) -> foldex_index::Result<(PathBuf, Backend)> {
        if let Some(scope) = self.values.iter_mut().find(|scope| {
            scope.directory_only == request.directory_only
                && crate::bookmarks::path_eq(&scope.root, &request.root)
        }) {
            if scope.catalog.borrow().backend.is_none() {
                scope.cancel = Cancellation::new();
                let builder = Self::builder(
                    &scope.cache_dir,
                    &self.base,
                    &scope.root,
                    scope.directory_only,
                    &scope.cancel,
                )?;
                scope.catalog.borrow_mut().backend = Some(Backend::Building(builder));
                scope.dirty = None;
            }
            scope.last_used = Instant::now();
            if request.refresh && scope.refresh_serial != request.serial {
                scope.refresh_serial = request.serial;
                scope.dirty = Some(Instant::now());
                scope.catalog.borrow_mut().stale = true;
            }
            return Ok((
                scope.cache_dir.clone(),
                Backend::Shared(scope.catalog.clone()),
            ));
        }
        if self.values.len() >= IDLE_SCOPE_CACHE {
            let unused = self
                .values
                .iter()
                .enumerate()
                .filter(|(_, scope)| Rc::strong_count(&scope.catalog) == 1)
                .min_by_key(|(_, scope)| scope.last_used)
                .map(|(index, _)| index);
            if let Some(index) = unused {
                self.values.remove(index);
            } else if self.values.len() == MAX_SCOPES {
                return Err(foldex_index::Error::InvalidIndex(format!(
                    "At most {MAX_SCOPES} live search scopes can be subscribed"
                )));
            }
        }
        std::fs::create_dir_all(&self.base)?;
        let cache_dir = Index::cache_dir_for(
            &scope_cache_base(&self.base, request.directory_only),
            &request.root,
        )?;
        let monitor = Monitor::new(
            &request.root,
            &self.base,
            &self.shared,
            request.directory_only,
        );
        let cancel = Cancellation::new();
        let cached = (!request.refresh)
            .then(|| {
                NativeIndex::open_scope(&cache_dir, &request.root)
                    .map(Backend::Cached)
                    .or_else(|_| Index::open_scope(&cache_dir, &request.root).map(Backend::Legacy))
            })
            .and_then(Result::ok);
        let backend = match cached {
            Some(backend) => backend,
            None => Backend::Building(Self::builder(
                &cache_dir,
                &self.base,
                &request.root,
                request.directory_only,
                &cancel,
            )?),
        };
        let last_status = backend.status();
        let catalog = Rc::new(RefCell::new(SharedCatalog {
            backend: Some(backend),
            stale: false,
            last_status,
        }));
        let now = Instant::now();
        self.values.push(Scope {
            root: request.root.clone(),
            directory_only: request.directory_only,
            cache_dir: cache_dir.clone(),
            catalog: catalog.clone(),
            monitor,
            cancel,
            seen: 0,
            dirty: None,
            running: false,
            active: true,
            refresh_serial: request.serial,
            last_used: now,
            last_publication: now,
            last_progress: now,
            retried: now,
            scans: 0,
            last_rebuild: None,
        });
        Ok((cache_dir, Backend::Shared(catalog)))
    }
    fn builder(
        cache_dir: &Path,
        base: &Path,
        root: &Path,
        directory_only: bool,
        cancel: &Cancellation,
    ) -> foldex_index::Result<IndexBuilder> {
        IndexBuilder::create_with_options(
            cache_dir,
            root,
            &BuildOptions {
                excluded_root: Some(base.to_owned()),
                directory_only,
            },
            cancel,
        )
    }
    /// Query contexts own their immutable selection generations. A refresh only
    /// replaces the live scope; it never mutates a selected generation on disk.
    pub fn advance(&mut self, active: &[(PathBuf, bool)]) -> Vec<Update> {
        let mut updates = Vec::new();
        let mut running = self.values.iter().filter(|scope| scope.running).count();
        let now = Instant::now();
        for scope in &mut self.values {
            let active = active.iter().any(|(root, directory_only)| {
                *directory_only == scope.directory_only
                    && crate::bookmarks::path_eq(root, &scope.root)
            });
            scope.active = active;
            let revision = scope.monitor.signal.revision.load(Ordering::Acquire);
            if revision != scope.seen {
                scope.seen = revision;
                let due = (now + CHANGE_QUIET).max(
                    scope
                        .last_rebuild
                        .map_or(now, |started| started + REBUILD_SPACING),
                );
                scope.dirty.get_or_insert(due);
                scope.catalog.borrow_mut().stale = true;
                let status = Backend::Shared(scope.catalog.clone()).status();
                updates.push(Update {
                    catalog: scope.catalog.clone(),
                    status: Ok(status),
                    refetch: false,
                });
            }
            if (!scope.monitor.signal.healthy.load(Ordering::Acquire) || !scope.monitor.native())
                && scope.retried.elapsed() >= WATCH_RETRY
            {
                scope.monitor =
                    Monitor::new(&scope.root, &self.base, &self.shared, scope.directory_only);
                scope.seen = scope.monitor.signal.revision.load(Ordering::Acquire);
                scope.retried = now;
                scope.dirty = Some(now);
                scope.catalog.borrow_mut().stale = true;
            }
            if !active {
                if scope.running {
                    scope.cancel.cancel();
                    let mut catalog = scope.catalog.borrow_mut();
                    if let Some(Backend::Building(builder)) = catalog.backend.take() {
                        catalog.backend = builder.finish_cancelled().ok().map(Backend::Cached);
                    }
                    scope.running = false;
                    running = running.saturating_sub(1);
                    scope.dirty = Some(now);
                    catalog.stale = true;
                }
                continue;
            }
            let mut catalog = scope.catalog.borrow_mut();
            let rebuilding = matches!(catalog.backend, Some(Backend::Building(_)));
            let needs_rebuild = matches!(
                catalog.backend,
                Some(Backend::Cached(_) | Backend::Legacy(_))
            ) || catalog.backend.is_none()
                || scope.dirty.is_some_and(|due| due <= now);
            if !rebuilding && needs_rebuild && running < MAX_SCANS {
                scope.cancel = Cancellation::new();
                match Self::builder(
                    &scope.cache_dir,
                    &self.base,
                    &scope.root,
                    scope.directory_only,
                    &scope.cancel,
                ) {
                    Ok(builder) => {
                        catalog.backend = Some(Backend::Building(builder));
                        catalog.stale = false;
                        scope.dirty = None;
                        scope.last_rebuild = Some(now);
                    }
                    Err(error) => {
                        updates.push(Update {
                            catalog: scope.catalog.clone(),
                            status: Err(error),
                            refetch: false,
                        });
                        continue;
                    }
                }
            }
            let Some(Backend::Building(builder)) = catalog.backend.as_mut() else {
                continue;
            };
            if !scope.running {
                if running == MAX_SCANS {
                    continue;
                }
                scope.running = true;
                running += 1;
                scope.scans += 1;
            }
            let progress = builder.scan_step(PAGE_ROWS, &scope.cancel, &thread::yield_now);
            match progress {
                Ok(progress) => {
                    let refetch = progress.done
                        || scope.last_publication.elapsed() >= REFRESH_INTERVAL
                        || (progress.status.entries > 0 && catalog.last_status.entries == 0);
                    if progress.done {
                        if progress.status.entries == 0 && progress.status.skipped > 0 {
                            scope.monitor.signal.healthy.store(false, Ordering::Release);
                        }
                        let Some(Backend::Building(builder)) = catalog.backend.take() else {
                            unreachable!()
                        };
                        match builder.finish(true) {
                            Ok(index) => catalog.backend = Some(Backend::Complete(index)),
                            Err(error) => {
                                updates.push(Update {
                                    catalog: scope.catalog.clone(),
                                    status: Err(error),
                                    refetch: false,
                                });
                                scope.running = false;
                                running = running.saturating_sub(1);
                                continue;
                            }
                        }
                        scope.running = false;
                        running = running.saturating_sub(1);
                        // Changes arriving while enumeration was running require another
                        // coalesced pass; complete traversal alone does not imply freshness.
                        catalog.stale = scope.dirty.is_some()
                            || !scope.monitor.signal.healthy.load(Ordering::Acquire);
                    }
                    if refetch {
                        scope.last_publication = now;
                    }
                    let mut status = catalog.backend.as_ref().unwrap().status();
                    status.from_cache |= catalog.stale;
                    catalog.last_status = status.clone();
                    if refetch || scope.last_progress.elapsed() >= PROGRESS_INTERVAL {
                        scope.last_progress = now;
                        updates.push(Update {
                            catalog: scope.catalog.clone(),
                            status: Ok(status),
                            refetch,
                        });
                    }
                }
                Err(error) => {
                    scope.cancel.cancel();
                    scope.dirty = Some(now + WATCH_RETRY);
                    updates.push(Update {
                        catalog: scope.catalog.clone(),
                        status: Err(error),
                        refetch: false,
                    });
                }
            }
        }
        updates
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.values
            .iter()
            .filter(|scope| scope.active)
            .flat_map(|scope| {
                scope.dirty.into_iter().chain(
                    (!scope.monitor.signal.healthy.load(Ordering::Acquire)
                        || !scope.monitor.native())
                    .then_some(scope.retried + WATCH_RETRY),
                )
            })
            .min()
    }
    pub fn busy(&self) -> bool {
        self.values.iter().any(|scope| {
            scope.active
                && (scope.running
                    || matches!(
                        scope.catalog.borrow().backend,
                        Some(Backend::Building(_) | Backend::Cached(_) | Backend::Legacy(_))
                    ))
        })
    }
    pub fn active_paths(&self, cache_dir: &Path) -> Vec<PathBuf> {
        self.values
            .iter()
            .filter(|scope| scope.cache_dir == cache_dir)
            .filter_map(|scope| {
                scope
                    .catalog
                    .borrow()
                    .backend
                    .as_ref()
                    .map(Backend::database_path)
            })
            .collect()
    }
}
