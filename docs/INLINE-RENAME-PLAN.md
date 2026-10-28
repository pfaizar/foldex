# Inline rename and new-item editing: plan

Status: implemented (phases 1–7), not yet released. Multi-selection F2 keeps the batch dialog (option B).

Where it landed:

- Naming rules: `foldex/src/naming.rs`. Editor widget and balloon: `foldex-ui/src/inline_edit.rs`.
- Per-pane editor state, commit/cancel, optimistic labels, Tab, shell New templates: `foldex/src/pane/rename.rs`.
- Placement in the listing (all view modes), slow click: `foldex/src/pane/listing.rs`. Compact panes: `foldex/src/app/workspace_canvas.rs`. Home cards and Recent rows: `foldex/src/app/home_ui.rs`.
- Create-then-edit (`create_unique`, job outcomes): `foldex/src/platform.rs`. Extension-change prompt: `foldex/src/app/rename_ui.rs`.
- Tests: `foldex/src/app/rename_tests.rs` drives real frames, files and jobs. `--benchmark-ui <dir> --benchmark-inline-rename --secondary-monitor` captures screenshots of the real window without sending desktop input.

How the open questions in section 5 were settled until checked against Explorer by hand: Tab on the last item commits and stops; **No** in the extension prompt reopens the editor with the typed name; the list re-sorts after the job's refresh and the selection follows the item; leading spaces are kept; the mouse wheel commits a focused edit; folder name conflicts use the Windows shell's own dialog.

Goal: replace the modal Rename / New folder / New file dialog with an editor that opens inside the item itself, as File Explorer does. This document has three parts:

1. What Foldex does today.
2. What Explorer does, which is the spec this work copies.
3. A phased implementation plan.

File references are relative to `crates/`.

---

## 1. What Foldex does today

### The one modal

All three flows go through a single modal, `EditDialog { path, name, create, create_file, focus }` (`foldex/src/app.rs:114`), stored in `Foldex.edit`. It is opened from these places:

| Trigger | Code | Default text |
|---|---|---|
| F2, or `Command::Rename` with one item selected | `app/commands_ui.rs:220`, then `Action::RenameMany`, then `Action::Rename` (`app.rs:971-988`) | Full name, with **nothing pre-selected** |
| Context menu "Rename" (`CoreVerb::Rename`) | `commands_ui.rs:451` | Same as above |
| Ctrl+Shift+N, or context menu New > Folder | `Action::NewFolder` (`app.rs:1018`) | "New folder" |
| `Command::NewFile`, or context menu New > Text document | `commands_ui.rs:226`, `:465` (the struct is duplicated in both places) | "New file.txt" |

- **Rendering.** The dialog is rendered in `fn dialogs` at `app.rs:1683-1780`, using `components::dialog` and `input_box(Id::new("rename-name"))`. It submits `Job::Rename`, `Job::NewFolder` or `Job::NewFile`.
- **Blocking.** `popup_active()` (`commands_ui.rs:36`) counts the dialog, so every hotkey is blocked while it is open.
- **Creation timing.** The item is only created **after** the user confirms. If the name is taken, the create fails with an error. Nothing ever generates "New folder (2)".
- **After the job.** `Event::Done { message, refresh }` (`platform.rs:171`) carries **no path**, so the app shows a toast and refreshes. No code selects or scrolls to the created or renamed item. Selection restore after a refresh matches items by `raw_name` (`foldex-core/src/restore.rs`), so **a renamed item loses its selection**.
- **Multi-selection.** F2 with more than one item selected opens the separate `BatchRename` modal (`batch.rs`), which supports templates, find and replace, and case changes.

### Where items are drawn (every place an inline editor must appear)

| Surface | Name painting | Notes |
|---|---|---|
| Main listing, all view modes: Details, List, "Columns" (which is a multi-column list, not Miller columns), and Medium, Large and Extra-large icons | `pane/listing.rs:679-759`: a custom single-row galley (`name_galley`), clipped to the Name column in Details. The name rect comes from `item_parts` (`listing.rs:1197`) | Virtualized with `show_rows`. Row identity is `(pane, snapshot path, raw_name, relative_path)`, so **a rename changes the identity**. |
| Search results | Same listing (`pane.recursive`), plus a parent-path tail | Refreshing a recursive listing clears the selection unconditionally (`pane.rs:1008`). |
| Recycle Bin | Same listing | Rename and new-item actions are already blocked (`app/recycle_ui.rs:54-119`). |
| Compact pane (small workspace cells) | `app/workspace_canvas.rs:1095-1209`, using `elided_label` | Also virtualized. |
| Home: Recent rows and Recommended cards | `app/home_ui.rs:832-888`, `:735-830` | F2 already reaches `Action::Rename` from Home (`pane.rs:781`). New-item actions are blocked on Home. |
| Sidebar, breadcrumbs, tab strip | — | No rename exists in these today, and none is needed for this work (see §4, non-goals). |

### Plumbing worth reusing

- **Rename backend.** On Windows, rename already goes through `IFileOperation::RenameItem` with `FOF_ALLOWUNDO | FOFX_ADDUNDORECORD` (`platform/windows_ops.rs:1228`). That gives us the shell's own conflict dialog, the "File In Use" dialog and the administrator-permission prompt for free. These are exactly the dialogs Explorer shows.
- **Undo history.** History already records Create as Undo = recycle (`operation_history.rs:317`) and Rename as Undo = move back (`windows_history.rs:244`).
- **Name validation.** `platform::validate_name` (`platform.rs:932`) rejects:
  - empty names, `.` and `..`
  - names with a trailing space or dot
  - names longer than 255 UTF-16 units
  - control characters and the characters `/ \ : * ? " < > |`
  - reserved device names
- **Select-by-name after refresh.** `Pane.pending_selection` (`pane.rs:187`) already selects a named entry and scrolls to it after the next view. It is the right tool for re-selecting an item after a commit.
- **Focus and hotkey gating.** `Foldex::editing()` (`app.rs:1334`) treats any focused widget that is not registered as `Region::Listing` as text editing. While editing, file hotkeys and plain keystrokes are suppressed (`hotkeys.rs:2820-2971`). F-keys (F5, F6 and so on) still dispatch.
- **Text-field precedents.**
  - Programmatic selection of a text range: `goto.rs:532-544` (`CCursorRange::two`).
  - Consuming Enter before the `TextEdit` sees it: `commands::take_key`, as used in `theme_options.rs:194`.

---

## 2. What Explorer does (the spec)

Sources were Microsoft docs, Raymond Chen's *The Old New Thing*, ListView label-edit semantics, and shell-API tests run on this machine (Windows 11, build 26200). Items marked ⚠ are not confirmed and should be checked on a real Explorer before we rely on them; see §5.

### Triggers

- **F2**, **context menu > Rename**, and the command-bar Rename button.
- **Slow second click** on the **label text** of an item that is **already selected** in a view that **already has focus**. Explorer waits the system double-click time (`GetDoubleClickTime`, 500 ms by default) before opening the editor. A second click within that time (a double-click) or movement past the drag threshold cancels it. Clicks on the icon, on blank row space, or on one item of a multi-selection do not start a rename.
- **Ctrl+Shift+N** and **New > Folder** / **New > Text Document** / other New templates all *create the item and then* open the editor.

### Initial text selection

- **Files:** the stem is selected, without the last extension. `report.final.docx` selects `report.final`, and `archive.tar.gz` selects `archive.tar`.
- **Folders:** the whole name is selected, even if it contains dots (`v1.2`).
- **Names that start with a dot and have no other dot** (`.gitignore`): the whole name is selected.
- **Names with no extension:** the whole name is selected.
- **Hidden extensions:** Explorer edits the display name and re-appends the extension. **This does not apply to Foldex**, which always shows extensions.

### Editor geometry and look

- **Details and List:** a single line over the Name cell. It starts where the name text starts (right of the icon), is as wide as the Name column, and scrolls horizontally for long names.
- **Icon views:** a centred, word-wrapping box under the icon, the width of the cell. It grows downward up to a cap and then scrolls. It may overlap the items below.
- The editor uses the label font. The label text is hidden under the editor. The item keeps its selection highlight. The editor has a 1px accent border.

### Keys inside the editor

| Key | Behaviour |
|---|---|
| Enter | Commit. |
| Esc | Cancel and revert. |
| Tab / Shift+Tab | Commit, then open the editor on the next or previous item in view order. ⚠ Whether it wraps at the ends is unconfirmed. |
| Standard edit keys | Ctrl+Z undoes typing, plus Ctrl+A, Home, End, the arrow keys with or without Shift and Ctrl, and Ctrl+C, X and V. |
| Up / Down | Move the caret only. They never change the selected item. |
| F2 again | Does **not** cycle the text selection; that is a third-party feature. |

### Commit and cancel rules

- **Focus loss commits.** This covers a click elsewhere in the list, a click in another pane or the sidebar, and switching to another app.
- **Scrolling with the mouse wheel** commits and closes the editor.
- **Navigating away** ends the edit by committing it (through focus loss).
- **Committing an empty or unchanged name** reverts silently, with no error.
- **Refresh.** In Explorer, any refresh cancels the editor. This is a known Explorer weakness: on Windows 11, background refreshes can cancel the edit and drop typed letters. **We copy the rule for an explicit F5 but not for background refreshes**; see design decision D3.

### Input validation

- **Forbidden characters** `\ / : * ? " < > |` are **blocked as they are typed**, and a balloon appears under the editor: "A file name can't contain any of the following characters: \ / : * ? " < > |". When pasted, they are silently stripped and the rest is kept. The clipboard itself is not changed.
- **Trailing spaces and dots are trimmed silently.** For example, `.gitignore.` becomes `.gitignore`. ⚠ Whether leading spaces are also trimmed is unconfirmed.
- **Empty after trimming** produces "You must type a file name." and the editor stays open.
- **Reserved names** (CON, PRN, AUX, NUL, COM1-9, LPT1-9, and the superscript variants) produce "The specified device name is invalid." Recent Windows 11 builds accept `CON.txt`; we keep rejecting all of them, which is the safe choice.
- **Names longer than 255 characters** cannot be typed past the limit.
- **After a validation error,** the editor stays or reopens with the typed text, not with the old name.

### Extension change

- If the extension changes (including being removed), a modal "Rename" Yes/No prompt appears: "If you change a file name extension, the file might become unusable. Are you sure you want to change it?"
- It never appears for folders or for a change that only differs in case.
- ⚠ What No does is unconfirmed: either the rename is cancelled, or the editor reopens.

### Conflicts and I/O errors

These are all shown by the shell's `IFileOperation`, which Foldex already uses for rename.

- **A file with the same name exists:** a "Rename File" dialog offers to rename to `name (2).ext`.
- **A case-only rename** works.
- **Access denied:** the administrator-permission prompt appears, with Continue, Skip and Cancel.
- **The file is open in another program:** "File In Use", with Try Again and Cancel.
- In all three cases the editor is already closed, and Cancel leaves the original name.

### New folder and New file

1. The item is created on disk **first**, under a unique default name: `New folder`, `New Text Document.txt`. If the name is taken, ` (n)` is inserted before the last extension, starting at **2** and using the **lowest free n**. This was tested with `PathYetAnotherMakeUniqueName`, which produced `New folder (2)`, `New Text Document (2).txt` and `archive.tar (2).gz`.
2. The item appears, is selected and focused, scrolls into view, and opens in the editor. For folders all the text is selected; for files only the stem is selected.
3. Esc, or an empty or unchanged name, **keeps** the item under its default name. Nothing is deleted.
4. The list is not re-sorted while the editor is open. After a commit, the item is re-sorted, stays selected and is scrolled into view.
5. Undo works in two steps: the first Ctrl+Z undoes the rename (back to "New folder"), and the next one undoes the creation.

### Multi-selection F2

Explorer opens an editor on the focused item only. On commit, every selected item is renamed to `Name (1).ext`, `Name (2).ext` and so on, in view order. Each file keeps its own extension. One Ctrl+Z undoes the whole batch.

*Foldex deliberately does not copy this. It keeps its Batch Rename dialog for multi-selection (see Phase 6).*

### Where inline rename appears

- Inline rename is available in every folder view, in search results (it renames the real file) and in the navigation tree.
- It is not available in the Recycle Bin.

---

## 3. Design decisions

| # | Decision | Why |
|---|---|---|
| D1 | **The editor state lives on the `Pane`, not on the app.** `Pane.rename: Option<InlineRename>`, and at most one per app: starting a rename in one pane commits any open one in another. | The editor must follow its row through scrolling, refreshes and reflow, and each pane already owns those. |
| D2 | **Track the target by its parent path plus `raw_name`, not by row index or `identity`.** Re-resolve the row on every view and window update. | Rows move when other items arrive. `identity` includes the snapshot path. |
| D3 | **A background refresh (watcher, `Done`, column worker) does NOT cancel the editor.** It cancels only if the target disappears from the snapshot (externally deleted or renamed). An explicit **F5** commits first, then refreshes. | Creating "New folder" itself triggers a watcher refresh 50–200 ms later (`engine.rs:1346`). Copying Explorer's "any refresh cancels" rule would close the editor straight away. That is exactly the Windows 11 bug the research flagged. |
| D4 | **Draw the editor after the row loop, in the same layer, clipped to the listing body.** | A box drawn later paints over the rows below it (icon views grow downward) and wins hit-testing over the row's `interact`. No separate `Area` is needed. |
| D5 | **Create first, then rename**, as Explorer does. The worker picks the unique name, creates the item, and reports the created path back to the originating pane. The pane then opens the editor. | Esc keeps the item, the "(2)" names match Explorer, and Undo already handles the creation. |
| D6 | **Show the new name optimistically.** On commit, the row paints the new name with a pending style until the refresh confirms it. If the job fails, it reverts. | Without this, the old name flickers back for 50–200 ms before the refresh arrives. |
| D7 | **Re-select after a commit by name.** After a successful rename or create, set `pending_selection` to the final name, which selects and scrolls to the row through the existing restore path. | This fixes today's lost selection after a rename. |
| D8 | **No success toasts** for inline rename and create. Error toasts and the shell dialogs stay. | Explorer is silent; the item itself shows the result. |
| D9 | **Do not register the editor as `Region::Listing`.** | Then `editing()` stays true and every file hotkey stays suppressed without new gating code. |
| D10 | **Remove `EditDialog` completely.** No modal fallback. | Every surface that can rename gets an inline editor. |

---

## 4. Implementation plan

### Phase 1: Naming rules and the editor component (no behaviour change yet)

**New file `foldex/src/naming.rs`** (pure functions, all unit-tested):

- `initial_selection(name, is_dir) -> Range<usize>` (character indexes):
  - folders: the whole name
  - files: up to the last `.`, unless that dot is at index 0 or there is no dot
- `sanitize_typed(old, new) -> (String, rejected: bool)`: removes forbidden and control characters and reports whether any were removed, which drives the balloon. Also caps the name at 255 UTF-16 units. egui's `char_limit` counts `char`s, not UTF-16 units, so it cannot do this.
- `normalize_for_commit(text) -> Result<Commit, NameError>`. It trims trailing spaces and dots, then returns one of:
  - `Unchanged` or `Empty`: cancel silently
  - `Ok(name)`
  - `Err(Reserved | Empty after trim | TooLong)`, each with Explorer's message text
- `extension_changed(old, new, is_dir) -> bool`: case-insensitive. Always false for folders.
- `unique_child_name(parent, base, is_dir) -> String`: lowest free ` (n)` starting at 2, inserted before the last extension (`New Text Document (2).txt`, `archive.tar (2).gz`). The worker calls it and retries on `AlreadyExists`, which handles races.
- Align `platform::validate_name` and `batch::validate_name` on one shared rule set.

**New file `foldex-ui/src/inline_edit.rs`: `inline_name_editor(ui, rect, id, theme, state, mode)`**

- **Mode:**
  - `SingleLine` for Details, List, Columns and compact rows.
  - `Wrapping { max_lines }` for icon views, centred. It grows downward up to about 5 lines, then scrolls.
- **Painting:** the background colour, a 1px accent border and the label font.
- **Initial selection:** applied once, on the first frame, through `TextEditState` with `CCursorRange::two` (the `goto.rs` pattern).
- **Keys consumed before the `TextEdit`** (the `take_key` pattern): Enter, Escape, Tab and Shift+Tab. This stops egui from dropping focus itself, and stops multi-line mode from inserting a newline.
- **Returns `InlineOutcome`:** `Editing`, `Commit`, `Cancel`, `CommitAndMove(±1)` or `FocusLost`.
- **Invalid-character balloon:** a small tooltip anchored under the editor. It disappears on the next keystroke or after about 4 seconds.
- **IME:** Enter pressed while an IME is composing must not commit. Reuse the `keyboard.rs` composing check.

**Tests:** unit tests for every `naming` function, covering the Explorer cases listed in §2.

### Phase 2: Inline rename in the main listing (single item)

1. **`Pane` state.** Add `rename: Option<InlineRename { parent, raw_name, is_dir, text, opened_frame, error: Option<NameError>, origin: RenameOrigin }>` and the methods `begin_rename`, `cancel_rename`, `commit_rename` and `rename_row()` (re-resolves the row by name).
2. **Entry point.** `Action::Rename(path)` calls `pane.begin_rename(path)` instead of setting `self.edit`:
   - It finds the row.
   - It sets the selection to that row only, and sets focus and `scroll_to` to it.
   - If the row is not in the current snapshot (Home and search are covered in Phase 5), it falls back to selecting it once it arrives.
3. **Listing integration (`pane/listing.rs`).**
   - While the row loop runs, record `editor_rect` for the target row:
     - **Details:** from the name text's x position to the Name column's right edge minus 8·scale.
     - **List and Columns:** from `nr` to the row's right edge.
     - **Icon views:** `nr` widened to the cell, starting at the same top.
   - Skip painting the name galley for that row.
   - After the loop, call `inline_name_editor` with the recorded rect.
   - **If the target row is not visible** because it scrolled out, commit. If the row has disappeared from the snapshot, cancel (Explorer's rule).
   - **On mouse-wheel scroll** while editing, commit (Explorer). Detect a non-zero `smooth_scroll_delta` over the body before `show_rows`.
4. **Compact pane** (`workspace_canvas.rs:1168`): the same editor in `SingleLine` mode over the label rect.
5. **Outcome handling (in `Pane`, which returns an `Action`):**
   - **Cancel:** close the editor and focus the listing composite (`keyboard::listing_id`). The item stays selected.
   - **Commit:** run `normalize_for_commit`.
     - Silent cancel: as above.
     - Error: keep the editor open, and show the balloon with the error message and the typed text.
     - Extension changed: show the Yes/No confirmation (Phase 4). Until Phase 4 lands, skip the confirmation.
     - Otherwise: emit `Action::RenameInline { path, name, origin: pane.id }`, set the optimistic label, and close the editor.
   - **Focus lost:** treat it as Commit. This covers clicks elsewhere, other panes, the sidebar and the window losing OS focus. Detect the last case with `ctx.input(|i| i.focused)` turning false.
   - **Navigation, tab switch, pane close or split change:** commit first, as Explorer commits on focus loss. Hook this into `navigate()`, tab activation and pane teardown.
   - **Explicit F5:** while editing, commit, then refresh (decision D3).
6. **Job and event plumbing.**
   - Add `origin: u64` (the pane id) to `Job::Rename`.
   - Extend `Event::Done` with `outcome: Option<JobOutcome>`, where `JobOutcome` is `Renamed { origin, from, to }` or `Created { origin, path, then_rename }`.
   - On Windows, capture the **actual** final path. If the user accepts the shell's "rename to (2)" offer, the final name differs from the typed one. Read it from the `IFileOperationProgressSink::PostRenameItem` `psiNewlyCreated` item in `perform_batch`.
   - In `poll` (`app.rs:1191`):
     - On `Renamed`, set `pending_selection = to.file_name()` on the origin pane and clear the optimistic label.
     - On failure, clear the optimistic label. The error toast is already shown.
     - Suppress the success toast (decision D8).
7. **Gating cleanup.**
   - Remove `self.edit` from `popup_active()`.
   - Change `can_install_update` to check `!any_pane_renaming()`.
   - Update the keyboard benchmark checks `RenameOpen` and `RenameCancelled` (`keyboard_benchmark.rs:1125-1146`, `2268`, `2349`, `2517`, `2747`).
   - Update the tests that look for `Id::new("rename-name")` (`keyboard_ui.rs:419-466`, `app.rs:2008/2096/2135/2172/2519`). They should look for the per-pane editor id `Id::new((pane.id, "inline-rename"))`.
8. **Tests (`app.rs` fixture app):**
   - F2 opens the editor with the stem selected.
   - Typing and Enter renames on disk, and the item stays selected.
   - Esc leaves the name unchanged.
   - A watcher refresh in the middle of the edit does not close the editor.
   - Deleting the item externally closes the editor.
   - A click on another row commits and selects that row.
   - Wheel scrolling commits.
   - Typing `a:b` gives `ab` and shows the balloon.
   - `CON` keeps the editor open.
   - Trailing dots and spaces are trimmed.
   - A case-only rename works.
   - Type-to-search does not steal text from the editor.

### Phase 3: Create-then-rename for New folder and New file

1. **Jobs.** Replace `Job::NewFolder { parent, name }` and `Job::NewFile` with `Job::NewItem { parent, kind: Folder | File, base: String, origin }`.
   - The worker resolves `unique_child_name`, creates the item (retrying on `AlreadyExists`) and records history as it does today ("New Folder" / "New File").
   - It returns `JobOutcome::Created { origin, path, then_rename: true }`.
2. **Default names:**
   - "New folder" (unchanged).
   - "New Text Document.txt" for New file. Today it is "New file.txt"; this change matches Explorer.
3. **Entry points.**
   - `Action::NewFolder` and `Command::NewFile` submit the job directly. No dialog.
   - Delete the duplicated `EditDialog` construction at `commands_ui.rs:226` and `:465`.
4. **On `Created` in `poll`:**
   - Set `pending_selection` and a `pending_rename = path` on the origin pane.
   - When `Event::View` restores that row, call `begin_rename`. Folders get the whole name selected; files get the stem.
   - Esc or an unchanged name keeps the item. Commit runs the normal Phase 2 rename, which gives two undo steps, as in Explorer.
5. **If the active filter hides the new item** (a filter text, kind or type filter, or a dot-name with hidden items off), do not open an editor. Show a toast instead: "Created "New folder" (hidden by the current filter)". *Alternative to decide on: clear the footer filter.*
6. **If the user navigates away** before `Created` arrives, drop `pending_rename`.
7. **Tests:**
   - Ctrl+Shift+N twice creates "New folder" and then "New folder (2)".
   - Esc keeps the folder.
   - Typing a name renames it, and two Ctrl+Z presses remove it.
   - New file selects "New Text Document".
   - A watcher refresh before `Created` does not break the flow.

### Phase 4: Explorer polish

1. **Tab / Shift+Tab.** `CommitAndMove(±1)` commits, then calls `begin_rename` on the next or previous row in view order (`pane.indices`). At the ends, stop and just commit (⚠ verify against Explorer). With the optimistic label, the user can keep going without waiting for the job.
2. **Slow click to rename** (in `ListingClicks`, `listing.rs:10-106`). Start it on a primary release when all of these are true:
   - the press landed on the name galley rect, not the icon or blank space
   - the row was the only selected item
   - the listing owned focus before the press
   - there was no drag
   - Then arm a timer of `GetDoubleClickTime()` (native call; fall back to egui's value), using `request_repaint_after`. A second click, a drag, a selection change or a keypress disarms it.
   - Make this an Options toggle, "Click a selected name to rename", on by default.
3. **Extension-change confirmation.** A small modal "Rename" dialog, using the warning style of the existing `components::dialog`, with Yes/No.
   - **Yes:** submit.
   - **No:** reopen the editor with the typed text (⚠ verify against Explorer; reverting is the alternative).
4. **Editor context menu.** Right-clicking inside the editor shows Undo, Cut, Copy, Paste, Delete and Select all, rendered with Foldex's own menu components. It must not open the file context menu.
5. **Accessibility.** Set the editor's accessible name to `Rename {name}`. Check that AccessKit exposes the text value and the selection. Add the editor to the pane's accessibility tree.

### Phase 5: The other surfaces

1. **Search results.** The editor works through the same listing. After a commit:
   - Patch the entry in place, both `raw_name`/`name` and `relative_path`, so the result does not vanish while the index catches up. A recursive refresh clears the selection.
   - Notify the index (check what `foldex-index` exposes for an incremental update).
2. **Home: Recent rows and Recommended cards** (`home_ui.rs`).
   - A single-line editor over the row's name rect; on cards, over the label rect.
   - Home has only one selection, `home_selected: Option<PathBuf>`.
   - After a commit, remap the history entry from the old path to the new one in `home_history` (a new `remap` API). Otherwise the item disappears from Recent, because its old path no longer validates.
3. **Shell New templates** (Word, zip, bitmap and so on, invoked through `system.invoke` in `context.rs:785`).
   - Before the invoke, record the folder's current name set.
   - After the next snapshot, if exactly one new entry appeared, `begin_rename` on it with the stem selected.
   - Otherwise do nothing. Today these items are created with no rename step at all.
4. **Cut state.** If the renamed item was on the clipboard as a cut, update or clear the path in `cut_state` so it does not stay dimmed under a dead path.

### Phase 6: Multi-selection Rename — decided: keep the batch dialog (option B)

- No change here. F2 or Rename with several items selected still opens the `BatchRename` modal (`Action::RenameMany`, `app.rs:985`).
- Only single-item rename and new items become inline.
- **Tab / Shift+Tab (Phase 4)** moves within the current listing regardless of selection. Moving the editor to another item collapses the selection to that item, so it never turns into a multi-item rename.
- **Rejected alternative (A):** Explorer-style `Name (1)…(n)` numbering from an inline editor.

### Phase 7: Cleanup

- Delete `EditDialog`, `Foldex.edit`, and the dialog code at `app.rs:1683-1780`.
- Update the website Changelog and Documentation pages (`web/`) to describe inline rename, slow click and Tab-to-next.

### Non-goals

- Renaming sidebar bookmarks or places, tabs or workspaces. None of these support rename today. Explorer's navigation-tree rename has no equivalent here, because Foldex has no folder tree.
- Hidden-extension mode. Foldex always shows extensions.
- Copying Explorer's quirks:
  - Ctrl+Backspace inserting a box character.
  - Background refreshes cancelling the edit.

---

## 5. Explorer behaviours to confirm by hand (about 5 minutes on Windows 11)

I could not drive Explorer directly; access to it was declined in this session. These are the unconfirmed points that would change code:

1. Does Tab on the last item wrap to the first item, or stop?
2. In the extension-change prompt, does **No** revert the name or reopen the editor?
3. Does a created or renamed item re-sort immediately after commit, or does it stay in place until F5?
4. Are **leading** spaces trimmed?
5. Does mouse-wheel scrolling while editing commit or cancel?
6. Does renaming a folder to the name of an existing folder show an error, offer "(2)", or offer to merge? (It probably comes from the shell dialog, so Foldex would inherit the same behaviour.)

## 6. Risks

- **egui focus.** A `TextEdit` that is not drawn in a frame loses focus. The editor must be drawn every frame while it is open, or closed deliberately. Never leave it orphaned.
- **Input order.** Keys must be consumed before `hotkeys.dispatch`, and Enter must be taken before the `TextEdit` sees it. The early and late keyboard passes (`app.rs:3095`, `:3300`) run around the pane paint, so the editor must handle its keys inside `listing()`, and the late pass must not see them.
- **Identity change on rename.** `listing_motion` will treat the renamed row as a new item and skip the reflow animation. That is acceptable, but check that it does not flash.
- **Final name from the shell.** If the user accepts the shell's "(2)" auto-rename, the final name differs from the typed name. `PostRenameItem` capture (Phase 2, step 6) is required, or re-selection picks the wrong item.
