# Foldex

| | |
| :--- | :--- |
| **ID** | `foldex` |
| **Role** | Ours |
| **Version tested** | Release build version plus git commit, in `apps.json` |
| **Install type** | **Release installer build** (`Foldex-VERSION-windows-x64-setup.exe`), the same artifact users get. Built with the release profile (`lto = "thin"`, `codegen-units = 1`). Not a dev build. |
| **UI technology** | egui 0.36 + wgpu (DX12), winit window, AccessKit for UIA |
| **Main window class** | winit class name ⚠ (record in the pilot) |

## What the codebase tells us

These facts decide how Foldex is measured:

- **Listing**: `foldex-fs` enumerates with `FindFirstFileExW(FindExInfoBasic,
  FIND_FIRST_EX_LARGE_FETCH)` on background workers. Listings publish
  snapshots. A snapshot is `complete` only after traversal finishes without a
  limit (`foldex-core/src/model.rs`).
- **Limits**: direct listings have no entry cap but a **64 MiB memory budget**
  (`DEFAULT_DIRECTORY_BYTES`). Recursive scans stop at **250,000 entries /
  32 MiB** (`DEFAULT_SCAN_ENTRIES`, `DEFAULT_SCAN_BYTES`). A listing that hits
  the budget shows "scope limit" and offers **Load all items**.
  **`F-1M` and `T-250k` may hit these limits**, and the pilot must check
  this. A limited listing is **not** "fully listed". The run either clicks
  Load all (the time it takes is included) or fails `count_ok`.
- **Icons and thumbnails**: `IShellItemImageFactory` with
  `SIIGBF_INCACHEONLY` first, then a full extraction on a background worker. It
  shares the Windows thumbnail cache with Explorer.
- **Search**: `foldex-index` walks the scope itself (`parallel_walk`) and builds
  a catalog. It does **not** use the Windows Search index or the NTFS MFT/USN
  journal. The first search in a scope is a full walk; later queries in the
  same scope reuse the catalog. Both are measured.
- **Filter**: current-folder filter in the footer (`foldex-match`, fuzzy and
  substring modes). The default mode is recorded.
- **Resident mode**: one tray **background owner** per installation and user
  (`launcher/instance.rs`). A second `Foldex.exe <path>` hands the folder to
  it with `WM_COPYDATA` before any GPU or window setup. Whether the tray owner
  stays running by default depends on the launcher "background" setting. The
  pilot records the default.
- **Existing harness**: `--benchmark-ui`, `--benchmark-search`,
  `--benchmark-navigation-list` and others already exist. They run with
  isolated state and report in-process timings. They are **useful for
  diagnosing Foldex itself**, but they are not used for comparison numbers,
  because they time from inside the process and change startup (isolated state,
  automatic exit).

## Profile reset

Installed copies keep state in `%LOCALAPPDATA%\Foldex\data`. The fresh
`fxbench` account starts empty. Within a series, back up `data` after
first-run and restore it when a scenario needs factory state.

## Allowed deviations applied

- Automatic update checks off (Options → Application Updates).
- **Details view** for fixture folders.
- **The automation channel** (below), as a report-only hook.

## Launch

| Mode | Command | Notes |
| :--- | :--- | :--- |
| Cold | No Foldex process running → `Foldex.exe "D:\FoldexBench\F-100"` | Process create → GPU setup → window |
| Resident | Tray owner running, no window → `Foldex.exe "<path>"` | Handoff through `WM_COPYDATA` |
| Close | Automation `close`, or `WM_CLOSE` | |
| Kill everything | `taskkill /f /im Foldex.exe` (tray owner included) | |

## Automation channel (to build)

Foldex needs a small opt-in channel so the harness can drive it and hear when
it is ready. That gives Foldex the kind of signal Explorer already provides
through its COM object model.

- Enabled only by `--automation-pipe \\.\pipe\fxbench-<run>`. When the flag is
  absent, nothing changes. It must **not** turn on the `--benchmark-ui` isolated
  state or automatic exit.
- **Commands in** (JSON lines): `navigate`, `back`, `sort`, `view`, `new_tab`,
  `select_all`, `focus_filter`, `clear_filter`, `search`, `load_all` (the
  footer's **Load all items** action), `close`.
- **Events out**, written as soon as the condition holds in a frame that has
  been **presented**:
  - `first_item {path, count}`: active pane snapshot has ≥ 1 entry for the target path.
  - `listed {path, count, complete, limit}`: `snapshot.complete &&
    displayed_view_is_current()` (both already exist in `pane.rs` /
    `app.rs::service_benchmark`).
  - `sorted {key, first}`
  - `filtered {query, count}`: `rendered_query == query` and the view count is final.
  - `search_done {query, count, complete}`: the search status is no longer
    `Building`, and `total` is known (the same condition `performance::search` uses).
- Events contain **no timestamps**. The harness stamps them when they arrive.
- The pilot checks that events fire within one frame of what capture shows,
  and that enabling the channel moves no S1 or S2 median by more than 3%.

Until the channel exists, the fallbacks are UIA on the footer status text
("0 selected / N items", or "Loading…" while loading; see
`pane/footer.rs`), if AccessKit exposes it ⚠, and capture.

## Readiness signals

| Metric | Signal | Source |
| :--- | :--- | :--- |
| `t_first_item` | `first_item` event | Hook |
| `t_listed` | `listed` event with `complete: true` and `count == expected` | Hook, cross-checked with UIA footer text |
| `t_sorted` | `sorted` event, `first` matches the manifest | Hook |
| `t_filter_key` | `filtered` event for each prefix | Hook |
| `t_search_done` | `search_done` event, `complete: true`, `count == expected` | Hook |

## Resource attribution

Every `Foldex.exe` process in the session. The tray owner and any window
process count together (they may be the same process). Include the network
helper if it is ever started (it should not be in these local-only scenarios;
if it starts, that is a finding).

GPU memory **counts**: Foldex renders through DX12, and its swap chain and
texture atlases are real costs. They are reported as `mem_gpu_*` next to the
CPU-side memory, not hidden.

## Scenario notes

- **S1**: both rows, cold and resident (tray).
- **S4**: report the tray-only idle footprint (`S4.background`).
- **S5 search**: report the first walk (cold catalog) and repeat queries (warm
  catalog) as separate rows. Never show only the warm one.

## Caveats

- The first launch on a machine compiles DX12 pipelines and may build caches.
  The `cold-boot` and `cold-fs` rows include that cost. Don't warm it away.
- The limits above are a deliberate design choice. If they make `F-1M` or
  `T-250k` incomplete by default, publish that alongside the numbers.

## Verify in pilot

- [ ] Default for keeping the tray owner running (`session.launcher.background`).
- [ ] Whether `F-1M` direct listing fits in 64 MiB, or hits `MemoryBudget`.
- [ ] Whether `T-250k` search hits the 250k recursive cap (the fixture's 250,000 items sit right at it, plus the root).
- [ ] Whether hidden files are shown by default (Windows folder options or Foldex's own setting).
- [ ] Main window class name; AccessKit exposure of the footer text through UIA.
- [ ] Default filter match mode (fuzzy or substring) and whether its counts match the manifest's name-contains counts.
