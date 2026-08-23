from __future__ import annotations

import json
import tempfile
import unittest
import urllib.request
from pathlib import Path

from moli_benchmark.navigation_trace import (
    EXPECTED_LIFECYCLE_MILESTONES,
    NavigationTraceFixture,
    _navigation_a_document,
    load_browser_owner_trace,
    normalize_lifecycle_milestones,
    normalize_owner_shape,
)


def _document(page_id: int, generation: int, epoch: int) -> dict[str, int]:
    return {
        "renderer_page_id": page_id,
        "document_generation": generation,
        "lifecycle_epoch": epoch,
    }


class NavigationTraceTests(unittest.TestCase):
    def test_fixture_records_exact_navigation_request_order(self) -> None:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with NavigationTraceFixture() as fixture:
            for stage in ("a", "b", "complete"):
                with opener.open(fixture.url(stage, "unit-run"), timeout=2) as response:
                    response.read()
            fixture.wait_for("unit-run", "complete", 2)

            self.assertEqual(
                fixture.navigation_request_stages("unit-run"),
                ["a", "b", "complete"],
            )

    def test_fixture_navigation_is_inside_load_boundary_not_future_timer(self) -> None:
        source = _navigation_a_document().decode("utf-8")

        self.assertIn("addEventListener('load'", source)
        self.assertIn("location.href = '/navigation-trace/b", source)
        self.assertNotIn("setTimeout", source)

    def test_jsonl_loader_honors_record_boundary_offset(self) -> None:
        first = {"schema_version": 1, "stage": "bootstrap"}
        second = {"schema_version": 1, "stage": "measured"}
        first_line = (json.dumps(first) + "\n").encode("utf-8")
        with tempfile.TemporaryDirectory() as temp_dir:
            trace_path = Path(temp_dir) / "trace.jsonl"
            trace_path.write_bytes(first_line + (json.dumps(second) + "\n").encode("utf-8"))

            records = load_browser_owner_trace(trace_path, len(first_line))

        self.assertEqual(records, [second])

    def test_jsonl_loader_rejects_partial_record(self) -> None:
        with tempfile.TemporaryDirectory() as temp_dir:
            trace_path = Path(temp_dir) / "trace.jsonl"
            trace_path.write_text('{"schema_version":1}', encoding="utf-8")

            with self.assertRaisesRegex(RuntimeError, "partial JSONL record"):
                load_browser_owner_trace(trace_path)

    def test_lifecycle_normalization_keeps_exact_document_order(self) -> None:
        records = [
            {
                "stage": "renderer_lifecycle_reached",
                "renderer_lifecycle_kind": "started",
                "document_lifecycle_identity": _document(9, 4, 1),
            },
            {
                "stage": "renderer_lifecycle_reached",
                "renderer_lifecycle_kind": "dom-content-loaded",
                "document_lifecycle_identity": _document(9, 4, 1),
            },
            {
                "stage": "renderer_lifecycle_reached",
                "renderer_lifecycle_kind": "load",
                "document_lifecycle_identity": _document(9, 4, 1),
            },
            {
                "stage": "renderer_lifecycle_reached",
                "renderer_lifecycle_kind": "terminated",
                "document_lifecycle_identity": _document(9, 4, 1),
            },
            {
                "stage": "renderer_lifecycle_reached",
                "renderer_lifecycle_kind": "dom-content-loaded",
                "document_lifecycle_identity": _document(9, 5, 1),
            },
            {
                "stage": "renderer_lifecycle_reached",
                "renderer_lifecycle_kind": "load",
                "document_lifecycle_identity": _document(9, 5, 1),
            },
        ]

        self.assertEqual(
            normalize_lifecycle_milestones(records),
            list(EXPECTED_LIFECYCLE_MILESTONES),
        )

    def test_owner_shape_removes_ephemeral_ids_and_frontend_projection(self) -> None:
        records = [
            {
                "stage": "browser_action_published",
                "source": "frontend-command",
                "navigation_origin": "frontend-command",
                "owner_state_before": "frontend",
                "owner_state_after": "browser-owner-inbox",
                "browser_action_id": 91,
                "navigation_request_id": None,
                "page_residence_generation": 7,
                "document_lifecycle_identity": _document(11, 3, 1),
            },
            {
                "stage": "navigation_request_started",
                "source": "frontend-command",
                "navigation_origin": "frontend-command",
                "owner_state_before": "browser-owner",
                "owner_state_after": "request-pending",
                "browser_action_id": 91,
                "navigation_request_id": 42,
                "page_residence_generation": 7,
                "document_lifecycle_identity": _document(11, 3, 1),
            },
            {
                "stage": "frontend_load_projected",
                "source": "lifecycle",
                "browser_action_id": 91,
            },
            {
                "stage": "renderer_intent_published",
                "source": "renderer-intent",
                "navigation_origin": "renderer-intent",
                "owner_state_before": "renderer-task",
                "owner_state_after": "renderer-output",
                "browser_action_id": 117,
                "navigation_request_id": 99,
                "page_residence_generation": 8,
                "document_lifecycle_identity": _document(11, 4, 1),
            },
        ]

        normalized = normalize_owner_shape(records)

        self.assertEqual(
            [(record["action"], record["request"], record["page"], record["document"]) for record in normalized],
            [
                ("A1", None, "P1", "D1"),
                ("A1", "R1", "P1", "D1"),
                ("A2", "R2", "P2", "D2"),
            ],
        )
        self.assertFalse(any(record["stage"].startswith("frontend_") for record in normalized))


if __name__ == "__main__":
    unittest.main()
