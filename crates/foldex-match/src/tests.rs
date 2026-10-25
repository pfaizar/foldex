use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct TrackedAllocator;
thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;
unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|track| {
            if track.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: the test allocator forwards the caller's layout unchanged.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: pointers originate from the same System allocator.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|track| {
            if track.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: reallocation forwards the original allocation and layout.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[test]
fn recovered_numeric_ascii_scores() {
    for (query, candidate, expected) in [
        ("d", "Design", 135),
        ("d", "Downloads", 126),
        ("d", "Documents", 126),
        ("d", "Readme.md", 73),
        ("dsg", "Design", 141),
        ("de", "Design", 338),
        ("fb", "fooBar", 168),
        ("fb", "foo_bar", 160),
        ("fb", "foo/bar", 135),
        ("az", "a..........z", -180),
    ] {
        assert_eq!(
            Matcher::new(query).score(candidate),
            Some(expected),
            "{query:?} in {candidate:?}"
        );
    }
    assert_eq!(Matcher::new("z").score("Design"), None);
}

#[test]
fn negative_and_zero_scores_are_still_matches() {
    assert_eq!(Matcher::new("az").score("a..........z"), Some(-180));
    // n=51, pattern=1, first=0: 100-3*50+50 = 0.
    assert_eq!(
        Matcher::new("a").score(&format!("a{}", "x".repeat(50))),
        Some(0)
    );
    assert_eq!(
        Matcher::new("az").match_indices("a..........z"),
        Some(vec![0, 11])
    );
}

#[test]
fn unicode_source_folding_and_camel_boundaries() {
    assert_eq!(Matcher::new("é").score("Étage"), Some(138));
    assert_eq!(Matcher::new("пр").score("Проект"), Some(338));
    assert_eq!(Matcher::new("жз").score("жЗ"), Some(380));
    assert_eq!(Matcher::new("éB").score("éBeta"), Some(371));
    assert_eq!(Matcher::new("b").score("αB"), Some(96));
    assert_eq!(Matcher::new("ı").score("İ"), Some(150));
    // The source's compact map deliberately leaves Greek and normalization alone.
    assert_eq!(Matcher::new("α").score("Α"), None);
    assert_eq!(Matcher::new("é").score("e\u{301}"), None);
}

#[test]
fn highlights_are_original_utf8_byte_offsets() {
    assert_eq!(
        Matcher::new("dsg").match_indices("Design"),
        Some(vec![0, 2, 4])
    );
    assert_eq!(Matcher::new("жз").match_indices("жЗ"), Some(vec![0, 2]));
    assert_eq!(Matcher::new("😀b").match_indices("😀βB"), Some(vec![0, 6]));
    assert_eq!(Matcher::new("x").match_indices("😀βB"), None);
}

#[test]
fn alternate_alignment_prefers_consecutive_matches() {
    assert_eq!(Matcher::new("ab").score("aab"), Some(296));
    assert_eq!(Matcher::new("ab").match_indices("aab"), Some(vec![1, 2]));
    assert_eq!(Matcher::new("aa").score("aaaa"), Some(344));
    assert_eq!(Matcher::new("aa").match_indices("aaaa"), Some(vec![0, 1]));
    assert_eq!(Matcher::new("ab").score("a_bab"), Some(288));
    assert_eq!(Matcher::new("ab").match_indices("a_bab"), Some(vec![3, 4]));
    // Exhausting alternate attempts still allows a complete greedy match.
    assert!(
        Matcher::new(&"a".repeat(1024))
            .score(&"a".repeat(1024))
            .is_some()
    );
}

#[test]
fn substring_uses_linear_contiguous_matching() {
    let matcher = Matcher::new("sig").with_mode(MatchMode::Substring);
    assert_eq!(matcher.score("Design"), Some(489));
    assert_eq!(matcher.match_indices("Design"), Some(vec![2, 3, 4]));
    assert_eq!(
        Matcher::new("dsg")
            .with_mode(MatchMode::Substring)
            .score("Design"),
        None
    );
    let repeated = "a".repeat(1024);
    assert_eq!(
        Matcher::new(&format!("{}b", "a".repeat(511)))
            .with_mode(MatchMode::Substring)
            .score(&repeated),
        None
    );
    assert_eq!(
        Matcher::new("жз")
            .with_mode(MatchMode::Substring)
            .match_indices("жЗ"),
        Some(vec![0, 2])
    );
}

#[test]
fn literal_any_and_all_are_explicit_policies() {
    assert_eq!(Matcher::new("des notes").score("Design"), None);
    let any = Matcher::new("des notes").with_token_mode(TokenMode::Any);
    let all = Matcher::new("des notes").with_token_mode(TokenMode::All);
    assert_eq!(any.score("Design"), Some(541));
    assert_eq!(all.score("Design"), None);
    assert!(all.score("Design Notes").is_some());
    assert_eq!(
        Matcher::new("d ds d")
            .with_token_mode(TokenMode::All)
            .match_indices("Design"),
        Some(vec![0, 2])
    );
    let literal = any.with_token_mode(TokenMode::Literal);
    assert_eq!(literal.token_mode(), TokenMode::Literal);
    assert_eq!(literal.score("Design"), None);
}

#[test]
fn empty_patterns_and_inputs_are_defined() {
    let empty = Matcher::new("");
    assert!(empty.is_empty());
    assert_eq!(empty.score(""), Some(0));
    assert_eq!(empty.match_indices("anything"), Some(vec![]));
    assert_eq!(Matcher::new("a").score(""), None);
    assert_eq!(Matcher::new(" ").score(""), None);
    assert_eq!(
        Matcher::new("   ")
            .with_token_mode(TokenMode::Any)
            .score(""),
        Some(0)
    );
}

#[test]
fn utf8_prefix_bounds_apply_before_decoding() {
    let prefix = format!("{}é", "a".repeat(1023));
    let matcher = Matcher::new(&prefix);
    assert!(matcher.is_query_truncated());
    assert_eq!(matcher.query().len(), 1023);
    assert!(!matcher.query().contains('é'));
    assert_eq!(Matcher::new("é").score(&prefix), None);
    assert_eq!(
        Matcher::new("z").score(&format!("{}z", "a".repeat(32768))),
        None
    );
    let long_unicode = format!("{}z", "ж".repeat(32768));
    assert_eq!(Matcher::new("z").score(&long_unicode), None);
    assert_eq!(
        Matcher::new("ж").match_indices(&long_unicode),
        Some(vec![0])
    );
    assert_eq!(
        Matcher::new(&"a".repeat(4096)).query().len(),
        MAX_TEXT_BYTES
    );
}

#[test]
fn per_candidate_score_performs_no_heap_allocations() {
    let matchers = [
        Matcher::new("dsg"),
        Matcher::new("жз"),
        Matcher::new("aab").with_mode(MatchMode::Substring),
        Matcher::new("des notes").with_token_mode(TokenMode::Any),
    ];
    ALLOCATIONS.with(|count| count.set(0));
    TRACK_ALLOCATIONS.with(|track| track.set(true));
    for _ in 0..100 {
        for matcher in &matchers {
            std::hint::black_box(matcher.score(std::hint::black_box("Design")));
            std::hint::black_box(matcher.score(std::hint::black_box("жЗ")));
            std::hint::black_box(matcher.score(std::hint::black_box("aaab")));
        }
    }
    TRACK_ALLOCATIONS.with(|track| track.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
}

#[test]
fn descending_score_comparator_does_not_break_ties() {
    assert_eq!(compare_scores(135, 126), Ordering::Less);
    assert_eq!(compare_scores(126, 126), Ordering::Equal);
    assert_eq!(compare_scores(-180, 0), Ordering::Greater);
}

#[test]
fn exported_fold_keys_share_bounds_and_source_case_mapping() {
    assert_eq!(
        fold_key("Design ÉTAGE Проект İ Α"),
        "design étage проект ı Α"
    );
    assert_eq!(fold_codepoint('İ' as u32), 'ı' as u32);
    assert_eq!(fold_codepoint('Α' as u32), 'Α' as u32);
    let long = format!("{}é", "A".repeat(1023));
    assert_eq!(bounded_prefix(&long).len(), 1023);
    assert_eq!(fold_key(&long), "a".repeat(1023));
}

#[test]
fn word_search_matches_scalar_search_for_every_ascii_needle() {
    let mut state = 0x1234_5678_9abc_def0u64;
    let data: Vec<u8> = (0..1033)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state & 0x7f) as u8
        })
        .collect();
    for raw in 0u8..128 {
        let needle = raw.to_ascii_lowercase();
        for start in 0..=data.len() {
            let scalar = data[start..]
                .iter()
                .position(|byte| byte.to_ascii_lowercase() == needle)
                .map(|index| start + index);
            assert_eq!(
                find_ascii(&data, needle, start),
                scalar,
                "needle{needle} start{start}"
            );
        }
    }
}

// Frozen pre-optimization scalar search. This differential oracle protects the
// recovered shared call-budget semantics while word search and no-op recursive
// alternatives are optimized in production. It is intentionally unoptimized.
#[allow(clippy::too_many_arguments)]
fn reference_search(
    text: &[u32],
    pattern: &[u32],
    mut q: usize,
    start: usize,
    mut previous: Option<usize>,
    mut score: i32,
    calls: &mut u8,
) -> Option<i32> {
    *calls = calls.saturating_add(1);
    if *calls >= ALTERNATE_CALL_LIMIT || start >= text.len() {
        return None;
    }
    let mut best: Option<i32> = None;
    for index in start..text.len() {
        if simple_fold(text[index]) != pattern[q] {
            continue;
        }
        if let Some(alternative) =
            reference_search(text, pattern, q, index + 1, previous, score, calls)
        {
            if best.is_none_or(|best| alternative > best) {
                best = Some(alternative);
            }
        }
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
        score += boundary_bonus(Unicode(text), index);
        previous = Some(index);
        q += 1;
        if q == pattern.len() {
            return Some(best.map_or(score, |best| best.max(score)));
        }
    }
    best
}

#[test]
fn optimized_search_preserves_scalar_scores_and_alternate_budget() {
    let alphabet = [
        'a', 'A', 'b', 'B', '_', ' ', 'x', 'z', 'é', 'É', 'ж', 'Ж', '😀', 'Α',
    ];
    let mut state = 0xd1b5_4a32_d192_ed03u64;
    for case in 0..768 {
        let length = 8 + case % 57;
        let text: Vec<u32> = (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                alphabet[state as usize % if case % 2 == 0 { 8 } else { alphabet.len() }] as u32
            })
            .collect();
        let candidate: String = text
            .iter()
            .map(|&value| char::from_u32(value).unwrap())
            .collect();
        for offset in 0..8 {
            let query: String = text
                .iter()
                .skip(offset)
                .step_by(3 + offset)
                .take(2 + offset)
                .map(|&value| char::from_u32(value).unwrap())
                .collect();
            let pattern: Vec<u32> = query
                .chars()
                .map(|value| simple_fold(value as u32))
                .collect();
            let expected = reference_search(
                &text,
                &pattern,
                0,
                0,
                None,
                100 - 3 * (text.len() - pattern.len()) as i32,
                &mut 0,
            );
            assert_eq!(
                Matcher::new(&query).score(&candidate),
                expected,
                "query{query:?} candidate{candidate:?}"
            );
        }
    }
}

#[test]
fn returned_highlights_explain_each_score_for_ambiguous_short_inputs() {
    // Exercise alternate-path capture independently of the scorer's state:
    // reconstruct the documented numeric expression from its returned offsets.
    for length in 1..=7 {
        for mask in 0..(1 << length) {
            let candidate: String = (0..length)
                .map(|bit| if mask & (1 << bit) == 0 { 'a' } else { 'B' })
                .collect();
            for query in ["a", "b", "aa", "ab", "ba", "bb", "aaa", "aba", "bab"] {
                let matcher = Matcher::new(query);
                let score = matcher.score(&candidate);
                let positions = matcher.match_indices(&candidate);
                assert_eq!(score.is_some(), positions.is_some());
                let Some(positions) = positions else {
                    continue;
                };
                assert_eq!(positions.len(), query.len());
                let mut expected =
                    100 - 3 * (candidate.len() - query.len()) as i32 - positions[0] as i32;
                for (i, &position) in positions.iter().enumerate() {
                    assert_eq!(
                        candidate.as_bytes()[position].to_ascii_lowercase(),
                        query.as_bytes()[i]
                    );
                    if position == 0 {
                        expected += 50;
                    } else if candidate.as_bytes()[position - 1].is_ascii_lowercase()
                        && candidate.as_bytes()[position].is_ascii_uppercase()
                    {
                        expected += 30;
                    }
                    if i > 0 {
                        let gap = position - positions[i - 1];
                        if gap == 1 {
                            expected += 200;
                        }
                        if gap > 5 {
                            expected += (5 - gap as i32) * 50;
                        }
                    }
                }
                assert_eq!(
                    score,
                    Some(expected),
                    "{query} in {candidate}: {positions:?}"
                );
            }
        }
    }
}
