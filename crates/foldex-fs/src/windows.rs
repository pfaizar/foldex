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
                reparse_tag: (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                    .then_some(data.dwReserved0),
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
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
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
        if no_follow && !probe_handle(raw)?.can_enumerate() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Opaque or recall-on-open directory excluded",
            ));
        }
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
        reader.refill()?;
        Ok(reader)
    }
    fn refill(&mut self) -> io::Result<()> {
        let class = if self.first {
            FileIdExtdDirectoryRestartInfo
        } else {
            FileIdExtdDirectoryInfo
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
            let header = std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
            if offset % 8 != 0
                || offset.saturating_add(std::mem::size_of::<FILE_ID_EXTD_DIR_INFO>())
                    > BUFFER_BYTES
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
                    .cast::<FILE_ID_EXTD_DIR_INFO>()
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
                reparse_tag: (record.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
                    .then_some(record.ReparsePointTag),
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
        reparse_tag: (data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0)
            .then_some(data.dwReserved0),
        size: (u64::from(data.nFileSizeHigh) << 32) | u64::from(data.nFileSizeLow),
        modified_secs: filetime_secs(ticks(data.ftLastWriteTime)),
        created_secs: Some(filetime_secs(ticks(data.ftCreationTime))),
        accessed_secs: Some(filetime_secs(ticks(data.ftLastAccessTime))),
        metadata_known: true,
    })
}

fn probe_handle(
    raw: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<capabilities::Capabilities> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            raw,
            FileAttributeTagInfo,
            (&mut info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(capabilities::Capabilities::new(
        info.FileAttributes,
        (info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0).then_some(info.ReparseTag),
    ))
}
pub(super) fn probe(path: &Path) -> io::Result<capabilities::Capabilities> {
    let path = wide_path(path)?;
    let attributes = unsafe { GetFileAttributesW(path.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(capabilities::Capabilities::new(attributes, None));
    }
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _guard = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    probe_handle(raw)
}
pub(super) fn location(path: &Path) -> capabilities::Location {
    use capabilities::Location;
    let Some(component) = path.components().next() else {
        return Location::Other;
    };
    let std::path::Component::Prefix(prefix) = component else {
        return Location::Other;
    };
    let root = match prefix.kind() {
        std::path::Prefix::Disk(disk) | std::path::Prefix::VerbatimDisk(disk) => {
            vec![disk as u16, 58, 92, 0]
        }
        std::path::Prefix::UNC(_, _) | std::path::Prefix::VerbatimUNC(_, _) => {
            return Location::Network;
        }
        _ => return Location::Other,
    };
    match unsafe { GetDriveTypeW(root.as_ptr()) } {
        3 | 6 => Location::Fixed,
        2 | 5 => Location::Removable,
        4 => Location::Network,
        _ => Location::Other,
    }
}

pub(super) fn directory_identity(path: &Path) -> io::Result<capabilities::DirectoryId> {
    let path = wide_path(path)?;
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _guard = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    if !probe_handle(raw)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a directory",
        ));
    }
    let mut info = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            raw,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of_val(&info) as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok((info.VolumeSerialNumber, info.FileId.Identifier))
}

pub(super) fn same_directory(left: &Path, right: &Path) -> io::Result<bool> {
    Ok(directory_identity(left)? == directory_identity(right)?)
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    #[test]
    fn native_batch_retains_cloud_tags_and_never_confuses_them_with_links() {
        let root = std::env::temp_dir();
        let mut reader = Reader::open(&root, false).unwrap();
        reader.buffer.as_mut().unwrap().fill(0);
        let data = reader
            .buffer
            .as_mut()
            .unwrap()
            .as_mut_ptr()
            .cast::<FILE_ID_EXTD_DIR_INFO>();
        let name = "online.txt".encode_utf16().collect::<Vec<_>>();
        unsafe {
            (*data).FileNameLength = (name.len() * 2) as u32;
            (*data).FileAttributes = FILE_ATTRIBUTE_REPARSE_POINT
                | FILE_ATTRIBUTE_OFFLINE
                | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
            (*data).ReparsePointTag = 0x9000_001a;
            (*data).EndOfFile = 1234;
            ptr::copy_nonoverlapping(
                name.as_ptr(),
                ptr::addr_of_mut!((*data).FileName).cast(),
                name.len(),
            );
        }
        reader.offset = Some(0);
        reader.ended = false;
        let entry = reader.next_entry().unwrap().unwrap();
        assert_eq!(entry.name, "online.txt");
        assert_eq!(entry.size, 1234);
        assert_eq!(
            entry.capabilities().availability(),
            capabilities::Availability::OnlineOnly
        );
        assert!(!entry.is_opaque());
        assert!(!entry.capabilities().can_read_content());
    }
    #[test]
    fn aliases_are_compared_by_native_file_identity() {
        let root = std::env::temp_dir().join(format!("foldex-case-policy-{}", std::process::id()));
        fs::create_dir_all(root.join("scope")).unwrap();
        fs::create_dir_all(root.join("other")).unwrap();
        assert!(same_directory(&root.join("scope"), &root.join("scope")).unwrap());
        assert!(!same_directory(&root.join("scope"), &root.join("other")).unwrap());
        let upper = root.join("SCOPE");
        if upper.exists() {
            assert!(same_directory(&root.join("scope"), &upper).unwrap());
        }
        fs::remove_dir_all(root).unwrap();
    }
}
