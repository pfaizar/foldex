"""Encode recorded takes into seamless website loops.

Called by record.ps1 after recording, but usable on its own:

    python scripts/demo/encode.py --work target/demo-recording --web web
    python scripts/demo/encode.py ... --only palette --no-publish

For each take it crops/scales, crossfades the first `fade` seconds into the
tail so the wrap is continuous, writes H.264 MP4 and VP9 WebM, grabs a poster,
and regenerates the keycast timings in web/src/data/loops.ts.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path


def run(command: list[str]) -> None:
    subprocess.run(command, check=True)


def probe(ffprobe: str, path: Path, entries: str) -> str:
    return subprocess.run(
        [
            ffprobe,
            "-v",
            "error",
            "-show_entries",
            entries,
            "-of",
            "csv=p=0",
            str(path),
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def duration(ffprobe: str, path: Path) -> float:
    return float(probe(ffprobe, path, "format=duration"))


def size(ffprobe: str, path: Path) -> tuple[int, int]:
    width, height = probe(ffprobe, path, "stream=width,height").split(",")[:2]
    return int(width), int(height)


def load_keys(path: Path) -> list[dict]:
    if not path.exists():
        return []
    raw = json.loads(path.read_text(encoding="utf-8-sig"))
    if isinstance(raw, dict):
        return [raw]
    return list(raw)


def frame_filter(crop: str | None, width: int, fps: int) -> str:
    parts = []
    if crop:
        parts.append(f"crop={crop}")
    if width:
        parts.append(f"scale={width}:-2:flags=lanczos")
    parts += [f"fps={fps}", "format=yuv420p"]
    return ",".join(parts)


def encode_clip(
    ffmpeg: str,
    ffprobe: str,
    raw: Path,
    keys_path: Path,
    out: Path,
    crop: str | None,
    width: int,
    fade: float,
    fps: int,
) -> dict:
    total_raw = duration(ffprobe, raw)
    if fade and total_raw <= 2 * fade:
        raise SystemExit(
            f"{raw.name}: {total_raw:.2f}s is too short for a {fade:.2f}s crossfade"
        )

    fade = max(0.0, float(fade))
    pre = frame_filter(crop, width, fps)
    lossless = out / f"{raw.stem}.lossless.mkv"

    if fade:
        graph = (
            f"[0]{pre},split[a][b];"
            f"[a]trim=0:{fade},setpts=PTS-STARTPTS,fps={fps},settb=1/{fps}[head];"
            f"[b]trim={fade},setpts=PTS-STARTPTS,fps={fps},settb=1/{fps}[body];"
            f"[body][head]xfade=transition=fade:duration={fade}:"
            f"offset={total_raw - 2 * fade:.3f}[v]"
        )
        run(
            [
                ffmpeg,
                "-v",
                "error",
                "-y",
                "-i",
                str(raw),
                "-filter_complex",
                graph,
                "-map",
                "[v]",
                "-c:v",
                "ffv1",
                str(lossless),
            ]
        )
        total = round(total_raw - fade, 2)
    else:
        run(
            [
                ffmpeg,
                "-v",
                "error",
                "-y",
                "-i",
                str(raw),
                "-vf",
                pre,
                "-c:v",
                "ffv1",
                str(lossless),
            ]
        )
        total = round(total_raw, 2)

    mp4 = out / f"{raw.stem}.mp4"
    webm = out / f"{raw.stem}.webm"
    poster = out / f"{raw.stem}.png"

    run(
        [
            ffmpeg,
            "-v",
            "error",
            "-y",
            "-i",
            str(lossless),
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "slow",
            "-crf",
            "21",
            "-pix_fmt",
            "yuv420p",
            "-profile:v",
            "high",
            "-tune",
            "animation",
            "-movflags",
            "+faststart",
            str(mp4),
        ]
    )
    run(
        [
            ffmpeg,
            "-v",
            "error",
            "-y",
            "-i",
            str(lossless),
            "-an",
            "-c:v",
            "libvpx-vp9",
            "-b:v",
            "0",
            "-crf",
            "33",
            "-row-mt",
            "1",
            "-deadline",
            "good",
            "-cpu-used",
            "2",
            "-pix_fmt",
            "yuv420p",
            str(webm),
        ]
    )
    run(
        [
            ffmpeg,
            "-v",
            "error",
            "-y",
            "-i",
            str(lossless),
            "-frames:v",
            "1",
            str(poster),
        ]
    )

    keys = [
        {"t": round(item["t"] - fade, 2), "k": item["k"]}
        for item in load_keys(keys_path)
        if item["t"] >= fade
    ]

    encoded_width, encoded_height = size(ffprobe, lossless)
    return {
        "duration": total,
        "keys": keys,
        "width": encoded_width,
        "height": encoded_height,
        "bytes": {"mp4": mp4.stat().st_size, "webm": webm.stat().st_size},
    }


def ts_string(value: str) -> str:
    value = (
        value.replace("\\", "\\\\")
        .replace("'", "\\'")
        .replace("\n", "\\n")
        .replace("\r", "\\r")
    )
    return f"'{value}'"


def ts_number(value: float) -> str:
    return repr(round(float(value), 2) + 0.0)


def read_loops(path: Path) -> dict:
    """Read timings from an existing generated loops.ts, as a merge baseline.

    This lets `-Only palette` keep the other clips' timings on a fresh checkout
    where target/demo-recording/encoded.json does not exist yet.
    """
    if not path.exists():
        return {}
    text = path.read_text(encoding="utf-8")
    found = {}
    for match in re.finditer(
        r"(?ms)^  ([a-z][a-z0-9_]*): \{\n(.*?)^  \},$", text
    ):
        name, body = match.group(1), match.group(2)
        duration_match = re.search(r"duration: ([0-9.]+)", body)
        if not duration_match:
            continue
        keys = [
            {"t": float(t), "k": unescape_ts(k)}
            for t, k in re.findall(r"\[([0-9.]+), '((?:\\.|[^'\\])*)'\]", body)
        ]
        found[name] = {"duration": float(duration_match.group(1)), "keys": keys}
    return found


def unescape_ts(value: str) -> str:
    return (
        value.replace("\\n", "\n")
        .replace("\\r", "\r")
        .replace("\\'", "'")
        .replace("\\\\", "\\")
    )


def write_loops(web: Path, manifest: dict, state: dict) -> None:
    clips = [
        clip
        for clip in manifest["clips"]
        if clip.get("publish") and clip["name"] in state
    ]
    if not clips:
        raise SystemExit("no published clips to write; nothing was encoded")

    names = [clip["name"] for clip in clips]
    union = " | ".join(ts_string(name) for name in names)
    lines = [
        "// Key presses captured while recording each loop (seconds from the start",
        "// of the encoded video). Generated by scripts/demo/record.ps1; edit",
        "// scripts/demo/scenes.ps1 and re-record rather than editing this file.",
        f"export type LoopName = {union};",
        "",
        "export const loops: Record<",
        "  LoopName,",
        "  { duration: number; keys: [number, string][] }",
        "> = {",
    ]
    for clip in clips:
        name = clip["name"]
        entry = state[name]
        lines.append(f"  {name}: {{")
        lines.append(f"    duration: {ts_number(entry['duration'])},")
        keys = entry.get("keys", [])
        if keys:
            lines.append("    keys: [")
            for key in keys:
                lines.append(f"      [{ts_number(key['t'])}, {ts_string(key['k'])}],")
            lines.append("    ],")
        else:
            lines.append("    keys: [],")
        lines.append("  },")
    lines.append("};")
    lines.append("")

    target = web / "src" / "data" / "loops.ts"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("\n".join(lines), encoding="utf-8")
    print(f"wrote {target}")


def publish(web: Path, name: str, out: Path) -> None:
    media = web / "public" / "media"
    posters = web / "src" / "assets" / "media"
    media.mkdir(parents=True, exist_ok=True)
    posters.mkdir(parents=True, exist_ok=True)
    for ext in ("mp4", "webm"):
        shutil.copy2(out / f"{name}.{ext}", media / f"{name}.{ext}")
    shutil.copy2(out / f"{name}.png", posters / f"{name}.png")


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--work", required=True, type=Path)
    parser.add_argument("--web", required=True, type=Path)
    parser.add_argument("--ffmpeg", required=True)
    parser.add_argument("--ffprobe", required=True)
    parser.add_argument("--fps", type=int, default=0)
    parser.add_argument("--only", default="", help="comma-separated scene names")
    parser.add_argument(
        "--no-publish",
        action="store_true",
        help="encode into the work folder without touching web/",
    )
    args = parser.parse_args()

    work = args.work.resolve()
    web = args.web.resolve()
    manifest_path = work / "manifest.json"
    if not manifest_path.exists():
        raise SystemExit(f"missing manifest: {manifest_path}")

    manifest = json.loads(manifest_path.read_text(encoding="utf-8-sig"))
    clips = manifest["clips"]
    names = [clip["name"] for clip in clips]
    fps = args.fps or int(manifest.get("fps") or 25)
    only = [name.strip() for name in args.only.split(",") if name.strip()]
    if only:
        missing = [name for name in only if name not in names]
        if missing:
            raise SystemExit(f"unknown scene(s): {', '.join(missing)}")
    if not args.no_publish and not (web / "src").is_dir():
        raise SystemExit(f"web folder not found: {web}")

    state_path = work / "encoded.json"
    state = (
        json.loads(state_path.read_text(encoding="utf-8"))
        if state_path.exists()
        else {}
    )

    # On a fresh checkout there is no encoded.json yet. Keep the timings of
    # clips that are not part of this run so a partial re-record does not erase
    # them from loops.ts.
    if not args.no_publish:
        existing = read_loops(web / "src" / "data" / "loops.ts")
        for clip in clips:
            name = clip["name"]
            if clip.get("publish") and name not in state and name in existing:
                state[name] = existing[name]

    raw_dir = work / "raw"
    out_dir = work / "out"
    out_dir.mkdir(parents=True, exist_ok=True)

    encoded = []
    for clip in clips:
        name = clip["name"]
        if only and name not in only:
            continue
        raw = raw_dir / f"{name}.mkv"
        if not raw.exists():
            print(f"skip {name}: no take at {raw}")
            continue

        print(f"encode {name}")
        result = encode_clip(
            args.ffmpeg,
            args.ffprobe,
            raw,
            raw_dir / f"{name}.keys.json",
            out_dir,
            clip.get("crop"),
            int(clip.get("width") or 0),
            float(clip.get("fade") or 0),
            fps,
        )
        result["publish"] = bool(clip.get("publish"))
        state[name] = result
        encoded.append((name, result))

        if result["publish"] and not args.no_publish:
            publish(web, name, out_dir)

        print(
            f"  {result['duration']:.2f}s, {result['width']}x{result['height']}, "
            f"{result['bytes']['mp4'] // 1024} KiB mp4, "
            f"{result['bytes']['webm'] // 1024} KiB webm"
        )

    if not encoded:
        raise SystemExit("nothing encoded")

    if args.no_publish:
        print(f"preview written to {out_dir}; web/ and encoded.json left untouched")
        return 0

    # Drop clips whose scenes were removed.
    state = {name: state[name] for name in names if name in state}
    state_path.write_text(json.dumps(state, indent=2), encoding="utf-8")
    write_loops(web, manifest, state)

    return 0


if __name__ == "__main__":
    sys.exit(main())
