//! Bounded matching using scoring recovered from File Pilot's shipped binary.
//!
//! Query preparation owns its buffers. [`Matcher::score`] never allocates: ASCII
//! candidates are read directly and Unicode candidates decode into stack space.
//! Both query and candidate use at most [`MAX_TEXT_BYTES`] UTF-8 bytes, rounded
//! down to a character boundary. Scores are meaningful even when negative.
//!
//! Equality uses the source's compact Latin/Cyrillic simple-fold map, rather than
//! full Unicode normalization. A literal query is the default; multiword policies
//! are explicit because the source's caller-specific token policies are unproven.
//! See `docs/file-pilot-search-evidence.md` for evidence and reconstruction limits.

use std::cmp::Ordering;
use std::mem::MaybeUninit;

/// Maximum inspected UTF-8 prefix length for either query or candidate.
pub const MAX_TEXT_BYTES: usize = 1024;
/// Shared total-call threshold of the recovered alternate-alignment search.
pub const ALTERNATE_CALL_LIMIT: u8 = 10;

/// Matching algorithm. Substring is an explicit reconstruction option.
#[derive(
    Clone, Copy, Debug, Default, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum MatchMode {
    #[default]
    Fuzzy,
    Substring,
}

/// Combination of words separated by ASCII spaces.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TokenMode {
    /// Match the entire bounded query as one pattern, including its spaces.
    #[default]
    Literal,
    /// Sum the scores of matching tokens; at least one token must match.
    Any,
    /// Sum all token scores; every token must match.
    All,
}

#[derive(Clone, Debug)]
struct Pattern {
    folded: Vec<u32>,
    ascii: Option<Box<[u8]>>,
    // KMP failure links make substring matching linear even for repeated input.
    failure: Vec<usize>,
}

impl Pattern {
    fn new(query: &str) -> Self {
        let folded: Vec<_> = query.chars().map(|c| simple_fold(c as u32)).collect();
        let mut failure = vec![0; folded.len()];
        let mut matched = 0;
        for i in 1..folded.len() {
            while matched > 0 && folded[i] != folded[matched] {
                matched = failure[matched - 1];
            }
            if folded[i] == folded[matched] {
                matched += 1;
            }
            failure[i] = matched;
        }
        let ascii: Option<Box<[u8]>> = query.is_ascii().then(|| {
            query
                .bytes()
                .map(|byte| byte.to_ascii_lowercase())
                .collect()
        });
        Self {
            folded,
            ascii,
            failure,
        }
    }
}

/// Prepared, immutable query reusable across any number of candidates/threads.
#[derive(Clone, Debug)]
pub struct Matcher {
    query: Box<str>,
    patterns: Vec<Pattern>,
    mode: MatchMode,
    token_mode: TokenMode,
    query_truncated: bool,
}

impl Matcher {
    pub fn new(query: &str) -> Self {
        let bounded = bounded_prefix(query);
        let patterns = if bounded.is_empty() {
            Vec::new()
        } else {
            vec![Pattern::new(bounded)]
        };
        Self {
            query: bounded.into(),
            patterns,
            mode: MatchMode::Fuzzy,
            token_mode: TokenMode::Literal,
            query_truncated: bounded.len() < query.len(),
        }
    }

    pub fn with_mode(mut self, mode: MatchMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_token_mode(mut self, mode: TokenMode) -> Self {
        if mode != self.token_mode {
            self.patterns = match mode {
                TokenMode::Literal if self.query.is_empty() => Vec::new(),
                TokenMode::Literal => vec![Pattern::new(&self.query)],
                TokenMode::Any | TokenMode::All => self
                    .query
                    .split(' ')
                    .filter(|word| !word.is_empty())
                    .map(Pattern::new)
                    .collect(),
            };
            self.token_mode = mode;
        }
        self
    }

    pub fn mode(&self) -> MatchMode {
        self.mode
    }
    pub fn token_mode(&self) -> TokenMode {
        self.token_mode
    }
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
    pub fn is_query_truncated(&self) -> bool {
        self.query_truncated
    }
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns a rank for a complete match, including zero/negative ranks.
    ///
    /// An empty prepared query matches every candidate with rank zero. Ranking
    /// does not establish sorting stability: callers choose any tie breaker.
    pub fn score(&self, candidate: &str) -> Option<i32> {
        if self.is_empty() {
            return Some(0);
        }
        let candidate = bounded_prefix(candidate);
        if candidate.is_ascii() {
            self.evaluate(Ascii(candidate.as_bytes()), &mut NoIndices)
        } else {
            // Keep the bounded Unicode buffer in a separate stack frame. The
            // common ASCII path then needs neither a 4 KiB frame nor __chkstk.
            self.score_unicode(candidate)
        }
    }

    /// Exact membership without exploring alternate fuzzy ranks. A greedy
    /// subsequence exists exactly when a fuzzy alignment exists. ASCII uses
    /// the same eight-byte search as scoring; Unicode uses the same fold.
    pub fn is_match(&self, candidate: &str) -> bool {
        if self.is_empty() {
            return true;
        }
        if self.mode != MatchMode::Fuzzy {
            return self.score(candidate).is_some();
        }
        let candidate = bounded_prefix(candidate);
        if candidate.is_ascii() {
            self.membership(Ascii(candidate.as_bytes()))
        } else {
            self.membership_unicode(candidate)
        }
    }

    #[inline(never)]
    fn membership_unicode(&self, candidate: &str) -> bool {
        let mut points = [MaybeUninit::<u32>::uninit(); MAX_TEXT_BYTES];
        let mut length = 0;
        for character in candidate.chars() {
            points[length].write(character as u32);
            length += 1;
        }
        // SAFETY: all `length` code points were initialized, within the bounded prefix.
        let points = unsafe { std::slice::from_raw_parts(points.as_ptr().cast::<u32>(), length) };
        self.membership(Unicode(points))
    }

    fn membership<T: Units>(&self, text: T) -> bool {
        let mut any = false;
        for pattern in &self.patterns {
            let mut matches = pattern.folded.len() <= text.len() && text.can_match(pattern);
            let mut next = 0;
            if matches {
                for &letter in &pattern.folded {
                    match text.find(letter, next) {
                        Some(index) => next = index + 1,
                        None => {
                            matches = false;
                            break;
                        }
                    }
                }
            }
            if self.token_mode == TokenMode::All && !matches {
                return false;
            }
            any |= matches;
            if matches && self.token_mode != TokenMode::All {
                return true;
            }
        }
        any
    }

    #[inline(never)]
    fn score_unicode(&self, candidate: &str) -> Option<i32> {
        let mut points = [MaybeUninit::<u32>::uninit(); MAX_TEXT_BYTES];
        let mut length = 0;
        for character in candidate.chars() {
            points[length].write(character as u32);
            length += 1;
        }
        // SAFETY: exactly `length` values were initialized above; the bounded
        // UTF-8 prefix has at most MAX_TEXT_BYTES code points. u32 has no drop.
        let points = unsafe { std::slice::from_raw_parts(points.as_ptr().cast::<u32>(), length) };
        self.evaluate(Unicode(points), &mut NoIndices)
    }

    /// Matched character starts as sorted, unique UTF-8 byte offsets.
    ///
    /// This method deliberately allocates its returned vector; call it only for
    /// visible rows, after using allocation-free [`Self::score`] for filtering.
    /// Offsets refer to the original candidate and are always UTF-8 boundaries.
    pub fn match_indices(&self, candidate: &str) -> Option<Vec<usize>> {
        if self.is_empty() {
            return Some(Vec::new());
        }
        let candidate = bounded_prefix(candidate);
        let mut capture = Indices::default();
        if candidate.is_ascii() {
            self.evaluate(Ascii(candidate.as_bytes()), &mut capture)?;
            Some(
                capture
                    .union
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &selected)| selected.then_some(index))
                    .collect(),
            )
        } else {
            let mut points = [0; MAX_TEXT_BYTES];
            let mut offsets = [0; MAX_TEXT_BYTES];
            let mut length = 0;
            for (offset, character) in candidate.char_indices() {
                points[length] = character as u32;
                offsets[length] = offset;
                length += 1;
            }
            self.evaluate(Unicode(&points[..length]), &mut capture)?;
            Some(
                capture.union[..length]
                    .iter()
                    .enumerate()
                    .filter_map(|(index, &selected)| selected.then_some(offsets[index]))
                    .collect(),
            )
        }
    }

    fn evaluate<T: Units, C: Capture>(&self, text: T, capture: &mut C) -> Option<i32> {
        let mut total = 0;
        let mut matched_any = false;
        for pattern in &self.patterns {
            let result = if pattern.folded.len() > text.len() || !text.can_match(pattern) {
                None
            } else {
                match self.mode {
                    MatchMode::Fuzzy => text.fuzzy(pattern, capture),
                    MatchMode::Substring => text.substring(pattern, capture),
                }
            };
            match result {
                Some(score) => {
                    matched_any = true;
                    total += score;
                    capture.commit(pattern.folded.len());
                }
                None if self.token_mode == TokenMode::All => return None,
                None => {}
            }
        }
        matched_any.then_some(total)
    }
}

/// Inspect the same UTF-8 prefix used by [`Matcher`], without allocating.
pub fn bounded_prefix(text: &str) -> &str {
    if text.len() <= MAX_TEXT_BYTES {
        return text;
    }
    let mut end = MAX_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Build an indexing key using exactly the matcher's bounded simple folding.
///
/// This intentionally differs from `str::to_lowercase` for several characters,
/// including U+0130. Posting generation and query normalization can share this
/// helper to avoid case-map false negatives. It allocates only the output key.
pub fn fold_key(text: &str) -> String {
    let text = bounded_prefix(text);
    let mut key = String::with_capacity(text.len());
    for character in text.chars() {
        key.push(
            char::from_u32(simple_fold(character as u32))
                .expect("simple fold preserves scalar values"),
        );
    }
    key
}

/// Source-compatible simple folding of one numeric code point.
pub fn fold_codepoint(value: u32) -> u32 {
    simple_fold(value)
}

trait Units: Copy {
    fn len(self) -> usize;
    fn get(self, index: usize) -> u32;
    #[inline]
    fn can_match(self, _: &Pattern) -> bool {
        true
    }
    #[inline]
    fn folded(self, index: usize) -> u32 {
        simple_fold(self.get(index))
    }
    #[inline]
    fn find(self, needle: u32, start: usize) -> Option<usize> {
        (start..self.len()).find(|&index| self.folded(index) == needle)
    }
    #[inline]
    fn bonus(self, index: usize) -> i32 {
        boundary_bonus(self, index)
    }
    #[inline]
    fn fuzzy<C: Capture>(self, pattern: &Pattern, capture: &mut C) -> Option<i32> {
        fuzzy(self, &pattern.folded, capture)
    }
    #[inline]
    fn substring<C: Capture>(self, pattern: &Pattern, capture: &mut C) -> Option<i32> {
        substring(self, pattern, capture)
    }
}

#[derive(Clone, Copy)]
struct Ascii<'a>(&'a [u8]);
impl Units for Ascii<'_> {
    #[inline]
    fn len(self) -> usize {
        self.0.len()
    }
    #[inline]
    fn get(self, index: usize) -> u32 {
        self.0[index] as u32
    }
    #[inline]
    fn can_match(self, pattern: &Pattern) -> bool {
        pattern.ascii.is_some()
    }
    #[inline]
    fn folded(self, index: usize) -> u32 {
        self.0[index].to_ascii_lowercase() as u32
    }
    #[inline]
    fn find(self, needle: u32, start: usize) -> Option<usize> {
        find_ascii(self.0, needle as u8, start)
    }
    #[inline]
    fn bonus(self, index: usize) -> i32 {
        ascii_bonus(self.0, index)
    }
    #[inline]
    fn substring<C: Capture>(self, pattern: &Pattern, capture: &mut C) -> Option<i32> {
        let pattern_bytes = pattern.ascii.as_deref()?;
        if pattern_bytes.len() <= 32 {
            ascii_substring(self.0, pattern_bytes, capture)
        } else {
            substring(self, pattern, capture)
        }
    }
}
#[derive(Clone, Copy)]
struct Unicode<'a>(&'a [u32]);
impl Units for Unicode<'_> {
    fn len(self) -> usize {
        self.0.len()
    }
    fn get(self, index: usize) -> u32 {
        self.0[index]
    }
}

trait Capture {
    type Saved;
    fn record(&mut self, pattern_index: usize, candidate_index: usize);
    fn save(&self) -> Self::Saved;
    fn restore(&mut self, saved: &Self::Saved);
    fn commit(&mut self, length: usize);
}

struct NoIndices;
impl Capture for NoIndices {
    type Saved = ();
    #[inline(always)]
    fn record(&mut self, _: usize, _: usize) {}
    #[inline(always)]
    fn save(&self) {}
    #[inline(always)]
    fn restore(&mut self, _: &()) {}
    #[inline(always)]
    fn commit(&mut self, _: usize) {}
}

struct Indices {
    positions: [u16; MAX_TEXT_BYTES],
    union: [bool; MAX_TEXT_BYTES],
}
impl Default for Indices {
    fn default() -> Self {
        Self {
            positions: [0; MAX_TEXT_BYTES],
            union: [false; MAX_TEXT_BYTES],
        }
    }
}
impl Capture for Indices {
    type Saved = [u16; MAX_TEXT_BYTES];
    fn record(&mut self, pattern_index: usize, candidate_index: usize) {
        self.positions[pattern_index] = candidate_index as u16;
    }
    fn save(&self) -> Self::Saved {
        self.positions
    }
    fn restore(&mut self, saved: &Self::Saved) {
        self.positions = *saved;
    }
    fn commit(&mut self, length: usize) {
        for &index in &self.positions[..length] {
            self.union[index as usize] = true;
        }
    }
}

fn fuzzy<T: Units, C: Capture>(text: T, pattern: &[u32], capture: &mut C) -> Option<i32> {
    let base = 100 - 3 * (text.len() - pattern.len()) as i32;
    let first = text.find(pattern[0], 0)?;
    if pattern.len() == 1 && first == 0 {
        // A later one-character alignment cannot exceed the first-position
        // bonus50: its largest boundary bonus is30 and it also loses its index.
        capture.record(0, 0);
        return Some(base + 50);
    }
    if (2..=8).contains(&pattern.len()) {
        // Failure of the greedy subsequence proves no alignment can succeed.
        // Reject before alternate exploration; successful candidates still use
        // the unchanged shared-budget search for their rank and highlights.
        let mut next = first + 1;
        for &needle in &pattern[1..] {
            next = text.find(needle, next)? + 1;
        }
    }
    search(text, pattern, 0, first, None, base, &mut 1, capture)
}

// One shared counter bounds all recursive alternatives, matching the source's
// increment-at-entry then counter<10 condition. The greedy path is always tried,
// including when deeper alternatives have exhausted the shared search budget.
#[allow(clippy::too_many_arguments)]
fn search<T: Units, C: Capture>(
    text: T,
    pattern: &[u32],
    mut pattern_index: usize,
    mut index: usize,
    mut previous: Option<usize>,
    mut score: i32,
    calls: &mut u8,
    capture: &mut C,
) -> Option<i32> {
    let mut best: Option<(i32, C::Saved)> = None;
    loop {
        // Once exhausted, every later alternative returns before touching text.
        // Skip the function call while retaining the shared exhausted state.
        let alternative = if *calls < ALTERNATE_CALL_LIMIT - 1 {
            *calls += 1;
            // A recursive alternative with no occurrence of its first needle
            // only increments the shared count. Handle that empty search here
            // without a recursive stack frame; pass a known match otherwise.
            text.find(pattern[pattern_index], index + 1)
                .and_then(|alternative_index| {
                    search(
                        text,
                        pattern,
                        pattern_index,
                        alternative_index,
                        previous,
                        score,
                        calls,
                        capture,
                    )
                })
        } else {
            *calls = ALTERNATE_CALL_LIMIT;
            None
        };
        if let Some(alternative) = alternative {
            if best.as_ref().is_none_or(|(best, _)| alternative > *best) {
                best = Some((alternative, capture.save()));
            }
        }
        capture.record(pattern_index, index);
        if let Some(previous) = previous {
            let gap = index - previous;
            if gap > 5 {
                score += (5 - gap as i32) * 50;
            }
            if gap == 1 {
                score += 200;
            }
        } else {
            score -= index as i32;
        }
        score += text.bonus(index);
        previous = Some(index);
        pattern_index += 1;
        if pattern_index == pattern.len() {
            if let Some((alternative, saved)) = best {
                if alternative > score {
                    capture.restore(&saved);
                    return Some(alternative);
                }
            }
            return Some(score);
        }
        let Some(next) = text.find(pattern[pattern_index], index + 1) else {
            break;
        };
        index = next;
    }
    best.map(|(score, saved)| {
        capture.restore(&saved);
        score
    })
}

fn substring<T: Units, C: Capture>(text: T, pattern: &Pattern, capture: &mut C) -> Option<i32> {
    let length = pattern.folded.len();
    let base = 100 - 3 * (text.len() - length) as i32 + 200 * (length as i32 - 1);
    let mut matched = 0;
    let mut boundary_sum = 0;
    let mut best = None;
    for index in 0..text.len() {
        boundary_sum += text.bonus(index);
        if index >= length {
            boundary_sum -= text.bonus(index - length);
        }
        let unit = text.folded(index);
        while matched > 0 && unit != pattern.folded[matched] {
            matched = pattern.failure[matched - 1];
        }
        if unit == pattern.folded[matched] {
            matched += 1;
        }
        if matched == length {
            let start = index + 1 - length;
            let score = base - start as i32 + boundary_sum;
            if best.is_none_or(|(best, _)| score > best) {
                best = Some((score, start));
            }
            matched = pattern.failure[matched - 1];
        }
    }
    best.map(|(score, start)| {
        for index in 0..length {
            capture.record(index, start + index);
        }
        score
    })
}

fn boundary_bonus<T: Units>(text: T, index: usize) -> i32 {
    if index == 0 {
        return 50;
    }
    let previous = text.get(index - 1);
    let current = text.get(index);
    let camel = if is_lower(previous) && is_upper(current) {
        30
    } else {
        0
    };
    camel
        + if previous == b'_' as u32 || previous == b' ' as u32 {
            25
        } else {
            0
        }
}

#[inline]
fn ascii_bonus(text: &[u8], index: usize) -> i32 {
    if index == 0 {
        return 50;
    }
    let previous = text[index - 1];
    let current = text[index];
    let camel = if previous.is_ascii_lowercase() && current.is_ascii_uppercase() {
        30
    } else {
        0
    };
    camel
        + if previous == b'_' || previous == b' ' {
            25
        } else {
            0
        }
}

/// Eight-byte case-insensitive search without preparing or copying the text.
/// The zero-byte idiom's first set byte is exact even when borrow propagation
/// sets later bytes. from_le_bytes keeps the selected byte index portable.
#[inline]
fn find_ascii(text: &[u8], needle: u8, start: usize) -> Option<usize> {
    let remainder = text.get(start..)?;
    let letter = needle.is_ascii_lowercase();
    let repeated = u64::from(needle) * 0x0101_0101_0101_0101;
    let mut chunks = remainder.chunks_exact(8);
    let mut offset = start;
    for chunk in &mut chunks {
        let mut word = u64::from_le_bytes(chunk.try_into().unwrap());
        if letter {
            word |= 0x2020_2020_2020_2020;
        }
        let difference = word ^ repeated;
        let zero =
            difference.wrapping_sub(0x0101_0101_0101_0101) & !difference & 0x8080_8080_8080_8080;
        if zero != 0 {
            return Some(offset + zero.trailing_zeros() as usize / 8);
        }
        offset += 8;
    }
    chunks
        .remainder()
        .iter()
        .position(|&byte| (if letter { byte | 0x20 } else { byte }) == needle)
        .map(|index| offset + index)
}

fn ascii_substring<C: Capture>(text: &[u8], pattern: &[u8], capture: &mut C) -> Option<i32> {
    let length = pattern.len();
    let base = 100 - 3 * (text.len() - length) as i32 + 200 * (length as i32 - 1);
    let mut next = 0;
    let mut best = None;
    while let Some(start) = find_ascii(text, pattern[0], next) {
        let end = start + length;
        if end > text.len() {
            break;
        }
        if text[start..end].eq_ignore_ascii_case(pattern) {
            let mut score = base - start as i32;
            for index in start..end {
                score += ascii_bonus(text, index);
            }
            if best.is_none_or(|(best, _)| score > best) {
                best = Some((score, start));
            }
        }
        next = start + 1;
    }
    best.map(|(score, start)| {
        for index in 0..length {
            capture.record(index, start + index);
        }
        score
    })
}

// Explicit ranges mirror FUN_140169dc0. Greek, Turkish special casing,
// ligatures, expansions and composed/decomposed forms intentionally differ
// from full Unicode case folding.
fn simple_fold(value: u32) -> u32 {
    match value {
        0x41..=0x5a | 0xc0..=0xd6 | 0xd8..=0xde | 0x410..=0x42f => value + 0x20,
        0x400..=0x40f => value + 0x50,
        0x100..=0x17f | 0x460..=0x4ff | 0x500..=0x52f | 0xa640..=0xa66d | 0xa680..=0xa69b
            if value % 2 == 0 =>
        {
            value + 1
        }
        _ => value,
    }
}

fn is_lower(value: u32) -> bool {
    match value {
        0x61..=0x7a | 0xe0..=0xf6 | 0xf8..=0xff | 0x430..=0x45f => true,
        0x100..=0x17f | 0x460..=0x4ff | 0x500..=0x52f | 0xa640..=0xa66d | 0xa680..=0xa69b => {
            value % 2 == 1
        }
        _ => false,
    }
}

fn is_upper(value: u32) -> bool {
    match value {
        0x41..=0x5a | 0xc0..=0xd6 | 0xd8..=0xde | 0x400..=0x42f => true,
        0x100..=0x17f | 0x460..=0x4ff | 0x500..=0x52f | 0xa640..=0xa66d | 0xa680..=0xa69b => {
            value % 2 == 0
        }
        _ => false,
    }
}

/// Compare ranks in descending order, leaving ties to the caller.
pub fn compare_scores(left: i32, right: i32) -> Ordering {
    right.cmp(&left)
}

#[cfg(test)]
mod tests;
