
#[cfg(test)]
mod app_comparison_probes {
    use super::*;
    #[test]
    fn app_comparison_accessibility() {
        let engine = Engine::with_options(Arc::new(|| {}), foldex_core::EngineOptions { native_watch:false, poll_interval:None, ..Default::default() }).unwrap();
        let mut pane = Pane::dormant(99_999, &engine, PathBuf::from("C:/audit"));
        let filename = "audit-accessible-document.txt";
        pane.accept(Arc::new(Snapshot::from_entries(pane.path.clone(), vec![foldex_core::Entry::from_name(filename, false, 0, 0)])));
        pane.loading = false; pane.selected.insert(0); pane.focused = Some(0);
        let ctx = egui::Context::default(); ctx.enable_accesskit();
        let theme = Theme {reduced_motion:true, ..Default::default()}; theme.apply(&ctx);
        let body = Rect::from_min_size(Pos2::ZERO, vec2(900.0,600.0));
        let mut nodes = 0; let mut labels = Vec::new();
        for frame in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput {screen_rect:Some(body), time:Some(frame as f64/60.0), focused:true, ..Default::default()}, |ui| {
                crate::keyboard::request_id(ui.ctx(), pane.listing_focus_id());
                pane.listing(ui, &theme, true, body, &columns::DetailsLayout::new(&columns::StoredColumns::default(),body,1.0,0.5),None,&mut vec![]);
            });
            if let Some(update) = output.platform_output.accesskit_update.take() { for (_, node) in update.nodes { nodes += 1; if let Some(label) = node.label() { labels.push(label.to_owned()); } } }
            output.drop_without_applying_deltas();
        }
        println!("COMPARISON {}",serde_json::json!({"probe":"listing_accessibility", "nodes_over_three_frames":nodes, "filename_exposed":labels.iter().any(|label| label.contains(filename)), "labels":labels}));
    }
}
