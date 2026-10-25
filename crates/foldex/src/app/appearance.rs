//! Shared visible/hidden lifecycle entry point for appearance events.
use super::*;

impl Foldex {
    /// Safe to call from both `logic` and `ui`: worker replies are drained once
    /// and resolving the already-current system mode is a constant-time no-op.
    pub(super) fn service_theme(&mut self, ctx: &egui::Context) {
        // run_logic does not update egui's theme input while hidden. The raw
        // hook caches winit's latest event before either lifecycle callback.
        let system_changed = self
            .theme
            .resolve_system(self.options_popup.system_theme(ctx));
        let library_changed = self.options_popup.service_themes(ctx, &mut self.theme);
        if system_changed || library_changed {
            self.options_popup.theme_palette_changed(&mut self.theme);
            self.theme.apply(ctx);
        }
        if library_changed {
            self.options_popup.persist_theme(ctx, &self.theme, false);
        }
    }
}
