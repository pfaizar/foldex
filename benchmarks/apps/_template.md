# <App name>

> Copy this file to `apps/<id>.md`. Mark anything not yet confirmed on the
> benchmark machine with ⚠ and list it under **Verify in pilot**.

| | |
| :--- | :--- |
| **ID** | `<id>` (used in `apps.toml`, results and CLI flags) |
| **Role** | Baseline / competitor / ours |
| **Version tested** | Recorded per run in `apps.json`. How to read it: … |
| **Install type** | Installer / portable, and the path used on the benchmark machine |
| **UI technology** | e.g. Win32 + DirectUI, Delphi/VCL, egui + DX12 |
| **Main window class** | Used by the window observer to pick the right window |

## Profile reset

How to return the app to factory state between series: config folders,
registry keys, caches (including the thumbnail cache location).

## Allowed deviations applied

Only the items from [methodology.md](../methodology.md#allowed-deviations),
and exactly how each is applied for this app (first-run dialog, update check,
nag screen, Details view).

## Launch

| Mode | Command | Notes |
| :--- | :--- | :--- |
| Cold (no process running) | | |
| Resident (process already running) | | |
| Close | | |
| Kill everything | | |

## Control

| Action | How | Synchronous? |
| :--- | :--- | :--- |
| Navigate to folder | | |
| Back | | |
| Sort by name / size / date | | |
| Details view / thumbnail view | | |
| New tab | | |
| Select all | | |
| Focus filter | | |
| Recursive search | | |

## Readiness signals

| Metric | Signal | Source (hook / object model / UIA / capture) |
| :--- | :--- | :--- |
| `t_first_item` | | |
| `t_listed` | | |
| `t_sorted` | | |
| `t_filter_key` | | |
| `t_search_done` | | |

## Resource attribution

Which processes count toward this app's resources and why.

## Scenario notes

Anything per scenario that differs from the default procedure, including
`N/A` cells and their reasons.

## Caveats

Behaviors that affect how results should be read.

## Verify in pilot

- [ ] …
