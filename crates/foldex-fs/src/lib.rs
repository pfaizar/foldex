//! Batched basic directory metadata. No per-file metadata system call on Windows.
use std::{
    ffi::OsString,
    fs, io,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};
mod parallel;
mod queue;
#[cfg(windows)]
mod windows;
pub use parallel::{Batch, ReaderKind, Walk, WalkEvent, WalkOptions};

pub const BUFFER_BYTES: usize = 64 * 1024;

pub struct Entry {
    pub name: OsString,
    pub attributes: u32,
    pub size: u64,
    pub modified_secs: i64,
    pub created_secs: Option<i64>,
    pub accessed_secs: Option<i64>,
    pub metadata_known: bool,
}
impl Entry {
    pub fn is_dir(&self) -> bool {
        self.attributes & 0x10 != 0
    }
    pub fn is_reparse(&self) -> bool {
        self.attributes & 0x400 != 0
    }
    pub fn is_hidden(&self) -> bool {
        self.attributes & 0x2 != 0
    }
    /// Read basic metadata for one notified path; never opens its contents.
    pub fn stat(path: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        {
            windows::stat(path)
        }
        #[cfg(not(windows))]
        {
            let metadata = fs::symlink_metadata(path)?;
            let name = path
                .file_name()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Expected a file or folder name",
                    )
                })?
                .to_os_string();
            let attributes = u32::from(metadata.is_dir()) * 0x10
                | u32::from(metadata.is_symlink()) * 0x400
                | u32::from(name.as_encoded_bytes().starts_with(b".")) * 0x2;
            Ok(Self {
                name,
                attributes,
                size: metadata.len(),
                modified_secs: metadata.modified().map(unix_secs).unwrap_or(0),
                created_secs: metadata.created().ok().map(unix_secs),
                accessed_secs: metadata.accessed().ok().map(unix_secs),
                metadata_known: true,
            })
        }
    }

    fn from_std(entry: fs::DirEntry) -> Self {
        let name = entry.file_name();
        let metadata = entry.metadata().ok();
        let fallback = if metadata.is_none() {
            entry.file_type().ok()
        } else {
            None
        };
        #[cfg(windows)]
        let attributes = {
            use std::os::windows::fs::MetadataExt;
            metadata.as_ref().map_or_else(
                || {
                    u32::from(fallback.is_some_and(|kind| kind.is_dir())) * 0x10
                        | u32::from(fallback.is_some_and(|kind| kind.is_symlink())) * 0x400
                },
                |metadata| metadata.file_attributes(),
            )
        };
        #[cfg(not(windows))]
        let attributes = u32::from(metadata.as_ref().is_some_and(|metadata| metadata.is_dir()))
            * 0x10
            | u32::from(
                metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.is_symlink()),
            ) * 0x400
            | u32::from(name.as_encoded_bytes().starts_with(b".")) * 0x2;
        Self {
            name,
            attributes,
            size: metadata.as_ref().map_or(0, |metadata| metadata.len()),
            modified_secs: metadata
                .as_ref()
                .and_then(|metadata| metadata.modified().ok())
                .map_or(0, unix_secs),
            created_secs: metadata
                .as_ref()
                .and_then(|m| m.created().ok())
                .map(unix_secs),
            accessed_secs: metadata
                .as_ref()
                .and_then(|m| m.accessed().ok())
                .map(unix_secs),
            metadata_known: metadata.is_some(),
        }
    }
}
fn unix_secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(time) => time.as_secs().min(i64::MAX as u64) as i64,
        Err(error) => {
            -(error
                .duration()
                .as_secs()
                .saturating_add(u64::from(error.duration().subsec_nanos() != 0))
                .min(i64::MAX as u64) as i64)
        }
    }
}

pub enum ReadDir {
    #[cfg(windows)]
    Native(windows::Reader),
    #[cfg(windows)]
    LargeFetch(windows::FindReader),
    Standard(fs::ReadDir),
}
impl ReadDir {
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_policy(path, false)
    }
    pub fn open_no_follow(path: &Path) -> io::Result<Self> {
        Self::open_with_policy(path, true)
    }
    pub fn standard(path: &Path) -> io::Result<Self> {
        fs::read_dir(path).map(Self::Standard)
    }
    pub fn large_fetch(path: &Path) -> io::Result<Self> {
        #[cfg(windows)]
        {
            windows::FindReader::open(path).map(Self::LargeFetch)
        }
        #[cfg(not(windows))]
        {
            Self::standard(path)
        }
    }
    fn open_with_policy(path: &Path, no_follow: bool) -> io::Result<Self> {
        #[cfg(windows)]
        match windows::Reader::open(path, no_follow) {
            Ok(reader) => return Ok(Self::Native(reader)),
            Err(error) if matches!(error.raw_os_error(), Some(1 | 50 | 87 | 124)) => {}
            Err(error) => return Err(error),
        }
        if no_follow
            && fs::symlink_metadata(path).is_ok_and(|metadata| {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    metadata.file_attributes() & 0x400 != 0
                }
                #[cfg(not(windows))]
                {
                    metadata.is_symlink()
                }
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Reparse directory excluded",
            ));
        }
        Self::standard(path)
    }
    pub fn buffer_bytes(&self) -> usize {
        match self {
            #[cfg(windows)]
            Self::Native(_) => BUFFER_BYTES,
            #[cfg(windows)]
            Self::LargeFetch(_) => BUFFER_BYTES,
            Self::Standard(_) => 4096,
        }
    }
}
impl Iterator for ReadDir {
    type Item = io::Result<Entry>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            #[cfg(windows)]
            Self::Native(reader) => reader.next_entry().transpose(),
            #[cfg(windows)]
            Self::LargeFetch(reader) => reader.next_entry().transpose(),
            Self::Standard(reader) => reader.next().map(|entry| entry.map(Entry::from_std)),
        }
    }
}
