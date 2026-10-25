# Foldex @VERSION@ — Windows x64 portable

Extract this entire ZIP into a writable folder, then run `Foldex.exe`. Keep
`LICENSE`, `THIRD-PARTY-NOTICES.md`, and the `licenses` folder with the executable.

Requires Windows 10 x64 (1809+) and a DirectX 12-capable graphics adapter and driver.
Official releases statically link the Visual C++ runtime.
This initial public preview has limited desktop hardware coverage.

Use **Ctrl+Alt+Space** for the launcher, **Ctrl+,** for Options, and **F1** for the
keyboard reference. Closing the browser can keep Foldex in the system tray;
choose **Exit Foldex** from its tray menu to quit before updating.

The application creates `foldex-session.json`, `foldex-cache`, and other runtime
data beside the executable. Preserve your existing session, theme, and named
themes when updating. After exiting Foldex, you can delete `foldex-cache` to
rebuild generated search data.

Options → Application Updates can check for releases. To enable in-app update
installation, use the separate Windows setup EXE. Exit both copies and copy your
portable theme, session and themes directory into `%LOCALAPPDATA%\Foldex\data`
to migrate preferences. The installer preserves the portable folder.

If you enable **Default File Explorer** in Options, turn it off before moving or
deleting the portable folder. This restores Windows folder and Win+E registrations.

This build is unsigned. Windows may show an unknown-publisher or SmartScreen
prompt. Download it from the project's GitHub release and verify its SHA-256
checksum against the accompanying checksum file.

Copyright (c) 2026 Foldex contributors. Foldex is licensed under AGPL-3.0-only;
see `LICENSE`. Third-party components retain their upstream licenses and notices.

Corresponding source for this version: [browse the source](@SOURCE_URL@) or
[download the source ZIP](@SOURCE_ARCHIVE_URL@). Build instructions are in that
source tree's README. `BUILD-INFO.json` records the exact commit and build target.
