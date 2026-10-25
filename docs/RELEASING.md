# Releasing Foldex

Source code, the lockfiles, artwork, third-party patches, and release scripts are
tracked in Git. `dist/`, local backups, generated files, and user state are ignored.
Distribute Windows downloads as GitHub release assets instead of committing EXEs
or ZIPs to the repository.

## Prepare a version

1. Update `workspace.package.version` in `Cargo.toml` and regenerate `Cargo.lock`.
   Keep the three independently versioned workspace packages consistent if their
   versions change.
2. Add `docs/releases/vVERSION.md` with release notes and any changed requirements.
3. Run the checks used by CI and review the changes. Do not include personal
   sessions, caches, credentials, or local recovery backups.
4. Commit and push `main`, then confirm that CI passed.
5. Create an annotated tag matching Cargo's version and push that tag:

   ```powershell
   git tag -a v0.1.0 -m "Foldex 0.1.0"
   git push origin v0.1.0
   ```

The Release workflow checks the tag/version and notes, runs the Windows tests,
builds x64 from the tagged source, generates third-party notices, and publishes
the Windows setup EXE, portable ZIP and SHA-256 checksum file. The release build
statically links the Visual C++ runtime. Versions beginning with `0.` and
versions containing a prerelease suffix are marked as GitHub prereleases.

To start or retry a build of an existing tag without moving that tag, use the
Release workflow's **Run workflow** button and supply the tag, or run:

```powershell
gh workflow run release.yml --repo pfaizar/foldex --ref main -f tag=v0.1.0
```

Release tags identify published source and must not be moved to different commits.
Fixes should receive a new version and tag.

## Local packaging

Python 3.11+, Inno Setup 6.7.0, and the pinned Rust/MSVC build environment are required. From a clean
checkout of the release tag:

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'
cargo build --locked --release --target x86_64-pc-windows-msvc -p foldex
python -B scripts/package_portable.py --tag v0.1.0
python -B scripts/package_installer.py --tag v0.1.0 --iscc 'C:\Program Files (x86)\Inno Setup 6\ISCC.exe'
```

The portable script creates a fresh staging folder and copies only the compiled EXE, stock
theme, README, license, build information, and generated dependency notices. It
does not copy the contents of a live `dist/` folder. Missing license texts, an
incorrect CPU architecture, a mismatched tag, and uncommitted source changes stop
packaging. The source checkout and tag must remain available for AGPL recipients.

The installer script validates that archive's version, tag, commit, architecture
and executable checksum, extracts it into a separate staging folder, adds the
installed-mode marker and README, and invokes Inno Setup. It then regenerates
the checksum file to cover both artifacts. The EXE and ZIP are retained as
workflow artifacts and uploaded together. Never change `AppId`, the installer
mutex names, installed-mode marker, or asset naming without a migration plan.

The updater queries the public GitHub releases API. Publish the exact
`Foldex-VERSION-windows-x64-setup.exe` asset under `vVERSION`. GitHub supplies
`digest: sha256:...` for uploaded assets; the updater refuses downloads without
that digest. Do not replace assets under a published version; publish a new
version instead. Numeric `0.x` preview releases remain in the normal update
stream. SemVer prereleases require the user's prerelease setting.

## Before broader distribution

Test on clean desktop Windows installations, including runtime prerequisites,
graphics hardware, file operations, upgrade behavior, and user settings. The
builds are unsigned. Windows desktop validation of the installer and updater is
still required before broad distribution. See [the validation matrix and research](WINDOWS-UPDATES.md).
