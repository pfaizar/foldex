# Harness (`fxbench`)

The tool that runs the suite. It drives each app through an **adapter**,
watches it with **observers** that are the same for every app, and writes
**results** in one shared format.

## Where it lives

`benchmarks/harness/`: a standalone Cargo package with its own empty
`[workspace]` table, so it never builds into or ships with Foldex. (The root
workspace includes `crates/*`; a package anywhere else under the root needs its
own `[workspace]` or Cargo refuses to build it.) Rust with the `windows` crate,
which the team already knows, plus the COM and UI Automation bindings it
provides.

```
benchmarks/harness/
  Cargo.toml
  apps.toml              registered apps → adapter + install path + version probe
  src/
    main.rs              CLI: fixtures | run | summarize | export | doctor | churn
    clock.rs             QPC timestamps
    fixtures/            deterministic generator + manifests
    observers/
      winevent.rs        window shown/destroyed, modal dialogs
      ping.rs            WM_NULL responsiveness pinger
      uia.rs             UI Automation property and text watchers
      process.rs         process-tree sampler (memory, CPU, handles, GPU via PDH)
      signal.rs          app hook channel (named pipe + file fallback)
      capture.rs         Windows.Graphics.Capture recorder (pilot / validation)
    adapters/
      mod.rs             trait FileManager
      explorer.rs
      foldex.rs
    scenarios/           S1..S5 runners, built from the adapter calls only
    stats.rs             median, p90, bootstrap CI, MAD flags
  hooks/                 report-only scripts/add-ins for apps that need them (one folder per app)
  purge/                 fxbench-purge: tiny elevated standby-list helper
```

## Adapters

Each adapter implements one trait. Scenarios call only these methods, so a new
file manager needs a new adapter and an `apps/<id>.md` file, and no scenario
code changes.

```rust
trait FileManager {
    fn id(&self) -> &'static str;
    fn version(&self) -> Result<String>;
    /// Remove profile/config/caches so the next launch is factory-fresh.
    fn reset_profile(&self) -> Result<()>;
    /// Install benchmark hooks (report-only) and apply allowed deviations.
    fn prepare(&self, deviations: &Deviations) -> Result<()>;

    fn launch_cold(&self, folder: &Path) -> Result<Session>;      // process not running
    fn open_window(&self, folder: &Path) -> Result<Session>;      // resident process
    fn navigate(&self, s: &Session, folder: &Path) -> Result<()>;
    fn back(&self, s: &Session) -> Result<()>;
    fn sort(&self, s: &Session, by: SortKey, descending: bool) -> Result<()>;
    fn set_view(&self, s: &Session, view: ViewMode) -> Result<()>;
    fn new_tab(&self, s: &Session, folder: &Path) -> Capability<()>;
    fn select_all(&self, s: &Session) -> Result<()>;
    fn focus_filter(&self, s: &Session) -> Capability<()>;        // then SendInput types
    fn clear_filter(&self, s: &Session) -> Capability<()>;
    fn search(&self, s: &Session, scope: &Path, q: &SearchQuery) -> Capability<()>;

    /// Readiness: what the view holds right now, as cheaply as possible.
    fn view_state(&self, s: &Session) -> Result<ViewState>;       // count, path, busy flag
    /// Processes whose resources count for this app (see metrics.md).
    fn process_set(&self, s: &Session) -> Vec<Pid>;
    fn close(&self, s: Session) -> Result<()>;
    fn kill_all(&self) -> Result<()>;
}
```

`Capability<T>` is `Supported(T) | NotSupported(&'static str)`. An unsupported
feature becomes an `N/A` cell with its reason.

How readiness is detected, from most to least preferred:

1. **Event from the app's own hook**, sent over the signal channel (the Foldex
   automation channel, or a script add-in for an app that supports one).
2. **Polling the app's own object model** (Explorer `IFolderView2::ItemCount`).
3. **UI Automation**: a status-bar text pattern such as `^(\d[\d,.]*) items`.
4. **Capture**: visual settle (pilot or fallback only).

Whichever signal is used, the pilot validates it against capture
([methodology.md](methodology.md#pilot)).

## Observers

Observers depend on nothing app-specific except a process set and a main
window handle.

- **winevent**: `SetWinEventHook(EVENT_OBJECT_SHOW, EVENT_OBJECT_DESTROY)`,
  plus `EVENT_SYSTEM_DIALOGSTART`, filtered by process. Gives `t_window` and
  the `no_dialog` gate.
- **ping**: a dedicated thread that sends `SendMessageTimeoutW(hwnd, WM_NULL,
  0, 0, SMTO_ABORTIFHUNG | SMTO_ERRORONEXIT, 5000)` every 10 ms and records
  each round-trip. It also calls `IsHungAppWindow` each second.
- **uia**: an `IUIAutomation` client on its own MTA thread. It watches named
  elements through `PropertyChanged` events and falls back to polling every 10
  ms. Used for status-bar counts and filter boxes.
- **process**: a 10 Hz sampler over the process set. It uses
  `GetProcessMemoryInfo`, `GetProcessTimes`, `GetProcessIoCounters`,
  `GetGuiResources` and `GetProcessHandleCount`, plus one PDH query for the GPU
  and context-switch counters.
- **signal**: a named pipe `\\.\pipe\fxbench-<run>` and a watched file
  `%TEMP%\fxbench\<run>\signal.log`, for hooks that can only append to a file.
  The harness timestamps each message **when it receives it**, never using a
  time the app wrote. The pilot measures how much later file-based signals
  arrive.
- **capture**: `Windows.Graphics.Capture` on the app window at the display's
  refresh rate, with frames hashed per region on the fly (only hashes and
  thumbnails are kept, not video). Settle time is the last frame whose item
  region hash changed before 2 s of stillness. Used in the pilot, for
  `t_icons_settled` / `t_thumbs_settled`, and as a fallback signal.

## Running

```
fxbench fixtures generate --root D:\FoldexBench --seed 20261028
fxbench run --suite full --apps explorer,foldex --out results/
fxbench run --scenario S2 --fixture F-100k --apps foldex,explorer --cache warm
fxbench summarize results/2026-11-03-bench01
fxbench export web results/2026-11-03-bench01      # → web/src/data/benchmarks.ts
fxbench doctor                                     # leftover processes/files/registry
```

A suite file (`suites/full.toml`) lists scenario × fixture × cache-state
combinations, so smaller suites (`smoke`, `listing-only`) reuse the same
definitions.

## Results

```
results/<YYYY-MM-DD>-<machine-id>/
  machine.json       hardware, Windows build, display, power, Defender, harness commit
  apps.json          per app: version, install type, settings snapshot, deviations applied
  suite.toml         exact suite that ran
  runs.jsonl         one line per run (below)
  summary.json       per scenario × variant × app: median, p90, min, max, CI, failures
  summary.md         human-readable tables, generated
  raw/               ping traces, process samples, capture hashes (gitignored)
```

`runs.jsonl` line:

```json
{"run":"S2/F-100k/warm/foldex/07","scenario":"S2","variant":"F-100k","app":"foldex",
 "cache":"warm","iteration":7,"order":["foldex","explorer"],
 "metrics":{"t_first_item":31.2,"t_listed":182.4,"stall_max":14.1},
 "signal":"hook","gates":{"count_ok":true,"no_crash":true,"no_dialog":true},
 "expected_count":100000,"observed_count":100000,"error":null}
```

Commit `machine.json`, `apps.json`, `suite.toml`, `runs.jsonl`, `summary.*`.
Ignore `raw/` with a `benchmarks/results/*/raw/` rule in `.gitignore`. (The
root rule `/results/` covers only the top-level folder, so it does not affect
`benchmarks/results/`.)

## Publishing to the website

`fxbench export web` rewrites `web/src/data/benchmarks.ts` from `summary.json`:

- `benchmarkInfo.machine` comes from `machine.json`.
- `benchmarkInfo.method` comes from the run's N and cache states.
- Each `results` entry lists **every app that ran**, Foldex first, matching the
  file's existing "Foldex first" rule.
- A headline `claim` is written only when the confidence-interval rule in
  [methodology.md](methodology.md#statistics) allows it.

The current figures in that file (210 ms vs 740 ms, and so on) have no
measurement behind them yet. They must be replaced by an export before the site
presents them as results.
