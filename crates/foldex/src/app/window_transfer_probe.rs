//! Opt-in, finite native smoke probe. Only generated folders and app-owned
//! windows participate; launcher/clipboard/shell-history ownership is disabled.
use super::*;
use crate::{
    navigation::{Navigation, Route},
    workspace::ViewSettings,
};
use eframe::App;
use std::{ffi::OsString, io, path::Path, sync::Mutex};

const DEADLINE: Duration = Duration::from_secs(40);

#[derive(Default)]
struct Report {
    child: Option<serde_json::Value>,
    checks: Vec<(&'static str, bool)>,
    error: Option<String>,
    finished: bool,
}
enum Phase {
    Begin,
    Transfer,
    FailedActivation,
    FailedSpawn,
    Child,
    Finished,
}
struct Probe {
    app: Foldex,
    output: PathBuf,
    executable: PathBuf,
    source: u64,
    spare: u64,
    phase: Phase,
    began: Instant,
    report: Arc<Mutex<Report>>,
}

pub(crate) fn run(args: &[OsString]) -> eframe::Result {
    run_inner(args).map_err(|error| eframe::Error::AppCreation(Box::new(error)))
}
fn run_inner(args: &[OsString]) -> io::Result<()> {
    let output = args
        .iter()
        .position(|arg| arg == "--probe-window-transfer")
        .and_then(|at| args.get(at + 1))
        .filter(|arg| !arg.to_string_lossy().starts_with("--"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::other("Use --probe-window-transfer <new-output-directory-outside-project>")
        })?;
    let output = std::path::absolute(output)?;
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("Workspace root missing"))?;
    if output.starts_with(workspace) {
        return Err(io::Error::other(
            "The transfer probe output must be outside the project",
        ));
    }
    fs_create_fixture(&output)?;
    crate::install_panic_log(output.join("foldex-crash.log"));
    let report = Arc::new(Mutex::new(Report::default()));
    let shared = report.clone();
    let directory = output.clone();
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!(
                "Foldex Tab Transfer Probe · {}",
                std::process::id()
            ))
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
        "Foldex Tab Transfer Probe",
        options,
        Box::new(move |cc| {
            let mut session = Session::default();
            session.launcher.background = false;
            session.launcher.clipboard_history = false;
            session.launcher.hotkey.clear();
            session.launcher.file_roots = vec![directory.join("fixture")];
            let mut app = Foldex::new(
                cc,
                Theme::default(),
                Some("foldex:home".into()),
                directory.join("source-state/theme.toml"),
                directory.join("source-state/session.json"),
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
            let source = app.tabs[0].left.id;
            // Keep a legacy two-sided tab to exercise migration to two native
            // workspace groups, including a virtual Home and the active right side.
            let mut navigation = Navigation::new(Route::Folder(directory.join("fixture")));
            navigation.visit(Route::Home);
            app.tabs[0].left.restore_navigation(navigation);
            ViewSettings {
                zoom: 0.52,
                sort: 3,
                descending: true,
                hidden: true,
                ..Default::default()
            }
            .apply(&mut app.tabs[0].left);
            app.tabs[0].left.filter = "notes".into();
            app.tabs[0].left.query.text = "notes".into();
            let mut right =
                Pane::dormant(app.next_id, &app.engine, directory.join("fixture/right"));
            app.next_id += 1;
            right.attach_search(app.search.client());
            right.restore_navigation(Navigation::new(Route::Folder(
                directory.join("fixture/right"),
            )));
            ViewSettings {
                zoom: 0.73,
                sort: 2,
                descending: true,
                hidden: true,
                ..Default::default()
            }
            .apply(&mut right);
            right.query.match_mode = foldex_match::MatchMode::Substring;
            right.filter = "needle".into();
            right.filter_lower = "needle".into();
            right.query.text = "needle".into();
            right.recursive = true;
            app.tabs[0].right = Some(right);
            app.tabs[0].vertical = true;
            app.tabs[0].split = 0.375;
            let mut spare = Pane::dormant(app.next_id, &app.engine, directory.join("fixture"));
            app.next_id += 1;
            spare.attach_search(app.search.client());
            spare.restore_navigation(Navigation::new(Route::Home));
            let spare_id = spare.id;
            app.tabs.push(Tab {
                left: spare,
                right: None,
                vertical: false,
                pending_axis: None,
                split: 0.5,
                reveal: 0.0,
                closing: false,
            });
            app.desk.add(spare_id);
            app.desk.focus(source);
            app.active = 0;
            app.active_pane = 1;
            Ok(Box::new(Probe {
                app,
                output: directory,
                executable: std::env::current_exe()?,
                source,
                spare: spare_id,
                phase: Phase::Begin,
                began: Instant::now(),
                report: shared,
            }))
        }),
    );
    let report = report.lock().unwrap_or_else(|error| error.into_inner());
    let ok = runtime.is_ok()
        && report.finished
        && report.error.is_none()
        && !report.checks.is_empty()
        && report.checks.iter().all(|(_, ok)| *ok);
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "ok": ok, "finished": report.finished, "checks": report.checks,
        "error": report.error, "child": report.child,
        "runtime_error": runtime.as_ref().err().map(ToString::to_string),
    }))?;
    std::fs::write(output.join("report.json"), bytes)?;
    runtime.map_err(|error| io::Error::other(error.to_string()))?;
    if !ok {
        return Err(io::Error::other(
            "Native tab transfer probe failed; inspect report.json",
        ));
    }
    Ok(())
}
fn fs_create_fixture(output: &Path) -> io::Result<()> {
    // Refuse existing output; every file belongs to this one explicit probe.
    std::fs::create_dir(output)?;
    std::fs::create_dir(output.join("source-state"))?;
    std::fs::create_dir(output.join("fixture"))?;
    std::fs::create_dir(output.join("fixture/right"))?;
    std::fs::write(
        output.join("fixture/right/needle.txt"),
        b"owned transfer fixture\n",
    )?;
    Ok(())
}
impl Probe {
    fn finish(&mut self, ctx: &egui::Context, error: Option<String>) {
        let mut report = self
            .report
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        report.error = error;
        report.finished = true;
        self.phase = Phase::Finished;
        self.app.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    fn step(&mut self, ctx: &egui::Context) {
        if matches!(self.phase, Phase::Finished) {
            return;
        }
        if self.began.elapsed() >= DEADLINE {
            self.finish(
                ctx,
                Some("The native transfer probe exceeded its finite deadline".into()),
            );
            return;
        }
        match self.phase {
            Phase::Begin => {
                let request = self.app.begin_tab_transfer(
                    self.source,
                    self.executable.clone(),
                    Some(self.output.join("child-report.json")),
                );
                if let Err(error) = request {
                    self.finish(ctx, Some(error.to_string()));
                    return;
                }
                let kept = self.app.tabs.iter().any(|tab| tab.left.id == self.source);
                self.report
                    .lock()
                    .unwrap()
                    .checks
                    .push(("source retained before readiness", kept));
                self.phase = Phase::Transfer;
            }
            Phase::Transfer => {
                if let Some(result) = self.app.window_transfer.completion.take() {
                    if let Err(error) = result {
                        self.finish(ctx, Some(error));
                        return;
                    }
                    let removed = !self.app.tabs.iter().any(|tab| tab.left.id == self.source);
                    self.report
                        .lock()
                        .unwrap()
                        .checks
                        .push(("source removed only after readiness and commit", removed));
                    let request = self
                        .app
                        .begin_tab_transfer_without_activation(self.spare, self.executable.clone());
                    if let Err(error) = request {
                        self.finish(ctx, Some(error.to_string()));
                        return;
                    }
                    self.phase = Phase::FailedActivation;
                }
            }
            Phase::FailedActivation => {
                if let Some(result) = self.app.window_transfer.completion.take() {
                    let kept = result.is_err()
                        && self.app.window_transfer.ready_acknowledged
                        && self.app.tabs.iter().any(|tab| tab.left.id == self.spare);
                    self.report.lock().unwrap().checks.push((
                        "child failure after readiness retains the original tab",
                        kept,
                    ));
                    let request = self.app.begin_tab_transfer(
                        self.spare,
                        self.output.join("missing-foldex.exe"),
                        None,
                    );
                    if let Err(error) = request {
                        self.finish(ctx, Some(error.to_string()));
                        return;
                    }
                    self.phase = Phase::FailedSpawn;
                }
            }
            Phase::FailedSpawn => {
                if let Some(result) = self.app.window_transfer.completion.take() {
                    let kept = result.is_err()
                        && self.app.tabs.iter().any(|tab| tab.left.id == self.spare);
                    self.report
                        .lock()
                        .unwrap()
                        .checks
                        .push(("failed spawn retains the original tab", kept));
                    self.phase = Phase::Child;
                }
            }
            Phase::Child => {
                if let Ok(bytes) = std::fs::read(self.output.join("child-report.json")) {
                    if let Ok(child) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                        let mut report = self.report.lock().unwrap();
                        report.checks.push((
                            "child preserves complete transferred view and active side",
                            child["ok"] == true,
                        ));
                        report.checks.push((
                            "child launcher remains isolated",
                            child["isolated_launcher"] == true,
                        ));
                        let outside = child["session_path"]
                            .as_str()
                            .is_some_and(|path| !Path::new(path).starts_with(&self.output));
                        report
                            .checks
                            .push(("child session uses its own temporary directory", outside));
                        report.child = Some(child);
                        drop(report);
                        self.finish(ctx, None);
                    }
                }
            }
            Phase::Finished => {}
        }
        ctx.request_repaint_after(Duration::from_millis(30));
    }
}
impl eframe::App for Probe {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.app.logic(ctx, frame);
    }
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        self.step(ui.ctx());
        self.app.ui(ui, frame);
    }
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, input);
    }
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        self.app.clear_color(visuals)
    }
}
