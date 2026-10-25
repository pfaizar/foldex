#!/usr/bin/env python3
"""Reproduce the app audit using temporary, automatically restored source additions.

The behavioral probes assert the observed problems at the audited commit. A probe
may fail after its corresponding problem is fixed; these are investigation tools,
not regression tests to add to CI. Directory mode creates 150,000 empty files in
an owned temporary directory and removes them on successful completion.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import json
from pathlib import Path
import statistics
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[2]
PROBES = Path(__file__).resolve().parent / "probes"
BEHAVIOR_TARGETS = {
    "crates/foldex/src/search.rs": "search_probes.rs",
    "crates/foldex/src/persistence.rs": "persistence_probes.rs",
    "crates/foldex/src/platform.rs": "platform_probe.rs",
    "crates/foldex/src/pane/listing.rs": "accessibility_probe.rs",
}


@contextmanager
def temporary_sources(targets: dict[str, str], *, append: bool):
    changes = []
    for relative, probe_name in targets.items():
        target = ROOT / relative
        original = target.read_bytes() if target.exists() else None
        if append:
            if original is None or b"mod app_audit_probes" in original:
                raise RuntimeError(f"Cannot append audit probes to {target}")
        elif original is not None:
            raise RuntimeError(f"Refusing to replace existing example {target}")
        addition = (PROBES / probe_name).read_bytes()
        expected = (original or b"") + addition
        changes.append((target, original, expected, not target.parent.exists()))

    written = []
    try:
        for target, original, expected, created_parent in changes:
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(expected)
            written.append((target, original, expected, created_parent))
        yield
    finally:
        conflicts = []
        for target, original, expected, created_parent in reversed(written):
            if not target.exists() or target.read_bytes() != expected:
                conflicts.append(str(target))
                continue
            if original is None:
                target.unlink()
                if created_parent:
                    target.parent.rmdir()
            else:
                target.write_bytes(original)
        if conflicts:
            raise RuntimeError(
                "Sources changed during the audit; preserved those edits. "
                "Remove the appended audit module manually from: " + ", ".join(conflicts)
            )


def cargo(subcommand: str, args: list[str], offline: bool) -> str:
    command = ["cargo", subcommand, "--locked"]
    if offline:
        command.append("--offline")
    result = subprocess.run(
        command + args,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    if result.returncode:
        sys.stderr.write(result.stdout)
        raise RuntimeError(f"Cargo exited with status {result.returncode}")
    return result.stdout


def behavioral(offline: bool) -> list[dict]:
    args = ["-p", "foldex"]
    if sys.platform != "win32":
        args += ["--no-default-features", "--features", "renderer-glow"]
    args += ["app_audit", "--", "--nocapture", "--test-threads=1"]
    with temporary_sources(BEHAVIOR_TARGETS, append=True):
        output = cargo("test", args, offline)
    rows = [
        json.loads(line.split("AUDIT ", 1)[1])
        for line in output.splitlines()
        if "AUDIT " in line
    ]
    if len(rows) != 7:
        raise RuntimeError(f"Expected seven behavioral results, received {len(rows)}")
    return rows


def example(package: str, name: str, source: str, offline: bool) -> list[dict]:
    target = f"crates/{package}/examples/{name}.rs"
    args = ["-p", package, "--example", name]
    if package == "foldex-index":
        args += ["--features", "catalog-experiment"]
    with temporary_sources({target: source}, append=False):
        output = cargo("run", args, offline)
    return [json.loads(line) for line in output.splitlines() if line.startswith('{"probe"')]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=["probes", "directory", "catalog", "all"], default="probes")
    parser.add_argument("--offline", action="store_true", help="Use already cached Cargo dependencies")
    parser.add_argument("--catalog-runs", type=int, default=5)
    parser.add_argument("--output", type=Path, help="Write results to this file instead of stdout")
    options = parser.parse_args()
    if not 1 <= options.catalog_runs <= 20:
        parser.error("--catalog-runs must be between 1 and 20")
    results = {}
    if options.mode in ("probes", "all"):
        results["behavioral_probes"] = behavioral(options.offline)
    if options.mode in ("directory", "all"):
        results["large_directory"] = example(
            "foldex-core", "app_audit_directory", "directory_probe.rs", options.offline
        )[0]
    if options.mode in ("catalog", "all"):
        measurements = []
        for run in range(options.catalog_runs):
            rows = example("foldex-index", "app_audit_catalog", "catalog_probe.rs", options.offline)
            measurements.extend(dict(row, run=run + 1) for row in rows)
        summary = []
        for query, page in dict.fromkeys((row["query"], row["page"]) for row in measurements):
            values = [
                row["elapsed_ms"] for row in measurements
                if row["query"] == query and row["page"] == page
            ]
            summary.append({
                "query": query, "page": page, "runs": len(values),
                "median_ms": round(statistics.median(values), 3),
                "min_ms": min(values), "max_ms": max(values),
            })
        results["catalog_paging"] = {"measurements": measurements, "summary": summary}
    rendered = json.dumps(results, indent=2) + "\n"
    if options.output:
        options.output.write_text(rendered, encoding="utf-8")
    else:
        sys.stdout.write(rendered)


if __name__ == "__main__":
    main()
