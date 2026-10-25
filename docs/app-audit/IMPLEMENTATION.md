# Foldex improvement implementation record

Implementation branch: `codex/app-improvement-audit`. Baseline: `fa372957b280ba2733092b24a01efa0ac6907296`; original measurements and probes are preserved in `evidence.json`.

Each stage receives a focused implementation, edge-case tests, a diff review, and a separate commit before moving to the next stage. The final PR will report the integrated checks and comparable before/after results. Native Windows validation is recorded separately from portable logic and Linux measurements.

| Stage | Scope | Status |
| --- | --- | --- |
| 1 | Shared preferences, independent window state, compact saved layouts, recoverable save limits | Complete; reviewed; 592 applicable app tests pass |
| 2 | Shared live search catalogs, freshness, and catalog reuse | Complete; reviewed; 603 applicable app tests pass |
| 3 | Independent interactive Shell actions and operation status | In progress |
| 4 | Fair scheduling for visible searches and details | Pending |
| 5 | Complete large listings and predictable deep paging | Pending |
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
