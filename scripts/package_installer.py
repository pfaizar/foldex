"""Build the Inno Setup EXE from a validated, clean portable release archive."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess
import tempfile
import zipfile

import package_portable as portable


def extract_archive(archive: Path, destination: Path, version: str, commit: str) -> Path:
    prefix = f"Foldex-{version}-windows-x64-portable"
    info = json.loads(archive.with_name(f".{archive.stem}.json").read_text(encoding="utf-8"))
    if info["version"] != version or info["commit"] != commit or info["tag"] != f"v{version}":
        raise ValueError("Portable archive does not match the checked-out release")
    if hashlib.sha256(archive.read_bytes()).hexdigest() != info["archive_sha256"]:
        raise ValueError("Portable archive differs from CI's validated package")
    with zipfile.ZipFile(archive) as zipped:
        names = zipped.namelist()
        if len(names) != len(set(names)):
            raise ValueError("Duplicate portable archive entries")
        for name in names:
            path = PurePosixPath(name)
            if (path.is_absolute() or ".." in path.parts or "\\" in name or ":" in name
                    or not path.parts or path.parts[0] != prefix):
                raise ValueError("Unsafe portable archive entry")
        binary = zipped.read(f"{prefix}/Foldex.exe")
        portable.validate_x64(binary)
        if hashlib.sha256(binary).hexdigest() != info["executable_sha256"]:
            raise ValueError("Portable executable checksum does not match the validated package")
        zipped.extractall(destination)
    return destination / prefix


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--iscc", default="ISCC.exe")
    args = parser.parse_args()
    version = portable.version()
    portable.validate_tag(args.tag, version)
    commit = portable.run("git", "rev-parse", "HEAD")
    if portable.run("git", "rev-parse", f"{args.tag}^{{commit}}") != commit:
        raise ValueError("The release tag does not point to the checked-out source")
    if portable.run("git", "status", "--porcelain=v1", "--untracked-files=normal"):
        raise ValueError("Commit source changes before packaging a release")
    output = portable.ROOT / "dist"
    archive = output / f"Foldex-{version}-windows-x64-portable.zip"
    installer = output / f"Foldex-{version}-windows-x64-setup.exe"
    # A failed compile must never leave an earlier successful installer publishable.
    installer.unlink(missing_ok=True)
    with tempfile.TemporaryDirectory(prefix=".foldex-installer-", dir=output) as temporary:
        stage = extract_archive(archive, Path(temporary), version, commit)
        readme = (portable.ROOT / "packaging/windows/installed-README.md").read_text(encoding="utf-8")
        readme = readme.replace("@VERSION@", version).replace("@SOURCE_URL@", f"{portable.REPOSITORY}/tree/{commit}")
        readme = readme.replace("@SOURCE_ARCHIVE_URL@", f"{portable.REPOSITORY}/archive/refs/tags/{args.tag}.zip")
        (stage / "README.md").write_text(readme, encoding="utf-8")
        (stage / "foldex-installed.ini").write_text(f"[Installation]\nVersion={version}\n", encoding="utf-8")
        subprocess.run([args.iscc, f"/DAppVersion={version}", f"/DSourceDir={stage}",
            f"/DReleaseDir={output}", str(portable.ROOT / "packaging/windows/foldex.iss")], check=True)
    if not installer.is_file() or installer.stat().st_size < 1024:
        raise ValueError("Inno Setup did not produce a complete installer")
    archive.with_name(f".{archive.stem}.json").unlink()
    print(json.dumps({"installer": str(installer)}))


if __name__ == "__main__":
    main()
