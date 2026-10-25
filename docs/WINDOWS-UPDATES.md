# Windows installation and updates

## Design decisions and research

Foldex uses Inno Setup rather than replacing its own executable. A fixed
`Foldex.Desktop` AppId gives every release one uninstall identity; upgrades reuse
the saved directory and refuse another destination or an older SemVer. A
same-version repair is allowed. The setup wizard exposes location, optional
desktop shortcut and optional user PATH, and always registers Start and Windows
Installed apps. It does not request elevation or install a service.

These choices follow the primary documentation:

- [Microsoft's installation guidance](https://blogs.windows.com/windowsdeveloper/2025/05/19/enhance-your-application-security-with-administrator-protection/)
  recommends unelevated per-user installation.
- [Inno AppId](https://jrsoftware.org/ishelp/topic_setup_appid.htm) and
  [UsePreviousAppDir](https://jrsoftware.org/ishelp/topic_setup_usepreviousappdir.htm)
  explain persistent upgrade/uninstall identity and location reuse.
- [SetupMutex](https://jrsoftware.org/ishelp/topic_setup_setupmutex.htm) and
  [AppMutex](https://jrsoftware.org/ishelp/topic_setup_appmutex.htm) describe setup
  serialization and the running-application contract. Foldex checks its hashed
  running mutex after directory selection and before uninstall. Inno expands
  `[Setup] AppMutex` before first-install directory selection, so that directive
  alone would record the wrong mutex for a custom directory. A second atomic
  installation transaction mutex prevents concurrent mutation and startup races.
- [CloseApplications](https://jrsoftware.org/ishelp/topic_setup_closeapplications.htm)
  documents Restart Manager and the risk of force closing. Foldex disables it;
  users finish their file operations before applying updates.
- [Inno's scripting events](https://jrsoftware.org/ishelp/topic_scriptevents.htm)
  define validation before installation and uninstall cleanup before files are
  removed. Cleanup failure aborts uninstall, leaving the executable available to
  retry. The upstream 6.7 source was checked for event ordering.
- [ChangesEnvironment](https://jrsoftware.org/ishelp/topic_setup_changesenvironment.htm)
  notifies Explorer after PATH changes. Setup preserves the original registry
  string type, deduplicates entries, and records ownership. Uninstall removes
  only an owned installation entry, keeping every unrelated segment and type.
- [GitHub's release asset API](https://docs.github.com/en/rest/releases/assets)
  supplies the SHA-256 digest used for download verification.

## Updater behavior

Options → Application Updates has manual checks, automatic checks (default),
optional automatic downloads (off by default), and optional prerelease versions.
Checks run on one worker after a 15-second startup delay and then every six hours
while the app runs. Manual checks have a one-minute cooldown. Auxiliary windows,
benchmarks, tests and helpers do not start update network activity. Disabling
automatic checks stops scheduled requests; users can still check manually.

Discovery uses the newest compatible SemVer among the newest 100 public releases,
ignoring drafts. Numeric 0.x releases marked as GitHub previews are the normal
stream; explicit SemVer prereleases and 1.x releases marked prerelease require
opt-in. No downgrade or same-version update is offered. Release URLs and asset
names must match this repository exactly. Metadata is bounded to 4 MiB,
installers to 512 MiB, connections to ten seconds, each read to twenty seconds,
metadata requests to thirty seconds, and a download request to fifteen minutes.

HTTPS uses the platform certificate verifier and supported proxy configuration.
Download redirects are limited to GitHub's HTTPS asset hosts. Downloads are
streamed into a unique temporary directory; size and SHA-256 must match GitHub's
API metadata. Partial/cancelled/corrupt downloads are removed. No ZIP extraction
or command shell is involved in in-app updates. Old retained staging directories
are cleaned after seven days. Checksum validation authenticates the download
against GitHub's metadata; it is not an independent publisher signature. Official
builds remain unsigned until a signing identity is configured separately.

Install and restart rechecks release metadata, checksum and other Foldex
processes off the UI thread. The app then checks queued and active work again,
saves its session and theme, exits the tray, and passes the installer to its
post-event-loop handoff. Setup waits for the old PID to exit before it checks
running mutexes or replaces files. It upgrades the registered location silently
with visible progress and relaunches Foldex on success. It never force closes
other windows or schedules replacement on reboot. A failed/cancelled download,
preflight, session save or installer start leaves the installed version intact.
The installer log remains available through Inno Setup's normal logging.

Installed data is `%LOCALAPPDATA%\Foldex\data`. Portable copies retain their
beside-executable data and offer release checks and a link to the installer;
they do not silently convert to installed mode. Uninstall preserves personal
data and restores Windows integration only if its backup belongs to this EXE.
Values subsequently changed by another application are preserved.

## Validation and remaining desktop checks

The implementation received five review passes: installer identity/process
coordination; release trust and bounded downloads; UI/worker lifecycle and save
failures; personal-data/PATH/Explorer ownership; and packaging/publication failure
paths. Fixes from those passes include checking the final custom installation
directory, releasing Options keyboard ownership before opening Updates, stopping
on theme-save failures, and preserving other copies' Explorer registrations.

Local validation includes 578 passing application tests and Windows-target
cross-compilation of the workspace and test targets. Two existing application
tests are excluded from the final Linux run: Windows verbatim-path semantics in
`launcher::tests::default_roots_do_not_scan_verbatim_home_twice`, and
`search::tests::repeated_unfinished_refreshes_do_not_accumulate_owned_generations`.
The search-cache failure was reproduced against an untouched checkout of the
base commit. Both tests remain enabled in Windows CI; this change does not edit
either implementation or test.

The PR is **untested on a real Windows desktop**. Linux unit tests and Windows
cross-compilation cannot establish actual installer UI, Windows Shell, driver,
SmartScreen or updater restart behavior. CI contains a disposable Windows smoke
test for custom Unicode paths, shortcuts, PATH type/ownership, concurrent setup,
running-app blocks, prerelease ordering, upgrades, repairs, downgrade rejection,
single install identity and uninstall with personal-data preservation.

Before a public release, run the Windows CI suite and manually exercise:

1. Fresh install under a standard Windows account and a custom directory. Launch
   from Start, desktop and a new terminal. Check Installed apps and uninstall.
2. Install over an earlier version and repair the same version. Start two setup
   EXEs, attempt a downgrade, and launch Foldex while Setup is changing files.
3. Publish two disposable versions in a test fork; point a test build's updater
   repository at the fork. Check discovery, download progress, auto-download,
   cancellation, offline/rate-limit errors and restart into the new version.
4. Try installing with a copy/move queued or active, a native conflict dialog,
   an auxiliary window, tray-only mode, a pending tab transfer and a failing
   session/theme save. Confirm no forced exit or premature installation.
5. Change another app's folder registration after enabling Foldex integration,
   then uninstall. Confirm that app's values survive and Foldex's own values
   restore. Check that portable settings and personal data remain intact.
6. Cancel Setup, deny access to the target, and interrupt a download. Verify the
   existing app still launches; check Inno's recovery after failed file copying.
   Power loss during installation requires normal Windows installer recovery;
   no application can promise zero failure cases under every interruption.
