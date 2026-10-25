"""Select CI jobs from the complete push or pull-request diff."""

import json
import os
from pathlib import Path
import re
import subprocess


def classify(paths):
    jobs = dict(native=False, installer=False, website=False)
    for path in paths:
        if path.startswith(".github/") or path.startswith("scripts/ci_") or path.startswith("scripts/tests/test_ci_"):
            return dict.fromkeys(jobs, True)
        if path.startswith("web/"):
            jobs["website"] = True
        elif path.startswith("docs/") or path in {"README.md", ".gitignore", ".gitattributes"}:
            continue
        elif path.startswith(("packaging/", "scripts/")):
            jobs["installer"] = True
        elif path.startswith(("crates/", "vendor/")) and not path.endswith("/build.rs"):
            jobs["native"] = True
        else:
            # Build inputs and unfamiliar files get conservative Windows coverage.
            jobs["native"] = jobs["installer"] = True
    return jobs


def successful_base(head):
    """Include earlier pushes whose checks were cancelled or have not passed yet."""
    try:
        response = subprocess.check_output([
            "gh", "api", "--method", "GET",
            f"repos/{os.environ['GITHUB_REPOSITORY']}/actions/workflows/ci.yml/runs",
            "-f", "branch=main", "-f", "event=push", "-f", "status=success", "-f", "per_page=100",
        ], text=True)
        runs = json.loads(response)["workflow_runs"]
        for run in sorted(runs, key=lambda item: item["id"], reverse=True):
            sha = run["head_sha"]
            if (sha == head or not re.fullmatch(r"[0-9a-f]{40}", sha)
                    or run["head_branch"] != "main" or run["event"] != "push"
                    or run["path"] != ".github/workflows/ci.yml" or run["conclusion"] != "success"):
                continue
            if subprocess.run(["git", "merge-base", "--is-ancestor", sha, head],
                              capture_output=True).returncode == 0:
                return sha
    except (OSError, subprocess.CalledProcessError, KeyError, ValueError, TypeError):
        pass
    return None


def changed_paths(event, event_name, *, cwd=None):
    if event_name == "pull_request":
        pull = event["pull_request"]
        revisions = [f"{pull['base']['sha']}...{pull['head']['sha']}"]
    elif event_name == "push" and event.get("before", "").strip("0"):
        revisions = [event["before"], event["after"]]
    else:
        return None
    try:
        # Treat renames as a deletion plus an addition, including both old and new paths.
        result = subprocess.check_output([
            "git", "diff", "--name-only", "--no-renames", "-z", *revisions, "--",
        ], cwd=cwd, stderr=subprocess.PIPE)
    except subprocess.CalledProcessError:
        return None
    return result.decode("utf-8", errors="surrogateescape").rstrip("\0").split("\0") if result else []


def main():
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
    event_name = os.environ["GITHUB_EVENT_NAME"]
    if event_name == "push":
        event["before"] = successful_base(event["after"]) or "0" * 40
    paths = changed_paths(event, event_name)
    jobs = dict(native=True, installer=True, website=True) if paths is None else classify(paths)
    print(json.dumps({"jobs": jobs, "full_check_fallback": paths is None}))
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for name, enabled in jobs.items():
            output.write(f"{name}={str(enabled).lower()}\n")


if __name__ == "__main__":
    main()
