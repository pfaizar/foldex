//! Query-dependent memoization of exact source scores. This never approximates a rank.
use foldex_match::{MAX_TEXT_BYTES, Matcher, bounded_prefix, fold_codepoint};
use std::collections::VecDeque;

const MAX_ENTRIES: usize = 64;
const BYTE_BUDGET: usize = 64 * 1024;
const SIGNATURE_WORDS: usize = 2 + 3 * MAX_TEXT_BYTES;

struct Saved {
    hash: u64,
    signature: Box<[u32]>,
    score: Option<i32>,
}

pub(crate) struct ScoreMemo {
    query: String,
    ascii: [bool; 128],
    other: Vec<u32>,
    query_units: usize,
    miss_streak: usize,
    bypass_left: usize,
    scratch: Vec<u32>,
    saved: VecDeque<Saved>,
    bytes: usize,
    #[cfg(any(test, feature = "diagnostics"))]
    hits: usize,
    #[cfg(any(test, feature = "diagnostics"))]
    misses: usize,
    #[cfg(any(test, feature = "diagnostics"))]
    bypasses: usize,
}

impl ScoreMemo {
    pub(crate) fn new() -> Self {
        Self {
            query: String::new(),
            ascii: [false; 128],
            other: Vec::new(),
            query_units: 0,
            miss_streak: 0,
            bypass_left: 0,
            scratch: Vec::with_capacity(SIGNATURE_WORDS),
            saved: VecDeque::with_capacity(MAX_ENTRIES),
            bytes: 0,
            #[cfg(any(test, feature = "diagnostics"))]
            hits: 0,
            #[cfg(any(test, feature = "diagnostics"))]
            misses: 0,
            #[cfg(any(test, feature = "diagnostics"))]
            bypasses: 0,
        }
    }

    pub(crate) fn query_matches(&self, query: &str) -> bool {
        self.query == query
    }

    #[cfg(feature = "diagnostics")]
    pub(crate) fn statistics(&self) -> (u64, u64) {
        (self.hits as u64, (self.misses + self.bypasses) as u64)
    }

    #[cfg(feature = "diagnostics")]
    pub(crate) fn reset_statistics(&mut self) {
        self.hits = 0;
        self.misses = 0;
        self.bypasses = 0;
    }

    fn prepare(&mut self, query: &str) {
        if self.query == query {
            return;
        }
        self.query.clear();
        self.query.push_str(query);
        self.ascii.fill(false);
        self.other.clear();
        self.query_units = query.chars().count();
        self.miss_streak = 0;
        self.bypass_left = 0;
        for ch in query.chars() {
            let folded = fold_codepoint(ch as u32);
            if folded < 128 {
                self.ascii[folded as usize] = true;
            } else {
                self.other.push(folded);
            }
        }
        self.other.sort_unstable();
        self.other.dedup();
        self.saved.clear();
        self.bytes = 0;
    }

    fn signature(&mut self, candidate: &str) -> u64 {
        let text = bounded_prefix(candidate);
        self.scratch.clear();
        self.scratch.extend([0, u32::from(text.is_ascii())]);
        let mut previous = u32::MAX;
        let mut length = 0;
        for (index, ch) in text.chars().enumerate() {
            let raw = ch as u32;
            let folded = fold_codepoint(raw);
            let matches = if folded < 128 {
                self.ascii[folded as usize]
            } else {
                self.other.binary_search(&folded).is_ok()
            };
            if matches {
                self.scratch.extend([index as u32, raw, previous]);
            }
            previous = raw;
            length += 1;
        }
        self.scratch[0] = length;
        let mut hash = 0xcbf29ce484222325u64;
        for &word in &self.scratch {
            hash = (hash ^ word as u64).wrapping_mul(0x100000001b3);
        }
        hash
    }

    pub(crate) fn score(&mut self, matcher: &Matcher, candidate: &str) -> Option<i32> {
        self.prepare(matcher.query());
        // Direct ASCII and one-character scoring beat signature construction even at a
        // near-perfect hit rate. Random Unicode names also need bounded admission work:
        // after repeated misses, periodically retry rather than hashing every candidate.
        if self.query_units < 2 || candidate.is_ascii() || self.bypass_left > 0 {
            self.bypass_left = self.bypass_left.saturating_sub(1);
            #[cfg(any(test, feature = "diagnostics"))]
            {
                self.bypasses += 1;
            }
            return matcher.score(candidate);
        }
        let hash = self.signature(candidate);
        // The hash only speeds lookup: every hit compares the complete signature.
        if let Some(found) = self
            .saved
            .iter()
            .find(|saved| saved.hash == hash && *saved.signature == self.scratch)
        {
            self.miss_streak = 0;
            #[cfg(any(test, feature = "diagnostics"))]
            {
                self.hits += 1;
            }
            return found.score;
        }
        #[cfg(any(test, feature = "diagnostics"))]
        {
            self.misses += 1;
        }
        let score = matcher.score(candidate);
        self.miss_streak += 1;
        if self.miss_streak >= 16 {
            self.miss_streak = 0;
            self.bypass_left = 256;
        }
        let bytes = self.scratch.len() * size_of::<u32>();
        while self.saved.len() >= MAX_ENTRIES || self.bytes.saturating_add(bytes) > BYTE_BUDGET {
            let Some(old) = self.saved.pop_front() else {
                break;
            };
            self.bytes -= old.signature.len() * size_of::<u32>();
        }
        self.saved.push_back(Saved {
            hash,
            signature: self.scratch.clone().into_boxed_slice(),
            score,
        });
        self.bytes += bytes;
        score
    }
}

// Proof boundary: the literal fuzzy matcher reads candidate length, the positions of every
// query-codepoint occurrence, and each matched character's boundary bonus. Keeping original
// current/previous codepoints at every possible match preserves those bonuses exactly without
// copying their implementation. Nonmatching characters cannot change any alternate alignment.
// The ASCII/Unicode execution path is retained conservatively. Matcher changes require parity tests.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_unmatched_text_reuses_the_exact_score() {
        let matcher = Matcher::new("irpt");
        let mut memo = ScoreMemo::new();
        for name in [
            "item000_東京_report.txt",
            "item923_東京_report.txt",
            "item456_東京_report.txt",
        ] {
            assert_eq!(memo.score(&matcher, name), matcher.score(name));
        }
        assert_eq!(memo.misses, 1);
        assert_eq!(memo.hits, 2);
        for name in ["invoice_000_東京_report.txt", "invoice_923_東京_report.txt"] {
            assert_eq!(memo.score(&matcher, name), matcher.score(name));
        }
        assert!(memo.hits >= 3);
    }

    #[test]
    fn camel_separators_unicode_and_alternate_alignments_keep_matcher_parity() {
        let names = [
            "xaB",
            "1aB",
            "xa_b",
            "xa b",
            "XAB",
            "readme",
            "ReadMe",
            "read_me",
            "read me",
            "İABC",
            "ıABC",
            "Ωω_ABC",
            "東京の本.txt",
            "東京本東京.txt",
            "Àa_ÄB",
            "Данные_Файл.txt",
            "__ab_ab_ab_ba__",
            "bookkeeper",
            "foo.bar/baz",
            "\\aBc",
            "",
            "a",
        ];
        let queries = [
            "ab", "AB", "aB", "rm", "d", "bkkp", "ooo", "東本", "ıa", "Ωa", "фл", "_ab", " ", "",
        ];
        let mut memo = ScoreMemo::new();
        for query in queries {
            let matcher = Matcher::new(query);
            for _ in 0..3 {
                for name in names {
                    assert_eq!(
                        memo.score(&matcher, name),
                        matcher.score(name),
                        "{query:?} / {name:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn mixed_and_adversarial_names_match_source_and_cache_remains_bounded() {
        let alphabet = [
            'a', 'A', 'b', 'B', 'z', '0', '9', '_', ' ', '.', '-', '東', '京', 'Ω', 'ω', 'İ', 'ı',
            'Д', 'д', 'À', 'à',
        ];
        let mut seed = 0x8f6dfecc1234u64;
        let mut memo = ScoreMemo::new();
        for query in ["ab", "aB", "東b", "0a9", "Ωω", "ıД", "Aa_", "zz", " ", ".a"] {
            let matcher = Matcher::new(query);
            for _ in 0..1500 {
                let mut name = String::new();
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                let length = 1 + ((seed >> 32) % 70) as usize;
                for _ in 0..length {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    name.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
                }
                assert_eq!(
                    memo.score(&matcher, &name),
                    matcher.score(&name),
                    "{query:?} / {name:?}"
                );
                assert!(memo.bytes <= BYTE_BUDGET && memo.saved.len() <= MAX_ENTRIES);
            }
        }
        let matcher = Matcher::new("aa東");
        let long = format!("{}東", "a".repeat(MAX_TEXT_BYTES - 4));
        for suffix in ["_suffix1", "_suffix2"] {
            let name = format!("{long}{suffix}");
            assert_eq!(memo.score(&matcher, &name), matcher.score(&name));
        }
        assert!(memo.scratch.capacity() <= SIGNATURE_WORDS);
    }

    #[test]
    fn hash_collisions_do_not_reuse_another_signature() {
        let matcher = Matcher::new("ab");
        let mut memo = ScoreMemo::new();
        memo.score(&matcher, "ab_東京");
        let hash = memo.signature("__a_b_東京");
        memo.saved[0].hash = hash;
        assert_eq!(
            memo.score(&matcher, "__a_b_東京"),
            matcher.score("__a_b_東京")
        );
        assert_eq!(memo.misses, 2);
    }

    #[test]
    fn expensive_misses_back_off_and_a_query_change_resets_admission() {
        let matcher = Matcher::new("ab");
        let mut memo = ScoreMemo::new();
        for index in 0..300 {
            let name = format!("{}ab_東京", "_".repeat(index));
            assert_eq!(memo.score(&matcher, &name), matcher.score(&name));
        }
        assert!(memo.bypasses >= 256);
        assert!(memo.misses < 40);
        let changed = Matcher::new("東a");
        let name = "東京_ab";
        assert_eq!(memo.score(&changed, name), changed.score(name));
        assert_eq!(memo.bypass_left, 0);
        assert!(memo.bytes <= BYTE_BUDGET);
    }

    #[test]
    #[ignore = "explicit release scoring microbenchmark; no filesystem input"]
    fn benchmark_structured_mixed_and_adversarial_candidates() {
        use std::{hint::black_box, time::Instant};
        let mut seed = 0x83ad8101u64;
        for workload in [
            "structured",
            "ascii-structured",
            "mixed",
            "mixed-single",
            "adversarial",
        ] {
            let mut names = Vec::with_capacity(50_000);
            for index in 0..50_000 {
                let name = match workload {
                    "structured" => format!("item{index:09}_東京_report.txt"),
                    "ascii-structured" => format!("item{index:09}_annual_report.txt"),
                    "mixed" | "mixed-single" => match index % 10 {
                        0 => format!("Design-review-{index}.md"),
                        1 => format!("projectReport_{index:08}.rs"),
                        2 => format!("invoice {index}_FINAL.pdf"),
                        3 => format!("東京_写真_{index:05}.jpg"),
                        4 => format!("README-{index}.txt"),
                        5 => format!("runtime_cache_{index}.json"),
                        6 => format!("Summary Q{}_{}.xlsx", index % 4, index),
                        7 => format!("buildArtifact-{index}.bin"),
                        8 => format!("Данные_Файл_{index}.csv"),
                        _ => format!("main_lib_{index}.toml"),
                    },
                    _ => {
                        let alphabet = [
                            'r', 'R', 'p', 'P', 't', 'T', 'a', 'A', '_', ' ', '東', '京', '0', '9',
                        ];
                        let mut text = String::new();
                        for _ in 0..32 {
                            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                            text.push(alphabet[(seed >> 32) as usize % alphabet.len()]);
                        }
                        text
                    }
                };
                names.push(name);
            }
            let query = match workload {
                "structured" => "it東rpt",
                "ascii-structured" => "iart",
                "mixed-single" => "r",
                _ => "rpt",
            };
            let matcher = Matcher::new(query);
            // SQL applies exact membership before calling the ranked scalar, so profile
            // matching candidates here rather than charging the memo for rejected names.
            names.retain(|name| matcher.score(name).is_some());
            let expected: Vec<_> = names.iter().map(|name| matcher.score(name)).collect();
            let started = Instant::now();
            for name in &names {
                black_box(matcher.score(black_box(name)));
            }
            let direct_ms = started.elapsed().as_secs_f64() * 1000.0;
            let mut memo = ScoreMemo::new();
            let started = Instant::now();
            for (name, expected) in names.iter().zip(expected) {
                assert_eq!(black_box(memo.score(&matcher, black_box(name))), expected);
            }
            let memo_ms = started.elapsed().as_secs_f64() * 1000.0;
            println!(
                "{{\"workload\":\"{workload}\",\"generated\":50000,\"candidates\":{},\"query\":\"{query}\",\"source_ms\":{direct_ms:.3},\"memo_ms\":{memo_ms:.3},\"hits\":{},\"misses\":{},\"bypasses\":{},\"signature_bytes\":{}}}",
                names.len(),
                memo.hits,
                memo.misses,
                memo.bypasses,
                memo.bytes
            );
        }
    }
}
