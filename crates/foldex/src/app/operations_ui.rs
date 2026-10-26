use super::*;
impl Foldex {
    pub(super) fn operations_ui(&mut self, ctx: &egui::Context) {
        if !self.operations_open {
            return;
        }
        let operations = self.platform.operations();
        let records = operations.snapshot();
        let mut open = true;
        foldex_ui::components::dialog(
            ctx,
            Id::new("file-operations"),
            &self.theme,
            "File operations",
            560.0 * self.theme.metrics().scale,
            &mut open,
            |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Clear completed").clicked() {
                        operations.clear_completed();
                    }
                    if ui
                        .add_enabled(
                            records.iter().any(|op| op.state().active()),
                            egui::Button::new("Cancel all"),
                        )
                        .clicked()
                    {
                        operations.cancel_all();
                    }
                });
                if records.is_empty() {
                    ui.label("No recent file operations in this window.");
                }
                egui::ScrollArea::vertical()
                    .max_height(420.0 * self.theme.metrics().scale)
                    .show(ui, |ui| {
                        for op in &records {
                            ui.push_id(op.id, |ui| {
                                ui.separator();
                                let state = op.state();
                                ui.horizontal(|ui| {
                                    ui.strong(op.label);
                                    ui.label(format!(
                                        "{} · {:.1}s",
                                        state.label(),
                                        op.elapsed().as_secs_f64()
                                    ));
                                    if state.active() {
                                        let cancelling =
                                            op.cancel.load(std::sync::atomic::Ordering::Acquire);
                                        if ui
                                            .add_enabled(
                                                !cancelling,
                                                egui::Button::new(if cancelling {
                                                    "Cancelling…"
                                                } else {
                                                    "Cancel"
                                                }),
                                            )
                                            .clicked()
                                        {
                                            op.request_cancel();
                                        }
                                    }
                                });
                                if !op.description.is_empty() {
                                    ui.add(egui::Label::new(&op.description).wrap());
                                }
                                if state == platform::operations::State::Running {
                                    if let Some(fraction) = op.fraction() {
                                        ui.add(egui::ProgressBar::new(fraction).show_percentage());
                                    } else {
                                        ui.label("Waiting for filesystem or Windows progress…");
                                    }
                                }
                                let message = op.message();
                                if !message.is_empty() {
                                    ui.add(egui::Label::new(message).wrap());
                                }
                            });
                        }
                    });
                if records.iter().any(|op| op.state().active()) {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            },
        );
        self.operations_open = open;
    }
}
