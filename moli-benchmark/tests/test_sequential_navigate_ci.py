from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from moli_benchmark.sequential_navigate_ci import (
    COMMENT_MARKER,
    build_comparison,
    comparison_health_issues,
    read_sequential_navigation_report,
    render_comparison_comment,
    render_infrastructure_comment,
)


MIB = 1024 * 1024
BASE_SHA = "a" * 40
HEAD_SHA = "b" * 40


def report_payload(*, aborted_after_index: int | None = None) -> dict[str, object]:
    quarters = []
    for quarter in range(1, 5):
        start = (quarter - 1) * 50 + 1
        end = quarter * 50
        quarters.append(
            {
                "quarter": quarter,
                "start_index": start,
                "end_index": end,
                "sample_count": 50,
                "rss_bytes": {
                    "observed_samples": 50,
                    "average": (140 + quarter * 5) * MIB,
                    "final": (142 + quarter * 5) * MIB,
                    "peak": (145 + quarter * 5) * MIB,
                },
                "pss_bytes": {
                    "observed_samples": 50,
                    "average": (130 + quarter * 5) * MIB,
                    "final": (132 + quarter * 5) * MIB,
                    "peak": (135 + quarter * 5) * MIB,
                },
                "fd_count": {
                    "observed_samples": 50,
                    "average": 25,
                    "final": 25,
                    "peak": 26,
                },
            }
        )
    return {
        "schema_version": 5,
        "results": [
            {
                "target": "moli-cdp",
                "started_at": "2026-08-12T00:00:00+00:00",
                "finished_at": "2026-08-12T00:05:00+00:00",
                "summary": {
                    "planned": 200,
                    "attempted": 200,
                    "observable_passes": 198,
                    "failures": 2,
                    "recovery_attempts": 2,
                    "recovery_passes": 2,
                    "recovery_failures": 0,
                    "order_violations": 0,
                    "network_order_violations": 0,
                    "aborted_after_index": aborted_after_index,
                },
                "navigation_resources": {
                    "summary": {
                        "sample_count": 200,
                        "sample_errors": 0,
                        "initial_sample_present": True,
                        "rss_bytes": {
                            "observed_samples": 200,
                            "first_window_average": 140 * MIB,
                            "last_window_average": 160 * MIB,
                            "first_to_last_window_delta": 20 * MIB,
                            "warm_slope_per_100_navigations": 8 * MIB,
                        },
                        "pss_bytes": {
                            "observed_samples": 200,
                            "first_window_average": 130 * MIB,
                            "last_window_average": 150 * MIB,
                            "first_to_last_window_delta": 20 * MIB,
                            "warm_slope_per_100_navigations": 7 * MIB,
                        },
                        "fd_count": {"observed_samples": 200},
                        "periodic": {
                            "peak_rss_bytes": 170 * MIB,
                            "peak_pss_bytes": 160 * MIB,
                            "peak_fd_count": 26,
                            "peak_thread_count": 12,
                            "observer_error": None,
                        },
                        "quarters": quarters,
                    }
                },
                "process": {"returncode": 143},
            }
        ],
    }


class SequentialNavigateCiTests(unittest.TestCase):
    def write_report(self, directory: Path, payload: dict[str, object]) -> Path:
        path = directory / "report.json"
        path.write_text(json.dumps(payload), encoding="utf-8")
        return path

    def test_comparison_comment_reports_200_navigation_memory_shape(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            report = self.write_report(Path(temporary), report_payload())
            base = read_sequential_navigation_report(report, 1)
            head = read_sequential_navigation_report(report, 1)

        comparison = build_comparison(
            base_sha=BASE_SHA,
            head_sha=HEAD_SHA,
            execution_order="head-first",
            expected_navigations=200,
            base_run=base,
            head_run=head,
        )
        comment = render_comparison_comment(
            comparison,
            run_url="https://github.com/example/moli/actions/runs/123",
            conclusion="success",
        )

        self.assertEqual(comparison_health_issues(comparison), [])
        self.assertIn(COMMENT_MARKER, comment)
        self.assertIn("200 `Page.navigate` commands", comment)
        self.assertIn("Periodic peak RSS", comment)
        self.assertIn("RSS · warm slope / 100 nav", comment)
        self.assertIn("151–200", comment)
        self.assertIn("170.0 MiB", comment)
        self.assertIn("workflow run and full", comment)

    def test_health_requires_complete_session_and_resource_samples(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            report = self.write_report(
                Path(temporary),
                report_payload(aborted_after_index=117),
            )
            run = read_sequential_navigation_report(report, 1)
        run["metrics"]["attempted"] = 117
        run["metrics"]["resource_sample_count"] = 117
        comparison = build_comparison(
            base_sha=BASE_SHA,
            head_sha=HEAD_SHA,
            execution_order="base-first",
            expected_navigations=200,
            base_run=run,
            head_run=run,
        )

        issues = comparison_health_issues(comparison)

        self.assertIn("HEAD attempted is not 200", issues)
        self.assertIn("HEAD resource_sample_count is not 200", issues)
        self.assertIn("HEAD session aborted before all navigations", issues)

    def test_reader_rejects_old_report_schema(self) -> None:
        payload = report_payload()
        payload["schema_version"] = 4
        with tempfile.TemporaryDirectory() as temporary:
            report = self.write_report(Path(temporary), payload)
            run = read_sequential_navigation_report(report, 2)

        self.assertFalse(run["available"])
        self.assertEqual(run["exit_code"], 2)

    def test_infrastructure_comment_does_not_render_untrusted_links(self) -> None:
        comment = render_infrastructure_comment(
            run_url="javascript:alert(1)",
            conclusion="invented",
        )

        self.assertIn(COMMENT_MARKER, comment)
        self.assertIn("workflow: `unknown`", comment.lower())
        self.assertNotIn("javascript:", comment)

    def test_trusted_renderer_handles_malformed_optional_artifact_fields(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            report = self.write_report(Path(temporary), report_payload())
            run = read_sequential_navigation_report(report, 0)
        comparison = build_comparison(
            base_sha=BASE_SHA,
            head_sha=HEAD_SHA,
            execution_order="base-first",
            expected_navigations=200,
            base_run=run,
            head_run=run,
        )
        comparison["runs"]["head"]["metrics"]["periodic_observer_error"] = {}
        comparison["runs"]["head"]["metrics"]["quarters"] = ["untrusted"] * 100

        comment = render_comparison_comment(
            comparison,
            run_url=None,
            conclusion="failure",
        )

        self.assertIn("periodic resource observer failed", comment)
        self.assertIn("navigation-quarter memory is unavailable", comment)


if __name__ == "__main__":
    unittest.main()
