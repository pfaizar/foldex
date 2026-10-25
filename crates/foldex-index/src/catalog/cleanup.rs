use super::{
    NativeIndex,
    manifest::{Manifest, plain},
};
use crate::{codec, model::*};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

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
        if active.len() > 2 {
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
            let Ok((files, queues, directory, bytes)) = verified else {
                report.retained_unverified += 1;
                continue;
            };
            if fs::remove_file(&path).is_err() {
                report.retained_unverified += 1;
                continue;
            }
            report.removed_generations += 1;
            report.removed_bytes += bytes;
            for (file, bytes) in files {
                if fs::remove_file(file).is_ok() {
                    report.removed_bytes += bytes;
                }
            }
            if queues {
                let _ = fs::remove_dir(directory.join("queues"));
            }
            let _ = fs::remove_dir(directory);
            for suffix in ["-wal", "-shm", "-journal"] {
                let sidecar = root.join(format!("{}{suffix}", entry.file_name().to_string_lossy()));
                if let Ok(metadata) = fs::symlink_metadata(&sidecar) {
                    if plain(&metadata, false)
                        && sidecar.canonicalize()?.parent() == Some(root.as_path())
                        && fs::remove_file(sidecar).is_ok()
                    {
                        report.removed_bytes += metadata.len();
                    }
                }
            }
        }
        Ok(report)
    }
}
