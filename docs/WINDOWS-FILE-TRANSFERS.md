# Windows file-transfer UX

## Investigation

The native operation wrapper deliberately kept the default `IFileOperation`
progress dialog enabled for every copy, move, rename and deletion, without a
presentation delay or a guard against opening on the final progress update.
That allowed a fast operation to produce a fleeting window already at 100%.

File rows requested drag input but never dispatched a file drag. Separately,
marquee selection armed on every press anywhere inside the listing, including
on a file. Incoming drops were processed before layout and always targeted the
keyboard-active pane. Winit's generic Windows target also advertised Copy for
every drag, regardless of the modifiers or the transfer the app would execute.

## Progress policy

Microsoft recommends avoiding distracting progress bars for short operations
and providing progress and cancellation for lengthy work. Foldex uses the
documented `IFileOperation::SetProgressDialog` extension point to insert a small
`IOperationsProgressDialog` adapter in front of the native dialog:

- Start the operation immediately; do not sleep or delay the filesystem work.
- Keep progress hidden for the first second.
- Do not start a window on a completion update, even if it arrives after the
  delay. Zero totals represent indeterminate work.
- Cache action, mode, location and progress until presentation, then forward
  updates and native cancellation/status/timer calls.
- Stop visible progress on completion or teardown; there is no timer thread
  that could open a window after the operation has finished.

Collision, warning, error, retry and elevation behavior still belongs to the
Shell. No global silent/auto-confirm/error-suppression flags are introduced.
The adapter is shared by the normal file-operation worker and direct permanent
deletion of Recycle Bin selections. Dialogs owned by independent Shell verbs or
other applications remain under their owner's control.

References: [Microsoft progress guidance](https://learn.microsoft.com/en-us/windows/win32/uxguide/progress-bars),
[SetProgressDialog](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ifileoperation-setprogressdialog),
[IOperationsProgressDialog](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ioperationsprogressdialog).

## Drag-and-drop policy

A drag beginning on a loaded file or folder preserves an existing multiple
selection, or selects just that item if it was not selected. Only presses in
empty space can arm marquee selection. An unresolved selection cannot export a
partial set of paths.

The source uses a Shell `IDataObject` built from absolute item identities and
`SHDoDragDrop`, offering copy, move and link. The window registers an OLE
`IDropTarget` that hit-tests the visible folder under the native client position,
with DPI conversion. Folder rows override their pane's background. Sidebar
folders and breadcrumb ancestors are destinations too. Virtual content and
modal overlays do not become filesystem destinations.

The destination folder's own Shell drop target negotiates the effect and handles
external sources, preserving Windows' modifier, volume and source-cleanup rules.
For a drag originating in the same Foldex window, copy and move are submitted to
the existing bounded operation worker after the drag ends. These operations keep
Foldex's history, conflict handling and delayed progress. The OLE source receives
`DROPEFFECT_NONE` for that deferred handoff so it cannot independently delete
originals. Link drops and transfers involving other windows/apps use the native
Shell protocol. The consumed mouse release is returned to egui to prevent a
stuck pressed button after cancellation or completion.

References: [SHDoDragDrop](https://learn.microsoft.com/en-us/windows/win32/api/shlobj_core/nf-shlobj_core-shdodragdrop),
[IShellItem::BindToHandler](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nf-shobjidl_core-ishellitem-bindtohandler),
[IDropTarget](https://learn.microsoft.com/en-us/windows/win32/api/oleidl/nn-oleidl-idroptarget).

## Broader workflow assessment

The repository already implements navigation history, tabs and splits, filename
search, file creation and renaming, clipboard copy/cut/paste, recycling and
permanent deletion, compatible-operation undo/redo, native context menus,
properties, shortcuts, appearance settings and installed-app updates. The
concrete missing interactions found in this review were file-drag initiation,
folder-level destinations, cross-pane routing, and correct native transfer
negotiation. This change fills those gaps together instead of introducing
unrelated features.

## Validation

The normal suite exercises the progress adapter with reference-counted COM
fakes: short and completed operations, delayed and indeterminate work, large
64-bit progress, cancellation, retained locations, timers and teardown. UI input
tests cover file dragging in details and icon views, preservation of multiple
selection, and empty-space marquee selection. Target tests cover folder priority,
inactive panes and stale geometry.

On the Linux development host, run the workspace with `--no-default-features
--features foldex/renderer-glow`. Two existing tests depend on Windows behavior:
`launcher::tests::default_roots_do_not_scan_verbatim_home_twice` and
`search::tests::repeated_unfinished_refreshes_do_not_accumulate_owned_generations`.
The latter relies on Windows open-file deletion guards; the Unix backend
deliberately retains generations. Exclude those two from the Linux run; the
normal Windows CI suite runs them without exclusions. Cross-check the Windows
application and tests with `cargo check --locked -p foldex --tests --target
x86_64-pc-windows-gnu`.

Windows desktop smoke checks:

1. Rename, recycle and permanently delete generated small files: no progress
   flash. Copy a large file: delayed native progress and cancellation work.
2. Drag a file onto a child folder in details and icon views; undo the transfer.
   Repeat with a multiple selection, Ctrl-copy and Shift-move.
3. Drop onto the inactive split pane, a breadcrumb ancestor and a sidebar folder.
4. Drag to and from Explorer, and into an application that accepts file drops.
   Verify source preservation for copies and removal after completed moves.
5. Press Escape during a drag, then click and drag again. Check an invalid/self
   destination and a name collision. Repeat at a non-default display scale.

These interactive checks require a Windows desktop; cross-compilation does not
establish visual behavior or compatibility with every third-party drop handler.
