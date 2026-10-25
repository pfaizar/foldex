//! OLE clipboard reads retain the original IDataObject for Shell feedback.
//! Writes are rendered before leaving this job's STA; there is no resident
//! delayed-rendering provider blocked on the file worker's receive queue.

use super::*;
use std::{ffi::OsString, fs, os::windows::ffi::OsStringExt};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HANDLE, HGLOBAL},
    System::{
        Com::{DVASPECT_CONTENT, FORMATETC, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL},
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardSequenceNumber, OpenClipboard,
            RegisterClipboardFormatW, SetClipboardData,
        },
        Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{
            CF_HDROP, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE, OleGetClipboard,
            ReleaseStgMedium,
        },
    },
    UI::{
        Shell::{
            CFSTR_LOGICALPERFORMEDDROPEFFECT, CFSTR_PASTESUCCEEDED, CFSTR_PERFORMEDDROPEFFECT,
            CFSTR_PREFERREDDROPEFFECT, DragQueryFileW,
        },
        WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, HWND_MESSAGE, MWMO_INPUTAVAILABLE,
            MsgWaitForMultipleObjectsEx, QS_ALLINPUT,
        },
    },
};

#[repr(C)]
struct DataObjectVtable {
    unknown: UnknownVtable,
    get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> i32, // 3
    get_data_here: usize,                                                                      // 4
    query_get_data: usize,                                                                     // 5
    get_canonical_format: usize,                                                               // 6
    set_data:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, *const STGMEDIUM, i32) -> i32, // 7
    remaining: [usize; 4], // EnumFormatEtc, DAdvise, DUnadvise, EnumDAdvise
}

fn format(format: u32) -> FORMATETC {
    FORMATETC {
        cfFormat: format as u16,
        ptd: ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed: TYMED_HGLOBAL as u32,
    }
}

struct Medium(STGMEDIUM);
impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}
fn get_data(object: &ComPointer, clipboard_format: u32) -> io::Result<Medium> {
    let mut medium = STGMEDIUM::default();
    hresult(
        unsafe {
            (object.vtable::<DataObjectVtable>().get_data)(
                object.0,
                &format(clipboard_format),
                &mut medium,
            )
        },
        "Read Shell clipboard data",
    )?;
    let medium = Medium(medium);
    if medium.0.tymed != TYMED_HGLOBAL as u32 || unsafe { medium.0.u.hGlobal }.is_null() {
        return Err(invalid(
            "The Shell clipboard returned an unsupported storage medium",
        ));
    }
    Ok(medium)
}

struct GlobalMemory(HGLOBAL);
impl GlobalMemory {
    fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let memory = Self(unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) });
        if memory.0.is_null() {
            return Err(io::Error::last_os_error());
        }
        let data = unsafe { GlobalLock(memory.0) };
        if data.is_null() {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            ptr::copy_nonoverlapping(bytes.as_ptr(), data.cast::<u8>(), bytes.len());
            GlobalUnlock(memory.0);
        }
        Ok(memory)
    }
    fn transfer(mut self, clipboard_format: u32) -> io::Result<()> {
        if unsafe { SetClipboardData(clipboard_format, self.0 as HANDLE) }.is_null() {
            return Err(io::Error::last_os_error());
        }
        self.0 = ptr::null_mut();
        Ok(())
    }
}
impl Drop for GlobalMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { GlobalFree(self.0) };
        }
    }
}

fn registered(name: *const u16) -> io::Result<u32> {
    let value = unsafe { RegisterClipboardFormatW(name) };
    if value == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

fn set_effect(object: &ComPointer, name: *const u16, effect: u32) -> io::Result<()> {
    let clipboard_format = registered(name)?;
    let mut memory = GlobalMemory::from_bytes(&effect.to_le_bytes())?;
    let medium = STGMEDIUM {
        tymed: TYMED_HGLOBAL as u32,
        u: STGMEDIUM_0 { hGlobal: memory.0 },
        pUnkForRelease: ptr::null_mut(),
    };
    hresult(
        unsafe {
            (object.vtable::<DataObjectVtable>().set_data)(
                object.0,
                &format(clipboard_format),
                &medium,
                1,
            )
        },
        "Report Shell clipboard effect",
    )?;
    // fRelease=TRUE transfers the medium only when SetData succeeds.
    memory.0 = ptr::null_mut();
    Ok(())
}

fn read_effect(object: &ComPointer, name: *const u16) -> Option<u32> {
    let medium = get_data(object, registered(name).ok()?).ok()?;
    let global = unsafe { medium.0.u.hGlobal };
    if unsafe { GlobalSize(global) } < 4 {
        return None;
    }
    let data = unsafe { GlobalLock(global) };
    if data.is_null() {
        return None;
    }
    let effect = unsafe { ptr::read_unaligned(data.cast::<u32>()) };
    unsafe { GlobalUnlock(global) };
    Some(effect)
}

fn encode_drop_files(paths: &[PathBuf]) -> io::Result<Vec<u8>> {
    let mut bytes = vec![0; 20]; // DROPFILES: DWORD, POINT, BOOL, BOOL
    bytes[..4].copy_from_slice(&20u32.to_le_bytes());
    bytes[16..20].copy_from_slice(&1i32.to_le_bytes()); // fWide
    for path in paths {
        let units = wide(path)?;
        if bytes
            .len()
            .saturating_add(units.len().saturating_mul(2))
            .saturating_add(2)
            > MAX_PATH_BYTES
        {
            return Err(invalid("Selected paths exceed the 1 MiB clipboard budget"));
        }
        for unit in units {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    Ok(bytes)
}

fn read_paths(object: &ComPointer) -> io::Result<Vec<PathBuf>> {
    let medium = get_data(object, CF_HDROP as u32)
        .map_err(|_| invalid("Clipboard contains no filesystem files to paste"))?;
    let drop = unsafe { medium.0.u.hGlobal };
    let size = unsafe { GlobalSize(drop) };
    if !(20..=MAX_PATH_BYTES).contains(&size) {
        return Err(invalid(
            "File clipboard exceeds the 1 MiB budget or has an invalid header",
        ));
    }
    let locked = unsafe { GlobalLock(drop) };
    if locked.is_null() {
        return Err(io::Error::last_os_error());
    }
    let valid =
        validate_drop_payload(unsafe { std::slice::from_raw_parts(locked.cast::<u8>(), size) });
    unsafe { GlobalUnlock(drop) };
    valid?;
    let count = unsafe { DragQueryFileW(drop, u32::MAX, ptr::null_mut(), 0) } as usize;
    if count == 0 || count > MAX_SELECTION {
        return Err(invalid("Clipboard must contain between 1 and 4,096 files"));
    }
    let mut paths = Vec::with_capacity(count);
    let mut units = 0usize;
    for index in 0..count {
        let length = unsafe { DragQueryFileW(drop, index as u32, ptr::null_mut(), 0) } as usize;
        units = units.saturating_add(length.saturating_add(1));
        if length == 0 || length > 32_767 || units.saturating_mul(2) > MAX_PATH_BYTES {
            return Err(invalid(
                "File clipboard contains an invalid or oversized path",
            ));
        }
        let mut name = vec![0u16; length + 1];
        let copied =
            unsafe { DragQueryFileW(drop, index as u32, name.as_mut_ptr(), name.len() as u32) }
                as usize;
        if copied != length {
            return Err(invalid("File clipboard changed while reading"));
        }
        name.truncate(length);
        paths.push(PathBuf::from(OsString::from_wide(&name)));
    }
    validate_paths(&paths)?;
    Ok(paths)
}

fn validate_drop_payload(bytes: &[u8]) -> io::Result<()> {
    if !(20..=MAX_PATH_BYTES).contains(&bytes.len()) {
        return Err(invalid("Invalid file clipboard header"));
    }
    let offset = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if offset < 20 || offset >= bytes.len() {
        return Err(invalid("Invalid file clipboard path offset"));
    }
    let wide = i32::from_le_bytes(bytes[16..20].try_into().unwrap()) != 0;
    let list = &bytes[offset..];
    let terminated = if wide {
        offset.is_multiple_of(2)
            && list.len() >= 4
            && list
                .chunks_exact(2)
                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                .try_fold(false, |previous_zero, unit| {
                    if previous_zero && unit == 0 {
                        Err(())
                    } else {
                        Ok(unit == 0)
                    }
                })
                .is_err()
    } else {
        list.windows(2).any(|units| units == [0, 0])
    };
    if !terminated {
        return Err(invalid(
            "File clipboard paths are not double-null terminated",
        ));
    }
    Ok(())
}

struct ClipboardGuard;
impl ClipboardGuard {
    fn open(owner: HWND) -> io::Result<Self> {
        for attempt in 0..5 {
            if unsafe { OpenClipboard(owner) } != 0 {
                return Ok(Self);
            }
            if attempt < 4 {
                // Pump this STA while retrying; do not block source COM calls.
                unsafe {
                    MsgWaitForMultipleObjectsEx(
                        0,
                        ptr::null(),
                        10,
                        QS_ALLINPUT,
                        MWMO_INPUTAVAILABLE,
                    )
                };
                pump_messages();
            }
        }
        Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "The Windows clipboard is busy; try again",
        ))
    }
}
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

struct ClipboardWindow(HWND);
impl ClipboardWindow {
    fn new() -> io::Result<Self> {
        let class: Vec<_> = "STATIC\0".encode_utf16().collect();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
            )
        };
        if window.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self(window))
        }
    }
}
impl Drop for ClipboardWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

fn write_immediate(paths: &[PathBuf], cut: bool, expected_sequence: Option<u32>) -> io::Result<()> {
    let data = if paths.is_empty() {
        None
    } else {
        Some(GlobalMemory::from_bytes(&encode_drop_files(paths)?)?)
    };
    let effect = GlobalMemory::from_bytes(
        &(if cut {
            DROPEFFECT_MOVE
        } else {
            DROPEFFECT_COPY
        })
        .to_le_bytes(),
    )?;
    let preferred = registered(CFSTR_PREFERREDDROPEFFECT)?;
    let owner = ClipboardWindow::new()?;
    let _clipboard = ClipboardGuard::open(owner.0)?;
    if expected_sequence.is_some_and(|sequence| sequence != unsafe { GetClipboardSequenceNumber() })
    {
        return Ok(());
    }
    if unsafe { EmptyClipboard() } == 0 {
        return Err(io::Error::last_os_error());
    }
    if let Some(data) = data {
        data.transfer(CF_HDROP as u32)?;
        effect.transfer(preferred)?;
    }
    Ok(())
}

pub(in crate::platform) fn write_clipboard(paths: &[PathBuf], cut: bool) -> io::Result<String> {
    validate_paths(paths)?;
    let _apartment = Apartment::new()?;
    // Validate Shell identities without following filesystem links via
    // canonicalize. Immediate CF_HDROP supports flat and mixed-folder sources.
    for path in paths {
        let _item = shell_item(path)?;
    }
    write_immediate(paths, cut, None)?;
    Ok(format!(
        "{} {} item{} to clipboard",
        if cut { "Cut" } else { "Copied" },
        paths.len(),
        if paths.len() == 1 { "" } else { "s" }
    ))
}

fn feedback(data: &ComPointer, cut: bool, success: bool) {
    // IFileOperation.MoveItem performs an optimized move: it already removes
    // originals. PERFORMED=MOVE would ask the source to delete them a second
    // time. LOGICAL=MOVE describes the user's result; PASTESUCCEEDED completes
    // cut highlighting only after the entire selection actually succeeded.
    send_feedback(cut, success, |name, effect| set_effect(data, name, effect));
}

fn send_feedback(cut: bool, success: bool, mut set: impl FnMut(*const u16, u32) -> io::Result<()>) {
    let (performed, logical, pasted) = feedback_effects(cut, success);
    let acknowledged = set(CFSTR_PERFORMEDDROPEFFECT, performed).is_ok();
    let _ = set(CFSTR_LOGICALPERFORMEDDROPEFFECT, logical);
    if let Some(pasted) = pasted {
        // A source can retain a PERFORMED=MOVE value from an earlier paste.
        // Unless it accepted our optimized-move acknowledgement, do not send
        // PASTESUCCEEDED=MOVE and risk asking it to delete the source again.
        let pasted = if cut && !acknowledged {
            DROPEFFECT_NONE
        } else {
            pasted
        };
        let _ = set(CFSTR_PASTESUCCEEDED, pasted);
    }
}

fn feedback_effects(cut: bool, success: bool) -> (u32, u32, Option<u32>) {
    let logical = if !success {
        DROPEFFECT_NONE
    } else if cut {
        DROPEFFECT_MOVE
    } else {
        DROPEFFECT_COPY
    };
    let performed = if cut { DROPEFFECT_NONE } else { logical };
    (
        performed,
        logical,
        cut.then_some(if success {
            DROPEFFECT_MOVE
        } else {
            DROPEFFECT_NONE
        }),
    )
}

fn remaining_cut_sources(paths: &[PathBuf], outcomes: &[u8]) -> Vec<PathBuf> {
    paths.iter().zip(outcomes).filter(|(path, outcome)| {
        // Require both Shell completion and absence of the source. Preserve
        // skipped/canceled/uncertain sources and sources recreated mid-move.
        retain_cut_source(**outcome, matches!(fs::symlink_metadata(path), Err(error) if error.kind() == io::ErrorKind::NotFound))
    }).map(|(path, _)| path.clone()).collect()
}

fn retain_cut_source(outcome: u8, source_missing: bool) -> bool {
    outcome != SUCCEEDED || !source_missing
}

#[cfg(test)]
pub(in crate::platform) fn paste(
    destination: &Path,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> io::Result<String> {
    paste_tracked(destination, owner, stop).result
}

pub(in crate::platform) fn paste_tracked(
    destination: &Path,
    owner: isize,
    stop: Arc<AtomicBool>,
) -> Mutation {
    let attempt = || -> io::Result<Mutation> {
        let _apartment = Apartment::new()?;
        check_stop(&stop)?;
        let (data, paths, cut, sequence) = read_snapshot()?;
        let inputs = history::transfer_inputs(&paths, destination, &vec![cut; paths.len()]);
        let operation = transfer_report(&paths, destination, &vec![cut; paths.len()], owner, stop);
        let report = match operation {
            Ok(report) => report,
            Err(error) => {
                feedback(&data, cut, false);
                return Err(error);
            }
        };
        let mut clipboard_warning = None;
        if cut {
            let remaining = remaining_cut_sources(&paths, &report.outcomes);
            // A canceled/no-op move that changed nothing leaves the original
            // clipboard object intact, including any source-specific formats.
            if remaining.len() != paths.len()
                && let Err(error) = write_immediate(&remaining, true, Some(sequence))
            {
                clipboard_warning = Some(format!("Clipboard could not be updated: {error}"));
            }
        }
        // Replacing the still-current cut selection precedes source feedback:
        // SetData can itself change the clipboard sequence or complete Explorer's
        // delete-on-paste protocol. It must not invalidate our original guard.
        feedback(&data, cut, report.full_success());
        let mut mutation =
            history::mutation(report, inputs, if cut { "Moved" } else { "Copied" }, false);
        if let Some(warning) = clipboard_warning {
            mutation.note(warning);
        }
        Ok(mutation)
    };
    attempt().unwrap_or_else(Mutation::failed)
}

fn read_snapshot() -> io::Result<(ComPointer, Vec<PathBuf>, bool, u32)> {
    for _ in 0..3 {
        let before = unsafe { GetClipboardSequenceNumber() };
        let mut data = ptr::null_mut();
        hresult(
            unsafe { OleGetClipboard(&mut data) },
            "Read Shell clipboard",
        )?;
        let data = ComPointer::checked(data, "clipboard data object")?;
        let paths = read_paths(&data)?;
        // Only the exact MOVE preference requests a cut. Combined or missing
        // preferences conservatively copy, following Shell's contract.
        let cut = read_effect(&data, CFSTR_PREFERREDDROPEFFECT) == Some(DROPEFFECT_MOVE);
        let after = unsafe { GetClipboardSequenceNumber() };
        // Delayed rendering can increment the sequence too. Retry with the
        // settled object; never pair an old object with a newer user's stamp.
        if before == after {
            return Ok((data, paths, cut, after));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "Clipboard changed while reading; try Paste again",
    ))
}

#[cfg(test)]
pub(in crate::platform) fn clipboard_encoding_for_test(paths: &[PathBuf]) -> io::Result<Vec<u8>> {
    encode_drop_files(paths)
}

#[cfg(test)]
#[path = "windows_clipboard_memory_tests.rs"]
mod memory_data_object_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optimized_move_feedback_never_requests_source_deletion() {
        assert_eq!(
            feedback_effects(true, true),
            (DROPEFFECT_NONE, DROPEFFECT_MOVE, Some(DROPEFFECT_MOVE))
        );
        assert_eq!(
            feedback_effects(true, false),
            (DROPEFFECT_NONE, DROPEFFECT_NONE, Some(DROPEFFECT_NONE))
        );
        assert_eq!(
            feedback_effects(false, true),
            (DROPEFFECT_COPY, DROPEFFECT_COPY, None)
        );
        assert_eq!(
            feedback_effects(false, false),
            (DROPEFFECT_NONE, DROPEFFECT_NONE, None)
        );
    }

    #[test]
    fn rejected_optimized_move_acknowledgement_never_signals_delete_on_paste() {
        let mut calls = Vec::new();
        send_feedback(true, true, |name, effect| {
            calls.push((name as usize, effect));
            if calls.len() == 1 {
                Err(io::Error::other("source refused feedback"))
            } else {
                Ok(())
            }
        });
        assert_eq!(calls.last().unwrap().1, DROPEFFECT_NONE);
        assert!(
            calls
                .iter()
                .all(|(name, effect)| *name != CFSTR_PERFORMEDDROPEFFECT as usize
                    || *effect != DROPEFFECT_MOVE)
        );
    }

    #[test]
    fn cut_sources_require_both_shell_completion_and_confirmed_absence() {
        assert!(!retain_cut_source(SUCCEEDED, true));
        assert!(retain_cut_source(SUCCEEDED, false));
        for outcome in [PENDING, SKIPPED, FAILED] {
            assert!(retain_cut_source(outcome, false));
            assert!(retain_cut_source(outcome, true));
        }
    }

    #[test]
    fn malformed_drop_payloads_are_rejected_before_native_path_queries() {
        let valid = encode_drop_files(&[PathBuf::from(r"C:\fixture\résumé.txt")]).unwrap();
        assert!(validate_drop_payload(&valid).is_ok());
        let mut invalid_offset = valid.clone();
        invalid_offset[..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(validate_drop_payload(&invalid_offset).is_err());
        let mut header_offset = valid.clone();
        header_offset[..4].copy_from_slice(&4u32.to_le_bytes());
        assert!(validate_drop_payload(&header_offset).is_err());
        assert!(validate_drop_payload(&valid[..valid.len() - 2]).is_err());
        let mut unterminated = valid;
        for byte in &mut unterminated[20..] {
            *byte = 65;
        }
        assert!(validate_drop_payload(&unterminated).is_err());
    }

    #[test]
    fn data_object_vtable_uses_sdk_get_and_set_slots() {
        let slot = std::mem::size_of::<usize>();
        assert_eq!(std::mem::offset_of!(DataObjectVtable, get_data), 3 * slot);
        assert_eq!(std::mem::offset_of!(DataObjectVtable, set_data), 7 * slot);
        assert_eq!(std::mem::size_of::<DataObjectVtable>(), 12 * slot);
    }

    #[test]
    #[ignore = "Opt-in native clipboard probe; requires a private noninteractive window station"]
    fn native_fixture_clipboard_copy_cut_cancel_and_replacement_are_safe() {
        use super::super::native_probe_tests::{Fixture, private_clipboard_required};
        private_clipboard_required();
        let fixture = Fixture::new("clipboard");
        let source = fixture.root.join("résumé.txt");
        let replacement = fixture.root.join("new clipboard.txt");
        let destination = fixture.root.join("destination");
        fs::create_dir(&destination).unwrap();
        fs::write(&source, b"clipboard payload").unwrap();
        fs::write(&replacement, b"new clipboard payload").unwrap();

        write_clipboard(&[source.clone()], false).unwrap();
        paste(&fixture.root, 0, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"clipboard payload");
        assert_eq!(
            fs::read_dir(&fixture.root)
                .unwrap()
                .filter(|item| item.as_ref().unwrap().path().is_file())
                .count(),
            3
        );

        write_clipboard(&[source.clone()], true).unwrap();
        let before = unsafe { GetClipboardSequenceNumber() };
        let error = paste(&destination, 0, Arc::new(AtomicBool::new(true))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(before, unsafe { GetClipboardSequenceNumber() });
        assert_eq!(fs::read(&source).unwrap(), b"clipboard payload");

        paste(&destination, 0, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(!source.exists());
        assert_eq!(
            fs::read(destination.join("résumé.txt")).unwrap(),
            b"clipboard payload"
        );

        write_clipboard(&[replacement.clone()], false).unwrap();
        let old = unsafe { GetClipboardSequenceNumber() };
        let moved = destination.join("résumé.txt");
        write_clipboard(&[moved.clone()], false).unwrap();
        write_immediate(&[], true, Some(old)).unwrap();
        let _apartment = Apartment::new().unwrap();
        let (_, paths, cut, _) = read_snapshot().unwrap();
        assert_eq!(paths, vec![moved]);
        assert!(!cut);
    }
}
