# Metrics

One shared vocabulary for every scenario. Times are in milliseconds from the
harness clock unless marked. `t0` is the moment the harness issues the
command: it calls `CreateProcessW`, sends the app's navigate or filter
command, or calls `SendInput` for the first keystroke.

## Timing

| ID | Name | Definition |
| :--- | :--- | :--- |
| `t_window` | Window shown | First top-level window of the app's process tree becomes visible (`EVENT_OBJECT_SHOW` through `SetWinEventHook`, filtered to the app's window class). |
| `t_responsive` | Input ready | First moment after `t_window` when the UI thread answers a `WM_NULL` ping within 50 ms **and** keeps answering within 50 ms for the next 500 ms. |
| `t_first_item` | First item visible | The app's view holds ≥ 1 item of the target folder (from its readiness signal). |
| `t_listed` | Fully listed | The view holds exactly the manifest's item count, **and** the next ping after that is answered within 50 ms. The main listing metric. |
| `t_sorted` | Re-sorted | After a sort command: the readiness signal reports the new order is active, and the next ping is answered within 50 ms. |
| `t_icons_settled` | Icons settled | Capture pass only: the last frame where the item area changed, before 2 s of stillness. Covers icons and overlays. |
| `t_thumbs_settled` | Thumbnails settled | Same as `t_icons_settled`, in thumbnail view. |
| `t_filter_key` | Keystroke → filtered | For each keystroke in a filter query: from `SendInput` until the view's count equals the manifest count for that prefix and a ping is answered. Reported per keystroke and as p50/max over the query. |
| `t_search_first` | First search result | From the search command until ≥ 1 result is in the view. |
| `t_search_done` | Search complete | Until the app reports the search finished **and** the result count equals the manifest count. |
| `t_back` | Back navigation | From the Back command until the previous folder is fully listed again. |

## Responsiveness

The harness pings the app's main window every **10 ms** with
`SendMessageTimeoutW(WM_NULL, SMTO_ABORTIFHUNG, 5000 ms)` from a dedicated
thread, and records each round-trip.

| ID | Name | Definition |
| :--- | :--- | :--- |
| `stall_max` | Longest stall | Longest ping round-trip during the measured window. **The headline metric for S3.** |
| `stall_p99` | p99 stall | 99th percentile of the ping round-trips. |
| `stall_over_100` | Stall time > 100 ms | Total time spent in pings longer than 100 ms. |
| `hung_events` | "Not Responding" count | How many times `IsHungAppWindow` returned true (Windows ghosts the window after about 5 s). |

Every app pumps window messages on the UI thread, Foldex included through
winit, so a slow ping means the user's clicks and keys would also wait. The
measure is the same for every app and needs no special support from it.

## Resources

Sampled at **10 Hz** over the app's whole process tree (see each app file for
which processes count).

| ID | Name | Source |
| :--- | :--- | :--- |
| `mem_private` | Private bytes | `PROCESS_MEMORY_COUNTERS_EX2.PrivateUsage`, summed |
| `mem_ws_private` | Private working set | PDH `\Process(*)\Working Set - Private`, summed |
| `mem_gpu_dedicated` | GPU dedicated memory | PDH `\GPU Process Memory(pid_*)\Dedicated Usage` |
| `mem_gpu_shared` | GPU shared memory | PDH `\GPU Process Memory(pid_*)\Shared Usage` |
| `cpu_ms` | CPU time | `GetProcessTimes` user + kernel, as a difference over the window |
| `cpu_idle_pct` | Idle CPU | `cpu_ms` / wall time / logical cores, while idle |
| `wakeups_per_s` | Context switches/s | PDH `\Thread(*)\Context Switches/sec`, summed over the tree. Shows idle wake-ups (battery cost) |
| `gpu_util_pct` | GPU engine use | PDH `\GPU Engine(pid_*_engtype_3D)\Utilization Percentage` |
| `handles` / `gdi` / `user` | Handles, GDI and USER objects | `GetProcessHandleCount`, `GetGuiResources` |
| `threads` | Thread count | Toolhelp snapshot |
| `io_read_bytes` | Disk read | `GetProcessIoCounters.ReadTransferCount`, as a difference |
| `leak_slope` | Memory growth | Least-squares slope of `mem_private` over a soak run, MB per hour |

For **Explorer**, the folder windows share `explorer.exe` with the taskbar and
desktop. Its figures are the **difference** from a baseline taken just before
the window opens ([apps/explorer.md](apps/explorer.md#resource-attribution)).

## Correctness gates

| ID | Rule |
| :--- | :--- |
| `count_ok` | Listed, filtered or found count equals the manifest count |
| `no_crash` | No process in the tree exited unexpectedly during the run |
| `no_dialog` | No unexpected top-level modal dialog appeared (detected by WinEvent) |

A run that fails a gate is a **failed run**. Its timings are kept in raw data
but excluded from medians, and the failure count is published.
