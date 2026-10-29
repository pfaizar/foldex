# S2: Folder listing

| | |
| :--- | :--- |
| **Question it answers** | "I opened a big folder. How long until it's all there, and can I sort it?" |
| **Headline metric** | `t_listed` |
| **Fixtures** | `F-1k`, `F-10k`, `F-100k`, `F-1M`, `D-50k`, `I-5k` (Details view) |
| **Cache states** | `warm`, `cold-fs` |
| **N** | 15 / 10 (`F-1M`: 5 / 5) |

## Variants

| Variant ID | Fixture | What it measures |
| :--- | :--- | :--- |
| `S2.open.<set>` | each set | Navigate from `F-100` → set. The main listing number |
| `S2.sort.<key>` | `F-100k` | Re-sort by `size`, then `modified`, then back to `name` |
| `S2.back` | `F-100k` → `F-100` → Back | Back navigation to a large folder visited earlier |
| `S2.icons` | `F-10k` | `t_icons_settled`, from capture only |

## Procedure

1. Window already open at `F-100`, resized to 1280 × 800, Details view, idle
   (CPU < 1% for 3 s).
2. For `cold-fs`: close the window, purge the standby list, reopen at
   `F-100`, wait for idle. (That way the app process is warm but the target
   folder's metadata is not cached.)
3. Start ping and process observers.
4. `t0`: `navigate(set)`.
5. Wait for `listed`, `count == expected`.
6. Keep observing for 1 s.
7. Sort variants: `sort(key)`, wait for `sorted`, 1 s, next key.
8. Back variant: `navigate(F-100)`, wait for listed; `t0`: `back()`, wait for
   listed.
9. `navigate(F-100)` to reset for the next iteration.

## Metrics recorded

| Metric | Notes |
| :--- | :--- |
| `t_first_item` | Shows whether an app streams results in or shows nothing until the end |
| `t_listed` | **Headline** |
| `t_sorted` | For each sort key |
| `t_back` | |
| `stall_max` / `stall_p99` | During the whole navigate → listed period |
| `mem_private` peak | During `F-100k` and `F-1M` |
| `t_icons_settled` | `S2.icons` only |

**Results present the gap between `t_first_item` and `t_listed` as a progress
curve**, which is what the website's race chart draws
(`curve.kind = 'progress'`). The harness keeps the full count-over-time series
from the readiness polling for this.

## Per-app notes

| App | Notes / `N/A` reason |
| :--- | :--- |
| `explorer` | `t_first_item` from `IFolderView2::ItemCount ≥ 1`. Folder-type detection may switch templates. Left at the default. |
| `foldex` | `F-1M` may hit the 64 MiB listing budget. If it does, the run sends Load all and keeps waiting, and the result is labelled "needed Load all". |

## Pass / fail

Standard gates. The run fails if `count != expected` 60 s after the last count
change (300 s for `F-1M`).
