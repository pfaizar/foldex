"""Keep path filtering from silently skipping native changes."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ci_changes


class ChangeSelection(unittest.TestCase):
    def test_docs_and_website_do_not_build_windows(self):
        self.assertEqual(ci_changes.classify(["README.md", "docs/releases/v0.1.2.md"]),
                         dict(native=False, installer=False, website=False))
        self.assertEqual(ci_changes.classify(["web/src/pages/index.astro", "docs/RELEASING.md"]),
                         dict(native=False, installer=False, website=True))

    def test_native_changes_keep_the_workspace_tests(self):
        for path in ["crates/foldex/src/main.rs", "vendor/eframe/src/lib.rs"]:
            with self.subTest(path=path):
                self.assertTrue(ci_changes.classify([path])["native"])

    def test_installer_changes_keep_the_lifecycle_checks(self):
        for path in ["packaging/windows/foldex.iss", "scripts/package_portable.py",
                     "scripts/test_installer.ps1", "Cargo.toml", "Cargo.lock", "theme.toml", "LICENSE"]:
            with self.subTest(path=path):
                self.assertTrue(ci_changes.classify([path])["installer"])

    def test_workflow_and_selector_changes_run_every_check(self):
        for path in [".github/workflows/ci.yml", "scripts/ci_changes.py", "scripts/tests/test_ci_gate.py"]:
            with self.subTest(path=path):
                self.assertTrue(all(ci_changes.classify([path]).values()))

    def test_unknown_inputs_get_windows_coverage(self):
        jobs = ci_changes.classify(["new-build-input.txt"])
        self.assertTrue(jobs["native"] and jobs["installer"])

    def test_main_uses_the_latest_successful_ancestor_instead_of_the_previous_push(self):
        ancestor = "a" * 40
        unrelated = "b" * 40
        head = "c" * 40
        runs = [dict(id=index, head_sha=sha, head_branch="main", event="push",
                     path=".github/workflows/ci.yml", conclusion="success")
                for index, sha in [(1, ancestor), (2, unrelated), (3, head)]]
        with mock.patch.dict("os.environ", {"GITHUB_REPOSITORY": "example/repo"}), \
                mock.patch.object(ci_changes.subprocess, "check_output", return_value=json.dumps({"workflow_runs": runs})), \
                mock.patch.object(ci_changes.subprocess, "run", side_effect=[
                    subprocess.CompletedProcess([], 1), subprocess.CompletedProcess([], 0),
                ]) as ancestry:
            self.assertEqual(ci_changes.successful_base(head), ancestor)
            self.assertEqual(ancestry.call_args_list[-1].args[0][-2:], [ancestor, head])

    def test_unavailable_ci_history_requires_full_checks(self):
        with mock.patch.dict("os.environ", {"GITHUB_REPOSITORY": "example/repo"}), \
                mock.patch.object(ci_changes.subprocess, "check_output", side_effect=OSError("API unavailable")):
            self.assertIsNone(ci_changes.successful_base("c" * 40))

    def test_full_push_range_includes_deleted_paths_and_renames(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)

            def git(*args):
                return subprocess.check_output(["git", *args], cwd=root, text=True).strip()

            git("init", "--quiet")
            git("config", "user.name", "CI test")
            git("config", "user.email", "ci@example.invalid")
            native = root / "crates/foldex/src/main.rs"
            native.parent.mkdir(parents=True)
            native.write_text("fn main() {}\n")
            git("add", ".")
            git("commit", "--quiet", "-m", "base")
            before = git("rev-parse", "HEAD")
            (root / "web").mkdir()
            git("mv", "crates/foldex/src/main.rs", "web/example.txt")
            git("commit", "--quiet", "-m", "move native source")
            (root / "web/example.txt").write_text("changed again\n")
            git("commit", "--quiet", "-am", "later website-only change")
            after = git("rev-parse", "HEAD")

            for event_name, event in [
                ("push", {"before": before, "after": after}),
                ("pull_request", {"pull_request": {"base": {"sha": before}, "head": {"sha": after}}}),
                ("push", {"before": "0" * 40, "after": after}),
                ("push", {"before": "f" * 40, "after": after}),
            ]:
                with self.subTest(event=event):
                    paths = ci_changes.changed_paths(event, event_name, cwd=root)
                    if paths is None:
                        self.assertIn(event["before"], ["0" * 40, "f" * 40])
                    else:
                        selected = ci_changes.classify(paths)
                        self.assertTrue(selected["native"])
                        self.assertTrue(selected["website"])


if __name__ == "__main__":
    unittest.main()
