//! Virtual locations never become filesystem paths or browser requests.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_NAVIGATION: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum Route {
    Home,
    RecycleBin,
    Folder(PathBuf),
    Search(SearchLocation),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SearchLocation {
    pub scope: PathBuf,
    pub query: foldex_core::Query,
    pub directory_only: bool,
}
impl SearchLocation {
    pub fn validate(&self) -> Result<(), String> {
        if !self.scope.is_absolute()
            || self.scope.as_os_str().len() > 128 * 1024
            || self.scope.as_os_str().as_encoded_bytes().contains(&0)
            || self.query.text.len() > 1024
        {
            return Err(
                "Search needs a full folder path and a shorter search text".into(),
            );
        }
        self.query.conditions.validate()
    }
    pub fn resolved_query(&self) -> foldex_core::Query {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64) as i64;
        let mut query = self.query.clone();
        query.conditions = query.conditions.resolved(now);
        query
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SavedSearch {
    pub name: String,
    pub location: SearchLocation,
}

pub fn normalize_searches(searches: Vec<SavedSearch>) -> Vec<SavedSearch> {
    let mut valid = Vec::new();
    for search in searches.into_iter().take(32) {
        if !search.name.trim().is_empty()
            && search.name.len() <= 96
            && search.location.validate().is_ok()
            && !valid.iter().any(|s: &SavedSearch| s.name == search.name)
        {
            valid.push(search);
        }
    }
    valid
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Navigation {
    entries: Vec<Route>,
    at: usize,
}

impl Navigation {
    pub fn new(route: Route) -> Self {
        Self {
            entries: vec![route],
            at: 0,
        }
    }
    pub fn current(&self) -> &Route {
        &self.entries[self.at]
    }
    pub fn can_back(&self) -> bool {
        self.at > 0
    }
    pub fn can_forward(&self) -> bool {
        self.at + 1 < self.entries.len()
    }
    pub fn visit(&mut self, route: Route) {
        if self.current() == &route {
            return;
        }
        self.entries.truncate(self.at + 1);
        self.entries.push(route);
        if self.entries.len() > MAX_NAVIGATION {
            self.entries.remove(0);
        }
        self.at = self.entries.len() - 1;
    }
    pub fn replace_current(&mut self, route: Route) {
        self.entries[self.at] = route;
    }
    pub fn back(&mut self) -> Option<Route> {
        if !self.can_back() {
            return None;
        }
        self.at -= 1;
        Some(self.current().clone())
    }
    pub fn forward(&mut self) -> Option<Route> {
        if !self.can_forward() {
            return None;
        }
        self.at += 1;
        Some(self.current().clone())
    }
    /// Serialized state is untrusted. Keep the selected route while trimming.
    pub fn normalized(mut self, fallback: Route) -> Self {
        if self.entries.is_empty() {
            return Self::new(fallback);
        }
        self.at = self.at.min(self.entries.len() - 1);
        let start = self.at.saturating_sub(MAX_NAVIGATION - 1);
        let end = (start + MAX_NAVIGATION).min(self.entries.len());
        self.entries = self.entries.drain(start..end).collect();
        self.at -= start;
        if self.entries.iter().any(|route| {
            matches!(route, Route::Folder(path) if path.as_os_str().is_empty())
                || matches!(route, Route::Search(search) if search.validate().is_err())
        }) {
            return Self::new(fallback);
        }
        self
    }
}

pub fn is_home_address(text: &Path) -> bool {
    text.to_str().is_some_and(|text| {
        text.eq_ignore_ascii_case("foldex:home") || text.eq_ignore_ascii_case("foldex://home")
    })
}

pub const RECYCLE_BIN_ADDRESS: &str = "foldex:recycle-bin";

pub fn is_recycle_bin_address(text: &Path) -> bool {
    text.to_str().is_some_and(|text| {
        text.eq_ignore_ascii_case(RECYCLE_BIN_ADDRESS)
            || text.eq_ignore_ascii_case("foldex://recycle-bin")
    })
}

pub fn special_route(text: &Path) -> Option<Route> {
    if is_home_address(text) {
        Some(Route::Home)
    } else if is_recycle_bin_address(text) {
        Some(Route::RecycleBin)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recycle_bin_is_a_serializable_route_and_never_a_folder_address() {
        let mut nav = Navigation::new(Route::Home);
        nav.visit(Route::RecycleBin);
        nav.visit(Route::Folder("C:/work".into()));
        assert_eq!(nav.back(), Some(Route::RecycleBin));
        let restored: Navigation =
            serde_json::from_slice(&serde_json::to_vec(&nav).unwrap()).unwrap();
        assert_eq!(restored.current(), &Route::RecycleBin);
        assert_eq!(
            special_route(Path::new("FOLDEX://RECYCLE-BIN")),
            Some(Route::RecycleBin)
        );
        assert!(special_route(Path::new("C:/recycle-bin")).is_none());
    }
    #[test]
    fn virtual_home_participates_in_back_forward_and_branching() {
        let mut nav = Navigation::new(Route::Home);
        nav.visit(Route::Folder("C:/first".into()));
        nav.visit(Route::Folder("C:/second".into()));
        assert_eq!(nav.back(), Some(Route::Folder("C:/first".into())));
        assert_eq!(nav.back(), Some(Route::Home));
        assert_eq!(nav.forward(), Some(Route::Folder("C:/first".into())));
        nav.visit(Route::Home);
        assert!(!nav.can_forward());
        assert_eq!(nav.back(), Some(Route::Folder("C:/first".into())));
    }
    #[test]
    fn navigation_cap_and_serialization_preserve_virtual_route() {
        let mut nav = Navigation::new(Route::Home);
        for n in 0..100 {
            nav.visit(Route::Folder(format!("C:/{n}").into()));
        }
        nav.visit(Route::Home);
        let restored: Navigation =
            serde_json::from_slice(&serde_json::to_vec(&nav).unwrap()).unwrap();
        assert_eq!(restored.entries.len(), MAX_NAVIGATION);
        assert_eq!(restored.current(), &Route::Home);
        assert!(restored.can_back());
    }
    #[test]
    fn malformed_navigation_is_clamped_and_empty_stack_uses_migration_route() {
        let nav = Navigation {
            entries: vec![Route::Home; 100],
            at: usize::MAX,
        }
        .normalized(Route::Home);
        assert_eq!(nav.entries.len(), MAX_NAVIGATION);
        assert_eq!(nav.current(), &Route::Home);
        assert_eq!(
            Navigation {
                entries: vec![],
                at: 2
            }
            .normalized(Route::Home)
            .current(),
            &Route::Home
        );
    }
}
