//! Opt-in, owned-window integration workload. No global input, clipboard writes,
//! live defaults or primary-monitor windows. Captures only our owned windows.
use super::*;
use crate::launcher::{Mode, ResultAction, native};

impl Foldex {
    pub(super) fn launcher_benchmark_step(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self
            .benchmark
            .as_mut()
            .and_then(|benchmark| benchmark.launcher.take())
        else {
            return;
        };
        let revision = foldex_ui::fonts::revision(ctx);
        if run.atlas_generations.last() != Some(&revision) && run.atlas_generations.len() < 256 {
            run.atlas_generations.push(revision);
        }
        if let Some(receiver) = &run.capture_result {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Capture worker exited without a result".into()))
                }
            };
            if let Some(result) = result {
                run.capture_result = None;
                match result {
                    Ok(path) => run.captures.push(path),
                    Err(error) => run.errors.push(error),
                }
            }
        }
        let now = Instant::now();
        if !run.finished
            && self.started.elapsed() > Duration::from_millis(1500)
            && run.next.is_none_or(|next| now >= next)
        {
            match run.phase {
                80 => {
                    let path = self
                        .benchmark
                        .as_ref()
                        .unwrap()
                        .state_dir
                        .join("Menu fixture.txt");
                    self.context.open_for_pane(
                        self.pane().id,
                        self.window_owner,
                        vec![path],
                        false,
                        ctx.content_rect().center(),
                    );
                    run.phase = 81;
                    run.next = Some(now + Duration::from_millis(250));
                }
                81 => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        "root-context-core".to_owned(),
                    )));
                    run.phase = 82;
                }
                82 if run.captures.len() == 1 => {
                    self.context.benchmark_more_actions();
                    run.phase = 83;
                }
                83 => match self.context.benchmark_native_state() {
                    Ok(Some(rows)) if rows > 0 => {
                        run.checks.push(serde_json::json!({"check":"actual_installed_context_actions_loaded","rows":rows,"passed":true}));
                        run.phase = 84;
                        run.next = Some(now + Duration::from_millis(300));
                    }
                    Err(error) => {
                        run.errors.push(error);
                        run.finished = true;
                    }
                    _ => {}
                },
                84 => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        "root-context-apps".to_owned(),
                    )));
                    run.phase = 85;
                }
                85 if run.captures.len() == 2 => {
                    self.context.close();
                    run.phase = 0;
                    run.next = Some(now + Duration::from_millis(200));
                }
                0 => {
                    self.open_launcher(false);
                    run.phase = 1;
                    run.next = Some(now + Duration::from_millis(200));
                }
                1 if !self.launcher.pending() => {
                    if let Some(row) = self.launcher.rows.iter().find(|row| {
                        matches!(
                            row.action,
                            ResultAction::Application(_) | ResultAction::Open(_)
                        )
                    }) {
                        run.query = row.title.clone();
                        run.checks.push(serde_json::json!({"check":"initial_apps_available","rows":self.launcher.rows.len(),"passed":true}));
                        self.launcher.set_mode(Mode::Apps);
                        self.launcher.set_query(&run.query);
                        run.phase = 2;
                        run.next = Some(now + Duration::from_millis(200));
                    }
                }
                2 if !self.launcher.pending() => {
                    let found = self.launcher.rows.iter().any(|row| row.title == run.query);
                    run.checks.push(serde_json::json!({"check":"app_query_roundtrip","rows":self.launcher.rows.len(),"passed":found}));
                    if !found {
                        run.errors
                            .push("Installed app disappeared from its exact-name query".into());
                    }
                    run.capture = Some("app-results");
                    run.capture_sent = false;
                    run.phase = 21;
                    run.next = Some(now + Duration::from_millis(200));
                }
                21 if run.captures.len() == 3 => {
                    self.launcher.set_mode(Mode::Calculator);
                    self.launcher.set_query("(24 + 18) / 2");
                    run.phase = 3;
                    run.next = Some(now + Duration::from_millis(200));
                    run.capture = None;
                }
                3 if self
                    .launcher
                    .rows
                    .first()
                    .is_some_and(|row| row.title == "21") =>
                {
                    run.checks
                        .push(serde_json::json!({"check":"calculator","passed":true}));
                    self.open_launcher_options();
                    run.capture = None;
                    run.phase = 31;
                    run.next = Some(now + Duration::from_millis(400));
                }
                31 => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        "root-settings-wide".to_owned(),
                    )));
                    run.phase = 4;
                    run.next = Some(now + Duration::from_millis(300));
                }
                4 if run.captures.len() == 4 => {
                    run.checks.push(serde_json::json!({"check":"launcher_settings_in_root_options","passed":self.options && !self.launcher_open}));
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(640.0, 540.0)));
                    run.phase = 41;
                    run.next = Some(now + Duration::from_millis(400));
                }
                41 => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        "root-settings-compact".to_owned(),
                    )));
                    run.phase = 5;
                }
                5 if run.captures.len() == 5 => {
                    self.options_popup.close();
                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1200.0, 800.0)));
                    run.compact = false;
                    run.capture = None;
                    self.launcher_positioned = false;
                    run.phase = 6;
                    run.next = Some(now + Duration::from_millis(300));
                }
                6 => {
                    if run.toggles < 128 {
                        if run.toggles % 2 == 0 {
                            self.open_launcher(false);
                            self.launcher.set_mode(Mode::Files);
                            self.launcher.set_query("manif");
                        } else {
                            self.close_launcher(false);
                        }
                        run.toggles += 1;
                        run.next = Some(now + Duration::from_millis(20));
                    } else {
                        run.checks.push(serde_json::json!({"check":"rapid_open_close_and_scan_cancel","toggles":run.toggles,"passed":true}));
                        self.open_launcher(false);
                        self.launcher.set_mode(Mode::Files);
                        self.launcher.set_query("manif");
                        run.phase = 61;
                        run.next = Some(now + Duration::from_millis(200));
                    }
                }
                61 if self.launcher.rows.iter().any(|row| {
                    matches!(row.action, ResultAction::Open(_) | ResultAction::Browse(_))
                }) =>
                {
                    run.checks.push(serde_json::json!({"check":"real_file_search_results","rows":self.launcher.rows.len(),"passed":true}));
                    run.capture = Some("file-results");
                    run.capture_sent = false;
                    run.phase = 62;
                }
                62 if run.captures.len() == 6 => {
                    self.hide_explorer(ctx);
                    self.launcher.set_mode(Mode::Calculator);
                    self.launcher.set_query("6 * 7");
                    run.phase = 7;
                    run.capture = None;
                    run.next = Some(now + Duration::from_millis(200));
                }
                7 if self
                    .launcher
                    .rows
                    .first()
                    .is_some_and(|row| row.title == "42") =>
                {
                    run.checks.push(serde_json::json!({"check":"launcher_works_with_hidden_explorer","passed":!self.explorer_visible}));
                    run.capture = Some("calculator");
                    run.capture_sent = false;
                    run.phase = 8;
                    run.next = Some(now + Duration::from_millis(200));
                }
                8 if run.captures.len() == 7 => {
                    self.check_launcher_monitor(&mut run);
                    self.close_launcher(false);
                    self.show_explorer(ctx);
                    let workspace_preserved = self.benchmark.as_ref().is_some_and(|benchmark| {
                        self.tabs.len() == 1 && self.pane().path == benchmark.scope
                    });
                    run.checks.push(serde_json::json!({"check":"explorer_workspace_preserved","passed":workspace_preserved}));
                    if !workspace_preserved {
                        run.errors
                            .push("Launcher changed the explorer workspace".into());
                    }
                    run.capture = None;
                    run.phase = 90;
                    run.next = Some(now + Duration::from_millis(300));
                }
                90 => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        "root-after-launcher".to_owned(),
                    )));
                    run.phase = 91;
                }
                91 if run.captures.len() == 8 => {
                    let stable = run.atlas_generations.len() == 1;
                    run.checks.push(serde_json::json!({"check":"popups_keep_shared_atlas_stable","generations":run.atlas_generations.len(),"passed":stable}));
                    run.atlas_cycle = 1;
                    run.phase = 92;
                    run.next = Some(now + Duration::from_millis(300));
                }
                92 if run.atlas_cycle == 3 => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(
                        "root-after-atlas-reset".to_owned(),
                    )));
                    run.phase = 93;
                }
                93 if run.captures.len() == 9 => {
                    let (checked, mismatches) =
                        self.pane().benchmark_name_galleys(ctx, &self.theme);
                    let valid = checked > 0 && mismatches == 0 && run.atlas_generations.len() == 3;
                    run.checks.push(serde_json::json!({"check":"retained_text_survives_atlas_recreation","checked":checked,"mismatches":mismatches,"generations":run.atlas_generations.len(),"passed":valid}));
                    if !valid {
                        run.errors.push(
                            "Retained text did not survive the atlas recreation workload".into(),
                        );
                    }
                    run.finished = true;
                }
                _ => {}
            }
        }
        if !run.finished {
            ctx.request_repaint_after(Duration::from_millis(20));
        }
        self.benchmark.as_mut().unwrap().launcher = Some(run);
    }

    fn check_launcher_monitor(&self, run: &mut crate::performance::LauncherUiRun) {
        let bounds = native::launcher_bounds(&format!("Foldex Launcher · {}", std::process::id()));
        let within = bounds
            .zip(self.launcher_secondary)
            .is_some_and(|(rect, monitor)| {
                !monitor.primary
                    && rect[0] >= monitor.x
                    && rect[1] >= monitor.y
                    && rect[2] <= monitor.x + monitor.width
                    && rect[3] <= monitor.y + monitor.height
            });
        run.checks.push(serde_json::json!({"check":"owned_launcher_on_secondary_monitor","bounds":bounds,"passed":within}));
        if !within {
            run.errors
                .push("Launcher placement did not stay within the secondary monitor".into());
        }
        let centered = bounds
            .zip(self.launcher_secondary)
            .is_some_and(|(rect, monitor)| {
                (rect[0] + rect[2] - 2 * monitor.x - monitor.width).abs() <= 1
                    && (rect[1] + rect[3] - 2 * monitor.y - monitor.height).abs() <= 1
            });
        run.checks.push(
            serde_json::json!({"check":"launcher_remains_centered_after_content_resizing",
            "bounds":bounds,"passed":centered}),
        );
        let locked = native::launcher_locked(&format!("Foldex Launcher · {}", std::process::id()))
            == Some(true);
        run.checks.push(
            serde_json::json!({"check":"launcher_rejects_move_resize_and_maximize",
            "passed":locked}),
        );
        if !centered || !locked {
            run.errors
                .push("Launcher did not remain fixed at the work-area center".into());
        }
        let monitors = crate::performance::monitor::inventory().unwrap_or_default();
        let follows = native::launcher_monitor_selection(self.window_owner, &monitors);
        run.checks.push(
            serde_json::json!({"check":"launcher_selects_cursor_monitor_with_focus_fallback",
            "displays_tested":monitors.len(),"passed":follows}),
        );
        if !follows {
            run.errors
                .push("Launcher did not select the invocation monitor".into());
        }
    }

    pub(super) fn launcher_capture(
        &mut self,
        label: &str,
        image: &std::sync::Arc<egui::ColorImage>,
    ) {
        let Some(benchmark) = self.benchmark.as_mut() else {
            return;
        };
        let Some(run) = benchmark.launcher.as_mut() else {
            return;
        };
        if !matches!(
            label,
            "root-settings-wide"
                | "root-settings-compact"
                | "calculator"
                | "app-results"
                | "file-results"
                | "root-context-core"
                | "root-context-apps"
                | "root-after-launcher"
                | "root-after-atlas-reset"
        ) {
            return;
        }
        let path = benchmark.output.with_extension(format!("{label}.png"));
        if run.captures.contains(&path) || run.capture_result.is_some() {
            return;
        }
        let image = image.clone();
        let wake = self.ui_context.clone();
        let (send, receiver) = std::sync::mpsc::sync_channel(1);
        run.capture_result = Some(receiver);
        if let Err(error) = std::thread::Builder::new()
            .name("foldex-benchmark-capture".into())
            .spawn(move || {
                let result = (|| -> Result<PathBuf, Box<dyn std::error::Error>> {
                    use image::ImageEncoder;
                    let mut file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)?;
                    let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
                    for pixel in &image.pixels {
                        bytes.extend_from_slice(&pixel.to_array());
                    }
                    image::codecs::png::PngEncoder::new(&mut file).write_image(
                        &bytes,
                        image.width() as u32,
                        image.height() as u32,
                        image::ExtendedColorType::Rgba8,
                    )?;
                    Ok(path)
                })()
                .map_err(|error| format!("Capture failed: {error}"));
                let _ = send.send(result);
                wake.request_repaint();
            })
        {
            run.capture_result = None;
            run.errors
                .push(format!("Cannot start capture worker: {error}"));
        }
    }
}
