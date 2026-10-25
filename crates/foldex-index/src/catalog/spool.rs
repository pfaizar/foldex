//! Disposable ordered results for arbitrary scroll positions. Filename storage
//! remains immutable; this bounded SQLite cache is built only for deep seeks.
use super::{Catalog, CatalogQuery, Hit};
use crate::{Cancellation, Error, MatchMode, QuerySpec, Result, SortColumn, model::MAX_PAGE_ROWS};
use rusqlite::{Connection, OpenFlags, params, types::ValueRef};
use std::{
    fs,
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Primary<'a>(ValueRef<'a>);
impl rusqlite::ToSql for Primary<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::Borrowed(self.0))
    }
}

static NEXT: AtomicU64 = AtomicU64::new(1);
pub(super) const CACHE_BYTES: usize = 1536 * 1024;
const FLAT_BYTES: u64 = 64 * 1024 * 1024;

pub(super) struct Spool {
    connection: Option<Connection>,
    path: PathBuf,
    flat_path: PathBuf,
    flat: Option<fs::File>,
    order: String,
    pub hash: u64,
    pub prefix: u64,
    pub total: u64,
}
impl Drop for Spool {
    fn drop(&mut self) {
        drop(self.connection.take());
        drop(self.flat.take());
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.flat_path);
    }
}
impl Spool {
    pub fn create(data: &Catalog, spec: &QuerySpec, hash: u64) -> Result<Self> {
        let path = data.root.join(format!(
            "result-{}-{}.sqlite3",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        drop(
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?,
        );
        let flat_path = path.with_extension("rows");
        let mut spool = Self {
            connection: None,
            path,
            flat_path,
            flat: None,
            order: String::new(),
            hash,
            prefix: 0,
            total: 0,
        };
        spool.connection = Some(Connection::open_with_flags(
            &spool.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?);
        let connection = spool.connection.as_ref().unwrap();
        // This cache is thrown away on interruption/error; no recovery journal.
        connection.execute_batch("PRAGMA page_size=4096; PRAGMA max_page_count=65536; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-1024; PRAGMA cache_spill=ON; PRAGMA temp_store=FILE; PRAGMA temp.cache_size=-512; PRAGMA mmap_size=0; PRAGMA trusted_schema=OFF; PRAGMA threads=0;")?;
        let direction = if spec.descending { "DESC" } else { "ASC" };
        spool.order = format!(
            "folder DESC,rank DESC,p {direction},natural {direction},raw {direction},id {direction}"
        );
        connection.execute_batch("CREATE TABLE ordered(folder INTEGER NOT NULL,rank INTEGER NOT NULL,p NOT NULL,natural BLOB NOT NULL,raw BLOB NOT NULL,id INTEGER PRIMARY KEY)")?;
        Ok(spool)
    }

    /// On failure the caller must discard this entire disposable cache.
    pub fn append(
        &mut self,
        data: &Catalog,
        spec: &QuerySpec,
        prefix: u64,
        cancel: &Cancellation,
    ) -> Result<()> {
        cancel.check()?;
        if prefix == self.prefix {
            return Ok(());
        }
        if prefix < self.prefix || prefix > i64::MAX as u64 {
            return Err(Error::InvalidCursor);
        }
        let connection = self.connection.as_mut().unwrap();
        let token = cancel.clone();
        connection.progress_handler(256, Some(move || token.is_cancelled()))?;
        let transaction = connection.transaction()?;
        let mut insert = transaction.prepare("INSERT INTO ordered VALUES(?1,?2,?3,?4,?5,?6)")?;
        let query = CatalogQuery {
            spec: spec.clone(),
            lower_id: self.prefix,
            upper_id: Some(prefix),
            ..CatalogQuery::default()
        };
        let added = data.visit_matches(&query, cancel, &mut |id, record, score| {
            let size = record.size.to_be_bytes();
            let primary = match spec.sort {
                SortColumn::Name => ValueRef::Integer(0),
                SortColumn::Size => ValueRef::Blob(&size),
                SortColumn::Modified => ValueRef::Integer(record.modified),
                SortColumn::Created => {
                    ValueRef::Integer(record.created.unwrap_or(if spec.descending {
                        i64::MIN
                    } else {
                        i64::MAX
                    }))
                }
                SortColumn::Accessed => {
                    ValueRef::Integer(record.accessed.unwrap_or(if spec.descending {
                        i64::MIN
                    } else {
                        i64::MAX
                    }))
                }
                SortColumn::Kind => {
                    let name = std::str::from_utf8(record.lower()).map_err(|_| {
                        Error::InvalidIndex("catalog lowercase is not UTF-8".into())
                    })?;
                    ValueRef::Text(crate::model::extension(name, record.flags & 1 != 0).as_bytes())
                }
            };
            insert.execute(params![
                (record.flags & 1 != 0) as i64,
                score.unwrap_or(0),
                Primary(primary),
                record.natural(),
                record.name(),
                id as i64
            ])?;
            Ok(())
        })?;
        if self.total.saturating_add(added).saturating_mul(12) > FLAT_BYTES {
            return Err(Error::InvalidIndex("ordered result cache exceeds its disk budget; narrow the query or continue with sequential pages".into()));
        }
        drop(insert);
        cancel.check()?;
        transaction.commit()?;
        drop(self.flat.take());
        if self.flat_path.exists() {
            fs::remove_file(&self.flat_path)?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&self.flat_path)?;
        let mut output = BufWriter::with_capacity(64 * 1024, file);
        let mut select = connection.prepare(&format!(
            "SELECT id,rank FROM ordered ORDER BY {}",
            self.order
        ))?;
        let mut cursor = select.query([])?;
        let mut count = 0;
        while let Some(row) = cursor.next()? {
            if count % 256 == 0 {
                cancel.check()?;
            }
            output.write_all(&row.get::<_, i64>(0)?.to_le_bytes())?;
            output.write_all(&row.get::<_, i32>(1)?.to_le_bytes())?;
            count += 1;
        }
        if count != self.total + added {
            return Err(Error::InvalidIndex(
                "ordered result materialization lost rows".into(),
            ));
        }
        drop(cursor);
        drop(select);
        output.flush()?;
        self.flat = Some(output.into_inner().map_err(|error| error.into_error())?);
        connection.progress_handler(0, None::<fn() -> bool>)?;
        connection.execute_batch("PRAGMA shrink_memory")?;
        self.total = count;
        self.prefix = prefix;
        Ok(())
    }

    pub fn page(
        &mut self,
        data: &Catalog,
        spec: &QuerySpec,
        offset: u64,
        limit: usize,
        cancel: &Cancellation,
    ) -> Result<Vec<Hit>> {
        cancel.check()?;
        let ranked = spec.match_mode == MatchMode::Fuzzy
            && spec.sort == SortColumn::Name
            && !foldex_match::fold_key(&spec.text).is_empty();
        let limit = limit
            .min(MAX_PAGE_ROWS)
            .min(self.total.saturating_sub(offset) as usize);
        let file = self
            .flat
            .as_mut()
            .ok_or_else(|| Error::InvalidIndex("ordered result file is missing".into()))?;
        file.seek(SeekFrom::Start(
            offset.checked_mul(12).ok_or(Error::InvalidCursor)?,
        ))?;
        let mut bytes = [0u8; MAX_PAGE_ROWS * 12];
        file.read_exact(&mut bytes[..limit * 12])?;
        let mut hits = Vec::with_capacity(limit);
        for row in bytes[..limit * 12].chunks_exact(12) {
            let id = u64::from_le_bytes(row[..8].try_into().unwrap());
            let score = ranked.then(|| i32::from_le_bytes(row[8..].try_into().unwrap()));
            let shard = data
                .shards
                .partition_point(|shard| shard.first_id <= id)
                .checked_sub(1)
                .ok_or(Error::InvalidCursor)?;
            let ordinal = usize::try_from(id - data.shards[shard].first_id)
                .map_err(|_| Error::InvalidCursor)?;
            if ordinal >= data.shards[shard].count {
                return Err(Error::InvalidCursor);
            }
            hits.push(Hit::location(id, shard, ordinal, score, spec));
        }
        cancel.check()?;
        Ok(hits)
    }

    pub fn cache_used(&self) -> Result<usize> {
        let (mut used, mut highwater) = (0, 0);
        // SAFETY: this live owning connection retains no pointers from the call.
        let result = unsafe {
            rusqlite::ffi::sqlite3_db_status(
                self.connection.as_ref().unwrap().handle(),
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
        Ok(used.max(0) as usize)
    }
    pub fn bytes(&self) -> u64 {
        fs::metadata(&self.path).map_or(0, |metadata| metadata.len())
            + fs::metadata(&self.flat_path).map_or(0, |metadata| metadata.len())
    }
}
