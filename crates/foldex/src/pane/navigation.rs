use super::*;
use crate::navigation::{Navigation, Route};

impl Pane {
    pub fn is_home(&self) -> bool {
        matches!(self.navigation.current(), Route::Home)
    }
    pub fn route(&self) -> &Route {
        self.navigation.current()
    }
    pub fn title(&self) -> String {
        if self.is_home() {
            "Home".into()
        } else if self.is_recycle_bin() {
            "Recycle Bin".into()
        } else if matches!(self.route(), Route::Search(_)) {
            format!("Search: {}", path_label(&self.path))
        } else {
            path_label(&self.path)
        }
    }
    pub fn location_text(&self) -> String {
        if self.is_home() {
            "foldex:home".into()
        } else if self.is_recycle_bin() {
            crate::navigation::RECYCLE_BIN_ADDRESS.into()
        } else {
            self.path.to_string_lossy().into_owned()
        }
    }
    pub fn navigate_route(&mut self, route: Route, remember: bool) {
        match route {
            Route::Home => self.navigate_home(remember),
            Route::RecycleBin => self.navigate_recycle_bin(remember),
            Route::Folder(path) => self.navigate(path, remember),
            Route::Search(location) => {
                if let Err(error) = location.validate() {
                    self.error = Some(crate::friendly::present("Couldn't start that search", &error));
                    return;
                }
                self.navigate(location.scope.clone(), false);
                if remember {
                    self.navigation.visit(Route::Search(location.clone()));
                }
                self.install_search(location);
                self.refilter();
                self.needs_refresh = self.search.is_none();
            }
        }
    }
    /// Restore a virtual route without ever scheduling its fallback folder.
    pub fn restore_navigation(&mut self, navigation: Navigation) {
        self.navigation = navigation.normalized(Route::Folder(self.path.clone()));
        match self.navigation.current().clone() {
            Route::Home => self.navigate_home(false),
            Route::RecycleBin => {
                let filter = self.filter.clone();
                let query = self.query.clone();
                self.navigate_recycle_bin(false);
                self.filter = filter;
                self.filter_lower = self.filter.to_lowercase();
                self.query = query;
            }
            Route::Search(location) => self.install_search(location),
            Route::Folder(path) => {
                self.path = path;
                self.address = self.path.to_string_lossy().into_owned();
            }
        }
    }
    fn install_search(&mut self, location: crate::navigation::SearchLocation) {
        self.path = location.scope.clone();
        self.address = self.path.to_string_lossy().into_owned();
        self.query = location.resolved_query();
        self.columns.enter_recursive(&mut self.query);
        self.filter = self.query.text.clone();
        self.filter_lower = self.filter.to_lowercase();
        self.browser.cancel_navigation();
        self.browser.cancel_filter();
        self.browser.set_active(false);
        self.recursive = true;
        self.directory_only = location.directory_only;
        self.needs_refresh = true;
        self.loading = true;
        if let Some(search) = &self.search {
            search.set_active(self.active);
        }
    }
    pub fn navigate_home(&mut self, remember: bool) {
        if remember {
            self.navigation.visit(Route::Home);
        }
        self.browser.cancel_navigation();
        self.browser.cancel_filter();
        self.columns.leave_recycle();
        self.recycle_model = None;
        self.recycle_requested = None;
        self.recycle_refresh_requested = false;
        self.recycle_busy = false;
        self.browser.cancel_scan();
        self.browser.set_active(false);
        if let Some(search) = &self.search {
            search.cancel();
            search.set_active(false);
        }
        self.reset_search();
        self.columns.cancel(self.id);
        self.columns.refresh_pending = false;
        self.memory_retry = None;
        self.recursive = false;
        self.loading = false;
        self.needs_refresh = false;
        self.resolving = false;
        self.scanning = false;
        self.nav_generation = 0;
        self.routing_revision = self.routing_revision.wrapping_add(1);
        self.query_generation = self.query_generation.wrapping_add(1);
        self.render_epoch = self.render_epoch.wrapping_add(1);
        self.visit_pending = false;
        self.home_selected = None;
        self.error = None;
        self.selected.clear();
        self.focused = None;
        self.anchor = None;
        self.last_selection = None;
        self.pending_selection = None;
        self.pending_action = None;
        self.deferred_action = None;
        self.select_all_pending = false;
        self.invert_pending = false;
        self.address = "foldex:home".into();
        self.address_mode = false;
        self.filter.clear();
        self.filter_lower.clear();
        self.query.text.clear();
        self.query.conditions = Default::default();
        self.breadcrumb_active = None;
        self.breadcrumb_anchor = None;
        self.breadcrumb_menu.menu.close();
        self.more_menu.close();
        self.zoom_open = false;
        self.marquee = None;
        self.focus_visual = None;
        self.row_cache.clear();
        self.name_cache.clear();
        self.listing_motion.clear();
        self.listing_clicks.clear();
        self.listing_snapshot = None;
        self.presented_entries.clear();
        self.presented_scratch.clear();
        self.listing_appearing = 0;
        self.listing_primed = false;
        if let Some(snapshot) = self.snapshot.take() {
            self.retire(snapshot);
        }
        if let Some(snapshot) = self.pending_snapshot.take() {
            self.retire(snapshot);
        }
        self.indices = Arc::from([]);
        self.kind_counts = None;
    }
    /// One usage event per successful user navigation; watcher refreshes don't
    /// inflate frequency. Restored hidden tabs don't count as viewed folders.
    pub fn take_folder_visit(&mut self) -> Option<PathBuf> {
        if self.is_virtual()
            || !self.active
            || !self.visit_pending
            || self.loading
            || self.recursive
            || self.error.is_some()
        {
            return None;
        }
        let snapshot = self.snapshot.as_ref()?;
        if !snapshot.complete || !crate::bookmarks::path_eq(&snapshot.path, &self.path) {
            return None;
        }
        self.visit_pending = false;
        Some(self.path.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn virtual_home_never_prepares_refreshes_or_searches_a_fallback_folder() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let mut pane = Pane::dormant(1, &engine, PathBuf::from("C:/not-a-home-folder"));
        pane.restore_navigation(Navigation::new(Route::Home));
        pane.set_active(true);
        pane.prepare_view(&egui::Context::default());
        pane.refresh();
        pane.toggle_recursive();
        pane.up();
        assert!(pane.is_home());
        assert!(!pane.loading && !pane.recursive && !pane.scanning);
        assert!(pane.snapshot.is_none());
        assert_eq!(pane.nav_generation, 0);
        assert!(pane.take_folder_visit().is_none());
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;
    #[test]
    fn search_route_restores_conditions_through_history_and_workspace() {
        let engine = Engine::new(Arc::new(|| {})).unwrap();
        let scope = std::env::temp_dir();
        let mut pane = Pane::dormant(88, &engine, scope.clone());
        let location = crate::navigation::SearchLocation {
            scope: scope.clone(),
            directory_only: false,
            query: Query {
                text: "report".into(),
                conditions: foldex_query::Conditions {
                    modified_within_days: Some(7),
                    min_bytes: Some(1024),
                    ..Default::default()
                },
                ..Default::default()
            },
        };
        pane.navigate_route(Route::Search(location.clone()), true);
        assert!(pane.recursive && !pane.is_virtual());
        assert!(pane.query.conditions.modified_after.is_some());
        assert!(pane.query.conditions.modified_within_days.is_none());
        pane.filter = "invoice".into();
        pane.refilter();
        let saved = pane.route().clone();
        pane.navigate(scope.join("next"), true);
        assert!(pane.query.conditions.is_empty());
        pane.back();
        assert_eq!(pane.route(), &saved);
        assert_eq!(pane.filter, "invoice");
        pane.back();
        assert!(matches!(pane.route(), Route::Folder(_)));
        pane.forward();
        assert_eq!(pane.route(), &saved);
        let state = crate::workspace::PaneState::capture(&pane);
        let restored: crate::workspace::PaneState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        let mut other = Pane::dormant(89, &engine, scope);
        other.restore_navigation(restored.navigation.unwrap());
        assert_eq!(other.route(), &saved);
        assert_eq!(other.filter, "invoice");
        assert_eq!(other.query.conditions.min_bytes, Some(1024));
        assert_eq!(other.nav_generation, 0);
    }
}
