use crate::{codec, model::*};
use rusqlite::{Connection, OpenFlags, params};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const APP_ID: i64 = 0x4649_4c49;
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);
pub(crate) const SELECT_ROW: &str = "SELECT e.id,e.relative,e.is_dir,e.hidden,e.symlink,e.size_key,e.modified,e.natural_key,e.raw_sort,e.extension FROM entries e WHERE e.id=?1";

const SCHEMA: &str = "
CREATE TABLE manifest(single INTEGER PRIMARY KEY CHECK(single=1),schema_version INTEGER NOT NULL,
 codec TEXT NOT NULL,normalization TEXT NOT NULL,generation TEXT NOT NULL,revision INTEGER NOT NULL,
 scope BLOB NOT NULL,state INTEGER NOT NULL,entries INTEGER NOT NULL,files INTEGER NOT NULL,folders INTEGER NOT NULL,
 skipped INTEGER NOT NULL,links INTEGER NOT NULL,excluded_cache INTEGER NOT NULL,started INTEGER NOT NULL,updated INTEGER NOT NULL,
 scope_modified INTEGER,synthetic INTEGER NOT NULL);
CREATE TABLE entries(id INTEGER PRIMARY KEY,relative BLOB NOT NULL,
 lower_name TEXT NOT NULL,source_key TEXT,is_dir INTEGER NOT NULL CHECK(is_dir IN(0,1)),hidden INTEGER NOT NULL,symlink INTEGER NOT NULL,
 size_key BLOB NOT NULL CHECK(length(size_key)=8),modified INTEGER NOT NULL,natural_key BLOB NOT NULL,raw_sort BLOB NOT NULL,extension TEXT NOT NULL);
CREATE INDEX entries_name ON entries(is_dir,natural_key,raw_sort,id);
CREATE INDEX entries_hidden ON entries(hidden,is_dir);
CREATE VIRTUAL TABLE names_fts USING fts5(lower_name,content='entries',content_rowid='id',tokenize='trigram case_sensitive 1',detail='none');
CREATE VIRTUAL TABLE characters_fts USING fts5(tokens,content='',tokenize='ascii',detail='none');
CREATE VIRTUAL TABLE names_vocab USING fts5vocab(names_fts,'row');
CREATE VIRTUAL TABLE characters_vocab USING fts5vocab(characters_fts,'row');
CREATE TABLE pending(id INTEGER PRIMARY KEY,relative BLOB NOT NULL UNIQUE);
";

pub struct Index {
    pub(crate) connection: Connection,
    pub(crate) status: IndexStatus,
    pub(crate) count_cache: Option<(u64, u64, u64, u64)>,
    pub(crate) candidate_plan: Option<crate::query::CandidatePlan>,
    pub(crate) rank_cache: std::cell::RefCell<crate::rank_cache::RankCache>,
    #[cfg(feature = "diagnostics")]
    pub(crate) query_cache_enabled: bool,
    #[cfg(feature = "diagnostics")]
    pub(crate) query_diagnostics: std::cell::RefCell<QueryDiagnostics>,
}

impl Index {
    /// Opens the current finished generation, or the latest partial build if no finished one exists.
    pub fn open(cache_dir: &Path) -> Result<Self> {
        let pointer = if cache_dir.join("current.index").is_file() {
            "current.index"
        } else {
            "building.index"
        };
        let text = read_pointer(cache_dir, pointer)?;
        let name = text.trim();
        if name.len() > 128
            || !name.starts_with("index-")
            || !name.ends_with(".sqlite3")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
        {
            return Err(Error::InvalidIndex("invalid generation pointer".into()));
        }
        Self::open_database(&cache_dir.join(name))
    }

    pub fn open_scope(cache_dir: &Path, scope: &Path) -> Result<Self> {
        let index = Self::open(cache_dir)?;
        if index.status.scope != absolute(scope)? {
            return Err(Error::InvalidIndex(
                "cached scope does not match requested scope".into(),
            ));
        }
        Ok(index)
    }

    /// Opens a caller-selected index generation for progressive committed results.
    pub fn open_database(database_path: &Path) -> Result<Self> {
        let connection = Connection::open_with_flags(
            database_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        configure(&connection, false)?;
        let application: i64 =
            connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
        if application != APP_ID {
            return Err(Error::InvalidIndex("application identity mismatch".into()));
        }
        let mut status = load_status(&connection, database_path)?;
        status.from_cache = true;
        Ok(Self {
            connection,
            status,
            count_cache: None,
            candidate_plan: None,
            rank_cache: std::cell::RefCell::default(),
            #[cfg(feature = "diagnostics")]
            query_cache_enabled: true,
            #[cfg(feature = "diagnostics")]
            query_diagnostics: std::cell::RefCell::default(),
        })
    }

    pub fn status(&self) -> &IndexStatus {
        &self.status
    }

    #[cfg(feature = "diagnostics")]
    pub fn last_query_diagnostics(&self) -> QueryDiagnostics {
        self.query_diagnostics.borrow().clone()
    }

    /// Stable cache directory helper. Scope identity is also verified inside each database.
    pub fn cache_dir_for(base: &Path, scope: &Path) -> Result<PathBuf> {
        let scope = absolute(scope)?;
        let hash = codec::encode(scope.as_os_str())
            .iter()
            .fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ *byte as u64).wrapping_mul(0x100000001b3)
            });
        Ok(base.join(format!("scope-{hash:016x}")))
    }

    /// Worker-only: call after dropping connections to obsolete generations. Retains the two
    /// pointer targets and removes only databases with verified identity, schema, and scope.
    /// Unrelated files, links/reparse points, malformed databases, and busy files are retained.
    pub fn cleanup_owned(cache_dir: &Path, scope: &Path) -> Result<CleanupReport> {
        Self::cleanup_owned_retaining(cache_dir, scope, &[])
    }

    /// Retain up to two explicitly active connection files in addition to current/building.
    /// Call only on the owning worker after obsolete connections have been dropped.
    pub fn cleanup_owned_retaining(
        cache_dir: &Path,
        scope: &Path,
        active: &[PathBuf],
    ) -> Result<CleanupReport> {
        if active.len() > 2 {
            return Err(Error::InvalidIndex(
                "at most two active generation files may be retained".into(),
            ));
        }
        let root = fs::canonicalize(cache_dir)?;
        let scope = absolute(scope)?;
        let mut retained = Vec::with_capacity(4);
        for path in active {
            let path = path.canonicalize()?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::InvalidIndex("invalid active generation filename".into()))?;
            if path.parent() != Some(root.as_path()) || !valid_generation_name(name) {
                return Err(Error::InvalidIndex(
                    "active generation is outside its cache".into(),
                ));
            }
            retained.push(name.to_owned());
        }
        for pointer in ["current.index", "building.index"] {
            match read_pointer(&root, pointer) {
                Ok(value) => {
                    let name = value.trim();
                    if !valid_generation_name(name) {
                        return Err(Error::InvalidIndex(
                            "invalid generation pointer; cache cleanup refused".into(),
                        ));
                    }
                    retained.push(name.to_owned());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        let mut report = CleanupReport::default();
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !valid_generation_name(name) || retained.iter().any(|keep| keep == name) {
                continue;
            }
            let path = entry.path();
            let verified = (|| -> Result<u64> {
                let metadata = fs::symlink_metadata(&path)?;
                if !plain_file(&metadata) || path.canonicalize()?.parent() != Some(root.as_path()) {
                    return Err(Error::InvalidIndex(
                        "generation path is not an owned regular file".into(),
                    ));
                }
                let connection = Connection::open_with_flags(
                    &path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?;
                connection.busy_timeout(Duration::from_millis(25))?;
                let application: i64 =
                    connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
                let schema: i64 =
                    connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
                if application != APP_ID || schema != 2 {
                    return Err(Error::InvalidIndex("generation ownership mismatch".into()));
                }
                let (generation, owned_scope) = cleanup_identity(&connection)?;
                if owned_scope != scope || name != format!("index-{generation}.sqlite3") {
                    return Err(Error::InvalidIndex(
                        "generation scope or filename mismatch".into(),
                    ));
                }
                drop(connection);
                Ok(metadata.len())
            })();
            let Ok(bytes) = verified else {
                report.retained_unverified += 1;
                continue;
            };
            if fs::remove_file(&path).is_err() {
                report.retained_unverified += 1;
                continue;
            }
            report.removed_generations += 1;
            report.removed_bytes += bytes;
            for suffix in ["-wal", "-shm", "-journal"] {
                let sidecar = root.join(format!("{name}{suffix}"));
                if let Ok(metadata) = fs::symlink_metadata(&sidecar) {
                    if plain_file(&metadata)
                        && sidecar
                            .canonicalize()
                            .ok()
                            .as_ref()
                            .and_then(|path| path.parent())
                            == Some(root.as_path())
                        && fs::remove_file(&sidecar).is_ok()
                    {
                        report.removed_bytes += metadata.len();
                    }
                }
            }
        }
        Ok(report)
    }

    pub fn metrics(&self) -> Result<CacheMetrics> {
        let (mut used, mut highwater) = (0, 0);
        // SAFETY: the borrowed connection remains alive; this read-only SQLite status call
        // receives valid output pointers and does not retain them.
        let result = unsafe {
            rusqlite::ffi::sqlite3_db_status(
                self.connection.handle(),
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                &mut used,
                &mut highwater,
                0,
            )
        };
        if result != rusqlite::ffi::SQLITE_OK {
            return Err(
                rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(result), None).into(),
            );
        }
        // SAFETY: SQLite's global accounting accessor takes no pointers and is thread safe.
        let sqlite_memory = unsafe { rusqlite::ffi::sqlite3_memory_used() };
        let database_bytes = fs::metadata(&self.status.database_path)?.len();
        let fts5: bool = self.connection.query_row(
            "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| row.get(0),
        )?;
        Ok(CacheMetrics {
            page_cache_budget: PAGE_CACHE_BUDGET,
            page_cache_used: used.max(0) as usize,
            rank_cache_budget: 2 * crate::rank_cache::GROUP_BYTES,
            rank_cache_used: self.rank_cache.borrow().bytes(),
            sqlite_global_memory: sqlite_memory.max(0) as usize,
            database_bytes,
            sqlite_version: rusqlite::version().into(),
            fts5,
        })
    }

    /// Worker-only convenience traversal. Use IndexBuilder::scan_step to interleave live queries.
    pub fn build_scope(
        scope: &Path,
        cache_dir: &Path,
        progress: &mut dyn FnMut(BuildProgress),
        cancel: &Cancellation,
        foreground_yield: &dyn Fn(),
    ) -> Result<Self> {
        let mut builder = IndexBuilder::create(cache_dir, scope, cancel)?;
        loop {
            let update = builder.scan_step(512, cancel, foreground_yield)?;
            let done = update.done;
            progress(update);
            if done {
                return builder.finish(!cancel.is_cancelled());
            }
        }
    }
}

fn valid_generation_name(name: &str) -> bool {
    name.len() <= 128
        && name.starts_with("index-")
        && name.ends_with(".sqlite3")
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn read_pointer(cache_dir: &Path, name: &str) -> std::io::Result<String> {
    let path = cache_dir.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    if !plain_file(&metadata) || metadata.len() > 256 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "generation pointer is not a bounded regular file",
        ));
    }
    fs::read_to_string(path)
}

fn plain_file(metadata: &fs::Metadata) -> bool {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}

fn cleanup_identity(connection: &Connection) -> Result<(String, PathBuf)> {
    // Ownership is independent of the current query-normalization version: an upgrade must
    // still be able to reclaim its obsolete generations without accepting them for search.
    let (version, codec_name, generation, raw): (i64, String, String, Vec<u8>) = connection
        .query_row(
            "SELECT schema_version,codec,generation,scope FROM manifest WHERE single=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    if version != 2 || generation.len() > 100 || raw.len() > 128 * 1024 {
        return Err(Error::InvalidIndex(
            "unrecognized generation ownership".into(),
        ));
    }
    let scope = if codec_name == codec::CODEC {
        PathBuf::from(codec::decode(&raw)?)
    } else {
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            if codec_name != "windows-utf16le-v1" || raw.len() % 2 != 0 {
                return Err(Error::InvalidIndex("unrecognized ownership codec".into()));
            }
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            if units.contains(&0) {
                return Err(Error::InvalidIndex("NUL in owned scope".into()));
            }
            PathBuf::from(std::ffi::OsString::from_wide(&units))
        }
        #[cfg(not(windows))]
        {
            return Err(Error::InvalidIndex("unrecognized ownership codec".into()));
        }
    };
    if !scope.is_absolute() {
        return Err(Error::InvalidIndex("owned scope is not absolute".into()));
    }
    Ok((generation, scope))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    None,
    Manual,
    Filesystem,
}

pub struct IndexBuilder {
    pub(crate) index: Option<Index>,
    pub(crate) cache_dir: PathBuf,
    pub(crate) excluded_relative: Option<PathBuf>,
    pub(crate) excluded_root: PathBuf,
    pub(crate) canonical_scope_known: bool,
    pub(crate) started: Instant,
    pub(crate) transaction: bool,
    pub(crate) mode: Mode,
    pub(crate) current: Option<crate::walk::CurrentDirectory>,
    pub(crate) scan_workers: usize,
    pub(crate) directory_only: bool,
    pub(crate) walker: Option<foldex_fs::Walk>,
    pub(crate) walk_batch: Option<crate::parallel_walk::CurrentBatch>,
    pub(crate) walk_done: bool,
    pub(crate) last_commit: Instant,
    pub(crate) committed_entries: u64,
    pub(crate) stats: IngestionStats,
}

impl IndexBuilder {
    pub fn create(cache_dir: &Path, scope: &Path, cancel: &Cancellation) -> Result<Self> {
        Self::create_with_options(cache_dir, scope, &BuildOptions::default(), cancel)
    }

    pub fn create_with_options(
        cache_dir: &Path,
        scope: &Path,
        options: &BuildOptions,
        cancel: &Cancellation,
    ) -> Result<Self> {
        cancel.check()?;
        let scope = absolute(scope)?;
        fs::create_dir_all(cache_dir)?;
        let cache_dir = fs::canonicalize(cache_dir)?;
        let excluded_root = options
            .excluded_root
            .as_deref()
            .map(fs::canonicalize)
            .transpose()?
            .unwrap_or_else(|| cache_dir.clone());
        if !cache_dir.starts_with(&excluded_root) {
            return Err(Error::InvalidPath(
                "excluded cache root must contain this scope's cache directory".into(),
            ));
        }
        let canonical_scope = fs::canonicalize(&scope).ok();
        let excluded_relative = canonical_scope.as_ref().and_then(|root| {
            if root.starts_with(&excluded_root) {
                Some(PathBuf::new())
            } else {
                excluded_root.strip_prefix(root).ok().map(Path::to_path_buf)
            }
        });
        let canonical_scope_known = canonical_scope.is_some();
        let generation = format!(
            "{}-{}-{}",
            now_ms(),
            std::process::id(),
            NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
        );
        let database_path = cache_dir.join(format!("index-{generation}.sqlite3"));
        // Never overwrite an existing file in the caller's cache directory.
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
        connection.execute_batch("PRAGMA application_id=1179208777; PRAGMA user_version=2;")?;
        connection.execute_batch(SCHEMA)?;
        // The pinned bundled SQLite supports this FTS5 hash bound; verify at runtime.
        connection.execute(
            "INSERT INTO names_fts(names_fts,rank) VALUES('hashsize',262144)",
            [],
        )?;
        connection.execute(
            "INSERT INTO characters_fts(characters_fts,rank) VALUES('hashsize',262144)",
            [],
        )?;
        let fts5: bool = connection.query_row(
            "SELECT sqlite_compileoption_used('ENABLE_FTS5')",
            [],
            |row| row.get(0),
        )?;
        if !fts5 {
            return Err(Error::InvalidIndex("bundled engine lacks FTS5".into()));
        }
        let started = now_ms();
        let source_root_modified_secs = fs::metadata(&scope)
            .and_then(|metadata| metadata.modified())
            .ok()
            .map(unix_secs);
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
        };
        connection.execute(
            "INSERT INTO manifest VALUES(1,2,?1,?2,?3,0,?4,0,0,0,0,0,0,0,?5,?5,?6,0)",
            params![
                codec::CODEC,
                normalization(),
                status.generation,
                codec::encode(status.scope.as_os_str()),
                status.started_unix_ms as i64,
                source_root_modified_secs
            ],
        )?;
        write_pointer(&cache_dir, "building.index", &status.database_path)?;
        Ok(Self {
            index: Some(Index {
                connection,
                status,
                count_cache: None,
                candidate_plan: None,
                rank_cache: std::cell::RefCell::default(),
                #[cfg(feature = "diagnostics")]
                query_cache_enabled: true,
                #[cfg(feature = "diagnostics")]
                query_diagnostics: std::cell::RefCell::default(),
            }),
            cache_dir,
            excluded_relative,
            excluded_root,
            canonical_scope_known,
            started: Instant::now(),
            transaction: false,
            mode: Mode::None,
            current: None,
            scan_workers: 0,
            directory_only: options.directory_only,
            walker: None,
            walk_batch: None,
            walk_done: false,
            last_commit: Instant::now(),
            committed_entries: 0,
            stats: IngestionStats::default(),
        })
    }

    pub fn status(&self) -> &IndexStatus {
        &self.index.as_ref().expect("builder exists").status
    }

    /// Configure directory overlap before traversal. Zero retains the serial
    /// reference path; positive values use a bounded pool, capped at 16 workers.
    pub fn set_scan_workers(&mut self, workers: usize) -> Result<()> {
        if self.mode != Mode::None {
            return Err(Error::WrongMode);
        }
        self.scan_workers = workers.min(16);
        Ok(())
    }

    pub(crate) fn ensure_transaction(&mut self) -> Result<()> {
        if !self.transaction {
            self.index
                .as_ref()
                .unwrap()
                .connection
                .execute_batch("BEGIN IMMEDIATE")?;
            self.transaction = true;
        }
        Ok(())
    }

    pub fn ingest(&mut self, record: &IndexRecord, cancel: &Cancellation) -> Result<()> {
        self.ingest_batch(std::slice::from_ref(record), cancel)
    }

    /// Bounded batch ingestion; the caller can generate arbitrarily many batches without a RAM index.
    pub fn ingest_batch(&mut self, records: &[IndexRecord], cancel: &Cancellation) -> Result<()> {
        if self.mode == Mode::Filesystem {
            return Err(Error::WrongMode);
        }
        self.mode = Mode::Manual;
        self.index.as_mut().unwrap().status.synthetic = true;
        self.store_batch(records, cancel, false)
    }

    pub(crate) fn store_batch(
        &mut self,
        records: &[IndexRecord],
        cancel: &Cancellation,
        enqueue_directories: bool,
    ) -> Result<()> {
        cancel.check()?;
        if records.len() > 512
            || records.iter().map(IndexRecord::heap_bytes).sum::<usize>() > MAX_PAGE_BYTES
        {
            return Err(Error::InvalidPath(
                "ingestion batch exceeds 512 records or 1 MiB".into(),
            ));
        }
        self.ensure_transaction()?;
        let index = self.index.as_mut().unwrap();
        let mut insert = index.connection.prepare_cached("INSERT INTO entries(relative,lower_name,source_key,is_dir,hidden,symlink,size_key,modified,natural_key,raw_sort,extension) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)")?;
        let mut fts = index
            .connection
            .prepare_cached("INSERT INTO names_fts(rowid,lower_name) VALUES(?1,?2)")?;
        let mut pending = index
            .connection
            .prepare_cached("INSERT INTO pending(relative) VALUES(?1)")?;
        let mut characters = index
            .connection
            .prepare_cached("INSERT INTO characters_fts(rowid,tokens) VALUES(?1,?2)")?;
        for record in records {
            cancel.check()?;
            let relative = codec::relative(&record.relative_path, false)?;
            let encoded = codec::encode(relative.as_os_str());
            if encoded.len() > 128 * 1024 {
                return Err(Error::InvalidPath(
                    "path exceeds supported operating-system path size".into(),
                ));
            }
            let raw_name = relative
                .file_name()
                .ok_or_else(|| Error::InvalidPath("missing basename".into()))?;
            let name = raw_name.to_string_lossy();
            let lower = name.to_lowercase();
            let folded = foldex_match::fold_key(&name);
            let source_key = (folded != lower).then_some(&folded);
            let natural = codec::natural_key(&lower);
            let size = record.size.to_be_bytes();
            let entry_started = Instant::now();
            insert.execute(params![
                encoded,
                lower,
                source_key,
                record.is_dir,
                record.hidden,
                record.symlink,
                &size[..],
                record.modified_secs,
                natural,
                raw_name.as_encoded_bytes(),
                extension(&lower, record.is_dir)
            ])?;
            self.stats.entry_write += entry_started.elapsed();
            let id = index.connection.last_insert_rowid();
            let trigram_started = Instant::now();
            fts.execute(params![id, lower])?;
            self.stats.trigram_write += trigram_started.elapsed();
            let character_started = Instant::now();
            characters.execute(params![id, crate::fuzzy::tokens(&folded, false)])?;
            self.stats.character_write += character_started.elapsed();
            self.stats.records += 1;
            if enqueue_directories && !self.directory_only && record.is_dir && !record.symlink {
                pending.execute([&encoded])?;
            }
            index.status.entries += 1;
            if record.is_dir {
                index.status.folders += 1;
            } else {
                index.status.files += 1;
            }
            if record.symlink {
                index.status.excluded_links += 1;
            }
        }
        Ok(())
    }

    /// Commits a consistent progressive generation, making it readable by another worker.
    pub fn checkpoint(&mut self) -> Result<BuildProgress> {
        self.commit()?;
        Ok(BuildProgress {
            status: self.status().clone(),
            done: self.walk_done,
            elapsed: self.started.elapsed(),
        })
    }

    pub fn ingestion_stats(&self) -> &IngestionStats {
        &self.stats
    }

    pub(crate) fn progressive_checkpoint(&mut self) -> Result<BuildProgress> {
        if self.walk_done
            || self.last_commit.elapsed() >= Duration::from_millis(100)
            || self.status().entries.saturating_sub(self.committed_entries) >= 8192
        {
            self.commit()?;
        }
        Ok(BuildProgress {
            status: self.status().clone(),
            done: self.walk_done,
            elapsed: self.started.elapsed(),
        })
    }

    pub(crate) fn commit(&mut self) -> Result<()> {
        if !self.transaction {
            return Ok(());
        }
        let index = self.index.as_mut().unwrap();
        index.status.revision += 1;
        index.status.updated_unix_ms = now_ms();
        write_status(&index.connection, &index.status)?;
        let commit_started = Instant::now();
        index.connection.execute_batch("COMMIT")?;
        self.stats.commit += commit_started.elapsed();
        self.stats.commits += 1;
        self.committed_entries = index.status.entries;
        self.last_commit = Instant::now();
        self.transaction = false;
        Ok(())
    }

    pub fn query(
        &mut self,
        spec: &QuerySpec,
        offset: u64,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.commit()?;
        self.index
            .as_mut()
            .unwrap()
            .query(spec, offset, page_size, cancel)
    }
    pub fn query_after(
        &mut self,
        spec: &QuerySpec,
        cursor: Option<&PageCursor>,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.commit()?;
        self.index
            .as_mut()
            .unwrap()
            .query_after(spec, cursor, page_size, cancel)
    }
    pub fn query_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        offset: u64,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.commit()?;
        self.index
            .as_mut()
            .unwrap()
            .query_as_of(spec, boundary, offset, page_size, cancel)
    }
    pub fn query_after_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        cursor: Option<&PageCursor>,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.commit()?;
        self.index
            .as_mut()
            .unwrap()
            .query_after_as_of(spec, boundary, cursor, page_size, cancel)
    }
    pub fn metrics(&self) -> Result<CacheMetrics> {
        self.index.as_ref().unwrap().metrics()
    }

    #[cfg(feature = "diagnostics")]
    pub fn benchmark_set_query_cache_enabled(&mut self, enabled: bool) {
        self.index
            .as_mut()
            .unwrap()
            .benchmark_set_query_cache_enabled(enabled);
    }

    /// Measurement-only overrides; shipping construction keeps its normal defaults.
    #[cfg(feature = "diagnostics")]
    pub fn benchmark_configure_fts(
        &mut self,
        automerge: u32,
        crisismerge: u32,
        hash_bytes: u32,
    ) -> Result<()> {
        if self.status().entries != 0
            || automerge > 16
            || !(2..=2048).contains(&crisismerge)
            || !(64 * 1024..=8 * 1024 * 1024).contains(&hash_bytes)
        {
            return Err(Error::InvalidPath(
                "invalid benchmark FTS configuration".into(),
            ));
        }
        let connection = &self.index.as_ref().unwrap().connection;
        for table in ["names_fts", "characters_fts"] {
            for (command, value) in [
                ("automerge", automerge),
                ("crisismerge", crisismerge),
                ("hashsize", hash_bytes),
            ] {
                connection.execute(
                    &format!("INSERT INTO {table}({table},rank) VALUES(?1,?2)"),
                    params![command, value],
                )?;
            }
        }
        Ok(())
    }

    /// `complete` is supplied for manual ingestion; filesystem mode also requires exhausted traversal.
    pub fn finish(mut self, complete: bool) -> Result<Index> {
        let status = &mut self.index.as_mut().unwrap().status;
        status.state = if status.state == BuildState::Cancelled {
            BuildState::Cancelled
        } else if complete
            && (self.mode != Mode::Filesystem || self.walk_done)
            && status.skipped == 0
        {
            BuildState::Complete
        } else {
            BuildState::Partial
        };
        status.complete = status.state == BuildState::Complete;
        self.current = None;
        self.walk_batch = None;
        if let Some(mut walker) = self.walker.take() {
            walker.finish();
        }
        self.ensure_transaction()?;
        self.commit()?;
        let index = self.index.take().unwrap();
        if index.status.state != BuildState::Cancelled {
            write_pointer(
                &self.cache_dir,
                "current.index",
                &index.status.database_path,
            )?;
        }
        // Avoid forcing a global optimize/merge at finish; future compaction is a bounded idle task.
        index
            .connection
            .execute_batch("PRAGMA wal_checkpoint(PASSIVE); PRAGMA shrink_memory;")?;
        Ok(index)
    }

    pub fn finish_cancelled(mut self) -> Result<Index> {
        self.index.as_mut().unwrap().status.state = BuildState::Cancelled;
        self.finish(false)
    }
}

impl Drop for IndexBuilder {
    fn drop(&mut self) {
        if let Some(mut walker) = self.walker.take() {
            walker.finish();
        }
        if self.transaction {
            if let Some(index) = &self.index {
                let _ = index.connection.execute_batch("ROLLBACK");
            }
        }
    }
}

fn configure(connection: &Connection, create: bool) -> Result<()> {
    connection.busy_timeout(Duration::from_millis(100))?;
    // Only fixed hot statements use this cache; query-shaped SQL remains short-lived.
    connection.set_prepared_statement_cache_capacity(12);
    if create {
        connection.execute_batch("PRAGMA page_size=4096; PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA wal_autocheckpoint=256;")?;
    }
    // 3 MiB main + 512 KiB temp payload leaves headroom for page bookkeeping in a 4 MiB target.
    connection.execute_batch(
        "PRAGMA cache_size=-3072; PRAGMA temp_store=FILE; PRAGMA temp.cache_size=-512;
      PRAGMA mmap_size=0; PRAGMA cache_spill=ON; PRAGMA threads=1; PRAGMA trusted_schema=OFF;",
    )?;
    crate::fuzzy::register(connection)?;
    Ok(())
}

pub(crate) fn normalization() -> String {
    let (major, minor, patch) = std::char::UNICODE_VERSION;
    format!("rust-lowercase-v1-source-fold-v1-count8-natural-v2-unicode-{major}.{minor}.{patch}")
}

pub(crate) fn load_status(connection: &Connection, database_path: &Path) -> Result<IndexStatus> {
    let value = connection.prepare_cached("SELECT schema_version,codec,normalization,generation,revision,scope,state,entries,files,folders,skipped,links,excluded_cache,started,updated,scope_modified,synthetic FROM manifest WHERE single=1")?.query_row([], |row| {
        Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?,row.get::<_,String>(3)?,row.get::<_,i64>(4)?,
          row.get::<_,Vec<u8>>(5)?,row.get::<_,i64>(6)?,row.get::<_,i64>(7)?,row.get::<_,i64>(8)?,row.get::<_,i64>(9)?,
          row.get::<_,i64>(10)?,row.get::<_,i64>(11)?,row.get::<_,i64>(12)?,row.get::<_,i64>(13)?,row.get::<_,i64>(14)?,row.get::<_,Option<i64>>(15)?,row.get::<_,bool>(16)?))
    })?;
    if value.0 != 2 || value.1 != codec::CODEC || value.2 != normalization() {
        return Err(Error::InvalidIndex(
            "schema, path codec, or normalization version mismatch".into(),
        ));
    }
    let scope = PathBuf::from(codec::decode(&value.5)?);
    if !scope.is_absolute() {
        return Err(Error::InvalidIndex("scope must be absolute".into()));
    }
    let state = BuildState::decode(value.6)?;
    Ok(IndexStatus {
        generation: value.3,
        revision: value.4.max(0) as u64,
        scope,
        database_path: database_path.to_path_buf(),
        state,
        complete: state == BuildState::Complete,
        entries: value.7.max(0) as u64,
        files: value.8.max(0) as u64,
        folders: value.9.max(0) as u64,
        skipped: value.10.max(0) as u64,
        excluded_links: value.11.max(0) as u64,
        excluded_cache: value.12.max(0) as u64,
        started_unix_ms: value.13.max(0) as u64,
        updated_unix_ms: value.14.max(0) as u64,
        source_root_modified_secs: value.15,
        from_cache: false,
        synthetic: value.16,
    })
}

pub(crate) fn write_status(connection: &Connection, status: &IndexStatus) -> Result<()> {
    connection.prepare_cached("UPDATE manifest SET revision=?1,state=?2,entries=?3,files=?4,folders=?5,skipped=?6,links=?7,excluded_cache=?8,updated=?9,synthetic=?10 WHERE single=1")?.execute(
        params![status.revision as i64,status.state.code(),status.entries as i64,status.files as i64,status.folders as i64,
          status.skipped as i64,status.excluded_links as i64,status.excluded_cache as i64,status.updated_unix_ms as i64,status.synthetic])?;
    Ok(())
}

pub(crate) fn write_pointer(cache_dir: &Path, pointer: &str, database_path: &Path) -> Result<()> {
    let name = database_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::InvalidIndex("non-Unicode generation filename".into()))?;
    let temporary = cache_dir.join(format!(
        "{pointer}-{}.tmp",
        NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)
    ));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    writeln!(output, "{name}")?;
    output.sync_all()?;
    drop(output);
    fs::rename(temporary, cache_dir.join(pointer))?;
    Ok(())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as u64
}
pub(crate) fn unix_secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(value) => value.as_secs().min(i64::MAX as u64) as i64,
        Err(error) => {
            -(error
                .duration()
                .as_secs()
                .saturating_add(u64::from(error.duration().subsec_nanos() != 0))
                .min(i64::MAX as u64) as i64)
        }
    }
}
