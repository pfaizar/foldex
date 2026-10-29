# S1: Startup

| | |
| :--- | :--- |
| **Question it answers** | "I hit the shortcut. How long until I can work in the folder?" |
| **Headline metric** | `t_listed` for `F-100`, measured from process launch or command |
| **Fixtures** | `F-100` (target), `F-10k` (secondary) |
| **Cache states** | `warm`, `cold-fs`, `cold-boot` |
| **N** | 15 / 10 / 5 |

## Variants

Apps get different rows because they have different launch models. Each row is
labelled with how the app was launched, and rows are compared only with rows
of the same kind.

| Variant ID | Mode | Applies to |
| :--- | :--- | :--- |
| `S1.cold` | No process of the app running → launch with target folder | `foldex` |
| `S1.resident` | App's background process running, no window → open window with target folder | `explorer` (the shell), `foldex` (tray owner) |
| `S1.boot` | `cold-boot`: first window after login | both (Explorer: first `CabinetWClass` window after login) |
| `S1.cold-shell` | Explorer only: restart the shell, then open a window. Published with a "not normal use" label | `explorer` |
| `S1.10k` | Same as `S1.cold` / `S1.resident`, target `F-10k` | all that apply |

`S1.cold` has no Explorer row of the same kind. It may be shown next to
Explorer's `S1.resident`, labelled "Foldex from scratch vs. a new Explorer
window". That comparison is tougher for Foldex, so it is fair to show, but the
label must say which is which.

**The headline "startup" comparison is `S1.boot`**, because every app,
Explorer included, is really cold there. `S1.cold` and `S1.resident` are the
day-to-day numbers.

## Procedure

1. Make sure the starting state is right: `kill_all()` for cold variants;
   start the resident process and wait for idle (CPU < 1% for 5 s) for
   resident variants.
2. For `cold-fs`: purge the standby list. For `cold-boot`: the logon task
   starts this step.
3. Start the observers: winevent (process set), ping (attaches as soon as the
   window exists), process sampler, signal channel.
4. `t0`: `launch_cold(F-100)` or `open_window(F-100)`.
5. Wait for `listed` with `count == expected`, or time out after 60 s.
6. Keep observing for **2 s** after `t_listed`, which catches late stalls
   such as a toolbar or sidebar loading after the list.
7. `close()`. For cold variants also `kill_all()`.

The window is not resized before `t_listed` in S1, because resizing would
interfere with the app's own first layout. Each app opens at the size it
chooses on a fresh profile. The pilot records those sizes for each app.

## Metrics recorded

| Metric | Notes |
| :--- | :--- |
| `t_window` | Launch → window visible |
| `t_responsive` | Window visible → input ready |
| `t_listed` | **Headline** |
| `stall_max` | From `t_window` to `t_listed + 2 s` |
| `io_read_bytes` | Disk read during startup (cold variants) |
| `mem_private` | At `t_listed + 2 s` |

## Per-app notes

| App | Notes / `N/A` reason |
| :--- | :--- |
| `explorer` | `S1.cold` is `N/A`: the shell is always running. `S1.resident` is its everyday number. |
| `foldex` | The resident handoff (`WM_COPYDATA` to the tray owner) is `S1.resident`. The first DX12 pipeline build is included in `S1.boot`. |

## Pass / fail

The standard gates. A first-run or reminder dialog appearing during a run
fails it: that would mean the profile reset or the allowed deviations went
wrong.
