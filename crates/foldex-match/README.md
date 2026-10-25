# foldex-match

Dependency-free bounded matching shared by directory views, indexed search, and
visible row highlights. Buffers are allocated during query preparation; `score` performs no
per-candidate heap allocation. The ASCII path reads the original byte slice.
Unicode candidates decode once into at most 1024 stack-resident code points.

```rust
use foldex_match::{Matcher, MatchMode, TokenMode};

let matcher = Matcher::new("dsg");
assert_eq!(matcher.score("Design"), Some(141));
assert_eq!(matcher.match_indices("Design"), Some(vec![0, 2, 4]));
let substring = Matcher::new("sig").with_mode(MatchMode::Substring);
let all_words = Matcher::new("des notes").with_token_mode(TokenMode::All);
```

`match_indices` returns sorted unique UTF-8 **byte offsets** into the original
candidate. Compute it for visible rows only. Empty queries match with score zero;
successful matches retain negative scores. Consumers must not test `score > 0`
to decide whether the match succeeded. Ties have no ordering contract.

Both query and candidate inspect a prefix of at most 1024 bytes, rounded down to
a UTF-8 character boundary. This bounds scan/decode work for unusually long
paths. It deliberately excludes later matches; a full-path search product can
apply this matcher separately to bounded filename/path components. The query
exposes its bounded text and `is_query_truncated` flag.

Fuzzy scores, the simple Latin/Cyrillic fold map, camel/space/underscore bonuses,
and the shared alternate-call threshold come from the retained static evidence
in `../../docs/file-pilot-search-evidence.md`. Unicode case folding is limited
to those source ranges; no normalization or multi-code-point expansion occurs.
Fuzzy work is bounded by nine executed searches over the bounded candidate,
plus inexpensive rejected alternate calls, with recursion depth at most nine.
ASCII scanning uses borrowed slices and eight-byte case-insensitive searches;
it never initializes a1024-element candidate array. Short substring patterns
use bounded byte comparisons and calculate boundary bonuses only for full hits.
Longer patterns and Unicode use precomputed KMP links with linear scanning. Multiple
explicit tokens each reuse the decoded candidate and have independent budgets.

`fold_key`, `fold_codepoint`, and `bounded_prefix` expose the same normalization
and UTF-8 boundary convention for indexing. Use these rather than a different
Unicode lowercase map when constructing postings or query keys.

Default `TokenMode::Literal` treats spaces as part of the pattern. `Any` sums
matching tokens; `All` requires every token and sums their scores. These token
policies and substring mode are explicit reconstruction choices; the source's
caller-specific multiword behavior remains partially observed. No native source
assets or original source code are included.
