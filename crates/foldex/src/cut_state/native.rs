//! Event-only native observer. Listener never opens the clipboard, so delayed
//! rendering on the single reader cannot hide a later clipboard replacement.
use super::*;
use std::{ptr, sync::atomic::AtomicIsize, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::{HANDLE, HGLOBAL, HWND},
    System::{
        DataExchange::{
            AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
            GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
            RegisterClipboardFormatW, RemoveClipboardFormatListener,
        },
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
        Ole::{CF_HDROP, DROPEFFECT_MOVE},
    },
    UI::{
        Shell::{CFSTR_PASTESUCCEEDED, CFSTR_PREFERREDDROPEFFECT, DragQueryFileW},
        WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE, MSG,
            PostMessageW, WM_CLIPBOARDUPDATE, WM_CLOSE,
        },
    },
};

pub(super) struct Observer {
    window: Arc<AtomicIsize>,
}
impl Observer {
    pub(super) fn start(shared: Arc<Shared>, receiver: Receiver<()>) -> io::Result<Self> {
        let window = Arc::new(AtomicIsize::new(0));
        let observer = Self {
            window: window.clone(),
        };
        let reader = shared.clone();
        thread::Builder::new()
            .name("foldex-cut-reader".into())
            .spawn(move || {
                while receiver.recv().is_ok() && !reader.stopped.load(Ordering::Acquire) {
                    let cancel = reader.cancel.swap(0, Ordering::AcqRel);
                    if cancel != 0 {
                        cancel_current((cancel - 1) as u32, &reader);
                    }
                    let generation = reader.generation.load(Ordering::Acquire);
                    // Busy owners get a short bounded retry, with no idle timer.
                    for attempt in 0..6 {
                        if reader.stopped.load(Ordering::Acquire)
                            || reader.generation.load(Ordering::Acquire) != generation
                        {
                            break;
                        }
                        match snapshot(generation) {
                            Ok(snapshot) => {
                                reader.publish(snapshot);
                                break;
                            }
                            Err(error)
                                if error.kind() == io::ErrorKind::WouldBlock && attempt < 5 =>
                            {
                                thread::sleep(Duration::from_millis(10 << attempt));
                            }
                            Err(_) => {
                                reader.publish(CutSnapshot {
                                    generation,
                                    ..Default::default()
                                });
                                break;
                            }
                        }
                    }
                }
            })?;
        if let Err(error) = thread::Builder::new()
            .name("foldex-cut-events".into())
            .spawn(move || {
                listen(shared, window);
            })
        {
            // Dropping the caller's Service signals the already-started reader.
            return Err(error);
        }
        Ok(observer)
    }

    pub(super) fn stop(&self) {
        let window = self.window.load(Ordering::Acquire) as HWND;
        if !window.is_null() {
            unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
        }
    }
}

fn listen(shared: Arc<Shared>, slot: Arc<AtomicIsize>) {
    if shared.stopped.load(Ordering::Acquire) {
        return;
    }
    let name: Vec<_> = "STATIC\0".encode_utf16().collect();
    let window = unsafe {
        CreateWindowExW(
            0,
            name.as_ptr(),
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
        return;
    }
    slot.store(window as isize, Ordering::Release);
    if unsafe { AddClipboardFormatListener(window) } != 0 {
        shared.changed(); // Read the current selection once, including a pre-existing cut.
        let mut message = MSG::default();
        while !shared.stopped.load(Ordering::Acquire)
            && unsafe { GetMessageW(&mut message, ptr::null_mut(), 0, 0) } > 0
        {
            if message.message == WM_CLOSE {
                break;
            }
            if message.message == WM_CLIPBOARDUPDATE {
                shared.changed();
            } else {
                unsafe { DispatchMessageW(&message) };
            }
        }
        unsafe { RemoveClipboardFormatListener(window) };
    }
    slot.store(0, Ordering::Release);
    unsafe { DestroyWindow(window) };
}

struct Clipboard;
impl Clipboard {
    fn open() -> io::Result<Self> {
        if unsafe { OpenClipboard(ptr::null_mut()) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Clipboard is busy",
            ));
        }
        Ok(Self)
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn registered(name: *const u16) -> io::Result<u32> {
    let format = unsafe { RegisterClipboardFormatW(name) };
    if format == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(format)
    }
}

fn effect(format: u32) -> Option<u32> {
    if unsafe { IsClipboardFormatAvailable(format) } == 0 {
        return None;
    }
    let memory = unsafe { GetClipboardData(format) } as HGLOBAL;
    if memory.is_null() || unsafe { GlobalSize(memory) } < 4 {
        return None;
    }
    let data = unsafe { GlobalLock(memory) };
    if data.is_null() {
        return None;
    }
    let effect = unsafe { data.cast::<u32>().read_unaligned() };
    unsafe { GlobalUnlock(memory) };
    Some(effect)
}

fn pending_move(preferred: Option<u32>, pasted: Option<u32>) -> bool {
    preferred == Some(DROPEFFECT_MOVE) && pasted != Some(DROPEFFECT_MOVE)
}

fn snapshot(generation: u64) -> io::Result<CutSnapshot> {
    let preferred = registered(CFSTR_PREFERREDDROPEFFECT)?;
    let pasted = registered(CFSTR_PASTESUCCEEDED)?;
    let _clipboard = Clipboard::open()?;
    let before = unsafe { GetClipboardSequenceNumber() };
    let mut snapshot = CutSnapshot {
        generation,
        sequence: before,
        ..Default::default()
    };
    let preferred = effect(preferred);
    if preferred == Some(DROPEFFECT_MOVE)
        && pending_move(preferred, effect(pasted))
        && unsafe { IsClipboardFormatAvailable(CF_HDROP as u32) } != 0
    {
        let drop = unsafe { GetClipboardData(CF_HDROP as u32) };
        if drop.is_null() {
            return Err(invalid("Cut clipboard has no file data"));
        }
        read_paths(drop, &mut snapshot)?;
    }
    if before != unsafe { GetClipboardSequenceNumber() } {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "Cut clipboard changed while reading",
        ));
    }
    Ok(snapshot)
}

fn read_paths(drop: HANDLE, snapshot: &mut CutSnapshot) -> io::Result<()> {
    let size = unsafe { GlobalSize(drop) };
    if !(20..=MAX_PATH_BYTES).contains(&size) {
        return Err(invalid("Oversized cut clipboard"));
    }
    let data = unsafe { GlobalLock(drop) };
    if data.is_null() {
        return Err(io::Error::last_os_error());
    }
    let valid = validate_payload(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) });
    unsafe { GlobalUnlock(drop) };
    valid?;
    let count = unsafe { DragQueryFileW(drop, u32::MAX, ptr::null_mut(), 0) } as usize;
    if count == 0 || count > MAX_PATHS {
        return Err(invalid("Invalid cut path count"));
    }
    let mut total = 0usize;
    for index in 0..count {
        let length = unsafe { DragQueryFileW(drop, index as u32, ptr::null_mut(), 0) } as usize;
        total = total.saturating_add(length.saturating_add(1).saturating_mul(2));
        if length == 0 || length > MAX_PATH_UNITS || total > MAX_PATH_BYTES {
            return Err(invalid("Invalid cut path length"));
        }
        let mut units = vec![0u16; length + 1];
        if unsafe { DragQueryFileW(drop, index as u32, units.as_mut_ptr(), units.len() as u32) }
            as usize
            != length
        {
            return Err(invalid("Cut path changed while reading"));
        }
        units.truncate(length);
        // CF_HDROP is documented to contain fully qualified paths. Reject
        // malformed relative/device data rather than matching a basename.
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        if !PathBuf::from(OsString::from_wide(&units)).is_absolute() {
            return Err(invalid("Cut clipboard contains a relative path"));
        }
        snapshot.insert(PathKey::from_units(&units).ok_or_else(|| invalid("Invalid cut path"))?)?;
    }
    Ok(())
}

fn validate_payload(bytes: &[u8]) -> io::Result<()> {
    if !(20..=MAX_PATH_BYTES).contains(&bytes.len()) {
        return Err(invalid("Invalid DROPFILES size"));
    }
    let offset = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    if offset < 20 || offset >= bytes.len() {
        return Err(invalid("Invalid DROPFILES offset"));
    }
    let wide = i32::from_le_bytes(bytes[16..20].try_into().unwrap()) != 0;
    let list = &bytes[offset..];
    let terminated = if wide {
        offset.is_multiple_of(2)
            && list.len() >= 4
            && list
                .chunks_exact(2)
                .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                .try_fold(false, |zero, unit| {
                    if zero && unit == 0 {
                        Err(())
                    } else {
                        Ok(unit == 0)
                    }
                })
                .is_err()
    } else {
        list.windows(2).any(|pair| pair == [0, 0])
    };
    if !terminated {
        return Err(invalid("Unterminated DROPFILES payload"));
    }
    Ok(())
}

fn can_cancel(expected: u32, actual: u32, preferred: Option<u32>) -> bool {
    expected == actual && preferred == Some(DROPEFFECT_MOVE)
}

fn cancel_current(expected: u32, shared: &Shared) {
    let Ok(preferred) = registered(CFSTR_PREFERREDDROPEFFECT) else {
        return;
    };
    for attempt in 0..6 {
        if shared.stopped.load(Ordering::Acquire) {
            return;
        }
        if let Ok(_clipboard) = Clipboard::open() {
            // Reading a delayed effect can advance the sequence, so compare
            // after it has been rendered and immediately before EmptyClipboard.
            let preferred = effect(preferred);
            if can_cancel(expected, unsafe { GetClipboardSequenceNumber() }, preferred) {
                unsafe { EmptyClipboard() };
            }
            return;
        }
        if attempt < 5 {
            thread::sleep(Duration::from_millis(10 << attempt));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_pending_exact_move_is_dimmed() {
        for preferred in [None, Some(0), Some(1), Some(3), Some(4), Some(0x80000002)] {
            assert!(!pending_move(preferred, None));
        }
        assert!(pending_move(Some(DROPEFFECT_MOVE), None));
        assert!(pending_move(Some(DROPEFFECT_MOVE), Some(0)));
        assert!(!pending_move(Some(DROPEFFECT_MOVE), Some(DROPEFFECT_MOVE)));
    }

    #[test]
    fn cancellation_never_clears_a_replaced_or_copy_clipboard() {
        assert!(can_cancel(7, 7, Some(DROPEFFECT_MOVE)));
        assert!(!can_cancel(7, 8, Some(DROPEFFECT_MOVE)));
        assert!(!can_cancel(7, 7, Some(1)));
        assert!(!can_cancel(7, 7, None));
        assert!(!can_cancel(u32::MAX, 0, Some(DROPEFFECT_MOVE)));
    }

    #[test]
    fn malformed_or_oversized_dropfiles_are_rejected_without_clipboard_access() {
        let mut bytes = vec![0u8; 20];
        bytes[..4].copy_from_slice(&20u32.to_le_bytes());
        bytes[16..20].copy_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[65, 0, 0, 0, 0, 0]);
        assert!(validate_payload(&bytes).is_ok());
        assert!(validate_payload(&bytes[..24]).is_err());
        bytes[..4].copy_from_slice(&21u32.to_le_bytes());
        assert!(validate_payload(&bytes).is_err());
        assert!(validate_payload(&vec![0; MAX_PATH_BYTES + 1]).is_err());
    }
}
