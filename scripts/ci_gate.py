"""Reuse successful main CI for the exact release source, waiting when necessary."""

import argparse
import json
import os
import re
import subprocess
import time


def matching_run(runs, sha):
    candidates = [run for run in runs if (
        run["head_sha"] == sha
        and run["event"] == "push"
        and run["head_branch"] == "main"
        and run["path"] == ".github/workflows/ci.yml"
    )]
    return max(candidates, key=lambda run: run["id"], default=None)


def wait_for_ci(sha, fetch_runs, *, allow_missing=False, timeout=2700, missing_timeout=90,
                clock=time.monotonic, sleep=time.sleep):
    started = clock()
    while True:
        run = matching_run(fetch_runs(), sha)
        elapsed = clock() - started
        if run is not None:
            if run["status"] == "completed":
                if run["conclusion"] != "success":
                    raise RuntimeError(f"CI run {run['id']} ended with {run['conclusion']}; rerun CI before releasing.")
                print(f"Reusing successful CI run {run['id']} for {sha}.")
                return True
        elif elapsed >= missing_timeout:
            if allow_missing:
                print("No main CI exists for this tag; the release will run the validation itself.")
                return False
            raise RuntimeError("No main CI run was found for this release commit.")
        if elapsed >= timeout:
            raise TimeoutError("Timed out waiting for CI; the release was not built or published.")
        print(f"Waiting for main CI on {sha} ({int(elapsed)}s elapsed).", flush=True)
        sleep(15)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--allow-missing", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.sha):
        parser.error("The release source must be an exact commit SHA.")
    repository = os.environ["GITHUB_REPOSITORY"]

    def fetch_runs():
        response = subprocess.check_output([
            "gh", "api", "--method", "GET", f"repos/{repository}/actions/workflows/ci.yml/runs",
            "-f", f"head_sha={args.sha}", "-f", "event=push", "-f", "per_page=100",
        ], text=True)
        return json.loads(response)["workflow_runs"]

    passed = wait_for_ci(args.sha, fetch_runs, allow_missing=args.allow_missing)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"ci_passed={str(passed).lower()}\n")


if __name__ == "__main__":
    main()
