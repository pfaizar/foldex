use super::*;
impl Foldex {
    /// Inert records in every state; no job reaches the platform worker.
    #[cfg(test)]
    pub(super) fn catalog_operations(&mut self) {
        let operations = self.platform.operations();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let copy = |count: usize, destination: &str| Job::Copy {
            sources: (0..count)
                .map(|i| PathBuf::from(format!("C:/Photos/{i}.jpg")))
                .collect(),
            destination: PathBuf::from(destination),
            cut: false,
        };
        let jobs = [
            (
                copy(3, "D:/Backups/Photos"),
                Some(Ok("Copied 3 items".to_owned())),
            ),
            (
                Job::Recycle(vec![PathBuf::from("C:/Projects/Atlas/build.log")]),
                Some(Err("Access is denied. (os error 5)".to_owned())),
            ),
            (
                Job::Copy {
                    sources: vec![PathBuf::from("C:/Videos/Trip.mp4")],
                    destination: PathBuf::from("E:/Archive"),
                    cut: true,
                },
                Some(Err("cancelled".to_owned())),
            ),
            (copy(240, "E:/Archive/2026"), None),
            (copy(12, "D:/Shared/Exports"), None),
        ];
        for (index, (job, result)) in jobs.into_iter().enumerate() {
            let Ok(Some(op)) = operations.track(&job, wake.clone()) else {
                continue;
            };
            match result {
                Some(result) => {
                    op.start();
                    if index == 2 {
                        op.request_cancel();
                    }
                    op.finish(&result.map_err(std::io::Error::other));
                }
                None if index == 3 => {
                    op.start();
                    op.progress(100, 62);
                }
                None => {}
            }
        }
    }
    pub(super) fn operations_ui(&mut self, ctx: &egui::Context) {
        use foldex_ui::dialog::{Action, Dialog, Footer, Kind, Size};
        if !self.operations_open {
            return;
        }
        let operations = self.platform.operations();
        let records = operations.snapshot();
        let t = &self.theme;
        let id = Id::new("file-operations");
        let active = records.iter().filter(|op| op.state().active()).count();
        let finished = records.len() - active;
        let summary = match (active, finished) {
            (0, 0) => String::new(),
            (0, n) => format!("{n} finished"),
            (n, 0) => format!("{n} in progress"),
            (a, f) => format!("{a} in progress · {f} finished"),
        };
        let mut dialog =
            Dialog::new(id, "File operations", Size::Medium.width(t)).subtitle(&summary);
        if !records.is_empty() {
            let mut actions = vec![
                Action::new(id.with("clear"), "Clear finished", Kind::Secondary)
                    .enabled(finished > 0),
            ];
            if active > 0 {
                actions.push(Action::new(
                    id.with("cancel-all"),
                    "Cancel all",
                    Kind::Secondary,
                ));
            }
            dialog = dialog.footer(Footer::new(actions));
        }
        let mut open = true;
        let shown = dialog.show(ctx, t, &mut open, |ui| {
            if records.is_empty() {
                foldex_ui::dialog::empty_state(
                    ui,
                    t,
                    Icon::Copy,
                    "No file operations",
                    "Copies, moves and deletions you start in this window show up here.",
                    None,
                );
                return;
            }
            ui.spacing_mut().item_spacing.y = 0.0;
            for (index, op) in records.iter().enumerate() {
                operation_row(ui, t, id.with(op.id), op, index > 0);
            }
        });
        if shown.clicked == Some(0) {
            operations.clear_completed();
        } else if shown.clicked == Some(1) {
            operations.cancel_all();
        }
        if active > 0 {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        self.operations_open = open;
    }
}

/// One operation: status, verb and subject, then where it goes or why it
/// stopped, and a thin bar while it runs.
fn operation_row(
    ui: &mut Ui,
    t: &Theme,
    id: Id,
    op: &platform::operations::Operation,
    divider: bool,
) {
    use foldex_ui::dialog::{Glyph, Text, glyph, progress_at, spinner, text_at};
    use platform::operations::State;
    let m = t.metrics();
    let s = m.scale;
    let state = op.state();
    let cancelling = op.cancel.load(std::sync::atomic::Ordering::Acquire);
    let danger = Theme::color(t.danger);
    let message = op.message();
    let reason = match state {
        State::Failed | State::Partial if !message.is_empty() => {
            let what = format!("{} didn't complete", op.label);
            let text = crate::friendly::present(&what, &message);
            Some(
                text.strip_prefix(&format!("{what}. "))
                    .map(str::to_owned)
                    .unwrap_or(text),
            )
        }
        _ => None,
    };
    let seconds = op.elapsed().as_secs_f64();
    let meta = match state {
        State::Queued if cancelling => "Cancelling…".to_owned(),
        State::Queued => "Waiting".to_owned(),
        State::Running if cancelling => "Cancelling…".to_owned(),
        State::Running => op
            .fraction()
            .map_or("Starting…".to_owned(), |f| format!("{:.0}%", f * 100.0)),
        State::Complete if seconds >= 1.0 => format!("Done in {seconds:.0} s"),
        State::Complete => "Done".to_owned(),
        State::Partial => "Finished with issues".to_owned(),
        State::Failed => "Failed".to_owned(),
        State::Cancelled => "Cancelled".to_owned(),
    };
    let detail = reason.clone().unwrap_or_else(|| op.target.clone());
    let pad = 10.0 * s;
    let title_h = m.control;
    let detail_h = if detail.is_empty() { 0.0 } else { 18.0 * s };
    let bar_h = if state == State::Running {
        10.0 * s
    } else {
        0.0
    };
    let height = pad * 2.0 + title_h + detail_h + bar_h;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    if divider {
        ui.painter().hline(
            rect.x_range(),
            rect.top(),
            Stroke::new(1.0, t.line().gamma_multiply(0.7)),
        );
    }
    let glyph_size = 16.0 * s;
    let line = Rect::from_min_size(
        pos2(rect.left(), rect.top() + pad),
        vec2(rect.width(), title_h),
    );
    let glyph_center = pos2(rect.left() + glyph_size * 0.5, line.center().y);
    match state {
        State::Running => spinner(ui, glyph_center, glyph_size, t.accent()),
        State::Queued => glyph(ui, glyph_center, glyph_size, Glyph::Waiting, t.dim()),
        State::Complete => glyph(ui, glyph_center, glyph_size, Glyph::Done, t.dim()),
        State::Partial | State::Failed => glyph(ui, glyph_center, glyph_size, Glyph::Error, danger),
        State::Cancelled => glyph(ui, glyph_center, glyph_size, Glyph::Stopped, t.dim()),
    }
    let text_left = rect.left() + glyph_size + 12.0 * s;
    // Every row reserves the cancel column so status text lines up.
    let button = Rect::from_min_max(
        pos2(rect.right() - m.control, line.top()),
        pos2(rect.right(), line.bottom()),
    );
    if state.active()
        && !cancelling
        && foldex_ui::components::icon_button_at(
            ui,
            button,
            id.with("cancel"),
            t,
            Icon::Close,
            false,
        )
        .on_hover_text("Cancel")
        .clicked()
    {
        op.request_cancel();
    }
    let right = button.left() - 4.0 * s;
    let meta_width = foldex_ui::dialog::text_width(ui, t, Text::Caption, &meta);
    let meta_rect = Rect::from_min_max(
        pos2(right - meta_width, line.top()),
        pos2(right, line.bottom()),
    );
    text_at(
        ui,
        meta_rect,
        t,
        Text::Caption,
        &meta,
        if matches!(state, State::Failed | State::Partial) {
            danger
        } else {
            t.dim()
        },
        Align2::RIGHT_CENTER,
    );
    let title = format!("{} {}", op.label, op.subject);
    text_at(
        ui,
        Rect::from_min_max(
            pos2(text_left, line.top()),
            pos2(meta_rect.left() - 12.0 * s, line.bottom()),
        ),
        t,
        Text::Body,
        &title,
        t.fg(),
        Align2::LEFT_CENTER,
    );
    if !detail.is_empty() {
        let detail_rect = Rect::from_min_size(
            pos2(text_left, line.bottom() - 4.0 * s),
            vec2(rect.right() - text_left, detail_h),
        );
        text_at(
            ui,
            detail_rect,
            t,
            Text::Secondary,
            &detail,
            if reason.is_some() { danger } else { t.dim() },
            Align2::LEFT_CENTER,
        );
        ui.interact(detail_rect, id.with("detail"), Sense::hover())
            .on_hover_text(&detail);
    }
    if state == State::Running {
        let bar = Rect::from_min_size(
            pos2(text_left, rect.bottom() - pad - 4.0 * s),
            vec2(rect.right() - text_left, 4.0 * s),
        );
        progress_at(ui, bar, t, op.fraction());
    }
}
