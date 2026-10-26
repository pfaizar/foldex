//! Sparse immutable deletion masks. Raw row IDs remain append-only within a generation.
use super::{Catalog, format};
use crate::{Cancellation, Error, Result, codec};
use std::{collections::HashSet, path::PathBuf};
pub(super) const MASK_BUDGET: usize = 4 * 1024 * 1024;
#[derive(Default, Debug)]
pub(super) struct Removed {
    pub files: u64,
    pub folders: u64,
    pub links: u64,
    pub exact_directories: HashSet<PathBuf>,
}
pub(super) fn deleted(mask: Option<&Vec<u8>>, row: usize) -> bool {
    mask.is_some_and(|bits| {
        bits.get(row / 8)
            .is_some_and(|byte| byte & (1 << (row % 8)) != 0)
    })
}
impl Catalog {
    pub(super) fn masked(&self, shard: usize, row: usize) -> bool {
        deleted(self.deleted.get(&shard), row)
    }
    pub(super) fn mask_bytes(&self) -> usize {
        self.deleted
            .iter()
            .map(|(_, bits)| bits.capacity() + 64)
            .sum()
    }
    pub(super) fn mask_paths(
        &mut self,
        prefixes: &[PathBuf],
        exact: &[PathBuf],
        cancel: &Cancellation,
    ) -> Result<Removed> {
        let prefixes = prefixes.iter().cloned().collect::<HashSet<_>>();
        let exact = exact.iter().cloned().collect::<HashSet<_>>();
        let mut removed = Removed::default();
        let mut mask_bytes = self.mask_bytes();
        for (number, shard) in self.shards.iter().enumerate() {
            cancel.check()?;
            let view = format::View::open(shard)?;
            let previous = self.deleted.remove(&number);
            if let Some(bits) = &previous {
                mask_bytes = mask_bytes.saturating_sub(bits.capacity() + 64);
            }
            let mut bits = previous.unwrap_or_else(|| vec![0; shard.count.div_ceil(8)]);
            for row in 0..shard.count {
                if row % 128 == 0 {
                    cancel.check()?;
                }
                if deleted(Some(&bits), row) {
                    continue;
                }
                let record = view.record(row)?;
                let path = PathBuf::from(codec::decode(record.path())?)
                    .join(codec::decode(record.name())?);
                if !exact.contains(&path)
                    && !path.ancestors().any(|parent| prefixes.contains(parent))
                {
                    continue;
                }
                if exact.contains(&path) {
                    if record.flags & 1 == 0 || record.flags & 4 != 0 {
                        return Err(Error::WrongMode);
                    }
                    removed.exact_directories.insert(path.clone());
                }
                bits[row / 8] |= 1 << (row % 8);
                self.masked += 1;
                if record.flags & 1 != 0 {
                    removed.folders += 1;
                } else {
                    removed.files += 1;
                }
                removed.links += u64::from(record.flags & 4 != 0);
            }
            if bits.iter().any(|byte| *byte != 0) {
                mask_bytes += bits.capacity() + 64;
                self.deleted.insert(number, bits);
            }
            if mask_bytes > MASK_BUDGET {
                return Err(Error::WrongMode);
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        IndexRecord, QuerySpec,
        catalog::{NativeIndex, NativeIndexBuilder},
    };
    #[test]
    fn masks_survive_reopening_and_filter_ranked_pages_spools_and_launcher_candidates() {
        let root = std::env::temp_dir().join(format!("foldex-mask-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("scope")).unwrap();
        let cancel = Cancellation::new();
        let mut builder =
            NativeIndexBuilder::create(&root.join("cache"), &root.join("scope"), &cancel).unwrap();
        for first in (0..4096).step_by(256) {
            let rows = (first..first + 256)
                .map(|i| {
                    IndexRecord::new(
                        format!(
                            "{}\\report{i:04}.txt",
                            if i % 2 == 0 { "gone" } else { "kept" }
                        )
                        .replace('\\', "/")
                        .into(),
                        false,
                        1,
                        0,
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            builder.ingest_batch(&rows, &cancel).unwrap();
        }
        builder.checkpoint().unwrap();
        let index = builder.index.as_mut().unwrap();
        let removed = index
            .data
            .mask_paths(&[PathBuf::from("gone")], &[], &cancel)
            .unwrap();
        assert_eq!(removed.files, 2048);
        index.manifest.status.files -= removed.files;
        let index = builder.finish(true).unwrap();
        let path = index.status().database_path.clone();
        drop(index);
        let mut index = NativeIndex::open_database(&path).unwrap();
        assert_eq!(index.status().entries, 4096);
        assert_eq!(index.status().files, 2048);
        for spec in [
            QuerySpec::default(),
            QuerySpec {
                text: "rpt".into(),
                ..Default::default()
            },
        ] {
            let first = index.query(&spec, 0, 256, &cancel).unwrap();
            assert!(
                first
                    .rows
                    .iter()
                    .all(|row| row.relative_path.starts_with("kept"))
            );
            let deep = index.query(&spec, 1950, 128, &cancel).unwrap();
            assert!(
                deep.rows
                    .iter()
                    .all(|row| row.relative_path.starts_with("kept"))
            );
            assert!(!deep.has_more);
            assert_eq!(deep.rows.len(), 98);
        }
        let batch = index
            .candidate_batch(0, &cancel, &mut |_, parent, _, _| {
                assert_eq!(parent, "kept");
                Some(1)
            })
            .unwrap();
        assert_eq!(batch.scanned_to, 4096);
        assert_eq!(batch.hits.len(), 64);
        drop(index);
        std::fs::remove_dir_all(root).unwrap();
    }
}
