//! Launcher policy around Frizbee's maintained SIMD Smith-Waterman matcher.
//!
//! Query words are independent literal fuzzy patterns, so omitted title words
//! and punctuation in app names need no special query syntax. All alignment,
//! typo handling and ranking comes from Frizbee; this module sets input limits,
//! a relevance floor, and a short initialism alias for named applications.

use frizbee::{CaseMatching, Config, Scoring};

const MAX_WORDS: usize = 16;
const MAX_BYTES: usize = foldex_match::MAX_TEXT_BYTES;

struct Word {
    matcher: frizbee::Matcher,
    floor: u16,
}

pub(crate) struct Matcher {
    query: String,
    words: Vec<Word>,
    alias: Option<frizbee::Matcher>,
    truncated: bool,
    initials: String,
}

impl Matcher {
    pub(crate) fn new(query: &str) -> Self {
        let query = query.trim();
        let bounded = foldex_match::bounded_prefix(query);
        debug_assert!(bounded.len() <= MAX_BYTES);
        let mut truncated = bounded.len() != query.len();
        let mut words = Vec::new();
        for word in bounded.split_whitespace() {
            if words.len() == MAX_WORDS {
                truncated = true;
                break;
            }
            let characters = word.chars().count();
            // Same length-based prefilter policy documented by Frizbee/skim.
            // Cap it to avoid turning a long app query into arbitrary matches.
            let typos = (characters / 4).min(2) as u16;
            let scoring = Scoring {
                // Launchers have few relevant names, unlike a full file picker.
                // Discourage long scattered runs using the upstream scorer,
                // while an initialism is scored as its own name below.
                gap_open_penalty: 12,
                gap_extend_penalty: 4,
                ..Scoring::default()
            };
            let floor = if characters <= 2 {
                0
            } else {
                (characters as u16)
                    .saturating_sub(typos)
                    .saturating_mul(scoring.match_score)
                    // A transposition damages two adjacent alignment cells.
                    // Leave room for the upstream gap cost too, including when
                    // the word is later in a title and has no prefix bonus.
                    .saturating_sub(typos.saturating_mul(scoring.gap_open_penalty))
            };
            let config = Config {
                max_typos: Some(typos),
                casing: CaseMatching::Ignore,
                scoring,
                ..Config::default()
            };
            words.push(Word {
                matcher: frizbee::Matcher::new(word, &config),
                floor,
            });
        }
        let alias = (words.len() == 1 && bounded.chars().count() <= 4).then(|| {
            frizbee::Matcher::new(
                bounded,
                &Config {
                    max_typos: Some(0),
                    casing: CaseMatching::Ignore,
                    ..Config::default()
                },
            )
        });
        Self {
            query: bounded.into(),
            words,
            alias,
            truncated,
            initials: String::with_capacity(32),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn score(&mut self, title: &str) -> Option<i32> {
        if self.truncated {
            return None;
        }
        let title = foldex_match::bounded_prefix(title);
        let mut total = 0i32;
        for word in &mut self.words {
            let score = word.matcher.match_one(title, 0)?.score;
            if score < word.floor {
                return None;
            }
            total = total.saturating_add(i32::from(score));
        }
        Some(total)
    }

    pub(crate) fn app_score(&mut self, title: &str) -> Option<i32> {
        let direct = self.score(title);
        // Aliases belong to the bounded app catalog, never paths/clipboard
        // contents. Restrict them to short single terms, with no typo budget.
        if self.truncated || self.alias.is_none() {
            return direct;
        }
        self.initials.clear();
        let mut previous: Option<char> = None;
        let mut count = 0;
        for character in foldex_match::bounded_prefix(title).chars() {
            let boundary = character.is_alphanumeric()
                && previous.is_none_or(|before| {
                    !before.is_alphanumeric() || before.is_lowercase() && character.is_uppercase()
                });
            if boundary {
                self.initials.push(character);
                count += 1;
                if count > MAX_WORDS {
                    return direct;
                }
            }
            previous = Some(character);
        }
        if count < 2 {
            return direct;
        }
        // The same upstream engine scores aliases, with typos disabled. A
        // vendor prefix (Microsoft Visual Studio Code -> MVSC) may be omitted.
        let alias = self
            .alias
            .as_mut()
            .and_then(|matcher| matcher.match_one(&self.initials, 0))
            .filter(|matched| matched.score >= self.words[0].floor)
            .map(|matched| i32::from(matched.score));
        direct.into_iter().chain(alias).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_words_transpositions_and_unicode_are_real_matches() {
        for (query, title) in [
            ("visual code", "Visual Studio Code"),
            ("visaul code", "Visual Studio Code"),
            ("visual cdoe", "Visual Studio Code"),
            ("visaul cdoe", "Microsoft Visual Studio Code"),
            ("noets", "Microsoft Notes"),
            ("code visual", "Visual Studio Code"),
            ("visul code", "Visual Studio Code"),
            ("notpa", "Notepad"),
            ("ntpd", "Notepad"),
            ("win term", "Windows Terminal"),
            ("прив", "Привет"),
            ("東京 文書", "東京の文書"),
            ("Éditeur", "ÉDITEUR"),
            ("C++", "C++ Editor"),
        ] {
            assert!(
                Matcher::new(query).score(title).is_some(),
                "{query:?}: {title:?}"
            );
        }
    }

    #[test]
    fn app_initialisms_do_not_open_a_scattered_path_exception() {
        for (query, title) in [
            ("vsc", "Visual Studio Code"),
            ("VSC", "VisualStudioCode"),
            ("vsc", "Microsoft Visual Studio Code"),
            ("wsdk", "Windows Software Development Kit"),
        ] {
            assert!(
                Matcher::new(query).app_score(title).is_some(),
                "{query:?}: {title:?}"
            );
        }
        for title in [
            "Windows Software Development Kit",
            "WindowsStandaloneSupport",
            "n___o___t___e___p",
            "n____o____t____e____p",
            "D:\\Workspace\\Project\\QuanTrading\\Code\\terminal-rs — Windows PowerShell - Otty",
        ] {
            assert!(Matcher::new("notep").score(title).is_none(), "{title:?}");
        }
        assert!(
            Matcher::new("visual code")
                .score("Visual Calendar")
                .is_none()
        );
        assert!(
            Matcher::new("vcs")
                .app_score("Visual Studio Code")
                .is_none()
        );
    }

    #[test]
    fn exact_and_omitted_word_matches_rank_above_typos() {
        let title = "Visual Studio Code";
        let exact = Matcher::new("visual code").score(title).unwrap();
        let typo = Matcher::new("visaul code").score(title).unwrap();
        assert!(exact > typo);
    }

    #[test]
    fn bounds_do_not_match_silently_truncated_queries() {
        let query = "a".repeat(MAX_BYTES + 1);
        assert!(Matcher::new(&query).score(&query).is_none());
        let query = "word ".repeat(MAX_WORDS + 1);
        assert!(Matcher::new(&query).score(&query).is_none());
        assert_eq!(Matcher::new("").score("Anything"), Some(0));
    }
}
