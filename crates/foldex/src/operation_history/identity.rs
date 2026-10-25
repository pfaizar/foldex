use super::{Identity, Stamp};
use std::{io, path::Path};

#[cfg(windows)]
pub(super) fn read(path: &Path) -> io::Result<Stamp> {
    read_windows(path, false)
}
#[cfg(windows)]
pub(super) fn read_parent(path: &Path) -> io::Result<Stamp> {
    read_windows(path, true)
}

#[cfg(windows)]
fn read_windows(path: &Path, follow: bool) -> io::Result<Stamp> {
    use std::{
        hash::{DefaultHasher, Hasher},
        os::windows::io::{FromRawHandle, OwnedHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::*,
        System::{IO::DeviceIoControl, Ioctl::FSCTL_GET_REPARSE_POINT},
    };
    let wide = metadata_name(path)?;
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS
                | if follow {
                    0
                } else {
                    FILE_FLAG_OPEN_REPARSE_POINT
                },
            ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _handle = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
    let mut basic = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(raw, &mut basic) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file_id = FILE_ID_INFO::default();
    let identity = if unsafe {
        GetFileInformationByHandleEx(
            raw,
            FileIdInfo,
            (&mut file_id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0
    {
        Identity {
            volume: file_id.VolumeSerialNumber,
            id: file_id.FileId.Identifier,
        }
    } else {
        // Older/FAT/remote providers may expose only the documented 64-bit ID.
        let index = (basic.nFileIndexHigh as u64) << 32 | basic.nFileIndexLow as u64;
        if index == 0 {
            return Err(io::Error::other(
                "This filesystem does not expose a stable file identity for Undo",
            ));
        }
        Identity {
            volume: basic.dwVolumeSerialNumber as u64,
            id: (index as u128).to_le_bytes(),
        }
    };
    let reparse = if !follow && basic.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        if unsafe {
            GetFileInformationByHandleEx(
                raw,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let hash = if tag.ReparseTag & 0x2000_0000 != 0 {
            // Read bounded opaque metadata without traversing the link target.
            let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
            let mut count = 0u32;
            if unsafe {
                DeviceIoControl(
                    raw,
                    FSCTL_GET_REPARSE_POINT,
                    ptr::null(),
                    0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut count,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if count as usize > buffer.len() {
                return Err(io::Error::other("Invalid reparse metadata length"));
            }
            let mut hash = DefaultHasher::new();
            hash.write(&buffer[..count as usize]);
            hash.finish()
        } else {
            0
        }; // Cloud/provider state is not a namespace-link target.
        Some((tag.ReparseTag, hash))
    } else {
        None
    };
    let ticks = |time: windows_sys::Win32::Foundation::FILETIME| {
        (time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64
    };
    Ok(Stamp {
        identity,
        directory: basic.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        bytes: (basic.nFileSizeHigh as u64) << 32 | basic.nFileSizeLow as u64,
        modified: ticks(basic.ftLastWriteTime),
        created: ticks(basic.ftCreationTime),
        reparse,
    })
}

#[cfg(windows)]
fn metadata_name(path: &Path) -> io::Result<Vec<u16>> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::Storage::FileSystem::GetFullPathNameW;
    let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
    if !path.is_absolute() || name.len() > 32_766 || name.contains(&0) {
        return Err(io::Error::other(
            "Undo requires a valid absolute Windows path",
        ));
    }
    name.push(0);
    if name.len() > 260 && !name.starts_with(&[92, 92, 63, 92]) {
        // Long-path support must not depend on the executable's manifest. The
        // lexical Win32 expansion preserves normal dot/separator semantics;
        // it does not resolve the final link or open a filesystem object.
        let mut full = vec![0u16; name.len() + 8];
        let count = unsafe {
            GetFullPathNameW(
                name.as_ptr(),
                full.len() as u32,
                full.as_mut_ptr(),
                ptr::null_mut(),
            )
        } as usize;
        if count == 0 {
            return Err(io::Error::last_os_error());
        }
        if count >= full.len() {
            return Err(io::Error::other("Expanded Undo path is too long"));
        }
        full.truncate(count);
        name = if full.starts_with(&[92, 92]) {
            let mut extended = vec![92, 92, 63, 92, 85, 78, 67, 92];
            extended.extend_from_slice(&full[2..]);
            extended
        } else {
            let mut extended = vec![92, 92, 63, 92];
            extended.extend_from_slice(&full);
            extended
        };
        if name.len() > 32_766 {
            return Err(io::Error::other("Expanded Undo path is too long"));
        }
        name.push(0);
    }
    Ok(name)
}

#[cfg(unix)]
pub(super) fn read(path: &Path) -> io::Result<Stamp> {
    read_unix(path, false)
}
#[cfg(unix)]
pub(super) fn read_parent(path: &Path) -> io::Result<Stamp> {
    read_unix(path, true)
}
#[cfg(unix)]
fn read_unix(path: &Path, follow: bool) -> io::Result<Stamp> {
    use std::{
        hash::{DefaultHasher, Hash, Hasher},
        os::unix::fs::MetadataExt,
    };
    let metadata = if follow {
        std::fs::metadata(path)?
    } else {
        std::fs::symlink_metadata(path)?
    };
    let reparse = if metadata.file_type().is_symlink() {
        let mut hash = DefaultHasher::new();
        std::fs::read_link(path)?.hash(&mut hash);
        Some((0xa000_000c, hash.finish()))
    } else {
        None
    };
    Ok(Stamp {
        identity: Identity {
            volume: metadata.dev(),
            id: (metadata.ino() as u128).to_le_bytes(),
        },
        directory: metadata.is_dir(),
        bytes: metadata.len(),
        modified: metadata.mtime() as u64 ^ (metadata.mtime_nsec() as u64).rotate_left(32),
        created: metadata.ctime() as u64 ^ (metadata.ctime_nsec() as u64).rotate_left(32),
        reparse,
    })
}

#[cfg(not(any(windows, unix)))]
pub(super) fn read(_: &Path) -> io::Result<Stamp> {
    Err(io::Error::other(
        "File identity is unavailable on this platform",
    ))
}
#[cfg(not(any(windows, unix)))]
pub(super) fn read_parent(path: &Path) -> io::Result<Stamp> {
    read(path)
}
