
#[cfg(test)]
mod app_audit_probes {
    use super::*;

    #[test]
    fn app_audit_auxiliary_save_reverts_newer_shared_preferences() {
        let base = std::env::temp_dir().join(format!("foldex-audit-session-{}", std::process::id()));
        assert!(!base.exists());
        std::fs::create_dir(&base).unwrap();
        let path = base.join("foldex-session.json");
        let owner = serde_json::json!({"bookmarks":["C:/new-bookmark"], "hotkeys":["new-shortcut"], "launcher":{"background":false}, "workspace":{"active":2}});
        std::fs::write(&path, serde_json::to_vec(&owner).unwrap()).unwrap();
        let stale_aux = serde_json::json!({"bookmarks":["C:/old-bookmark"], "hotkeys":["old-shortcut"], "launcher":{"background":true}, "workspace":{"active":0}});
        save_request(&Request { revision:1, path:path.clone(), preserve_launcher:true, bytes:serde_json::to_vec(&stale_aux).unwrap() }).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        println!("AUDIT {}", serde_json::json!({"probe":"multiwindow_session", "newer_bookmark_survives":saved["bookmarks"] == owner["bookmarks"], "newer_shortcut_survives":saved["hotkeys"] == owner["hotkeys"], "owner_launcher_survives":saved["launcher"] == owner["launcher"], "owner_workspace_survives":saved["workspace"] == owner["workspace"]}));
        assert_eq!(saved["launcher"], owner["launcher"]);
        assert_eq!(saved["bookmarks"], stale_aux["bookmarks"]);
        assert_eq!(saved["hotkeys"], stale_aux["hotkeys"]);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn app_audit_valid_workspaces_exceed_the_session_writer_budget() {
        use crate::{app::Session, navigation::{Navigation, Route}, workspace::{NamedWorkspace, PaneState, TabState, WorkspaceState}};
        let mut workspace = WorkspaceState::default();
        for tab in 0..4 {
            let mut nav = Navigation::new(Route::Home);
            for n in 0..64 {
                nav.visit(Route::Folder(PathBuf::from(format!("C:/Users/Auditor/Documents/projects/{}/tab-{tab}/folder-{n:02}", "x".repeat(120)))));
            }
            let route = nav.current().clone();
            let Route::Folder(path) = route.clone() else { unreachable!() };
            workspace.tabs.push(TabState { left:PaneState {path, view:Default::default(), route:Some(route), navigation:Some(nav)}, right:None, vertical:false, split:0.5 });
        }
        let mut session = Session::default();
        session.workspace = Some(workspace.clone());
        for n in 0..3 {
            session.saved_layouts.push(NamedWorkspace {name:format!("Layout {n}"), state:workspace.clone()});
        }
        let bytes = serde_json::to_vec_pretty(&session).unwrap();
        let size = bytes.len();
        let writer = Persistence::new(Arc::new(|| {}), false).unwrap();
        let error = writer.save(std::env::temp_dir().join("foldex-audit-no-write.json"), bytes).unwrap_err();
        println!("AUDIT {}", serde_json::json!({"probe":"session_budget", "tabs":4, "navigation_entries_per_tab":64, "saved_layouts":3, "serialized_bytes":size, "writer_budget":256*1024, "error":error}));
        assert!(size > 256*1024);
    }
}
