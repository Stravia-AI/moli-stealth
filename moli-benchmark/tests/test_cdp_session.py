from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from moli_benchmark.cdp_session import (
    _benchmark_marker_expression,
    _benchmark_marker_wait_expression,
    _cdp_trace_events,
    _trace_summary,
    _write_trace_artifact,
    run_cdp_session_suite,
)


class CdpSessionTraceTests(unittest.TestCase):
    def test_marker_expression_requires_current_url_case_and_ok_marker(self) -> None:
        expression = _benchmark_marker_expression(url="http://fixture/static-html", case="static-html")

        self.assertIn("document.readyState === 'complete'", expression)
        self.assertIn('location.href === "http://fixture/static-html"', expression)
        self.assertIn('node.getAttribute(\'data-benchmark-case\') === "static-html"', expression)
        self.assertIn('document.querySelector(\'[data-benchmark-status="ok"]\') !== null', expression)

    def test_marker_wait_expression_wraps_strict_marker_check(self) -> None:
        expression = _benchmark_marker_wait_expression(url="http://fixture/dom-heavy", case="dom-heavy", timeout_seconds=1.25)

        self.assertIn("new Promise", expression)
        self.assertIn("Date.now() + 1250", expression)
        self.assertIn('location.href === "http://fixture/dom-heavy"', expression)
        self.assertIn('node.getAttribute(\'data-benchmark-case\') === "dom-heavy"', expression)

    def test_cdp_trace_events_compact_console_exception_and_network_events(self) -> None:
        events = _cdp_trace_events(
            [
                {
                    "method": "Runtime.consoleAPICalled",
                    "params": {"type": "error", "args": [{"value": "boom"}, {"description": "detail"}]},
                },
                {
                    "method": "Runtime.exceptionThrown",
                    "params": {"exceptionDetails": {"text": "Uncaught Error", "url": "http://fixture/", "lineNumber": 7}},
                },
                {
                    "method": "Network.responseReceived",
                    "params": {"requestId": "1", "type": "Document", "response": {"url": "http://fixture/", "status": 200}},
                },
                {
                    "method": "Network.loadingFailed",
                    "params": {"requestId": "2", "type": "Script", "errorText": "net::ERR_FAILED", "canceled": False},
                },
                {"method": "Log.entryAdded", "params": {"entry": {"level": "warning", "text": "slow"}}},
                {"method": "Log.entryAdded", "params": {"entry": {"level": "error", "text": "broken"}}},
                {"id": 99, "result": {}},
            ]
        )

        self.assertEqual(
            [event["method"] for event in events],
            [
                "Runtime.consoleAPICalled",
                "Runtime.exceptionThrown",
                "Network.responseReceived",
                "Network.loadingFailed",
                "Log.entryAdded",
                "Log.entryAdded",
            ],
        )
        self.assertEqual(events[0]["text"], "boom detail")
        self.assertEqual(events[2]["status"], 200)
        self.assertEqual(
            _trace_summary(events),
            {"console_errors": 2, "js_exceptions": 1, "network_failures": 1},
        )

    def test_write_trace_artifact_uses_suite_relative_path(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            suite_dir = Path(temp_dir) / "cdp-session"
            row = {"target": "moli", "run": 1, "case": "static-html", "ok": False}
            relative = _write_trace_artifact(
                suite_dir=suite_dir,
                row=row,
                events=[{"method": "Network.loadingFailed", "error_text": "failed"}],
            )

            self.assertFalse(Path(relative).is_absolute())
            self.assertTrue((suite_dir / relative).exists())

    def test_cdp_session_rejects_unmeasured_gate_target(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            with self.assertRaisesRegex(RuntimeError, "must be included"):
                run_cdp_session_suite(
                    output_dir=Path(temp_dir),
                    target_matrix={},
                    targets=("lightpanda",),
                    cases=("static-html",),
                    runs=1,
                    timeout_seconds=1.0,
                    gate_target="moli",
                )

    def test_summary_total_trace_events_counts_trace_artifacts_only(self) -> None:
        class FakeServer:
            def __enter__(self) -> "FakeServer":
                return self

            def __exit__(self, exc_type: object, exc: object, tb: object) -> None:
                return None

        async def fake_run_target_session(**_kwargs: object) -> tuple[list[dict[str, object]], dict[str, object]]:
            return (
                [
                    {
                        "target": "moli-cdp",
                        "engine": "moli",
                        "driver": "cdp",
                        "label": "moli / cdp",
                        "binary_key": "moli",
                        "case": "static-html",
                        "run": 1,
                        "ok": True,
                        "elapsed_ms": 1.0,
                        "messages": 12,
                        "trace_events": 12,
                        "console_errors": 0,
                        "js_exceptions": 0,
                        "network_failures": 0,
                        "error": None,
                    },
                    {
                        "target": "moli-cdp",
                        "engine": "moli",
                        "driver": "cdp",
                        "label": "moli / cdp",
                        "binary_key": "moli",
                        "case": "js-xhr-fetch",
                        "run": 1,
                        "ok": False,
                        "elapsed_ms": 2.0,
                        "messages": 30,
                        "trace_events": 30,
                        "console_errors": 0,
                        "js_exceptions": 0,
                        "network_failures": 1,
                        "trace_artifact": "traces/moli-cdp-run-1-js-xhr-fetch.json",
                        "error": "marker did not become true",
                    },
                ],
                {},
            )

        with tempfile.TemporaryDirectory() as temp_dir:
            with patch("moli_benchmark.cdp_session.SyntheticServer", FakeServer):
                with patch("moli_benchmark.cdp_session._run_target_session", fake_run_target_session):
                    summary = run_cdp_session_suite(
                        output_dir=Path(temp_dir),
                        target_matrix={"moli": {"available": True, "path": "/bin/true"}},
                        targets=("moli-cdp",),
                        cases=("static-html", "js-xhr-fetch"),
                        runs=1,
                        timeout_seconds=1.0,
                        gate_target="moli-cdp",
                    )

        self.assertEqual(summary["total_trace_events"], 1)


if __name__ == "__main__":
    unittest.main()
