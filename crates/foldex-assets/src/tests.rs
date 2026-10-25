use super::*;
use std::sync::mpsc;

fn key(name: &str, size: u16) -> AssetKey {
    AssetKey {
        path: std::env::temp_dir().join(name),
        size,
        thumbnail: false,
    }
}

fn request(key: AssetKey) -> Vec<AssetRequest> {
    vec![AssetRequest { key }]
}

fn retry(mut action: impl FnMut() -> io::Result<()>) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match action() {
            Ok(()) => return,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1));
            }
            other => panic!("mailbox retry failed: {other:?}"),
        }
    }
}

fn receive(loader: &AssetLoader) -> AssetEvent {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(event) = loader.try_recv() {
            return event;
        }
        assert!(Instant::now() < deadline, "asset event deadline");
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready() {
        assert!(Instant::now() < deadline, "asset state deadline");
        thread::sleep(Duration::from_millis(1));
    }
}

fn join(mut loader: AssetLoader) {
    loader.shutdown();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !loader.is_worker_finished() {
        assert!(Instant::now() < deadline, "asset worker shutdown deadline");
        thread::sleep(Duration::from_millis(1));
    }
    loader.worker.take().unwrap().join().expect("asset worker");
}

struct Fake {
    count: Arc<AtomicUsize>,
    started: Option<mpsc::Sender<()>>,
    gate: Option<mpsc::Receiver<()>>,
}

impl Backend for Fake {
    fn load(&mut self, key: &AssetKey) -> Result<RawPixels, String> {
        let call = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some(started) = self.started.take() {
            started.send(()).expect("started listener");
        }
        if let Some(gate) = self.gate.take() {
            gate.recv_timeout(Duration::from_secs(3))
                .expect("release fake native call");
        }
        let size = usize::from(key.size);
        let mut rgba = vec![call as u8; size * size * 4];
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        Ok(RawPixels {
            size: [size, size],
            rgba,
        })
    }
}

fn fake() -> (AssetLoader, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let backend_count = Arc::clone(&count);
    let loader = AssetLoader::spawn(Arc::new(|| {}), move || Fake {
        count: backend_count,
        started: None,
        gate: None,
    })
    .expect("fake loader");
    (loader, count)
}

fn gated() -> (AssetLoader, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (started_tx, started_rx) = mpsc::channel();
    let (gate_tx, gate_rx) = mpsc::channel();
    let loader = AssetLoader::spawn(Arc::new(|| {}), move || Fake {
        count: Arc::new(AtomicUsize::new(0)),
        started: Some(started_tx),
        gate: Some(gate_rx),
    })
    .expect("gated loader");
    (loader, started_rx, gate_tx)
}

#[test]
fn superseding_epoch_rejects_inflight_native_pixels_and_cache_entry() {
    let (loader, started, release) = gated();
    retry(|| loader.set_requests(1, 10, request(key("old.txt", 16))));
    started
        .recv_timeout(Duration::from_secs(3))
        .expect("native call started");
    retry(|| loader.set_requests(1, 11, request(key("new.txt", 16))));
    release.send(()).unwrap();
    let event = receive(&loader);
    assert_eq!(event.epoch, 11);
    assert_eq!(event.key.path, key("new.txt", 16).path);
    assert!(event.pixels.is_ok());
    assert!(loader.try_recv().is_none());
    assert_eq!(loader.metrics().cached_entries, 1);
    join(loader);
}

#[test]
fn rapid_viewport_changes_keep_an_exact_visible_inflight_key_and_publish_latest_epoch() {
    let (loader, started, release) = gated();
    let visible = key("still-visible.txt", 48);
    retry(|| loader.set_requests(1, 10, request(visible.clone())));
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    for epoch in 11..=40 {
        let mut requests = request(key("another-row.txt", 48));
        requests.extend(request(visible.clone()));
        retry(|| loader.set_requests_ref(1, epoch, &requests));
    }
    release.send(()).unwrap();
    let event = receive(&loader);
    assert_eq!(event.epoch, 40);
    assert_eq!(event.key, visible);
    assert_eq!(
        event.pixels.unwrap().rgba[0],
        1,
        "reuse the original native call"
    );
    join(loader);
}

#[test]
fn a_published_visible_key_follows_a_new_epoch_before_the_ui_receives_it() {
    let (loader, count) = fake();
    let visible = key("queued-visible.txt", 16);
    retry(|| loader.set_requests(1, 1, request(visible.clone())));
    wait_until(|| !loader.shared.state.lock().unwrap().events.is_empty());
    retry(|| loader.set_requests(1, 2, request(visible.clone())));
    let event = receive(&loader);
    assert_eq!(event.epoch, 2);
    assert_eq!(event.key, visible);
    assert_eq!(event.pixels.unwrap().rgba[0], 1);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    join(loader);
}

#[test]
fn invalidation_or_cancelled_view_lifetime_never_reuses_inflight_pixels() {
    for invalidate in [true, false] {
        let (loader, started, release) = gated();
        let visible = key("same-path-new-content.txt", 48);
        retry(|| loader.set_requests(1, 10, request(visible.clone())));
        started.recv_timeout(Duration::from_secs(3)).unwrap();
        if invalidate {
            retry(|| loader.invalidate_path(&visible.path));
        } else {
            retry(|| loader.cancel_view(1));
        }
        retry(|| loader.set_requests(1, 11, request(visible.clone())));
        release.send(()).unwrap();
        let event = receive(&loader);
        assert_eq!(event.epoch, 11);
        assert_eq!(event.key, visible);
        assert_eq!(
            event.pixels.unwrap().rgba[0],
            2,
            "reject invalidated/cancelled native pixels"
        );
        join(loader);
    }
}

#[test]
fn cancelled_view_publishes_nothing_and_retires_buffers_on_shutdown() {
    let (loader, started, release) = gated();
    retry(|| loader.set_requests(1, 1, request(key("cancel.txt", 256))));
    started.recv_timeout(Duration::from_secs(3)).unwrap();
    retry(|| loader.cancel_view(1));
    release.send(()).unwrap();
    let shared = Arc::clone(&loader.shared);
    join(loader);
    assert!(shared.state.lock().unwrap().events.is_empty());
    assert_eq!(shared.budget.used.load(Ordering::SeqCst), 0);
}

#[test]
fn request_count_views_paths_sizes_and_mailbox_are_bounded() {
    let (loader, _) = fake();
    assert_eq!(
        loader
            .set_requests(
                1,
                0,
                vec![
                    AssetRequest {
                        key: key("many", 16)
                    };
                    129
                ]
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    for size in [0, 257, u16::MAX] {
        assert!(
            loader
                .set_requests(1, 0, request(key("bad", size)))
                .is_err()
        );
    }
    let relative = AssetKey {
        path: PathBuf::from("relative"),
        size: 16,
        thumbnail: false,
    };
    assert!(loader.set_requests(1, 0, request(relative)).is_err());
    let too_many_path_bytes: Vec<_> = (0..100)
        .map(|index| AssetRequest {
            key: key(&format!("{}-{index}", "x".repeat(4000)), 16),
        })
        .collect();
    assert_eq!(
        loader
            .set_requests(1, 0, too_many_path_bytes)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(
        loader
            .set_requests(1, 0, request(key("invalid\0.txt", 16)))
            .is_err()
    );
    for view in 1..=MAX_VIEWS as u64 {
        let name = format!("view-{view}");
        retry(|| loader.set_requests(view, 0, request(key(&name, 16))));
    }
    assert_eq!(loader.shared.state.lock().unwrap().views.len(), MAX_VIEWS);
    let overflow_view = MAX_VIEWS as u64 + 1;
    assert_eq!(
        loader
            .set_requests(overflow_view, 0, request(key("overflow", 16)))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    {
        let _guard = loader.shared.state.lock().unwrap();
        assert_eq!(
            loader.set_requests(1, 2, Vec::new()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(loader.try_recv().is_none());
    }
    let released_view = MAX_VIEWS as u64;
    retry(|| loader.cancel_view(released_view));
    assert_eq!(
        loader.shared.state.lock().unwrap().views.len(),
        MAX_VIEWS - 1
    );
    retry(|| loader.set_requests(overflow_view, 0, request(key("overflow", 16))));
    {
        let state = loader.shared.state.lock().unwrap();
        assert_eq!(state.views.len(), MAX_VIEWS);
        assert!(state.views.iter().any(|view| view.view == overflow_view));
        assert!(!state.views.iter().any(|view| view.view == released_view));
    }
    join(loader);
}

#[test]
fn duplicate_requests_and_identical_frames_do_not_restart_native_work() {
    let (loader, count) = fake();
    let one = key("deduplicated", 16);
    retry(|| loader.set_requests(1, 1, vec![AssetRequest { key: one.clone() }; 128]));
    let event = receive(&loader);
    assert_eq!(event.pixels.unwrap().size, [16, 16]);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    retry(|| loader.set_requests(1, 1, request(one.clone())));
    assert_eq!(loader.shared.state.lock().unwrap().views[0].next, 1);
    assert!(loader.try_recv().is_none());
    join(loader);
}

#[test]
fn borrowed_viewport_retries_preserve_cached_keys_and_skip_unchanged_generations() {
    let (loader, count) = fake();
    let requests = request(key("borrowed-set", 16));
    retry(|| loader.set_requests_ref(1, 1, &requests));
    drop(receive(&loader));
    let sequence = loader.shared.state.lock().unwrap().sequence;
    for _ in 0..100 {
        retry(|| loader.set_requests_ref(1, 1, &requests));
    }
    assert_eq!(loader.shared.state.lock().unwrap().sequence, sequence);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    {
        let _guard = loader.shared.state.lock().unwrap();
        assert_eq!(
            loader.set_requests_ref(1, 2, &requests).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }
    join(loader);
}

#[test]
fn invalidating_same_epoch_path_reloads_every_cached_size() {
    let (loader, count) = fake();
    let first = key("modified.txt", 16);
    let second = key("modified.txt", 24);
    retry(|| {
        loader.set_requests(
            1,
            1,
            vec![
                AssetRequest { key: first.clone() },
                AssetRequest {
                    key: second.clone(),
                },
            ],
        )
    });
    let first_image = receive(&loader).pixels.unwrap();
    let second_image = receive(&loader).pixels.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 2);
    retry(|| loader.invalidate_key(&first));
    let next_first = receive(&loader).pixels.unwrap();
    let next_second = receive(&loader).pixels.unwrap();
    assert!(next_first.rgba[0] >= 3 && next_second.rgba[0] >= 3);
    assert_ne!(first_image.rgba[0], next_first.rgba[0]);
    assert_ne!(second_image.rgba[0], next_second.rgba[0]);
    assert_eq!(count.load(Ordering::SeqCst), 4);
    join(loader);
}

#[test]
fn pixel_budget_follows_caller_arcs_and_returns_on_last_drop() {
    let budget = Arc::new(PixelBudget {
        used: AtomicUsize::new(0),
        limit: CPU_PIXEL_BUDGET,
        worker_thread: OnceLock::new(),
    });
    let raw = || RawPixels {
        size: [256, 256],
        rgba: vec![0; 256 * 256 * 4],
    };
    let mut held = Vec::new();
    for _ in 0..32 {
        held.push(raw().finish(&budget).unwrap());
    }
    assert_eq!(budget.used.load(Ordering::SeqCst), CPU_PIXEL_BUDGET);
    assert!(raw().finish(&budget).is_err());
    held.pop();
    held.push(raw().finish(&budget).unwrap());
    drop(held);
    assert_eq!(budget.used.load(Ordering::SeqCst), 0);
    for size in [[0, 1], [257, 1], [16, 0]] {
        assert!(
            RawPixels {
                size,
                rgba: vec![0; 4]
            }
            .finish(&budget)
            .is_err()
        );
    }
    let mut oversized_allocation = Vec::with_capacity(MAX_PIXEL_BYTES + 1);
    oversized_allocation.extend_from_slice(&[0; 4]);
    assert!(
        RawPixels {
            size: [1, 1],
            rgba: oversized_allocation,
        }
        .finish(&budget)
        .is_err()
    );
    assert_eq!(budget.used.load(Ordering::SeqCst), 0);
}

#[test]
fn cache_lru_expiry_releases_old_path_generations_without_new_requests() {
    let budget = Arc::new(PixelBudget {
        used: AtomicUsize::new(0),
        limit: CPU_PIXEL_BUDGET,
        worker_thread: OnceLock::new(),
    });
    let image = RawPixels {
        size: [1, 1],
        rgba: vec![0; 4],
    }
    .finish(&budget)
    .unwrap();
    let mut cache = Cache::default();
    cache.insert(key("expired", 1), Ok(image));
    cache.entries[0].expires = Instant::now() - Duration::from_millis(1);
    cache.expire();
    assert!(cache.entries.is_empty());
    assert_eq!(cache.path_bytes, 0);
    assert_eq!(budget.used.load(Ordering::SeqCst), 0);
}

#[test]
fn pixel_pressure_preserves_negative_cache_entries() {
    let budget = Arc::new(PixelBudget {
        used: AtomicUsize::new(0),
        limit: CPU_PIXEL_BUDGET,
        worker_thread: OnceLock::new(),
    });
    let image = RawPixels {
        size: [1, 1],
        rgba: vec![0; 4],
    }
    .finish(&budget)
    .unwrap();
    let failed_key = key("negative-pixel-pressure", 1);
    let mut cache = Cache::default();
    cache.insert(failed_key.clone(), Err("provider unavailable".to_owned()));
    cache.insert(key("held-pixel-pressure", 1), Ok(Arc::clone(&image)));
    cache.make_room(&budget, CPU_PIXEL_BUDGET);
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(cache.path_bytes, key_weight(&failed_key));
    assert_eq!(
        cache.get(&failed_key).unwrap().unwrap_err(),
        "provider unavailable"
    );
    assert_eq!(budget.used.load(Ordering::SeqCst), 4);
    drop(image);
    assert_eq!(budget.used.load(Ordering::SeqCst), 0);
}

#[test]
fn idle_worker_shutdown_wakes_without_waiting_for_cache_expiry() {
    for _ in 0..16 {
        let (loader, _) = fake();
        retry(|| loader.set_requests(1, 1, request(key("idle-shutdown", 16))));
        drop(receive(&loader));
        join(loader);
    }
}

#[test]
fn queued_events_and_total_cpu_bytes_stay_bounded_when_ui_does_not_poll() {
    let (loader, count) = fake();
    let first: Vec<_> = (0..128)
        .map(|i| AssetRequest {
            key: key(&format!("view1-{i}"), 256),
        })
        .collect();
    let second: Vec<_> = (0..128)
        .map(|i| AssetRequest {
            key: key(&format!("view2-{i}"), 256),
        })
        .collect();
    retry(|| loader.set_requests(1, 1, first.clone()));
    retry(|| loader.set_requests(2, 1, second.clone()));
    wait_until(|| loader.metrics().backpressure_waits > 0);
    assert_eq!(loader.shared.state.lock().unwrap().events.len(), 32);
    assert_eq!(count.load(Ordering::SeqCst), 32);
    assert_eq!(loader.metrics().cpu_pixel_bytes, CPU_PIXEL_BUDGET);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..256 {
        let event = receive(&loader);
        assert!(seen.insert((event.view, event.key)));
        assert!(
            event.pixels.is_ok(),
            "capacity must not become a cached error"
        );
        assert!(loader.metrics().cpu_pixel_bytes <= CPU_PIXEL_BUDGET);
        drop(event.pixels);
    }
    assert_eq!(count.load(Ordering::SeqCst), 256);
    let shared = Arc::clone(&loader.shared);
    join(loader);
    assert_eq!(shared.budget.used.load(Ordering::SeqCst), 0);
}

#[test]
fn full_event_queue_keeps_every_result_and_coalesces_publication_wakes() {
    let (loader, count) = fake();
    for view in 1..=3 {
        let requests: Vec<_> = (0..MAX_REQUESTS_PER_VIEW)
            .map(|index| AssetRequest {
                key: key(&format!("queue-{view}-{index}"), 16),
            })
            .collect();
        retry(|| loader.set_requests(view, 1, requests.clone()));
    }
    wait_until(|| loader.shared.state.lock().unwrap().events.len() == MAX_EVENTS);
    assert_eq!(loader.metrics().ui_wakeups, 1);
    assert_eq!(count.load(Ordering::SeqCst), MAX_EVENTS);
    let mut seen = std::collections::HashSet::new();
    for _ in 0..3 * MAX_REQUESTS_PER_VIEW {
        let event = receive(&loader);
        assert!(seen.insert((event.view, event.key)));
        assert!(event.pixels.is_ok());
    }
    assert_eq!(seen.len(), 3 * MAX_REQUESTS_PER_VIEW);
    assert_eq!(count.load(Ordering::SeqCst), seen.len());
    join(loader);
}

#[test]
fn dropping_caller_pixels_resumes_capacity_wait_with_latest_epoch() {
    let (loader, count) = fake();
    let mut held: Vec<_> = (0..32)
        .map(|_| {
            RawPixels {
                size: [256, 256],
                rgba: vec![0; MAX_PIXEL_BYTES],
            }
            .finish(&loader.shared.budget)
            .unwrap()
        })
        .collect();
    retry(|| loader.set_requests(1, 1, request(key("budget-old", 256))));
    wait_until(|| loader.metrics().backpressure_waits > 0);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    retry(|| loader.set_requests(1, 2, request(key("budget-current", 256))));
    drop(held.pop());
    let event = receive(&loader);
    assert_eq!(event.epoch, 2);
    assert_eq!(event.key, key("budget-current", 256));
    assert!(event.pixels.is_ok());
    assert_eq!(count.load(Ordering::SeqCst), 1);
    drop(event);
    drop(held);
    join(loader);
}

#[test]
fn cancelled_capacity_wait_and_shutdown_do_not_require_caller_pixel_release() {
    let (loader, count) = fake();
    let budget = Arc::clone(&loader.shared.budget);
    let held: Vec<_> = (0..32)
        .map(|_| {
            RawPixels {
                size: [256, 256],
                rgba: vec![0; MAX_PIXEL_BYTES],
            }
            .finish(&budget)
            .unwrap()
        })
        .collect();
    retry(|| loader.set_requests(1, 1, request(key("budget-cancelled", 256))));
    wait_until(|| loader.metrics().backpressure_waits > 0);
    retry(|| loader.cancel_view(1));
    join(loader);
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert_eq!(budget.used.load(Ordering::SeqCst), CPU_PIXEL_BUDGET);
    drop(held);
    assert_eq!(budget.used.load(Ordering::SeqCst), 0);
}

#[test]
fn shared_visible_key_uses_one_native_load_and_one_pixel_lease() {
    let (loader, count) = fake();
    let shared_key = key("shared-view-asset", 16);
    retry(|| loader.set_requests(1, 1, request(shared_key.clone())));
    retry(|| loader.set_requests(2, 1, request(shared_key.clone())));
    let first = receive(&loader).pixels.unwrap();
    let second = receive(&loader).pixels.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(loader.metrics().cache_hits, 1);
    assert_eq!(loader.metrics().cpu_pixel_bytes, 16 * 16 * 4);
    join(loader);
}

#[test]
fn contended_receive_rearms_a_coalesced_publication_wake() {
    let (loader, _) = fake();
    retry(|| loader.set_requests(1, 1, request(key("contended-wake", 16))));
    wait_until(|| loader.metrics().ui_wakeups == 1);
    {
        let _guard = loader.shared.state.lock().unwrap();
        assert!(loader.try_recv().is_none());
    }
    assert_eq!(loader.metrics().ui_wakeups, 2);
    assert!(receive(&loader).pixels.is_ok());
    join(loader);
}

#[test]
fn provider_failures_remain_cached_until_path_invalidation() {
    struct Failing;
    impl Backend for Failing {
        fn load(&mut self, _: &AssetKey) -> Result<RawPixels, String> {
            Err("provider unavailable".to_owned())
        }
    }
    let loader = AssetLoader::spawn(Arc::new(|| {}), || Failing).unwrap();
    let failed_key = key("provider-failure", 16);
    for view in 1..=2 {
        retry(|| loader.set_requests(view, 1, request(failed_key.clone())));
        assert_eq!(receive(&loader).pixels.unwrap_err(), "provider unavailable");
    }
    assert_eq!(loader.metrics().native_loads, 1);
    assert_eq!(loader.metrics().cache_hits, 1);
    retry(|| loader.invalidate_key(&failed_key));
    for _ in 0..2 {
        assert!(receive(&loader).pixels.is_err());
    }
    assert_eq!(loader.metrics().native_loads, 2);
    join(loader);
}

#[cfg(windows)]
#[test]
fn native_worker_loads_generated_local_file_and_folder_assets_without_windows() {
    let _native_guard = super::windows::NATIVE_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let direct = super::windows::direct_thumbnail_for_test(&fixture.bitmap, 224)
        .expect("strict generated BMP thumbnail, without icon fallback");
    assert!(
        direct.rgba.chunks_exact(4).any(|pixel| {
            pixel[0] > 180 && (60..140).contains(&pixel[1]) && pixel[2] < 80 && pixel[3] > 200
        }),
        "generated color absent from direct thumbnail ({:?})",
        direct.size
    );
    let loader = AssetLoader::with_options(
        AssetOptions {
            extract_local_thumbnails: true,
        },
        Arc::new(|| {}),
    )
    .unwrap();
    let keys = [
        AssetKey {
            path: fixture.text.clone(),
            size: 42,
            thumbnail: false,
        },
        AssetKey {
            path: fixture.root.clone(),
            size: 132,
            thumbnail: false,
        },
        AssetKey {
            path: fixture.bitmap.clone(),
            size: 224,
            thumbnail: true,
        },
        AssetKey {
            path: fixture.root.clone(),
            size: 224,
            thumbnail: true,
        },
    ];
    retry(|| {
        loader.set_requests(
            1,
            1,
            keys.iter()
                .cloned()
                .map(|key| AssetRequest { key })
                .collect(),
        )
    });
    let events: Vec<_> = keys.iter().map(|_| receive(&loader)).collect();
    join(loader);
    for event in events {
        let image = event.pixels.expect("native shell pixels");
        assert!(image.size[0] > 0 && image.size[0] <= usize::from(event.key.size));
        assert!(image.size[1] > 0 && image.size[1] <= usize::from(event.key.size));
        assert_eq!(image.rgba.len(), image.size[0] * image.size[1] * 4);
        assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
        if event.key.path == fixture.bitmap {
            // A successful generic file icon also has pixels, so assert the
            // generated bitmap's colors to prove actual extraction.
            // The shell may pad a thumbnail to a square; its source color is
            // the invariant, while THUMBNAILONLY above rejects icon fallback.
            assert!(image.rgba.chunks_exact(4).any(|pixel| {
                pixel[0] > 180 && (60..140).contains(&pixel[1]) && pixel[2] < 80 && pixel[3] > 200
            }));
        }
    }
    drop(fixture);
}

#[cfg(windows)]
struct Fixture {
    base: PathBuf,
    root: PathBuf,
    text: PathBuf,
    bitmap: PathBuf,
}

#[cfg(windows)]
impl Fixture {
    fn new() -> Self {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/assets-test-fixtures");
        std::fs::create_dir_all(&base).unwrap();
        let base = base.canonicalize().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = base.join(format!("owned-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let text = root.join("Generated Notes.txt");
        std::fs::write(&text, "Generated native asset fixture.\n").unwrap();
        let bitmap = root.join("Generated Image.bmp");
        let (width, height) = (64u32, 32u32);
        let pixel_bytes = width * height * 4;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&(54 + pixel_bytes).to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&54u32.to_le_bytes());
        bytes.extend_from_slice(&40u32.to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&pixel_bytes.to_le_bytes());
        bytes.extend_from_slice(&[0; 16]);
        for _ in 0..width * height {
            bytes.extend_from_slice(&[32, 96, 224, 255]);
        }
        std::fs::write(&bitmap, bytes).unwrap();
        Self {
            base,
            root,
            text,
            bitmap,
        }
    }
}

#[cfg(windows)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let target = self.root.canonicalize().unwrap();
        assert!(target.starts_with(&self.base) && target != self.base);
        assert!(
            target
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("owned-")
        );
        // Only explicitly created files/directories; never recursively remove.
        std::fs::remove_file(&self.text).unwrap();
        std::fs::remove_file(&self.bitmap).unwrap();
        std::fs::remove_dir(&target).unwrap();
    }
}
