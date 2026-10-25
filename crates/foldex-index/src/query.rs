use crate::{
    codec,
    model::*,
    store::{Index, SELECT_ROW, load_status},
};
use rusqlite::{params_from_iter, types::Value};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::PathBuf,
    time::Instant,
};

struct Predicate {
    from: &'static str,
    sql: String,
    ranked_sql: Option<String>,
    values: Vec<Value>,
    postings: Option<&'static str>,
}

const CANDIDATE_PLAN_BYTES: usize = 16 * 1024;

pub(crate) struct CandidatePlan {
    prefix: u64,
    table: &'static str,
    expression: String,
    selected: String,
    dense: bool,
}

impl Index {
    #[cfg(feature = "diagnostics")]
    pub fn benchmark_set_query_cache_enabled(&mut self, enabled: bool) {
        self.query_cache_enabled = enabled;
        self.count_cache = None;
        self.candidate_plan = None;
        *self.rank_cache.borrow_mut() = crate::rank_cache::RankCache::default();
    }

    fn query_caches_enabled(&self) -> bool {
        #[cfg(feature = "diagnostics")]
        {
            self.query_cache_enabled
        }
        #[cfg(not(feature = "diagnostics"))]
        {
            true
        }
    }

    /// Opt-in benchmark oracle: compare raw-name verification with the prior
    /// persisted folded-name predicate over the complete retained dataset.
    #[cfg(feature = "diagnostics")]
    pub fn benchmark_membership_disagreements(&self, text: &str) -> Result<u64> {
        let folded = foldex_match::fold_key(text);
        let count:i64=self.connection.query_row("SELECT COUNT(*) FROM entries e WHERE foldex_subsequence_raw(e.raw_sort,?1) != foldex_subsequence(COALESCE(e.source_key,e.lower_name),?1)",[folded],|row|row.get(0))?;
        Ok(count.max(0) as u64)
    }
    pub fn query(
        &mut self,
        spec: &QuerySpec,
        offset: u64,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run_query(spec, offset, None, page_size, cancel, None)
    }
    pub fn query_after(
        &mut self,
        spec: &QuerySpec,
        cursor: Option<&PageCursor>,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run_query(
            spec,
            cursor.map_or(0, |value| value.offset),
            cursor,
            page_size,
            cancel,
            None,
        )
    }

    /// Reads the exact append-only prefix represented by a previously published generation.
    /// Later rows cannot change global positions within a pinned selection/result set.
    pub fn query_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        offset: u64,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run_query(spec, offset, None, page_size, cancel, Some(boundary))
    }

    pub fn query_after_as_of(
        &mut self,
        spec: &QuerySpec,
        boundary: &IndexStatus,
        cursor: Option<&PageCursor>,
        page_size: usize,
        cancel: &Cancellation,
    ) -> Result<QueryPage> {
        self.run_query(
            spec,
            cursor.map_or(0, |value| value.offset),
            cursor,
            page_size,
            cancel,
            Some(boundary),
        )
    }
    fn run_query(
        &mut self,
        spec: &QuerySpec,
        offset: u64,
        cursor: Option<&PageCursor>,
        page_size: usize,
        cancel: &Cancellation,
        boundary: Option<&IndexStatus>,
    ) -> Result<QueryPage> {
        cancel.check()?;
        spec.conditions.validate().map_err(Error::InvalidPath)?;
        let conditions = spec.conditions.clone();
        self.connection.create_scalar_function(
            "foldex_metadata",
            4,
            rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC
                | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS,
            move |ctx| {
                let raw = ctx.get_raw(0).as_blob()?;
                let path = PathBuf::from(
                    codec::decode(raw)
                        .map_err(|e| rusqlite::Error::UserFunctionError(Box::new(e)))?,
                );
                let bytes = ctx.get_raw(2).as_blob()?;
                let bytes: [u8; 8] = bytes
                    .try_into()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                Ok(conditions.matches(
                    &path,
                    ctx.get::<bool>(1)?,
                    u64::from_be_bytes(bytes),
                    ctx.get(3)?,
                ))
            },
        )?;
        let started = Instant::now();
        #[cfg(feature = "diagnostics")]
        {
            *self.query_diagnostics.borrow_mut() = QueryDiagnostics::default();
            crate::fuzzy::reset_diagnostics(&self.connection)?;
        }
        let token = cancel.clone();
        self.connection
            .progress_handler(1000, Some(move || token.is_cancelled()))?;
        let result = (|| {
            self.ensure_sort(spec.sort)?;
            self.connection.execute_batch("BEGIN DEFERRED")?;
            let from_cache = self.status.from_cache;
            self.status = load_status(&self.connection, &self.status.database_path)?;
            self.status.from_cache = from_cache;
            if let Some(boundary) = boundary {
                if boundary.excluded_subtrees != self.status.excluded_subtrees
                    || boundary.directory_only != self.status.directory_only
                    || boundary.generation != self.status.generation
                    || boundary.scope != self.status.scope
                    || boundary.entries > self.status.entries
                    || boundary.revision > self.status.revision
                    || boundary.entries > i64::MAX as u64
                {
                    return Err(Error::InvalidCursor);
                }
            }
            let source_status = boundary.unwrap_or(&self.status).clone();
            let upper_id = boundary.map(|status| status.entries);
            let query_hash = fingerprint(spec, upper_id);
            if let Some(cursor) = cursor {
                if cursor.generation != source_status.generation
                    || cursor.revision != source_status.revision
                    || cursor.query_hash != query_hash
                {
                    return Err(Error::InvalidCursor);
                }
            }
            let mut predicate = predicate(spec, upper_id)?;
            // Dense trigram hits are cheaper to serve in persisted B-tree order: sorting a
            // million matching postings just to paint 256 rows would waste foreground latency.
            // Probe only a bounded number of postings, then verify the same literal predicate.
            let ordered = if self.plan_candidates(&mut predicate)? {
                Some(scan_predicate(spec, upper_id)?)
            } else {
                None
            };
            let page_predicate = ordered.as_ref().unwrap_or(&predicate);
            let count = if spec.include_total || (offset > 0 && cursor.is_none()) {
                if spec.text.is_empty()
                    && spec.type_filter.is_none()
                    && spec.conditions.is_empty()
                    && spec.show_hidden
                    && upper_id.is_none_or(|bound| bound == self.status.entries)
                {
                    let folders = if !spec.kind.includes(true) {
                        0
                    } else {
                        self.status.folders
                    };
                    let files = if !spec.kind.includes(false) {
                        0
                    } else {
                        self.status.files
                    };
                    Some((folders, files))
                } else {
                    Some(self.counts(&predicate, fingerprint(spec, None), source_status.entries)?)
                }
            } else {
                None
            };
            let limit = page_size.min(MAX_PAGE_ROWS);
            let mut rows = Vec::with_capacity(limit);
            let mut bytes = 0usize;
            let mut has_more = false;
            if limit > 0 {
                for is_dir in [true, false] {
                    if !spec.kind.includes(is_dir) {
                        continue;
                    }
                    if cursor.is_some_and(|value| !value.last.is_dir) && is_dir {
                        continue;
                    }
                    let local_offset = if cursor.is_some() {
                        0
                    } else if let Some((folders, _)) = count {
                        if is_dir {
                            offset
                        } else {
                            offset.saturating_sub(folders)
                        }
                    } else {
                        0
                    };
                    if let Some((folders, files)) = count {
                        let group_count = if is_dir { folders } else { files };
                        if local_offset >= group_count {
                            continue;
                        }
                    }
                    let after = cursor.filter(|value| value.last.is_dir == is_dir);
                    let (group, more) = self.group(
                        spec,
                        page_predicate,
                        is_dir,
                        local_offset,
                        after,
                        limit.saturating_sub(rows.len()),
                        MAX_PAGE_BYTES.saturating_sub(bytes),
                        rows.is_empty(),
                        source_status.entries,
                        cancel,
                    )?;
                    bytes += group.iter().map(IndexRow::bytes).sum::<usize>();
                    rows.extend(group);
                    if more {
                        has_more = true;
                        break;
                    }
                    if rows.len() >= limit {
                        // A following group can exist even if the current group ended exactly at the page boundary.
                        has_more = if is_dir && spec.kind != KindFilter::Folders {
                            match count {
                                Some((_, files)) => files > 0,
                                None => self.group_exists(page_predicate, false)?,
                            }
                        } else {
                            false
                        };
                        break;
                    }
                }
            }
            let next_offset = offset.saturating_add(rows.len() as u64);
            let total = count.map(|(folders, files)| folders.saturating_add(files));
            if let Some(total) = total {
                has_more = next_offset < total;
            }
            let next = if has_more {
                rows.last().cloned().map(|last| PageCursor {
                    generation: source_status.generation.clone(),
                    revision: source_status.revision,
                    query_hash,
                    last,
                    offset: next_offset,
                })
            } else {
                None
            };
            cancel.check()?;
            Ok(QueryPage {
                rows,
                total,
                kind_counts: count,
                offset,
                next_offset,
                has_more,
                next,
                status: source_status,
                elapsed: started.elapsed(),
                used_trigram: predicate.postings == Some("names_fts"),
                used_character_index: predicate.postings == Some("characters_fts"),
            })
        })();
        // Reset the interrupt handler before cleanup; a cancelled token must not interrupt cleanup SQL.
        let reset = self.connection.progress_handler(0, None::<fn() -> bool>);
        let rollback = self.connection.execute_batch("ROLLBACK");
        if result.is_ok() {
            reset?;
            rollback?;
        }
        #[cfg(feature = "diagnostics")]
        crate::fuzzy::read_diagnostics(&self.connection, &mut self.query_diagnostics.borrow_mut())?;
        result
    }

    fn ensure_sort(&self, sort: SortColumn) -> Result<()> {
        let sql = match sort {
            SortColumn::Name | SortColumn::Created | SortColumn::Accessed => return Ok(()),
            SortColumn::Size => {
                "CREATE INDEX IF NOT EXISTS entries_size ON entries(is_dir,size_key,natural_key,raw_sort,id)"
            }
            SortColumn::Modified => {
                "CREATE INDEX IF NOT EXISTS entries_modified ON entries(is_dir,modified,natural_key,raw_sort,id)"
            }
            SortColumn::Kind => {
                "CREATE INDEX IF NOT EXISTS entries_kind ON entries(is_dir,extension,natural_key,raw_sort,id)"
            }
        };
        self.connection.execute_batch(sql)?;
        Ok(())
    }

    fn counts(
        &mut self,
        predicate: &Predicate,
        fingerprint: u64,
        prefix: u64,
    ) -> Result<(u64, u64)> {
        let previous = self.count_cache.filter(|(hash, previous, _, _)| {
            self.query_caches_enabled() && *hash == fingerprint && *previous <= prefix
        });
        if let Some((_, previous, folders, files)) = previous {
            if previous == prefix {
                return Ok((folders, files));
            }
        }
        let lower = previous.map(|(_, previous, _, _)| previous);
        let mut values = predicate.values.clone();
        let (from, conditions) = range_after(predicate, &predicate.sql, lower, &mut values);
        let sql = format!(
            "SELECT COALESCE(SUM(e.is_dir),0),COUNT(*) FROM {} WHERE {}",
            from, conditions
        );
        let (folders, total): (i64, i64) =
            self.connection
                .query_row(&sql, params_from_iter(values.iter()), |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
        let added_folders = folders.max(0) as u64;
        let added_files = (total.max(0) as u64).saturating_sub(added_folders);
        let folders = previous.map_or(0, |(_, _, folders, _)| folders) + added_folders;
        let files = previous.map_or(0, |(_, _, _, files)| files) + added_files;
        self.count_cache = Some((fingerprint, prefix, folders, files));
        Ok((folders, files))
    }

    fn dense_candidates(&self, predicate: &Predicate) -> Result<bool> {
        let Some(Value::Text(expression)) = predicate.values.last() else {
            return Ok(false);
        };
        let Some(table) = predicate.postings else {
            return Ok(false);
        };
        #[cfg(feature = "diagnostics")]
        {
            self.query_diagnostics.borrow_mut().candidate_probes += 1;
        }
        let candidates: i64 = self
            .connection
            .prepare_cached(&format!(
                "SELECT COUNT(*) FROM (SELECT rowid FROM {table} WHERE {table} MATCH ?1 LIMIT 4097)"
            ))?
            .query_row([expression], |row| row.get(0))?;
        Ok(candidates > 4096)
    }

    fn plan_candidates(&mut self, predicate: &mut Predicate) -> Result<bool> {
        let Some(table) = predicate.postings else {
            return Ok(false);
        };
        let Some(Value::Text(expression)) = predicate.values.last() else {
            return Ok(false);
        };
        if self.query_caches_enabled() {
            if let Some(plan) = self.candidate_plan.as_ref().filter(|plan| {
                plan.prefix == self.status.entries
                    && plan.table == table
                    && plan.expression == *expression
            }) {
                *predicate.values.last_mut().expect("posting parameter") =
                    Value::Text(plan.selected.clone());
                #[cfg(feature = "diagnostics")]
                {
                    self.query_diagnostics.borrow_mut().candidate_cache_hits += 1;
                }
                return Ok(plan.dense);
            }
        }
        let expression = expression.clone();
        self.selective_terms(predicate)?;
        let dense = self.dense_candidates(predicate)?;
        if self.query_caches_enabled() {
            let Some(Value::Text(selected)) = predicate.values.last() else {
                unreachable!("posting parameter");
            };
            // A single bounded planning hint; long queries still use the identical
            // uncached path. Filtering and exact verification never depend on admission.
            self.candidate_plan =
                (expression.len() + selected.len() <= CANDIDATE_PLAN_BYTES).then(|| {
                    CandidatePlan {
                        prefix: self.status.entries,
                        table,
                        expression,
                        selected: selected.clone(),
                        dense,
                    }
                });
        }
        Ok(dense)
    }

    fn selective_terms(&self, predicate: &mut Predicate) -> Result<()> {
        let Some(table) = predicate.postings else {
            return Ok(());
        };
        let Some(Value::Text(expression)) = predicate.values.last() else {
            return Ok(());
        };
        let mut lookup = self.connection.prepare_cached(&format!(
            "SELECT COUNT(*),COALESCE(MAX(rowid),0) FROM (SELECT rowid FROM {table} WHERE {table} MATCH ?1 LIMIT 4097)"
        ))?;
        let mut weighted = Vec::new();
        for token in expression.split(" AND ") {
            #[cfg(feature = "diagnostics")]
            {
                self.query_diagnostics.borrow_mut().candidate_probes += 1;
            }
            // Exact FTS5vocab document frequencies walk whole posting lists. A capped probe
            // distinguishes selective terms without making common queries scan the scope.
            let (count, last): (i64, i64) =
                lookup.query_row([token], |row| Ok((row.get(0)?, row.get(1)?)))?;
            let (count, last) = (count.max(0) as u64, last.max(0) as u64);
            // Once capped, use sampled row-ID density to break common-term ties. This is
            // deliberately an estimate: exact membership verification remains mandatory.
            let frequency = if count < 4097 {
                count
            } else {
                ((count as u128 * self.status.entries.max(last) as u128) / last.max(1) as u128)
                    .min(u64::MAX as u128) as u64
            };
            weighted.push((frequency.max(count), token));
        }
        weighted.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        let count = if table == "names_fts" { 2 } else { 3 };
        let selected = weighted
            .iter()
            .take(count)
            .map(|(_, token)| *token)
            .collect::<Vec<_>>()
            .join(" AND ");
        *predicate.values.last_mut().expect("posting parameter") = Value::Text(selected);
        Ok(())
    }

    fn group_exists(&self, predicate: &Predicate, is_dir: bool) -> Result<bool> {
        let mut values = predicate.values.clone();
        values.push(Value::Integer(i64::from(is_dir)));
        let sql = format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE {} AND e.is_dir=?{} LIMIT 1)",
            predicate.from,
            predicate.sql,
            values.len()
        );
        Ok(self
            .connection
            .query_row(&sql, params_from_iter(values.iter()), |row| row.get(0))?)
    }

    #[allow(clippy::too_many_arguments)]
    fn group(
        &self,
        spec: &QuerySpec,
        predicate: &Predicate,
        is_dir: bool,
        offset: u64,
        cursor: Option<&PageCursor>,
        remaining: usize,
        byte_budget: usize,
        allow_first: bool,
        prefix: u64,
        cancel: &Cancellation,
    ) -> Result<(Vec<IndexRow>, bool)> {
        if offset > i64::MAX as u64 {
            return Ok((Vec::new(), false));
        }
        let columns = sort_columns(spec.sort);
        let ranked = spec.match_mode == MatchMode::Fuzzy
            && !spec.text.is_empty()
            && spec.sort == SortColumn::Name;
        if ranked && self.query_caches_enabled() && offset < crate::rank_cache::KEY_LIMIT as u64 {
            if let Some(page) = self.cached_ranked_group(
                spec,
                predicate,
                is_dir,
                offset,
                cursor,
                remaining,
                byte_budget,
                allow_first,
                prefix,
                cancel,
            )? {
                return Ok(page);
            }
        }
        let mut values = predicate.values.clone();
        values.push(Value::Integer(i64::from(is_dir)));
        let base = if ranked {
            predicate.ranked_sql.as_deref().unwrap_or(&predicate.sql)
        } else {
            &predicate.sql
        };
        let mut conditions = format!("{base} AND e.is_dir=?{}", values.len());
        if let Some(cursor) = cursor {
            let score_parameter = if ranked {
                values.push(Value::Integer(
                    cursor.last.fuzzy_score.ok_or(Error::InvalidCursor)? as i64,
                ));
                Some(values.len())
            } else {
                None
            };
            let keys = cursor_values(&cursor.last, spec.sort);
            let first = values.len() + 1;
            let placeholders = (first..first + keys.len())
                .map(|value| format!("?{value}"))
                .collect::<Vec<_>>()
                .join(",");
            values.extend(keys);
            let comparison = format!(
                "({columns}) {} ({placeholders})",
                if spec.descending { "<" } else { ">" }
            );
            if let Some(score_parameter) = score_parameter {
                conditions.push_str(&format!(" AND (foldex_fuzzy_score(e.raw_sort,?1) < ?{score_parameter} OR (foldex_fuzzy_score(e.raw_sort,?1)=?{score_parameter} AND {comparison}))"));
            } else {
                conditions.push_str(&format!(" AND {comparison}"));
            }
        }
        let direction = if spec.descending { "DESC" } else { "ASC" };
        let mut order = columns
            .split(',')
            .map(|column| format!("{column} {direction}"))
            .collect::<Vec<_>>()
            .join(",");
        let selected = if ranked {
            order = format!("fuzzy_rank DESC,{order}");
            "e.id,foldex_fuzzy_score(e.raw_sort,?1) AS fuzzy_rank"
        } else {
            "e.id"
        };
        values.push(Value::Integer((remaining + 1).min(MAX_PAGE_ROWS + 1) as i64));
        let limit_param = values.len();
        values.push(Value::Integer(offset as i64));
        let sql = format!(
            "SELECT {selected} FROM {} WHERE {conditions} ORDER BY {order} LIMIT ?{limit_param} OFFSET ?{}",
            predicate.from,
            values.len()
        );
        #[cfg(feature = "diagnostics")]
        let plan = {
            let mut explain = self
                .connection
                .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
            explain
                .query_map(params_from_iter(values.iter()), |row| {
                    row.get::<_, String>(3)
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        let mut statement = self.connection.prepare(&sql)?;
        let mut source = statement.query(params_from_iter(values.iter()))?;
        // Keep any SQLite top-N sorter independent of full path lengths; fetch payloads only
        // after selecting IDs. The Rust result-byte cap is applied before another row is retained.
        let mut payload = self.connection.prepare_cached(SELECT_ROW)?;
        let mut output = Vec::with_capacity(remaining);
        let mut used = 0usize;
        #[cfg(feature = "diagnostics")]
        let mut payload_elapsed = std::time::Duration::ZERO;
        let result = (|| {
            while let Some(row) = source.next()? {
                cancel.check()?;
                if output.len() >= remaining {
                    return Ok((output, true));
                }
                let id: i64 = row.get(0)?;
                #[cfg(feature = "diagnostics")]
                let payload_started = Instant::now();
                let mut selected = payload.query([id])?;
                let mut decoded = decode_row(
                    selected
                        .next()?
                        .ok_or_else(|| Error::InvalidIndex("selected row disappeared".into()))?,
                    &self.status.scope,
                )?;
                if ranked {
                    decoded.fuzzy_score = row.get(1)?;
                }
                #[cfg(feature = "diagnostics")]
                {
                    payload_elapsed += payload_started.elapsed();
                }
                let bytes = decoded.bytes();
                if (!output.is_empty() || !allow_first) && used.saturating_add(bytes) > byte_budget
                {
                    return Ok((output, true));
                }
                if bytes > MAX_PAGE_BYTES {
                    return Err(Error::InvalidIndex(
                        "one indexed row exceeds the result-page byte budget".into(),
                    ));
                }
                used += bytes;
                output.push(decoded);
            }
            Ok((output, false))
        })();
        #[cfg(feature = "diagnostics")]
        {
            drop(source);
            use rusqlite::StatementStatus;
            self.query_diagnostics
                .borrow_mut()
                .groups
                .push(GroupDiagnostics {
                    is_dir,
                    sql,
                    plan,
                    elapsed: started.elapsed(),
                    payload_elapsed,
                    vm_steps: statement.get_status(StatementStatus::VmStep).max(0) as u64,
                    sorts: statement.get_status(StatementStatus::Sort).max(0) as u64,
                    full_scan_steps: statement.get_status(StatementStatus::FullscanStep).max(0)
                        as u64,
                    rows: result.as_ref().map_or(0, |(rows, _)| rows.len()),
                });
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn cached_ranked_group(
        &self,
        spec: &QuerySpec,
        predicate: &Predicate,
        is_dir: bool,
        offset: u64,
        cursor: Option<&PageCursor>,
        remaining: usize,
        byte_budget: usize,
        allow_first: bool,
        prefix: u64,
        cancel: &Cancellation,
    ) -> Result<Option<(Vec<IndexRow>, bool)>> {
        use crate::rank_cache::{GROUP_BYTES, Group, KEY_LIMIT, Key};
        let mut cache = self.rank_cache.borrow_mut();
        cache.select(&self.status.generation, fingerprint(spec, None));
        let slot = &mut cache.groups[usize::from(is_dir)];
        #[cfg(feature = "diagnostics")]
        let started = Instant::now();
        #[cfg(feature = "diagnostics")]
        let mut diagnostic = GroupDiagnostics {
            is_dir,
            sql: "rank_cache".into(),
            plan: Vec::new(),
            elapsed: Default::default(),
            payload_elapsed: Default::default(),
            vm_steps: 0,
            sorts: 0,
            full_scan_steps: 0,
            rows: 0,
        };
        if !slot.as_ref().is_some_and(|group| group.prefix == prefix) {
            let previous = slot.take().filter(|group| group.prefix < prefix);
            let lower = previous.as_ref().map(|group| group.prefix);
            let mut values = predicate.values.clone();
            let base = predicate.ranked_sql.as_deref().unwrap_or(&predicate.sql);
            let (from, mut conditions) = range_after(predicate, base, lower, &mut values);
            values.push(Value::Integer(i64::from(is_dir)));
            conditions.push_str(&format!(" AND e.is_dir=?{}", values.len()));
            let direction = if spec.descending { "DESC" } else { "ASC" };
            let sql = format!(
                "SELECT e.id,foldex_fuzzy_score(e.raw_sort,?1) AS fuzzy_rank,e.natural_key,e.raw_sort FROM {from} WHERE {conditions} ORDER BY fuzzy_rank DESC,e.natural_key {direction},e.raw_sort {direction},e.id {direction} LIMIT {}",
                KEY_LIMIT + 1
            );
            #[cfg(feature = "diagnostics")]
            {
                diagnostic.sql = sql.clone();
                diagnostic.plan = self
                    .connection
                    .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
                    .query_map(params_from_iter(values.iter()), |row| {
                        row.get::<_, String>(3)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
            }
            let mut statement = self.connection.prepare(&sql)?;
            let mut source = statement.query(params_from_iter(values.iter()))?;
            let mut keys = Vec::with_capacity(KEY_LIMIT);
            let mut bytes = KEY_LIMIT * std::mem::size_of::<Key>();
            let mut complete = true;
            while let Some(row) = source.next()? {
                cancel.check()?;
                if keys.len() >= KEY_LIMIT {
                    complete = false;
                    break;
                }
                let key = Key {
                    id: row.get::<_, i64>(0)?.max(0) as u64,
                    score: row.get(1)?,
                    natural: row.get(2)?,
                    raw: row.get(3)?,
                };
                let payload = key.bytes() - std::mem::size_of::<Key>();
                if bytes.saturating_add(payload) > GROUP_BYTES {
                    complete = false;
                    break;
                }
                bytes += payload;
                keys.push(key);
            }
            drop(source);
            #[cfg(feature = "diagnostics")]
            {
                use rusqlite::StatementStatus;
                diagnostic.vm_steps = statement.get_status(StatementStatus::VmStep).max(0) as u64;
                diagnostic.sorts = statement.get_status(StatementStatus::Sort).max(0) as u64;
                diagnostic.full_scan_steps =
                    statement.get_status(StatementStatus::FullscanStep).max(0) as u64;
            }
            let newer = Group {
                prefix,
                keys,
                complete,
            };
            *slot = Some(match previous {
                Some(previous) => previous.merge(newer, spec.descending),
                None => newer,
            });
        }
        let group = slot.as_ref().expect("populated result prefix");
        let start = if let Some(cursor) = cursor {
            match group
                .keys
                .binary_search_by(|key| key.compare_row(&cursor.last, spec.descending))
            {
                Ok(position) => position + 1,
                Err(_) => return Ok(None),
            }
        } else {
            offset as usize
        };
        if !group.complete && group.keys.len().saturating_sub(start) < remaining {
            return Ok(None);
        }
        let mut payload = self.connection.prepare_cached(SELECT_ROW)?;
        let mut output = Vec::with_capacity(remaining);
        let mut used = 0usize;
        let mut position = start;
        for key in group.keys.iter().skip(start).take(remaining) {
            cancel.check()?;
            #[cfg(feature = "diagnostics")]
            let payload_started = Instant::now();
            let mut selected = payload.query([key.id as i64])?;
            let mut decoded = decode_row(
                selected
                    .next()?
                    .ok_or_else(|| Error::InvalidIndex("cached result row disappeared".into()))?,
                &self.status.scope,
            )?;
            decoded.fuzzy_score = key.score;
            #[cfg(feature = "diagnostics")]
            {
                diagnostic.payload_elapsed += payload_started.elapsed();
            }
            let bytes = decoded.bytes();
            if (!output.is_empty() || !allow_first) && used.saturating_add(bytes) > byte_budget {
                break;
            }
            if bytes > MAX_PAGE_BYTES {
                return Err(Error::InvalidIndex(
                    "one indexed row exceeds the result-page byte budget".into(),
                ));
            }
            used += bytes;
            position += 1;
            output.push(decoded);
        }
        let more = position < group.keys.len() || !group.complete;
        #[cfg(feature = "diagnostics")]
        {
            diagnostic.elapsed = started.elapsed();
            diagnostic.rows = output.len();
            self.query_diagnostics.borrow_mut().groups.push(diagnostic);
        }
        Ok(Some((output, more)))
    }
}

fn range_after<'a>(
    predicate: &'a Predicate,
    base: &str,
    lower: Option<u64>,
    values: &mut Vec<Value>,
) -> (&'a str, String) {
    let Some(lower) = lower else {
        return (predicate.from, base.to_owned());
    };
    values.push(Value::Integer(lower as i64));
    let parameter = values.len();
    let mut conditions = format!("{base} AND e.id>?{parameter}");
    if let Some(table) = predicate.postings {
        conditions.push_str(&format!(" AND {table}.rowid>?{parameter}"));
        (predicate.from, conditions)
    } else {
        // Incremental updates must visit new primary-key rows, rather than
        // walking every old name-index entry again to reject its row ID.
        ("entries e NOT INDEXED", conditions)
    }
}

fn predicate(spec: &QuerySpec, upper_id: Option<u64>) -> Result<Predicate> {
    make_predicate(spec, true, upper_id)
}

fn scan_predicate(spec: &QuerySpec, upper_id: Option<u64>) -> Result<Predicate> {
    make_predicate(spec, false, upper_id)
}

fn make_predicate(spec: &QuerySpec, use_fts: bool, upper_id: Option<u64>) -> Result<Predicate> {
    if spec.type_filter.as_ref().is_some_and(|filter| {
        filter.extensions.len() > 256 || filter.extensions.iter().any(|value| value.len() > 1024)
    }) {
        return Err(Error::InvalidPath(
            "type filter exceeds its bounded extension list".into(),
        ));
    }
    if spec.text.len() > 128 * 1024 {
        return Err(Error::InvalidPath(
            "query exceeds operating-system filename length".into(),
        ));
    }
    let needle = if spec.match_mode == MatchMode::Fuzzy {
        foldex_match::fold_key(&spec.text)
    } else {
        spec.text.to_lowercase()
    };
    let expression = if !use_fts || needle.is_empty() {
        None
    } else {
        match spec.match_mode {
            MatchMode::Fuzzy => Some(crate::fuzzy::tokens(&needle, true)),
            MatchMode::Substring => trigram_expression(&needle),
        }
    };
    // Ranked predicates and score scalars share parameter 1. Reserve it before
    // adding extension parameters so every fuzzy path uses the same query text.
    let has_needle = !needle.is_empty();
    let mut values = if has_needle {
        vec![Value::Text(needle)]
    } else {
        Vec::new()
    };
    let mut conditions = vec!["1=1".to_string()];
    if !spec.conditions.is_empty() {
        conditions.push("foldex_metadata(e.relative,e.is_dir,e.size_key,e.modified)".into());
    }
    let mut fuzzy_verification = None;
    if !spec.show_hidden {
        conditions.push("e.hidden=0".into());
    }
    match spec.kind {
        KindFilter::Files => conditions.push("e.is_dir=0".into()),
        KindFilter::Folders => conditions.push("e.is_dir=1".into()),
        KindFilter::None => conditions.push("0".into()),
        KindFilter::All => {}
    }
    if let Some(filter) = &spec.type_filter {
        let mut types = Vec::new();
        if filter.include_folders {
            types.push("e.is_dir=1".to_string());
        }
        if filter.exclude && filter.extensions.is_empty() {
            types.push("e.is_dir=0".to_string());
        } else if !filter.extensions.is_empty() {
            let parameters = filter
                .extensions
                .iter()
                .map(|extension| {
                    values.push(Value::Text(extension.clone()));
                    format!("?{}", values.len())
                })
                .collect::<Vec<_>>()
                .join(",");
            let operator = if filter.exclude { "NOT IN" } else { "IN" };
            types.push(format!(
                "(e.is_dir=0 AND e.extension {operator} ({parameters}))"
            ));
        }
        conditions.push(if types.is_empty() {
            "0".into()
        } else {
            format!("({})", types.join(" OR "))
        });
    }
    if has_needle {
        if spec.match_mode == MatchMode::Fuzzy {
            fuzzy_verification = Some(conditions.len());
        }
        conditions.push(match spec.match_mode {
            MatchMode::Substring => "instr(e.lower_name,?1)>0".into(),
            MatchMode::Fuzzy => "foldex_subsequence(COALESCE(e.source_key,e.lower_name),?1)".into(),
        });
    }
    if let Some(upper_id) = upper_id {
        values.push(Value::Integer(upper_id as i64));
        conditions.push(format!("e.id<=?{}", values.len()));
    }
    let postings = expression.as_ref().map(|_| {
        if spec.match_mode == MatchMode::Fuzzy {
            "characters_fts"
        } else {
            "names_fts"
        }
    });
    if let Some(expression) = expression {
        values.push(Value::Text(expression));
        conditions.push(format!(
            "{} MATCH ?{}",
            postings.expect("posting expression"),
            values.len()
        ));
    }
    let sql = conditions.join(" AND ");
    let ranked_sql = fuzzy_verification.map(|at| {
        // Exact membership can read the original name already in entries_name.
        // This permits a covering scan instead of fetching every full path/
        // metadata row merely to read the separately folded name.
        conditions[at] = "foldex_subsequence_raw(e.raw_sort,?1)".into();
        conditions.join(" AND ")
    });
    Ok(Predicate {
        from: match postings {
            Some("names_fts") => "entries e JOIN names_fts ON names_fts.rowid=e.id",
            Some(_) => "entries e JOIN characters_fts ON characters_fts.rowid=e.id",
            None => "entries e",
        },
        sql,
        ranked_sql,
        values,
        postings,
    })
}

fn trigram_expression(needle: &str) -> Option<String> {
    if needle.contains('\0') {
        return None;
    }
    let chars: Vec<_> = needle.chars().collect();
    if chars.len() < 3 {
        return None;
    }
    let last = chars.len() - 3;
    let mut grams = Vec::<String>::new();
    for position in [0, last / 4, last / 2, last * 3 / 4, last] {
        let gram: String = chars[position..position + 3].iter().collect();
        if !grams.contains(&gram) {
            grams.push(gram);
        }
    }
    Some(
        grams
            .into_iter()
            .map(|gram| format!("\"{}\"", gram.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

pub(crate) fn fingerprint(spec: &QuerySpec, upper_id: Option<u64>) -> u64 {
    let mut hasher = DefaultHasher::new();
    match spec.match_mode {
        MatchMode::Fuzzy => foldex_match::fold_key(&spec.text).hash(&mut hasher),
        MatchMode::Substring => spec.text.to_lowercase().hash(&mut hasher),
    }
    spec.match_mode.hash(&mut hasher);
    spec.sort.hash(&mut hasher);
    spec.descending.hash(&mut hasher);
    spec.show_hidden.hash(&mut hasher);
    spec.kind.hash(&mut hasher);
    spec.type_filter.hash(&mut hasher);
    spec.conditions.hash(&mut hasher);
    upper_id.hash(&mut hasher);
    hasher.finish()
}

fn sort_columns(column: SortColumn) -> &'static str {
    match column {
        SortColumn::Name | SortColumn::Created | SortColumn::Accessed => {
            "e.natural_key,e.raw_sort,e.id"
        }
        SortColumn::Size => "e.size_key,e.natural_key,e.raw_sort,e.id",
        SortColumn::Modified => "e.modified,e.natural_key,e.raw_sort,e.id",
        SortColumn::Kind => "e.extension,e.natural_key,e.raw_sort,e.id",
    }
}
fn cursor_values(row: &IndexRow, column: SortColumn) -> Vec<Value> {
    let mut values = Vec::with_capacity(4);
    match column {
        SortColumn::Name | SortColumn::Created | SortColumn::Accessed => {}
        SortColumn::Size => values.push(Value::Blob(row.size.to_be_bytes().to_vec())),
        SortColumn::Modified => values.push(Value::Integer(row.modified_secs)),
        SortColumn::Kind => values.push(Value::Text(row.extension.clone())),
    }
    values.extend([
        Value::Blob(row.natural_key.clone()),
        Value::Blob(row.raw_sort.clone()),
        Value::Integer(row.id as i64),
    ]);
    values
}

fn decode_row(row: &rusqlite::Row<'_>, scope: &std::path::Path) -> Result<IndexRow> {
    let relative = row.get_ref(1)?.as_blob().map_err(rusqlite::Error::from)?;
    let relative_path = codec::relative(&PathBuf::from(codec::decode(relative)?), false)?;
    let raw_name = relative_path
        .file_name()
        .ok_or_else(|| Error::InvalidIndex("missing original basename".into()))?
        .to_os_string();
    let name = raw_name.to_string_lossy().into_owned();
    let size: [u8; 8] = row.get(5)?;
    Ok(IndexRow {
        availability: foldex_fs::capabilities::Availability::from_code(row.get(10)?)
            .ok_or_else(|| Error::InvalidIndex("invalid availability".into()))?,
        id: row.get::<_, i64>(0)?.max(0) as u64,
        path: scope.join(&relative_path),
        relative_path,
        raw_name,
        name,
        is_dir: row.get(2)?,
        hidden: row.get(3)?,
        symlink: row.get(4)?,
        size: u64::from_be_bytes(size),
        modified_secs: row.get(6)?,
        created_secs: None,
        accessed_secs: None,
        natural_key: row.get(7)?,
        raw_sort: row.get(8)?,
        extension: row.get(9)?,
        fuzzy_score: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trigram_queries_quote_literal_punctuation_and_short_queries_fall_back() {
        assert!(trigram_expression("東京").is_none());
        assert!(trigram_expression("ab").is_none());
        assert!(trigram_expression("a\0b").is_none());
        assert_eq!(trigram_expression("a\"b"), Some("\"a\"\"b\"".into()));
        assert!(trigram_expression("abc def").unwrap().contains(" AND "));
    }
}
