# Foldex app improvement audit

Foldex's highest value next work is workspace durability, current search results, and independent interactive actions during file operations. These affect whether people can trust the explorer with an ongoing workspace. The strongest new feature opportunity is saved, structured searches built on that foundation.

This audit covers commit `fa372957b280ba2733092b24a01efa0ac6907296` in the separate `codex/app-improvement-audit` worktree. It traces browsing and freshness, indexing and paging, file operations and undo, window and session ownership, launcher providers, previews and thumbnails, folder statistics, navigation, and the accessibility tree. The application already has bounded workers, latest-request cancellation, visible-row rendering, native folder notifications, protected undo, and paged search results. Those are useful foundations to preserve.

Seven focused probes reproduced behavioral limitations using the application code. Additional probes exercised a real 150,000-file directory and a synthetic 100,000-row catalog. All measurements below are Linux `dev` builds with the repository's optimization settings, using recently written, warm fixtures. They establish mechanisms and local costs; Windows release timing, native Shell interaction, DX12 behavior, screen-reader interaction, and OneDrive provider behavior need Windows validation. The cloud compatibility finding is based on the Windows source paths.

| Order | Recommendation | Main benefit | Relative scope |
| --- | --- | --- | --- |
| 1 | Separate shared preferences from window workspaces and make save limits recoverable | Prevent lost settings and unsaved workspaces | Medium |
| 2 | Maintain one live catalog per search scope | Correct external-change results; reduce repeated disk scans | Large, with a smaller freshness first step |
| 3 | Give interactive Shell actions an independent worker lane | Open files and apps while copies and moves run | Medium |
| 4 | Schedule visible panes fairly within resource limits | Remove indefinitely pending searches in split workspaces | Medium |
| 5 | Continue large folder listings on disk and make deep paging predictable | Complete results with bounded memory and fewer latency spikes | Large, staged |
| 6 | Add structured searches and saved collections | Turn repeated finding and cleanup tasks into reusable workflows | Medium after the search foundations |
| 7 | Expose the virtual file listing through accessibility semantics | Make keyboard browsing usable through assistive technology | Medium |
| 8 | Distinguish cloud placeholders from redirecting links | Improve Windows cloud-folder search, previews, and totals | Medium to large; Windows validation required |

Scope estimates describe the number of interacting subsystems, rather than delivery dates. The evidence and acceptance criteria below should drive the size of each implementation.

**1. Make workspace persistence safe across windows and normal feature limits**

A regular New Window uses the same `foldex-session.json` as the owner. The auxiliary writer takes a cross-process lock and preserves the owner's `launcher` field, then replaces the rest with its own snapshot. Atomic replacement protects file integrity, but an older auxiliary snapshot can still replace newer bookmarks, shortcut bindings, and workspace state. The probe exercised the real writer with owner and auxiliary snapshots: the newer launcher survived; the newer bookmark, shortcut, and workspace did not. Detached-tab windows use a separate temporary state directory, so their shared-preference ownership also needs an explicit policy.

There is a second failure in the same workflow. Four tabs with 64 navigation entries each and three saved layouts serialized to **339,555 bytes** in the probe. That fits the application's tab/history/layout features but exceeds the writer's **262,144-byte** limit. Saved layouts currently duplicate navigation history. A serialization/queueing error in `service_session` is retried after 50 ms, including permanent oversize errors, so a workspace that cannot fit can also cause repeated serialization, repainting, and error notifications.

Separate global preferences, per-window workspace state, and disposable navigation history. Have the primary process own shared preference mutations through the existing instance/IPC infrastructure, or use explicit revisioned field updates under the existing lock. Keep atomic replacement. Store only the information needed to reopen a named layout, bound expendable history by serialized size, and handle permanent save failures with a stable recovery action. Retrying a busy writer should have a different policy from exceeding a persistent size limit.

Acceptance: edits in two regular windows survive saves and closes in both orders; detached-window preference changes have defined ownership; the four-tab/three-layout fixture remains saveable; an oversized or failed write preserves the last good workspace and stops automatic 50 ms retries.

Sources: [startup and window ownership](../../crates/foldex/src/main.rs#L151), [auxiliary merge](../../crates/foldex/src/persistence.rs#L107), [session writer limit](../../crates/foldex/src/persistence.rs#L58), [layout state](../../crates/foldex/src/workspace.rs#L127), [save scheduling](../../crates/foldex/src/app.rs#L3546). Evidence: `multiwindow_session`, `session_budget`.

**2. Give recursive search a shared, live catalog**

Ordinary folder browsing already receives native change notifications. Expanded search disables that browser activity and uses `SearchHub` instead. Once its backend is complete, there is no filesystem-change step that reconciles the completed catalog. The probe completed a one-file search, created another file externally, and requested the same search again. It still returned one result, marked complete and current. An explicit refresh returned both files. Changes performed through Foldex can trigger its own refresh path; the reproduced gap concerns external changes such as builds, downloads, or edits in another application.

Conversely, hiding and reactivating an unchanged search opens its cached catalog and then creates a new generation and traverses the scope again. Cached data accelerates first publication, but it does not prevent the full rescan. Separate clients can also build separate generations for the same scope. This adds unnecessary I/O to tab switching and overlapping explorer/launcher searches.

Make the scope, rather than the individual client, own catalog lifetime and change tracking. Browsers and launcher queries should subscribe to one generation with separate query state. A first step can detect subtree changes, mark affected results stale, and reconcile dirty directories in coalesced batches. A longer-term NTFS path can use journal checkpoints and a bounded delta overlay; other filesystems need a truthful watcher/rescan fallback. Watcher overflow, journal discontinuity, permissions changes, and volume replacement must force reconciliation. Root-directory modification time alone cannot validate descendants.

Acceptance: external create/rename/delete changes appear in an open search; a catalog is never labelled current while known changes remain unprocessed; unchanged scope reactivation avoids a full traversal when change tracking is valid; overlapping clients share scan work; selections remain tied to a valid generation during updates.

Sources: [recursive browser handoff](../../crates/foldex/src/pane.rs#L1162), [cached and completed backend transitions](../../crates/foldex/src/search.rs#L890), [completed-result replay](../../crates/foldex/src/search.rs#L946), [native browsing watcher](../../crates/foldex-core/src/watch.rs#L1). Evidence: `search_freshness`, `unchanged_scope_rebuild`.

**3. Keep opening files and applications responsive during transfers**

`Platform` puts copy/move/delete jobs, `Open`, application activation, terminal opening, new windows, and Folder Options on the same eight-slot FIFO worker. Preview, path resolution, Properties, launcher activation, persistence, and integration already have independent paths, which shows the architecture supports this separation. A long native transfer or modal operation can still delay a double-click, launcher application result, or terminal request behind it. When the queue fills, those actions receive a generic busy error.

The controlled probe held the shared mutation journal, queued a folder mutation and an Open request, and observed no result until the gate was released. Both results arrived afterward. This demonstrates the shared queue dependency without claiming a Windows copy-duration measurement.

Add a small, bounded interactive Shell lane with its own COM apartment and message pumping. Preserve serialized mutation history and ordering on the mutation lane. Capture clipboard cut/copy requests through an appropriate independent path so they remain useful while a transfer runs. The next product increment can be a compact operation center exposing job identity, source/destination, queued/running/completed/partial status, and per-job cancellation, using the existing native progress callbacks and conflict dialogs. Completed and partial outcomes deserve a durable place beyond the current toast.

Acceptance: a deliberately blocked transfer does not delay opening an unrelated file, launching an app, opening a terminal, or creating a new browser window. Cancellation reports partial work accurately and maintains the existing undo protections. Both worker counts and operation records remain bounded.

Sources: [job routing](../../crates/foldex/src/platform.rs#L302), [single operation consumer](../../crates/foldex/src/platform.rs#L497), [Shell progress integration](../../crates/foldex/src/platform/windows_ops.rs#L802), [mutation journal](../../crates/foldex/src/operation_history.rs#L546). Evidence: `operation_queue`.

**4. Treat the worker limit as a concurrency limit, not a permanent admission limit**

The workspace supports multiple visible groups, but the search worker sorts all active clients by request recency and truncates the list to two. Completed clients remain eligible and occupy both contexts. The worker then sleeps without admitting an older waiting client. With three active clients, the probe's oldest search received no event even after the two newer searches completed. It completed when a newer client was deactivated. The query was pending, rather than slow.

The details coordinator also has two view leases. It explicitly pauses additional panes and promotes the focused pane, so folder statistics can remain paused in a visible split workspace. These are resource policies, not rendering limitations.

Keep a bounded number of running scans/calculations, but maintain a fair queue of visible scope jobs. Rotate bounded work turns and release running permits on completion. Keep the minimum catalog lease needed to protect selected generations independently of scheduling priority. Give the focused pane a latency advantage while guaranteeing eventual progress for other visible panes. Share identical scopes as part of recommendation 2. Show a real queued state when capacity is occupied.

Acceptance: three and four visible panes with distinct scopes all publish and finish without requiring a user to edit another query or hide a pane. Repeat with the launcher open, a slow scope, and a pinned selection. Verify retained model bytes, handle counts, and active scan workers stay within explicit budgets.

Sources: [search admission and idle loop](../../crates/foldex/src/search.rs#L1506), [details leases](../../crates/foldex/src/pane/columns/worker.rs#L102), [details capacity feedback](../../crates/foldex/src/pane/columns.rs#L1484), [multiple visible groups](../../crates/foldex/src/app/workspace_canvas.rs#L105). Evidence: `three_active_searches`.

**5. Make bounded browsing complete and smooth beyond its fast path**

There are two distinct limits to address. Ordinary directory enumeration stops at its model reservation budget. In a generated directory containing 150,000 empty files with 121-byte names, it returned **126,542 entries** with `MemoryBudget`. An omitted file existed on disk, but an exact local filter returned zero matches. The footer does expose a scope-limit status; there is no continuation path for that ordinary listing. The threshold varies with names and platform, and the application's shared model budget can constrain concurrent views further. The managed reservation includes query/order headroom and is not a process-memory measurement.

The disk catalog handles larger scopes, but caches only a bounded head of 1,025 result keys. The first arbitrary page beyond that head materializes the full matching order in SQLite and writes a flat result file. The current UI starts each new result window with an offset query, so this path is reachable during browsing, even though continuation pages inside a window use cursors.

For query `report`, all 100,000 synthetic rows matched. Five independent runs produced these median query times:

| Page | Offset | Median |
| --- | --- | --- |
| First page | 0 | 10.453 ms |
| Within cached head | 768 | 0.074 ms |
| First page crossing head | 1,024 | 116.783 ms |
| Later page after materialization | 50,000 | 0.104 ms |

The empty-query crossing median was 99.172 ms. This is result-delivery latency on the background worker, not a measured UI-thread freeze.

Preserve the current in-memory directory fast path and continue oversized listings through a nonrecursive disk-backed store. A smaller initial release could offer an explicit continuation into an indexed listing when the cap is reached. Reuse paged selection and actions while preserving ordinary view sorts and metadata. For search paging, retain keyset continuity across neighboring windows, prepare common unfiltered orders once per generation, and build arbitrary-query sort materializations in cancellable, prioritized stages. Bound their disk footprint as well as RAM.

Acceptance: every file remains reachable and filterable in a directory exceeding the in-memory admission budget; counts distinguish partial and complete coverage; sequential scrolling and distant thumb jumps are measured separately; the first deep page does not become a full blocking sort on the shared query turn. Establish Windows release latency targets on a fixed reference machine before choosing a cache strategy.

Sources: [directory admission](../../crates/foldex-core/src/scan.rs#L106), [global model budget](../../crates/foldex-core/src/engine.rs#L43), [result head and deep-page branch](../../crates/foldex-index/src/catalog/index.rs#L216), [sort materialization](../../crates/foldex-index/src/catalog/spool.rs#L81), [window requests](../../crates/foldex/src/search.rs#L1062). Evidence: `large_directory`, `catalog_paging`.

**6. Add structured search and saved collections as the next substantial feature**

The index already stores parent paths, size, modification time, extension-related data, and file/folder flags. The public query supports filename text, match mode, visibility, kind, type filters, and ordering. It does not express path constraints, date/size ranges, or subtree exclusions. Matching is applied to the basename. Saved folder views retain presentation settings, and the persisted routes are Home, Recycle Bin, and Folder; there is no saved search route containing a query and its scope.

A useful feature slice is a visual query builder with a compact optional syntax and named sidebar collections. Examples: PDFs under Downloads modified in the last seven days; build artifacts over 100 MiB under a selected project; filenames under `src/` while excluding selected generated trees. Scope traversal exclusions should be explicit and user-controlled, with visibility into coverage. Relative dates should be evaluated when the collection opens. These workflows use existing metadata without requiring file-content indexing.

Introduce one structured predicate model that each backend can evaluate before expensive fuzzy scoring. Preserve each surface's existing name-matching policy. Persist the scope and predicates as a first-class navigation route so Back, Forward, reopen, and saved workspaces retain the search. Provide editable chips and a clear explanation of which conditions matched.

Acceptance: a saved collection survives restart with identical scope and predicates; path/date/size/type combinations agree between ordinary and indexed views on generated fixtures; excluded trees avoid traversal when configured as scope exclusions; counts and selection actions refer to the same result revision.

Sources: [query fields](../../crates/foldex-index/src/model.rs#L266), [basename evaluation](../../crates/foldex-index/src/catalog/search.rs#L92), [navigation routes](../../crates/foldex/src/navigation.rs#L8), [saved view and pane state](../../crates/foldex/src/workspace.rs#L9), [scan exclusions](../../crates/foldex-index/src/model.rs#L373).

**7. Expose filenames and selection through the accessibility tree**

AccessKit is enabled in the application, and custom menus and controls register accessible roles and labels. The main file listing paints its rows manually. Its focus surface reports the generic label `Files`; the visible filename, selected state, item position, and row actions are not exposed as file-list items.

The headless probe enabled AccessKit and rendered a focused listing containing a selected file for three frames. It generated 21 nodes in total, with `Files` as the only labels and no filename label. This establishes a missing tree representation; a Narrator or NVDA end-to-end assessment still needs Windows.

Give the virtual collection a suitable list/grid/table role, expose visible and focused rows with stable identities, and publish filename, selected state, position, and useful metadata. Handle focus, activation, and scroll-to-item accessibility actions through the same command paths as keyboard input. Keep the accessibility representation virtualized, including a path to request an offscreen focused row.

Acceptance: Narrator and NVDA can identify, select, open, and rename a file in details and icon views, including after scrolling, filtering, and result updates. Accessibility node count follows the viewport/focus budget rather than directory size.

Sources: [listing interactions and painting](../../crates/foldex/src/pane/listing.rs#L499), [generic collection focus](../../crates/foldex/src/keyboard.rs#L204), [listing focus surface](../../crates/foldex/src/pane/keyboard.rs#L326), [existing menu semantics](../../crates/foldex-ui/src/components.rs#L2837). Evidence: `listing_accessibility`.

**8. Use one cloud-aware filesystem capability policy**

Several Windows paths treat every `FILE_ATTRIBUTE_REPARSE_POINT` as a redirecting link. The search walker does not descend into those directories. The Inspector returns its special-item message for those files, and thumbnail/statistics policies reject reparse ancestors. Consequently, cloud-provider files or directories that retain reparse metadata can lose functionality even when data is locally available. The exact affected cases depend on the provider and hydration state.

The undo implementation already contains a more precise distinction: it checks the name-surrogate tag bit and explicitly treats cloud directories as traversable trees. That useful policy has not been shared with browsing services. This is a concrete inconsistency within the application.

Introduce a shared classification describing ordinary files, redirecting links, cloud placeholders, availability, network/removable location, and permitted metadata/content operations. Apply it consistently to search, Inspector, thumbnails, and statistics. Preserve the existing protection against background downloads: enumerate safe metadata where supported, use cached thumbnails for unavailable content, and make downloading/opening a deliberate user action. Surface availability when it changes what a user can do.

Acceptance: a Windows matrix covering hydrated files, online-only files, cloud directories, symlinks, junctions, UNC paths, and removable storage produces consistent results. Verify that browsing or searching never triggers an unintended hydration. Include a real OneDrive provider test before claiming compatibility.

Sources: [filesystem attributes](../../crates/foldex-fs/src/lib.rs#L16), [search traversal](../../crates/foldex-fs/src/parallel.rs#L251), [Inspector classification](../../crates/foldex/src/inspector.rs#L203), [thumbnail ancestor policy](../../crates/foldex-assets/src/windows.rs#L670), [statistics ancestor policy](../../crates/foldex-folder-stats/src/native.rs#L170), [existing cloud distinction in undo](../../crates/foldex/src/operation_history.rs#L218).

**Implementation sequence**

Start with workspace ownership and save recovery, independent interactive actions, and fair search admission. They address reproduced failures without first changing the catalog format. Build the shared live-scope service next, then extend it to oversized ordinary listings and predictable paging. That provides the base for saved collections. Accessibility and cloud classification can proceed as focused work with explicit Windows acceptance tests. The search changes should share one scope/catalog design so each feature does not introduce another scan owner.

**Validation and reproduction**

The core/index/matching/filesystem/folder-statistics baseline passed **104 tests**; two opt-in stress/microbenchmark tests remained ignored. The unchanged application passed **578 applicable tests** after excluding two Windows-specific checks on Linux. The unfiltered Linux app run also demonstrated those two platform limitations: Windows verbatim/UNC path assertions use Linux path semantics, and native catalog cleanup deliberately retains generations on Unix. Neither result establishes a Windows application defect.

All **seven behavioral probes passed**, meaning they reproduced the conditions described above. The directory probe confirmed the count cap and filter omission. The catalog benchmark contains 60 timed page queries across five fixture runs and three query texts. Raw results, environment information, and probe hashes are in [evidence.json](evidence.json). These probes assert the audited behavior; fixing a problem can intentionally make its corresponding probe fail. They are kept outside the normal application tests and CI.

From the repository root, with the pinned Rust toolchain available:

```sh
python3 docs/app-audit/run.py --mode probes
python3 docs/app-audit/run.py --mode catalog --catalog-runs 5
python3 docs/app-audit/run.py --mode directory
```

Add `--offline` when the locked dependencies are already cached, and `--output /path/to/results.json` to save a new run separately. The runner temporarily appends the probes or creates Cargo examples and restores its additions afterward. Run it in an isolated worktree. Directory mode creates and, on success, removes 150,000 empty files under an owned system-temporary directory. Catalog timings use synthetic metadata, rather than scanning an actual 100,000-file tree.

The portable application baseline command used here was:

```sh
cargo test --locked -p foldex --no-default-features --features renderer-glow -- \
  --skip launcher::tests::default_roots_do_not_scan_verbatim_home_twice \
  --skip search::tests::repeated_unfinished_refreshes_do_not_accumulate_owned_generations
```

On Windows, run the normal workspace test suite and the native acceptance checks without these Linux exclusions. No application behavior or release configuration was changed by this audit.
