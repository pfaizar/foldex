# S4: Resource use

| | |
| :--- | :--- |
| **Question it answers** | "What does it cost to keep open, and does it give memory back?" |
| **Headline metric** | `mem_private` at idle with one window, plus `cpu_idle_pct` |
| **Fixtures** | `F-100`, `F-100k`, `F-1M`, `F-10k` |
| **Cache states** | `warm` |
| **N** | 10 (soak: 3) |

## Variants

| Variant ID | State measured | Sampling |
| :--- | :--- | :--- |
| `S4.idle` | One window at `F-100`, after S1-style launch, 60 s settle | 60 s at 10 Hz → median |
| `S4.loaded` | One window at `F-100k`, 60 s settle | 60 s → median, plus peak during load |
| `S4.reclaim` | Navigate `F-1M` → listed → `F-100`, then 60 s settle | Median of the last 30 s, compared with `S4.idle` |
| `S4.tabs` | 10 tabs (Explorer: 10 windows) on `F-10k` copies, 60 s settle | 60 s → median |
| `S4.background` | Resident process with **no window** (Foldex tray owner), 120 s | 120 s → median. Foldex only: Explorer's resident process is the shell itself, so there is nothing comparable. |
| `S4.soak` | 30-minute loop: navigate through 20 fixture folders with sorts, then back to `F-100` | `leak_slope` over the run, plus `mem_private` at the end compared with the start |

## Procedure

1. Launch fresh (S1 `cold` / `resident` as applies) and wait for the state.
2. **Settle:** wait 60 s with no input. Many apps trim caches or finish
   background work after a short delay, so the measurement starts after that.
3. Sample the process set at 10 Hz for the variant's period.
4. For Explorer, take the baseline first and report the differences
   ([apps/explorer.md](../apps/explorer.md#resource-attribution)).

## Metrics recorded

| Metric | Reported as |
| :--- | :--- |
| `mem_private` | **Headline** (median over the period) |
| `mem_ws_private` | What Task Manager shows as "Memory", for readers who check it themselves |
| `mem_gpu_dedicated`, `mem_gpu_shared` | Shown next to CPU memory, never added together silently |
| `cpu_idle_pct`, `wakeups_per_s` | `S4.idle`, `S4.background`: should be near zero |
| `gpu_util_pct` | `S4.idle`: should be near zero when nothing is moving |
| `handles`, `gdi`, `user`, `threads` | `S4.idle`, `S4.tabs` |
| `leak_slope` | `S4.soak` |

The website's "Memory over a session" curve (`curve.kind = 'level'`) is drawn
from `S4.soak`'s `mem_private` series.

## Per-app notes

| App | Notes / `N/A` reason |
| :--- | :--- |
| `explorer` | Differences from the baseline, labelled "added by one window". The whole-shell size is shown as context only. |
| `foldex` | GPU memory is reported. Foldex's egui UI redraws only when something changes (`request_repaint_after`), so idle GPU and CPU use should be near zero, and this verifies it. |

## Pass / fail

Standard gates. `S4.soak` also fails if any process in the set exits.
