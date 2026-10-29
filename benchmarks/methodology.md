# Methodology

The rules every scenario and every app follow. When a scenario needs an
exception, it states the exception and the reason.

## Principles

1. **One clock, outside every app.** The harness timestamps every event with
   its own `QueryPerformanceCounter` clock. An app's internal timing (including
   Foldex's `--benchmark-ui` reports) is diagnostic only and never a headline
   number.
2. **The same definition of "done" for every app.** A folder counts as listed
   when the app's view holds exactly the expected number of items **and** the
   UI thread is answering messages again. Icons, thumbnails and size columns
   are timed separately ([metrics.md](metrics.md)).
3. **No computer use.** Apps are driven by their own command lines, scripting
   or COM interfaces, and by UI Automation. Synthetic keyboard input
   (`SendInput`) is used only where a scenario measures typing, and only on a
   locked benchmark desktop.
4. **Wrong is worse than slow.** Every run checks item and result counts
   against the fixture manifest. A wrong count fails the run; it is not
   recorded as a fast time.
5. **Defaults unless stated.** Each app runs with factory settings in a fresh
   profile. The few allowed changes are listed in
   [Allowed deviations](#allowed-deviations) and in each app file.
6. **Report everything.** Losses, `N/A` cells, failed runs and outliers are all
   published. We do not hand-pick runs.

## Benchmark machine

- One dedicated machine, with the machine manifest recorded per run
  ([harness.md](harness.md#results)): CPU, RAM, storage model and bus, GPU and
  driver, Windows edition and build, display resolution, scale and refresh
  rate, power plan, Defender status, and app versions.
- **A dedicated local Windows account `fxbench`**, created fresh for each
  results series. It isolates Explorer state (Quick Access, recent files, view
  memory) and Foldex's `%LOCALAPPDATA%\Foldex`, and it will cover any app
  added later. It is the simplest reset that treats every app equally.
- Plugged in, default Balanced power plan, lid open, one display at a fixed
  resolution and 100% or 150% scale (recorded). Windows Update paused for the
  session, notifications off (Do Not Disturb), no other apps open.
- Microsoft Defender **on**, with no exclusions (that is what users have).
  Recorded in the manifest.
- Fixtures live on a dedicated NTFS folder, `D:\FoldexBench`, outside the
  Windows Search index by default. S5 adds an indexed copy as an explicit
  variant.
- Before each series: reboot, log in as `fxbench`, wait for the post-login
  quiet period (CPU < 5% for 120 s, measured by the harness), then start.

## Cache states

Every timing scenario declares which of these it runs under.

| State | How it is produced | What it represents |
| :--- | :--- | :--- |
| `warm` | The same operation has run at least twice in this session | Everyday repeated use |
| `cold-fs` | Standby list purged right before the run; app process freshly started | First visit to a folder since boot |
| `cold-boot` | Reboot → log in → wait for quiet → run once | First launch of the day |

- Purging the standby list needs administrator rights. A small elevated helper
  (`fxbench-purge`) is called for each run. **The harness itself and every app
  under test run unelevated**, because an elevated launcher makes apps start
  elevated, and they behave differently that way.
- `cold-boot` is slow (one sample per reboot) and is used only for S1, with
  N = 5, using a logon scheduled task that resumes the harness.

## Window and view

- The harness moves and sizes every app window to **1280 × 800 physical pixels**
  on the primary display, using `SetWindowPos` after the window first appears.
  Scenarios start timing only after this happens, except S1, which times
  launch → window shown before resizing.
- **The only view change allowed: Details view** in S2, S3 and S5, so every app
  renders the same kind of rows. Thumbnail runs use each app's own large-icon or
  thumbnail mode. Each app's column set stays at its default.
- Sort: name ascending (the default in both apps) unless the scenario sorts.

## Repetitions and order

- `warm`: 2 discarded warm-up runs, then **N = 15** measured runs.
- `cold-fs`: **N = 10**.
- `cold-boot`: **N = 5**.
- **Interleaved order.** Each iteration runs every app once, in a rotated order
  (AB, BA, AB, … for two apps; ABC, BCA, CAB, … when more are added), so
  slow drift (thermals, background services)
  is spread across all apps rather than landing on one.
- Each app's process tree is fully closed between scenarios unless the
  scenario measures a running app. Resident processes (such as the Foldex
  tray owner) are killed when the scenario says "cold".
- A **timeout** per run (default 60 s, 300 s for 1M-item and search runs).
  A timeout is reported as a failure with the elapsed time, not as the timeout
  value.

## Statistics

- Report **median**, **p90**, **min**, **max** and the **count of failed
  runs** for every metric.
- Ratios ("3.1× faster") use medians, with a **95% bootstrap confidence
  interval** (10,000 resamples). Publish a ratio only when its interval stays
  entirely above 1.0 (or below it for a loss). Otherwise write "about the same".
- Find outliers with the median absolute deviation, but **never remove them**.
  Flag them, and look into any run above 3 MAD before publishing.
- Stall metrics (S3) report the **maximum** and **p99** across all runs, since
  the worst stall is what users feel.

## Allowed deviations

Applied the same way to every app and recorded in `apps.json` for each run:

1. Details view where a scenario requires it.
2. First-run dialogs, tips, update checks and trial or registration prompts
   dismissed or disabled **before** measuring, because they block input and
   are not part of browsing. Each app file lists how.
3. Automatic update checks off for the run (Foldex: Options → Application
   Updates; others: each app file lists how). They cause network traffic and
   CPU wake-ups that are unrelated to file browsing.
4. Benchmark hooks that only **report** events (script add-ins, the Foldex
   automation channel). Hooks must not change how an app loads, caches or
   draws anything. The pilot checks this ([below](#pilot)).

Anything else, including Explorer's "launch folder windows in a separate
process", Quick Access changes, or Defender exclusions, is **not** applied.

## Pilot

Before the first publishable run:

1. **Capability check.** Confirm every ⚠ item in each `apps/*.md` works on the
   benchmark machine and app versions. Correct the app file from what you see.
2. **Readiness-signal check.** For each app, record 10 runs of S2 `F-10k` and
   `F-100k` with the window capture running ([harness.md](harness.md#observers)).
   Compare each app's readiness signal with the capture's visual-settle time.
   - The signal must land **at or after** the last visible change, and no more
     than **one frame plus 16 ms** after it.
   - If a signal fires early (before rows are drawn), it cannot be used. Fix the
     adapter or switch to the capture as the signal for that app.
3. **Hook overhead check.** Run S1 and S2 with and without each app's hook
   installed, using capture-only timing. The medians must agree within 3%.
   Otherwise the hook changes behavior and must be redesigned.
4. **Clean-up check.** After one full suite, `fxbench doctor` confirms no stray
   processes, temporary files or registry changes remain.

## What a published result must include

- The machine manifest, app versions and each app's settings snapshot.
- The full metric table, including losses and `N/A` cells with their reasons.
- The cache state and N for every number.
- A link to the raw `runs.jsonl` and to this methodology at the commit used.
