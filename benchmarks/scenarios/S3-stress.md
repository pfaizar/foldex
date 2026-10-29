# S3: Stress (local only)

| | |
| :--- | :--- |
| **Question it answers** | "When things get heavy, does it freeze?" |
| **Headline metric** | `stall_max` (also `hung_events`) |
| **Fixtures** | `F-1M`, `D-50k`, `I-5k`, `P-long`, `U-names`, `C-churn`, `F-100k` |
| **Cache states** | `warm` (thumbnails: thumbnail cache cleared, file cache warm) |
| **N** | 10 (`F-1M`: 5) |

The question here is whether the app stays usable under load and still gets
the right answer, not how fast it finishes. A run passes when the app finishes
correctly. The comparison is how long the UI thread was blocked along the way.

## Variants

| Variant ID | Fixture | Action | Extra pass criteria |
| :--- | :--- | :--- | :--- |
| `S3.huge` | `F-1M` | Navigate in, then `sort(size)`, then `select_all()` | Count correct after each step |
| `S3.dirs` | `D-50k` | Navigate in, then `sort(modified)` | |
| `S3.thumbs` | `I-5k` | Thumbnail view, navigate in, wait for `t_thumbs_settled` | No placeholder left after settle |
| `S3.longpath` | `P-long` | Navigate to each of the 300-, 1,000- and 4,000-character leaf folders | Lists 100 items, or a clean "not supported" error (recorded) |
| `S3.unicode` | `U-names` | Navigate, `sort(name)` | Names round-trip (UIA name or hook item names match the manifest for a sample of 50) |
| `S3.churn` | `C-churn` | Navigate in, start the writer at 1,000 changes/s for 30 s, stop, wait | Count converges to the writer's final count within 10 s of stopping |
| `S3.burst` | 50 folders from `T-250k` | `navigate()` to each of the 50 as soon as the previous one is accepted (not listed) | Ends on the 50th folder, correct count |
| `S3.tabs` | `F-10k` × 20 | Open 20 tabs (Explorer: 20 windows) on different `F-10k` copies | All 20 listed |
| `S3.selectall` | `F-100k` | `select_all()` | Selection count = 100,000 through the hook/UIA |

## Procedure

1. Window open at `F-100`, 1280 × 800, idle.
2. Start ping (10 ms), WinEvent (dialogs) and process observers.
3. Run the variant's actions. Each step waits for its readiness signal before
   the next step, except `S3.burst`.
4. After the last step, keep observing for 3 s.
5. For `S3.churn`, the writer's log provides the expected count at the end.

## Metrics recorded

| Metric | Notes |
| :--- | :--- |
| `stall_max` | **Headline**, for each variant |
| `stall_p99`, `stall_over_100` | |
| `hung_events` | Any > 0 is called out in results |
| Completion time | `t_listed` / `t_sorted` / `t_thumbs_settled` of each step |
| `mem_private` peak | |
| Pass / fail | Per variant, per app |

## Per-app notes

| App | Notes / `N/A` reason |
| :--- | :--- |
| `explorer` | `S3.tabs` uses windows (no tab API). `S3.longpath` depends on how Explorer handles long paths. Whatever it does is recorded. |
| `foldex` | `S3.huge` may need Load all (see S2). `S3.thumbs`: background preview extraction only runs on fixed local drives, so `D:` must be a fixed internal drive. |

## Pass / fail

Standard gates, plus each variant's extra criteria. A crash or a hung window
that never recovers within the timeout is a failure and is **published as
such**.
