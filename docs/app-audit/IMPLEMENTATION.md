# Foldex improvement implementation record

Implementation branch: `codex/app-improvement-audit`. Baseline: `fa372957b280ba2733092b24a01efa0ac6907296`; original measurements and probes are preserved in `evidence.json`.

Each stage receives a focused implementation, edge-case tests, a diff review, and a separate commit before moving to the next stage. The final PR will report the integrated checks and comparable before/after results. Native Windows validation is recorded separately from portable logic and Linux measurements.

| Stage | Scope | Status |
| --- | --- | --- |
| 1 | Shared preferences, independent window state, compact saved layouts, recoverable save limits | Complete; reviewed; 592 applicable app tests pass |
| 2 | Shared live search catalogs, freshness, and catalog reuse | Complete; reviewed; 603 applicable app tests pass |
| 3 | Independent interactive Shell actions and operation status | Complete; reviewed; 611 applicable app tests pass |
| 4 | Fair scheduling for visible searches and details | Complete; reviewed; 615 applicable app tests pass |
| 5 | Complete large listings and predictable deep paging | In progress |
| 6 | Structured search and saved collections | Pending |
| 7 | Accessible virtual file listings | Pending |
| 8 | Shared cloud-aware filesystem capabilities | Pending |

The implementation must preserve raw file identities, bounded worker and memory policies, latest-request cancellation, generation-pinned selections, native conflict handling, protected undo, and atomic settings writes. Tests cover failure and recovery paths as well as successful actions.

## Stage 1

Shared preferences now use a versioned document, a cross-process lock, and three-way field changes. Independently edited bookmarks, shortcut commands, layouts, views, and nested settings merge without reverting unchanged fields. Membership deletions and Windows lexical path identity are preserved. Concurrent collection overflow is reported rather than dropping an existing writer's data. Frequency history uses idempotent maxima and the newest timestamp. Atomic writes retain a previous preference document, and corrupt/future documents are preserved with a visible startup error.

Primary and auxiliary windows save independent workspace documents. Preference updates are observed on the sleeping persistence worker and applied around unsaved UI edits; navigation remains local. Named layouts retain destinations and views while removing unrelated navigation stacks. Compact serialization reduced the original four-tab/three-layout fixture from **339,555 bytes to 74,002 bytes**; stored workspace and preference documents total 74,044 bytes including the version envelope. The maximum state size is consistently bounded at 4 MiB, and only transient queue contention triggers a 50 ms retry.

Review fixes included preserving shortcut sequences as atomic arrays, keeping remote bookmark additions during local reordering, avoiding preference-acknowledgement save loops, and closing a stamp race immediately after a concurrent writer commits. Regressions cover both save orders, simultaneous writers, failed workspace writes after a successful preference merge, invalid documents, capacity failures, live refresh with local edits, and permanent-error UI behavior.

Validation: `cargo fmt --all -- --check`; **592 passed**, two documented Windows-only checks excluded on Linux. Native Windows checks are still required and will be recorded with the integrated results.

## Stage 2

Scopes now own a shared live catalog and watcher, independently of explorer and launcher requests. Active subscribers share traversal; cancelling one subscriber does not cancel another's scan. Eight scope caches/watchers and two concurrent traversals bound resources. Native subtree changes invalidate results and trigger coalesced reconciliation. Fallback polling never reads file contents and receives periodic full validation; watcher errors, root replacement, and notification loss reattach and reconcile. A healthy unchanged scope reuses its generation across tab reactivation. Initial results can still use a saved cache, while a process restart always validates it. Frequent changes are spaced to avoid continuously restarting full traversals; stage 5 will extend the disk backend's incremental and paging work.

Selected generations reopen through independent read leases and remain immutable while unselected results refresh. The footer explains why selected results are held. Cache paths are derived from the selected generation's actual scope and validated against the caller. Cleanup runs after completed refreshes and includes live/pinned leases. Review also fixed an inactive dirty-scope timer loop, disabled the watcher's default symlink traversal, excluded generated cache changes, and made directory replacement recoverable.

The external nested-create fixture published the changed result in **123.219 ms** on Linux (one observed run). Rename, deletion, and size changes with a preserved modification timestamp are covered. Four unchanged reactivations retained **one generation and zero additional scans**. Further tests cover independent cancellation, selection snapshots across a refresh, cache self-invalidation, symbolic links, file reads, watch-loss recovery, idle work, and scope eviction.

Validation: **603 applicable app tests passed**; **34 index tests passed**, two opt-in index stress/microbenchmarks ignored. The Windows GNU index target also passed `cargo check`. Formatting and diff checks passed. Windows runtime and full native-application checks remain part of integrated validation.

## Stage 3

Interactive Open, application activation, Reveal, Folder Options, external windows, consoles, and metrics use two bounded Shell workers independently of the mutation FIFO. Native calls initialize their own apartments. Clipboard writes and pastes retain their original ordering, and mutations still share the protected undo journal with batch actions.

The File Operations command and application-menu entry show a bounded 64-record history, queue/running/completion states, destination summaries, elapsed time, Windows estimated-work progress, per-operation cancellation, and clear-completed/cancel-all controls. Cancellation is checked before execution, after waiting for the journal, inside portable copy loops, and in native progress callbacks. Native callbacks retain a thread-safe operation token, so cancellation and progress do not rely on callback thread affinity. Confirmed child changes survive a failed parent outcome and are reported as partial completion; existing undo and native conflict/retry behavior remain authoritative. Operation history is intentionally local to the current window/process, not a durable recovery journal; batch rename retains its existing progress UI.

The original blocked-mutation fixture delivered **0 interactive events in 250 ms**. Its regression now returns the Open error **before releasing the mutation**, observed at 2.328–2.630 ms in portable runs. This measures queue independence with a missing-file Open, not Windows application startup. Additional tests cover queued cancellation, journal-wait cancellation, queue rejection without phantom records, bounded history/UTF-8 messages, cancellation racing success, native callback ownership, and rendering the operation dialog. Review caught partial-child changes and an application-menu grouping expectation after the new entry.

Validation: **611 applicable app tests pass**, formatting and diff checks pass. The full Windows GNU application and its test sources pass `cargo check`; native runtime interaction remains for Windows CI/manual acceptance. No runtime Windows latency is claimed.

## Stage 4

Search subscription state and selected-generation readers no longer reserve the two query turns. The coordinator rotates up to two productive turns, skips settled subscribers, and advances independently owned traversals. New requests retain their initial latency preference without permanently displacing older work. Up to 32 active subscriptions/scopes are supported; unused scopes still use an eight-entry cache. Excess subscriptions receive an explicit error. Running traversals remain capped at two (each retains the existing bounded filesystem-reader pool).

Details views now register bounded subscriptions rather than competing for two permanent leases or evicting each other on focus changes. The folder-statistics service already has round-robin queued-view admission and sliced walks; those mechanisms now serve up to 32 views with the same **four active walks**, 512 KiB request-path budget, and 64 cached tree results. Completion events remain coalesced per request. Generation cleanup accepts the larger bounded set of pinned readers.

Regression fixtures cover four distinct visible searches plus a launcher, a selected generation resolving actions while four other scopes run, eight detail views without focus evictions, twelve scopes finishing with at most two running traversals, and round-robin folder requests across six views. A Windows-native test additionally checks six real folder totals and traversal resource bounds. Five search subscriptions finished in **13.556 ms** in one portable fixture, including a 2,049-file scope; the baseline's older third client never published until another client was hidden. These are behavior/resource tests and local development timings, not a Windows release SLO.

Validation: the full portable app suite passed **614 tests**; the subsequently added twelve-scope resource test passed independently (**615 applicable app tests total**). All **7 portable folder-statistics tests** passed. Windows GNU application/test checks, formatting and diff checks passed. The native six-view test awaits Windows runtime CI.

## Stage 5, part A: complete directory continuation

A limited ordinary listing now offers **Load all items**, preserving its current filename/type/visibility query while continuing through a bounded paged catalog of direct children. Directory and recursive scopes use separate cache identities, watchers and traversal options. Loading all direct children never queues their subdirectories; Ctrl+E can subsequently switch to expanded search. File actions and selection use the existing generation-pinned paged implementation. The original in-memory directory and shared model budgets are unchanged. A generated limited-prefix pane regression finds an omitted file, excludes its similarly named descendant, preserves creation-date sort, and resolves Copy to the correct raw path.

Native directory batches and immutable catalog rows now retain creation/access dates, and those sorts work across head pages, cursor continuation and deep seeks. The disposable native format advances to v5 and older native caches rebuild. Legacy SQLite rows have no creation/access metadata and continue to represent those fields as unknown. Review caught ranking-heap replacement retaining an old item's new date fields; date-order tests cover both directions, unknown values, unique row coverage and deep-page agreement. Empty nonfinal pages now produce an explicit consistency error instead of a missing continuation cursor.

Validation for part A: **617 applicable app tests passed** and both new native-index continuation/date tests passed. Windows GNU compilation and integrated performance work are recorded in subsequent stage-5 entries. The real 150,000-file before/after fixture will be repeated after paging changes land.

## Stage 5, part B: bounded query work and cursor continuity

Adjacent result windows retain up to three source-validated cursor landmarks and cross the cached-head boundary without materializing the full matching order. Short pages from a bounded cached head are combined into a complete requested window. Different pane queries retain independent heads within a two-MiB archive budget, avoiding repeated head rebuilds when count facets or another subscriber changes the query. Decoded immutable rows remain shared within each reader.

Two bounded query workers handle uncached arbitrary seeks and expensive queries of at least 50,000 catalog rows, with four queued jobs. The coordinator continues to serve cached/small windows, launcher ranking turns, watchers and other scopes. Jobs pin an exact committed prefix, retain selected generations, cancel on supersession, and return latest-only windows. Full queues sleep until worker completion rather than spinning. Kind-facet counts are computed on those workers and reused before the viewport query so a warm ordered cache is not inadvertently discarded. Readers reopen when the manifest revision changes even if the row count does not.

Live-scope and owned selected-reader query payload caches each have a combined **16 MiB** trim budget. Each query worker retains one generation reader with at most about **6.5 MiB** of bounded head/row/spool payload caches (SQLite/descriptor/model allocations are separate). Each disposable ordered-cache database is capped at 256 MiB, with a 64 MiB flat result file; SQLite may also use transient sorting files. Directory-only scans use a single filesystem reader, since there is no descendant work to parallelize.

Review regressions prove neighboring windows do not create an ordered-cache file, query-cache eviction preserves cursor correctness, an occupied seek queue cannot reserve the coordinator, full queues produce no busy loop, newer requests replace old jobs, selected prefixes remain exact, a completed same-size manifest reopens correctly, and facet reuse preserves a completed ordered file. Query-worker completion uses a work notification that does not cancel unrelated in-flight queries.

Validation: **621 applicable app tests passed**, both Windows GNU application/test checks passed, and the native date/continuation/multi-query-head tests passed. Comparative full-window benchmarks remain pending until the integrated implementation is complete; the original single-page probe now receives a shorter cached-head page at the boundary, so its raw latency alone is not an equal-payload comparison.
