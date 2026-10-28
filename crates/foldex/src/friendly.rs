//! Plain-language error text for people, with the raw detail kept for Developer Mode.
//!
//! Every message that reaches the screen as a failure goes through [`present`]. By default
//! it returns a short sentence a non-technical person can act on ("Couldn't move the
//! selected items. You don't have permission."). With Developer Mode on, the original
//! detail (OS error numbers, HRESULTs, worker names) follows on a second line. The raw
//! detail is also appended to `foldex.log` whatever the setting is.
//!
//! Anything the classifier does not recognise falls back to "Please try again" instead of
//! leaking internal wording, so a new technical message is safe by default.
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fs,
    hash::{Hash, Hasher},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const FALLBACK: &str = "Something went wrong";
const LOG_LIMIT: u64 = 512 * 1024;
const LOG_MEMORY: usize = 64;

thread_local! {
    // Presentation happens on the UI thread only; workers send raw details. A
    // thread-local keeps parallel tests independent and fails safe (off) elsewhere.
    static DEVELOPER: Cell<bool> = const { Cell::new(false) };
    static LOG: RefCell<Log> = const { RefCell::new(Log { path: None, seen: VecDeque::new() }) };
}

struct Log {
    path: Option<PathBuf>,
    seen: VecDeque<u64>,
}

/// Whether Developer Mode is on for this (UI) thread.
pub fn developer_mode() -> bool {
    DEVELOPER.with(Cell::get)
}

pub fn set_developer_mode(on: bool) {
    DEVELOPER.with(|flag| flag.set(on));
}

/// Where [`present`] records raw details. Called once when the app starts.
pub fn set_log_path(path: PathBuf) {
    LOG.with(|log| log.borrow_mut().path = Some(path));
}

pub fn log_path() -> Option<PathBuf> {
    LOG.with(|log| log.borrow().path.clone())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    Permission,
    InUse,
    NotFound,
    Exists,
    DiskFull,
    BadName,
    TooLong,
    Unavailable,
    Offline,
    ReadOnly,
    Cancelled,
    Busy,
    TooMany,
    Changed,
    Unknown,
}

impl Cause {
    /// A sentence for people, or `None` when nothing useful can be said.
    fn reason(self) -> Option<&'static str> {
        Some(match self {
            Self::Permission => "You don't have permission to do that.",
            Self::InUse => "It's being used by another program.",
            Self::NotFound => "It may have been moved or deleted.",
            Self::Exists => "An item with that name already exists.",
            Self::DiskFull => "There isn't enough space on the drive.",
            Self::BadName => "That name isn't allowed.",
            Self::TooLong => "The path is too long.",
            Self::Unavailable => "The drive or network location isn't available.",
            Self::Offline => "Check your internet connection and try again.",
            Self::ReadOnly => "The drive is write-protected.",
            Self::Cancelled => "The operation was cancelled.",
            Self::Busy => "Foldex is busy. Please try again in a moment.",
            Self::TooMany => "That's too much to handle at once. Try with fewer items.",
            Self::Changed => "Something changed in the meantime. Please try again.",
            Self::Unknown => return None,
        })
    }
}

/// `(os error N)` from a Rust `io::Error`, or the low word of an `0x8007xxxx` HRESULT.
fn win32_code(detail: &str) -> Option<u32> {
    if let Some(at) = detail.find("(os error ") {
        let digits: String = detail[at + 10..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(code) = digits.parse() {
            return Some(code);
        }
    }
    let lower = detail.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(at) = rest.find("0x") {
        let hex: String = rest[at + 2..]
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        if hex.len() == 8
            && let Ok(value) = u32::from_str_radix(&hex, 16)
            && value >> 16 == 0x8007
        {
            return Some(value & 0xffff);
        }
        rest = &rest[at + 2..];
    }
    None
}

fn from_win32(code: u32) -> Cause {
    match code {
        5 | 1314 => Cause::Permission,
        32 | 33 | 1224 => Cause::InUse,
        2 | 3 | 15 | 1168 => Cause::NotFound,
        80 | 183 => Cause::Exists,
        39 | 112 | 1450 => Cause::DiskFull,
        123 | 161 | 267 => Cause::BadName,
        206 => Cause::TooLong,
        21 | 51 | 53 | 55 | 59 | 64 | 67 | 1167 | 1219 | 1222 | 1231 | 1117 | 23 => {
            Cause::Unavailable
        }
        19 => Cause::ReadOnly,
        995 | 1223 => Cause::Cancelled,
        _ => Cause::Unknown,
    }
}

pub fn classify(detail: &str) -> Cause {
    if let Some(cause) = win32_code(detail).map(from_win32)
        && cause != Cause::Unknown
    {
        return cause;
    }
    let text = detail.to_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| text.contains(word));
    if any(&["cancel", "interrupted", "e_abort"]) {
        Cause::Cancelled
    } else if any(&["access is denied", "permission denied", "not permitted"]) {
        Cause::Permission
    } else if any(&["used by another", "sharing violation", "in use"]) {
        Cause::InUse
    } else if any(&["already exists", "file exists"]) {
        Cause::Exists
    } else if any(&["not enough space", "disk full", "no space"]) {
        Cause::DiskFull
    } else if any(&["reserved", "device name", "invalid name", "nonempty name"]) {
        Cause::BadName
    } else if any(&["too long"]) {
        Cause::TooLong
    } else if any(&["network is unavailable", "network path", "network name"]) {
        Cause::Unavailable
    } else if any(&[
        "could not contact",
        "download failed",
        "dns",
        "timed out",
        "connection",
    ]) {
        Cause::Offline
    } else if any(&[
        "cannot find",
        "could not find",
        "not found",
        "does not exist",
        "no longer exist",
        "no longer available",
        "is missing",
    ]) {
        Cause::NotFound
    } else if any(&[
        "busy",
        "try again shortly",
        "already in progress",
        "already running",
        "already open",
        "already moving",
        "queue is full",
        "history is full",
    ]) {
        Cause::Busy
    } else if any(&[
        "budget",
        "exceed",
        "too large",
        "too many",
        "limit",
        "up to 4096",
        "between 1 and",
        "at most",
    ]) {
        Cause::TooMany
    } else if any(&[
        "changed since",
        "changed while",
        "changed after",
        "changed during",
        "no longer applicable",
        "are outside",
        "stale",
    ]) {
        Cause::Changed
    } else {
        Cause::Unknown
    }
}

/// Messages that are already written for people. Shown as they are, never replaced by a
/// generic sentence, so the specific guidance (a name rule, a clipboard hint) survives.
const PLAIN: &[&str] = &[
    "This action supports up to",
    "The selected files are no longer available",
    "Wait for the selected files to finish loading",
    "The search results changed",
    "Select between 1 and",
    "Select at most",
    "No items selected",
    "A drive or share root cannot be",
    "The same source was selected more than once",
    "Creating shortcuts with Ctrl+Shift drag is not supported",
    "Use a nonempty name",
    "This name is reserved by Windows",
    "Windows reserves this device name",
    "Symbolic links, junctions and unknown reparse items are not",
    "Only ordinary files and folders are supported",
    "Clipboard contains no filesystem files",
    "Clipboard changed while reading; try Paste again",
    "The Windows clipboard is busy",
    "Explorer integration is busy",
    "Open a folder",
    "Record a shortcut with a modifier",
    "Choose up to two search folders",
    "Use an absolute path for each search folder",
    "Search folders must fit within",
    "Windows could not position the launcher",
    "The command's panel has closed",
    "A Properties window is already open",
    "Recycled ",
    "Rename was not applied",
    "No names changed",
    "All names are unchanged",
    "The displayed preview is no longer applicable",
    "Destination already exists",
    "A parent folder changed since preview",
    "A selected destination changed since preview",
    "The same item is selected more than once",
    "Selected more than once",
    "Two selected names point to",
    "Same file as another",
    "Two items would end up",
    "Same new name as another",
    "Changed since preview",
    "Search needs",
    "Foldex Setup is",
    "The workspace could not be saved",
    "The workspace could not be queued",
    "Could not prepare the installer",
    "File work started while preparing",
    "This update has changed or is no longer available",
    "This image is too large",
    "Online-only item",
    "Link or special item",
    "Binary file or unsupported encoding",
    "There are too many excluded",
    "The minimum must not exceed",
    "Excluded folders must be",
    "Days must be",
    "Unknown size unit",
    "The originating pane was closed",
    "Operation stopped",
    "Cancelled before starting",
    "Update installation was cancelled",
    "The tab changed while the new window was opening",
    "The source tab was closed",
    "The update worker",
    "Finish file operations",
    "GitHub is limiting update requests",
    "GitHub returned HTTP",
    "Installer download was incomplete",
    "Installer checksum did not match",
    "Download cancelled",
    "The theme library is",
    "Choose a theme file",
    "Use 1",
    "Choose a theme name",
];

/// True for a message already written for people and free of internal detail.
pub fn is_plain(detail: &str) -> bool {
    let detail = detail.trim();
    !detail.contains("os error")
        && !detail.contains("HRESULT")
        && PLAIN.iter().any(|prefix| detail.starts_with(prefix))
}

/// Text this module already produced. Presenting it again returns it unchanged, so a
/// message can be stored in a pane or dialog and shown on every frame.
fn is_presented(text: &str) -> bool {
    [
        "Couldn't ",
        "Something went wrong",
        "Your ",
        "Installation was cancelled because",
    ]
    .iter()
    .any(|start| text.starts_with(start))
}

/// The sentence a normal user sees.
pub fn simple(what: &str, detail: &str) -> String {
    if is_presented(detail.trim()) {
        return detail.trim().to_owned();
    }
    if is_plain(detail) {
        return detail.trim().to_owned();
    }
    let what = what.trim().trim_end_matches('.');
    let what = if what.is_empty() { FALLBACK } else { what };
    match classify(detail).reason() {
        Some(reason) => format!("{what}. {reason}"),
        None => format!("{what}. Please try again."),
    }
}

/// What to put on screen for a failure. `what` names the action in plain words
/// ("Couldn't copy the selected items"); pass `""` when the caller has no context.
/// Safe to call again on its own output.
pub fn present(what: &str, detail: &str) -> String {
    if is_presented(detail.trim()) {
        return detail.trim().to_owned();
    }
    record(what, detail);
    compose(simple(what, detail), detail, developer_mode())
}

/// What to show for a success message that may carry technical notes (undo bookkeeping,
/// unverified Shell statuses). Plain results such as "Copied 3 items" are unchanged.
pub fn present_notice(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() {
        return String::new();
    }
    let simple = if let Some(at) = message.find(". Undo is unavailable") {
        format!("{}. This change can't be undone.", &message[..at])
    } else if let Some(at) = message.find(". Clipboard could not be updated") {
        format!("{}. The clipboard couldn't be updated.", &message[..at])
    } else if message.starts_with("Canceled or stopped:") {
        "Cancelled. Changes already made were kept.".to_owned()
    } else if message.starts_with("Windows ") && message.contains("could not be verified") {
        "Some items may not have finished. Refresh to check.".to_owned()
    } else {
        message.to_owned()
    };
    if simple != message {
        record("", message);
    }
    compose(simple, message, developer_mode())
}

/// For text written by the app but possibly carrying raw system detail (previews, tooltips):
/// authored sentences pass through, anything with OS or HRESULT codes is simplified.
pub fn present_authored(what: &str, text: &str) -> String {
    let technical = text.contains("os error") || text.contains("HRESULT") || text.contains("0x8");
    if technical {
        present(what, text)
    } else {
        text.to_owned()
    }
}

/// A plain sentence with the technical detail attached only in Developer Mode.
pub fn with_detail(simple: &str, detail: &str) -> String {
    record(simple, detail);
    compose(simple.to_owned(), detail, developer_mode())
}

/// Like [`present`], with a closing sentence for the person ("Close again to exit…").
pub fn present_with_note(what: &str, detail: &str, note: &str) -> String {
    record(what, detail);
    let mut simple = simple(what, detail);
    if !note.is_empty() {
        simple.push(' ');
        simple.push_str(note);
    }
    compose(simple, detail, developer_mode())
}

fn compose(simple: String, detail: &str, developer: bool) -> String {
    let detail = detail.trim();
    if !developer || detail.is_empty() || simple == detail {
        simple
    } else {
        format!("{simple}\n{detail}")
    }
}

/// Appends the raw detail to the log once per distinct message, at any setting.
fn record(what: &str, detail: &str) {
    let detail = detail.trim();
    if detail.is_empty() {
        return;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (what, detail).hash(&mut hasher);
    let key = hasher.finish();
    LOG.with(|log| {
        let mut log = log.borrow_mut();
        let Some(path) = log.path.clone() else { return };
        if log.seen.contains(&key) {
            return;
        }
        if log.seen.len() >= LOG_MEMORY {
            log.seen.pop_front();
        }
        log.seen.push_back(key);
        let _ = append(&path, what, detail);
    });
}

fn append(path: &Path, what: &str, detail: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::metadata(path).is_ok_and(|meta| meta.len() > LOG_LIMIT) {
        let _ = fs::rename(path, path.with_extension("old.log"));
    }
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let what = if what.is_empty() { "-" } else { what };
    writeln!(file, "[{seconds}] {what} | {}", detail.replace('\n', " / "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_errors_become_plain_reasons() {
        let simple = simple(
            "Couldn't move the selected items",
            "Access is denied. (os error 5)",
        );
        assert_eq!(
            simple,
            "Couldn't move the selected items. You don't have permission to do that."
        );
        assert_eq!(
            classify(
                "C:\\a: The process cannot access the file because it is being used by another process. (os error 32)"
            ),
            Cause::InUse
        );
        assert_eq!(
            classify("Delete: Windows HRESULT 0x80070070"),
            Cause::DiskFull
        );
        assert_eq!(classify("The file exists. (os error 80)"), Cause::Exists);
    }

    #[test]
    fn unknown_technical_text_never_leaks_by_default() {
        for detail in [
            "inspector worker closed",
            "shell menu mailbox busy; retry on the next frame",
            "Read Shell clipboard: Windows HRESULT 0x8000FFFF",
            "TOML parse error at line 3, column 1",
            "Windows returned an empty file operation",
            "Process diagnostics: GetProcessMemoryInfo: Access is denied. (os error 5)",
        ] {
            let text = simple("", detail);
            assert!(
                !text.contains("worker")
                    && !text.contains("HRESULT")
                    && !text.contains("os error")
                    && !text.contains("TOML")
                    && !text.contains("mailbox"),
                "{detail:?} leaked as {text:?}"
            );
        }
        assert_eq!(
            simple("", "inspector worker closed"),
            "Something went wrong. Please try again."
        );
    }

    /// The technical wording that used to reach people, taken from the toast inventory.
    const TECHNICAL: &[&str] = &[
        "inspector requires an absolute, NUL-free path within 64 KiB",
        "inspector worker closed",
        "Path lookup worker is unavailable",
        "Preview is busy; try again shortly",
        "The selected paths exceed this action's memory budget",
        "The selected rows are outside these search results",
        "Invalid index path: bad cursor",
        "Windows copy completed 1/3 items: Windows HRESULT 0x80004005",
        "Selected paths exceed the 1 MiB operation budget",
        "Settings exceed the 4 MiB save limit",
        "Every source needs a transfer intent",
        "File worker is shutting down",
        "Shell worker is unavailable",
        "Operation history is busy",
        "Cannot open the launcher: The system cannot find the file specified. (os error 2)",
        "Process diagnostics: GetProcessMemoryInfo: Access is denied. (os error 5)",
        "C:\\a\\b: The system cannot find the path specified. (os error 3)",
        "Initialize Windows Shell: Windows HRESULT 0x80004005",
        "Windows returned an empty file operation",
        "Undo requires an absolute, non-root path",
        "The recorded file identity was reused; the replacement was left unchanged",
        "This tree exceeds the bounded Undo entry limit",
        "The change completed, but its inverse is unavailable: Windows did not return the resulting item",
        "Read Shell clipboard: Windows HRESULT 0x8000FFFF",
        "File clipboard exceeds the 1 MiB budget or has an invalid header",
        "Invalid file clipboard header",
        "Cannot lock C:\\x: Reparse points require a separate rename operation",
        "Rollback cannot restore C:\\x: Access is denied. (os error 5)",
        "Recovery item remains at C:\\x\\.foldex-rename-1",
        "Target name is not Unicode",
        "Windows could not open this item (shell code 31)",
        "Reveal item in Windows Explorer failed (Windows HRESULT 0x80070002)",
        "Invalid installed application identifier",
        "open merged Properties failed (HRESULT 0x80004005)",
        "shell menu mailbox busy; retry on the next frame",
        "shell submenu expansion limit reached",
        "native item has no invokable command identifier",
        "Generic CommandStore handler cannot be initialized; invocation is disabled",
        "No Windows drag window",
        "Windows file drag: HRESULT 0x80004005",
        "The new window did not become ready in time",
        "Tab state exceeds the 4 MiB transfer budget",
        "Shared preferences exceed 4 MiB",
        "Unsupported shared preferences format; the original file was preserved",
        "Workspace writer is unavailable",
        "Theme file exceeds 64 KiB",
        "expected newline, found `=` at line 3 column 5",
        "Windows Explorer integration: Access is denied. (os error 5)",
        "The saved Explorer integration backup is invalid.",
        "Explorer registration changed while it was being read. Try again.",
        "Could not contact GitHub: error sending request",
        "Download failed: connection reset",
        "Installer checksum did not match. The download was discarded.",
        "Cannot preview this item: The process cannot access the file because it is being used by another process. (os error 32)",
    ];

    #[test]
    fn nothing_from_the_toast_inventory_leaks_technical_wording() {
        const BANNED: &[&str] = &[
            "os error",
            "HRESULT",
            "0x",
            "worker",
            "mailbox",
            "budget",
            "journal",
            "reparse",
            "identity",
            "KiB",
            "MiB",
            "NUL",
            "serde",
            "Shell",
            "CommandStore",
            "line 3",
            "inverse",
            "cursor",
            "index path",
            "shell code",
            ".foldex-rename",
            "SendTo",
        ];
        for detail in TECHNICAL {
            let text = simple("Couldn't do that", detail);
            for word in BANNED {
                assert!(
                    !text.contains(word),
                    "{detail:?} leaked {word:?} as {text:?}"
                );
            }
            assert!(!text.trim().is_empty());
            // With Developer Mode on the same message keeps the raw detail.
            let developer = compose(text.clone(), detail, true);
            if text != *detail {
                assert!(developer.ends_with(detail), "{developer:?}");
            }
        }
    }

    #[test]
    fn plain_guidance_is_kept() {
        let name = "This name is reserved by Windows";
        assert_eq!(simple("Couldn't rename that item", name), name);
        let hint = "Use a nonempty name up to 255 characters, without / \\ : * ? \" < > | or a final space/dot";
        assert_eq!(simple("Couldn't rename that item", hint), hint);
    }

    #[test]
    fn plain_text_with_a_system_code_is_not_plain() {
        let detail = "Recycled 1/3 items. Windows HRESULT 0x80070005";
        assert!(!is_plain(detail));
        assert_eq!(classify(detail), Cause::Permission);
    }

    #[test]
    fn developer_mode_appends_the_detail_once() {
        let detail = "Access is denied. (os error 5)";
        assert_eq!(compose("Nope.".into(), detail, false), "Nope.");
        assert_eq!(
            compose("Nope.".into(), detail, true),
            "Nope.\nAccess is denied. (os error 5)"
        );
        assert_eq!(compose(detail.into(), detail, true), detail);
    }

    #[test]
    fn developer_mode_is_per_thread_and_off_by_default() {
        assert!(!developer_mode());
        set_developer_mode(true);
        assert!(developer_mode());
        assert!(!std::thread::spawn(developer_mode).join().unwrap());
        set_developer_mode(false);
    }

    #[test]
    fn details_are_logged_once_even_when_off() {
        let dir = std::env::temp_dir().join(format!("foldex-friendly-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        set_log_path(dir.join("foldex.log"));
        let shown = present(
            "Couldn't copy the selected items",
            "Access is denied. (os error 5)",
        );
        present(
            "Couldn't copy the selected items",
            "Access is denied. (os error 5)",
        );
        assert!(!shown.contains("os error"));
        let log = fs::read_to_string(dir.join("foldex.log")).unwrap();
        assert_eq!(log.matches("os error 5").count(), 1);
        let _ = fs::remove_dir_all(&dir);
    }
}
