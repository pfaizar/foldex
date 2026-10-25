# Foldex @VERSION@

Launch Foldex from Start, the optional desktop shortcut, or `Foldex` in a new
terminal if you selected Add to PATH. This is a per-user installation and does
not require administrator rights. Windows 10 x64 (1809+) and DirectX 12 are required.

Use Options → Application Updates to check for updates or enable automatic
downloads. Installation happens only when you choose Install and restart.
Running another installer upgrades this same copy; older versions are blocked.

Preferences, themes, sessions and caches live in `%LOCALAPPDATA%\Foldex\data`.
Updates and repairs preserve this folder. To migrate from a portable copy, exit
both copies and copy its theme.toml, foldex-session.json, foldex-preferences.json
(when present) and themes directory
there. The portable copy is not deleted or changed by Setup.

Uninstall through Windows Settings → Apps → Installed apps → Foldex. Exit every
Foldex window and its tray instance first. Uninstall removes the installed
program, shortcuts, its owned PATH entry, and restores Windows folder integration
that still belongs to this installation. Personal data is preserved; delete
`%LOCALAPPDATA%\Foldex` after uninstall if you also want to erase preferences.

Builds are currently unsigned. In-app updates verify GitHub's SHA-256 asset
digest over HTTPS; a checksum is not a publisher signature.

Source: @SOURCE_URL@
Source archive: @SOURCE_ARCHIVE_URL@
License: AGPL-3.0-only. See LICENSE and THIRD-PARTY-NOTICES.md.
