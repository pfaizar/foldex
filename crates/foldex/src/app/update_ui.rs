use super::*;

impl Foldex {
    pub(super) fn can_install_update(&self) -> bool {
        !self.platform.update_busy()
            && self.batch.is_none()
            && self.edit.is_none()
            && !self.window_transfer.busy()
            && !self.deletion.is_open()
            && !self.recycle.is_open()
            && !self.context.is_open()
            && self.close_requested.is_none()
    }

    pub(super) fn service_updates(&mut self, ctx: &egui::Context) {
        self.updates.pump(ctx);
        if self.updates.prepared && !self.updates.exit_pending {
            if self.can_install_update() {
                self.updates.begin_exit();
                self.exit_for_update(ctx);
            } else {
                self.updates.cancel_exit(
                    "Finish file operations and editing dialogs before installing this update.",
                );
            }
        }
    }

    fn exit_for_update(&mut self, ctx: &egui::Context) {
        self.close_launcher(false);
        self.options_popup.persist_theme(ctx, &self.theme, true);
        self.close_failed = false;
        self.close_requested = Some(Instant::now());
        self.session_dirty = true;
        self.save_due = Some(Instant::now());
        ctx.request_repaint_of(egui::ViewportId::ROOT);
    }

    pub(super) fn cancel_update_exit(&mut self, ctx: &egui::Context, message: &str) {
        self.close_requested = None;
        self.close_failed = false;
        self.allow_close = false;
        self.updates.cancel_exit(message);
        self.show_explorer(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_during_update_save_cancels_installation_instead_of_forcing_exit() {
        let (mut app, ctx, _) = crate::app::tests::fixture_app();
        app.updates.exit_pending = true;
        app.close_requested = Some(Instant::now() - Duration::from_secs(5));
        app.request_explorer_close(&ctx);
        assert!(!app.allow_close);
        assert!(app.close_requested.is_none());
        assert!(!app.updates.exit_pending);
        assert!(app.updates.open);
    }

    #[test]
    fn failed_session_save_aborts_update_and_keeps_the_app_open() {
        let (mut app, ctx, root) = crate::app::tests::fixture_app();
        let blocker = root.join("not-a-directory");
        std::fs::write(&blocker, b"owned fixture").unwrap();
        app.session_path = blocker.join("session.json");
        app.updates.exit_pending = true;
        app.close_requested = Some(Instant::now());
        app.session_dirty = true;
        app.save_due = Some(Instant::now());
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.updates.exit_pending {
            app.service_session(&ctx);
            assert!(
                Instant::now() < deadline,
                "the failed save did not cancel the update"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!app.allow_close);
        assert!(app.close_requested.is_none());
        assert!(app.updates.open);
    }
}
