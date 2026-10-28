//! Opt-in measurements. Search mode is headless; UI mode uses isolated state,
//! records startup/idle metrics, and closes automatically without user preferences.
use crate::search::{SearchEvent, SearchHub};
use foldex_core::Query;
#[cfg(feature = "renderer-wgpu")]
mod gpu;
pub(crate) mod keyboard;
pub(crate) mod monitor;
pub(crate) use keyboard::KeyboardUiRun;
use std::{
    error::Error,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn value(args: &[OsString], flag: &str) -> Option<OsString> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}
pub fn gpu_context(args: &[OsString], launched: Instant) -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "renderer-wgpu")]
    {
        gpu::run(args, launched)
    }
    #[cfg(not(feature = "renderer-wgpu"))]
    {
        let _ = (args, launched);
        Err("GPU context measurement requires the WGPU build".into())
    }
}
pub(super) struct Fixture {
    path: PathBuf,
    base: PathBuf,
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

pub(super) struct UiRun {
    pub scope: PathBuf,
    pub state_dir: PathBuf,
    pub output: PathBuf,
    pub expires: Instant,
    pub first_directory_ms: Option<f64>,
    pub done: bool,
    pub no_shell_assets: bool,
    pub no_metrics: bool,
    pub monitor: Option<monitor::Monitor>,
    pub stress: Option<StressRun>,
    pub full_search: Option<SearchUiRun>,
    pub navigation: Option<NavigationUiRun>,
    pub resize: Option<ResizeUiRun>,
    pub launcher: Option<LauncherUiRun>,
    pub fixes: Option<FixesUiRun>,
    pub keyboard: Option<KeyboardUiRun>,
    pub pre_framework_ms: f64,
    pub framework_ready_ms: Option<f64>,
    pub app_ready_ms: Option<f64>,
}
pub(super) fn prepare_ui(
    args: &[OsString],
    launched: Instant,
) -> Result<Option<(UiRun, Fixture)>, Box<dyn Error>> {
    let keyboard_mode = keyboard::validate_args(args)?;
    let requested_scope = value(args, "--benchmark-ui");
    if requested_scope.is_none() && !keyboard_mode {
        return Ok(None);
    }
    let scope = requested_scope.map(fs::canonicalize).transpose()?;
    if args.iter().any(|arg| arg == "--benchmark-refinement") {
        // This workload deliberately mutates generated fixture entries. Refuse
        // every other scope, including ordinary user folders passed by mistake.
        let fixture = fs::canonicalize(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../foldex-refinement-2026-10-23/fixture"),
        )?;
        if scope.as_ref() != Some(&fixture) {
            return Err("Refinement workload requires its generated workspace fixture".into());
        }
    }
    if args.iter().any(|arg| {
        arg == "--benchmark-launcher"
            || arg == "--benchmark-fixes"
            || arg == "--benchmark-refinement"
            || arg == "--benchmark-inline-rename"
    }) && !args.iter().any(|arg| arg == "--secondary-monitor")
    {
        return Err("Launcher benchmark requires --secondary-monitor".into());
    }
    if scope.as_ref().is_some_and(|scope| !scope.is_dir()) {
        return Err("UI benchmark requires a directory".into());
    }
    let seconds: u64 = value(args, "--benchmark-seconds")
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .unwrap_or(if keyboard_mode { 120 } else { 8 });
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/performance-fixtures");
    fs::create_dir_all(&base)?;
    let state_dir = base.join(format!(
        "owned-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&state_dir)?;
    let fixture = Fixture {
        path: state_dir.clone(),
        base,
    };
    let scope = if keyboard_mode {
        keyboard::create_fixture(&state_dir)?
    } else {
        scope.expect("Non-keyboard benchmark has a caller-provided scope")
    };
    if args.iter().any(|arg| arg == "--benchmark-launcher") {
        fs::write(
            state_dir.join("Menu fixture.txt"),
            "Generated Foldex native context menu fixture.\n",
        )?;
    }
    let output = value(args, "--benchmark-output")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if keyboard_mode {
                return Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                    "../../../foldex-keyboard-2026-10-23/keyboard-{}.metrics.json",
                    std::process::id()
                ));
            }
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
                "../../benchmarks/2026-10-21/ui-{}.metrics.json",
                std::process::id()
            ))
        });
    if keyboard_mode && let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(Some((
        UiRun {
            scope,
            state_dir,
            output,
            expires: launched
                + Duration::from_secs(seconds.clamp(2, if keyboard_mode { 420 } else { 120 })),
            first_directory_ms: None,
            done: false,
            no_shell_assets: args.iter().any(|arg| arg == "--benchmark-no-shell-assets"),
            no_metrics: args.iter().any(|arg| arg == "--benchmark-no-metrics"),
            monitor: None,
            stress: args
                .iter()
                .any(|arg| arg == "--benchmark-stress" || arg == "--benchmark-many-tabs")
                .then(|| StressRun {
                    many_tabs: args.iter().any(|arg| arg == "--benchmark-many-tabs"),
                    ..Default::default()
                }),
            full_search: args
                .iter()
                .any(|arg| arg == "--benchmark-full-search")
                .then(SearchUiRun::default),
            navigation: value(args, "--benchmark-navigation-list")
                .map(|path| -> Result<_, Box<dyn Error>> {
                    let bytes = fs::read(path)?;
                    if bytes.len() > 64 * 1024 {
                        return Err("Navigation list is too large".into());
                    }
                    let paths: Vec<PathBuf> = serde_json::from_slice(&bytes)?;
                    if paths.is_empty() || paths.len() > 128 {
                        return Err("Navigation list requires 1–128 folders".into());
                    }
                    Ok(NavigationUiRun {
                        paths,
                        ..Default::default()
                    })
                })
                .transpose()?,
            resize: args
                .iter()
                .any(|arg| arg == "--benchmark-resize")
                .then(ResizeUiRun::default),
            launcher: args
                .iter()
                .any(|arg| arg == "--benchmark-launcher")
                .then(|| LauncherUiRun {
                    phase: 80,
                    ..Default::default()
                }),
            fixes: args
                .iter()
                .any(|arg| {
                    arg == "--benchmark-fixes"
                        || arg == "--benchmark-refinement"
                        || arg == "--benchmark-inline-rename"
                })
                .then(|| FixesUiRun {
                    refinement: args.iter().any(|arg| arg == "--benchmark-refinement"),
                    phase: if args.iter().any(|arg| arg == "--benchmark-refinement") {
                        2000
                    } else if args.iter().any(|arg| arg == "--benchmark-inline-rename") {
                        6000
                    } else {
                        0
                    },
                    ..Default::default()
                }),
            keyboard: keyboard_mode.then(KeyboardUiRun::default),
            pre_framework_ms: 0.0,
            framework_ready_ms: None,
            app_ready_ms: None,
        },
        fixture,
    )))
}

/// Explicit, isolated visual scenarios for the October interaction fixes.
/// The application drives its own state; no OS mouse/keyboard input is sent.
#[derive(Default)]
pub(crate) struct FixesUiRun {
    pub refinement: bool,
    pub force_focus: bool,
    pub delay_ms: u64,
    pub last_phase: u32,
    pub phase_started: Option<Instant>,
    pub baseline: Option<crate::pane::columns::ColumnDiagnostics>,
    pub saved_search: Option<egui::Rect>,
    pub saved_expand: Option<egui::Rect>,
    pub shortcut_before: Option<String>,
    pub samples: Vec<serde_json::Value>,
    pub phase: u32,
    pub next: Option<Instant>,
    pub waiting_capture: bool,
    pub captures: Vec<PathBuf>,
    pub capture_result: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    pub checks: Vec<serde_json::Value>,
    pub errors: Vec<String>,
    pub finished: bool,
    pub compact: bool,
    pub pointer: Option<egui::Pos2>,
    pub input: Vec<egui::Event>,
}
impl FixesUiRun {
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"refinement":self.refinement,"phase":self.phase,"finished":self.finished,"captures":self.captures,"checks":self.checks,"samples":self.samples,"errors":self.errors,"presentation_measured":false,"global_input_sent":false})
    }
}

#[derive(Default)]
pub(crate) struct LauncherUiRun {
    pub phase: u32,
    pub next: Option<Instant>,
    pub toggles: u32,
    pub query: String,
    pub compact: bool,
    pub capture: Option<&'static str>,
    pub capture_sent: bool,
    pub captures: Vec<PathBuf>,
    pub capture_result: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    pub errors: Vec<String>,
    pub checks: Vec<serde_json::Value>,
    pub ui_ms: Vec<f64>,
    pub finished: bool,
    pub atlas_cycle: u8,
    pub atlas_generations: Vec<usize>,
}
impl LauncherUiRun {
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"phase":self.phase,"finished":self.finished,"toggles":self.toggles,"captures":self.captures,"errors":self.errors,"checks":self.checks,"ui_ms":self.ui_ms,"atlas_generations":self.atlas_generations,"presentation_measured":false})
    }
}

#[derive(Default)]
pub(crate) struct SearchUiRun {
    pub phase: u8,
    pub changed: Option<Instant>,
    pub last_frame: Option<Instant>,
    pub interval_ms: f64,
    pub poll_ms: f64,
    pub samples: Vec<serde_json::Value>,
}

#[derive(Default)]
pub(crate) struct NavigationUiRun {
    pub paths: Vec<PathBuf>,
    pub next: usize,
    pub started: Option<Instant>,
    pub finished: Option<Instant>,
    pub first_ms: Option<f64>,
    pub first_full_ms: Option<f64>,
    pub first_cached: bool,
    pub samples: Vec<serde_json::Value>,
}

#[derive(Default)]
pub(crate) struct ResizeUiRun {
    pub step: usize,
    pub origin: Option<[i32; 4]>,
    pub last_frame: Option<Instant>,
    pub samples: Vec<serde_json::Value>,
}

#[derive(Clone, serde::Serialize)]
pub(crate) struct LayoutProbe {
    pub pane: u64,
    pub row: usize,
    pub text: [f32; 2],
    pub icon: [f32; 4],
}
pub(crate) fn record_layout_probe(ctx: &egui::Context, probe: LayoutProbe) {
    ctx.data_mut(|data| {
        let probes = data
            .get_temp_mut_or_default::<Vec<LayoutProbe>>(egui::Id::new("benchmark-layout-probes"));
        if probes.len() < 64 {
            probes.push(probe);
        }
    });
}

pub(crate) fn window_scaling() -> Option<i32> {
    #[cfg(all(windows, feature = "renderer-wgpu"))]
    {
        Some(wgpu::hal::dx12::last_window_scaling())
    }
    #[cfg(not(all(windows, feature = "renderer-wgpu")))]
    {
        None
    }
}

pub(crate) fn graphics_timings() -> Option<[u64; 6]> {
    #[cfg(all(windows, feature = "renderer-wgpu"))]
    {
        Some(wgpu::hal::dx12::take_window_timings())
    }
    #[cfg(not(all(windows, feature = "renderer-wgpu")))]
    {
        None
    }
}

#[derive(Default)]
pub(crate) struct StressRun {
    pub raw_input_ms: f64,
    pub many_tabs: bool,
    pub actions: usize,
    pub max_tabs: usize,
    pub max_panes: usize,
    pub command_ms: Vec<f64>,
    pub frame_ms: Vec<f64>,
    pub intervals_ms: Vec<f64>,
    pub last_frame: Option<Instant>,
    pub finished: bool,
    pub frames: Vec<StressFrame>,
}

#[derive(serde::Serialize)]
pub(crate) struct StressFrame {
    pub at_ms: f64,
    pub interval_ms: f64,
    pub raw_input_ms: f64,
    pub graphics_ns: Option<[u64; 6]>,
    pub managed_bytes: usize,
    pub action: usize,
    pub tabs: usize,
    pub panes: usize,
    pub setup_ms: f64,
    pub assets_ms: f64,
    pub poll_ms: f64,
    pub chrome_ms: f64,
    pub workspace_ms: f64,
    pub overlays_ms: f64,
    pub ui_ms: f64,
    pub previous_renderer_ms: f64,
}
impl StressRun {
    pub fn report(&self) -> serde_json::Value {
        let summary = |values: &[f64]| {
            let mut sorted = values.to_vec();
            sorted.sort_unstable_by(f64::total_cmp);
            let percentile = |p: usize| {
                sorted
                    .get(sorted.len().saturating_sub(1) * p / 100)
                    .copied()
                    .unwrap_or(0.0)
            };
            serde_json::json!({"samples":sorted.len(),"p50":percentile(50),"p95":percentile(95),"p99":percentile(99),"max":sorted.last()})
        };
        serde_json::json!({"workload":if self.many_tabs{"many-tabs"}else{"split-churn"},"actions":self.actions,"max_tabs":self.max_tabs,"max_panes":self.max_panes,"finished":self.finished,
            "command_cpu_ms":summary(&self.command_ms),"ui_cpu_ms":summary(&self.frame_ms),"callback_interval_ms":summary(&self.intervals_ms),"frames":self.frames,"presentation_measured":false})
    }
}
pub fn search(args: &[OsString], launched: Instant) -> Result<(), Box<dyn Error>> {
    let scope =
        fs::canonicalize(value(args, "--benchmark-search").ok_or("missing benchmark scope")?)?;
    let seconds: u64 = value(args, "--benchmark-seconds")
        .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
        .unwrap_or(180);
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/performance-fixtures");
    fs::create_dir_all(&base)?;
    let path = base.join(format!(
        "owned-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    ));
    fs::create_dir(&path)?;
    let fixture = Fixture { path, base };
    let (sender, receiver) = mpsc::sync_channel(1);
    let hub = SearchHub::new(
        fixture.path.join("cache"),
        Arc::new(move || {
            let _ = sender.try_send(());
        }),
    )?;
    let client = hub.client();
    let query = Query {
        text: value(args, "--benchmark-query")
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into()),
        ..Query::default()
    };
    let request = client.request(scope.clone(), query, 0, true);
    let (mut first, mut entries, mut windows, mut final_status) = (None, 0u64, 0u64, None);
    let deadline = launched + Duration::from_secs(seconds.max(1));
    while Instant::now() < deadline {
        while let Some(event) = client.try_recv() {
            match event {
                SearchEvent::Failed {
                    request: id,
                    message,
                } if id == request => return Err(message.into()),
                SearchEvent::Progress {
                    request: id,
                    status,
                } if id == request => {
                    entries = entries.max(status.entries);
                }
                SearchEvent::Window(window) if window.request == request => {
                    windows += 1;
                    entries = entries.max(window.status.entries);
                    if !window.snapshot.entries.is_empty() && first.is_none() {
                        first = Some(launched.elapsed());
                    }
                    if !matches!(
                        window.status.state,
                        foldex_index::BuildState::Building | foldex_index::BuildState::Cancelled
                    ) && window.total.is_some()
                    {
                        final_status = Some(window.status.clone());
                    }
                }
                _ => {}
            }
        }
        if final_status.is_some() {
            break;
        }
        let _ = receiver.recv_timeout(
            Duration::from_millis(30).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    let scan_ms = launched.elapsed().as_secs_f64() * 1000.0;
    let mut queries = Vec::new();
    if final_status.is_some() {
        for text in ["file", "rs", "design", "file", "rs", "design"] {
            let started = Instant::now();
            let request = client.request(
                scope.clone(),
                Query {
                    text: text.into(),
                    ..Query::default()
                },
                0,
                false,
            );
            let deadline = started + Duration::from_secs(30);
            let mut received = None;
            while received.is_none() && Instant::now() < deadline {
                while let Some(event) = client.try_recv() {
                    match event {
                        SearchEvent::Failed {
                            request: id,
                            message,
                        } if id == request => return Err(message.into()),
                        SearchEvent::Window(window) if window.request == request => {
                            received = Some(
                                serde_json::json!({"query":text,"first_window_ms":started.elapsed().as_secs_f64()*1000.0,"rows":window.snapshot.entries.len(),"entries":window.status.entries}),
                            );
                            break;
                        }
                        _ => {}
                    }
                }
                if received.is_none() {
                    let _ = receiver.recv_timeout(Duration::from_millis(30));
                }
            }
            queries.push(received.ok_or("app search timed out")?);
        }
    }
    client.close();
    drop(client);
    let finish = Instant::now();
    hub.shutdown();
    let report = serde_json::json!({"schema":1,"workload":"app-search-worker","scope":scope,"entries":entries,"first_nonempty_window_ms":first.map(|elapsed|elapsed.as_secs_f64()*1000.0),"scan_ms":scan_ms,"windows":windows,"exhausted":final_status.is_some(),"complete":final_status.as_ref().is_some_and(|status|status.complete),"skipped":final_status.as_ref().map(|status|status.skipped),"queries":queries,"shutdown_ms":finish.elapsed().as_secs_f64()*1000.0});
    if let Some(path) = value(args, "--benchmark-output") {
        fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    println!("{report}");
    Ok(())
}
