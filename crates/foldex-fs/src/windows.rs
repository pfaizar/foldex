use super::*;
use std::{
    cell::RefCell,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    ptr,
};
use windows_sys::Win32::{
    Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
    Storage::FileSystem::*,
};

type Buffer = Box<[u64; BUFFER_BYTES / 8]>;
thread_local! {static BUFFERS:RefCell<Vec<Buffer>>=const{RefCell::new(Vec::new())};}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let absolute: PathBuf = std::path::absolute(path)?;
    let mut wide: Vec<_> = absolute
        .as_os_str()
        .encode_wide()
        .map(|unit| if unit == 47 { 92 } else { unit })
        .collect();
    if wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"));
    }
    if !wide.starts_with(&[92, 92, 63, 92]) && !wide.starts_with(&[92, 92, 46, 92]) {
        let mut extended = if wide.starts_with(&[92, 92]) {
            vec![92, 92, 63, 92, 85, 78, 67, 92]
        } else {
            vec![92, 92, 63, 92]
        };
        extended.extend_from_slice(if wide.starts_with(&[92, 92]) {
            &wide[2..]
        } else {
            &wide
        });
        wide = extended;
    }
    wide.push(0);
    Ok(wide)
}

struct FindHandle(windows_sys::Win32::Foundation::HANDLE);
// SAFETY: find handles are thread-agnostic. The owner cannot be cloned and
// FindNextFileW is called only through an exclusive mutable reader reference.
unsafe impl Send for FindHandle {}
impl Drop for FindHandle {
    fn drop(&mut self) {
        unsafe {
            FindClose(self.0);
        }
    }
}

/// Same cached WIN32_FIND_DATAW metadata as std, with background bulk fetching.
pub struct FindReader {
    handle: Option<FindHandle>,
    first: Option<WIN32_FIND_DATAW>,
    ended: bool,
}
impl FindReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        if path.as_os_str().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Empty directory path",
            ));
        }
        let pattern = wide_path(&path.join("*"))?;
        let mut data = WIN32_FIND_DATAW::default();
        // SAFETY: the NUL-terminated UTF-16 pattern and output record remain
        // valid for this synchronous call. The find handle has its own RAII owner.
        let mut raw = unsafe {
            FindFirstFileExW(
                pattern.as_ptr(),
                FindExInfoBasic,
                (&mut data as *mut WIN32_FIND_DATAW).cast(),
                FindExSearchNameMatch,
                ptr::null(),
                FIND_FIRST_EX_LARGE_FETCH,
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            if matches!(error.raw_os_error(), Some(50 | 87)) {
                raw = unsafe {
                    FindFirstFileExW(
                        pattern.as_ptr(),
                        FindExInfoBasic,
                        (&mut data as *mut WIN32_FIND_DATAW).cast(),
                        FindExSearchNameMatch,
                        ptr::null(),
                        0,
                    )
                };
            } else if error.raw_os_error() != Some(2) {
                return Err(error);
            }
            if raw == INVALID_HANDLE_VALUE {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(2) {
                    return Err(error);
                }
                return Ok(Self {
                    handle: None,
                    first: None,
                    ended: true,
                });
            }
        }
        Ok(Self {
            handle: Some(FindHandle(raw)),
            first: Some(data),
            ended: false,
        })
    }
    pub fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        loop {
            if self.ended {
                return Ok(None);
            }
            let data = if let Some(first) = self.first.take() {
                first
            } else {
                let mut data = WIN32_FIND_DATAW::default();
                if unsafe { FindNextFileW(self.handle.as_ref().unwrap().0, &mut data) } == 0 {
                    self.ended = true;
                    let error = io::Error::last_os_error();
                    return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                        Ok(None)
                    } else {
                        Err(error)
                    };
                }
                data
            };
            let Some(end) = data.cFileName.iter().position(|unit| *unit == 0) else {
                self.ended = true;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Unterminated filename",
                ));
            };
            let name = &data.cFileName[..end];
            if name == [46] || name == [46, 46] {
                continue;
            }
            if name.is_empty() || name.iter().any(|unit| matches!(*unit, 47 | 92 | 58)) {
                self.ended = true;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invalid filename component",
                ));
            }
            let modified = (u64::from(data.ftLastWriteTime.dwHighDateTime) << 32)
                | u64::from(data.ftLastWriteTime.dwLowDateTime);
            return Ok(Some(Entry {
                name: OsString::from_wide(name),
                attributes: data.dwFileAttributes,
                size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
                modified_secs: ((i128::from(modified) - 116_444_736_000_000_000i128)
                    .div_euclid(10_000_000)) as i64,
                created_secs: Some(filetime_secs(
                    (u64::from(data.ftCreationTime.dwHighDateTime) << 32)
                        | u64::from(data.ftCreationTime.dwLowDateTime),
                )),
                accessed_secs: Some(filetime_secs(
                    (u64::from(data.ftLastAccessTime.dwHighDateTime) << 32)
                        | u64::from(data.ftLastAccessTime.dwLowDateTime),
                )),
                metadata_known: true,
            }));
        }
    }
}

pub struct Reader {
    guard: OwnedHandle,
    buffer: Option<Buffer>,
    offset: Option<usize>,
    first: bool,
    ended: bool,
}
impl Drop for Reader {
    fn drop(&mut self) {
        // At most 128KiB remains per worker, independent of directory count.
        if let Some(buffer) = self.buffer.take() {
            let _ = BUFFERS.try_with(|buffers| {
                let mut buffers = buffers.borrow_mut();
                if buffers.len() < 2 {
                    buffers.push(buffer);
                }
            });
        }
    }
}
impl Reader {
    pub fn open(path: &Path, no_follow: bool) -> io::Result<Self> {
        let wide = wide_path(path)?;
        // Synchronous directory-only metadata I/O. OPEN_REPARSE_POINT prevents
        // a descendant changed into a junction from redirecting the traversal.
        let raw = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS
                    | if no_follow {
                        FILE_FLAG_OPEN_REPARSE_POINT
                    } else {
                        0
                    },
                ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateFileW returned a new valid owned synchronous handle.
        let guard = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        let buffer = BUFFERS
            .with(|buffers| buffers.borrow_mut().pop())
            .unwrap_or_else(|| Box::new([0; BUFFER_BYTES / 8]));
        let mut reader = Self {
            guard,
            buffer: Some(buffer),
            offset: None,
            first: true,
            ended: false,
        };
        if let Err(error) = reader.refill() {
            if no_follow && matches!(error.raw_os_error(), Some(1 | 50 | 87 | 124)) {
                let mut info = BY_HANDLE_FILE_INFORMATION::default();
                if unsafe { GetFileInformationByHandle(raw, &mut info) } != 0
                    && info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Reparse directory excluded",
                    ));
                }
            }
            return Err(error);
        }
        Ok(reader)
    }
    fn refill(&mut self) -> io::Result<()> {
        let class = if self.first {
            FileFullDirectoryRestartInfo
        } else {
            FileFullDirectoryInfo
        };
        self.first = false;
        // The API writes a chain of initialized records ending with offset zero.
        // Retained tail bytes are never read; each header/name/next offset is bounded.
        if unsafe {
            GetFileInformationByHandleEx(
                self.guard.as_raw_handle().cast(),
                class,
                self.buffer.as_mut().unwrap().as_mut_ptr().cast(),
                BUFFER_BYTES as u32,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            self.ended = true;
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                return Ok(());
            }
            return Err(error);
        }
        self.offset = Some(0);
        Ok(())
    }
    pub fn next_entry(&mut self) -> io::Result<Option<Entry>> {
        loop {
            if self.ended {
                return Ok(None);
            }
            if self.offset.is_none() {
                self.refill()?;
                if self.ended {
                    return Ok(None);
                }
            }
            let offset = self.offset.unwrap();
            let header = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
            if offset % 8 != 0
                || offset.saturating_add(std::mem::size_of::<FILE_FULL_DIR_INFO>()) > BUFFER_BYTES
            {
                return self.invalid("Invalid directory record offset");
            }
            // SAFETY: u64 allocation and validated 8-byte offset satisfy the OS
            // record alignment; the successful query initializes this header.
            let pointer = unsafe {
                self.buffer
                    .as_ref()
                    .unwrap()
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset)
                    .cast::<FILE_FULL_DIR_INFO>()
            };
            let record = unsafe { ptr::read(pointer) };
            let length = record.FileNameLength as usize;
            let end = offset.saturating_add(header).saturating_add(length);
            if length % 2 != 0 || end > BUFFER_BYTES || record.EndOfFile < 0 {
                return self.invalid("Invalid directory record fields");
            }
            let next = record.NextEntryOffset as usize;
            if next != 0
                && (next % 8 != 0
                    || next < header + length
                    || offset.saturating_add(next) >= BUFFER_BYTES)
            {
                return self.invalid("Invalid directory record chain");
            }
            self.offset = if next == 0 { None } else { Some(offset + next) };
            // SAFETY: FileNameLength and buffer extent were checked above; the
            // kernel initializes precisely these UTF-16 filename units.
            let name = unsafe {
                std::slice::from_raw_parts(
                    ptr::addr_of!((*pointer).FileName).cast::<u16>(),
                    length / 2,
                )
            };
            if name == [46] || name == [46, 46] {
                continue;
            }
            if name.is_empty() || name.iter().any(|unit| matches!(*unit, 0 | 47 | 92 | 58)) {
                return self.invalid("Invalid filename component");
            }
            return Ok(Some(Entry {
                name: OsString::from_wide(name),
                attributes: record.FileAttributes,
                size: record.EndOfFile as u64,
                modified_secs: ((record.LastWriteTime as u64 as i128 - 116_444_736_000_000_000i128)
                    .div_euclid(10_000_000)) as i64,
                created_secs: Some(filetime_secs(record.CreationTime as u64)),
                accessed_secs: Some(filetime_secs(record.LastAccessTime as u64)),
                metadata_known: true,
            }));
        }
    }
    fn invalid(&mut self, message: &str) -> io::Result<Option<Entry>> {
        self.ended = true;
        Err(io::Error::new(io::ErrorKind::InvalidData, message))
    }
}

fn filetime_secs(value: u64) -> i64 {
    ((i128::from(value) - 116_444_736_000_000_000i128).div_euclid(10_000_000)) as i64
}

/// Find metadata for exactly one name, retaining its actual directory spelling.
pub(super) fn stat(path: &Path) -> io::Result<Entry> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Expected a filename"))?;
    if name.encode_wide().any(|unit| matches!(unit, 42 | 63)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Wildcard metadata paths are not supported",
        ));
    }
    let path = wide_path(path)?;
    let mut data = WIN32_FIND_DATAW::default();
    // The exact, NUL-terminated path and output record outlive this call. No
    // content handle is opened and symbolic-link metadata is not followed.
    let raw = unsafe {
        FindFirstFileExW(
            path.as_ptr(),
            FindExInfoBasic,
            (&mut data as *mut WIN32_FIND_DATAW).cast(),
            FindExSearchNameMatch,
            ptr::null(),
            0,
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _guard = FindHandle(raw);
    let length = data
        .cFileName
        .iter()
        .position(|unit| *unit == 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Unterminated filename"))?;
    let ticks = |time: windows_sys::Win32::Foundation::FILETIME| {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    };
    Ok(Entry {
        name: OsString::from_wide(&data.cFileName[..length]),
        attributes: data.dwFileAttributes,
        size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
        modified_secs: filetime_secs(ticks(data.ftLastWriteTime)),
        created_secs: Some(filetime_secs(ticks(data.ftCreationTime))),
        accessed_secs: Some(filetime_secs(ticks(data.ftLastAccessTime))),
        metadata_known: true,
    })
}
