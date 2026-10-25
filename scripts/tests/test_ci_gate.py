"""A release may reuse only successful CI for its exact source commit."""

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import ci_gate


SHA = "a" * 40


def run(**changes):
    result = dict(id=10, head_sha=SHA, event="push", head_branch="main",
                  path=".github/workflows/ci.yml", status="completed", conclusion="success")
    return result | changes


class ReleaseCIGate(unittest.TestCase):
    def wait(self, batches, **options):
        now = [0]
        responses = iter(batches)

        def sleep(seconds):
            now[0] += seconds

        return ci_gate.wait_for_ci(SHA, lambda: next(responses), clock=lambda: now[0],
                                   sleep=sleep, missing_timeout=15, timeout=30, **options)

    def test_reuses_success_for_the_exact_main_commit(self):
        self.assertTrue(self.wait([[run()]]))

    def test_waits_for_running_ci_and_for_registration_races(self):
        self.assertTrue(self.wait([[], [run(status="in_progress", conclusion=None)], [run()]]))

    def test_ignores_other_commits_branches_events_and_workflows(self):
        for changes in [dict(head_sha="b" * 40), dict(head_branch="feature"),
                        dict(event="pull_request"), dict(path=".github/workflows/other.yml")]:
            with self.subTest(changes=changes):
                self.assertIsNone(ci_gate.matching_run([run(**changes)], SHA))

    def test_failed_or_cancelled_latest_ci_cannot_fall_back_to_old_success(self):
        for conclusion in ["failure", "cancelled", "timed_out", "skipped"]:
            with self.subTest(conclusion=conclusion), self.assertRaises(RuntimeError):
                self.wait([[run(id=9), run(id=10, conclusion=conclusion)]], allow_missing=True)

    def test_tags_without_existing_main_ci_request_full_validation(self):
        self.assertFalse(self.wait([[], []], allow_missing=True))

    def test_main_releases_require_ci_instead_of_bypassing_it(self):
        with self.assertRaisesRegex(RuntimeError, "No main CI"):
            self.wait([[], []])

    def test_ci_timeout_blocks_the_release(self):
        running = [run(status="in_progress", conclusion=None)]
        with self.assertRaises(TimeoutError):
            self.wait([running, running, running])

    def test_api_failures_do_not_enable_the_fallback(self):
        def fail():
            raise ConnectionError("API unavailable")

        with self.assertRaises(ConnectionError):
            ci_gate.wait_for_ci(SHA, fail, allow_missing=True)


if __name__ == "__main__":
    unittest.main()
