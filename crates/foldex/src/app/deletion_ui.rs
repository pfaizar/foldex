//! Explicit delete intent and the permanent-deletion confirmation boundary.
use super::*;
use crate::pane::DeleteMode;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Decision {
    Confirm,
    Cancel,
}

struct Confirmation {
    id: Id,
    paths: Vec<PathBuf>,
    origin: u64,
    navigation: u64,
    decision: Option<Decision>,
    focus_cancel: bool,
}

#[derive(Default)]
pub(super) struct Deletion {
    pending: Option<Confirmation>,
    next_request: u64,
    // Tests intercept at the native-job boundary, before any filesystem work.
    #[cfg(test)]
    captured_jobs: Option<Vec<Job>>,
}

impl Deletion {
    pub(super) fn is_open(&self) -> bool {
        self.pending.is_some()
    }
}

impl Foldex {
    pub(super) fn request_delete(&mut self, paths: Vec<PathBuf>, mode: DeleteMode) {
        // Keep a modal's frozen selection and decision intact, including while
        // it fades out. Delayed selection results cannot replace its request.
        if paths.is_empty()
            || self.deletion.is_open()
            || crate::keyboard::composing(&self.ui_context)
        {
            return;
        }
        match mode {
            DeleteMode::Recycle => self.submit_delete(paths, mode),
            DeleteMode::Permanent => {
                self.deletion.next_request = self.deletion.next_request.wrapping_add(1);
                self.deletion.pending = Some(Confirmation {
                    // Alternate two scopes so adjacent requests cannot inherit
                    // closing/focus state, without growing egui's modal caches.
                    id: Id::new(("permanent-delete-dialog", self.deletion.next_request & 1)),
                    paths,
                    origin: self.pane().id,
                    navigation: self.pane().navigation_epoch(),
                    decision: None,
                    focus_cancel: true,
                });
                self.ui_context.request_repaint();
            }
        }
    }

    fn submit_delete(&mut self, paths: Vec<PathBuf>, mode: DeleteMode) {
        let job = match mode {
            DeleteMode::Recycle => Job::Recycle(paths),
            DeleteMode::Permanent => Job::DeletePermanent(paths),
        };
        #[cfg(test)]
        if let Some(jobs) = &mut self.deletion.captured_jobs {
            jobs.push(job);
            return;
        }
        self.job(job);
    }

    pub(super) fn delete_confirmation_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = &mut self.deletion.pending else {
            return;
        };
        let origin_present = self.tabs.iter().any(|tab| {
            std::iter::once(&tab.left)
                .chain(tab.right.iter())
                .any(|pane| {
                    pane.id == pending.origin && pane.navigation_epoch() == pending.navigation
                })
        });
        let composing = crate::keyboard::composing(ctx);
        if !origin_present
            || (!composing && crate::commands::take_key(ctx, Modifiers::NONE, Key::Escape))
        {
            // Cancellation wins even if a confirm event was batched with Esc.
            pending.decision = Some(Decision::Cancel);
        }
        let mut open = true;
        let t = &self.theme;
        let scale = t.metrics().scale;
        let id = pending.id;
        let cancel_id = id.with("cancel");
        let confirm_id = id.with("confirm");
        foldex_ui::components::dialog_interactive(
            ctx,
            id,
            t,
            "Delete permanently",
            460.0 * scale,
            &mut open,
            pending.decision.is_none(),
            |ui| {
                ui.label(format!(
                    "Permanently delete {} item{}?",
                    pending.paths.len(),
                    if pending.paths.len() == 1 { "" } else { "s" }
                ));
                ui.label(
                    egui::RichText::new(
                        "These items will be deleted permanently. This cannot be undone.",
                    )
                    .color(t.dim()),
                );
                ui.add_space(4.0 * scale);
                for (index, path) in pending.paths.iter().take(4).enumerate() {
                    let (rect, _) = ui.allocate_exact_size(
                        vec2(ui.available_width(), t.font_size + 7.0 * scale),
                        Sense::hover(),
                    );
                    foldex_ui::components::elided_label(
                        ui.painter(),
                        rect,
                        t,
                        &path_label(path),
                        t.dim(),
                        false,
                    );
                    ui.interact(rect, id.with(("path", index)), Sense::hover())
                        .on_hover_text(path.to_string_lossy());
                }
                if pending.paths.len() > 4 {
                    ui.label(
                        egui::RichText::new(format!("and {} more", pending.paths.len() - 4))
                            .color(t.dim()),
                    );
                }
                ui.add_space(8.0 * scale);
                ui.horizontal_wrapped(|ui| {
                    if delete_button(ui, t, cancel_id, "Cancel").clicked() {
                        pending.decision = Some(Decision::Cancel);
                    }
                    if delete_button(ui, t, confirm_id, "Delete permanently").clicked()
                        && pending.decision.is_none()
                        && !composing
                    {
                        pending.decision = Some(Decision::Confirm);
                    }
                });
            },
        );
        if pending.focus_cancel
            && pending.decision.is_none()
            && ctx
                .read_response(cancel_id)
                .is_some_and(|response| response.enabled() && response.interact_rect.is_positive())
        {
            ctx.memory_mut(|memory| memory.request_focus(cancel_id));
            pending.focus_cancel = false;
        }
        if pending.decision.is_some() {
            foldex_ui::components::close_dialog(ctx, id);
        }
        if !open {
            let finished = self.deletion.pending.take().unwrap();
            if finished.decision == Some(Decision::Confirm) && origin_present {
                self.submit_delete(finished.paths, DeleteMode::Permanent);
            }
        }
    }
}

fn delete_button(ui: &mut Ui, t: &Theme, id: Id, label: &str) -> egui::Response {
    let m = t.metrics();
    let width = (ui
        .painter()
        .layout_no_wrap(label.to_owned(), t.font(), t.fg())
        .size()
        .x
        + 16.0 * m.scale)
        .max(m.control)
        .min(ui.max_rect().width().max(m.control));
    let (_, rect) = ui.allocate_space(vec2(width, m.control));
    foldex_ui::components::text_button_at(ui, rect, id, t, label, false)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{key_event, pump_events, ready_fixture};
    use super::*;
    use crate::commands::Command;

    fn fixture() -> (Foldex, egui::Context, usize, PathBuf) {
        let (mut app, ctx, frame) = ready_fixture();
        app.theme.reduced_motion = true;
        app.theme.apply(&ctx);
        app.deletion.captured_jobs = Some(Vec::new());
        let path = app.pane().primary_selected_path().unwrap();
        crate::keyboard::request_id(&ctx, crate::keyboard::listing_id(app.pane().id));
        (app, ctx, frame, path)
    }

    fn press(key: Key, modifiers: Modifiers) -> Vec<egui::Event> {
        let mut release = key_event(key, modifiers);
        if let egui::Event::Key { pressed, .. } = &mut release {
            *pressed = false;
        }
        vec![key_event(key, modifiers), release]
    }

    fn frame(app: &mut Foldex, ctx: &egui::Context, at: &mut usize, events: Vec<egui::Event>) {
        pump_events(app, ctx, *at, events);
        *at += 1;
    }

    fn settle(app: &mut Foldex, ctx: &egui::Context, at: &mut usize) {
        for _ in 0..4 {
            frame(app, ctx, at, vec![]);
        }
    }

    fn jobs(app: &Foldex) -> &[Job] {
        app.deletion.captured_jobs.as_ref().unwrap()
    }

    fn click(position: egui::Pos2) -> Vec<egui::Event> {
        let button = |pressed| egui::Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        vec![
            egui::Event::PointerMoved(position),
            button(true),
            button(false),
        ]
    }

    #[test]
    fn raw_delete_recycles_immediately_and_shift_delete_waits_for_explicit_confirmation() {
        let (mut app, ctx, mut at, path) = fixture();
        frame(&mut app, &ctx, &mut at, press(Key::Delete, Modifiers::NONE));
        assert!(!app.deletion.is_open());
        assert!(matches!(jobs(&app), [Job::Recycle(paths)] if paths == &[path.clone()]));

        let mut shifted = press(Key::Delete, Modifiers::SHIFT);
        // Native input may pair Shift+Delete with the editor Cut alias.
        shifted.insert(1, egui::Event::Cut);
        frame(&mut app, &ctx, &mut at, shifted);
        assert!(app.deletion.is_open());
        assert_eq!(jobs(&app).len(), 1);
        settle(&mut app, &ctx, &mut at);
        let pending = app.deletion.pending.as_ref().unwrap();
        assert_eq!(pending.paths, [path.clone()]);
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(pending.id.with("cancel"))
        );

        let confirm = pending.id.with("confirm");
        ctx.memory_mut(|memory| memory.request_focus(confirm));
        frame(&mut app, &ctx, &mut at, press(Key::Enter, Modifiers::NONE));
        settle(&mut app, &ctx, &mut at);
        assert!(!app.deletion.is_open());
        assert!(
            matches!(jobs(&app), [Job::Recycle(first), Job::DeletePermanent(second)]
            if first == &[path.clone()] && second == &[path.clone()])
        );
        settle(&mut app, &ctx, &mut at);
        assert_eq!(jobs(&app).len(), 2, "a closing dialog cannot submit twice");
        assert!(
            path.exists(),
            "captured delete jobs never reach the native worker"
        );
    }

    #[test]
    fn raw_permanent_delete_cancellation_never_submits_or_reuses_a_stale_modal() {
        let (mut app, ctx, mut at, path) = fixture();
        frame(
            &mut app,
            &ctx,
            &mut at,
            press(Key::Delete, Modifiers::SHIFT),
        );
        settle(&mut app, &ctx, &mut at);
        let old_id = app.deletion.pending.as_ref().unwrap().id;
        let confirm_position = ctx
            .read_response(old_id.with("confirm"))
            .unwrap()
            .rect
            .center();
        let mut cancelled = click(confirm_position);
        cancelled.extend(press(Key::Escape, Modifiers::NONE));
        frame(&mut app, &ctx, &mut at, cancelled);
        // Even an in-flight selection action cannot replace the cancelled request.
        app.request_delete(vec![path.clone()], DeleteMode::Recycle);
        settle(&mut app, &ctx, &mut at);
        assert!(!app.deletion.is_open());
        assert!(jobs(&app).is_empty());

        frame(
            &mut app,
            &ctx,
            &mut at,
            press(Key::Delete, Modifiers::SHIFT),
        );
        settle(&mut app, &ctx, &mut at);
        let new_id = app.deletion.pending.as_ref().unwrap().id;
        assert_ne!(new_id, old_id);
        assert_eq!(app.deletion.pending.as_ref().unwrap().decision, None);
        let cancel_position = ctx
            .read_response(new_id.with("cancel"))
            .unwrap()
            .rect
            .center();
        frame(&mut app, &ctx, &mut at, click(cancel_position));
        settle(&mut app, &ctx, &mut at);
        assert!(!app.deletion.is_open());
        assert!(jobs(&app).is_empty());
        assert!(path.exists());
    }

    #[test]
    fn raw_permanent_delete_defaults_to_cancel_and_does_not_accept_the_invoking_enter() {
        let (mut app, ctx, mut at, path) = fixture();
        let mut batch = press(Key::Delete, Modifiers::SHIFT);
        batch.extend(press(Key::Enter, Modifiers::NONE));
        frame(&mut app, &ctx, &mut at, batch);
        settle(&mut app, &ctx, &mut at);
        assert_eq!(app.deletion.pending.as_ref().unwrap().decision, None);
        assert!(jobs(&app).is_empty());
        frame(&mut app, &ctx, &mut at, press(Key::Enter, Modifiers::NONE));
        settle(&mut app, &ctx, &mut at);
        assert!(
            !app.deletion.is_open(),
            "Enter activates the initially focused Cancel"
        );
        assert!(jobs(&app).is_empty());
        assert!(path.exists());
    }

    #[test]
    fn raw_delete_preserves_editor_ime_and_modal_ownership() {
        let (mut app, ctx, mut at, path) = fixture();
        frame(&mut app, &ctx, &mut at, press(Key::F, Modifiers::CTRL));
        frame(&mut app, &ctx, &mut at, press(Key::Delete, Modifiers::NONE));
        frame(
            &mut app,
            &ctx,
            &mut at,
            press(Key::Delete, Modifiers::SHIFT),
        );
        assert!(!app.deletion.is_open());
        assert!(jobs(&app).is_empty());

        crate::keyboard::request_id(&ctx, crate::keyboard::listing_id(app.pane().id));
        let mut composing = vec![egui::Event::Ime(egui::ImeEvent::Preedit {
            text: "候補".into(),
            active_range_chars: None,
        })];
        composing.extend(press(Key::Delete, Modifiers::SHIFT));
        frame(&mut app, &ctx, &mut at, composing);
        assert!(!app.deletion.is_open());
        assert!(jobs(&app).is_empty());
        frame(
            &mut app,
            &ctx,
            &mut at,
            vec![egui::Event::Ime(egui::ImeEvent::Commit(String::new()))],
        );
        app.execute_command(&ctx, Command::KeyboardHelp);
        frame(&mut app, &ctx, &mut at, press(Key::Delete, Modifiers::NONE));
        frame(
            &mut app,
            &ctx,
            &mut at,
            press(Key::Delete, Modifiers::SHIFT),
        );
        assert!(app.keyboard_help);
        assert!(!app.deletion.is_open());
        assert!(jobs(&app).is_empty());
        assert!(path.exists());
    }

    #[test]
    fn permanent_delete_freezes_its_paths_and_cancels_if_the_source_panel_navigates() {
        let (mut app, ctx, mut at, path) = fixture();
        app.request_delete(vec![path.clone()], DeleteMode::Permanent);
        let id = app.deletion.pending.as_ref().unwrap().id;
        app.request_delete(vec![path.with_extension("other")], DeleteMode::Permanent);
        assert_eq!(app.deletion.pending.as_ref().unwrap().id, id);
        assert_eq!(app.deletion.pending.as_ref().unwrap().paths, [path.clone()]);
        app.pane_mut().navigate_home(true);
        settle(&mut app, &ctx, &mut at);
        assert!(!app.deletion.is_open());
        assert!(jobs(&app).is_empty());
        assert!(path.exists());
    }
}
