//! Typed shell identities share the ordinary bounded asset mailbox/cache. These
//! keys are never filesystem paths and never enable arbitrary shell parsing.
use super::AssetKey;
use std::path::{Path, PathBuf};

const PREFIX: &str = "foldex-icon:";
pub const MAX_SHELL_ICON_ID_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnownFolderIcon {
    Home,
    Profile,
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StockIcon {
    File,
    Folder,
    Drive,
    Application,
    Network,
    NetworkDrive,
    OfflineNetworkDrive,
    RecycleBin,
    Computer,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum NativeIcon {
    Known(KnownFolderIcon),
    Stock(StockIcon),
    ShellItem(Vec<u8>),
    AppUserModelId(String),
    Window(isize),
    FileType(String),
}

impl AssetKey {
    pub fn known_folder_icon(folder: KnownFolderIcon, size: u16) -> Self {
        let name = match folder {
            KnownFolderIcon::Home => "home",
            KnownFolderIcon::Profile => "profile",
            KnownFolderIcon::Desktop => "desktop",
            KnownFolderIcon::Downloads => "downloads",
            KnownFolderIcon::Documents => "documents",
            KnownFolderIcon::Pictures => "pictures",
            KnownFolderIcon::Music => "music",
            KnownFolderIcon::Videos => "videos",
        };
        Self::icon_identity(format!("{PREFIX}known/{name}"), size)
    }

    pub fn stock_icon(icon: StockIcon, size: u16) -> Self {
        let name = match icon {
            StockIcon::File => "file",
            StockIcon::Folder => "folder",
            StockIcon::Drive => "drive",
            StockIcon::Application => "application",
            StockIcon::Network => "network",
            StockIcon::NetworkDrive => "network-drive",
            StockIcon::OfflineNetworkDrive => "network-drive-offline",
            StockIcon::RecycleBin => "recycle-bin",
            StockIcon::Computer => "computer",
        };
        Self::icon_identity(format!("{PREFIX}stock/{name}"), size)
    }

    /// Association icon for a dotless extension. This typed request never
    /// identifies a file and cannot enable thumbnails or path-based extraction.
    pub fn file_type_icon(extension: &str, size: u16) -> Option<Self> {
        valid_extension(extension).then(|| {
            Self::icon_identity(format!("{PREFIX}type/{}", extension.to_lowercase()), size)
        })
    }

    /// Apps-folder PIDLs preserve packaged/UWP identity without inventing an
    /// executable path. Oversized or malformed targets retain the caller icon.
    pub fn application_icon(identifier: &[u8], size: u16) -> Option<Self> {
        if !valid_pidl(identifier) {
            return None;
        }
        let mut text = String::with_capacity(PREFIX.len() + 5 + identifier.len() * 2);
        text.push_str(PREFIX);
        text.push_str("pidl/");
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in identifier {
            text.push(HEX[(byte >> 4) as usize] as char);
            text.push(HEX[(byte & 15) as usize] as char);
        }
        Some(Self::icon_identity(text, size))
    }

    pub fn app_user_model_icon(identifier: &str, size: u16) -> Option<Self> {
        if !valid_app_id(identifier) {
            return None;
        }
        Some(Self::icon_identity(
            format!("{PREFIX}aumid/{identifier}"),
            size,
        ))
    }

    pub fn window_icon(window: isize, size: u16) -> Option<Self> {
        (window > 0).then(|| Self::icon_identity(format!("{PREFIX}window/{window:x}"), size))
    }

    fn icon_identity(path: String, size: u16) -> Self {
        Self {
            path: PathBuf::from(path),
            size,
            thumbnail: false,
        }
    }
}

fn valid_app_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 512
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '!'))
}

fn valid_extension(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '+'))
}

fn valid_pidl(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || bytes.len() > MAX_SHELL_ICON_ID_BYTES {
        return false;
    }
    let mut at = 0;
    while at + 2 <= bytes.len() {
        let size = u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
        if size == 0 {
            return at + 2 == bytes.len();
        }
        if size < 2 || size > bytes.len() - at {
            return false;
        }
        at += size;
    }
    false
}

pub(crate) fn decode(path: &Path) -> Option<NativeIcon> {
    let text = path.to_str()?.strip_prefix(PREFIX)?;
    let (kind, value) = text.split_once('/')?;
    Some(match kind {
        "known" => NativeIcon::Known(match value {
            "home" => KnownFolderIcon::Home,
            "profile" => KnownFolderIcon::Profile,
            "desktop" => KnownFolderIcon::Desktop,
            "downloads" => KnownFolderIcon::Downloads,
            "documents" => KnownFolderIcon::Documents,
            "pictures" => KnownFolderIcon::Pictures,
            "music" => KnownFolderIcon::Music,
            "videos" => KnownFolderIcon::Videos,
            _ => return None,
        }),
        "stock" => NativeIcon::Stock(match value {
            "file" => StockIcon::File,
            "folder" => StockIcon::Folder,
            "drive" => StockIcon::Drive,
            "application" => StockIcon::Application,
            "network" => StockIcon::Network,
            "network-drive" => StockIcon::NetworkDrive,
            "network-drive-offline" => StockIcon::OfflineNetworkDrive,
            "recycle-bin" => StockIcon::RecycleBin,
            "computer" => StockIcon::Computer,
            _ => return None,
        }),
        "aumid" if valid_app_id(value) => NativeIcon::AppUserModelId(value.to_owned()),
        "type" if valid_extension(value) => NativeIcon::FileType(value.to_owned()),
        "window" => {
            let window = isize::from_str_radix(value, 16).ok()?;
            if window <= 0 {
                return None;
            }
            NativeIcon::Window(window)
        }
        "pidl" if value.len() <= MAX_SHELL_ICON_ID_BYTES * 2 && value.len() % 2 == 0 => {
            let bytes = value
                .as_bytes()
                .chunks_exact(2)
                .map(|pair| {
                    let nibble = |c: u8| match c {
                        b'0'..=b'9' => Some(c - b'0'),
                        b'a'..=b'f' => Some(c - b'a' + 10),
                        _ => None,
                    };
                    Some(nibble(pair[0])? << 4 | nibble(pair[1])?)
                })
                .collect::<Option<Vec<_>>>()?;
            if !valid_pidl(&bytes) {
                return None;
            }
            NativeIcon::ShellItem(bytes)
        }
        _ => return None,
    })
}

/// For consumers which validate keys before submitting to AssetLoader. Ordinary
/// keys still require absolute, bounded, NUL-free filesystem paths.
pub fn is_native_icon_path(path: &Path) -> bool {
    decode(path).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn file_type_icons_are_bounded_association_keys_without_file_or_pidl_identity() {
        let key = AssetKey::file_type_icon("XLSX", 152).unwrap();
        assert_eq!(key.path, PathBuf::from("foldex-icon:type/xlsx"));
        assert_eq!(decode(&key.path), Some(NativeIcon::FileType("xlsx".into())));
        assert!(is_native_icon_path(&key.path));
        assert!(!key.thumbnail);
        for unsafe_name in [
            "",
            ".xlsx",
            "C:/original/report.xlsx",
            "txt/child",
            "txt\\child",
            "txt:stream",
            "..",
            "txt\0",
        ] {
            assert!(AssetKey::file_type_icon(unsafe_name, 152).is_none());
        }
        assert!(AssetKey::file_type_icon(&"x".repeat(65), 152).is_none());
        assert!(decode(Path::new("foldex-icon:type/C:/original/report.xlsx")).is_none());
    }
    #[test]
    fn network_icons_are_typed_stock_assets_without_remote_path_probes() {
        for icon in [
            StockIcon::Network,
            StockIcon::NetworkDrive,
            StockIcon::OfflineNetworkDrive,
        ] {
            let key = AssetKey::stock_icon(icon, 24);
            assert_eq!(decode(&key.path), Some(NativeIcon::Stock(icon)));
            assert!(!key.thumbnail);
            assert!(is_native_icon_path(&key.path));
        }
    }
    #[test]
    fn typed_native_icons_roundtrip_without_accepting_arbitrary_shell_names() {
        let key = AssetKey::known_folder_icon(KnownFolderIcon::Home, 24);
        assert_eq!(
            decode(&key.path),
            Some(NativeIcon::Known(KnownFolderIcon::Home))
        );
        let bytes = [4, 0, 1, 2, 0, 0];
        let key = AssetKey::application_icon(&bytes, 24).unwrap();
        assert_eq!(
            decode(&key.path),
            Some(NativeIcon::ShellItem(bytes.to_vec()))
        );
        assert!(
            AssetKey::app_user_model_icon("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App", 24)
                .is_some()
        );
        for bad in [
            "foldex-icon:known/unknown",
            "foldex-icon:aumid/../calc.exe",
            "shell:AppsFolder",
            "foldex-icon:window/0",
            "foldex-icon:pidl/0400010200000000",
        ] {
            assert!(!is_native_icon_path(Path::new(bad)), "{bad}");
        }
    }
    #[test]
    fn shell_icon_identity_is_bounded_and_malformed_pidls_never_reach_com() {
        assert!(AssetKey::application_icon(&[0, 0], 24).is_none());
        assert!(AssetKey::application_icon(&[9, 0, 0, 0], 24).is_none());
        assert!(AssetKey::application_icon(&vec![0; MAX_SHELL_ICON_ID_BYTES + 1], 24).is_none());
        assert!(AssetKey::app_user_model_icon(&"x".repeat(513), 24).is_none());
        assert!(AssetKey::app_user_model_icon("calc\0hidden", 24).is_none());
    }
}
