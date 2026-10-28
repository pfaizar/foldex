//! File-name rules shared by inline rename, item creation and the file workers.
//! The editor applies them as the user types, the same way File Explorer does:
//! forbidden characters never enter the text, trailing spaces and dots are
//! dropped on commit, and an empty or unchanged name reverts silently.

/// Windows limits one path component to 255 UTF-16 code units.
pub(crate) const MAX_NAME_UNITS: usize = 255;

pub(crate) const FORBIDDEN_MESSAGE: &str =
    "A file name can't contain any of the following characters: \\ / : * ? \" < > |";

pub(crate) const DEFAULT_FOLDER: &str = "New folder";
pub(crate) const DEFAULT_TEXT_FILE: &str = "New Text Document.txt";

pub(crate) fn forbidden(character: char) -> bool {
    character <= '\u{1f}'
        || matches!(
            character,
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
        )
}

/// Characters selected when the editor opens: a file's name without its last
/// extension, or the whole name for folders, dot-files and extensionless names.
pub(crate) fn initial_selection(name: &str, is_dir: bool) -> (usize, usize) {
    let all = name.chars().count();
    if is_dir {
        return (0, all);
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => (0, name[..dot].chars().count()),
        _ => (0, all),
    }
}

/// Removes characters Windows can't store in a name and caps the length.
/// Returns true when a forbidden character was removed, which shows the
/// editor's explanation balloon.
pub(crate) fn sanitize(text: &mut String) -> bool {
    let mut rejected = false;
    if text.chars().any(forbidden) {
        text.retain(|character| {
            let keep = !forbidden(character);
            rejected |= !keep;
            keep
        });
    }
    if text.encode_utf16().count() > MAX_NAME_UNITS {
        let mut units = 0;
        let end = text
            .char_indices()
            .find(|(_, character)| {
                units += character.len_utf16();
                units > MAX_NAME_UNITS
            })
            .map_or(text.len(), |(at, _)| at);
        text.truncate(end);
    }
    rejected
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NameProblem {
    Empty,
    Forbidden,
    Reserved(String),
    TooLong,
    TrailingSpaceOrDot,
}

impl NameProblem {
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Empty => "Type a name for this item.".into(),
            Self::Forbidden => FORBIDDEN_MESSAGE.into(),
            Self::Reserved(stem) => {
                format!("“{stem}” is reserved by Windows. Choose a different name.")
            }
            Self::TooLong => "This name is too long. Use 255 characters or fewer.".into(),
            Self::TrailingSpaceOrDot => "A name can't end with a space or a dot.".into(),
        }
    }
}

/// The rules Windows enforces for one name, without trimming anything.
pub(crate) fn problem(name: &str) -> Option<NameProblem> {
    if name.is_empty() || name == "." || name == ".." {
        return Some(NameProblem::Empty);
    }
    if name.chars().any(forbidden) {
        return Some(NameProblem::Forbidden);
    }
    if name.ends_with([' ', '.']) {
        return Some(NameProblem::TrailingSpaceOrDot);
    }
    if name.encode_utf16().count() > MAX_NAME_UNITS {
        return Some(NameProblem::TooLong);
    }
    let stem = name.split('.').next().unwrap_or("");
    let upper = stem.to_uppercase();
    let reserved = matches!(
        upper.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        upper.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    });
    reserved.then(|| NameProblem::Reserved(stem.to_owned()))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Commit {
    /// Nothing to do: the text is empty or equals the current name.
    Unchanged,
    Rename(String),
}

/// What committing the editor's text means. Trailing spaces and dots are
/// dropped silently; text that is only spaces and dots is an error, while an
/// empty box reverts to the old name.
pub(crate) fn prepare_commit(original: &str, typed: &str) -> Result<Commit, NameProblem> {
    if typed.is_empty() {
        return Ok(Commit::Unchanged);
    }
    let trimmed = typed.trim_end_matches([' ', '.']);
    if trimmed.trim().is_empty() {
        return Err(NameProblem::Empty);
    }
    if trimmed == original {
        return Ok(Commit::Unchanged);
    }
    if let Some(problem) = problem(trimmed) {
        return Err(problem);
    }
    Ok(Commit::Rename(trimmed.to_owned()))
}

fn extension(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[dot + 1..],
        _ => "",
    }
}

/// File Explorer asks before a rename changes or removes a file's extension.
pub(crate) fn extension_changed(original: &str, renamed: &str, is_dir: bool) -> bool {
    !is_dir && !extension(original).eq_ignore_ascii_case(extension(renamed))
}

/// `New folder`, then `New folder (2)`, `(3)`… using the lowest free number,
/// inserted before a file's last extension like File Explorer does.
pub(crate) fn unique_name(base: &str, is_dir: bool, taken: impl Fn(&str) -> bool) -> String {
    if !taken(base) {
        return base.to_owned();
    }
    let (stem, extension) = match base.rfind('.') {
        Some(dot) if dot > 0 && !is_dir => base.split_at(dot),
        _ => (base, ""),
    };
    (2..)
        .map(|number| format!("{stem} ({number}){extension}"))
        .find(|name| !taken(name))
        .expect("an unbounded sequence always has a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_skips_only_the_last_extension_of_files() {
        assert_eq!(initial_selection("report.final.docx", false), (0, 12));
        assert_eq!(initial_selection("archive.tar.gz", false), (0, 11));
        assert_eq!(initial_selection(".gitignore", false), (0, 10));
        assert_eq!(initial_selection("noext", false), (0, 5));
        assert_eq!(initial_selection("v1.2", true), (0, 4));
        assert_eq!(initial_selection("New Text Document.txt", false), (0, 17));
        assert_eq!(initial_selection("résumé.pdf", false), (0, 6));
    }

    #[test]
    fn sanitizing_removes_forbidden_characters_and_caps_length() {
        let mut text = String::from("a:b*c?.txt");
        assert!(sanitize(&mut text));
        assert_eq!(text, "abc.txt");
        let mut text = String::from("tab\there");
        assert!(sanitize(&mut text));
        assert_eq!(text, "tabhere");
        let mut text = String::from("fine name.txt");
        assert!(!sanitize(&mut text));
        let mut text = "😀".repeat(200);
        assert!(!sanitize(&mut text));
        assert!(text.encode_utf16().count() <= MAX_NAME_UNITS);
        assert_eq!(text.chars().count(), 127);
    }

    #[test]
    fn commits_trim_trailing_spaces_and_dots_and_revert_when_empty() {
        assert_eq!(prepare_commit("a.txt", ""), Ok(Commit::Unchanged));
        assert_eq!(prepare_commit("a.txt", "a.txt"), Ok(Commit::Unchanged));
        assert_eq!(prepare_commit("a.txt", "a.txt. . "), Ok(Commit::Unchanged));
        assert_eq!(
            prepare_commit("a", ".gitignore."),
            Ok(Commit::Rename(".gitignore".into()))
        );
        assert_eq!(prepare_commit("a", " . ."), Err(NameProblem::Empty));
        assert_eq!(prepare_commit("a", ".."), Err(NameProblem::Empty));
        assert_eq!(
            prepare_commit("a", "nul.txt"),
            Err(NameProblem::Reserved("nul".into()))
        );
        assert_eq!(
            prepare_commit("readme", "README"),
            Ok(Commit::Rename("README".into()))
        );
        assert_eq!(
            prepare_commit("a", "  leading"),
            Ok(Commit::Rename("  leading".into()))
        );
    }

    #[test]
    fn problems_match_the_windows_rules() {
        for name in [
            "",
            ".",
            "..",
            "x\\y",
            "C:foo",
            "a\0b",
            "bad?",
            "trailing.",
            "trailing ",
            "con",
            "NUL.txt",
            "COM1",
            "LPT9.log",
            "COM¹",
        ] {
            assert!(problem(name).is_some(), "{name:?} must be rejected");
        }
        for name in [
            "hello",
            "my folder",
            "résumé.txt",
            "COM10.txt",
            ".gitignore",
        ] {
            assert_eq!(problem(name), None, "{name:?} must be accepted");
        }
    }

    #[test]
    fn extension_changes_ignore_case_and_folders() {
        assert!(extension_changed("a.txt", "a.md", false));
        assert!(extension_changed("a.txt", "a", false));
        assert!(extension_changed("a", "a.txt", false));
        assert!(!extension_changed("a.txt", "b.TXT", false));
        assert!(!extension_changed("v1.2", "v1.3", true));
        assert!(!extension_changed(".gitignore", ".npmignore", false));
    }

    #[test]
    fn unique_names_use_the_lowest_free_number_before_the_extension() {
        let taken = |names: &[&str]| {
            let names: Vec<String> = names.iter().map(|name| name.to_lowercase()).collect();
            move |name: &str| names.contains(&name.to_lowercase())
        };
        assert_eq!(unique_name("New folder", true, taken(&[])), "New folder");
        assert_eq!(
            unique_name("New folder", true, taken(&["new folder", "New folder (3)"])),
            "New folder (2)"
        );
        assert_eq!(
            unique_name(
                "New folder",
                true,
                taken(&["New folder", "New folder (2)", "New folder (3)"])
            ),
            "New folder (4)"
        );
        assert_eq!(
            unique_name(DEFAULT_TEXT_FILE, false, taken(&[DEFAULT_TEXT_FILE])),
            "New Text Document (2).txt"
        );
        assert_eq!(
            unique_name("archive.tar.gz", false, taken(&["archive.tar.gz"])),
            "archive.tar (2).gz"
        );
        assert_eq!(unique_name("v1.2", true, taken(&["v1.2"])), "v1.2 (2)");
    }
}
