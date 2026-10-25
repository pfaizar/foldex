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
the portable ZIP and SHA-256 checksum file. Versions beginning with `0.` and
versions containing a prerelease suffix are marked as GitHub prereleases.

To start or retry a build of an existing tag without moving that tag, use the
Release workflow's **Run workflow** button and supply the tag, or run:

```powershell
gh workflow run release.yml --repo pfaizar/foldex --ref main -f tag=v0.1.0
```

Release tags identify published source and must not be moved to different commits.
Fixes should receive a new version and tag.

## Local packaging

Python 3.11+ and the pinned Rust/MSVC build environment are required. From a clean
checkout of the release tag:

```powershell
cargo build --locked --release --target x86_64-pc-windows-msvc -p foldex
python -B scripts/package_portable.py --tag v0.1.0
```

The script creates a fresh staging folder and copies only the compiled EXE, stock
theme, README, license, build information, and generated dependency notices. It
does not copy the contents of a live `dist/` folder. Missing license texts, an
incorrect CPU architecture, a mismatched tag, and uncommitted source changes stop
packaging. The source checkout and tag must remain available for AGPL recipients.

## Before broader distribution

Test on clean desktop Windows installations, including runtime prerequisites,
graphics hardware, file operations, upgrade behavior, and user settings. The
current portable build is unsigned; signing and installer behavior require their
own release work.
