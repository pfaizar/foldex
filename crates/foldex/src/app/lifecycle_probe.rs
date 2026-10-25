//! Opt-in native lifecycle regression. All files and windows belong to the probe;
//! the normal session and the Native tray/hotkey/clipboard thread are never used.
use super::*;
use crate::launcher::native;
use std::{ffi::OsString, io, sync::Mutex};

const CYCLES: usize = 4;
const DEADLINE: Duration = Duration::from_secs(15);

#[derive(Default)]
struct Outcome {
    checks: Vec<serde_json::Value>,
    errors: Vec<String>,
    finished: bool,
    logic_calls: u64,
    hidden_logic_calls: u64,
    ui_calls: u64,
}

#[derive(Clone, Copy)]
enum Phase {
    Warmup,
    Minimize,
    WaitOpen(Instant, usize),
    Opening(usize),
    Closing(usize),
    WaitRestore(Instant),
    Restore,
    Quit,
    Finished,
}

struct Probe {
    app: Foldex,
    root: Arc<winit::window::Window>,
    outcome: Arc<Mutex<Outcome>>,
    began: Instant,
    phase: Phase,
    title: String,
    original_path: PathBuf,
    original_theme: Theme,
    launcher_window: Option<isize>,
    minimized_ui_calls: u64,
    restored_ui_calls: u64,
    visible_minimized_root: bool,
}

pub(crate) fn run(args: &[OsString]) -> eframe::Result {
    let result = run_inner(args);
    result.map_err(|error| eframe::Error::AppCreation(Box::new(error)))
}

fn run_inner(args: &[OsString]) -> io::Result<()> {
    let output = args
        .iter()
        .position(|arg| arg == "--probe-lifecycle")
        .and_then(|at| args.get(at + 1))
        .filter(|path| !path.to_string_lossy().starts_with("--"))
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("Use --probe-lifecycle <new-output-directory>"))?;
    let output = std::path::absolute(output)?;
    // Refuse existing directories so no prior report or user state is replaced.
    std::fs::create_dir(&output)?;
    crate::install_panic_log(output.join("foldex-crash.log"));
    let fixture = output.join("fixture");
    let state = output.join("state");
    std::fs::create_dir(&fixture)?;
    std::fs::create_dir(&state)?;
    std::fs::write(fixture.join("Lifecycle.txt"), b"owned lifecycle fixture\n")?;

    let outcome = Arc::new(Mutex::new(Outcome::default()));
    let shared = outcome.clone();
    let visible_minimized_root = args.iter().any(|arg| arg == "--visible-minimized-root");
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("Foldex Lifecycle Probe · {}", std::process::id()))
            .with_inner_size([960.0, 640.0])
            .with_active(false),
        persist_window: false,
        ..Default::default()
    };
    #[cfg(feature = "renderer-wgpu")]
    {
        options.renderer = eframe::Renderer::Wgpu;
    }
    #[cfg(feature = "renderer-glow")]
    {
        options.renderer = eframe::Renderer::Glow;
    }
    let runtime = eframe::run_native(
        "Foldex Lifecycle Probe",
        options,
        Box::new(move |cc| {
            let root = cc
                .winit_window()
                .cloned()
                .ok_or_else(|| io::Error::other("native probe root missing"))?;
            let mut theme = Theme::default();
            theme.set_color_mode(foldex_ui::ColorMode::Dark, None);
            theme.apply(&cc.egui_ctx);
            let mut session = Session::default();
            session.launcher.clipboard_history = false;
            session.launcher.hotkey.clear();
            session.launcher.file_roots = vec![fixture.clone()];
            let app = Foldex::new(
                cc,
                theme.clone(),
                Some(fixture.clone()),
                state.join("theme.toml"),
                state.join("session.json"),
                session,
                Instant::now(),
                None,
                crate::launcher::Runtime {
                    isolate: true,
                    auxiliary: false,
                    tray_only: false,
                    open_launcher: false,
                    open_settings: false,
                    channel: None,
                    secondary: None,
                },
            )?;
            Ok(Box::new(Probe {
                original_path: fixture,
                original_theme: theme,
                app,
                root,
                outcome: shared,
                began: Instant::now(),
                phase: Phase::Warmup,
                title: format!("Foldex Launcher · {}", std::process::id()),
                launcher_window: None,
                minimized_ui_calls: 0,
                restored_ui_calls: 0,
                visible_minimized_root,
            }))
        }),
    );
    let outcome = outcome.lock().unwrap_or_else(|error| error.into_inner());
    let passed = runtime.is_ok() && outcome.finished && outcome.errors.is_empty();
    let report = serde_json::json!({
        "schema": 1, "workload": "native-hidden-root-launcher-lifecycle",
        "passed": passed, "clean_exit": runtime.is_ok() && outcome.finished,
        "cycles": CYCLES, "checks": outcome.checks, "errors": outcome.errors,
        "root_before_launcher": if visible_minimized_root { "visible-minimized" } else { "hidden-minimized" },
        "runtime_error": runtime.as_ref().err().map(ToString::to_string),
        "logic_calls": outcome.logic_calls, "hidden_logic_calls": outcome.hidden_logic_calls,
        "root_ui_calls": outcome.ui_calls,
        "isolated": { "tray": false, "global_hotkey": false, "clipboard": false, "normal_session": false },
    });
    use std::io::Write;
    std::fs::File::create_new(output.join("report.json"))?
        .write_all(&serde_json::to_vec_pretty(&report)?)?;
    if passed {
        Ok(())
    } else {
        Err(io::Error::other(
            "Lifecycle probe failed; inspect report.json and foldex-crash.log",
        ))
    }
}

impl Probe {
    fn check(&mut self, name: &str, passed: bool) -> bool {
        let mut outcome = self
            .outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        outcome
            .checks
            .push(serde_json::json!({"check": name, "passed": passed}));
        if !passed {
            outcome.errors.push(name.to_owned());
        }
        passed
    }

    fn signal(&self, event: u32) {
        // Queue exactly the message-thread event without registering any native
        // resources or posting input into the real user's desktop.
        self.app
            .launcher
            .native
            .state
            .events
            .fetch_or(event, std::sync::atomic::Ordering::Release);
    }

    fn finish(&mut self) {
        self.phase = Phase::Finished;
        self.outcome
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .finished = true;
    }

    fn fail(&mut self, ctx: &egui::Context, reason: &str) {
        self.check(reason, false);
        self.app.close_launcher(false);
        self.app.allow_close = true;
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Close);
        ctx.request_repaint_of(egui::ViewportId::ROOT);
        self.finish();
    }

    fn step(&mut self, ctx: &egui::Context) {
        if matches!(self.phase, Phase::Finished) {
            return;
        }
        if self.began.elapsed() >= DEADLINE {
            self.fail(ctx, "lifecycle completed within deadline");
            return;
        }
        let now = Instant::now();
        match self.phase {
            Phase::Warmup if self.app.frame_count >= 2 && !self.app.pane().loading => {
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::ROOT,
                    egui::ViewportCommand::Minimized(true),
                );
                self.phase = Phase::Minimize;
            }
            Phase::Minimize if ctx.input(|input| input.viewport().minimized == Some(true)) => {
                self.minimized_ui_calls = self.outcome.lock().unwrap().ui_calls;
                self.app.background_allowed = true;
                self.app.launcher.settings.background = true;
                self.app
                    .launcher
                    .native
                    .state
                    .tray_ready
                    .store(true, std::sync::atomic::Ordering::Release);
                if self.visible_minimized_root {
                    if !self.check(
                        "minimized explorer remains logically and natively visible",
                        self.app.explorer_visible
                            && self.root.is_visible() == Some(true)
                            && !self.app.launcher_initialized
                            && self.app.close_requested.is_none(),
                    ) {
                        self.fail(ctx, "visible minimized setup hid the explorer");
                        return;
                    }
                } else {
                    self.app.close_tab(self.app.active);
                    if !self.check(
                        "last tab hides explorer and preserves root/session",
                        !self.app.explorer_visible
                            && self.app.close_requested.is_none()
                            && !self.app.allow_close
                            && self.app.tabs.len() == 1
                            && self.app.pane().path == self.original_path,
                    ) {
                        self.fail(ctx, "last tab unexpectedly requested process shutdown");
                        return;
                    }
                }
                self.phase = Phase::WaitOpen(now + Duration::from_millis(200), 0);
            }
            Phase::WaitOpen(due, cycle) if now >= due => {
                if cycle == 0 {
                    let output = self.outcome.lock().unwrap();
                    let skipped = output.ui_calls == self.minimized_ui_calls
                        && output.hidden_logic_calls >= 2;
                    drop(output);
                    if !self.check("minimized root services logic without root UI", skipped) {
                        self.fail(ctx, "minimized root did not exercise logic-only path");
                        return;
                    }
                }
                self.signal(native::TOGGLE);
                self.phase = Phase::Opening(cycle);
            }
            Phase::Opening(cycle)
                if self.app.launcher_open
                    && self.app.launcher_positioned
                    && native::launcher_visible(&self.title) == Some(true) =>
            {
                let window = native::launcher_window(&self.title);
                let retained = self
                    .launcher_window
                    .is_none_or(|previous| window == Some(previous));
                self.launcher_window = window;
                if !self.check(
                    &format!("launcher cycle {} opens on minimized root", cycle + 1),
                    self.app.explorer_visible == self.visible_minimized_root
                        && self.root.is_minimized() == Some(true)
                        && window.is_some()
                        && retained
                        && self.app.close_requested.is_none(),
                ) {
                    self.fail(ctx, "launcher lifecycle changed its owner or exited");
                    return;
                }
                self.signal(native::TOGGLE);
                self.phase = Phase::Closing(cycle);
            }
            Phase::Closing(cycle)
                if !self.app.launcher_open
                    && native::launcher_visible(&self.title) == Some(false) =>
            {
                self.check(
                    &format!(
                        "launcher cycle {} closes while process stays resident",
                        cycle + 1
                    ),
                    self.app.explorer_visible == self.visible_minimized_root
                        && self.app.close_requested.is_none(),
                );
                self.phase = if cycle + 1 < CYCLES {
                    Phase::WaitOpen(now + Duration::from_millis(120), cycle + 1)
                } else {
                    Phase::WaitRestore(now + Duration::from_millis(120))
                };
            }
            Phase::WaitRestore(due) if now >= due => {
                self.restored_ui_calls = self.outcome.lock().unwrap().ui_calls;
                self.signal(native::EXPLORER);
                self.phase = Phase::Restore;
            }
            Phase::Restore
                if self.app.explorer_visible
                    && self.root.is_visible() == Some(true)
                    && self.root.is_minimized() == Some(false)
                    && self.outcome.lock().unwrap().ui_calls > self.restored_ui_calls =>
            {
                self.check(
                    "explorer restores its workspace and theme",
                    self.app.tabs.len() == 1
                        && self.app.pane().path == self.original_path
                        && self.app.theme.background == self.original_theme.background
                        && self.app.close_requested.is_none(),
                );
                self.app.hide_explorer(ctx);
                ctx.send_viewport_cmd_to(
                    egui::ViewportId::ROOT,
                    egui::ViewportCommand::Minimized(true),
                );
                self.phase = Phase::Quit;
                self.signal(native::EXIT);
            }
            Phase::Quit if self.app.allow_close => {
                self.check(
                    "explicit Quit saves before closing hidden root",
                    self.app.close_requested.is_some()
                        && !self.app.session_dirty
                        && self.app.saved_revision >= self.app.session_revision
                        && self.app.session_revision > 0,
                );
                self.finish();
            }
            _ => {}
        }
        if !matches!(self.phase, Phase::Finished) {
            // A timer wake is essential here: Windows sends no RedrawRequested
            // for the hidden root. Its cold child must be created from new_events.
            ctx.request_repaint_after(Duration::from_millis(60));
        }
    }
}

impl eframe::App for Probe {
    fn run_ui_when_hidden(&self) -> bool {
        self.app.run_ui_when_hidden()
    }
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, input);
    }
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        {
            let mut outcome = self.outcome.lock().unwrap();
            outcome.logic_calls += 1;
            if !self.app.launcher_open
                && ctx.input(|input| {
                    let root = input.viewport();
                    root.minimized == Some(true) || root.occluded == Some(true)
                })
            {
                outcome.hidden_logic_calls += 1;
            }
        }
        self.step(ctx);
        self.app.logic(ctx, frame);
        if matches!(self.phase, Phase::Quit) && self.app.allow_close {
            self.step(ctx);
        }
    }
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.outcome.lock().unwrap().ui_calls += 1;
        self.app.ui(ui, frame);
        if matches!(self.phase, Phase::Quit) && self.app.allow_close {
            self.step(ui.ctx());
        }
    }
}
