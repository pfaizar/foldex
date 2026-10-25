//! One bounded, persisted folder bookmark list. Identity is lexical: this never
//! resolves links, touches a drive, or removes unavailable locations.
use std::path::{Path, PathBuf};

pub const MAX_BOOKMARKS: usize = 32;
const MAX_PATH_UNITS: usize = 32_768;

pub fn contains(paths: &[PathBuf], path: &Path) -> bool {
    paths.iter().any(|saved| path_eq(saved, path))
}

/// Returns false only for an invalid path or a full list. Removing a bookmark
/// works even when the list is full, and removes legacy duplicate spellings.
pub fn toggle(paths: &mut Vec<PathBuf>, path: PathBuf) -> bool {
    if contains(paths, &path) {
        paths.retain(|saved| !path_eq(saved, &path));
        return true;
    }
    if paths.len() >= MAX_BOOKMARKS || !valid(&path) {
        return false;
    }
    paths.push(path);
    true
}

pub fn normalize(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::with_capacity(paths.len().min(MAX_BOOKMARKS));
    for path in paths {
        if valid(&path) && !contains(&unique, &path) {
            unique.push(path);
            if unique.len() == MAX_BOOKMARKS {
                break;
            }
        }
    }
    unique
}

pub fn move_by(paths: &mut [PathBuf], path: &Path, offset: isize) -> bool {
    let Some(index) = paths.iter().position(|saved| path_eq(saved, path)) else {
        return false;
    };
    let Some(next) = index
        .checked_add_signed(offset)
        .filter(|next| *next < paths.len())
    else {
        return false;
    };
    paths.swap(index, next);
    true
}

fn valid(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= MAX_PATH_UNITS * 3
        && !path.as_os_str().as_encoded_bytes().contains(&0)
}

pub fn path_eq(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
        let left = windows_lexical(&left.as_os_str().encode_wide().collect::<Vec<_>>());
        let right = windows_lexical(&right.as_os_str().encode_wide().collect::<Vec<_>>());
        if left.len() > i32::MAX as usize || right.len() > i32::MAX as usize {
            return false;
        }
        // Ordinal Windows case comparison preserves non-Unicode OS strings and
        // is independent of the user's locale. No filesystem APIs are used.
        unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            ) == CSTR_EQUAL
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(any(windows, test))]
pub(crate) fn windows_lexical(path: &[u16]) -> Vec<u16> {
    let mut path: Vec<_> = path
        .iter()
        .map(|unit| if *unit == 47 { 92 } else { *unit })
        .collect();
    // Canonical Windows paths commonly carry the long-path prefix. Only unify
    // ordinary components: verbatim trailing dots/spaces have different meaning.
    let verbatim = path.starts_with(&[92, 92, 63, 92]);
    let ordinary = path
        .split(|unit| *unit == 92)
        .all(|part| !part.ends_with(&[32]) && !part.ends_with(&[46]));
    if verbatim && ordinary {
        if path.len() >= 8
            && path[4..8]
                .iter()
                .zip([85, 78, 67, 92])
                .all(|(a, b)| *a == b || (*a >= 97 && *a <= 122 && *a - 32 == b))
        {
            path = [vec![92, 92], path[8..].to_vec()].concat();
        } else if path.len() >= 7 && path[5] == 58 && path[6] == 92 {
            path.drain(..4);
        }
    }
    // Do not reinterpret device namespaces or special verbatim components.
    if path.starts_with(&[92, 92, 63, 92]) || path.starts_with(&[92, 92, 46, 92]) {
        return path;
    }
    let unc = path.starts_with(&[92, 92]);
    let rooted = path.first() == Some(&92);
    let drive_root = path.len() >= 3 && path[1] == 58 && path[2] == 92;
    let parts: Vec<_> = path
        .split(|unit| *unit == 92)
        .filter(|part| !part.is_empty() && *part != [46])
        .collect();
    let mut result = if unc {
        vec![92, 92]
    } else if rooted {
        vec![92]
    } else {
        Vec::new()
    };
    for part in parts {
        if !result.is_empty() && result.last() != Some(&92) {
            result.push(92);
        }
        result.extend_from_slice(part);
    }
    if drive_root && result.len() == 2 {
        result.push(92);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lexical(path: &str) -> Vec<u16> {
        windows_lexical(&path.encode_utf16().collect::<Vec<_>>())
    }

    #[test]
    fn windows_identity_unifies_separators_roots_and_standard_long_paths() {
        assert_eq!(
            lexical("C:/Users/Test/./Docs//"),
            lexical(r"C:\Users\Test\Docs")
        );
        assert_eq!(
            lexical(r"\\?\C:\Users\Test\Docs"),
            lexical(r"C:\Users\Test\Docs")
        );
        assert_eq!(
            lexical(r"\\?\UNC\server\share\folder"),
            lexical(r"\\server\share\folder\")
        );
        assert_ne!(lexical("C:"), lexical("C:/"));
        assert_ne!(lexical(r"C:\link\..\Docs"), lexical(r"C:\Docs"));
        assert_ne!(lexical(r"\\?\C:\Docs.\"), lexical(r"C:\Docs.\"));
    }

    #[cfg(windows)]
    #[test]
    fn toggle_removes_all_equivalent_spellings_and_retains_offline_paths() {
        let mut paths = vec![
            PathBuf::from(r"C:\Users\TEST\Docs"),
            PathBuf::from("c:/users/test/docs/"),
        ];
        assert!(toggle(&mut paths, PathBuf::from(r"\\?\C:\Users\Test\Docs")));
        assert!(paths.is_empty());
        assert!(toggle(&mut paths, PathBuf::from(r"Z:\Offline\Saved")));
        assert!(contains(&paths, Path::new(r"z:\offline\saved\")));
        assert!(path_eq(Path::new(r"C:\Ärger"), Path::new(r"c:\ärger")));
    }

    #[test]
    fn persisted_bookmarks_deduplicate_preserve_order_and_bound_the_list() {
        let root = std::env::temp_dir();
        let first = root.join("first");
        let mut paths = vec![first.clone(), first.clone(), PathBuf::from("relative")];
        paths.extend((0..MAX_BOOKMARKS + 5).map(|i| root.join(i.to_string())));
        let mut paths = normalize(paths);
        assert_eq!(paths.len(), MAX_BOOKMARKS);
        assert_eq!(paths[0], first);
        assert!(!toggle(&mut paths, root.join("full")));
        assert!(move_by(&mut paths, &first, 1));
        assert_eq!(paths[1], first);
        assert!(!move_by(&mut paths, &first, -(MAX_BOOKMARKS as isize)));
        assert!(toggle(&mut paths, first));
        assert!(toggle(&mut paths, root.join("full")));
    }
}
