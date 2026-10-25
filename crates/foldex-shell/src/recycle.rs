//! Lazy access to the real Windows Recycle Bin namespace.
//!
//! Call these blocking functions from a dedicated STA worker, never a paint
//! callback. Each call owns an OLE initialization until all its Shell objects
//! and native dialogs have finished. No work is performed at construction or
//! startup. Paths are display metadata only; operations use native identities.

use std::{
    collections::HashSet,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

/// A successful snapshot contains every enumerated item, within these bounds.
/// Exceeding either bound is an error, never a silently incomplete Restore All.
pub const MAX_ITEMS: usize = 100_000;
pub const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ID_BYTES: usize = 64 * 1024;
pub const CANCELLED: &str = "Recycle Bin operation cancelled";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Item {
    /// Opaque absolute PIDL bytes. Ephemeral, not a path or a persistent key.
    /// Never cast these bytes to a native pointer. Actions re-resolve them.
    pub id: Vec<u8>,
    pub name: String,
    /// Full original path reported by Shell metadata; empty when unavailable.
    /// This must never be used as a restore destination or launch target.
    pub original_path: PathBuf,
    /// UTC Unix seconds; zero when the Shell does not provide a deletion time.
    pub deleted_at: i64,
    pub size: u64,
    pub is_dir: bool,
    /// The optional current backing path reported by the Shell, for display.
    /// Its presence does not authorize normal file operations or activation.
    pub file_system_path: Option<PathBuf>,
}

pub fn enumerate() -> Result<Vec<Item>, String> {
    enumerate_cancellable(&AtomicBool::new(false))
}

/// Cancellation is observed between native calls. A blocked Shell call cannot
/// safely be interrupted. Cancellation returns an error and discards the list.
pub fn enumerate_cancellable(cancel: &AtomicBool) -> Result<Vec<Item>, String> {
    check_cancelled(cancel)?;
    #[cfg(windows)]
    return windows::enumerate(cancel);
    #[cfg(not(windows))]
    Err(unsupported())
}

/// Restores exactly these items using the Recycle Bin's native `undelete` verb.
/// Pass all IDs from a complete snapshot for Restore All. Empty means no-op.
/// Windows owns conflict/progress/cancel UI. Partial completion is possible;
/// callers should refresh after success, cancellation, or any operation error.
pub fn restore(ids: &[Vec<u8>], owner: isize) -> Result<(), String> {
    selection_action(ids, owner, Action::Restore)
}

/// Permanently deletes this selection through IFileOperation. Native warning,
/// progress, error and cancellation UI remain enabled; deletion is irreversible.
pub fn remove(ids: &[Vec<u8>], owner: isize) -> Result<(), String> {
    selection_action(ids, owner, Action::Remove)
}

/// Empties all the current user's drive bins with Windows confirmation and UI.
pub fn empty(owner: isize) -> Result<(), String> {
    empty_action(owner, false)
}

/// Use only after an affirmative application Empty Bin confirmation. Suppresses
/// just the duplicate native confirmation; Windows progress/errors stay enabled.
pub fn empty_confirmed(owner: isize) -> Result<(), String> {
    empty_action(owner, true)
}

fn empty_action(owner: isize, confirmed: bool) -> Result<(), String> {
    #[cfg(windows)]
    return windows::empty(owner, confirmed);
    #[cfg(not(windows))]
    {
        let _ = (owner, confirmed);
        Err(unsupported())
    }
}

/// Opens Properties for the native Recycle Bin selection, not its backing path.
/// The call retains/pumps its STA until native property sheets release it.
pub fn properties(ids: &[Vec<u8>], owner: isize) -> Result<(), String> {
    selection_action(ids, owner, Action::Properties)
}

#[derive(Clone, Copy)]
enum Action {
    Restore,
    Remove,
    Properties,
}

fn selection_action(ids: &[Vec<u8>], owner: isize, action: Action) -> Result<(), String> {
    validate_selection(ids)?;
    if ids.is_empty() {
        return Ok(());
    }
    #[cfg(windows)]
    return windows::selection_action(ids, owner, action);
    #[cfg(not(windows))]
    {
        let _ = (owner, action);
        Err(unsupported())
    }
}

fn validate_selection(ids: &[Vec<u8>]) -> Result<(), String> {
    if ids.len() > MAX_ITEMS {
        return Err("Recycle Bin selection exceeds 100,000 items".into());
    }
    let mut seen = HashSet::with_capacity(ids.len());
    let mut bytes = 0usize;
    for id in ids {
        validate_pidl(id)?;
        if !seen.insert(id.as_slice()) {
            return Err("Recycle Bin selection contains duplicate identities".into());
        }
        bytes = bytes
            .checked_add(id.len())
            .filter(|size| *size <= MAX_SNAPSHOT_BYTES)
            .ok_or("Recycle Bin selection identifiers exceed 64 MiB")?;
    }
    Ok(())
}

/// Checks framing without dereferencing or interpreting an opaque SHITEMID.
/// Structural validity alone never makes caller-supplied PIDLs safe for Shell.
fn validate_pidl(bytes: &[u8]) -> Result<usize, String> {
    if bytes.len() < 4 || bytes.len() > MAX_ID_BYTES {
        return Err("Invalid Recycle Bin identity length".into());
    }
    let mut offset = 0usize;
    let mut elements = 0usize;
    loop {
        let size = bytes
            .get(offset..offset.saturating_add(2))
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]) as usize)
            .ok_or("Truncated Recycle Bin identity")?;
        if size == 0 {
            return if offset + 2 == bytes.len() && elements > 0 {
                Ok(elements)
            } else {
                Err("Invalid Recycle Bin identity terminator".into())
            };
        }
        if size < 2 {
            return Err("Invalid Recycle Bin identity element".into());
        }
        offset = offset
            .checked_add(size)
            .filter(|end| *end <= bytes.len().saturating_sub(2))
            .ok_or("Truncated Recycle Bin identity element")?;
        elements += 1;
    }
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}

#[cfg(any(windows, test))]
fn unix_seconds(filetime: u64) -> i64 {
    ((i128::from(filetime) - 116_444_736_000_000_000i128).div_euclid(10_000_000)) as i64
}

#[cfg(any(windows, test))]
fn failure(operation: &str, hr: i32) -> String {
    match hr as u32 {
        0x8000_4004 | 0x8007_04c7 | 0x8027_0000 => CANCELLED.into(),
        code if code & 0xffff_0000 == 0x8007_0000 => format!(
            "Recycle Bin {operation} failed (0x{code:08X}): {}",
            std::io::Error::from_raw_os_error((code & 0xffff) as i32)
        ),
        code => format!("Recycle Bin {operation} failed (0x{code:08X})"),
    }
}

#[cfg(not(windows))]
fn unsupported() -> String {
    "Native Recycle Bin requires Windows".into()
}

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pidl_framing_accepts_opaque_odd_sized_and_multilevel_items() {
        assert_eq!(validate_pidl(&[3, 0, 0xff, 0, 0]), Ok(1));
        assert_eq!(validate_pidl(&[3, 0, 0xff, 2, 0, 0, 0]), Ok(2));
        assert_eq!(validate_pidl(&[4, 0, 0, 0, 0, 0]), Ok(1));
    }

    #[test]
    fn pidl_framing_rejects_empty_desktop_truncation_and_trailing_data() {
        for bytes in [
            &[][..],
            &[0, 0],
            &[1, 0, 0, 0],
            &[3, 0, 1, 0],
            &[4, 0, 1, 0, 0],
            &[3, 0, 1, 0, 0, 1],
            &[255, 255, 0, 0],
            &[0, 0, 0, 0],
        ] {
            assert!(validate_pidl(bytes).is_err(), "accepted {bytes:?}");
        }
        assert!(validate_pidl(&vec![0; MAX_ID_BYTES + 1]).is_err());
    }

    #[test]
    fn selection_checks_every_identity_and_rejects_duplicates_before_native_work() {
        let id = vec![3, 0, 42, 0, 0];
        assert!(validate_selection(&[id.clone()]).is_ok());
        assert!(validate_selection(&[id.clone(), id]).is_err());
        assert!(validate_selection(&[vec![3, 0, 42, 0, 0], vec![0, 0]]).is_err());
        assert!(validate_selection(&vec![Vec::new(); MAX_ITEMS + 1]).is_err());
        assert_eq!(restore(&[], 0), Ok(()));
        assert_eq!(remove(&[], 0), Ok(()));
        assert_eq!(properties(&[], 0), Ok(()));
    }

    #[test]
    fn cancelled_load_never_initializes_shell() {
        assert_eq!(
            enumerate_cancellable(&AtomicBool::new(true)),
            Err(CANCELLED.into())
        );
    }

    #[test]
    fn filetime_conversion_preserves_utc_and_pre_epoch_times() {
        assert_eq!(unix_seconds(116_444_736_000_000_000), 0);
        assert_eq!(unix_seconds(116_444_736_100_000_000), 10);
        assert_eq!(unix_seconds(116_444_735_999_999_999), -1);
        assert_eq!(unix_seconds(0), -11_644_473_600);
    }

    #[test]
    fn cancellation_hresult_and_other_failures_stay_distinct() {
        for hr in [0x8000_4004u32, 0x8007_04c7, 0x8027_0000] {
            assert_eq!(failure("fixture", hr as i32), CANCELLED);
        }
        assert!(failure("fixture", 0x8000_4005u32 as i32).contains("0x80004005"));
        assert!(failure("fixture", 0x8007_0005u32 as i32).contains("0x80070005"));
    }
}
