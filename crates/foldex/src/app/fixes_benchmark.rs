//! Isolated visual regression scenarios. Drives only this app's model and
//! owned viewports; never sends desktop input or launches a result action.
use super::*;
use crate::launcher::{Mode, ResultAction, ResultGroup};
use crate::pane::columns::Column;
use crate::performance::FixesUiRun;

impl Foldex {
    pub(super) fn fixes_benchmark_step(&mut self, ctx: &egui::Context) {
        let Some(mut run) = self.benchmark.as_mut().and_then(|run| run.fixes.take()) else {
            return;
        };
        if let Some(receiver) = &run.capture_result {
            match receiver.try_recv() {
                Ok(Ok(path)) => {
                    run.captures.push(path);
                    run.capture_result = None;
                    run.waiting_capture = false;
                }
                Ok(Err(error)) => {
                    run.errors.push(error);
                    run.capture_result = None;
                    run.waiting_capture = false;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    run.errors.push("Screenshot worker disconnected".into());
                    run.capture_result = None;
                    run.waiting_capture = false;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        let now = Instant::now();
        if !run.finished
            && !run.waiting_capture
            && self.started.elapsed() > Duration::from_millis(1500)
            && run.next.is_none_or(|next| now >= next)
        {
            let ready = !self.pane().loading && self.pane().displayed_view_is_current();
            run.delay_ms = 400;
            if run.refinement {
                if run.last_phase != run.phase {
                    run.last_phase = run.phase;
                    run.phase_started = Some(now);
                } else if run
                    .phase_started
                    .is_some_and(|started| now.duration_since(started) > Duration::from_secs(20))
                {
                    run.errors
                        .push(format!("Refinement phase {} timed out", run.phase));
                    run.samples.push(serde_json::json!({
                        "stage":"timeout", "phase":run.phase,
                        "display_current":self.pane().displayed_view_is_current(),
                        "loading":self.pane().loading,
                        "columns":self.pane().benchmark_column_diagnostics(),
                        "listing":self.pane().benchmark_listing_state()
                        ,"viewport":[ctx.content_rect().width(),ctx.content_rect().height()]
                    }));
                    run.finished = true;
                    self.benchmark.as_mut().unwrap().expires = now + Duration::from_secs(1);
                }
                if !run.finished {
                    self.refinement_benchmark_phase(ctx, &mut run, ready);
                }
            } else {
                match run.phase {
                    0 if ready => {
                        capture(ctx, &mut run, "fix-browser", false);
                        run.phase = 90;
                    }
                    90 => {
                        self.open_commands();
                        run.phase = 91;
                    }
                    91 => {
                        capture(ctx, &mut run, "fix-command-reference", false);
                        run.phase = 92;
                    }
                    92 => {
                        self.commands.close();
                        run.phase = 1;
                    }
                    1 => {
                        self.sidebar = false;
                        run.phase = 2;
                    }
                    2 => {
                        capture(ctx, &mut run, "fix-sidebar-hidden", false);
                        run.phase = 100;
                    }
                    100 => {
                        run.pointer = Some(self.pane().benchmark_footer_view_rect().center());
                        run.phase = 101;
                    }
                    101 => {
                        check(
                            &mut run,
                            "percentage_hover_opens_view",
                            self.pane().benchmark_footer_view_open(),
                        );
                        capture(ctx, &mut run, "fix-view-hover", false);
                        run.phase = 102;
                    }
                    102 => {
                        let pos = self.pane().benchmark_footer_view_rect().center();
                        for pressed in [true, false] {
                            run.input.push(egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed,
                                modifiers: Modifiers::NONE,
                            });
                        }
                        run.phase = 103;
                    }
                    103 => {
                        check(
                            &mut run,
                            "hover_then_click_keeps_view_open",
                            self.pane().benchmark_footer_view_open(),
                        );
                        run.pointer = Some(pos2(4.0, 180.0));
                        run.input.push(egui::Event::Key {
                            key: Key::Escape,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Modifiers::NONE,
                        });
                        run.phase = 110;
                    }
                    110 => {
                        self.pane_mut().benchmark_column_menu(
                            Column::Modified,
                            Rect::from_min_size(pos2(560.0, 82.0), vec2(260.0, 0.0)),
                            ctx,
                        );
                        run.phase = 111;
                    }
                    111 => {
                        capture(ctx, &mut run, "fix-column-menu", false);
                        run.phase = 112;
                    }
                    112 => {
                        escape(&mut run);
                        run.phase = 1120;
                    }
                    1120 => {
                        self.pane_mut().benchmark_type_filter(
                            Rect::from_min_size(pos2(480.0, 78.0), vec2(20.0, 25.0)),
                            ctx,
                        );
                        run.phase = 113;
                    }
                    113 => {
                        capture(ctx, &mut run, "fix-type-filter", false);
                        run.phase = 114;
                    }
                    114 => {
                        escape(&mut run);
                        run.phase = 1140;
                    }
                    1140 => {
                        self.pane_mut().query.type_filter = Some(foldex_core::TypeFilter {
                            include_folders: false,
                            exclude: false,
                            extensions: vec!["md".into(), "txt".into()],
                        });
                        self.pane_mut().refilter();
                        run.phase = 115;
                    }
                    115 if ready => {
                        let pane = self.pane();
                        check(
                            &mut run,
                            "multiple_type_filter",
                            pane.view_len() > 0
                                && (0..pane.view_len()).all(|row| {
                                    pane.entry_at(row).is_some_and(|entry| {
                                        !entry.is_dir && matches!(entry.extension(), "md" | "txt")
                                    })
                                }),
                        );
                        capture(ctx, &mut run, "fix-filtered-types", false);
                        run.phase = 116;
                    }
                    116 => {
                        self.pane_mut().query.type_filter = None;
                        self.pane_mut().refilter();
                        for column in Column::ALL {
                            self.pane_mut().benchmark_set_column(column, true);
                        }
                        run.phase = 117;
                    }
                    117 if ready && !self.pane().benchmark_column_state().2 => {
                        capture(ctx, &mut run, "fix-all-columns", false);
                        run.phase = 118;
                    }
                    118 => {
                        self.pane_mut().restore_columns(Default::default());
                        run.phase = 3;
                    }
                    3 => {
                        self.sidebar = true;
                        click(
                            &mut run,
                            self.pane().benchmark_footer_kind_rect(false).center(),
                        );
                        run.phase = 4;
                    }
                    4 if ready => {
                        self.pane_mut().selected.insert(0);
                        self.pane_mut().selected.insert(1);
                        let pane = self.pane();
                        check(
                            &mut run,
                            "folders_only",
                            pane.view_len() > 0
                                && (0..pane.view_len()).all(|row| {
                                    pane.entry_at(row).is_some_and(|entry| entry.is_dir)
                                }),
                        );
                        capture(ctx, &mut run, "fix-folders-only", false);
                        run.phase = 5;
                    }
                    5 => {
                        click(
                            &mut run,
                            self.pane().benchmark_footer_kind_rect(true).center(),
                        );
                        run.phase = 51;
                    }
                    51 => {
                        click(
                            &mut run,
                            self.pane().benchmark_footer_kind_rect(false).center(),
                        );
                        run.phase = 6;
                    }
                    6 if ready => {
                        let pane = self.pane();
                        check(
                            &mut run,
                            "files_only",
                            pane.view_len() > 0
                                && (0..pane.view_len()).all(|row| {
                                    pane.entry_at(row).is_some_and(|entry| !entry.is_dir)
                                }),
                        );
                        capture(ctx, &mut run, "fix-files-only", false);
                        run.phase = 7;
                    }
                    7 => {
                        click(
                            &mut run,
                            self.pane().benchmark_footer_kind_rect(false).center(),
                        );
                        run.phase = 8;
                    }
                    8 if ready => {
                        check(&mut run, "both_kinds_hidden", self.pane().view_len() == 0);
                        capture(ctx, &mut run, "fix-kinds-hidden", false);
                        run.phase = 9;
                    }
                    9 => {
                        self.pane_mut().query.kind = foldex_core::KindFilter::All;
                        self.pane_mut().filter = "Browsers".into();
                        self.pane_mut().refilter();
                        if !self.pane().recursive {
                            self.pane_mut().toggle_recursive();
                        }
                        run.phase = 10;
                    }
                    10 if ready && !self.pane().scanning && self.pane().view_len() > 0 => {
                        check(&mut run, "recursive_results", self.pane().recursive);
                        capture(ctx, &mut run, "fix-expanded-search", false);
                        run.phase = 11;
                    }
                    11 => {
                        self.pane_mut().query.kind = foldex_core::KindFilter::Folders;
                        self.pane_mut().refilter();
                        run.phase = 12;
                    }
                    12 if ready && !self.pane().scanning => {
                        let pane = self.pane();
                        check(
                            &mut run,
                            "recursive_folders_only",
                            pane.view_len() > 0
                                && (0..pane.view_len().min(32)).all(|row| {
                                    pane.entry_at(row).is_some_and(|entry| entry.is_dir)
                                }),
                        );
                        capture(ctx, &mut run, "fix-expanded-folders", false);
                        run.phase = 13;
                    }
                    13 => {
                        self.pane_mut().query.kind = foldex_core::KindFilter::All;
                        if self.pane().recursive {
                            self.pane_mut().toggle_recursive();
                        }
                        self.pane_mut().filter.clear();
                        self.pane_mut().refilter();
                        self.sidebar = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(640.0, 540.0)));
                        run.phase = 14;
                    }
                    14 if ready => {
                        capture(ctx, &mut run, "fix-compact-browser", false);
                        run.phase = 15;
                    }
                    15 => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(
                            1200.0, 800.0,
                        )));
                        self.sidebar = true;
                        let scope = self.benchmark.as_ref().unwrap().scope.clone();
                        self.bookmarks = vec![scope.join("Design"), scope.join("Projects")];
                        self.recent =
                            VecDeque::from(vec![scope.join("Documents"), scope.join("Notes.md")]);
                        self.open_launcher(false);
                        run.phase = 16;
                    }
                    16 if !self.launcher.pending() => {
                        check(
                            &mut run,
                            "unified_default_launcher",
                            self.launcher
                                .rows
                                .iter()
                                .any(|row| row.group == ResultGroup::Favorites)
                                && self
                                    .launcher
                                    .rows
                                    .iter()
                                    .any(|row| row.group == ResultGroup::Recents),
                        );
                        capture(ctx, &mut run, "fix-launcher-default", true);
                        run.phase = 17;
                    }
                    17 => {
                        self.launcher.set_mode(Mode::All);
                        self.launcher.set_query("(24 + 18) / 2");
                        run.phase = 18;
                    }
                    18 if !self.launcher.pending() => {
                        let valid = self.launcher.rows.first().is_some_and(|row| {
                        row.title == "21"
                            && matches!(&row.action, ResultAction::Copy(answer) if answer == "21")
                    });
                        check(&mut run, "automatic_calculator_copy_action", valid);
                        capture(ctx, &mut run, "fix-launcher-calculator", true);
                        run.phase = 19;
                    }
                    19 => {
                        self.launcher.set_query("notep");
                        run.phase = 20;
                    }
                    20 if !self.launcher.pending() => {
                        check(
                            &mut run,
                            "apps_lead_search",
                            self.launcher
                                .rows
                                .first()
                                .is_some_and(|row| row.group == ResultGroup::Apps),
                        );
                        capture(ctx, &mut run, "fix-launcher-results", true);
                        run.phase = 21;
                    }
                    21 => {
                        run.compact = true;
                        self.launcher_positioned = false;
                        self.launcher.set_query("");
                        run.phase = 22;
                    }
                    22 if !self.launcher.pending() => {
                        capture(ctx, &mut run, "fix-launcher-compact", true);
                        run.phase = 23;
                    }
                    23 => {
                        self.close_launcher(false);
                        check(
                            &mut run,
                            "launcher_hidden_synchronously",
                            !owned_launcher_visible(),
                        );
                        run.compact = false;
                        run.phase = 24;
                    }
                    24 => {
                        capture(ctx, &mut run, "fix-after-launcher", false);
                        run.phase = 25;
                    }
                    25 => {
                        self.theme = Theme::paper();
                        self.theme.apply(ctx);
                        run.phase = 26;
                    }
                    26 => {
                        capture(ctx, &mut run, "fix-browser-paper", false);
                        run.phase = 27;
                    }
                    27 => {
                        self.open_launcher(false);
                        run.phase = 28;
                    }
                    28 if !self.launcher.pending() => {
                        capture(ctx, &mut run, "fix-launcher-paper", true);
                        run.phase = 29;
                    }
                    29 => {
                        self.close_launcher(false);
                        self.theme = Theme::midnight();
                        self.theme.font_size = 17.0;
                        self.theme.apply(ctx);
                        run.phase = 30;
                    }
                    30 => {
                        capture(ctx, &mut run, "fix-browser-larger-text", false);
                        run.phase = 31;
                    }
                    31 => {
                        self.open_launcher(false);
                        run.phase = 32;
                    }
                    32 if !self.launcher.pending() => {
                        capture(ctx, &mut run, "fix-launcher-larger-text", true);
                        run.phase = 33;
                    }
                    33 => {
                        self.close_launcher(false);
                        run.finished = true;
                        self.benchmark.as_mut().unwrap().expires = now + Duration::from_secs(1);
                    }
                    _ => {}
                }
            }
            run.next = Some(now + Duration::from_millis(run.delay_ms));
        }
        if !run.finished {
            ctx.request_repaint_after(Duration::from_millis(20));
        }
        self.benchmark.as_mut().unwrap().fixes = Some(run);
    }

    pub(super) fn fixes_capture(&mut self, label: &str, image: &Arc<egui::ColorImage>) {
        if !label.starts_with("fix-") {
            return;
        }
        let Some(benchmark) = self.benchmark.as_mut() else {
            return;
        };
        let Some(run) = benchmark.fixes.as_mut() else {
            return;
        };
        let path = benchmark.output.with_extension(format!("{label}.png"));
        if run.captures.contains(&path) || run.capture_result.is_some() {
            return;
        }
        let image = image.clone();
        let wake = self.ui_context.clone();
        let (send, receiver) = std::sync::mpsc::sync_channel(1);
        run.capture_result = Some(receiver);
        if let Err(error) = std::thread::Builder::new()
            .name("foldex-fixes-capture".into())
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
                .map_err(|error| format!("Screenshot failed: {error}"));
                let _ = send.send(result);
                wake.request_repaint();
            })
        {
            run.capture_result = None;
            run.waiting_capture = false;
            run.errors.push(error.to_string());
        }
    }
}

impl Foldex {
    fn refinement_benchmark_phase(
        &mut self,
        ctx: &egui::Context,
        run: &mut FixesUiRun,
        ready: bool,
    ) {
        use crate::{
            home_history::Access,
            sidebar::{Section, Usage},
        };
        let scope = self.benchmark.as_ref().unwrap().scope.clone();
        match run.phase {
            2000 if ready && !self.pane().benchmark_column_diagnostics().busy => {
                ctx.data_mut(|data| data.insert_temp(Id::new("benchmark-probe-layout"), true));
                self.sidebar = true;
                self.bookmarks = vec![scope.join("Design"), scope.join("Projects")];
                self.pane_mut().selected.clear();
                capture(ctx, run, "fix-refine-browser", false);
                run.phase = 2001;
            }
            2001 => {
                self.new_tab(scope.clone());
                run.phase = 2002;
                run.delay_ms = 16;
            }
            2002 if ready => {
                let state = self.pane().benchmark_listing_state();
                check(
                    run,
                    "same_folder_tab_retains_items_without_entry_replay",
                    state.0 && state.1 == 0 && state.2 > 0,
                );
                run.samples
                    .push(serde_json::json!({"stage":"duplicate-tab","listing":state}));
                capture(ctx, run, "fix-refine-duplicate-tab", false);
                run.phase = 2003;
            }
            2003 if ready && !self.pane().benchmark_column_diagnostics().busy => {
                run.baseline = Some(self.pane().benchmark_column_diagnostics());
                run.samples
                    .push(serde_json::json!({"stage":"before-idle","columns":run.baseline}));
                self.pane_mut().selected.insert(0);
                run.pointer = Some(pos2(400.0, 175.0));
                self.pane_mut().refilter();
                run.phase = 2004;
                run.delay_ms = 2400;
            }
            2004 if ready && !self.pane().benchmark_column_diagnostics().busy => {
                let after = self.pane().benchmark_column_diagnostics();
                let before = run.baseline.unwrap();
                check(
                    run,
                    "idle_selection_and_same_query_do_not_restart_folder_walks",
                    after.walks_started == before.walks_started
                        && after.invalidations == before.invalidations,
                );
                run.samples
                    .push(serde_json::json!({"stage":"after-idle","columns":after}));
                self.pane_mut().selected.clear();
                let result = std::fs::write(
                    scope.join("Unrelated root activity.txt"),
                    b"Owned native watcher fixture\n",
                );
                if let Err(error) = result {
                    run.errors.push(error.to_string());
                }
                run.phase = 2005;
                run.delay_ms = 1500;
            }
            2005 if ready && !self.pane().benchmark_column_diagnostics().busy => {
                let after = self.pane().benchmark_column_diagnostics();
                check(
                    run,
                    "root_file_activity_preserves_unrelated_folder_walks",
                    after.walks_started == run.baseline.unwrap().walks_started,
                );
                run.samples
                    .push(serde_json::json!({"stage":"root-file-change","columns":after}));
                run.baseline = Some(after);
                if let Err(error) = std::fs::write(
                    scope.join("Design/Runtime change.txt"),
                    b"Real changed child\n",
                ) {
                    run.errors.push(error.to_string());
                }
                // The browser watches direct children. A reversible rename in
                // this validated generated scope gives a real child notification
                // without pretending a deep descendant write is covered.
                let source = scope.join("Design");
                let temporary = scope.join("Design - native watch fixture");
                if temporary.exists() {
                    run.errors
                        .push("Owned watch fixture target already exists".into());
                } else if let Err(error) = std::fs::rename(&source, &temporary)
                    .and_then(|()| std::fs::rename(&temporary, &source))
                {
                    run.errors
                        .push(format!("Owned child rename fixture: {error}"));
                }
                run.phase = 2006;
                run.delay_ms = 1400;
            }
            2006 if ready
                && !self.pane().benchmark_column_diagnostics().busy
                && self.pane().benchmark_column_diagnostics().walks_started
                    > run.baseline.unwrap().walks_started =>
            {
                check(run, "real_child_change_recalculates_affected_folder", true);
                run.samples.push(serde_json::json!({"stage":"child-change","columns":self.pane().benchmark_column_diagnostics()}));
                self.pane_mut().benchmark_pending_columns(true);
                run.pointer = Some(pos2(-20.0, -20.0));
                run.phase = 2007;
            }
            2007 => {
                check(
                    run,
                    "controlled_pending_scene_is_labeled",
                    self.pane()
                        .benchmark_column_diagnostics()
                        .controlled_pending,
                );
                capture(ctx, run, "fix-refine-pending-columns", false);
                run.phase = 2008;
            }
            2008 => {
                self.pane_mut().benchmark_pending_columns(false);
                run.baseline = Some(self.pane().benchmark_column_diagnostics());
                self.pane_mut().refresh();
                run.phase = 2009;
                run.delay_ms = 1000;
            }
            2009 if ready && !self.pane().benchmark_column_diagnostics().busy => {
                check(
                    run,
                    "explicit_refresh_invalidates_folder_statistics",
                    self.pane().benchmark_column_diagnostics().invalidations
                        > run.baseline.unwrap().invalidations,
                );
                run.saved_search = Some(self.pane().benchmark_footer_search_rect());
                run.saved_expand = Some(self.pane().benchmark_footer_expand_rect());
                self.pane_mut().filter = "note".into();
                self.pane_mut().refilter();
                run.phase = 2010;
            }
            2010 if ready => {
                let same_rect = |a: Rect, b: Rect| {
                    (a.min - b.min).length() < 0.75 && (a.max - b.max).length() < 0.75
                };
                check(
                    run,
                    "typing_keeps_search_and_expand_geometry_fixed",
                    same_rect(
                        run.saved_search.unwrap(),
                        self.pane().benchmark_footer_search_rect(),
                    ) && same_rect(
                        run.saved_expand.unwrap(),
                        self.pane().benchmark_footer_expand_rect(),
                    ),
                );
                let clear = self.pane().benchmark_footer_clear_rect();
                check(
                    run,
                    "clear_slot_appears_after_expanded_search_button",
                    clear.is_finite()
                        && clear.width() > 8.0
                        && clear.left()
                            >= self.pane().benchmark_footer_expand_rect().right() - 0.75,
                );
                capture(ctx, run, "fix-refine-filter-clear-slot", false);
                run.phase = 2011;
            }
            2011 => {
                click(run, self.pane().benchmark_footer_clear_rect().center());
                run.phase = 2012;
            }
            2012 if ready => {
                run.pointer = Some(pos2(-20.0, -20.0));
                check(
                    run,
                    "clear_control_removes_only_filter_text",
                    self.pane().filter.is_empty() && !self.pane().recursive,
                );
                self.action(Action::Bookmark(scope.clone()));
                run.phase = 2013;
            }
            2013 => {
                check(
                    run,
                    "bookmark_toggle_fills_current_folder_icon",
                    self.pane().bookmarked,
                );
                capture(ctx, run, "fix-refine-bookmarked-folder", false);
                run.phase = 2014;
            }
            2014 => {
                self.action(Action::Bookmark(scope.clone()));
                run.phase = 2015;
            }
            2015 => {
                check(
                    run,
                    "bookmark_toggle_removes_current_folder",
                    !self.pane().bookmarked,
                );
                if self.sidebar_data.benchmark_override_usage(&[
                    Usage::from_bytes(1_000_000_000_000, 50_000_000_000),
                    None,
                ]) > 0
                {
                    run.phase = 2016;
                }
            }
            2016 => {
                capture(ctx, run, "fix-refine-storage-near-full", false);
                run.phase = 2017;
            }
            2017 => {
                self.sidebar_sections.benchmark_open_section_menu(
                    Section::Storage,
                    ctx,
                    self.theme.metrics().scale,
                );
                run.phase = 2018;
            }
            2018 => {
                capture(ctx, run, "fix-refine-storage-menu", false);
                run.phase = 20181;
            }
            20181 => {
                escape(run);
                run.phase = 2019;
            }
            2019 => {
                self.sidebar_data.benchmark_restore_usage();
                self.sidebar_sections.benchmark_open_section_menu(
                    Section::Places,
                    ctx,
                    self.theme.metrics().scale,
                );
                run.phase = 2020;
            }
            2020 => {
                capture(ctx, run, "fix-refine-places-menu", false);
                run.phase = 20201;
            }
            20201 => {
                escape(run);
                run.phase = 2021;
            }
            2021 => {
                self.open_commands();
                run.phase = 2022;
            }
            2022 => {
                capture(ctx, run, "fix-refine-command-keycaps", false);
                run.phase = 20221;
            }
            20221 => {
                self.commands.close();
                run.phase = 2023;
            }
            2023 => {
                self.pane_mut().filter = "Browsers".into();
                self.pane_mut().refilter();
                if !self.pane().recursive {
                    self.pane_mut().toggle_recursive();
                }
                run.phase = 2024;
            }
            // The footer must have identical geometry for empty search results too.
            2024 if ready && !self.pane().scanning => {
                let search = self.pane().benchmark_footer_search_rect();
                check(
                    run,
                    "expanded_search_keeps_ordinary_field_width",
                    (search.width() - run.saved_search.unwrap().width()).abs() < 0.75,
                );
                capture(ctx, run, "fix-refine-expanded-search", false);
                run.phase = 2025;
            }
            2025 => {
                if self.pane().recursive {
                    self.pane_mut().toggle_recursive();
                }
                self.pane_mut().filter.clear();
                self.pane_mut().refilter();
                self.open_launcher_options();
                run.phase = 2026;
            }
            2026 => {
                check(
                    run,
                    "launcher_settings_live_in_root_options",
                    self.options && !self.launcher_open && self.options_popup.owns_navigation(ctx),
                );
                capture(ctx, run, "fix-refine-launcher-options", false);
                run.phase = 2027;
            }
            2027 => {
                run.shortcut_before = Some(self.launcher.settings.hotkey.clone());
                self.options_popup.close();
                self.options = false;
                self.hotkeys_open = true;
                self.hotkeys.focus_editor();
                self.hotkeys.record(crate::commands::Command::Launcher);
                run.force_focus = true;
                let modifiers = Modifiers::CTRL | Modifiers::ALT | Modifiers::SHIFT;
                run.input.push(egui::Event::ModifiersChanged(modifiers));
                for pressed in [true, false] {
                    run.input.push(egui::Event::Key {
                        key: Key::F12,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
                run.input
                    .push(egui::Event::ModifiersChanged(Modifiers::NONE));
                run.phase = 2028;
            }
            2028 => {
                let label = self.hotkeys.recording_label();
                check(
                    run,
                    "shortcut_recorder_previews_real_key_chord",
                    self.hotkeys.is_recording() && label.as_deref() == Some("Ctrl+Alt+Shift+F12"),
                );
                capture(ctx, run, "fix-refine-shortcut-recording", false);
                run.phase = 20281;
            }
            20281 => {
                escape(run);
                run.phase = 2029;
            }
            2029 => {
                check(
                    run,
                    "cancel_shortcut_recording_preserves_working_binding",
                    !self.hotkeys.is_recording()
                        && run.shortcut_before.as_ref() == Some(&self.launcher.settings.hotkey),
                );
                self.options_popup.close();
                self.options = false;
                self.hotkeys_open = true;
                self.hotkeys.focus_editor();
                self.hotkeys.record(crate::commands::Command::Launcher);
                run.input.push(egui::Event::Key {
                    key: Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers::NONE,
                });
                run.phase = 2030;
            }
            2030 => {
                capture(ctx, run, "fix-refine-shortcut-inline-validation", false);
                run.phase = 20301;
            }
            20301 => {
                escape(run);
                run.phase = 2031;
            }
            2031 => {
                run.force_focus = false;
                self.options_popup.close();
                self.hotkeys.cancel_recording();
                self.hotkeys_open = false;
                run.phase = 2032;
            }
            2032 if !self.options => {
                self.open_launcher(false);
                run.phase = 2033;
            }
            2033 if !self.launcher.pending() => {
                capture(ctx, run, "fix-refine-launcher-default", true);
                run.phase = 20331;
            }
            20331 => {
                self.launcher.set_query("notpead");
                run.phase = 2034;
            }
            2034 if !self.launcher.pending()
                && self
                    .assets
                    .view_ready(crate::launcher::Launcher::icon_view_id())
                    .1
                    > 0 =>
            {
                check(
                    run,
                    "native_launcher_finds_installed_app_despite_typo",
                    self.launcher.rows.iter().any(|row| {
                        row.group == ResultGroup::Apps
                            && row.title.to_lowercase().contains("notepad")
                    }),
                );
                let icons = self
                    .assets
                    .view_ready(crate::launcher::Launcher::icon_view_id());
                check(
                    run,
                    "launcher_native_icons_are_loaded",
                    icons.0 > 0 && icons.1 > 0,
                );
                run.samples.push(serde_json::json!({"stage":"launcher-icons","requests":icons.0,"ready":icons.1}));
                capture(ctx, run, "fix-refine-launcher-app-icons", true);
                run.phase = 2035;
            }
            2035 => {
                self.launcher.set_query("(24 + 18) / 2");
                run.phase = 2036;
            }
            2036 if !self.launcher.pending() => {
                check(
                    run,
                    "automatic_calculator_keeps_copy_action",
                    self.launcher.rows.first().is_some_and(
                        |row| matches!(&row.action, ResultAction::Copy(value) if value == "21"),
                    ),
                );
                capture(ctx, run, "fix-refine-launcher-calculator", true);
                run.phase = 2037;
            }
            2037 => {
                run.compact = true;
                self.launcher.set_query("notepad");
                run.phase = 2038;
            }
            2038 if !self.launcher.pending() => {
                capture(ctx, run, "fix-refine-launcher-compact", true);
                run.phase = 20381;
            }
            20381 => {
                self.close_launcher(false);
                check(
                    run,
                    "launcher_hides_before_clear",
                    !owned_launcher_visible(),
                );
                run.compact = false;
                run.phase = 2039;
            }
            2039 => {
                let mut paths = std::fs::read_dir(scope.join("Home history"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| path.is_file())
                    .collect::<Vec<_>>();
                paths.sort();
                let mut folders = std::fs::read_dir(scope.join("Home locations"))
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .filter(|path| path.is_dir())
                    .collect::<Vec<_>>();
                folders.sort();
                self.recent.clear();
                for path in folders {
                    self.remember(path);
                }
                let now = crate::home_history::now();
                for (index, path) in paths.into_iter().enumerate() {
                    let at = now - index as i64 * 6 * 3600;
                    self.home_history.record(path.clone(), Access::Opened, at);
                    if index < 4 {
                        for _ in 0..4 - index {
                            self.home_history.record(path.clone(), Access::Opened, at);
                        }
                    }
                }
                self.home_history.normalize();
                self.home_usage_revision = self.home_usage_revision.wrapping_add(1);
                self.home.set_import_windows_recent(false);
                self.navigate_home();
                run.phase = 2040;
            }
            2040 if !self.home.pending()
                && self.home.rows.len() > 10
                && self.assets.view_ready(self.pane().id).1 > 0 =>
            {
                check(
                    run,
                    "home_has_extended_recent_history_and_sidebar_is_capped",
                    self.home.rows.len() > 10
                        && self.recent.len() == 10
                        && self.recent.iter().all(|path| path.is_dir()),
                );
                check(
                    run,
                    "home_is_virtual_and_has_no_hidden_folder_selection",
                    self.pane().is_home() && self.pane().primary_selected_path().is_none(),
                );
                self.check_refinement_home_geometry(ctx, run, "home-wide");
                capture(ctx, run, "fix-refine-home", false);
                run.phase = 20401;
            }
            20401 => {
                if let Some(focused) = ctx.memory(|memory| memory.focused()) {
                    ctx.memory_mut(|memory| memory.surrender_focus(focused));
                }
                self.pane_mut().home_selected = self
                    .home
                    .rows
                    .get(self.home.rows.len() - 2)
                    .map(|row| row.entry.path.clone());
                for pressed in [true, false] {
                    run.input.push(egui::Event::Key {
                        key: Key::ArrowDown,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    });
                }
                run.phase = 20402;
            }
            20402
                if ctx
                    .data(|data| {
                        data.get_temp::<home_ui::HomeProbe>(Id::new((self.pane().id, "home-probe")))
                    })
                    .is_some_and(|probe| probe.selected_visible) =>
            {
                check(
                    run,
                    "extended_home_recent_is_keyboard_reachable",
                    self.home.rows.last().is_some_and(|row| {
                        self.pane().home_selected.as_ref().is_some_and(|selected| {
                            crate::bookmarks::path_eq(selected, &row.entry.path)
                        })
                    }),
                );
                capture(ctx, run, "fix-refine-home-scrolled", false);
                run.phase = 20403;
            }
            20403 => {
                if let Some(id) = ctx
                    .data(|data| {
                        data.get_temp::<home_ui::HomeProbe>(Id::new((self.pane().id, "home-probe")))
                    })
                    .and_then(|probe| probe.scroll_id)
                {
                    egui::scroll_area::State::default().store(ctx, id);
                }
                self.pane_mut().home_selected = None;
                run.phase = 2041;
            }
            2041 => {
                self.action(Action::Navigate(scope.join("Design")));
                run.phase = 2042;
            }
            2042 if ready => {
                self.pane_mut().back();
                run.phase = 2043;
            }
            2043 if self.pane().is_home() => {
                check(run, "home_back_navigation_restores_virtual_route", true);
                self.pane_mut().forward();
                run.phase = 2044;
            }
            2044 if ready && !self.pane().is_home() => {
                check(
                    run,
                    "home_forward_navigation_restores_real_folder",
                    crate::bookmarks::path_eq(&self.pane().path, &scope.join("Design")),
                );
                self.pane_mut().back();
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(640.0, 540.0)));
                run.phase = 2045;
            }
            2045 if self.pane().is_home() && refinement_size_settled(ctx, 640.0, 540.0) => {
                self.check_refinement_home_geometry(ctx, run, "home-compact");
                capture(ctx, run, "fix-refine-home-compact", false);
                run.phase = 2046;
            }
            2046 => {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(1200.0, 800.0)));
                self.theme = Theme::paper();
                self.theme.apply(ctx);
                run.phase = 2047;
            }
            2047 if refinement_size_settled(ctx, 1200.0, 800.0) => {
                self.check_refinement_home_geometry(ctx, run, "home-paper");
                capture(ctx, run, "fix-refine-home-paper", false);
                run.phase = 2048;
            }
            2048 => {
                self.open_launcher(false);
                self.launcher.set_query("notepad");
                run.phase = 2049;
            }
            2049 if !self.launcher.pending() => {
                capture(ctx, run, "fix-refine-launcher-paper", true);
                run.phase = 20491;
            }
            20491 => {
                self.close_launcher(false);
                run.phase = 2050;
            }
            2050 => {
                self.theme = Theme::midnight();
                self.theme.font_size = 17.0;
                self.theme.apply(ctx);
                run.phase = 2051;
            }
            2051 => {
                self.check_refinement_home_geometry(ctx, run, "home-larger-text");
                capture(ctx, run, "fix-refine-home-larger-text", false);
                run.phase = 2052;
            }
            2052 => {
                run.finished = true;
                self.benchmark.as_mut().unwrap().expires = Instant::now() + Duration::from_secs(1);
            }
            _ => {}
        }
    }

    fn check_refinement_home_geometry(
        &self,
        ctx: &egui::Context,
        run: &mut FixesUiRun,
        label: &str,
    ) {
        let probe = ctx.data(|data| {
            data.get_temp::<home_ui::HomeProbe>(Id::new((self.pane().id, "home-probe")))
        });
        let valid = probe.as_ref().is_some_and(|probe| {
            let positive =
                |rect: Rect| rect.is_finite() && rect.width() > 0.0 && rect.height() > 0.0;
            let horizontal = |rect: Rect| {
                rect.left() >= probe.body.left() - 0.75 && rect.right() <= probe.body.right() + 0.75
            };
            probe.recent > 10
                && (1..=4).contains(&probe.recommended)
                && !probe.cards.is_empty()
                && !probe.rows.is_empty()
                && positive(probe.body)
                && positive(probe.address)
                && probe
                    .cards
                    .iter()
                    .all(|rect| positive(*rect) && horizontal(*rect))
                && probe.rows.iter().all(|rect| {
                    positive(*rect)
                        && horizontal(*rect)
                        && (rect.height() - self.theme.file_row_height()).abs() < 0.75
                })
                && probe.cards.windows(2).all(|pair| {
                    (pair[0].top() - pair[1].top()).abs() < 0.75
                        && pair[0].right() <= pair[1].left()
                })
                && probe.rows[0].intersects(probe.body)
                && probe.rows[0].top() >= probe.cards.iter().map(Rect::bottom).fold(0.0, f32::max)
        });
        check(run, &format!("{label}_layout_fits"), valid);
        if let Some(probe) = probe {
            let coordinates = |rect: &Rect| [rect.left(), rect.top(), rect.right(), rect.bottom()];
            run.samples.push(
                serde_json::json!({"stage":label,"recommended":probe.recommended,
                "recent":probe.recent,"body":coordinates(&probe.body),
                "viewport":[ctx.content_rect().width(),ctx.content_rect().height()],
                "cards":probe.cards.iter().map(coordinates).collect::<Vec<_>>(),
                "rows":probe.rows.iter().map(coordinates).collect::<Vec<_>>()}),
            );
        }
    }
}

fn capture(ctx: &egui::Context, run: &mut FixesUiRun, label: &str, launcher: bool) {
    let viewport = if launcher {
        egui::ViewportId::from_hash_of("foldex-focused-launcher")
    } else {
        egui::ViewportId::ROOT
    };
    ctx.send_viewport_cmd_to(
        viewport,
        egui::ViewportCommand::Screenshot(egui::UserData::new(label.to_owned())),
    );
    run.waiting_capture = true;
}

fn refinement_size_settled(ctx: &egui::Context, width: f32, height: f32) -> bool {
    // The custom native frame integrates the Windows caption into the client.
    // Winit's standard-frame sizing can therefore add its logical caption band.
    let actual = ctx.content_rect().size();
    (actual.x - width).abs() < 2.0 && actual.y >= height - 2.0 && actual.y <= height + 40.0
}

fn check(run: &mut FixesUiRun, name: &str, passed: bool) {
    run.checks
        .push(serde_json::json!({"check":name,"passed":passed}));
    if !passed {
        run.errors.push(format!("Check failed: {name}"));
    }
}

fn escape(run: &mut FixesUiRun) {
    run.input.push(egui::Event::Key {
        key: Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::NONE,
    });
}

fn click(run: &mut FixesUiRun, pos: egui::Pos2) {
    run.pointer = Some(pos);
    for pressed in [true, false] {
        run.input.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
}

#[cfg(windows)]
fn owned_launcher_visible() -> bool {
    crate::launcher::native::launcher_visible(&format!("Foldex Launcher · {}", std::process::id()))
        .unwrap_or(false)
}
#[cfg(not(windows))]
fn owned_launcher_visible() -> bool {
    false
}
