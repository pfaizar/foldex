//! Slow launcher providers and clipboard access. Called only by the provider worker.
use super::{ResultAction, ResultGroup, ResultRow, matching::Matcher, row};
use foldex_ui::Icon;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

/// Drop overlapping roots without converting filesystem identities to lossy text.
pub(super) fn distinct_roots(roots: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut result: Vec<PathBuf> = vec![];
    for root in roots {
        if root.as_os_str().is_empty() || result.iter().any(|old| super::contains_path(old, &root))
        {
            continue;
        }
        result.retain(|old| !super::contains_path(&root, old));
        result.push(root);
    }
    result
}

/// Resolve redirected user folders on the provider thread. Never promote a whole
/// home, drive, program tree or cache to an implicit recursive search scope.
pub(super) fn default_roots(current: &Path, known_folders: bool) -> Vec<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let mut roots = user_folders(known_folders, home.as_deref());
    if useful_default_scope(current, home.as_deref()) {
        roots.push(current.to_path_buf());
    }
    let roots = distinct_roots(
        roots
            .into_iter()
            .filter(|path| useful_default_scope(path, home.as_deref())),
    );
    if known_folders {
        roots.into_iter().filter(|path| path.is_dir()).collect()
    } else {
        roots
    }
}

fn user_folders(known_folders: bool, home: Option<&Path>) -> Vec<PathBuf> {
    let fallback = |name| home.map(|root| root.join(name));
    #[cfg(windows)]
    if known_folders {
        use windows_sys::Win32::{System::Com::CoTaskMemFree, UI::Shell::*};
        let mut roots = vec![];
        for (id, name) in [
            (&FOLDERID_Documents, "Documents"),
            (&FOLDERID_Desktop, "Desktop"),
            (&FOLDERID_Downloads, "Downloads"),
        ] {
            let mut pointer = std::ptr::null_mut();
            let result = unsafe {
                SHGetKnownFolderPath(
                    id,
                    KF_FLAG_DONT_VERIFY as u32,
                    std::ptr::null_mut(),
                    &mut pointer,
                )
            };
            let path = if result >= 0 && !pointer.is_null() {
                use std::os::windows::ffi::OsStringExt;
                let mut length = 0;
                while length < 32_768 && unsafe { *pointer.add(length) } != 0 {
                    length += 1;
                }
                (length > 0 && length < 32_768).then(|| {
                    PathBuf::from(std::ffi::OsString::from_wide(unsafe {
                        std::slice::from_raw_parts(pointer, length)
                    }))
                })
            } else {
                None
            };
            if !pointer.is_null() {
                unsafe {
                    CoTaskMemFree(pointer.cast());
                }
            }
            if let Some(path) = path.or_else(|| fallback(name)) {
                roots.push(path);
            }
        }
        return roots;
    }
    let _ = known_folders;
    ["Documents", "Desktop", "Downloads"]
        .into_iter()
        .filter_map(fallback)
        .collect()
}

fn useful_default_scope(path: &Path, home: Option<&Path>) -> bool {
    if !path.is_absolute() || path.file_name().is_none() || noisy_path(path) {
        return false;
    }
    if home.is_some_and(|home| super::contains_path(path, home)) {
        return false;
    }
    for name in [
        "SystemRoot",
        "WINDIR",
        "PROGRAMFILES",
        "PROGRAMFILES(X86)",
        "PROGRAMDATA",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        if std::env::var_os(name).is_some_and(|root| super::contains_path(Path::new(&root), path)) {
            return false;
        }
    }
    true
}

fn drive_install_tree(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::path::{Component, Prefix};
        let mut components = path.components();
        let Some(Component::Prefix(prefix)) = components.next() else {
            return false;
        };
        if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
            || !matches!(components.next(), Some(Component::RootDir))
        {
            return false;
        }
        let Some(Component::Normal(name)) = components.next() else {
            return false;
        };
        name.to_str().is_some_and(|name| {
            ["Programs", "Apps", "Applications"]
                .iter()
                .any(|folder| name.eq_ignore_ascii_case(folder))
        })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

fn noisy_path(path: &Path) -> bool {
    drive_install_tree(path)
        || path.components().any(|component| {
            let Some(name) = component.as_os_str().to_str() else {
                return false;
            };
            matches!(
                name.to_ascii_lowercase().as_str(),
                "windows"
                    | "system32"
                    | "syswow64"
                    | "winsxs"
                    | "program files"
                    | "program files (x86)"
                    | "programdata"
                    | "appdata"
                    | "$recycle.bin"
                    | "system volume information"
                    | "node_modules"
                    | "bower_components"
                    | "vendor"
                    | "target"
                    | "build"
                    | "dist"
                    | "out"
                    | "bin"
                    | "obj"
                    | ".git"
                    | ".svn"
                    | ".hg"
                    | ".cache"
                    | "cache"
                    | "caches"
                    | "temp"
                    | "tmp"
                    | "__pycache__"
                    | ".cargo"
                    | ".rustup"
                    | ".npm"
                    | ".nuget"
                    | ".gradle"
                    | ".m2"
                    | ".venv"
                    | "venv"
                    | ".tox"
                    | ".next"
                    | ".turbo"
                    | "cmakefiles"
                    | "cmake-build-debug"
                    | "cmake-build-release"
            )
        })
}

fn artifact(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "dll"
                    | "pdb"
                    | "obj"
                    | "o"
                    | "a"
                    | "lib"
                    | "rlib"
                    | "rmeta"
                    | "ilk"
                    | "exp"
                    | "idb"
                    | "ipch"
                    | "tlog"
                    | "pyc"
                    | "pyo"
                    | "class"
                    | "d"
                    | "tmp"
                    | "temp"
                    | "lock"
            )
        })
}

pub(crate) fn title_score(matcher: &mut Matcher, title: &str) -> Option<i32> {
    matcher.score(title)
}

#[cfg(test)]
pub(super) fn file_row(
    path: PathBuf,
    is_dir: bool,
    matcher: &mut Matcher,
    explicit: bool,
) -> Option<ResultRow> {
    let title = crate::pane::path_label(&path);
    let score = candidate_score(
        matcher,
        &title,
        path.parent().unwrap_or(Path::new("")),
        is_dir,
        explicit,
    )?;
    if !explicit && drive_install_tree(&path) {
        return None;
    }
    Some(scored_file_row(path, is_dir, score))
}

/// Called on the search worker before retaining/decoding catalog paths. The
/// parent is display text for relevance checks; raw identities stay in the index.
#[cfg(test)]
pub(crate) fn candidate_score(
    matcher: &mut Matcher,
    title: &str,
    parent: &Path,
    is_dir: bool,
    explicit: bool,
) -> Option<i32> {
    if !explicit && drive_install_tree(parent) {
        return None;
    }
    let match_score = title_score(matcher, title)?;
    candidate_score_with_rank(
        title,
        parent,
        is_dir,
        explicit,
        matcher.query(),
        match_score,
    )
}

pub(crate) fn candidate_score_with_rank(
    title: &str,
    parent: &Path,
    is_dir: bool,
    explicit: bool,
    query: &str,
    match_score: i32,
) -> Option<i32> {
    if !explicit && drive_install_tree(parent) {
        return None;
    }
    let noisy = noisy_path(parent)
        || is_dir && noisy_path(Path::new(title))
        || !is_dir && artifact(Path::new(title));
    let exact = title.eq_ignore_ascii_case(query);
    if noisy && !explicit && !exact {
        return None;
    }
    Some(file_score(title, is_dir, match_score, exact, noisy))
}

fn file_score(title: &str, is_dir: bool, match_score: i32, exact: bool, noisy: bool) -> i32 {
    let utility = if is_dir {
        100
    } else {
        match Path::new(title)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "pdf" | "doc" | "docx" | "odt" | "rtf" | "xls" | "xlsx" | "ods" | "ppt" | "pptx"
            | "odp" => 220,
            "txt" | "md" | "csv" | "tsv" | "epub" | "pages" | "numbers" | "key" => 180,
            "png" | "jpg" | "jpeg" | "webp" | "svg" | "psd" | "ai" | "fig" | "mp4" | "mp3"
            | "wav" => 120,
            "zip" | "7z" | "rar" | "exe" | "msi" => 60,
            _ => 0,
        }
    };
    match_score
        .saturating_add(200 + utility + if exact { 1200 } else { 0 } - if noisy { 2400 } else { 0 })
}

pub(crate) fn scored_file_row(path: PathBuf, is_dir: bool, score: i32) -> ResultRow {
    let title = crate::pane::path_label(&path);
    let action = if is_dir {
        ResultAction::Browse(path.clone())
    } else {
        ResultAction::Open(path.clone())
    };
    row(
        title.clone(),
        crate::pane::friendly_path(&path),
        foldex_ui::file_icon(&title, is_dir),
        action,
        score,
    )
}

fn path_row(path: PathBuf, is_dir: bool, matcher: &foldex_match::Matcher) -> Option<ResultRow> {
    let title = crate::pane::path_label(&path);
    let score = file_score(
        &title,
        is_dir,
        matcher.score(&title)?,
        title.eq_ignore_ascii_case(matcher.query()),
        noisy_path(&path) || !is_dir && artifact(&path),
    );
    Some(scored_file_row(path, is_dir, score))
}
pub(super) fn discovery(
    favorites: &[PathBuf],
    recents: &[PathBuf],
    matcher: &mut Matcher,
    current: &dyn Fn() -> bool,
) -> Vec<Arc<ResultRow>> {
    let mut rows = vec![];
    let mut seen: Vec<&Path> = vec![];
    for (paths, group, limit, bias) in [
        (
            favorites,
            ResultGroup::Favorites,
            super::MAX_FAVORITES,
            300_000,
        ),
        (recents, ResultGroup::Recents, super::MAX_RECENTS, 200_000),
    ] {
        for (at, path) in paths.iter().take(limit).enumerate() {
            if !current() {
                return rows;
            }
            // Reject nonmatches before metadata calls, including saved paths
            // on disconnected or sleeping network shares.
            let title = crate::pane::path_label(path);
            let Some(score) = title_score(matcher, &title) else {
                continue;
            };
            if seen
                .iter()
                .any(|old| super::contains_path(old, path) && super::contains_path(path, old))
            {
                continue;
            }
            let Ok(metadata) = std::fs::metadata(path) else {
                continue;
            };
            if !metadata.is_file() && !metadata.is_dir() {
                continue;
            }
            let action = if metadata.is_dir() {
                ResultAction::Browse(path.clone())
            } else {
                ResultAction::Open(path.clone())
            };
            let mut result = row(
                title.clone(),
                crate::pane::friendly_path(path),
                foldex_ui::file_icon(&title, metadata.is_dir()),
                action,
                if matcher.is_empty() {
                    bias - at as i32
                } else {
                    score.saturating_add(500)
                },
            );
            result.group = group;
            rows.push(Arc::new(result));
            seen.push(path);
        }
    }
    rows
}

pub(super) fn path_intent(query: &str) -> bool {
    let text = query.trim().trim_matches('"');
    text == "~"
        || ["~/", "~\\", "./", ".\\", "../", "..\\"]
            .iter()
            .any(|prefix| text.starts_with(*prefix))
        || text.contains(['\\', '/'])
        || text.as_bytes().get(1) == Some(&b':')
        || text.starts_with('%') && text[1..].contains('%')
}

fn expand_path(query: &str, current: &Path) -> PathBuf {
    let text = query.trim().trim_matches('"');
    if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
        if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
            return PathBuf::from(home).join(text[1..].trim_start_matches(['/', '\\']));
        }
    }
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let tail = &rest[start + 1..];
        let Some(end) = tail.find('%') else {
            expanded.push_str(&rest[start..]);
            rest = "";
            break;
        };
        if let Some(value) = std::env::var_os(&tail[..end]) {
            expanded.push_str(&value.to_string_lossy());
        } else {
            expanded.push_str(&rest[start..start + end + 2]);
        }
        rest = &tail[end + 1..];
    }
    expanded.push_str(rest);
    let path = PathBuf::from(expanded);
    if path.is_absolute() {
        path
    } else {
        current.join(path)
    }
}

/// Exact paths and immediate-child completion never start a recursive drive scan.
pub(super) fn path_rows(
    query: &str,
    current_folder: &Path,
    current: &dyn Fn() -> bool,
) -> Vec<Arc<ResultRow>> {
    let path = expand_path(query, current_folder);
    let trailing = query.trim().trim_matches('"').ends_with(['/', '\\']);
    let mut rows = vec![];
    if let Ok(metadata) = std::fs::metadata(&path) {
        if let Some(mut result) = path_row(
            path.clone(),
            metadata.is_dir(),
            &foldex_match::Matcher::new(""),
        ) {
            result.score = 9000;
            rows.push(Arc::new(result));
        }
        if !metadata.is_dir() || !trailing {
            return rows;
        }
    }
    let (directory, leaf) = if trailing {
        (path.as_path(), "")
    } else {
        let Some(parent) = path.parent() else {
            return rows;
        };
        (
            parent,
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
    };
    let matcher = foldex_match::Matcher::new(leaf);
    let Ok(items) = std::fs::read_dir(directory) else {
        return rows;
    };
    for entry in items.take(2048).flatten() {
        if !current() {
            return vec![];
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() && !kind.is_dir() {
            continue;
        }
        if let Some(result) = path_row(entry.path(), kind.is_dir(), &matcher) {
            rows.push(Arc::new(result));
            if rows.len() > 128 {
                super::sort_rows(&mut rows);
                rows.truncate(64);
            }
        }
    }
    super::sort_rows(&mut rows);
    rows.truncate(64);
    rows
}

pub fn applications(current: &dyn Fn() -> bool) -> Option<Vec<Arc<ResultRow>>> {
    if !current() {
        return None;
    }
    let mut roots = vec![];
    if let Some(path) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(path).join("Microsoft/Windows/Start Menu/Programs"));
    }
    if let Some(path) = std::env::var_os("PROGRAMDATA") {
        roots.push(PathBuf::from(path).join("Microsoft/Windows/Start Menu/Programs"));
    }
    let mut apps = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut retained = 0;
    for (title, identifier) in super::applications::installed(current)? {
        if !current() {
            return None;
        }
        if seen.insert(title.to_lowercase()) {
            retained += title.len() + identifier.len();
            let priority = super::applications::default_priority(&title);
            let mut result = row(
                title,
                "Installed app".into(),
                Icon::Window,
                ResultAction::Application(identifier),
                priority,
            );
            result.group = ResultGroup::Apps;
            apps.push(Arc::new(result));
        }
    }
    let mut stack: Vec<_> = roots.into_iter().map(|root| (root, 0)).collect();
    let mut visited = 0;
    while let Some((directory, depth)) = stack.pop() {
        if !current() {
            return None;
        }
        if apps.len() >= 8192
            || retained >= super::applications::MAX_CATALOG_BYTES
            || visited >= 32_768
        {
            break;
        }
        let Ok(items) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in items.flatten() {
            if !current() {
                return None;
            }
            if apps.len() >= 8192
                || retained >= super::applications::MAX_CATALOG_BYTES
                || visited >= 32_768
            {
                break;
            }
            visited += 1;
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() && !kind.is_symlink() && depth < 8 {
                if stack.len() < 1024 {
                    stack.push((path, depth + 1));
                }
            } else if kind.is_file()
                && path.extension().is_some_and(|ext| {
                    ext.eq_ignore_ascii_case("lnk") || ext.eq_ignore_ascii_case("exe")
                })
            {
                let title = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if title.to_ascii_lowercase().contains("uninstall") {
                    continue;
                }
                let key = title.to_lowercase();
                if seen.insert(key) {
                    let bytes = title.len() + path.as_os_str().len() * 2;
                    if bytes > super::applications::MAX_CATALOG_BYTES.saturating_sub(retained) {
                        continue;
                    }
                    retained += bytes;
                    let priority = super::applications::default_priority(&title);
                    let mut result = row(
                        title,
                        crate::pane::friendly_path(&path),
                        Icon::Window,
                        ResultAction::Open(path),
                        priority,
                    );
                    result.group = ResultGroup::Apps;
                    apps.push(Arc::new(result));
                }
            }
        }
    }
    apps.sort_unstable_by(|a, b| a.title.cmp(&b.title).then_with(|| a.id.cmp(&b.id)));
    current().then_some(apps)
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn substantive_title_matches_require_recognizable_fragments_or_abbreviations() {
        for (query, title) in [
            ("notep", "Notepad"),
            ("NOTEP", "Notepad++"),
            ("pad", "Notepad"),
            ("notpa", "Notepad"),
            ("ntpd", "Notepad"),
            ("ntpad", "Notepad"),
            ("win term", "Windows Terminal"),
            ("wterm", "Windows Terminal"),
            ("vsc", "Visual Studio Code"),
            ("vscode", "VisualStudioCode"),
            ("wsdk", "Windows Software Development Kit"),
            ("wss", "WindowsStandaloneSupport"),
            ("прв", "Привет"),
            ("np", "Notepad"),
        ] {
            let mut matcher = Matcher::new(query);
            let raw = matcher.app_score(title);
            assert!(raw.is_some(), "{query:?} in {title:?}");
        }
        for title in [
            "Windows Software Development Kit",
            "WindowsStandaloneSupport",
            "D:\\Workspace\\Project\\QuanTrading\\Code\\terminal-rs — Windows PowerShell - Otty",
            "n___o___t___e___p",
        ] {
            let mut matcher = Matcher::new("notep");
            assert!(title_score(&mut matcher, title).is_none(), "{title}");
        }
        let mut matcher = Matcher::new("notep");
        assert!(
            file_row(
                PathBuf::from("C:/Users/Name/Documents/WindowsStandaloneSupport"),
                true,
                &mut matcher,
                true,
            )
            .is_none(),
            "Configured roots still require a relevant title match"
        );
        assert!(
            file_row(
                PathBuf::from("C:/Users/Name/Documents/Notepad notes.txt"),
                false,
                &mut matcher,
                false,
            )
            .is_some()
        );
    }

    #[test]
    fn user_documents_beat_artifacts_and_explicit_scopes_remain_searchable() {
        let mut matcher = Matcher::new("report");
        let document = file_row(
            PathBuf::from("C:/Users/Name/Documents/report.pdf"),
            false,
            &mut matcher,
            false,
        )
        .unwrap();
        for path in [
            "C:/Users/Name/Documents/report.dll",
            "C:/Work/project/target/release/report.pdf",
            "C:/Work/project/node_modules/reports/report.txt",
            "C:/Users/Name/AppData/Local/Cache/report.json",
            "C:/Windows/System32/report.dll",
        ] {
            assert!(
                file_row(PathBuf::from(path), false, &mut matcher, false).is_none(),
                "{path}"
            );
            let explicit = file_row(PathBuf::from(path), false, &mut matcher, true).unwrap();
            assert!(document.score > explicit.score, "{path}");
        }
        let mut exact = Matcher::new("report.dll");
        assert!(
            file_row(
                PathBuf::from("C:/Work/report.dll"),
                false,
                &mut exact,
                false
            )
            .is_some()
        );
    }

    #[test]
    fn persisted_discovery_validates_deduplicates_and_preserves_group_order() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/launcher-provider-fixtures")
            .join(format!("discovery-{}", std::process::id()));
        let first = root.join("Zebra Favorite");
        let second = root.join("Alpha Favorite");
        let recent = root.join("Recent document.txt");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(&recent, "launcher discovery fixture").unwrap();
        let favorites = vec![
            first.clone(),
            second.clone(),
            root.join("does-not-exist"),
            first.clone(),
        ];
        let recents = vec![second.clone(), recent.clone()];
        let mut matcher = Matcher::new("");
        let mut rows = discovery(&favorites, &recents, &mut matcher, &|| true);
        super::super::sort_rows(&mut rows);
        assert_eq!(
            rows.iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["Zebra Favorite", "Alpha Favorite", "Recent document.txt"]
        );
        assert_eq!(
            rows.iter().map(|row| row.group).collect::<Vec<_>>(),
            [
                ResultGroup::Favorites,
                ResultGroup::Favorites,
                ResultGroup::Recents
            ]
        );
        assert!(matches!(&rows[0].action, ResultAction::Browse(path) if *path == first));
        assert!(matches!(&rows[2].action, ResultAction::Open(path) if *path == recent));
        let again = discovery(&favorites, &recents, &mut matcher, &|| true);
        assert_eq!(
            rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            again.iter().map(|row| row.id).collect::<Vec<_>>()
        );
        assert!(discovery(&favorites, &recents, &mut matcher, &|| false).is_empty());
        let matched = discovery(
            &favorites,
            &recents,
            &mut Matcher::new("recent document"),
            &|| true,
        );
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, rows[2].id);
    }

    #[test]
    fn explicit_path_completion_does_not_walk_descendants() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/launcher-provider-fixtures")
            .join(format!("paths-{}", std::process::id()));
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::create_dir_all(root.join("WindowsStandaloneSupport")).unwrap();
        std::fs::write(root.join("visible.txt"), "visible").unwrap();
        std::fs::write(root.join("nested/hidden-descendant.txt"), "descendant").unwrap();
        let query = format!("{}/", root.display());
        assert!(path_intent(&query));
        let rows = path_rows(&query, &root, &|| true);
        assert!(rows.iter().any(|row| row.title == "visible.txt"));
        assert!(!rows.iter().any(|row| row.title == "hidden-descendant.txt"));
        let exact = root.join("visible.txt");
        let rows = path_rows(&mut exact.to_string_lossy(), &root, &|| true);
        assert_eq!(rows.len(), 1);
        assert!(matches!(&rows[0].action, ResultAction::Open(path) if *path == exact));
        let partial = root.join("notep");
        let rows = path_rows(&partial.to_string_lossy(), &root, &|| true);
        assert!(
            rows.iter()
                .any(|row| row.title == "WindowsStandaloneSupport"),
            "Explicit path completion retains its existing fuzzy matching"
        );
    }

    #[cfg(windows)]
    #[test]
    fn drive_install_trees_are_excluded_without_hiding_nested_user_folders() {
        let home = Path::new("C:/Users/Name");
        let mut matcher = Matcher::new("report");
        let mut exact = Matcher::new("report.pdf");
        for root in [
            "D:/Programs",
            "D:/Programs/Unity/Editor",
            "C:/Apps",
            "C:/Apps/Utilities",
            "D:/Applications",
            "D:/Applications/FilePilot",
            "\\\\?\\d:\\pRoGrAmS\\FilePilot",
        ] {
            let root = Path::new(root);
            assert!(!useful_default_scope(root, Some(home)), "{root:?}");
            assert!(noisy_path(root), "{root:?}");
            let path = root.join("report.pdf");
            assert!(file_row(path.clone(), false, &mut matcher, false).is_none());
            assert!(file_row(path.clone(), false, &mut exact, false).is_none());
            let explicit = file_row(path.clone(), false, &mut matcher, true).unwrap();
            assert!(matches!(&explicit.action, ResultAction::Open(target) if *target == path));
        }
        for root in [
            "C:/Users/Name/Documents/Programs",
            "C:/Users/Name/Documents/Applications",
            "D:/Projects/Apps",
            "D:/Projects/Programs",
            "D:/Projects/Applications",
            "D:/Programs-and-notes",
            "D:/Programs2",
            "\\\\?\\D:\\Projects\\Apps",
        ] {
            let root = Path::new(root);
            assert!(useful_default_scope(root, Some(home)), "{root:?}");
            assert!(!noisy_path(root), "{root:?}");
            assert!(file_row(root.join("report.pdf"), false, &mut matcher, false).is_some());
        }
        let explicit = vec![PathBuf::from("D:/Programs"), PathBuf::from("C:/Apps")];
        assert_eq!(distinct_roots(explicit.clone()), explicit);
    }

    #[cfg(windows)]
    #[test]
    fn defaults_exclude_broad_and_noisy_scopes_but_allow_useful_current_folders() {
        let home = Path::new("C:/Users/Name");
        for path in [
            "C:/",
            "C:/Users",
            "C:/Users/Name",
            "C:/Windows",
            "C:/Program Files/Tools",
            "C:/Users/Name/AppData/Local",
            "D:/Work/project/target",
            "D:/Work/project/node_modules",
        ] {
            assert!(!useful_default_scope(Path::new(path), Some(home)), "{path}");
        }
        assert!(useful_default_scope(
            Path::new("D:/Work/design"),
            Some(home)
        ));
        let roots = distinct_roots([
            home.join("Documents"),
            home.join("Desktop"),
            home.join("Downloads"),
            PathBuf::from("\\\\?\\c:\\users\\name\\Documents\\Design"),
        ]);
        assert_eq!(roots.len(), 3);
        assert!(!roots.iter().any(|root| root == home));
        // Explicit settings bypass the default-scope policy.
        let explicit = vec![
            PathBuf::from("C:/Windows"),
            PathBuf::from("D:/Work/project/target"),
        ];
        assert_eq!(distinct_roots(explicit.clone()), explicit);
    }
}

#[cfg(windows)]
pub fn windows() -> Vec<Arc<ResultRow>> {
    use windows_sys::Win32::{Foundation::*, Graphics::Dwm::*, UI::WindowsAndMessaging::*};
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        let rows = unsafe { &mut *(data as *mut Vec<Arc<ResultRow>>) };
        if rows.len() >= 512 {
            return 0;
        }
        if unsafe { IsWindowVisible(hwnd) } == 0
            || unsafe { GetWindow(hwnd, GW_OWNER) } != std::ptr::null_mut()
        {
            return 1;
        }
        let mut process = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, &mut process);
        }
        if process == std::process::id() {
            return 1;
        }
        let mut cloaked = 0_u32;
        if unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED as u32,
                (&mut cloaked as *mut u32).cast(),
                4,
            )
        } >= 0
            && cloaked != 0
        {
            return 1;
        }
        let count = unsafe { GetWindowTextLengthW(hwnd) }.min(2048);
        if count <= 0 {
            return 1;
        }
        let mut text = vec![0_u16; count as usize + 1];
        let written = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
        if written > 0 {
            let title = String::from_utf16_lossy(&text[..written as usize]);
            let detail = if unsafe { IsIconic(hwnd) } != 0 {
                "Minimized window"
            } else {
                "Open window"
            };
            rows.push(Arc::new(row(
                title,
                detail.into(),
                Icon::Window,
                ResultAction::Window(hwnd as isize),
                0,
            )));
        }
        1
    }
    let mut rows = vec![];
    unsafe {
        EnumWindows(
            Some(collect),
            (&mut rows as *mut Vec<Arc<ResultRow>>) as isize,
        );
    }
    rows
}
#[cfg(not(windows))]
pub fn windows() -> Vec<Arc<ResultRow>> {
    vec![]
}

#[cfg(windows)]
pub fn activate_window(target: isize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let hwnd = target as _;
    if target == 0 || unsafe { IsWindow(hwnd) } == 0 {
        return false;
    }
    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindowAsync(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd) != 0
    }
}
#[cfg(not(windows))]
pub fn activate_window(_: isize) -> bool {
    false
}

#[cfg(windows)]
pub fn clipboard_text() -> Option<String> {
    use windows_sys::Win32::{
        Foundation::*,
        System::{DataExchange::*, Memory::*},
    };
    if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
        return None;
    }
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    let _close = Close;
    let format = |name: &str| unsafe {
        RegisterClipboardFormatW(
            name.encode_utf16()
                .chain(Some(0))
                .collect::<Vec<_>>()
                .as_ptr(),
        )
    };
    for name in [
        "ExcludeClipboardContentFromMonitorProcessing",
        "Clipboard Viewer Ignore",
    ] {
        if unsafe { IsClipboardFormatAvailable(format(name)) } != 0 {
            return None;
        }
    }
    let history = format("CanIncludeInClipboardHistory");
    if unsafe { IsClipboardFormatAvailable(history) } != 0 {
        let data = unsafe { GetClipboardData(history) };
        if !data.is_null() && unsafe { GlobalSize(data as HGLOBAL) } >= 4 {
            let pointer = unsafe { GlobalLock(data as HGLOBAL) } as *const u32;
            if !pointer.is_null() {
                let allowed = unsafe { pointer.read_unaligned() };
                unsafe {
                    GlobalUnlock(data as HGLOBAL);
                }
                if allowed == 0 {
                    return None;
                }
            }
        }
    }
    let handle = unsafe { GetClipboardData(13) };
    if handle.is_null() {
        return None;
    }
    let bytes = unsafe { GlobalSize(handle as HGLOBAL) };
    if bytes == 0 || bytes > 64 * 1024 {
        return None;
    }
    let pointer = unsafe { GlobalLock(handle as HGLOBAL) } as *const u16;
    if pointer.is_null() {
        return None;
    }
    let units = unsafe { std::slice::from_raw_parts(pointer, bytes / 2) };
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    let result = String::from_utf16_lossy(&units[..end]);
    unsafe {
        GlobalUnlock(handle as HGLOBAL);
    }
    (!result.trim().is_empty()).then_some(result)
}
#[cfg(not(windows))]
pub fn clipboard_text() -> Option<String> {
    None
}

#[cfg(windows)]
pub fn write_clipboard(text: &str, owner: isize) -> std::io::Result<()> {
    use windows_sys::Win32::{
        Foundation::*,
        System::{DataExchange::*, Memory::*},
    };
    if text.len() > 64 * 1024 {
        return Err(std::io::Error::other("Clipboard text is too large"));
    }
    if owner == 0 {
        return Err(std::io::Error::other("The launcher is still starting"));
    }
    let mut opened = false;
    for _ in 0..3 {
        if unsafe { OpenClipboard(owner as _) } != 0 {
            opened = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    if !opened {
        return Err(std::io::Error::last_os_error());
    }
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    let _close = Close;
    let units: Vec<_> = text.encode_utf16().chain(Some(0)).collect();
    if units.len() * 2 > 64 * 1024 {
        return Err(std::io::Error::other("Clipboard text is too large"));
    }
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, units.len() * 2) };
    if memory.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let pointer = unsafe { GlobalLock(memory) } as *mut u16;
    if pointer.is_null() {
        unsafe {
            GlobalFree(memory);
        }
        return Err(std::io::Error::last_os_error());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(units.as_ptr(), pointer, units.len());
        GlobalUnlock(memory);
    }
    if unsafe { EmptyClipboard() } == 0
        || unsafe { SetClipboardData(13, memory as HANDLE) }.is_null()
    {
        unsafe {
            GlobalFree(memory);
        }
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(not(windows))]
pub fn write_clipboard(_: &str, _: isize) -> std::io::Result<()> {
    Err(std::io::Error::other("Clipboard requires Windows"))
}
