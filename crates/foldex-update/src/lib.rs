//! The updater never modifies the running application. It stages a verified
//! installer, then the UI explicitly hands control to Windows Setup at exit.
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub const REPOSITORY: &str = "https://github.com/pfaizar/foldex";
const RELEASES_API: &str = "https://api.github.com/repos/pfaizar/foldex/releases?per_page=100";
const MAX_METADATA: u64 = 4 * 1024 * 1024;
const MAX_INSTALLER: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub check_automatically: bool,
    pub download_automatically: bool,
    pub include_prereleases: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            check_automatically: true,
            download_automatically: false,
            include_prereleases: false,
        }
    }
}

pub fn load_settings(path: &Path) -> io::Result<Settings> {
    match File::open(path) {
        Ok(file) => {
            let mut bytes = Vec::new();
            file.take(16 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 16 * 1024 {
                return Err(io::Error::other("Update settings exceed their size limit"));
            }
            serde_json::from_slice(&bytes).map_err(io::Error::other)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error),
    }
}

pub fn save_settings(path: &Path, settings: &Settings) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing settings directory"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temporary, settings)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}
#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    state: String,
    digest: Option<String>,
}

/// All fields are private: installers can only originate from validated metadata.
#[derive(Clone, Debug)]
pub struct Update {
    version: Version,
    url: String,
    sha256: [u8; 32],
    size: u64,
}
impl Update {
    pub fn version(&self) -> String {
        self.version.to_string()
    }
    pub fn release_url(&self) -> String {
        format!("{REPOSITORY}/releases/tag/v{}", self.version)
    }
    pub fn size(&self) -> u64 {
        self.size
    }
}

pub fn select_update(
    bytes: &[u8],
    current: &str,
    previews: bool,
) -> Result<Option<Update>, String> {
    if bytes.len() as u64 > MAX_METADATA {
        return Err("Release information exceeds its size limit".into());
    }
    let current = Version::parse(current).map_err(|error| error.to_string())?;
    let releases: Vec<Release> =
        serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
    let latest = releases
        .into_iter()
        .filter(|release| !release.draft)
        .filter_map(|release| {
            let version = Version::parse(release.tag_name.strip_prefix('v')?).ok()?;
            // Numeric 0.x releases are public previews marked as prereleases by
            // our workflow. They remain the normal update stream for 0.x users.
            let preview = !version.pre.is_empty() || (release.prerelease && version.major != 0);
            (version.build.is_empty() && (previews || !preview) && version > current)
                .then_some((version, release))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b));
    let Some((version, release)) = latest else {
        return Ok(None);
    };
    let name = format!("Foldex-{version}-windows-x64-setup.exe");
    let mut matching = release
        .assets
        .into_iter()
        .filter(|asset| asset.name == name);
    let asset = matching
        .next()
        .ok_or("The new release does not yet include a Windows installer. Try again later.")?;
    if matching.next().is_some() {
        return Err("The release contains duplicate Windows installers".into());
    }
    let expected = format!("{REPOSITORY}/releases/download/v{version}/{name}");
    if asset.browser_download_url != expected
        || asset.state != "uploaded"
        || asset.size == 0
        || asset.size > MAX_INSTALLER
    {
        return Err("The release has invalid Windows installer information".into());
    }
    let digest = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .ok_or("GitHub has not supplied a SHA-256 digest for this installer. Try again later.")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("The release has an invalid installer checksum".into());
    }
    let mut sha256 = [0; 32];
    for (index, byte) in sha256.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digest[index * 2..index * 2 + 2], 16)
            .map_err(|error| error.to_string())?;
    }
    Ok(Some(Update {
        version,
        url: expected,
        sha256,
        size: asset.size,
    }))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .https_only(true)
        .http_status_as_error(false)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .user_agent(concat!("Foldex/", env!("CARGO_PKG_VERSION")))
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_per_read(Some(Duration::from_secs(20)))
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .build()
        .into()
}

pub fn check(current: &str, previews: bool) -> Result<Option<Update>, String> {
    let mut response = agent()
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .config()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .call()
        .map_err(|error| format!("Could not contact GitHub: {error}"))?;
    match response.status().as_u16() {
        200 => {}
        403 | 429 => return Err("GitHub is limiting update requests. Try again later.".into()),
        status => return Err(format!("GitHub returned HTTP {status}. Try again later.")),
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_METADATA + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read release information: {error}"))?;
    select_update(&bytes, current, previews)
}

fn allowed_download_url(url: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    uri.scheme_str() == Some("https")
        && uri.authority().is_some_and(|authority| {
            matches!(
                authority.as_str(),
                "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        })
}

pub struct Downloaded {
    directory: tempfile::TempDir,
    update: Update,
}
impl Downloaded {
    pub fn matches(&self, update: &Update) -> bool {
        self.update.version == update.version
            && self.update.url == update.url
            && self.update.sha256 == update.sha256
            && self.update.size == update.size
    }
    pub fn path(&self) -> PathBuf {
        self.directory.path().join("setup.exe")
    }
    pub fn update(&self) -> &Update {
        &self.update
    }
    /// Re-read immediately before handoff. Never execute a stale/tampered cache.
    pub fn verify(&self) -> Result<(), String> {
        verify_stream(
            File::open(self.path()).map_err(|error| error.to_string())?,
            io::sink(),
            &self.update,
            &AtomicBool::new(false),
            &mut |_| {},
        )
    }
    /// Setup runs after the app exits. Keep its staging directory until a later
    /// launch cleans it, rather than deleting an installer that has just started.
    pub fn keep(self) -> PathBuf {
        self.directory.keep().join("setup.exe")
    }
}

pub fn download(
    update: Update,
    cache: &Path,
    cancelled: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> Result<Downloaded, String> {
    fs::create_dir_all(cache).map_err(|error| error.to_string())?;
    let directory = tempfile::Builder::new()
        .prefix("installer-")
        .tempdir_in(cache)
        .map_err(|error| error.to_string())?;
    let agent = agent();
    let mut url = update.url.clone();
    for redirect in 0..=5 {
        if cancelled.load(Ordering::Acquire) {
            return Err("Download cancelled".into());
        }
        if !allowed_download_url(&url) {
            return Err("Untrusted installer download address".into());
        }
        let mut response = agent
            .get(&url)
            .call()
            .map_err(|error| format!("Download failed: {error}"))?;
        if response.status().is_redirection() && redirect < 5 {
            url = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or("GitHub returned an invalid download redirect")?
                .to_owned();
            continue;
        }
        if response.status().as_u16() != 200 {
            return Err(format!(
                "Installer download returned HTTP {}",
                response.status()
            ));
        }
        let mut file =
            File::create(directory.path().join("setup.exe")).map_err(|error| error.to_string())?;
        verify_stream(
            response.body_mut().as_reader(),
            &mut file,
            &update,
            cancelled,
            &mut progress,
        )?;
        file.sync_all().map_err(|error| error.to_string())?;
        return Ok(Downloaded { directory, update });
    }
    Err("Too many download redirects".into())
}

fn verify_stream(
    mut reader: impl Read,
    mut writer: impl Write,
    update: &Update,
    cancelled: &AtomicBool,
    progress: &mut impl FnMut(u64),
) -> Result<(), String> {
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err("Download cancelled".into());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > update.size || size > MAX_INSTALLER {
            return Err("Installer download is larger than expected".into());
        }
        hash.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|error| error.to_string())?;
        progress(size);
    }
    if size != update.size {
        return Err("Installer download was incomplete. Retry the download.".into());
    }
    if hash.finalize().as_slice() != update.sha256 {
        return Err("Installer checksum did not match. The download was discarded.".into());
    }
    Ok(())
}

/// Cross-language mutex identity shared with packaging/windows/foldex.iss.
/// Hash UTF-16LE without a terminator (Inno's GetSHA256OfUnicodeString).
pub fn running_mutex(directory: &Path) -> String {
    #[cfg(windows)]
    let mut units: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        directory.as_os_str().encode_wide().collect()
    };
    #[cfg(not(windows))]
    let mut units: Vec<u16> = directory.to_string_lossy().encode_utf16().collect();
    if units.starts_with(&[92, 92, 63, 92]) {
        units.drain(..4);
    }
    while units.last() == Some(&92) {
        units.pop();
    }
    for unit in &mut units {
        if (65..=90).contains(unit) {
            *unit += 32;
        }
    }
    let bytes: Vec<_> = units.into_iter().flat_map(u16::to_le_bytes).collect();
    format!("Global\\Foldex.Running.{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
