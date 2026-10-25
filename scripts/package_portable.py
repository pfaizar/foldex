"""Package a tagged x64 build without reading the user's live dist contents."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tempfile
import tomllib
import zipfile


ROOT = Path(__file__).resolve().parent.parent
TARGET = "x86_64-pc-windows-msvc"
REPOSITORY = "https://github.com/pfaizar/foldex"
NOTICE_NAME = re.compile(r"licen[cs]e|copying|copyright|notice|^OFL|^UFL", re.I)


def run(*args: str) -> str:
    return subprocess.run(
        args, cwd=ROOT, check=True, capture_output=True, encoding="utf-8", errors="replace"
    ).stdout.strip()


def version() -> str:
    with (ROOT / "Cargo.toml").open("rb") as stream:
        return tomllib.load(stream)["workspace"]["package"]["version"]


def validate_tag(tag: str, package_version: str) -> None:
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?", tag):
        raise ValueError(f"Invalid release tag: {tag!r}")
    if tag != f"v{package_version}":
        raise ValueError(f"Tag {tag} does not match Cargo version {package_version}")


def validate_x64(binary: bytes) -> None:
    if len(binary) < 64 or binary[:2] != b"MZ":
        raise ValueError("The release binary is not a Windows executable")
    offset = struct.unpack_from("<I", binary, 0x3C)[0]
    if offset + 6 > len(binary) or binary[offset : offset + 4] != b"PE\0\0":
        raise ValueError("The release binary has no valid PE header")
    if struct.unpack_from("<H", binary, offset + 4)[0] != 0x8664:
        raise ValueError("The release binary is not x64")


def runtime_packages() -> tuple[dict, list[dict]]:
    metadata = json.loads(
        run("cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", TARGET)
    )
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = [next(package["id"] for package in metadata["packages"] if package["name"] == "foldex")]
    seen = set()
    while pending:
        identifier = pending.pop()
        if identifier in seen:
            continue
        seen.add(identifier)
        pending.extend(
            dependency["pkg"]
            for dependency in nodes[identifier]["deps"]
            if any(kind["kind"] is None for kind in dependency["dep_kinds"])
        )
    return metadata, sorted((packages[identifier] for identifier in seen), key=lambda item: (item["name"], item["version"]))


def write_notices(destination: Path, packages: list[dict]) -> int:
    notices = destination / "licenses"
    notices.mkdir(parents=True)
    index = [
        "# Third-party notices",
        "",
        "Foldex's original code is licensed under AGPL-3.0-only. Dependencies and",
        "adapted components retain their own licenses and copyright notices.",
        "This inventory follows the resolved normal dependency graph for Windows x64;",
        "some entries are supporting or generated-code crates rather than separate DLLs.",
        "",
        "Where an upstream crate omits its dual-license files, this binary distribution",
        "elects Apache-2.0 and includes that license. Font licenses are included separately.",
        "",
    ]
    count = 0
    for package in packages:
        source = Path(package["manifest_path"]).parent.resolve()
        if source.is_relative_to(ROOT / "crates"):
            continue
        label = f"{package['name']}-{package['version']}"
        folder = notices / label
        folder.mkdir()
        files = sorted(
            path for path in source.rglob("*")
            if path.is_file() and NOTICE_NAME.search(path.name)
            and path.suffix.lower() not in {".rs", ".c", ".h", ".ttf", ".woff", ".woff2"}
        )
        root_license = any(path.parent == source for path in files)
        if not root_license:
            expression = package.get("license") or ""
            if "Apache-2.0" in expression:
                shutil.copyfile(ROOT / "vendor/epaint/LICENSE-APACHE", folder / "LICENSE-APACHE")
            elif expression == "BSL-1.0":
                shutil.copyfile(ROOT / "packaging/licenses/BSL-1.0.txt", folder / "LICENSE-BSL-1.0.txt")
            else:
                raise ValueError(f"No upstream license text available for {label}: {expression}")
        for path in files:
            relative = path.relative_to(source)
            output = folder / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, output)
        patch = source / "FOLDEX-PATCH.md"
        if patch.is_file():
            shutil.copyfile(patch, folder / patch.name)
        if package["name"] == "epaint_default_fonts":
            for name in ["Hack-Regular.txt", "OFL.txt", "UFL.txt", "emoji-icon-font-mit-license.txt"]:
                source_notice = source / "fonts" / name
                if not source_notice.is_file():
                    raise ValueError(f"Bundled font notice is missing: {name}")
                output = folder / "fonts" / name
                output.parent.mkdir(exist_ok=True)
                shutil.copyfile(source_notice, output)
        upstream = package.get("repository") or f"https://crates.io/crates/{package['name']}/{package['version']}"
        index.extend([
            f"## {package['name']} {package['version']}",
            "",
            f"Declared upstream license: `{package.get('license') or 'see notice files'}`.",
            f"Notices: [{label}](licenses/{label}/).",
            f"Upstream: {upstream}",
            "",
        ])
        count += 1
    rolling = (ROOT / "crates/foldex-ui/src/rolling.rs").read_text(encoding="utf-8").splitlines()
    start = rolling.index("//! MIT License")
    text = []
    for line in rolling[start:]:
        if not line.startswith("//!"):
            break
        text.append(line[3:].removeprefix(" "))
    if not any("Kit Langton" in line for line in text):
        raise ValueError("The adapted rolling-number attribution was not found")
    (notices / "rolling-number-MIT.txt").write_text("\n".join(text) + "\n", encoding="utf-8")
    index.extend(["## Rolling Number adaptation", "", "Copyright (c) 2026 Kit Langton.",
                  "See [the retained MIT notice](licenses/rolling-number-MIT.txt).", ""])
    (destination / "THIRD-PARTY-NOTICES.md").write_text("\n".join(index), encoding="utf-8")
    return count


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--validate-only", action="store_true")
    parser.add_argument("--notices-only", action="store_true")
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    package_version = version()
    validate_tag(args.tag, package_version)
    notes = ROOT / "docs/releases" / f"{args.tag}.md"
    if not notes.is_file():
        raise ValueError(f"Release notes are missing: {notes.relative_to(ROOT)}")
    if args.validate_only:
        print(json.dumps({"version": package_version, "tag": args.tag}))
        return
    metadata, packages = runtime_packages()
    output = args.output.resolve()
    if not output.is_relative_to(ROOT):
        raise ValueError("Release outputs must stay inside the project")
    output.mkdir(parents=True, exist_ok=True)
    if args.notices_only:
        count = write_notices(output, packages)
        print(json.dumps({"upstream_packages": count, "output": str(output)}))
        return
    commit = run("git", "rev-parse", "HEAD")
    if run("git", "rev-parse", f"{args.tag}^{{commit}}") != commit:
        raise ValueError("The release tag does not point to the checked-out source")
    if run("git", "status", "--porcelain=v1", "--untracked-files=normal"):
        raise ValueError("Commit source changes before packaging a release")
    binary_path = Path(metadata["target_directory"]) / TARGET / "release/foldex.exe"
    binary = binary_path.read_bytes()
    validate_x64(binary)
    label = f"Foldex-{package_version}-windows-x64-portable"
    archive = output / f"{label}.zip"
    checksums = output / f"Foldex-{package_version}-SHA256SUMS.txt"
    with tempfile.TemporaryDirectory(prefix=".foldex-package-", dir=output) as temporary:
        stage = Path(temporary).resolve()
        if not stage.is_relative_to(output):
            raise ValueError("Temporary release folder leaves the output directory")
        folder = stage / label
        folder.mkdir()
        (folder / "Foldex.exe").write_bytes(binary)
        for name in ["LICENSE", "theme.toml"]:
            shutil.copyfile(ROOT / name, folder / name)
        readme = (ROOT / "packaging/portable-README.md").read_text(encoding="utf-8")
        readme = readme.replace("@VERSION@", package_version).replace("@SOURCE_URL@", f"{REPOSITORY}/tree/{commit}")
        readme = readme.replace("@SOURCE_ARCHIVE_URL@", f"{REPOSITORY}/archive/refs/tags/{args.tag}.zip")
        (folder / "README.md").write_text(readme, encoding="utf-8")
        (folder / "BUILD-INFO.json").write_text(json.dumps({
            "version": package_version, "tag": args.tag, "commit": commit,
            "target": TARGET, "renderer": "DirectX 12", "license": "AGPL-3.0-only",
            "source": f"{REPOSITORY}/tree/{commit}", "signed": False,
            "executable_sha256": hashlib.sha256(binary).hexdigest(),
        }, indent=2) + "\n", encoding="utf-8")
        notice_count = write_notices(folder, packages)
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zipped:
            for path in sorted(folder.rglob("*")):
                if path.is_file():
                    zipped.write(path, path.relative_to(stage).as_posix())
        with zipfile.ZipFile(archive) as zipped:
            if zipped.testzip() is not None:
                raise ValueError("ZIP integrity verification failed")
            expected = {path.relative_to(stage).as_posix() for path in folder.rglob("*") if path.is_file()}
            if set(zipped.namelist()) != expected:
                raise ValueError("ZIP inventory differs from the clean staging folder")
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    checksums.write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    print(json.dumps({"archive": str(archive), "checksums": str(checksums), "sha256": digest,
                      "upstream_packages": notice_count, "commit": commit}))


if __name__ == "__main__":
    main()
