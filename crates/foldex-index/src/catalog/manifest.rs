//! Small SQLite manifest only; filename rows and candidate filters live in shards.
//! Keeping the manifest connection open also prevents deleting an active generation.
use super::{Catalog, format};
use crate::{codec, model::*, store};
use rusqlite::{Connection, OpenFlags, params};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};

const APP_ID: i64 = 0x4649_4c43;
const FORMAT: i64 = 6;
static NEXT: AtomicU64 = AtomicU64::new(1);
const SCHEMA: &str = "
CREATE TABLE manifest(single INTEGER PRIMARY KEY CHECK(single=1),schema_version INTEGER NOT NULL,
 codec TEXT NOT NULL,normalization TEXT NOT NULL,generation TEXT NOT NULL,revision INTEGER NOT NULL,
 scope BLOB NOT NULL,state INTEGER NOT NULL,entries INTEGER NOT NULL,files INTEGER NOT NULL,folders INTEGER NOT NULL,
 skipped INTEGER NOT NULL,links INTEGER NOT NULL,excluded_cache INTEGER NOT NULL,started INTEGER NOT NULL,updated INTEGER NOT NULL,
 scope_modified INTEGER,synthetic INTEGER NOT NULL);
CREATE TABLE chunks(ordinal INTEGER PRIMARY KEY,first_id INTEGER NOT NULL,count INTEGER NOT NULL CHECK(count BETWEEN 1 AND 4096),
 bytes INTEGER NOT NULL CHECK(bytes BETWEEN 64 AND 2097152),checksum INTEGER NOT NULL);
CREATE TABLE traversal_policy(single INTEGER PRIMARY KEY CHECK(single=1),directory_only INTEGER NOT NULL CHECK(directory_only IN(0,1)));
INSERT INTO traversal_policy VALUES(1,0);
CREATE TABLE deleted(ordinal INTEGER PRIMARY KEY,bits BLOB NOT NULL CHECK(length(bits) BETWEEN 1 AND 512));
";

type Saved = (u64, u64, u64, u64, u64, u64, i64, bool, bool);
fn saved(status: &IndexStatus) -> Saved {
    (
        status.entries,
        status.files,
        status.folders,
        status.skipped,
        status.excluded_links,
        status.excluded_cache,
        status.state.code(),
        status.synthetic,
        status.directory_only,
    )
}

pub(super) struct Manifest {
    pub status: IndexStatus,
    pub cache_dir: PathBuf,
    pub connection: Connection,
    published: usize,
    saved: Saved,
}

fn configure(connection: &Connection, create: bool) -> Result<()> {
    connection.busy_timeout(Duration::from_millis(100))?;
    connection.set_prepared_statement_cache_capacity(4);
    if create {
        connection.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA wal_autocheckpoint=128;")?;
    }
    connection.execute_batch("PRAGMA cache_size=-128; PRAGMA temp_store=FILE; PRAGMA temp.cache_size=-512; PRAGMA mmap_size=0; PRAGMA trusted_schema=OFF;")?;
    Ok(())
}

fn data_root(database: &Path) -> Result<PathBuf> {
    let stem = database
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::InvalidIndex("invalid catalog generation filename".into()))?;
    if !stem.starts_with("catalog-")
        || stem.len() > 100
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(Error::InvalidIndex(
            "invalid catalog generation filename".into(),
        ));
    }
    Ok(database.with_file_name(format!("{stem}.data")))
}

impl Manifest {
    pub fn create(
        cache_dir: &Path,
        scope: &Path,
        directory_only: bool,
        cancel: &Cancellation,
    ) -> Result<(Self, Catalog)> {
        cancel.check()?;
        fs::create_dir_all(cache_dir)?;
        let cache_dir = fs::canonicalize(cache_dir)?;
        let scope = absolute(scope)?;
        let started = store::now_ms();
        let generation = format!(
            "{started}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let database_path = cache_dir.join(format!("catalog-{generation}.sqlite3"));
        drop(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&database_path)?,
        );
        let connection = Connection::open_with_flags(
            &database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&connection, true)?;
        connection.execute_batch(&format!(
            "PRAGMA application_id={APP_ID}; PRAGMA user_version={FORMAT}; {SCHEMA}"
        ))?;
        let source_root_modified_secs = fs::metadata(&scope)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(store::unix_secs);
        let status = IndexStatus {
            generation,
            revision: 0,
            scope,
            database_path,
            state: BuildState::Building,
            complete: false,
            entries: 0,
            files: 0,
            folders: 0,
            skipped: 0,
            excluded_links: 0,
            excluded_cache: 0,
            started_unix_ms: started,
            updated_unix_ms: started,
            source_root_modified_secs,
            from_cache: false,
            synthetic: false,
            directory_only,
        };
        connection.execute(
            "INSERT INTO manifest VALUES(1,2,?1,?2,?3,0,?4,0,0,0,0,0,0,0,?5,?5,?6,0)",
            params![
                codec::CODEC,
                store::normalization(),
                status.generation,
                codec::encode(status.scope.as_os_str()),
                started as i64,
                source_root_modified_secs
            ],
        )?;
        connection.execute(
            "UPDATE traversal_policy SET directory_only=?1 WHERE single=1",
            params![directory_only],
        )?;
        let catalog = Catalog::create(&data_root(&status.database_path)?)?;
        super::cleanup::publish_pointer(
            &cache_dir,
            "building-native.index",
            &status.database_path,
        )?;
        let saved = saved(&status);
        Ok((
            Self {
                connection,
                status,
                cache_dir,
                published: 0,
                saved,
            },
            catalog,
        ))
    }

    pub fn open(database: &Path) -> Result<(Self, Catalog)> {
        let database = absolute(database)?;
        if !plain(&fs::symlink_metadata(&database)?, false) {
            return Err(Error::InvalidIndex(
                "catalog manifest must be a regular file".into(),
            ));
        }
        let connection = Connection::open_with_flags(
            &database,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&connection, false)?;
        let app: i64 = connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if app != APP_ID || version != FORMAT {
            return Err(Error::InvalidIndex(
                "catalog manifest identity/version mismatch".into(),
            ));
        }
        connection.execute_batch("BEGIN DEFERRED")?;
        let loaded = (|| -> Result<_> {
            let mut status = store::load_status(&connection, &database)?;
            if status.generation.len() > 100
                || database.file_stem().and_then(|s| s.to_str())
                    != Some(&format!("catalog-{}", status.generation))
            {
                return Err(Error::InvalidIndex(
                    "catalog generation identity mismatch".into(),
                ));
            }
            let root = data_root(&database)?;
            if !plain(&fs::symlink_metadata(&root)?, true) {
                return Err(Error::InvalidIndex(
                    "catalog data root must be a regular directory".into(),
                ));
            }
            let mut lookup = connection.prepare(
                "SELECT ordinal,first_id,count,bytes,checksum FROM chunks ORDER BY ordinal",
            )?;
            let mut source = lookup.query([])?;
            let mut shards = Vec::new();
            let (mut expected, mut bytes) = (1u64, 0u64);
            while let Some(row) = source.next()? {
                if shards.len() >= 1_000_000 {
                    return Err(Error::InvalidIndex(
                        "catalog manifest exceeds descriptor bound".into(),
                    ));
                }
                let ordinal: i64 = row.get(0)?;
                let first: i64 = row.get(1)?;
                let count: i64 = row.get(2)?;
                let length: i64 = row.get(3)?;
                let checksum: i64 = row.get(4)?;
                if ordinal != shards.len() as i64
                    || first < 1
                    || first as u64 != expected
                    || !(1..=4096).contains(&count)
                    || !(64..=2 * 1024 * 1024).contains(&length)
                    || !(0..=u32::MAX as i64).contains(&checksum)
                {
                    return Err(Error::InvalidIndex(
                        "invalid catalog chunk descriptors".into(),
                    ));
                }
                shards.push(format::Shard {
                    path: root.join(format!("shard-{ordinal:08}.bin")),
                    first_id: expected,
                    count: count as usize,
                    bytes: length as u64,
                    checksum: checksum as u32,
                    verified: AtomicBool::new(false),
                });
                expected = expected
                    .checked_add(count as u64)
                    .ok_or_else(|| Error::InvalidIndex("catalog row count overflow".into()))?;
                bytes = bytes
                    .checked_add(length as u64)
                    .ok_or_else(|| Error::InvalidIndex("catalog byte count overflow".into()))?;
            }
            let mut deleted = std::collections::HashMap::new();
            let mut masked = 0u64;
            let mut mask_bytes = 0usize;
            let mut lookup_masks = connection.prepare("SELECT ordinal,bits FROM deleted")?;
            let mut masks = lookup_masks.query([])?;
            while let Some(row) = masks.next()? {
                let ordinal: i64 = row.get(0)?;
                let shard = usize::try_from(ordinal)
                    .ok()
                    .and_then(|index| shards.get(index))
                    .ok_or_else(|| Error::InvalidIndex("invalid deletion mask shard".into()))?;
                let bits = row.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
                if bits.len() != shard.count.div_ceil(8)
                    || bits.is_empty()
                    || shard.count % 8 != 0
                        && bits.last().unwrap() & !((1u8 << (shard.count % 8)) - 1) != 0
                {
                    return Err(Error::InvalidIndex(
                        "invalid deletion mask length or tail".into(),
                    ));
                }
                mask_bytes = mask_bytes.saturating_add(bits.len() + 64);
                if mask_bytes > super::masks::MASK_BUDGET {
                    return Err(Error::InvalidIndex(
                        "deletion masks exceed their memory budget".into(),
                    ));
                }
                masked += bits
                    .iter()
                    .map(|byte| u64::from(byte.count_ones()))
                    .sum::<u64>();
                deleted.insert(ordinal as usize, bits.to_vec());
            }
            if expected - 1 != status.entries
                || status
                    .files
                    .checked_add(status.folders)
                    .and_then(|live| live.checked_add(masked))
                    != Some(status.entries)
            {
                return Err(Error::InvalidIndex(
                    "catalog live/deleted totals do not match chunks".into(),
                ));
            }
            status.from_cache = true;
            let catalog = Catalog {
                root,
                shards,
                pending: format::Buffer::new(),
                entries: status.entries,
                bytes,
                lower_scratch: String::new(),
                natural_scratch: Vec::new(),
                signature_scratch: format::Signature::default(),
                substring_safe: false,
                deleted,
                masked,
            };
            Ok((status, catalog))
        })();
        connection.execute_batch("ROLLBACK")?;
        let (status, catalog) = loaded?;
        let published = catalog.shards.len();
        let saved = saved(&status);
        let cache_dir = database
            .parent()
            .ok_or_else(|| Error::InvalidIndex("catalog cache has no parent".into()))?
            .to_owned();
        Ok((
            Self {
                connection,
                status,
                cache_dir,
                published,
                saved,
            },
            catalog,
        ))
    }

    pub fn commit(&mut self, catalog: &mut Catalog) -> Result<()> {
        catalog.checkpoint()?;
        if self.status.entries != catalog.published_entries() {
            return Err(Error::InvalidIndex(
                "catalog publication count mismatch".into(),
            ));
        }
        let state = saved(&self.status);
        if state == self.saved && self.published == catalog.shards.len() {
            return Ok(());
        }
        let mut status = self.status.clone();
        status.revision += 1;
        status.updated_unix_ms = store::now_ms();
        self.connection.execute_batch("BEGIN IMMEDIATE")?;
        let written = (|| -> Result<()> {
            let mut insert = self
                .connection
                .prepare_cached("INSERT INTO chunks VALUES(?1,?2,?3,?4,?5)")?;
            for (ordinal, shard) in catalog.shards.iter().enumerate().skip(self.published) {
                insert.execute(params![
                    ordinal as i64,
                    shard.first_id as i64,
                    shard.count as i64,
                    shard.bytes as i64,
                    shard.checksum
                ])?;
            }
            self.connection.execute("DELETE FROM deleted", [])?;
            let mut masks = self
                .connection
                .prepare_cached("INSERT INTO deleted VALUES(?1,?2)")?;
            for (ordinal, bits) in &catalog.deleted {
                masks.execute(params![*ordinal as i64, bits])?;
            }
            store::write_status(&self.connection, &status)?;
            Ok(())
        })();
        if let Err(error) = written {
            let _ = self.connection.execute_batch("ROLLBACK");
            return Err(error);
        }
        self.connection.execute_batch("COMMIT")?;
        self.status = status;
        self.saved = state;
        self.published = catalog.shards.len();
        Ok(())
    }

    pub fn publish_current(&self) -> Result<()> {
        super::cleanup::publish_pointer(
            &self.cache_dir,
            "current-native.index",
            &self.status.database_path,
        )
    }
}

pub(super) fn plain(metadata: &fs::Metadata, directory: bool) -> bool {
    if metadata.is_dir() != directory || (!directory && !metadata.is_file()) {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 == 0
    }
    #[cfg(not(windows))]
    {
        !metadata.is_symlink()
    }
}
