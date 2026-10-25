//! Opt-in real UI command workload; never injects global keyboard/mouse input.
use super::*;
use crate::commands::Command;
impl Foldex {
    pub(super) fn resize_stress_step(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self.benchmark.as_mut().and_then(|run| run.resize.take()) else {
            return;
        };
        let probes = ctx
            .data_mut(|data| {
                data.remove_temp::<Vec<crate::performance::LayoutProbe>>(Id::new(
                    "benchmark-layout-probes",
                ))
            })
            .unwrap_or_default();
        if self.started.elapsed() > Duration::from_secs(2) && run.step <= 512 {
            if let Some(frame) = &self.native_frame {
                if let Some(current) = frame.benchmark_rect() {
                    let origin = *run.origin.get_or_insert(current);
                    let viewport = ctx.content_rect();
                    let now = Instant::now();
                    let interval_ms = run
                        .last_frame
                        .replace(now)
                        .map(|previous| now.duration_since(previous).as_secs_f64() * 1000.0);
                    run.samples
                        .push(serde_json::json!({"step":run.step,"window":current,
                        "viewport":[viewport.width(),viewport.height()],"anchors":probes,
                        "cpu_ms":self.frames.back(),"interval_ms":interval_ms,
                        "previous_renderer_ms":self.renderer_cpu_ms,
                        "graphics_ns":crate::performance::graphics_timings(),
                        "actual_swapchain_scaling":crate::performance::window_scaling()}));
                    if run.step < 512 {
                        let wave = (run.step % 64).min(64 - run.step % 64) as i32;
                        let dx = wave * 12;
                        let dy = wave * 5;
                        let mut target = origin;
                        match run.step / 64 {
                            0 => target[2] -= dx,
                            1 => {
                                target[0] += dx;
                                target[2] -= dx;
                            }
                            2 => target[3] -= dy,
                            3 => {
                                target[1] += dy;
                                target[3] -= dy;
                            }
                            4 => {
                                target[0] += dx;
                                target[1] += dy;
                                target[2] -= dx;
                                target[3] -= dy;
                            }
                            5 => {
                                target[2] -= dx;
                                target[3] -= dy;
                            }
                            6 => target[0] += dx,
                            _ => {
                                target[0] += dx;
                                target[1] += dy;
                            }
                        }
                        frame.benchmark_resize(target, true);
                    } else {
                        frame.benchmark_resize(origin, false);
                    }
                    run.step += 1;
                    ctx.request_repaint();
                }
            }
        } else if run.step == 0 {
            ctx.request_repaint_after(Duration::from_millis(16));
        }
        self.benchmark.as_mut().unwrap().resize = Some(run);
    }

    pub(super) fn navigation_stress_step(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self
            .benchmark
            .as_mut()
            .and_then(|run| run.navigation.take())
        else {
            return;
        };
        if run.started.is_none()
            && run.next < run.paths.len() * 2
            && self.started.elapsed() > Duration::from_secs(2)
            && run
                .finished
                .is_none_or(|at| at.elapsed() > Duration::from_millis(120))
        {
            let path = run.paths[run.next % run.paths.len()].clone();
            run.started = Some(Instant::now());
            run.first_ms = None;
            run.first_full_ms = None;
            self.action(Action::Navigate(path));
            run.next += 1;
        }
        if run.next < run.paths.len() * 2 || run.started.is_some() {
            ctx.request_repaint_after(Duration::from_millis(8));
        }
        self.benchmark.as_mut().unwrap().navigation = Some(run);
    }

    // Observe after poll and painting, so this is the callback that actually
    // displays the new model rather than an extra frame of instrumentation lag.
    pub(super) fn navigation_stress_sample(&mut self) {
        let Some(mut run) = self
            .benchmark
            .as_mut()
            .and_then(|run| run.navigation.take())
        else {
            return;
        };
        if let Some(started) = run.started {
            let path = &run.paths[(run.next - 1) % run.paths.len()];
            let pane = self.pane();
            let usable =
                pane.path == *path && pane.snapshot.as_ref().is_some_and(|s| s.path == *path);
            if usable && run.first_ms.is_none() {
                run.first_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                run.first_cached = pane.cached;
            }
            let published = usable && pane.displayed_view_is_current();
            if published
                && run.first_full_ms.is_none()
                && pane
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.complete)
            {
                run.first_full_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
            }
            if published && !pane.loading
                || pane.error.is_some()
                || started.elapsed() > Duration::from_secs(10)
            {
                run.samples.push(serde_json::json!({"path":path,"round":(run.next - 1)/run.paths.len(),
                    "first_ms":run.first_ms,"first_full_ms":run.first_full_ms,"complete_ms":started.elapsed().as_secs_f64()*1000.0,
                    "backend_ms":pane.load_time.as_secs_f64()*1000.0,"first_cached":run.first_cached,
                    "entries":pane.view_len(),"model_entries":pane.snapshot.as_ref().map(|snapshot|snapshot.entries.len()),
                    "publication_ready":published,"error":pane.error}));
                run.started = None;
                run.finished = Some(Instant::now());
            }
        }
        self.benchmark.as_mut().unwrap().navigation = Some(run);
    }

    pub(super) fn search_stress_step(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self
            .benchmark
            .as_mut()
            .and_then(|run| run.full_search.take())
        else {
            return;
        };
        let now = Instant::now();
        run.interval_ms = run
            .last_frame
            .map_or(0.0, |last| now.duration_since(last).as_secs_f64() * 1000.0);
        run.last_frame = Some(now);
        match run.phase {
            0 if self.started.elapsed() > Duration::from_secs(2) => {
                self.pane_mut().toggle_recursive();
                run.phase = 1;
            }
            1 if self
                .pane()
                .search_status
                .as_ref()
                .is_some_and(|s| s.complete && !s.from_cache)
                && !self.pane().scanning =>
            {
                run.phase = 2;
                run.changed = Some(now);
            }
            2 if run
                .changed
                .is_some_and(|at| at.elapsed() > Duration::from_secs(5)) =>
            {
                self.pane_mut().filter = "file".into();
                self.pane_mut().refilter();
                run.phase = 3;
                run.changed = Some(now);
            }
            3 if run
                .changed
                .is_some_and(|at| at.elapsed() > Duration::from_secs(5)) =>
            {
                self.pane_mut().filter.clear();
                self.pane_mut().refilter();
                run.phase = 4;
                run.changed = Some(now);
            }
            4 if run
                .changed
                .is_some_and(|at| at.elapsed() > Duration::from_secs(5)) =>
            {
                self.pane_mut().toggle_recursive();
                run.phase = 5;
                run.changed = Some(now);
            }
            _ => {}
        }
        ctx.request_repaint_after(Duration::from_millis(16));
        self.benchmark.as_mut().unwrap().full_search = Some(run);
    }

    pub(super) fn search_stress_sample(&mut self, cpu_ms: f64) {
        let pane = self.pane();
        let entries = pane.search_status.as_ref().map_or(0, |s| s.entries);
        let total = pane.view_len();
        let loaded = pane.snapshot.as_ref().map_or(0, |s| s.entries.len());
        let scanning = pane.scanning;
        if let Some(run) = self
            .benchmark
            .as_mut()
            .and_then(|run| run.full_search.as_mut())
        {
            if run.samples.len() < 20_000 {
                run.samples.push(serde_json::json!({"at_ms":self.started.elapsed().as_secs_f64()*1000.0,
                    "phase":run.phase,"entries":entries,"matches":total,"loaded_rows":loaded,
                    "scanning":scanning,"ui_cpu_ms":cpu_ms,"poll_ms":run.poll_ms,"interval_ms":run.interval_ms}));
            }
        }
    }

    pub(super) fn stress_step(&mut self, ctx: &egui::Context) {
        let Some(mut stress) = self.benchmark.as_mut().and_then(|run| run.stress.take()) else {
            return;
        };
        let now = Instant::now();
        if self.started.elapsed() >= Duration::from_secs(2) && stress.actions < 1536 {
            if let Some(previous) = stress.last_frame {
                stress
                    .intervals_ms
                    .push(now.duration_since(previous).as_secs_f64() * 1000.0);
            }
            stress.last_frame = Some(now);
            let step = stress.actions % 128;
            let begin = Instant::now();
            if stress.many_tabs {
                match stress.actions {
                    0..=511 => self.execute_command(ctx, Command::NewTab),
                    512..=1023 => self.next_group_tab(false),
                    _ => self.close_tab(self.active),
                }
            } else {
                match step {
                    // Thirty-three tabs in the original workspace, then thirty-two
                    // independent visible workspaces with their own tab stacks.
                    0..=31 => self.execute_command(ctx, Command::NewTab),
                    32..=63 => self.execute_command(
                        ctx,
                        if step % 3 == 0 {
                            Command::SplitBelow
                        } else {
                            Command::SplitRight
                        },
                    ),
                    64..=95 => {
                        let at = (step * 17) % self.tabs.len();
                        let id = self.tabs[at].left.id;
                        self.focus_pane(id);
                        if step % 8 == 0 {
                            self.execute_command(ctx, Command::NewTab);
                        } else if step % 7 == 0 {
                            self.execute_command(ctx, Command::Refresh);
                        } else {
                            self.next_workspace(step % 2 == 0);
                        }
                    }
                    96..=127 => {
                        if self.desk.groups.len() > 1 {
                            self.close_workspace(self.desk.focused);
                        } else if self.tabs.len() > 1 {
                            self.close_tab(self.active);
                        }
                    }
                    _ => {}
                }
            }
            stress
                .command_ms
                .push(begin.elapsed().as_secs_f64() * 1000.0);
            stress.actions += 1;
            stress.max_tabs = stress.max_tabs.max(self.tabs.len());
            stress.max_panes = stress.max_panes.max(self.desk.groups.len());
            assert!(!self.tabs.is_empty() && self.active < self.tabs.len());
            let mut leaves = vec![];
            self.desk.root.leaves(&mut leaves);
            assert_eq!(leaves.len(), self.desk.groups.len());
            let mut owned = std::collections::HashSet::new();
            for group in &self.desk.groups {
                assert!(leaves.contains(&group.id) && group.tabs.contains(&group.active));
                for id in &group.tabs {
                    assert!(owned.insert(*id));
                    assert!(self.tabs.iter().any(|tab| tab.left.id == *id));
                }
            }
            assert_eq!(owned.len(), self.tabs.len());
            ctx.request_repaint();
        } else if stress.actions < 1536 {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else {
            stress.finished = true;
        }
        self.benchmark.as_mut().unwrap().stress = Some(stress);
    }
}
