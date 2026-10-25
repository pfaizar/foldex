use rusqlite::{Connection, functions::FunctionFlags};
use std::fmt::Write;
use std::{borrow::Cow, cell::RefCell, sync::Arc};

struct CachedScore {
    memo: crate::score_cache::ScoreMemo,
    name: [u8; foldex_match::MAX_TEXT_BYTES],
    length: usize,
    score: Option<i32>,
    #[cfg(feature = "diagnostics")]
    calls: u64,
    #[cfg(feature = "diagnostics")]
    last_hits: u64,
}

pub(crate) fn register(connection: &Connection) -> rusqlite::Result<()> {
    #[cfg(feature = "diagnostics")]
    let membership_calls = std::cell::Cell::new(0u64);
    connection.create_scalar_function(
        "foldex_subsequence",
        2,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        move |context| {
            #[cfg(feature = "diagnostics")]
            {
                if matches!(context.get_raw(0), rusqlite::types::ValueRef::Null) {
                    if matches!(context.get_raw(1), rusqlite::types::ValueRef::Null) {
                        membership_calls.set(0);
                    }
                    return Ok(membership_calls.get().min(i64::MAX as u64) as i64);
                }
                membership_calls.set(membership_calls.get() + 1);
            }
            let name = context
                .get_raw(0)
                .as_str()
                .map_err(|error| rusqlite::Error::UserFunctionError(error.into()))?;
            let needle = context
                .get_raw(1)
                .as_str()
                .map_err(|error| rusqlite::Error::UserFunctionError(error.into()))?;
            Ok(i64::from(subsequence(name, needle)))
        },
    )?;
    #[cfg(feature = "diagnostics")]
    let raw_membership_calls = std::cell::Cell::new(0u64);
    connection.create_scalar_function(
        "foldex_subsequence_raw",
        2,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        move |context| {
            #[cfg(feature = "diagnostics")]
            {
                if matches!(context.get_raw(0), rusqlite::types::ValueRef::Null) {
                    if matches!(context.get_raw(1), rusqlite::types::ValueRef::Null) {
                        raw_membership_calls.set(0);
                    }
                    return Ok(raw_membership_calls.get().min(i64::MAX as u64) as i64);
                }
                raw_membership_calls.set(raw_membership_calls.get() + 1);
            }
            let needle: Arc<Vec<u32>> = context.get_or_create_aux(1, |value| {
                let query = value
                    .as_str()
                    .map_err(|error| Box::<dyn std::error::Error + Send + Sync>::from(error))?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(
                    query.chars().map(u32::from).collect(),
                )
            })?;
            let raw = context
                .get_raw(0)
                .as_blob()
                .map_err(|error| rusqlite::Error::UserFunctionError(error.into()))?;
            let name = decoded_name(raw)?;
            let mut matched = 0usize;
            for letter in foldex_match::bounded_prefix(&name).chars() {
                if matched == needle.len() {
                    break;
                }
                if foldex_match::fold_codepoint(letter as u32) == needle[matched] {
                    matched += 1;
                }
            }
            Ok(i64::from(matched == needle.len()))
        },
    )?;
    let cache = RefCell::new(CachedScore {
        memo: crate::score_cache::ScoreMemo::new(),
        name: [0; foldex_match::MAX_TEXT_BYTES],
        length: usize::MAX,
        score: None,
        #[cfg(feature = "diagnostics")]
        calls: 0,
        #[cfg(feature = "diagnostics")]
        last_hits: 0,
    });
    connection.create_scalar_function(
        "foldex_fuzzy_score",
        2,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        move |context| {
            #[cfg(feature = "diagnostics")]
            {
                if matches!(context.get_raw(0), rusqlite::types::ValueRef::Null) {
                    let command = context.get_raw(1).as_i64().unwrap_or(-1);
                    let mut cached = cache.borrow_mut();
                    if command < 0 {
                        cached.calls = 0;
                        cached.last_hits = 0;
                        cached.memo.reset_statistics();
                        return Ok(Some(0i64));
                    }
                    let (hits, source) = cached.memo.statistics();
                    let value = match command {
                        1 => cached.calls,
                        2 => cached.last_hits,
                        3 => hits,
                        4 => source,
                        _ => 0,
                    };
                    return Ok(Some(value.min(i64::MAX as u64) as i64));
                }
            }
            let matcher: Arc<foldex_match::Matcher> = context.get_or_create_aux(1, |value| {
                let query = value
                    .as_str()
                    .map_err(|error| Box::<dyn std::error::Error + Send + Sync>::from(error))?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(foldex_match::Matcher::new(query))
            })?;
            let raw = context
                .get_raw(0)
                .as_blob()
                .map_err(|error| rusqlite::Error::UserFunctionError(error.into()))?;
            let mut cached = cache.borrow_mut();
            #[cfg(feature = "diagnostics")]
            {
                cached.calls += 1;
            }
            if raw.len() <= cached.name.len()
                && cached.length == raw.len()
                && cached.name[..cached.length] == *raw
                && cached.memo.query_matches(matcher.query())
            {
                #[cfg(feature = "diagnostics")]
                {
                    cached.last_hits += 1;
                }
                return Ok(cached.score.map(i64::from));
            }
            let name = decoded_name(raw)?;
            let score = cached.memo.score(&matcher, &name);
            if raw.len() <= cached.name.len() {
                cached.name[..raw.len()].copy_from_slice(raw);
                cached.length = raw.len();
                cached.score = score;
            } else {
                cached.length = usize::MAX;
            }
            Ok(score.map(i64::from))
        },
    )
}

fn decoded_name(raw: &[u8]) -> rusqlite::Result<Cow<'_, str>> {
    match std::str::from_utf8(raw) {
        Ok(text) => Ok(Cow::Borrowed(text)),
        Err(_) => Ok(Cow::Owned(
            crate::codec::decode(raw)
                .map_err(|error| {
                    rusqlite::Error::UserFunctionError(
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
                            .into(),
                    )
                })?
                .to_string_lossy()
                .into_owned(),
        )),
    }
}

#[cfg(feature = "diagnostics")]
pub(crate) fn reset_diagnostics(connection: &Connection) -> rusqlite::Result<()> {
    connection.query_row(
        "SELECT foldex_subsequence(NULL,NULL),foldex_subsequence_raw(NULL,NULL),foldex_fuzzy_score(NULL,-1)",
        [],
        |_| Ok(()),
    )
}

#[cfg(feature = "diagnostics")]
pub(crate) fn read_diagnostics(
    connection: &Connection,
    destination: &mut crate::QueryDiagnostics,
) -> rusqlite::Result<()> {
    connection.query_row(
        "SELECT foldex_subsequence(NULL,'calls')+foldex_subsequence_raw(NULL,'calls'),foldex_fuzzy_score(NULL,1),foldex_fuzzy_score(NULL,2),foldex_fuzzy_score(NULL,3),foldex_fuzzy_score(NULL,4)",
        [],
        |row| {
            destination.membership_calls = row.get::<_,i64>(0)?.max(0) as u64;
            destination.score_calls = row.get::<_,i64>(1)?.max(0) as u64;
            destination.last_name_hits = row.get::<_,i64>(2)?.max(0) as u64;
            destination.memo_hits = row.get::<_,i64>(3)?.max(0) as u64;
            destination.source_scores = row.get::<_,i64>(4)?.max(0) as u64;
            Ok(())
        }
    )
}

fn subsequence(name: &str, needle: &str) -> bool {
    let mut remaining = needle.chars();
    let Some(mut target) = remaining.next() else {
        return true;
    };
    for letter in name.chars() {
        if letter == target {
            let Some(next) = remaining.next() else {
                return true;
            };
            target = next;
        }
    }
    false
}

pub(crate) fn tokens(value: &str, query: bool) -> String {
    let mut letters: Vec<_> = value.chars().map(u32::from).collect();
    letters.sort_unstable();
    let mut output = String::with_capacity(letters.len() * 8);
    let mut at = 0;
    while at < letters.len() {
        let letter = letters[at];
        let start = at;
        while at < letters.len() && letters[at] == letter {
            at += 1;
        }
        // Prefix occurrence postings prevent a long repeated-letter/number query from
        // degenerating into a dense presence-only scan. Capping at8 is conservative:
        // the verifier checks exact multiplicities beyond that without false negatives.
        let count = (at - start).min(8);
        let first = if query { count } else { 1 };
        for occurrence in first..=count {
            if !output.is_empty() {
                output.push_str(if query { " AND " } else { " " });
            }
            write!(&mut output, "c{letter:x}").expect("writing a String cannot fail");
            if occurrence > 1 {
                write!(&mut output, "n{occurrence}").expect("String write");
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fuzzy_matching_keeps_gap_order_repetitions_and_unicode() {
        assert!(subsequence("design", "dsg"));
        assert!(subsequence("東京の本", "東本"));
        assert!(subsequence("bookkeeper", "bkkp"));
        assert!(!subsequence("design", "dgs"));
        assert!(!subsequence("book", "ooo"));
        assert!(subsequence("", ""));
        assert_eq!(tokens("ddb東", true), "c62 AND c64n2 AND c6771");
    }
}
