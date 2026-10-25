//! Metadata-only classification shared by traversal, previews, assets and undo.
use std::{fs, io, path::Path};
pub const NAME_SURROGATE: u32 = 0x2000_0000;
pub const DIRECTORY: u32 = 0x10;
pub const REPARSE: u32 = 0x400;
pub const OFFLINE: u32 = 0x1000;
pub const RECALL_ON_OPEN: u32 = 0x40000;
pub const RECALL_ON_DATA_ACCESS: u32 = 0x400000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Availability {
    #[default]
    Local = 0,
    HydratedCloud = 1,
    OnlineOnly = 2,
    Redirector = 3,
    UnknownReparse = 4,
}
impl Availability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "Available locally",
            Self::HydratedCloud => "Cloud file, available locally",
            Self::OnlineOnly => "Online-only or recall required",
            Self::Redirector => "Link or junction",
            Self::UnknownReparse => "Unrecognized reparse item",
        }
    }
    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::Local,
            1 => Self::HydratedCloud,
            2 => Self::OnlineOnly,
            3 => Self::Redirector,
            4 => Self::UnknownReparse,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub attributes: u32,
    pub reparse_tag: Option<u32>,
}
impl Capabilities {
    pub fn new(attributes: u32, reparse_tag: Option<u32>) -> Self {
        Self {
            attributes,
            reparse_tag,
        }
    }
    pub fn is_dir(self) -> bool {
        self.attributes & DIRECTORY != 0
    }
    pub fn redirects(self) -> bool {
        self.attributes & REPARSE != 0
            && self
                .reparse_tag
                .is_some_and(|tag| tag & NAME_SURROGATE != 0)
    }
    pub fn cloud(self) -> bool {
        self.attributes & REPARSE != 0
            && self
                .reparse_tag
                .is_some_and(|tag| tag & 0xffff_0fff == 0x9000_001a)
    }
    pub fn unknown_reparse(self) -> bool {
        self.attributes & REPARSE != 0 && !self.redirects() && !self.cloud()
    }
    pub fn recall(self) -> bool {
        self.attributes & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
    }
    pub fn availability(self) -> Availability {
        if self.redirects() {
            Availability::Redirector
        } else if self.unknown_reparse() {
            Availability::UnknownReparse
        } else if self.recall() {
            Availability::OnlineOnly
        } else if self.cloud() {
            Availability::HydratedCloud
        } else {
            Availability::Local
        }
    }
    /// Enumeration is metadata only, but recall-on-open and opaque items stay closed.
    pub fn can_enumerate(self) -> bool {
        self.is_dir()
            && !self.redirects()
            && !self.unknown_reparse()
            && self.attributes & RECALL_ON_OPEN == 0
            && (self.attributes & OFFLINE == 0 || self.cloud())
    }
    pub fn can_read_content(self) -> bool {
        !self.is_dir() && !self.redirects() && !self.unknown_reparse() && !self.recall()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    Fixed,
    Removable,
    Network,
    Other,
}
impl Location {
    pub fn background_content(self) -> bool {
        matches!(self, Self::Fixed)
    }
}

pub fn probe(path: &Path) -> io::Result<Capabilities> {
    #[cfg(windows)]
    {
        super::windows::probe(path)
    }
    #[cfg(not(windows))]
    {
        let metadata = fs::symlink_metadata(path)?;
        Ok(Capabilities::new(
            u32::from(metadata.is_dir()) * DIRECTORY | u32::from(metadata.is_symlink()) * REPARSE,
            metadata.is_symlink().then_some(0xa000_000c),
        ))
    }
}
pub fn location(path: &Path) -> Location {
    #[cfg(windows)]
    {
        super::windows::location(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Location::Fixed
    }
}
/// Bounded ancestor inspection; no contents and no redirected or recall-on-open ancestor.
pub fn background_path(path: &Path) -> io::Result<Capabilities> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected an absolute path",
        ));
    }
    if !location(path).background_content() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Background content is limited to fixed local drives",
        ));
    }
    let mut result = None;
    for (number, ancestor) in path
        .ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .enumerate()
    {
        if number >= 256 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Ancestor limit reached",
            ));
        }
        let caps = probe(ancestor)?;
        if number == 0 {
            result = Some(caps);
        }
        if caps.redirects()
            || caps.unknown_reparse()
            || caps.attributes & RECALL_ON_OPEN != 0
            || (number > 0 && !caps.can_enumerate())
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Link, unknown reparse or recall-on-open path",
            ));
        }
    }
    result.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Empty path"))
}
/// Only background preview callers use this guard. Explicit Open remains a Shell action.
pub fn open_background_content(path: &Path) -> io::Result<fs::File> {
    #[cfg(not(windows))]
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Expected an ordinary file",
        ));
    }
    if !background_path(path)?.can_read_content() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Open this item explicitly to download or follow it",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // OPEN_REPARSE_POINT + OPEN_NO_RECALL preserve the conservative metadata policy.
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x0020_0000 | 0x0010_0000)
            .open(path)
    }
    #[cfg(not(windows))]
    {
        fs::File::open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloud_redirector_unknown_and_recall_policies_are_distinct() {
        for tag in [0x9000_001a, 0x9000_101a, 0x9000_f01a] {
            let hydrated = Capabilities::new(REPARSE, Some(tag));
            assert!(hydrated.cloud() && hydrated.can_read_content());
            assert_eq!(hydrated.availability(), Availability::HydratedCloud);
            let online = Capabilities::new(REPARSE | OFFLINE | RECALL_ON_DATA_ACCESS, Some(tag));
            assert!(!online.can_read_content());
            assert_eq!(online.availability(), Availability::OnlineOnly);
            assert!(Capabilities::new(online.attributes | DIRECTORY, Some(tag)).can_enumerate());
            assert!(
                !Capabilities::new(DIRECTORY | REPARSE | RECALL_ON_OPEN, Some(tag)).can_enumerate()
            );
        }
        for tag in [0xa000_000c, 0xa000_0003] {
            let c = Capabilities::new(DIRECTORY | REPARSE, Some(tag));
            assert!(c.redirects() && !c.can_enumerate());
        }
        assert!(!Capabilities::new(REPARSE, None).can_read_content());
        assert!(!Capabilities::new(DIRECTORY | REPARSE, Some(0x8000_0023)).can_enumerate());
        assert!(!Capabilities::new(OFFLINE, None).can_read_content());
    }
    #[test]
    fn background_location_policy_is_explicit() {
        assert!(Location::Fixed.background_content());
        for value in [Location::Removable, Location::Network, Location::Other] {
            assert!(!value.background_content());
        }
    }
}

/// Worker-only alias verification; lexical case folding is insufficient in a case-sensitive directory.
pub fn same_directory(left: &Path, right: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        super::windows::same_directory(left, right)
    }
    #[cfg(not(windows))]
    {
        Ok(fs::canonicalize(left)? == fs::canonicalize(right)?)
    }
}
