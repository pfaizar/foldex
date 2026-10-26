//! Small persisted view descriptions. No filesystem work occurs when applying them.
use crate::pane::Pane;
use foldex_core::{KindFilter, SortColumn};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewSettings {
    pub zoom: f32,
    pub name_fraction: f32,
    pub sort: u8,
    pub descending: bool,
    pub hidden: bool,
    pub kind: u8,
    pub columns: crate::pane::columns::StoredColumns,
    pub type_filter: Option<crate::pane::columns::StoredTypeFilter>,
    pub compact_paths: bool,
    pub breadcrumb_history: bool,
}
impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            zoom: 0.0,
            name_fraction: 0.45,
            sort: 0,
            descending: false,
            hidden: false,
            kind: 0,
            columns: Default::default(),
            type_filter: None,
            compact_paths: true,
            breadcrumb_history: true,
        }
    }
}
impl ViewSettings {
    pub fn capture(p: &Pane) -> Self {
        Self {
            zoom: p.zoom,
            name_fraction: p.column_name,
            sort: match p.query.sort {
                SortColumn::Name => 0,
                SortColumn::Kind => 1,
                SortColumn::Modified => 2,
                SortColumn::Size => 3,
                SortColumn::Created => 4,
                SortColumn::Accessed => 5,
            },
            descending: p.query.descending,
            hidden: p.query.show_hidden,
            kind: match p.query.kind {
                KindFilter::All => 0,
                KindFilter::Files => 1,
                KindFilter::Folders => 2,
                KindFilter::None => 3,
            },
            compact_paths: p.compact_paths,
            columns: p.stored_columns(),
            type_filter: p
                .query
                .type_filter
                .as_ref()
                .map(crate::pane::columns::StoredTypeFilter::capture),
            breadcrumb_history: p.breadcrumb_history,
        }
    }
    pub fn apply(&self, p: &mut Pane) {
        p.set_zoom(self.zoom);
        p.set_breadcrumb_options(self.compact_paths, self.breadcrumb_history);
        p.column_name = if self.name_fraction.is_finite() {
            self.name_fraction.clamp(0.15, 0.8)
        } else {
            0.45
        };
        p.query.sort = match self.sort {
            1 => SortColumn::Kind,
            2 => SortColumn::Modified,
            3 => SortColumn::Size,
            4 => SortColumn::Created,
            5 => SortColumn::Accessed,
            _ => SortColumn::Name,
        };
        p.query.descending = self.descending;
        p.restore_columns(self.columns.clone());
        p.query.type_filter = self
            .type_filter
            .as_ref()
            .map(crate::pane::columns::StoredTypeFilter::restore);
        p.query.show_hidden = self.hidden;
        p.query.kind = match self.kind {
            1 => KindFilter::Files,
            2 => KindFilter::Folders,
            3 => KindFilter::None,
            _ => KindFilter::All,
        };
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct PaneState {
    pub path: PathBuf,
    pub view: ViewSettings,
    #[serde(default)]
    pub route: Option<crate::navigation::Route>,
    #[serde(default)]
    pub navigation: Option<crate::navigation::Navigation>,
}
impl PaneState {
    pub fn capture(p: &Pane) -> Self {
        Self {
            path: p.path.clone(),
            view: ViewSettings::capture(p),
            route: Some(p.route().clone()),
            navigation: Some(p.navigation.clone()),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct TabState {
    pub left: PaneState,
    pub right: Option<PaneState>,
    pub vertical: bool,
    pub split: f32,
}
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceState {
    pub tree: Option<crate::workspace_tree::TreeState>,
    pub tabs: Vec<TabState>,
    pub active: usize,
    pub active_pane: usize,
    pub sidebar: bool,
    pub sidebar_width: f32,
    pub inspector: bool,
    pub inspector_width: f32,
}
impl WorkspaceState {
    /// A named layout describes destinations and presentation. Replaying the
    /// navigation history from when it was saved is unrelated to that layout.
    pub fn clear_navigation_history(&mut self) {
        for tab in &mut self.tabs {
            tab.left.navigation = None;
            if let Some(right) = &mut tab.right {
                right.navigation = None;
            }
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct NamedWorkspace {
    pub name: String,
    pub state: WorkspaceState,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SavedFolderView {
    pub path: PathBuf,
    pub view: ViewSettings,
}

pub fn find_folder_view<'a>(views: &'a [SavedFolderView], path: &Path) -> Option<&'a ViewSettings> {
    views.iter().find(|v| v.path == path).map(|v| &v.view)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn column_order_widths_and_type_exclusions_survive_view_serialization() {
        use crate::pane::columns::{Column, ColumnSetting, StoredTypeFilter};
        let mut view = ViewSettings::default();
        view.columns.columns = vec![
            ColumnSetting {
                column: Column::Files,
                width: 92.0,
                visible: true,
            },
            ColumnSetting {
                column: Column::Name,
                width: 280.0,
                visible: true,
            },
        ];
        view.columns.name_auto = false;
        view.type_filter = Some(StoredTypeFilter {
            include_folders: false,
            exclude: true,
            extensions: vec!["tmp".into(), "png".into()],
        });
        let restored: ViewSettings =
            serde_json::from_slice(&serde_json::to_vec(&view).unwrap()).unwrap();
        assert_eq!(restored.columns.columns[0].column, Column::Files);
        assert_eq!(restored.columns.columns[0].width, 92.0);
        assert!(!restored.columns.name_auto);
        let types = restored.type_filter.unwrap().restore();
        assert!(types.exclude && !types.include_folders);
        assert_eq!(types.extensions, ["png", "tmp"]);
    }
    #[test]
    fn workspace_preserves_split_paths_and_view_choices() {
        let pane = PaneState {
            path: PathBuf::from("D:/fixture"),
            route: None,
            navigation: None,
            view: ViewSettings {
                zoom: 0.65,
                hidden: true,
                descending: true,
                ..Default::default()
            },
        };
        let s = WorkspaceState {
            tabs: vec![TabState {
                left: pane.clone(),
                right: Some(pane),
                vertical: true,
                split: 0.37,
            }],
            active_pane: 1,
            inspector: true,
            ..Default::default()
        };
        let decoded: WorkspaceState =
            serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(decoded.tabs[0].right.as_ref().unwrap().view.zoom, 0.65);
        assert!(decoded.tabs[0].vertical && decoded.inspector);
        assert_eq!(decoded.active_pane, 1);
        assert_eq!(decoded.tabs[0].split, 0.37);
    }

    #[test]
    fn legacy_folder_sessions_migrate_and_home_route_round_trips() {
        let old = serde_json::json!({"path":"C:/old", "view":{}});
        let mut state: PaneState = serde_json::from_value(old).unwrap();
        assert!(state.route.is_none() && state.navigation.is_none());
        state.route = Some(crate::navigation::Route::Home);
        let mut navigation = crate::navigation::Navigation::new(crate::navigation::Route::Folder(
            state.path.clone(),
        ));
        navigation.visit(crate::navigation::Route::Home);
        state.navigation = Some(navigation);
        let restored: PaneState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(restored.route, Some(crate::navigation::Route::Home));
        assert!(restored.navigation.unwrap().can_back());
        assert_eq!(restored.path, PathBuf::from("C:/old"));
    }
}
