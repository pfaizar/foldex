# Integrated improvement results

The eight stages in [IMPLEMENTATION.md](IMPLEMENTATION.md) are implemented in an isolated worktree. The original audit baseline is `fa372957b280ba2733092b24a01efa0ac6907296`. Current upstream `a3d2e8b` (including its drag/drop and hotkey changes) was merged before final validation. The original checkout was not edited.

## Reproduction and measurement limits

[compare.py](compare.py) temporarily appends probes, restores the source bytes on exit, and records every sample. Use separate worktrees for the baseline and implementation:

```sh
python3 docs/app-audit/compare.py --repo /path/to/baseline --cargo cargo --output before.json
python3 docs/app-audit/compare.py --repo /path/to/improved --improved --cargo cargo --output after.json
```

Results below are warm-file-cache **Linux development-profile** measurements (Rust 1.95, app optimization level 1, dependency level 2), on the same execution host. Five repetitions cover each query sequence and metadata refresh; directory loading is one observed fixture. No Windows release, frame-time, GPU, cold-disk or production percentile claim is made. Complete-window timings call the real window collector on one retained reader; they exclude UI/worker-queue dispatch. Production large/deep queries run on two bounded query workers.

Raw samples: [before](comparison-before.json), [accepted after](comparison-after.json), and [initial implementation trial](comparison-intermediate.json). The initial trial exposed repeated continuation ranking; the accepted implementation retains a bounded continuation block inside the existing two-MiB current-head budget. Plain queries also avoid preparing/evaluating unused metadata predicates.

## Complete result windows

100,000 synthetic records use the original `folderNNNN/report-NNNNNNNN.txt` distribution. Every timed result contains **768 rows**. The ordered sequence requests offsets `0`, `640`, `1024`, `50000`, and `50512`, retaining the same query/reader. Values are five-run medians in milliseconds.

| Query | First window: before → after | Offset 640: before → after | Offset 1024: before → after | First distant seek: before → after | Warm deep: before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| Empty | 7.473 → 8.456 | 9.249 → 9.592 | 98.712 → 2.007 | 3.188 → 107.847 | 2.188 → 2.136 |
| `report` | 11.494 → 13.887 | 18.324 → 15.094 | 123.196 → 2.104 | 3.179 → 127.053 | 2.107 → 2.147 |
| `rpt` | 10.163 → 12.841 | 16.149 → 13.844 | 117.278 → 1.913 | 3.213 → 121.466 | 2.194 → 2.193 |

The cached-head boundary no longer forces the first full ordered-cache build. **This defers sorting; it does not eliminate sorting.** The baseline has already paid that cost before the distant seek. The new implementation pays it at the first uncached arbitrary seek, on a query worker. Comparing only that distant-seek row would compare a warm baseline cache with a cold implementation cache.

| Cumulative sequence, median of each complete run | Before | After |
| --- | ---: | ---: |
| First three windows, empty query | 115.644 ms | 20.220 ms |
| First three windows, `report` | 153.034 ms | 31.011 ms |
| First three windows, `rpt` | 143.114 ms | 28.381 ms |
| All five windows, empty query | 120.932 ms | 131.558 ms |
| All five windows, `report` | 158.183 ms | 162.182 ms |
| All five windows, `rpt` | 148.355 ms | 154.940 ms |

Ordinary browsing through the boundary improves substantially. First-window latency and total work including a distant jump are **not universally faster**; the implementation also retains extra date/availability metadata. Ordered caches remain useful for arbitrary seeks and warm deep windows. These regressions are retained in the raw results and should inform Windows reference-machine tuning. No original single-page boundary latency is substituted for this equal-payload comparison.

## Production query-worker lane

The second probe uses `Context.step` and the production two-worker pool, polling pending work every 1 ms. Idle readers are shared across workers with exclusive ownership during jobs, preventing successive requests from reopening the same generation and losing its ordered cache. At most two readers remain retained; memory bounds are unchanged. [Before](coordinator-before.json), [accepted after](coordinator-after.json), and [the cache-locality trial](coordinator-intermediate.json) retain all samples. Reproduce with `compare.py --mode coordinator` and the same baseline/improved arguments.

For `report`, five-run medians:

| Phase | Complete window before → after | First coordinator turn before → after |
| --- | ---: | ---: |
| First window | 12.149 → 14.440 ms | 12.149 → 0.018 ms |
| Offset 640 | 19.400 → 15.182 ms | 19.400 → 0.017 ms |
| Offset 1024 | 122.945 → 2.500 ms | 122.945 → 0.013 ms |
| First distant seek | 3.294 → 129.598 ms | 3.293 → 0.012 ms |
| Warm deep | 2.355 → 2.484 ms | 2.355 → 0.019 ms |

The short coordinator turn measures dispatch, not frame latency. Query work still consumes time on a worker; the first distant sort remains deferred as described above. The accepted reader bank fixes the intermediate trial's warm-deep result (15.651 ms) and boundary window (18.181 ms), without adding workers or reader caches.

## Large folders and updates

The fixture creates 150,000 real empty files with 121-byte filenames. Both versions retain the same **126,542-row memory-limited prefix**, with a 49,367,721-byte logical model; the omitted file's ordinary filter returns zero. The improvement offers an explicit **Load all items** continuation through a paged direct-child catalog:

| Behavior | Before | After |
| --- | --- | --- |
| Total reachable/filterable direct children | 126,542 | **150,000** |
| Exact match for a file omitted from the memory prefix | 0 | **1** after continuation |
| Complete coverage available | No | Yes |
| Bounded-prefix enumeration, one run | 251.460 ms | 255.787 ms |
| Additional explicit catalog continuation, one run | Unavailable | 298.842 ms |

Continuation costs an additional pass; it preserves the original memory limit and never descends into child folders. Separate pane regressions cover filtering, dates, selection and file actions after continuation.

Changing one root file in that 150,000-file folder:

| Refresh work, five-run median | Before | After |
| --- | ---: | ---: |
| Refresh time | 218.671 ms | **57.198 ms** |
| Filesystem entries enumerated per refresh | 150,000 | **0** |
| Targeted changed-entry/ancestor metadata reads | Full traversal | **1** |
| Existing immutable shards reused | No | 74–78 hardlinked shards across the five deltas |
| Shard payload copied | Full rebuild | **0 bytes** |

The delta still scans private catalog metadata and performs root/cache/ownership checks. “1” counts the targeted metadata reads, **not all filesystem calls or constant-time computation**. No file contents are read by reconciliation. Unknown/lost notifications, unsafe ancestry, partial sources and compaction thresholds fall back to a full traversal. Existing selected generations remain independently readable.

## Durability, scheduling and accessible behavior

- The original four-tab/64-navigation-entry/three-layout state was **339,555 bytes** and exceeded the 256-KiB writer cap. With the final schema, the compact payload is **74,022 bytes**; the stored workspace/preferences total **74,064 bytes**, and the consistent cap is 4 MiB. The final schema's equivalent unpruned pretty JSON is 339,579 bytes. Concurrent shared updates merge without replacing another window's navigation; corrupt/future documents and previous preferences are retained.
- A blocked mutation originally prevented Open from producing any event during the 250-ms probe. The independent Shell lane produced the result before mutation release (2.328–2.630 ms in the portable missing-file fixture). This measures queue independence, not Windows application startup.
- Four panes plus a launcher finish without hiding newer subscribers. Twelve scope subscriptions retain at most two full traversals; details support 32 leases and four active statistics walks. Query and metadata lanes are independently bounded. Healthy unchanged reactivation reuses the catalog; notification bursts coalesce, and selected-only subscriptions pause further refreshes.
- The same one-file/three-frame AccessKit fixture changes from **21 nodes without its filename** to **27 nodes including the filename**. A separate 10,000-row test verifies bounded visible/focused semantics, raw identity, selection, positions, metadata, parentage, focus, reveal, stale-result rejection and activation across repeated render passes.
- Structured conditions agree across ordinary, native and legacy backends, including deep cursors and exact totals. Collections retain symbolic relative dates, traversal mode and scope through navigation, workspace restore and transfers. Calendar dates use `YYYY-MM-DD` in UTC; byte ranges use checked SI/binary units.
- Cloud-tag fixtures distinguish hydrated, online-only, redirecting and unknown items. Background previews guard ancestors and content availability; online-only thumbnails use cache-only requests. Statistics count cached logical sizes without reading offline contents. Raw case-sensitive siblings are preserved in catalog sharing and cache-notification filtering.

## Validation

- **934 portable workspace tests passed**, including **668 app tests**. Two opt-in index stress/microbenchmarks were ignored. Two existing Windows-only behavioral checks were excluded on Linux (verbatim-home lexical identity and native cross-process generation cleanup).
- Windows GNU compilation passes for the workspace and its test sources. Rust formatting, diff checks and all **23 CI/release-tooling tests** pass.
- Native Windows MSVC runtime CI is recorded on the PR. Actual Narrator/NVDA spoken interaction, real OneDrive/provider hydration callbacks, and Windows release frame/GPU performance remain manual acceptance items. Unit metadata fixtures and cross-compilation do not establish those behaviors.
