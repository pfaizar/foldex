"""Stage the fictional Foldex demo workspace used by the loop recordings.

    python scripts/demo/demo.py stage --root C:\\FoldexDemo --repo .
    python scripts/demo/demo.py reset --root C:\\FoldexDemo --repo . --theme dark

`stage` rebuilds the workspace tree, session, portable app copy and theme.
`reset` does the same and also writes the preferences and update settings used
for recordings. The tree is entirely made up: names, sizes and timestamps are
generated, and no personal files are read.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import re
import shutil
import sys
import time
from pathlib import Path

TREE = {
    "Brand": {
        "logo-mark.svg": 18_400,
        "logo-wordmark.svg": 24_100,
        "palette.ase": 3_200,
        "brand-guidelines.pdf": 8_420_000,
        "Photography": {
            "hero-01.jpg": 4_210_000,
            "hero-02.jpg": 3_880_000,
            "studio-desk.jpg": 5_020_000,
        },
    },
    "Design": {
        "Exports": {
            "onboarding@2x.png": 1_240_000,
            "settings@2x.png": 980_000,
            "dashboard@2x.png": 1_610_000,
        },
        "Archive": {
            "v1-explorations.fig": 18_200_000,
            "moodboard-2025.pdf": 12_400_000,
        },
        "design-system.fig": 42_100_000,
        "wireframes.fig": 18_800_000,
        "icon-set.fig": 6_300_000,
        "tokens.json": 31_000,
        "notes.md": 1_200,
        "handoff-checklist.md": 3_400,
    },
    "Development": {
        "src": {
            "main.rs": 12_400,
            "pane.rs": 48_200,
            "search.rs": 31_700,
            "theme.rs": 9_800,
        },
        "assets": {"icon.ico": 41_000, "splash.png": 220_000},
        "Cargo.toml": 1_900,
        "Cargo.lock": 84_000,
        "README.md": 6_100,
        "build.ps1": 2_300,
        ".gitignore": 140,
    },
    "References": {
        "competitive-review.pdf": 3_900_000,
        "user-interviews.docx": 412_000,
        "survey-results.xlsx": 238_000,
        "inspiration": {
            "ref-01.png": 880_000,
            "ref-02.png": 1_020_000,
            "ref-03.webp": 340_000,
        },
    },
    "Releases": {
        "foldex-0.2.0-setup.exe": 9_800_000,
        "foldex-0.2.0-portable.zip": 11_200_000,
        "release-notes.md": 4_800,
    },
    "Meeting notes": {
        "2026-10-12 kickoff.md": 2_900,
        "2026-10-19 review.md": 3_600,
        "2026-10-26 planning.md": 1_700,
    },
    "launch-plan.md": 2_400,
    "notes.txt": 840,
    "README.md": 1_800,
    "roadmap.xlsx": 64_000,
    "budget-2026.xlsx": 128_000,
    "team-photo.jpg": 3_400_000,
    "quarterly-review.pptx": 7_600_000,
}

LIGHT = {
    "background": [246, 247, 248],
    "sidebar": [237, 240, 242],
    "chrome": [231, 235, 238],
    "surface": [250, 251, 252],
    "hover": [223, 231, 237],
    "border": [207, 214, 220],
    "text": [39, 48, 56],
    "muted": [112, 123, 132],
    "accent": [0, 120, 169],
    "selection": [214, 234, 243],
    "folder": [218, 164, 28],
    "danger": [184, 49, 62],
}


def touch(path: Path, size: int, age_days: float, now: float) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "wb") as f:
        if size:
            f.seek(size - 1)
            f.write(b"\0")
    stamp = now - age_days * 86_400
    os.utime(path, (stamp, stamp))


def build_tree(node: dict, base: Path, now: float) -> None:
    for name, value in node.items():
        age = random.choice([0.05, 0.2, 0.6, 1.2, 1.8, 3, 6, 9, 14])
        if isinstance(value, dict):
            build_tree(value, base / name, now)
            stamp = now - age * 86_400
            os.utime(base / name, (stamp, stamp))
        else:
            touch(base / name, value, age, now)


def pane(path: Path) -> dict:
    return {"path": str(path), "view": {}, "route": None, "navigation": None}


def session_for(data: Path) -> dict:
    return {
        "tabs": [str(data), str(data / "Releases")],
        "active": 0,
        "bookmarks": [
            str(data),
            str(data / "Design"),
            str(data / "Development"),
            str(data / "References"),
        ],
        "recent": [str(data / "Design"), str(data), str(data / "Development")],
        "workspace": {
            "tree": None,
            "tabs": [
                {
                    "left": pane(data),
                    "right": pane(data / "Design"),
                    "vertical": False,
                    "split": 0.5,
                },
                {
                    "left": pane(data / "Releases"),
                    "right": None,
                    "vertical": False,
                    "split": 0.5,
                },
                {
                    "left": pane(data / "Development"),
                    "right": None,
                    "vertical": False,
                    "split": 0.5,
                },
            ],
            "active": 0,
            "active_pane": 0,
            "sidebar": True,
            "sidebar_width": 230.0,
            "inspector": False,
            "inspector_width": 300.0,
        },
    }


def stage(root: Path, repo: Path, app_exe: Path) -> None:
    if len(root.parts) < 2:
        raise SystemExit(f"refusing to replace {root}; use a dedicated folder")
    if not app_exe.is_file():
        raise SystemExit(f"app not found: {app_exe}\nRun 'cargo build --release' first.")
    theme = repo / "theme.toml"
    if not theme.is_file():
        raise SystemExit(f"theme not found: {theme}")

    if root.exists():
        shutil.rmtree(root)
    app = root / "app"
    data = root / "Studio"
    app.mkdir(parents=True)

    random.seed(7)
    build_tree(TREE, data, time.time())

    shutil.copy2(app_exe, app / "Foldex.exe")
    shutil.copy2(theme, app / "theme.toml")
    (app / "foldex-session.json").write_text(
        json.dumps(session_for(data), indent=2), encoding="utf-8"
    )
    print(f"staged {root}")


def theme_text(repo: Path, mode: str) -> str:
    text = (repo / "theme.toml").read_text(encoding="utf-8")
    if mode == "light":
        for key, value in LIGHT.items():
            text = re.sub(rf"(?m)^{key} = \[.*?\]", f"{key} = {value}", text)
        text = text.replace('name = "Graphite"', 'name = "Paper"')
    return text


def reset(root: Path, repo: Path, app_exe: Path, mode: str) -> None:
    stage(root, repo, app_exe)
    app = root / "app"

    (app / "foldex-updates.json").write_text(
        json.dumps(
            {
                "check_automatically": False,
                "download_automatically": False,
                "include_prereleases": False,
            }
        ),
        encoding="utf-8",
    )

    bookmarks = json.loads((app / "foldex-session.json").read_text(encoding="utf-8"))[
        "bookmarks"
    ]
    (app / "foldex-preferences.json").write_text(
        json.dumps(
            {
                "version": 1,
                "revision": 1,
                "preferences": {
                    "bookmarks": bookmarks,
                    "launcher": {
                        "background": True,
                        "clipboard_history": False,
                        "file_roots": [str(root / "Studio")],
                        "hotkey": "Ctrl+Alt+Shift+J",
                    },
                },
            }
        ),
        encoding="utf-8",
    )

    (app / "theme.toml").write_text(theme_text(repo, mode), encoding="utf-8")
    print(f"reset {root} ({mode})")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["stage", "reset"])
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--repo", required=True, type=Path)
    parser.add_argument("--app-exe", required=True, type=Path)
    parser.add_argument("--theme", choices=["dark", "light"], default="dark")
    args = parser.parse_args()

    root = args.root.resolve()
    repo = args.repo.resolve()
    app_exe = args.app_exe.resolve()
    if args.command == "stage":
        stage(root, repo, app_exe)
    else:
        reset(root, repo, app_exe, args.theme)
    return 0


if __name__ == "__main__":
    sys.exit(main())
