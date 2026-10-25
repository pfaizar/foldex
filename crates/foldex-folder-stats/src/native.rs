use super::Issues;
use std::{ffi::OsString, io, path::Path};

#[derive(Debug)]
pub(crate) enum Excluded {
    Reparse,
    Offline,
}
impl std::fmt::Display for Excluded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Reparse => "Reparse folder excluded",
            Self::Offline => "Offline folder excluded",
        })
    }
}
impl std::error::Error for Excluded {}
pub(crate) fn exclusion(error: &io::Error) -> Option<&Excluded> {
    error.get_ref()?.downcast_ref()
}

pub(crate) struct PolicyError {
    pub message: String,
    pub issues: Issues,
}
pub(crate) struct Entry {
    pub name: OsString,
    pub directory: bool,
    pub reparse: bool,
    pub offline: bool,
    pub bytes: u64,
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::{
        os::windows::{
            ffi::{OsStrExt, OsStringExt},
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE},
        Storage::FileSystem::*,
        System::{
            Threading::{
                GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
                THREAD_PRIORITY_LOWEST,
            },
            WindowsProgramming::{DRIVE_FIXED, DRIVE_RAMDISK},
        },
    };
    const OFFLINE: u32 = FILE_ATTRIBUTE_OFFLINE
        | FILE_ATTRIBUTE_RECALL_ON_OPEN
        | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS;
    pub(crate) struct Reader {
        guard: OwnedHandle,
        buffer: Box<[u64; 512]>,
        offset: Option<usize>,
        first: bool,
        ended: bool,
        #[cfg(test)]
        fixture_only: bool,
    }
    pub(crate) fn path_weight(path: &std::path::PathBuf) -> usize {
        path.capacity()
            .max(path.as_os_str().len())
            .saturating_mul(2)
    }
    pub(crate) fn invalid_path(path: &Path) -> bool {
        let mut length = 0;
        for unit in path.as_os_str().encode_wide() {
            length += 1;
            if unit == 0 || length > 32_766 {
                return true;
            }
        }
        false
    }
    pub(crate) fn background_priority() {
        unsafe {
            if SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) == 0 {
                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST);
            }
        }
    }
    pub(crate) fn check_root(path: &Path) -> Result<(), PolicyError> {
        let mut wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .map(|unit| if unit == 47 { 92 } else { unit })
            .collect();
        let start = if wide.starts_with(&[92, 92, 63, 92]) {
            4
        } else {
            0
        };
        let local = wide.len() >= start + 3
            && (wide[start] as u8).is_ascii_alphabetic()
            && wide[start] <= 127
            && wide[start + 1] == 58
            && wide[start + 2] == 92;
        if !local {
            return Err(PolicyError {
                message: "Only local fixed-drive paths are scanned by default".into(),
                issues: Issues {
                    skipped_nonlocal: 1,
                    ..Default::default()
                },
            });
        }
        let drive = [wide[start], 58, 92, 0];
        if !matches!(
            unsafe { GetDriveTypeW(drive.as_ptr()) },
            DRIVE_FIXED | DRIVE_RAMDISK
        ) {
            return Err(PolicyError {
                message: "Network, removable and unknown drives are excluded by default".into(),
                issues: Issues {
                    skipped_nonlocal: 1,
                    ..Default::default()
                },
            });
        }
        let mut ends = vec![start + 3];
        for index in start + 3..wide.len() {
            if wide[index] == 92 && index > start + 3 {
                if ends.len() >= 256 {
                    return Err(PolicyError {
                        message: "Root path exceeds the bounded ancestor check".into(),
                        issues: Issues {
                            path_limited: 1,
                            ..Default::default()
                        },
                    });
                }
                ends.push(index);
            }
        }
        if ends.last() != Some(&wide.len()) {
            ends.push(wide.len());
        }
        if ends.len() > 256 {
            return Err(PolicyError {
                message: "Root path exceeds the bounded ancestor check".into(),
                issues: Issues {
                    path_limited: 1,
                    ..Default::default()
                },
            });
        }
        wide.push(0);
        for end in ends {
            let saved = wide[end];
            wide[end] = 0;
            let attributes = unsafe { GetFileAttributesW(wide.as_ptr()) };
            wide[end] = saved;
            if attributes == INVALID_FILE_ATTRIBUTES {
                return Err(PolicyError {
                    message: io::Error::last_os_error().to_string(),
                    issues: Issues {
                        unreadable: 1,
                        ..Default::default()
                    },
                });
            }
            if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(PolicyError {
                    message: "A reparse-point root or ancestor is excluded".into(),
                    issues: Issues {
                        skipped_reparse: 1,
                        ..Default::default()
                    },
                });
            }
            if attributes & OFFLINE != 0 {
                return Err(PolicyError {
                    message: "An offline or recall-on-access ancestor is excluded".into(),
                    issues: Issues {
                        skipped_offline: 1,
                        ..Default::default()
                    },
                });
            }
            if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(PolicyError {
                    message: "The requested path is not a folder".into(),
                    issues: Issues {
                        unreadable: 1,
                        ..Default::default()
                    },
                });
            }
        }
        Ok(())
    }
    impl Reader {
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
            wide.push(0);
            let raw = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
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
            let guard = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            if unsafe { GetFileInformationByHandle(raw, &mut info) } == 0 {
                return Err(io::Error::last_os_error());
            }
            if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    Excluded::Reparse,
                ));
            }
            if info.dwFileAttributes & OFFLINE != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    Excluded::Offline,
                ));
            }
            if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Expected a directory",
                ));
            }
            Ok(Self {
                guard,
                buffer: Box::new([0; 512]),
                offset: None,
                first: true,
                ended: false,
                #[cfg(test)]
                fixture_only: false,
            })
        }
        pub(crate) fn next(&mut self) -> io::Result<Option<Entry>> {
            #[cfg(test)]
            if self.fixture_only && self.offset.is_none() {
                return Ok(None);
            }
            if self.ended {
                return Ok(None);
            }
            loop {
                if self.offset.is_none() {
                    self.buffer.fill(0);
                    let class = if self.first {
                        FileFullDirectoryRestartInfo
                    } else {
                        FileFullDirectoryInfo
                    };
                    self.first = false;
                    if unsafe {
                        GetFileInformationByHandleEx(
                            self.guard.as_raw_handle().cast(),
                            class,
                            self.buffer.as_mut_ptr().cast(),
                            (self.buffer.len() * 8) as u32,
                        )
                    } == 0
                    {
                        let error = io::Error::last_os_error();
                        self.ended = true;
                        return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                            Ok(None)
                        } else {
                            Err(error)
                        };
                    }
                    self.offset = Some(0);
                }
                let offset = self.offset.unwrap();
                let capacity = self.buffer.len() * 8;
                let header = std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName);
                if offset % 8 != 0
                    || offset.saturating_add(std::mem::size_of::<FILE_FULL_DIR_INFO>()) > capacity
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid directory entry offset",
                    ));
                }
                let pointer = unsafe {
                    self.buffer
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<FILE_FULL_DIR_INFO>()
                };
                let record = unsafe { ptr::read(pointer) };
                let length = record.FileNameLength as usize;
                if length % 2 != 0
                    || length > 510
                    || offset.saturating_add(header).saturating_add(length) > capacity
                    || record.EndOfFile < 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid directory entry fields",
                    ));
                }
                let next = record.NextEntryOffset as usize;
                if next != 0
                    && (next % 8 != 0
                        || next < header + length
                        || offset.saturating_add(next) >= capacity)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid directory entry chain",
                    ));
                }
                self.offset = if next == 0 { None } else { Some(offset + next) };
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
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Invalid filename component",
                    ));
                }
                let attributes = record.FileAttributes;
                let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                return Ok(Some(Entry {
                    name: if directory {
                        OsString::from_wide(name)
                    } else {
                        OsString::new()
                    },
                    directory,
                    reparse: attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
                    offline: attributes & OFFLINE != 0,
                    bytes: record.EndOfFile as u64,
                }));
            }
        }
        pub(crate) fn handles(&self) -> usize {
            1
        }
        #[cfg(test)]
        pub(crate) fn inject_reparse_fixture(&mut self, name: &str) {
            let wide: Vec<u16> = name.encode_utf16().collect();
            self.buffer.fill(0);
            let record = self.buffer.as_mut_ptr().cast::<FILE_FULL_DIR_INFO>();
            unsafe {
                (*record).FileAttributes = FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT;
                (*record).FileNameLength = (wide.len() * 2) as u32;
                ptr::copy_nonoverlapping(
                    wide.as_ptr(),
                    ptr::addr_of_mut!((*record).FileName).cast(),
                    wide.len(),
                );
            }
            self.offset = Some(0);
            self.fixture_only = true;
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub(crate) struct Reader;
    pub(crate) fn path_weight(path: &std::path::PathBuf) -> usize {
        path.capacity()
            .max(path.as_os_str().len())
            .saturating_mul(2)
    }
    pub(crate) fn invalid_path(path: &Path) -> bool {
        path.as_os_str().len() > 32_766 || path.as_os_str().to_string_lossy().contains('\0')
    }
    pub(crate) fn background_priority() {}
    pub(crate) fn check_root(_: &Path) -> Result<(), PolicyError> {
        Err(PolicyError {
            message: "Locality-checked folder statistics currently require Windows".into(),
            issues: Issues {
                skipped_nonlocal: 1,
                ..Default::default()
            },
        })
    }
    impl Reader {
        pub(crate) fn open(_: &Path) -> io::Result<Self> {
            Err(io::ErrorKind::Unsupported.into())
        }
        pub(crate) fn next(&mut self) -> io::Result<Option<Entry>> {
            Ok(None)
        }
        pub(crate) fn handles(&self) -> usize {
            0
        }
    }
}

pub(crate) use imp::{Reader, background_priority, check_root, invalid_path, path_weight};
