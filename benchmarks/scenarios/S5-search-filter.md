# S5: Search and filtering

| | |
| :--- | :--- |
| **Question it answers** | "I'm typing a name. How fast does the list narrow, and how fast does a deep search finish?" |
| **Headline metrics** | `t_filter_key` p50/max (filter); `t_search_done` (search) |
| **Fixtures** | `F-10k`, `F-100k` (filter); `T-250k`, `T-250k-indexed` (search) |
| **Cache states** | `warm`, `cold-fs` |
| **N** | 15 / 10 |

## Matching rules

Each app filters and searches with its own default matching rule (substring,
wildcard or fuzzy). A count is only correct relative to that rule. The
generator therefore writes expected counts **for each rule**:
`name-contains`, `name-glob` and `fuzzy-<algorithm>`. Each app file states
which rule its default mode uses.

The queries are chosen so that the **substring** count is meaningful for every
app (see the manifest `queries` / `filter_prefixes`). If an app's default is
fuzzy and its counts differ, that is reported as-is with its own expected
count, not "fixed" by changing modes.

## Variants

### Filter: current folder

| Variant ID | Fixture | Query | Typing |
| :--- | :--- | :--- | :--- |
| `S5.filter.10k` | `F-10k` | `report` | SendInput, 1 character every 150 ms |
| `S5.filter.100k` | `F-100k` | `report` | same |
| `S5.filter.100k.burst` | `F-100k` | `report` | 1 character every 30 ms (a fast typist) |
| `S5.filter.clear` | `F-100k` | after the filter is applied, `clear_filter()` | |

### Search: subfolders

| Variant ID | Scope | Query | Note |
| :--- | :--- | :--- | :--- |
| `S5.search.rare` | `T-250k` | `zq7` (5 hits) | Whole-tree walk, small output |
| `S5.search.common` | `T-250k` | `invoice` (hundreds of hits) | |
| `S5.search.huge` | `T-250k` | `e` (most items match) | Tests result handling, not just the walk |
| `S5.search.ext` | `T-250k` | `*.pdf` / extension filter | Each app's own extension syntax |
| `S5.search.repeat` | `T-250k` | `invoice`, then `report`, in the same session | The second query shows catalog or cache reuse |
| `S5.search.indexed` | `T-250k-indexed` | `invoice` | Explorer's best case, with the Windows Search index |

## Procedure

**Filter:**
1. Window at the fixture, Details view, idle.
2. `focus_filter()`.
3. Type the query with SendInput at the variant's spacing. Each keystroke's
   `t0` is its `SendInput` return.
4. After each keystroke, wait for the view count to match the manifest count
   for that prefix (or the next keystroke, whichever comes first, in burst
   mode).
5. Final: count stable for 500 ms and equal to the full query's count.
6. `clear_filter()`, wait for the count to return to the full folder.

Typing with SendInput is the one place synthetic input is used. The benchmark
desktop is locked to the harness during the run: no other windows, and focus
is confirmed through WinEvent before each keystroke.

**Search:**
1. Window at the scope's parent folder, idle.
2. `cold-fs`: purge the standby list first. For Foldex this also means a new
   session (catalog empty).
3. `t0`: `search(scope, query)`.
4. Wait for `search_done` with the count equal to expected.
5. `S5.search.repeat`: without closing, run the second query; record it
   separately as `warm-catalog`.

## Metrics recorded

| Metric | Notes |
| :--- | :--- |
| `t_filter_key` | Per keystroke; report p50 and max for each query |
| Filter final | `t0` of the last keystroke → final count stable |
| `t_search_first` | Results start appearing |
| `t_search_done` | **Headline for search** |
| `stall_max` | During typing and during search. A search that freezes the UI should show it. |
| `cpu_ms` | Total, including helper processes (Explorer's `SearchProtocolHost`, etc.) |

## Per-app notes

| App | Notes / `N/A` reason |
| :--- | :--- |
| `explorer` | **Filter: `N/A`.** No current-folder filter exists; the search box searches subfolders. Search: `search-ms:` navigation. Unindexed on `D:` (crawl) and indexed variant under the profile. |
| `foldex` | Filter: footer filter (default match mode recorded). Search: descendants search, with a fresh catalog for `cold-fs`. `T-250k` sits at the 250k recursive cap, so check `complete` in the event. |

## Pass / fail

Standard gates. For search, `count != expected` after completion is a failure.
A search that stops at a limit without saying so in the UI is a failure. One
that stops at a limit and shows it is recorded as "limited" and is not counted
as complete.
