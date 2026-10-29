"""Render Foldex dialogs offscreen and build a before/after catalog page.

The renders come from the ignored `ui_catalog` test in
crates/foldex/src/app/ui_catalog.rs, which drives the real app UI through
wgpu without a window, so no screen capture or input automation is needed.

usage:
    python scripts/ui_catalog.py                 # after set + page
    python scripts/ui_catalog.py --set before    # render the current tree as "before"
    python scripts/ui_catalog.py --page-only     # rebuild index.html from existing PNGs
    python scripts/ui_catalog.py --only batch,search

Output goes to target/ui-catalog/: one folder per set and index.html, which
references the PNGs relatively.
Pass --embed to inline the images so the page is a single portable file.
"""

from __future__ import annotations

import argparse
import base64
import html
import json
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "target" / "ui-catalog"


def render(out: Path, only: str = "") -> None:
    env = dict(os.environ)
    env["FOLDEX_UI_CATALOG_OUT"] = str(out)
    if only:
        env["FOLDEX_UI_CATALOG_ONLY"] = only
    else:
        env.pop("FOLDEX_UI_CATALOG_ONLY", None)
    print(f"rendering -> {out.relative_to(ROOT)}", flush=True)
    subprocess.run(
        [
            "cargo", "test", "-p", "foldex", "ui_catalog", "--",
            "--ignored", "--nocapture", "--test-threads=1",
        ],
        cwd=ROOT,
        env=env,
        check=True,
    )


def slug(name: str) -> str:
    return name.lower().replace(" ", "-")


def load(folder: Path) -> dict[str, dict]:
    manifest = folder / "manifest.json"
    if not manifest.exists():
        return {}
    return {shot["name"]: shot for shot in json.loads(manifest.read_text("utf-8"))["shots"]}


def image(path: Path, embed: bool) -> str:
    if not path.exists():
        return ""
    if embed:
        # Renders are 2x; a 1x WebP keeps the single-file page small.
        from io import BytesIO

        from PIL import Image

        with Image.open(path) as source:
            small = source.convert("RGB").resize((source.width // 2, source.height // 2), Image.LANCZOS)
        buffer = BytesIO()
        small.save(buffer, "WEBP", quality=88)
        data = base64.b64encode(buffer.getvalue()).decode()
        return f"data:image/webp;base64,{data}"
    return path.relative_to(OUT).as_posix()


def figure(src: str, label: str) -> str:
    if not src:
        return f'<figure class="missing"><span>Not in this set</span><figcaption>{label}</figcaption></figure>'
    return (
        f'<figure><a href="{src}" target="_blank"><img loading="lazy" src="{src}" alt="{label}"></a>'
        f"<figcaption>{label}</figcaption></figure>"
    )


def page(embed: bool) -> Path:
    before, after = load(OUT / "before"), load(OUT / "after")
    order = list(after) + [name for name in before if name not in after]
    groups: dict[str, list[str]] = {}
    for name in order:
        shot = after.get(name) or before[name]
        groups.setdefault(shot["group"], []).append(name)

    sections = []
    for group, names in groups.items():
        rows = []
        for name in names:
            shot = after.get(name) or before[name]
            old = image(OUT / "before" / f"{name}.png", embed) if name in before else ""
            new = image(OUT / "after" / f"{name}.png", embed) if name in after else ""
            rows.append(
                f'<div class="pair"><h3>{html.escape(shot["caption"])}</h3>'
                f'<div class="cols">{figure(old, "Before")}{figure(new, "After")}</div></div>'
            )
        sections.append(f'<section id="{slug(group)}"><h2>{html.escape(group)}</h2>{"".join(rows)}</section>')

    nav = "".join(f'<a href="#{slug(group)}">{html.escape(group)}</a>' for group in groups)
    document = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Foldex UI Catalog</title>
<style>
:root {{ --bg:#f6f6f5; --surface:#fff; --text:#1c1c1c; --muted:#6b6b6b; --line:#e2e2e0; }}
@media (prefers-color-scheme: dark) {{ :root:not([data-theme="light"]) {{ --bg:#161616; --surface:#1f1f1f; --text:#ececec; --muted:#9a9a9a; --line:#333; }} }}
:root[data-theme="dark"] {{ --bg:#161616; --surface:#1f1f1f; --text:#ececec; --muted:#9a9a9a; --line:#333; }}
* {{ box-sizing:border-box; }}
body {{ margin:0; background:var(--bg); color:var(--text); font:15px/1.5 "Segoe UI",system-ui,sans-serif; }}
header {{ padding:32px 16px 8px; max-width:1400px; margin:auto; }}
h1 {{ margin:0 0 4px; font-size:26px; font-weight:600; }}
.lede {{ color:var(--muted); margin:0 0 12px; }}
nav {{ position:sticky; top:0; z-index:1; background:var(--bg); border-bottom:1px solid var(--line);
       display:flex; gap:4px 16px; flex-wrap:wrap; padding:10px 16px; justify-content:center; }}
nav a {{ color:var(--muted); text-decoration:none; font-size:13px; }}
nav a:hover {{ color:var(--text); }}
main {{ max-width:1400px; margin:auto; padding:0 16px 64px; }}
section {{ padding-top:28px; }}
h2 {{ font-size:20px; font-weight:600; margin:0 0 12px; }}
h3 {{ font-size:13px; font-weight:600; color:var(--muted); margin:18px 0 8px; text-transform:none; }}
.cols {{ display:grid; grid-template-columns:1fr 1fr; gap:12px; }}
figure {{ margin:0; background:var(--surface); border:1px solid var(--line); border-radius:10px; padding:8px; }}
figure img {{ width:100%; height:auto; display:block; border-radius:6px; }}
figcaption {{ font-size:12px; color:var(--muted); padding:6px 2px 0; }}
figure.missing {{ display:flex; flex-direction:column; justify-content:center; align-items:center; min-height:120px; color:var(--muted); }}
code {{ font-size:13px; }}
@media (max-width:760px) {{ .cols {{ grid-template-columns:1fr; }} }}
</style></head><body>
<header><h1>Foldex UI Catalog</h1>
<p class="lede">Dialogs before and after the redesign, rendered offscreen by <code>scripts/ui_catalog.py</code>.</p></header>
<nav>{nav}</nav>
<main>{"".join(sections)}</main>
</body></html>
"""
    target = OUT / ("index-embedded.html" if embed else "index.html")
    target.write_text(document, "utf-8")
    return target


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--set", default="after", help="folder to render the current tree into")
    parser.add_argument("--only", default="", help="comma-separated scenario name prefixes")
    parser.add_argument("--page-only", action="store_true", help="skip rendering")
    parser.add_argument("--embed", action="store_true", help="inline images into the page")
    args = parser.parse_args()
    if not args.page_only:
        render(OUT / args.set, only=args.only)
    print(page(args.embed))
    return 0


if __name__ == "__main__":
    sys.exit(main())
