
#[cfg(test)]
mod app_audit_probes {
    use super::*;

    #[test]
    fn app_audit_visible_filenames_are_absent_from_accessibility_tree() {
        let engine = Engine::with_options(Arc::new(|| {}), foldex_core::EngineOptions { native_watch:false, poll_interval:None, ..Default::default() }).unwrap();
        let mut pane = Pane::dormant(99_999, &engine, PathBuf::from("C:/audit"));
        let filename = "audit-accessible-document.txt";
        pane.accept(Arc::new(Snapshot::from_entries(pane.path.clone(), vec![foldex_core::Entry::from_name(filename, false, 0, 0)])));
        pane.loading = false;
        pane.selected.insert(0);
        pane.focused = Some(0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let theme = Theme {reduced_motion:true, ..Default::default()};
        theme.apply(&ctx);
        let body = Rect::from_min_size(Pos2::ZERO, vec2(900.0,600.0));
        let mut labels = Vec::new();
        let mut roles = Vec::new();
        let mut nodes = 0;
        for frame in 0..3 {
            let mut output = ctx.run_ui(egui::RawInput {screen_rect:Some(body), time:Some(frame as f64/60.0), focused:true, ..Default::default()}, |ui| {
                crate::keyboard::request_id(ui.ctx(), pane.listing_focus_id());
                pane.listing(ui, &theme, true, body, &columns::DetailsLayout::new(&columns::StoredColumns::default(),body,1.0,0.5),None,&mut vec![]);
            });
            output.textures_delta.clear();
            if let Some(update) = output.platform_output.accesskit_update {
                for (_,node) in update.nodes {
                    nodes += 1;
                    if let Some(label) = node.label() { labels.push(label.to_owned()); }
                    roles.push(format!("{:?}",node.role()));
                }
            }
        }
        println!("AUDIT {}",serde_json::json!({"probe":"listing_accessibility", "generated_nodes_over_three_frames":nodes, "labels":labels, "roles":roles, "visible_filename_exposed":labels.iter().any(|label| label.contains(filename))}));
        assert!(nodes>0);
        assert!(!labels.iter().any(|label|label.contains(filename)));
    }
}
