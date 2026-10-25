use super::*;

fn retry(mut submit: impl FnMut() -> io::Result<()>) {
    let start = Instant::now();
    loop {
        match submit() {
            Ok(()) => return,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    && start.elapsed() < Duration::from_secs(5) =>
            {
                thread::yield_now()
            }
            Err(error) => panic!("{error}"),
        }
    }
}
fn receive_final(service: &FolderStatsService, view: u64, epoch: u64) -> FolderEvent {
    let start = Instant::now();
    loop {
        if let Some(event) = service.try_recv() {
            if event.view == view && event.epoch == epoch && event.stats.state != State::Scanning {
                return event;
            }
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "folder statistics timeout"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn join(service: FolderStatsService) {
    service.shutdown();
    let start = Instant::now();
    while !service.is_worker_finished() {
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(service.metrics().open_handles, 0);
}

#[test]
fn request_counts_views_paths_and_mailbox_are_bounded() {
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    let path = if cfg!(windows) {
        PathBuf::from("D:/owned-fixture")
    } else {
        PathBuf::from("/owned-fixture")
    };
    assert!(
        service
            .set_requests(
                1,
                1,
                &vec![FolderRequest { path: path.clone() }; MAX_REQUESTS_PER_VIEW + 1]
            )
            .is_err()
    );
    assert!(
        service
            .set_requests(
                1,
                1,
                &[FolderRequest {
                    path: PathBuf::from("relative")
                }]
            )
            .is_err()
    );
    retry(|| service.set_requests(1, 1, &[]));
    retry(|| service.set_requests(2, 1, &[]));
    assert_eq!(
        service.set_requests(3, 1, &[]).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    {
        let _guard = service.shared.state.lock().unwrap();
        assert_eq!(
            service.set_requests(1, 2, &[]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    retry(|| service.cancel_view(1));
    retry(|| service.set_requests(3, 1, &[]));
    join(service);
}

#[test]
fn cancelling_an_absent_view_does_not_wake_or_advance_the_service_revision() {
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    let before = service.shared.revision.load(Ordering::Acquire);
    for _ in 0..100 {
        retry(|| service.cancel_view(999));
    }
    assert_eq!(service.shared.revision.load(Ordering::Acquire), before);
    assert_eq!(service.metrics().walks_started, 0);
    join(service);
}

#[test]
fn pending_invalidation_blocks_claims_and_child_changes_reset_ancestor_tokens() {
    let root = Arc::new(PathBuf::from("D:/owned/root"));
    let child = Arc::new(root.join("child"));
    let sibling = Arc::new(PathBuf::from("D:/owned/sibling"));
    let mut state = Mailbox::default();
    state.views.push(View {
        id: 1,
        epoch: 1,
        floor: 0,
        weight: 0,
        requests: [root.clone(), child.clone(), sibling.clone()]
            .into_iter()
            .map(|path| Request {
                path,
                token: 0,
                floor: 0,
                stage: Stage::Queued,
            })
            .collect(),
    });
    let parent = state.claim(&root, true);
    let other = state.claim(&sibling, true);
    state.publish(&root, &parent, Arc::default(), true);
    state.publish(&sibling, &other, Arc::default(), true);
    let invalidation = Invalidation::scope(Arc::new(child.join("changed-file")));
    state.reset_paths(false, std::slice::from_ref(&invalidation));
    state.invalidate(invalidation);
    assert!(state.candidate().is_none());
    state.retire_events();
    assert_eq!(state.events.len(), 1);
    assert_eq!(state.events[0].event.path, sibling);
    assert!(!state.current(&parent[0], &root));
    assert!(state.current(&other[0], &sibling));
    assert!(state.views[0].requests[0].stage == Stage::Queued);
    assert!(state.views[0].requests[1].stage == Stage::Queued);
    assert!(state.views[0].requests[2].stage == Stage::Done);
    state.invalidations.clear();
    assert_eq!(state.candidate().unwrap().0, root);
}

#[test]
fn renewing_a_child_tree_does_not_invalidate_its_ancestor_total() {
    let root = PathBuf::from("D:/owned/root");
    let child = Arc::new(root.join("child"));
    let mut state = Mailbox::default();
    state.invalidate(Invalidation {
        path: child.clone(),
        ancestors: false,
    });
    assert!(!state.invalidations[0].matches(&root));
    assert!(state.invalidations[0].matches(&child));
    assert!(state.invalidations[0].matches(&child.join("descendant")));
    state.invalidate(Invalidation::scope(child));
    assert_eq!(state.invalidations.len(), 1);
    assert!(state.invalidations[0].matches(&root));
}

#[cfg(windows)]
#[test]
fn child_file_invalidation_refreshes_ancestor_cache_after_its_view_was_released() {
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    fixture.dir("root/child");
    let changed = fixture.file("root/child/a", 1);
    let other = fixture.dir("other");
    fixture.file("other/a", 2);
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    assert_eq!(receive_final(&service, 1, 1).stats.bytes, 1);
    retry(|| service.set_requests(2, 1, &request(&other)));
    let sibling = receive_final(&service, 2, 1);
    let before = service.metrics().walks_started;
    retry(|| service.cancel_view(1));
    std::fs::write(&changed, [0; 39]).unwrap();
    retry(|| service.invalidate_scope(&changed));
    retry(|| service.set_requests(1, 1, &request(&root)));
    assert_eq!(receive_final(&service, 1, 1).stats.bytes, 39);
    assert_eq!(service.metrics().walks_started, before + 1);
    retry(|| service.cancel_view(2));
    retry(|| service.set_requests(2, 1, &request(&other)));
    assert!(Arc::ptr_eq(
        &sibling.stats,
        &receive_final(&service, 2, 1).stats
    ));
    assert_eq!(service.metrics().walks_started, before + 1);
    join(service);
}

#[cfg(windows)]
#[test]
fn releasing_and_reacquiring_a_view_reuses_settled_stats_without_a_new_walk() {
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    fixture.file("root/a", 39);
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    let completed = receive_final(&service, 1, 1);
    let before = service.metrics();
    retry(|| service.cancel_view(1));
    retry(|| service.set_requests(2, 1, &request(&root)));
    let reacquired = receive_final(&service, 2, 1);
    assert!(Arc::ptr_eq(&completed.stats, &reacquired.stats));
    assert_eq!(service.metrics().walks_started, before.walks_started);
    assert_eq!(service.metrics().invalidations, before.invalidations);
    join(service);
}

#[cfg(windows)]
#[test]
fn explicit_scope_invalidation_refreshes_descendants_without_restarting_a_sibling_scope() {
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    let child = fixture.dir("root/child");
    let other = fixture.dir("other");
    fixture.file("root/child/a", 1);
    fixture.file("other/a", 39);
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&child)));
    assert_eq!(receive_final(&service, 1, 1).stats.bytes, 1);
    retry(|| service.set_requests(2, 1, &request(&other)));
    assert_eq!(receive_final(&service, 2, 1).stats.bytes, 39);
    let before = service.metrics().walks_started;
    fixture.file("root/child/b", 38);
    retry(|| service.invalidate_scope(&root));
    let changed = receive_final(&service, 1, 1);
    assert_eq!((changed.stats.files, changed.stats.bytes), (2, 39));
    assert_eq!(service.metrics().walks_started, before + 1);
    assert!(
        service.try_recv().is_none(),
        "an unrelated scope must stay settled"
    );
    join(service);
}

#[test]
fn cache_has_one_bounded_generation_per_path_and_rejects_old_revisions() {
    let mut cache = Cache::default();
    for index in 0..MAX_CACHE_ENTRIES * 4 {
        cache.insert(
            Arc::new(PathBuf::from(format!("D:/owned/{index}"))),
            index as u64,
            Arc::new(FolderSnapshot {
                state: State::Complete,
                ..Default::default()
            }),
        );
        assert!(cache.values.len() <= MAX_CACHE_ENTRIES && cache.bytes <= CACHE_PATH_BUDGET);
    }
    let path = Arc::new(PathBuf::from("D:/same"));
    cache.insert(
        path.clone(),
        100,
        Arc::new(FolderSnapshot {
            files: 1,
            state: State::Complete,
            ..Default::default()
        }),
    );
    assert!(cache.get(&path, 101).is_none());
    cache.insert(
        path.clone(),
        101,
        Arc::new(FolderSnapshot {
            files: 2,
            state: State::Complete,
            ..Default::default()
        }),
    );
    assert_eq!(cache.get(&path, 101).unwrap().files, 2);
    assert_eq!(cache.values.keys().filter(|key| ***key == *path).count(), 1);
}

#[test]
fn idle_shutdown_uses_a_sticky_wake_and_starts_no_scan() {
    for _ in 0..16 {
        let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
        assert_eq!(service.metrics().active_walks, 0);
        join(service);
    }
}

#[cfg(windows)]
pub(super) struct Fixture {
    root: PathBuf,
    base: PathBuf,
}
#[cfg(windows)]
impl Fixture {
    pub(super) fn new() -> Self {
        let base =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/folder-stats-fixtures");
        std::fs::create_dir_all(&base).unwrap();
        let base = std::fs::canonicalize(base).unwrap();
        static TICKET: AtomicU64 = AtomicU64::new(1);
        let root = base.join(format!(
            "owned-{}-{}-{}",
            std::process::id(),
            TICKET.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        Self { root, base }
    }
    pub(super) fn dir(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    pub(super) fn file(&self, name: &str, size: usize) -> PathBuf {
        let path = self.root.join(name);
        std::fs::write(&path, vec![7; size]).unwrap();
        path
    }
}
#[cfg(windows)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let resolved = std::fs::canonicalize(&self.root).unwrap();
        assert_eq!(resolved, self.root);
        assert!(resolved.starts_with(&self.base));
        assert!(
            resolved
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("owned-")
        );
        std::fs::remove_dir_all(resolved).unwrap();
    }
}
#[cfg(windows)]
fn request(path: &PathBuf) -> Vec<FolderRequest> {
    vec![FolderRequest { path: path.clone() }]
}

#[cfg(windows)]
#[test]
fn recursive_counts_match_the_source_design_fixture_and_empty_folder() {
    let fixture = Fixture::new();
    let design = fixture.dir("Design");
    fixture.file("Design/example.txt", 39);
    let empty = fixture.dir("Empty");
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&design)));
    let result = receive_final(&service, 1, 1);
    assert_eq!(
        (result.stats.folders, result.stats.files, result.stats.bytes),
        (0, 1, 39)
    );
    assert_eq!(result.stats.state, State::Complete);
    retry(|| service.set_requests(1, 2, &request(&empty)));
    let result = receive_final(&service, 1, 2);
    assert_eq!(
        (result.stats.folders, result.stats.files, result.stats.bytes),
        (0, 0, 0)
    );
    assert_eq!(result.stats.state, State::Complete);
    join(service);
}

#[cfg(windows)]
#[test]
fn wide_trees_do_not_store_each_child_and_depth_limit_is_explicit() {
    let fixture = Fixture::new();
    let wide = fixture.dir("wide");
    for index in 0..300 {
        fixture.dir(&format!("wide/d-{index}"));
        fixture.file(&format!("wide/d-{index}/a"), 3);
    }
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&wide)));
    let result = receive_final(&service, 1, 1);
    assert_eq!(
        (result.stats.folders, result.stats.files, result.stats.bytes),
        (300, 300, 900)
    );
    assert_eq!(result.stats.state, State::Complete);
    let deep = fixture.dir("deep");
    let mut path = deep.clone();
    for _ in 0..MAX_DEPTH + 10 {
        path.push("d");
        std::fs::create_dir(&path).unwrap();
    }
    std::fs::write(path.join("beyond-limit"), b"not enumerated").unwrap();
    retry(|| service.set_requests(1, 2, &request(&deep)));
    let result = receive_final(&service, 1, 2);
    assert_eq!(result.stats.state, State::Partial);
    assert!(result.stats.issues.depth_limited > 0);
    assert_eq!(result.stats.files, 0);
    assert!(service.metrics().open_handles <= MAX_ACTIVE_WALKS * MAX_DEPTH * 2);
    assert!(service.metrics().traversal_path_bytes <= MAX_ACTIVE_WALKS * WALK_PATH_BUDGET);
    join(service);
}

#[cfg(windows)]
#[test]
fn epoch_refresh_recounts_and_same_epoch_reuse_does_no_idle_work() {
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    fixture.file("root/a", 1);
    let wakes = Arc::new(AtomicUsize::new(0));
    let count = wakes.clone();
    let service = FolderStatsService::new(Arc::new(move || {
        count.fetch_add(1, Ordering::Relaxed);
    }))
    .unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    assert_eq!(receive_final(&service, 1, 1).stats.bytes, 1);
    let settled = Instant::now();
    while !service.shared.state.lock().unwrap().idle {
        assert!(settled.elapsed() < Duration::from_secs(5));
        thread::yield_now();
    }
    let before = wakes.load(Ordering::Relaxed);
    let sequence = service.shared.state.lock().unwrap().sequence;
    for _ in 0..100 {
        retry(|| service.set_requests(1, 1, &request(&root)));
    }
    let duplicate = vec![
        FolderRequest { path: root.clone() },
        FolderRequest { path: root.clone() },
    ];
    for _ in 0..100 {
        retry(|| service.set_requests(1, 1, &duplicate));
    }
    assert_eq!(service.shared.state.lock().unwrap().sequence, sequence);
    thread::sleep(Duration::from_millis(40));
    assert_eq!(wakes.load(Ordering::Relaxed), before);
    assert!(service.try_recv().is_none());
    fixture.file("root/b", 38);
    retry(|| service.set_requests(1, 2, &request(&root)));
    let result = receive_final(&service, 1, 2);
    assert_eq!((result.stats.files, result.stats.bytes), (2, 39));
    join(service);
}

#[cfg(windows)]
#[test]
fn newer_epoch_after_empty_view_cannot_reuse_an_old_cache_generation() {
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    fixture.file("root/a", 1);
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    receive_final(&service, 1, 1);
    retry(|| service.set_requests(1, 2, &[]));
    fixture.file("root/b", 2);
    retry(|| service.set_requests(1, 2, &request(&root)));
    assert_eq!(receive_final(&service, 1, 2).stats.bytes, 3);
    join(service);
}

#[cfg(windows)]
#[test]
fn cancellation_rejects_stale_events_and_releases_native_walks() {
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    for index in 0..2000 {
        fixture.file(&format!("root/{index}"), 1);
    }
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    retry(|| service.cancel_view(1));
    let start = Instant::now();
    while service.metrics().active_walks != 0 {
        assert!(start.elapsed() < Duration::from_secs(5));
        thread::sleep(Duration::from_millis(1));
    }
    assert!(service.try_recv().is_none());
    let small = fixture.dir("small");
    fixture.file("small/a", 39);
    retry(|| service.set_requests(2, 1, &request(&small)));
    assert_eq!(receive_final(&service, 2, 1).stats.bytes, 39);
    join(service);
}

#[cfg(windows)]
#[test]
fn queued_results_and_cache_remain_bounded_with_a_stalled_consumer() {
    let fixture = Fixture::new();
    let paths: Vec<_> = (0..64)
        .map(|index| fixture.dir(&format!("d-{index}")))
        .collect();
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    let left: Vec<_> = paths[..32]
        .iter()
        .cloned()
        .map(|path| FolderRequest { path })
        .collect();
    let right: Vec<_> = paths[32..]
        .iter()
        .cloned()
        .map(|path| FolderRequest { path })
        .collect();
    retry(|| service.set_requests(1, 1, &left));
    retry(|| service.set_requests(2, 1, &right));
    let start = Instant::now();
    loop {
        let done = service
            .shared
            .state
            .lock()
            .unwrap()
            .views
            .iter()
            .all(|view| {
                view.requests
                    .iter()
                    .all(|request| request.stage == Stage::Done)
            });
        if done {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(1));
    }
    let metrics = service.metrics();
    assert!(metrics.event_slots <= MAX_EVENTS);
    assert!(metrics.cached_entries <= MAX_CACHE_ENTRIES);
    assert!(metrics.cached_path_bytes <= CACHE_PATH_BUDGET);
    join(service);
}

#[cfg(windows)]
#[test]
fn unc_and_mapped_remote_names_are_rejected_before_enumeration() {
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    let remote = PathBuf::from("\\\\does-not-exist.invalid\\share\\folder");
    retry(|| service.set_requests(1, 1, &request(&remote)));
    let result = receive_final(&service, 1, 1);
    assert_eq!(result.stats.state, State::Unavailable);
    assert_eq!(result.stats.issues.skipped_nonlocal, 1);
    assert_eq!(result.stats.visited, 0);
    join(service);
}

#[cfg(windows)]
fn junction(link: &std::path::Path, target: &std::path::Path) -> io::Result<()> {
    use std::os::windows::{
        ffi::OsStrExt,
        io::{FromRawHandle, OwnedHandle},
    };
    use windows_sys::Win32::{
        Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::*,
        System::{
            IO::DeviceIoControl, Ioctl::FSCTL_SET_REPARSE_POINT,
            SystemServices::IO_REPARSE_TAG_MOUNT_POINT,
        },
    };
    std::fs::create_dir(link).unwrap();
    let target: Vec<u16> = target.as_os_str().encode_wide().collect();
    let print = target.strip_prefix(&[92, 92, 63, 92]).unwrap_or(&target);
    let mut substitute = vec![92, 63, 63, 92];
    substitute.extend_from_slice(print);
    let data_bytes = 8 + (substitute.len() + 1 + print.len() + 1) * 2;
    assert!(data_bytes <= u16::MAX as usize);
    let mut data = vec![0u8; 8 + data_bytes];
    data[..4].copy_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    data[4..6].copy_from_slice(&(data_bytes as u16).to_le_bytes());
    data[10..12].copy_from_slice(&((substitute.len() * 2) as u16).to_le_bytes());
    data[12..14].copy_from_slice(&(((substitute.len() + 1) * 2) as u16).to_le_bytes());
    data[14..16].copy_from_slice(&((print.len() * 2) as u16).to_le_bytes());
    for (index, unit) in substitute
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .chain(print.iter().copied())
        .chain(std::iter::once(0))
        .enumerate()
    {
        data[16 + index * 2..18 + index * 2].copy_from_slice(&unit.to_le_bytes());
    }
    let mut wide: Vec<_> = link.as_os_str().encode_wide().collect();
    wide.push(0);
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _guard = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let mut returned = 0;
    if unsafe {
        DeviceIoControl(
            raw,
            FSCTL_SET_REPARSE_POINT,
            data.as_ptr().cast(),
            data.len() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn generated_junction_and_its_descendant_root_are_never_followed() {
    let fixture = Fixture::new();
    let visible = fixture.dir("visible");
    let outside = fixture.dir("outside");
    fixture.dir("outside/deeper");
    fixture.file("outside/deeper/hidden", 4096);
    fixture.file("visible/normal", 39);
    let link = visible.join("link");
    if let Err(error) = junction(&link, &outside) {
        std::fs::remove_dir(&link).unwrap();
        if let Err(symbolic_error) = std::os::windows::fs::symlink_dir(&outside, &link) {
            eprintln!(
                "Native reparse fixture unavailable: junction={error}; symlink={symbolic_error}. Deterministic tagged-metadata traversal coverage runs separately; native link creation is not claimed."
            );
            return;
        }
    }
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&visible)));
    let result = receive_final(&service, 1, 1);
    assert_eq!(result.stats.state, State::Partial);
    assert_eq!(result.stats.issues.skipped_reparse, 1);
    assert_eq!(
        (result.stats.folders, result.stats.files, result.stats.bytes),
        (0, 1, 39)
    );
    let descendant = link.join("deeper");
    retry(|| service.set_requests(1, 2, &request(&descendant)));
    let result = receive_final(&service, 1, 2);
    assert_eq!(result.stats.state, State::Unavailable);
    assert_eq!(result.stats.visited, 0);
    assert_eq!(result.stats.issues.skipped_reparse, 1);
    join(service);
    std::fs::remove_dir(link).unwrap();
    assert!(outside.join("deeper/hidden").exists());
}

#[cfg(windows)]
#[test]
fn offline_file_is_counted_as_an_exclusion_without_reading_its_contents() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::*;
    let fixture = Fixture::new();
    let root = fixture.dir("visible");
    let path = fixture.file("visible/offline", 4096);
    fixture.file("visible/normal", 39);
    let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let before = unsafe { GetFileAttributesW(wide.as_ptr()) };
    assert_ne!(
        unsafe { SetFileAttributesW(wide.as_ptr(), before | FILE_ATTRIBUTE_OFFLINE) },
        0,
        "{}",
        io::Error::last_os_error()
    );
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    let result = receive_final(&service, 1, 1);
    assert_eq!(result.stats.state, State::Partial);
    assert_eq!(result.stats.issues.skipped_offline, 1);
    assert_eq!((result.stats.files, result.stats.bytes), (1, 39));
    join(service);
    assert_ne!(unsafe { SetFileAttributesW(wide.as_ptr(), before) }, 0);
}

#[cfg(windows)]
#[test]
fn progressive_snapshot_precedes_completion_and_is_coalesced() {
    use std::sync::mpsc;
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    for index in 0..1000 {
        fixture.file(&format!("root/{index}"), 1);
    }
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let (resume_tx, resume_rx) = mpsc::sync_channel(1);
    let resume = Mutex::new(resume_rx);
    let count = AtomicUsize::new(0);
    let service = FolderStatsService::new(Arc::new(move || {
        if count.fetch_add(1, Ordering::Relaxed) == 0 {
            ready_tx.send(()).unwrap();
            resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
    }))
    .unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let partial = service.try_recv().unwrap();
    assert_eq!(partial.stats.state, State::Scanning);
    assert!(partial.stats.files > 0 && partial.stats.files < 1000);
    resume_tx.send(()).unwrap();
    let result = receive_final(&service, 1, 1);
    assert_eq!((result.stats.files, result.stats.bytes), (1000, 1000));
    assert_eq!(result.stats.state, State::Complete);
    join(service);
}

#[cfg(windows)]
#[test]
fn refresh_then_cancel_still_invalidates_the_old_cached_path_for_another_view() {
    let fixture = Fixture::new();
    let root = fixture.dir("root");
    fixture.file("root/a", 1);
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    receive_final(&service, 1, 1);
    fixture.file("root/b", 38);
    retry(|| service.set_requests(1, 2, &request(&root)));
    retry(|| service.cancel_view(1));
    retry(|| service.set_requests(2, 1, &request(&root)));
    let result = receive_final(&service, 2, 1);
    assert_eq!((result.stats.files, result.stats.bytes), (2, 39));
    join(service);
}

#[cfg(windows)]
#[test]
fn long_deep_branch_hits_path_budget_explicitly_without_growing_handles() {
    let fixture = Fixture::new();
    let root = fixture.dir("long");
    let mut path = root.clone();
    for _ in 0..31 {
        path.push("x".repeat(240));
        std::fs::create_dir(&path).unwrap();
    }
    std::fs::write(path.join("not-followed"), b"owned").unwrap();
    let service = FolderStatsService::new(Arc::new(|| {})).unwrap();
    retry(|| service.set_requests(1, 1, &request(&root)));
    let result = receive_final(&service, 1, 1);
    assert_eq!(result.stats.state, State::Partial);
    assert!(result.stats.issues.path_limited > 0);
    assert_eq!(result.stats.files, 0);
    join(service);
}

#[cfg(windows)]
#[test]
fn directory_handle_does_not_lock_out_a_user_rename() {
    let fixture = Fixture::new();
    let root = fixture.dir("old");
    fixture.file("old/file", 39);
    let mut reader = native::Reader::open(&root).unwrap();
    std::fs::rename(&root, fixture.root.join("renamed")).unwrap();
    let mut files = 0;
    while let Some(entry) = reader.next().unwrap() {
        if !entry.directory {
            files += 1;
            assert_eq!(entry.bytes, 39);
        }
    }
    assert_eq!(files, 1);
    drop(reader);
}
