use super::{
    NativeIndex,
    manifest::{Manifest, plain},
};
use crate::{codec, model::*};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(super) fn publication_lock(root: &Path) -> Result<Option<fs::File>> {
    let path = root.join("native-publication.lock");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if !plain(&metadata, false) => {
            return Err(Error::InvalidIndex("invalid publication lock file".into()));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}
pub(super) fn publish_pointer(root: &Path, name: &str, database: &Path) -> Result<()> {
    for _ in 0..50 {
        if let Some(_guard) = publication_lock(root)? {
            return crate::store::write_pointer(root, name, database);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "catalog publication is busy; retry the search",
    )
    .into())
}
fn protected_pointer(root: &Path, candidate: &Path) -> Result<bool> {
    for name in ["current-native.index", "building-native.index"] {
        match pointer(root, name) {
            Ok(path) if path == candidate => return Ok(true),
            Ok(_) => {}
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}
fn remove_sidecars(root: &Path, manifest: &Path, report: &mut CleanupReport) -> Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = root.join(format!(
            "{}{suffix}",
            manifest.file_name().unwrap().to_string_lossy()
        ));
        if let Ok(metadata) = fs::symlink_metadata(&sidecar) {
            if plain(&metadata, false)
                && sidecar.canonicalize()?.parent() == Some(root)
                && fs::remove_file(sidecar).is_ok()
            {
                report.removed_bytes += metadata.len();
            }
        }
    }
    Ok(())
}

const PENDING: &str = "pending-cleanup";
const CLEANUP_MAGIC: &[u8] = b"FOLDEX-NATIVE-CLEANUP-1\0";
fn marker_bytes(scope: &Path, manifest: &Path) -> Result<Vec<u8>> {
    let scope = codec::encode(scope.as_os_str());
    if scope.len() > 128 * 1024 {
        return Err(Error::InvalidPath(
            "cleanup scope exceeds its path budget".into(),
        ));
    }
    let name = manifest
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_name(name))
        .ok_or_else(|| Error::InvalidIndex("invalid pending-cleanup generation".into()))?;
    let mut bytes = CLEANUP_MAGIC.to_vec();
    bytes.extend_from_slice(&(scope.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&scope);
    bytes.extend_from_slice(name.as_bytes());
    Ok(bytes)
}
fn valid_marker(directory: &Path, scope: &Path, manifest: &Path) -> Result<bool> {
    let path = directory.join(PENDING);
    let metadata = fs::symlink_metadata(&path)?;
    let expected = marker_bytes(scope, manifest)?;
    if !plain(&metadata, false) || metadata.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut bytes = Vec::with_capacity(expected.len());
    fs::File::open(path)?
        .take(expected.len() as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes == expected)
}
fn ensure_marker(directory: &Path, scope: &Path, manifest: &Path) -> Result<()> {
    let path = directory.join(PENDING);
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(&marker_bytes(scope, manifest)?)?;
            file.sync_all()?;
            Ok(())
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && valid_marker(directory, scope, manifest)? =>
        {
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
/// The marker survives failed hardlink deletion while another generation has a
/// mapped read guard. Never remove an unrecognized file or follow a reparse point.
fn finish_data_cleanup(
    directory: &Path,
    root: &Path,
    scope: &Path,
    manifest: &Path,
    report: &mut CleanupReport,
) -> Result<()> {
    let (files, queues) = data_files(directory, root)?;
    if !valid_marker(directory, scope, manifest)? {
        return Err(Error::InvalidIndex("unverified cleanup marker".into()));
    }
    let mut complete = true;
    for (file, bytes) in files {
        match fs::remove_file(file) {
            Ok(()) => report.removed_bytes += bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => complete = false,
        }
    }
    if queues && fs::remove_dir(directory.join("queues")).is_err() {
        complete = false;
    }
    if complete {
        fs::remove_file(directory.join(PENDING))?;
        if fs::remove_dir(directory).is_err() {
            // Preserve retry ownership if a file appeared after verification.
            ensure_marker(directory, scope, manifest)?;
        }
    }
    Ok(())
}
fn retry_orphan(
    directory: &Path,
    root: &Path,
    scope: &Path,
    report: &mut CleanupReport,
) -> Result<()> {
    if !plain(&fs::symlink_metadata(directory)?, true)
        || directory.canonicalize()?.parent() != Some(root)
    {
        return Err(Error::InvalidPath("unverified orphan directory".into()));
    }
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".data"))
        .ok_or_else(|| Error::InvalidIndex("invalid orphan data name".into()))?;
    let manifest = root.join(format!("{name}.sqlite3"));
    if !valid_name(manifest.file_name().unwrap().to_str().unwrap()) {
        return Err(Error::InvalidIndex("invalid orphan generation".into()));
    }
    match fs::symlink_metadata(&manifest) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            finish_data_cleanup(directory, root, scope, &manifest, report)?;
            remove_sidecars(root, &manifest, report)
        }
        Ok(_) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn valid_name(name: &str) -> bool {
    name.len() <= 128
        && name.starts_with("catalog-")
        && name.ends_with(".sqlite3")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}
pub(super) fn pointer(root: &Path, name: &str) -> Result<PathBuf> {
    let path = root.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    if !plain(&metadata, false) || metadata.len() > 256 {
        return Err(Error::InvalidIndex("invalid native pointer file".into()));
    }
    let mut text = String::new();
    fs::File::open(path)?.take(257).read_to_string(&mut text)?;
    if !valid_name(text.trim()) {
        return Err(Error::InvalidIndex(
            "invalid native generation pointer".into(),
        ));
    }
    Ok(root.join(text.trim()))
}

fn data_files(directory: &Path, cache: &Path) -> Result<(Vec<(PathBuf, u64)>, bool)> {
    if !plain(&fs::symlink_metadata(directory)?, true)
        || directory.canonicalize()?.parent() != Some(cache)
    {
        return Err(Error::InvalidIndex(
            "native data directory escaped its cache".into(),
        ));
    }
    let mut files = Vec::new();
    let mut queues = false;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| Error::InvalidIndex("unrecognized catalog file".into()))?;
        let metadata = fs::symlink_metadata(&path)?;
        if name == PENDING && plain(&metadata, false) && metadata.len() <= 129 * 1024 {
            continue;
        }
        if name == "queues"
            && plain(&metadata, true)
            && path.canonicalize()?.parent() == Some(directory)
        {
            queues = true;
            for child in fs::read_dir(path)? {
                let child = child?;
                let child_name = child.file_name();
                let child_name = child_name
                    .to_str()
                    .ok_or_else(|| Error::InvalidIndex("invalid queue filename".into()))?;
                let metadata = fs::symlink_metadata(child.path())?;
                if !plain(&metadata, false) || !owned_numbered(child_name, "owned-", ".queue") {
                    return Err(Error::InvalidIndex(
                        "unrecognized catalog queue file".into(),
                    ));
                }
                files.push((child.path(), metadata.len()));
            }
        } else {
            if !plain(&metadata, false)
                || path.canonicalize()?.parent() != Some(directory)
                || !(owned_numbered(name, "shard-", ".bin")
                    || owned_numbered(name, "result-", ".sqlite3")
                    || owned_numbered(name, "result-", ".rows"))
            {
                return Err(Error::InvalidIndex("unrecognized catalog data file".into()));
            }
            files.push((path, metadata.len()));
        }
        if files.len() > 1_000_000 {
            return Err(Error::InvalidIndex(
                "catalog cleanup file bound exceeded".into(),
            ));
        }
    }
    Ok((files, queues))
}
fn owned_numbered(name: &str, prefix: &str, suffix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|name| name.strip_suffix(suffix))
        .is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 80
                && id.bytes().all(|byte| byte.is_ascii_digit() || byte == b'-')
        })
}

impl NativeIndex {
    pub fn cleanup_owned(cache_dir: &Path, scope: &Path) -> Result<CleanupReport> {
        Self::cleanup_owned_retaining(cache_dir, scope, &[])
    }
    /// Windows SQLite denies deleting an open manifest. Remove that ownership
    /// guard first; only then remove validated files in its sibling directory.
    pub fn cleanup_owned_retaining(
        cache_dir: &Path,
        scope: &Path,
        active: &[PathBuf],
    ) -> Result<CleanupReport> {
        if active.len() > 64 {
            return Err(Error::InvalidIndex(
                "too many active native generations".into(),
            ));
        }
        let root = cache_dir.canonicalize()?;
        let scope = absolute(scope)?;
        let mut retained = Vec::with_capacity(4);
        for path in active {
            let path = path.canonicalize()?;
            if path.parent() != Some(root.as_path())
                || !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(valid_name)
            {
                return Err(Error::InvalidIndex(
                    "invalid active native generation".into(),
                ));
            }
            retained.push(path);
        }
        for name in ["current-native.index", "building-native.index"] {
            match pointer(&root, name) {
                Ok(path) => retained.push(path),
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let mut report = CleanupReport::default();
        // Unix SQLite's open handle is not a deletion guard; retain caches there
        // until a cross-process lease is implemented. Foldex currently targets Windows.
        if !cfg!(windows) {
            return Ok(report);
        }
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_name().to_str().is_some_and(valid_name) || retained.contains(&path) {
                continue;
            }
            let verified = (|| -> Result<_> {
                if path.canonicalize()?.parent() != Some(root.as_path()) {
                    return Err(Error::InvalidPath("catalog manifest escaped cache".into()));
                }
                let (manifest, data) = Manifest::open(&path)?;
                if codec::encode(manifest.status.scope.as_os_str())
                    != codec::encode(scope.as_os_str())
                {
                    return Err(Error::InvalidIndex("catalog cleanup scope mismatch".into()));
                }
                let (files, queues) = data_files(&data.root, &root)?;
                let bytes = fs::metadata(&path)?.len();
                let directory = data.root;
                drop(manifest);
                Ok((files, queues, directory, bytes))
            })();
            let Ok((_files, _queues, directory, bytes)) = verified else {
                report.retained_unverified += 1;
                continue;
            };
            if ensure_marker(&directory, &scope, &path).is_err() {
                report.retained_unverified += 1;
                continue;
            }
            let deleted = {
                let Some(_guard) = publication_lock(&root)? else {
                    report.retained_unverified += 1;
                    continue;
                };
                if protected_pointer(&root, &path)? {
                    continue;
                }
                fs::remove_file(&path).is_ok()
            };
            if !deleted {
                report.retained_unverified += 1;
                continue;
            }
            report.removed_generations += 1;
            report.removed_bytes += bytes;
            if finish_data_cleanup(&directory, &root, &scope, &path, &mut report).is_err() {
                report.retained_unverified += 1;
            }
            remove_sidecars(&root, &path, &mut report)?;
        }
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("catalog-") && name.ends_with(".data"))
                && fs::symlink_metadata(&path).is_ok_and(|metadata| plain(&metadata, true))
                && path.join(PENDING).try_exists().unwrap_or(false)
                && retry_orphan(&path, &root, &scope, &mut report).is_err()
            {
                report.retained_unverified += 1;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        BuildOptions, Cancellation, QuerySpec,
        catalog::{NativeIndexBuilder, format},
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    fn fixture() -> (PathBuf, NativeIndex) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "foldex-cleanup-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("scope")).unwrap();
        fs::write(root.join("scope/file.txt"), b"a").unwrap();
        let cancel = Cancellation::new();
        let mut builder =
            NativeIndexBuilder::create(&root.join("cache"), &root.join("scope"), &cancel).unwrap();
        while !builder.scan_step(256, &cancel, &|| {}).unwrap().done {}
        (root, builder.finish(true).unwrap())
    }
    fn data_path(manifest: &Path) -> PathBuf {
        manifest.with_file_name(format!(
            "{}.data",
            manifest.file_stem().unwrap().to_string_lossy()
        ))
    }
    #[test]
    fn publication_gate_is_shared_across_handles_and_releases_on_drop() {
        let (root, index) = fixture();
        let cache = index.manifest.cache_dir.clone();
        let guard = publication_lock(&cache).unwrap().unwrap();
        assert!(publication_lock(&cache).unwrap().is_none());
        drop(guard);
        assert!(publication_lock(&cache).unwrap().is_some());
        drop(index);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn orphan_retry_requires_matching_ownership_and_preserves_unknown_content() {
        let (root, index) = fixture();
        let manifest = index.status().database_path.clone();
        let scope = index.status().scope.clone();
        let cache = index.manifest.cache_dir.clone();
        let data = data_path(&manifest);
        ensure_marker(&data, &scope, &manifest).unwrap();
        drop(index);
        fs::remove_file(&manifest).unwrap();
        let mut report = CleanupReport::default();
        assert!(retry_orphan(&data, &cache, &scope.join("different"), &mut report).is_err());
        assert!(data.join("shard-00000000.bin").exists());
        fs::write(data.join("keep.txt"), b"unrecognized").unwrap();
        assert!(retry_orphan(&data, &cache, &scope, &mut report).is_err());
        assert_eq!(fs::read(data.join("keep.txt")).unwrap(), b"unrecognized");
        assert!(data.join("shard-00000000.bin").exists());
        fs::remove_file(data.join("keep.txt")).unwrap();
        retry_orphan(&data, &cache, &scope, &mut report).unwrap();
        assert!(!data.exists());
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn unsuccessful_data_deletion_keeps_the_retry_marker() {
        use std::os::unix::fs::PermissionsExt;
        let (root, index) = fixture();
        let manifest = index.status().database_path.clone();
        let scope = index.status().scope.clone();
        let cache = index.manifest.cache_dir.clone();
        let data = data_path(&manifest);
        ensure_marker(&data, &scope, &manifest).unwrap();
        drop(index);
        fs::remove_file(&manifest).unwrap();
        let original = fs::metadata(&data).unwrap().permissions();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o500)).unwrap();
        let result = retry_orphan(&data, &cache, &scope, &mut CleanupReport::default());
        if data.exists() {
            fs::set_permissions(&data, original).unwrap();
            result.unwrap();
            assert!(data.join(PENDING).exists());
            retry_orphan(&data, &cache, &scope, &mut CleanupReport::default()).unwrap();
            assert!(!data.exists());
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn hardlinked_mapped_shards_are_retried_after_the_reader_releases_them() {
        let (root, old) = fixture();
        let manifest = old.status().database_path.clone();
        let scope = old.status().scope.clone();
        let cache = old.manifest.cache_dir.clone();
        let data = data_path(&manifest);
        fs::write(scope.join("file.txt"), b"updated").unwrap();
        let cancel = Cancellation::new();
        let mut new = NativeIndex::reconcile(
            &manifest,
            &["file.txt".into()],
            &BuildOptions::default(),
            &cancel,
        )
        .unwrap()
        .index;
        let mapping = format::View::open(&new.data.shards[0]).unwrap();
        drop(old);
        NativeIndex::cleanup_owned(&cache, &scope).unwrap();
        assert!(!manifest.exists());
        // Windows that deletes a mapped hardlink name immediately has nothing left to retry.
        if data.exists() {
            assert!(data.join(PENDING).exists());
            drop(mapping);
            NativeIndex::cleanup_owned(&cache, &scope).unwrap();
            assert!(!data.exists());
        } else {
            drop(mapping);
        }
        assert_eq!(
            new.query(&QuerySpec::default(), 0, 256, &cancel)
                .unwrap()
                .rows[0]
                .size,
            7
        );
        drop(new);
        fs::remove_dir_all(root).unwrap();
    }
}
