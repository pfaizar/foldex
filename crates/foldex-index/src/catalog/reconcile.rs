//! Copy-on-write metadata reconciliation. Published generations are never edited.
use super::{
    Catalog, NativeIndex, format,
    manifest::{Manifest, plain},
};
use crate::{BuildOptions, BuildState, Cancellation, Error, Result, codec};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Duration,
};

#[derive(Default, Debug)]
pub struct ReconcileMetrics {
    pub metadata_reads: u64,
    pub enumerated_entries: u64,
    pub reused_shards: u64,
    pub copied_bytes: u64,
    pub retired_rows: u64,
}
pub struct Reconciled {
    pub index: NativeIndex,
    pub metrics: ReconcileMetrics,
}

fn changes(paths: &[PathBuf], directory_only: bool) -> Result<Vec<PathBuf>> {
    if paths.is_empty()
        || paths.len() > 1024
        || paths
            .iter()
            .map(|path| path.as_os_str().len())
            .sum::<usize>()
            > 256 * 1024
    {
        return Err(Error::WrongMode);
    }
    let mut paths = paths
        .iter()
        .map(|path| codec::relative(path, false))
        .collect::<Result<Vec<_>>>()?;
    if directory_only && paths.iter().any(|path| path.components().count() != 1) {
        return Err(Error::WrongMode);
    }
    paths.sort_by_key(|path| path.components().count());
    let mut unique = Vec::<PathBuf>::new();
    for path in paths {
        if !unique.iter().any(|parent| path.starts_with(parent)) {
            unique.push(path);
        }
    }
    Ok(unique)
}
fn metadata(
    scope: &Path,
    relative: &Path,
    missing: bool,
    metrics: &mut ReconcileMetrics,
) -> Result<Option<foldex_fs::Entry>> {
    metrics.metadata_reads += 1;
    match foldex_fs::Entry::stat(&scope.join(relative)) {
        Ok(entry) if Some(entry.name.as_os_str()) != relative.file_name() => Err(Error::WrongMode),
        Ok(entry) => Ok(Some(entry)),
        Err(error) if missing && error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
fn compact(data: &Catalog) -> bool {
    data.shards.len() > 1024 && data.entries / (data.shards.len() as u64) < 512
        || data.entries > 4096 && data.masked.saturating_mul(4) > data.entries
}
fn append(
    index: &mut NativeIndex,
    parent: &Path,
    entries: &[foldex_fs::Entry],
    cancel: &Cancellation,
) -> Result<()> {
    index.data.append_entries(parent, entries, cancel)?;
    let status = &mut index.manifest.status;
    status.entries = index.data.entries();
    for entry in entries {
        status.files += u64::from(!entry.is_dir());
        status.folders += u64::from(entry.is_dir());
        status.excluded_links += u64::from(entry.is_opaque());
    }
    Ok(())
}
impl NativeIndex {
    /// `changed` contains exact, scope-relative native notification names.
    /// WrongMode requests a full scan (lost coverage, partial source or compaction).
    /// An error discards the new generation; the source remains unchanged.
    pub fn reconcile(
        source: &Path,
        changed: &[PathBuf],
        options: &BuildOptions,
        cancel: &Cancellation,
    ) -> Result<Reconciled> {
        cancel.check()?;
        let source = Self::open_database(source)?;
        if !source.status().complete
            || source.status().synthetic
            || compact(&source.data)
            || source.data.shards.len() > 16_384
            || options.directory_only != source.status().directory_only
            || options.excluded_subtrees != source.status().excluded_subtrees
        {
            return Err(Error::WrongMode);
        }
        let paths = changes(changed, options.directory_only)?;
        if paths.iter().any(|path| {
            options
                .excluded_subtrees
                .iter()
                .any(|excluded| path.starts_with(excluded) || excluded.starts_with(path))
        }) {
            return Err(Error::WrongMode);
        }
        let scope = source.status().scope.clone();
        let mut metrics = ReconcileMetrics::default();
        let mut ancestors = HashSet::new();
        let mut ancestor_bytes = 0usize;
        for path in &paths {
            for ancestor in path
                .ancestors()
                .skip(1)
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                if !ancestors.contains(ancestor) {
                    ancestor_bytes = ancestor_bytes.saturating_add(ancestor.as_os_str().len());
                    if ancestor_bytes > 256 * 1024 || ancestors.len() >= 512 {
                        return Err(Error::WrongMode);
                    }
                    ancestors.insert(ancestor.to_path_buf());
                }
            }
        }
        let mut ancestors = ancestors.into_iter().collect::<Vec<_>>();
        ancestors.sort_by_key(|path| path.components().count());
        let mut parent_metadata = HashMap::new();
        // Check all ancestors before looking through a newly replaced junction.
        for path in ancestors {
            cancel.check()?;
            let entry = metadata(&scope, &path, false, &mut metrics)?.unwrap();
            if !entry.capabilities().can_enumerate() {
                return Err(Error::WrongMode);
            }
            parent_metadata.insert(path, entry);
        }
        let parents = paths
            .iter()
            .filter_map(|path| path.parent())
            .filter(|parent| {
                !parent.as_os_str().is_empty()
                    && !paths.iter().any(|changed| parent.starts_with(changed))
            })
            .map(Path::to_path_buf)
            .collect::<HashSet<_>>();
        let mut replacements = Vec::new();
        for path in &paths {
            cancel.check()?;
            replacements.push((path.clone(), metadata(&scope, path, true, &mut metrics)?));
        }
        let (manifest, mut data) = Manifest::create(
            &source.manifest.cache_dir,
            &scope,
            options.directory_only,
            &options.excluded_subtrees,
            cancel,
        )?;
        data.entries = source.data.entries;
        data.bytes = source.data.bytes;
        data.deleted = source.data.deleted.clone();
        data.masked = source.data.masked;
        for (number, shard) in source.data.shards.iter().enumerate() {
            cancel.check()?;
            if !plain(&fs::symlink_metadata(&shard.path)?, false) {
                return Err(Error::WrongMode);
            }
            let path = data.root.join(format!("shard-{number:08}.bin"));
            if fs::hard_link(&shard.path, &path).is_ok() {
                metrics.reused_shards += 1;
            } else {
                let input = fs::File::open(&shard.path)?;
                let mut input = input.take(shard.bytes.saturating_add(1));
                let mut output = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                let copied = io::copy(&mut input, &mut output)?;
                if copied != shard.bytes {
                    return Err(Error::InvalidIndex(
                        "source shard changed during reconciliation".into(),
                    ));
                }
                output.sync_all()?;
                metrics.copied_bytes += copied;
            }
            data.shards.push(format::Shard {
                path,
                first_id: shard.first_id,
                count: shard.count,
                bytes: shard.bytes,
                checksum: shard.checksum,
                verified: AtomicBool::new(false),
            });
        }
        let exact = parents.iter().cloned().collect::<Vec<_>>();
        let removed = data.mask_paths(&paths, &exact, cancel)?;
        if removed.exact_directories != parents {
            return Err(Error::WrongMode);
        }
        metrics.retired_rows = removed.files + removed.folders;
        let mut index = Self::new(manifest, data);
        index.manifest.status.entries = source.status().entries;
        index.manifest.status.files = source
            .status()
            .files
            .checked_sub(removed.files)
            .ok_or_else(|| {
                Error::InvalidIndex("source file count does not match its rows".into())
            })?;
        index.manifest.status.folders = source
            .status()
            .folders
            .checked_sub(removed.folders)
            .ok_or_else(|| {
                Error::InvalidIndex("source folder count does not match its rows".into())
            })?;
        index.manifest.status.excluded_links =
            source.status().excluded_links.saturating_sub(removed.links);
        index.manifest.status.excluded_cache = source.status().excluded_cache;
        if options
            .excluded_root
            .as_ref()
            .and_then(|excluded| excluded.strip_prefix(&scope).ok())
            .is_some_and(|excluded| paths.iter().any(|path| excluded.starts_with(path)))
        {
            index.manifest.status.excluded_cache = 0;
        }
        for parent in exact {
            let entry = parent_metadata.remove(&parent).ok_or(Error::WrongMode)?;
            append(
                &mut index,
                parent.parent().unwrap_or(Path::new("")),
                &[entry],
                cancel,
            )?;
        }
        for (relative, entry) in replacements {
            let Some(entry) = entry else { continue };
            let descend = entry.is_dir() && !entry.is_opaque() && !options.directory_only;
            append(
                &mut index,
                relative.parent().unwrap_or(Path::new("")),
                &[entry],
                cancel,
            )?;
            if !descend {
                continue;
            }
            let absolute = scope.join(&relative);
            let excluded = options
                .excluded_root
                .as_ref()
                .unwrap_or(&source.manifest.cache_dir);
            let excluded_relative = excluded.strip_prefix(&absolute).ok().map(Path::to_path_buf);
            let mut walk = foldex_fs::Walk::start_with_reader(
                absolute,
                foldex_fs::WalkOptions {
                    excluded_subtrees: Vec::new(),
                    workers: 1,
                    recursive: true,
                    spool_root: index.data.root.join("queues"),
                    excluded_relative,
                    excluded_root: None,
                },
                foldex_fs::ReaderKind::Native,
            )?;
            loop {
                cancel.check()?;
                match walk.next(Duration::from_millis(4))? {
                    foldex_fs::WalkEvent::Pending => continue,
                    foldex_fs::WalkEvent::Finished => break,
                    foldex_fs::WalkEvent::Batch(batch) => {
                        metrics.enumerated_entries += batch.entries.len() as u64;
                        index.manifest.status.skipped += batch.skipped;
                        index.manifest.status.excluded_cache += batch.excluded;
                        append(
                            &mut index,
                            &relative.join(batch.parent.as_ref()),
                            &batch.entries,
                            cancel,
                        )?;
                    }
                }
            }
            walk.finish();
        }
        if compact(&index.data) {
            return Err(Error::WrongMode);
        }
        index.manifest.status.complete = index.manifest.status.skipped == 0;
        index.manifest.status.state = if index.status().complete {
            BuildState::Complete
        } else {
            BuildState::Partial
        };
        index.manifest.commit(&mut index.data)?;
        index.data.release_ingestion_buffers();
        cancel.check()?;
        index.manifest.publish_current()?;
        Ok(Reconciled { index, metrics })
    }
}
