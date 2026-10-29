# Windows 11 File Explorer

| | |
| :--- | :--- |
| **ID** | `explorer` |
| **Role** | Baseline, **default settings** (no tuning) |
| **Version tested** | Windows build plus `explorer.exe` file version (`GetFileVersionInfoW`), in `apps.json` |
| **Install type** | Built in |
| **UI technology** | Win32 + DirectUI items view, XAML command bar and address bar (WinUI 3 islands on current builds) |
| **Main window class** | `CabinetWClass` |

## Profile reset

The fresh `fxbench` Windows account is the reset. It is new for each series, so
there is no Quick Access history, no saved per-folder views (the `Bags` /
`BagMRU` registry keys are empty) and no recent files.

Between scenarios inside a series, nothing is reset. A real user keeps Explorer
state too. The saved view for each fixture folder is set once at the start
(Details) and then left alone.

Thumbnail cache: `%LOCALAPPDATA%\Microsoft\Windows\Explorer\thumbcache_*.db`
(Foldex shares this cache).

## Allowed deviations applied

- **Details view** for fixture folders, set through
  `IFolderView2::SetViewModeAndIconSize(FVM_DETAILS, …)` during setup. Explorer
  stores it in its per-folder view settings.
- **No other changes.** "Launch folder windows in a separate process" stays
  off, Home and Gallery stay on, and the status bar stays at its default (on).

## Launch

Explorer is always running as the shell, so a **cold process start does not
exist for it** in normal use.

| Mode | Command | Notes |
| :--- | :--- | :--- |
| Cold (no process running) | **Not applicable.** The optional `S1.cold-shell` variant runs `taskkill /f /im explorer.exe`, starts `explorer.exe`, waits for the taskbar, then opens the window. It is published only with a "not normal use" label. | |
| Resident | `explorer.exe "D:\FoldexBench\F-100"` | A short-lived `explorer.exe` hands the request to the shell process and exits. `t_window` comes from the shell process's new `CabinetWClass` window. |
| First window after login | `cold-boot` series in S1 | Closest real equivalent to a cold start |
| Close | `IWebBrowser2::Quit()` on the window from `ShellWindows` | |
| Kill everything | Not done between runs. Closing all `CabinetWClass` windows is enough. | |

## Control

Everything goes through the shell's own COM interfaces. No synthetic input
except for typing in S5.

| Action | How | Synchronous? |
| :--- | :--- | :--- |
| Find window | `IShellWindows` (CLSID_ShellWindows) → match `HWND` → `IServiceProvider::QueryService(SID_STopLevelBrowser)` → `IShellBrowser` → `QueryActiveShellView` → `IFolderView2` | — |
| Navigate | `IWebBrowser2::Navigate2(path)` | No, poll readiness |
| Back | `IWebBrowser2::GoBack()` | No |
| Sort | `IFolderView2::SetSortColumns(PKEY_ItemNameDisplay / PKEY_Size / PKEY_DateModified)` | No |
| View | `IFolderView2::SetViewModeAndIconSize` | No |
| New tab | No public API. **S3 tab stress uses new windows instead** (`Navigate2` with `navOpenInNewWindow`), and this is noted in results. | — |
| Select all | `IFolderView2::SelectItem(-1, SVSI_SELECT)` ⚠, or `IShellView::SelectItem` for each item if needed | — |
| Focus filter | **Not supported.** Explorer has no current-folder filter. Its search box always searches subfolders. | — |
| Recursive search | Navigate to `search-ms:query=<q>&crumb=location:<scope>` | No |

## Readiness signals

| Metric | Signal | Source |
| :--- | :--- | :--- |
| `t_first_item` | `IFolderView2::ItemCount(SVGIO_ALLVIEW) ≥ 1` with the folder path matching the target | Object model, polled every 5 ms from the harness's STA ⚠ |
| `t_listed` | `ItemCount == expected` **and** the status-bar UIA text matches `^\s*([\d,.]+) items`, giving the same number | Object model + UIA |
| `t_sorted` | `IFolderView2::GetSortColumns` returns the new key **and** item 0 (`IFolderView::Item(0)`) is the manifest's expected first item | Object model |
| `t_search_done` | Address bar progress finished (UIA: the search progress element is gone ⚠) **and** `ItemCount == expected`, stable for 250 ms | UIA + object model |

## Resource attribution

Folder windows run on their own threads inside the **shell `explorer.exe`**,
together with the taskbar, Start and desktop. To attribute fairly:

1. Wait for the shell to be idle (CPU < 1% for 10 s).
2. Take a **baseline** sample of the shell process.
3. Open the window or run the scenario.
4. Report the **difference** in `mem_private`, `handles`, `gdi`, `user` and
   `threads`, plus the shell process's CPU during the run.

Also included, for the time they are alive during the run: `SearchProtocolHost.exe`
and `SearchFilterHost.exe` (search), `dllhost.exe` instances started during the
run (COM surrogate thumbnail and preview handlers), and `SearchHost.exe`. For
the indexed search variant, `SearchIndexer.exe` CPU during the query is
reported **separately**, not added in, because it serves the whole system.

The idle numbers are labelled "added by one window", which is the fairest
figure available. The whole shell process size is shown as context, without a
comparison.

## Scenario notes

- **S1**: Explorer is resident only. It is compared with the other apps'
  resident modes, and with their cold starts as a separate, labelled row.
- **S5 filter**: `N/A`, no current-folder filter. The closest Explorer feature
  is the search box, which is measured in S5 search.
- **S5 search**: two variants. `T-250k` on `D:` (not indexed, so Explorer
  crawls) and `T-250k-indexed` under the user profile (indexed).
- **S3 tabs**: Explorer has had tabs since 22H2, but they cannot be opened
  through an API. Windows are used instead, and this is noted.

## Caveats

- On Win11, Explorer can show "Working on it…" placeholders. `ItemCount` may
  be ahead of what is drawn. The pilot capture check settles which signal is
  trustworthy.
- Explorer's folder-type detection may switch fixture folders to the Pictures
  or Documents templates (different default columns). Details view is forced;
  columns are left at whatever Explorer picks. That is default behavior.
- Thumbnail extraction for some formats runs out of process (`dllhost.exe`).
  That cost is counted as described above.

## Verify in pilot

- [ ] `IFolderView2::ItemCount(SVGIO_ALLVIEW)` grows during async enumeration rather than jumping straight to the final count.
- [ ] The status-bar text is exposed through UIA on the current build, with the exact format.
- [ ] The search-in-progress indicator can be detected through UIA.
- [ ] `search-ms:` navigation with `crumb=location:` runs a recursive, unindexed search on `D:`.
- [ ] Select-all through `IFolderView2` works in a 100k folder.
- [ ] Polling `IFolderView2` from another process's STA does not itself slow Explorer down (overhead check).
