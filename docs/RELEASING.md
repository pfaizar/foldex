# Releasing Foldex

Windows downloads are GitHub release assets. Generated files, user settings and
build staging stay out of Git and out of the published downloads.

## Publish a version

1. Update `workspace.package.version` in `Cargo.toml` and the matching workspace
   entries in `Cargo.lock`. Independently versioned library crates may retain
   their own version.
2. Add `docs/releases/vVERSION.md` with release notes.
3. Commit and push the version and notes together to `main`.

A change to release notes on `main` starts the Release workflow. It checks the
version, formatting, Windows tests, installer lifecycle and packaging tests;
builds the application; creates the matching source tag if it does not exist;
and publishes the setup EXE and portable ZIP. GitHub adds source ZIP and tar.gz
links automatically.

Published downloads contain no separate checksum file or `BUILD-INFO.json`.
Portable downloads retain the stock theme, short README and required license
notices. Internal packaging validation records are consumed inside CI and are
not uploaded. The updater still verifies GitHub's SHA-256 asset digest.

Versions beginning with `0.` and versions with a prerelease suffix are marked
as GitHub prereleases. Numeric `0.x` versions remain in the normal update stream.

Existing tags also trigger the workflow when pushed. To retry an unpublished
tag, use the Release workflow's **Run workflow** button and supply that tag.
Published tags must not be moved, and published binary assets must not be replaced:
fixes receive a new version and tag.

## Local packaging

For manual builds, Python 3.11+, Inno Setup 6.7.0 and the pinned Rust/MSVC
environment are required. From a clean checkout of a release tag:

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --locked --release --target x86_64-pc-windows-msvc -p foldex
python -B scripts/package_portable.py --tag v0.1.1
python -B scripts/package_installer.py --tag v0.1.1 --iscc 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
```

The portable packager uses a clean staging folder. The installer packager checks
the source version, archive and executable against CI's internal staging record,
then consumes that record. It does not ship it. Both formats include required
third-party notices and source links. Release builds statically link the Visual
C++ runtime.

Keep the source tag available. Do not change the installer AppId, mutex names,
installed-mode marker or asset naming without a migration plan. The updater
requires the exact `Foldex-VERSION-windows-x64-setup.exe` asset under `vVERSION`.

## Desktop checks

Verify installation and updates on Windows desktop hardware, including graphics,
file operations, saved settings, manual cancellation and update/restart. Builds
are unsigned. See [the installer and updater validation notes](WINDOWS-UPDATES.md).
