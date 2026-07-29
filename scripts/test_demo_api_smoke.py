#!/usr/bin/env python3
"""Regression tests for the demo API smoke runner and manual workflow."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import re
import unittest
from pathlib import Path
from types import ModuleType
from unittest import mock


def load_script(filename: str, module_name: str) -> ModuleType:
    path = Path(__file__).with_name(filename)
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


SMOKE = load_script("demo-api-smoke.py", "demo_api_smoke")


class WaitConfigurationTests(unittest.TestCase):
    def test_absent_wait_is_zero(self) -> None:
        self.assertEqual(SMOKE.parse_wait_seconds(None), 0)

    def test_positive_wait_is_accepted(self) -> None:
        self.assertEqual(SMOKE.parse_wait_seconds("1200"), 1200)

    def test_invalid_wait_is_rejected(self) -> None:
        with self.assertRaisesRegex(
            SMOKE.PreflightError,
            "DEMO_API_WAIT_SECONDS must be a non-negative integer",
        ):
            SMOKE.parse_wait_seconds("twenty")

    def test_negative_wait_is_rejected(self) -> None:
        with self.assertRaisesRegex(
            SMOKE.PreflightError,
            "DEMO_API_WAIT_SECONDS must be a non-negative integer",
        ):
            SMOKE.parse_wait_seconds("-1")


class DemoStandDefaultsTests(unittest.TestCase):
    def test_default_api_url_uses_demo_stand_web_port(self) -> None:
        stand_script = (
            Path(__file__).resolve().parent / "demo-stand.sh"
        ).read_text(encoding="utf-8")
        match = re.search(
            r"(?m)^WEB_PORT=\$\{DEMO_WEB_PORT:-(\d+)\}$",
            stand_script,
        )

        self.assertIsNotNone(match)
        self.assertEqual(
            SMOKE.DEFAULT_BASE_URL,
            f"http://127.0.0.1:{match.group(1)}",
        )


class RetainedSegmentWaitTests(unittest.TestCase):
    @mock.patch("time.sleep")
    @mock.patch("time.monotonic", return_value=0.0)
    @mock.patch.object(
        SMOKE,
        "request_json",
        return_value={"segments": [{"min_ts": 1, "max_ts": 2}]},
    )
    def test_existing_segment_returns_without_sleep(
        self,
        request_json: mock.Mock,
        _monotonic: mock.Mock,
        sleep: mock.Mock,
    ) -> None:
        SMOKE.wait_for_retained_segments("http://demo", "Basic token", 1200)

        sleep.assert_not_called()
        request_json.assert_called_once_with(
            "http://demo",
            "/v1/segments",
            query={"from": -(2**63), "to": 2**63 - 1},
            authorization="Basic token",
        )

    @mock.patch("time.sleep")
    @mock.patch("time.monotonic", side_effect=[0.0, 0.0])
    @mock.patch.object(
        SMOKE,
        "request_json",
        side_effect=[
            {"segments": []},
            {"segments": [{"min_ts": 1, "max_ts": 2}]},
        ],
    )
    def test_empty_result_is_polled_until_a_segment_appears(
        self,
        request_json: mock.Mock,
        _monotonic: mock.Mock,
        sleep: mock.Mock,
    ) -> None:
        SMOKE.wait_for_retained_segments("http://demo", None, 1200)

        self.assertEqual(request_json.call_count, 2)
        sleep.assert_called_once_with(10)

    @mock.patch("time.sleep")
    @mock.patch("time.monotonic", side_effect=[0.0, 0.0, 1200.0])
    @mock.patch.object(SMOKE, "request_json", return_value={"segments": []})
    def test_empty_result_fails_at_the_deadline(
        self,
        request_json: mock.Mock,
        _monotonic: mock.Mock,
        sleep: mock.Mock,
    ) -> None:
        with self.assertRaisesRegex(
            SMOKE.PreflightError,
            "no retained segments after waiting 1200 seconds",
        ):
            SMOKE.wait_for_retained_segments("http://demo", None, 1200)

        self.assertEqual(request_json.call_count, 2)
        sleep.assert_called_once_with(10)


class SchemathesisRetryTests(unittest.TestCase):
    @staticmethod
    def result(returncode: int, output: str) -> object:
        return SMOKE.subprocess.CompletedProcess(
            args=["schemathesis"],
            returncode=returncode,
            stdout=output,
            stderr="",
        )

    @mock.patch.object(SMOKE.time, "sleep")
    @mock.patch.object(SMOKE.subprocess, "run")
    def test_success_is_not_retried(
        self,
        run: mock.Mock,
        sleep: mock.Mock,
    ) -> None:
        run.return_value = self.result(0, "all passed\n")

        with contextlib.redirect_stdout(io.StringIO()):
            status = SMOKE.run_schemathesis(["schemathesis"], Path("."), {})

        self.assertEqual(status, 0)
        run.assert_called_once()
        sleep.assert_not_called()

    @mock.patch.object(SMOKE.time, "sleep")
    @mock.patch.object(SMOKE.subprocess, "run")
    def test_analytic_capacity_failure_is_retried(
        self,
        run: mock.Mock,
        sleep: mock.Mock,
    ) -> None:
        run.side_effect = [
            self.result(1, '{"code":"analytic_capacity_unavailable"}\n'),
            self.result(0, "all passed\n"),
        ]
        output = io.StringIO()

        with contextlib.redirect_stdout(output):
            status = SMOKE.run_schemathesis(["schemathesis"], Path("."), {})

        self.assertEqual(status, 0)
        self.assertEqual(run.call_count, 2)
        sleep.assert_called_once_with(1)
        self.assertIn(
            "retrying Schemathesis after analytic capacity rejection",
            output.getvalue(),
        )

    @mock.patch.object(SMOKE.time, "sleep")
    @mock.patch.object(SMOKE.subprocess, "run")
    def test_unrelated_failure_is_not_retried(
        self,
        run: mock.Mock,
        sleep: mock.Mock,
    ) -> None:
        run.return_value = self.result(1, "response schema mismatch\n")

        with contextlib.redirect_stdout(io.StringIO()):
            status = SMOKE.run_schemathesis(["schemathesis"], Path("."), {})

        self.assertEqual(status, 1)
        run.assert_called_once()
        sleep.assert_not_called()

    @mock.patch.object(SMOKE.time, "sleep")
    @mock.patch.object(SMOKE.subprocess, "run")
    def test_analytic_capacity_retries_are_bounded(
        self,
        run: mock.Mock,
        sleep: mock.Mock,
    ) -> None:
        run.return_value = self.result(
            1,
            '{"code":"analytic_capacity_unavailable"}\n',
        )

        with contextlib.redirect_stdout(io.StringIO()):
            status = SMOKE.run_schemathesis(["schemathesis"], Path("."), {})

        self.assertEqual(status, 1)
        self.assertEqual(run.call_count, 3)
        self.assertEqual(sleep.call_args_list, [mock.call(1), mock.call(1)])


class ManualWorkflowContractTests(unittest.TestCase):
    def test_workflow_is_manual_only_and_owns_the_demo_lifecycle(self) -> None:
        workflow = (
            Path(__file__).resolve().parent.parent
            / ".github/workflows/demo-api-smoke.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("workflow_dispatch:", workflow)
        self.assertNotRegex(workflow, r"(?m)^  (push|pull_request|schedule):")
        self.assertIn("timeout-minutes: 60", workflow)
        self.assertIn('DEMO_API_WAIT_SECONDS: "1200"', workflow)
        for command in (
            "make demo-build",
            "make demo-up",
            "make demo-api-smoke",
            "make demo-down",
        ):
            self.assertIn(command, workflow)
        self.assertGreaterEqual(workflow.count("if: always()"), 2)
        self.assertIn("actions/upload-artifact@v4", workflow)


if __name__ == "__main__":
    unittest.main()
