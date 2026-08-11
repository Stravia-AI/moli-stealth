from __future__ import annotations

import unittest

from moli_benchmark.raw_cdp import RecordedCdpMessage
from moli_benchmark.sequential_navigate import (
    NavigationIdentity,
    find_exact_lifecycle_record,
    find_navigation_progress_record,
    make_recovery_url,
    network_event_order_violations,
    navigation_order_violations,
    normalize_public_url,
    parse_top_100_domains,
    select_seed_urls,
    selected_engines,
    summarize_network_activity,
)


def record(sequence: int, payload: dict[str, object]) -> RecordedCdpMessage:
    return RecordedCdpMessage(
        sequence=sequence,
        received_monotonic=float(sequence),
        received_epoch=float(sequence),
        payload=payload,
    )


def lifecycle(
    sequence: int,
    *,
    frame_id: str,
    loader_id: str,
    name: str,
    session_id: str = "SID-1",
) -> RecordedCdpMessage:
    return record(
        sequence,
        {
            "sessionId": session_id,
            "method": "Page.lifecycleEvent",
            "params": {
                "frameId": frame_id,
                "loaderId": loader_id,
                "name": name,
            },
        },
    )


def navigation_started(
    sequence: int,
    *,
    frame_id: str,
    loader_id: str,
    url: str = "https://next.test/",
    session_id: str = "SID-1",
) -> RecordedCdpMessage:
    return record(
        sequence,
        {
            "sessionId": session_id,
            "method": "Page.frameStartedNavigating",
            "params": {
                "frameId": frame_id,
                "loaderId": loader_id,
                "url": url,
            },
        },
    )


def network_event(
    sequence: int,
    *,
    method: str,
    request_id: str,
    session_id: str = "SID-1",
) -> RecordedCdpMessage:
    params: dict[str, object] = {"requestId": request_id}
    if method == "Network.requestWillBeSent":
        params.update(
            {
                "type": "XHR",
                "request": {"url": f"https://example.test/{request_id}"},
            }
        )
    elif method == "Network.responseReceived":
        params["type"] = "XHR"
    return record(
        sequence,
        {
            "sessionId": session_id,
            "method": method,
            "params": params,
        },
    )


class SequentialNavigateTests(unittest.TestCase):
    def test_url_normalization_preserves_explicit_data_url(self) -> None:
        url = "data:text/html,<title>fixture</title>"

        self.assertEqual(normalize_public_url(url), url)

    def test_seed_parser_reads_only_top_100_section(self) -> None:
        markdown = """
## Earlier
1. `ignored.example`
## Top 100
1. `csdn.net` — CSDN
2. `zol.com.cn` — ZOL
## Maintenance
1. `also-ignored.example`
"""
        self.assertEqual(parse_top_100_domains(markdown), ["csdn.net", "zol.com.cn"])

    def test_seed_selection_pins_repros_and_is_deterministic(self) -> None:
        domains = ["alpha.test", "csdn.net", "beta.test", "zol.com.cn", "gamma.test"]
        first = select_seed_urls(domains, seed=17, count=4)
        second = select_seed_urls(domains, seed=17, count=4)

        self.assertEqual(first, second)
        self.assertEqual(first[:2], ["https://csdn.net", "https://zol.com.cn"])
        self.assertEqual(len(set(first)), 4)

    def test_lifecycle_match_requires_exact_session_frame_and_loader(self) -> None:
        identity = NavigationIdentity(frame_id="FRAME-NEW", loader_id="LOADER-NEW")
        records = [
            lifecycle(1, frame_id="FRAME-OLD", loader_id="LOADER-OLD", name="DOMContentLoaded"),
            lifecycle(2, frame_id="FRAME-NEW", loader_id="LOADER-OLD", name="DOMContentLoaded"),
            lifecycle(
                3,
                frame_id="FRAME-NEW",
                loader_id="LOADER-NEW",
                name="DOMContentLoaded",
                session_id="SID-OLD",
            ),
            lifecycle(4, frame_id="FRAME-NEW", loader_id="LOADER-NEW", name="DOMContentLoaded"),
        ]

        matched = find_exact_lifecycle_record(
            records,
            session_id="SID-1",
            identity=identity,
            name="DOMContentLoaded",
        )

        self.assertIsNotNone(matched)
        self.assertEqual(matched.sequence if matched else None, 4)

    def test_navigation_progress_uses_earliest_lifecycle_or_same_frame_successor(self) -> None:
        identity = NavigationIdentity(frame_id="FRAME", loader_id="LOADER-INITIAL")
        successor = navigation_started(
            2,
            frame_id="FRAME",
            loader_id="LOADER-SUCCESSOR",
        )
        records = [
            navigation_started(1, frame_id="OTHER", loader_id="LOADER-IGNORED"),
            successor,
            lifecycle(3, frame_id="FRAME", loader_id="LOADER-INITIAL", name="DOMContentLoaded"),
        ]

        progress = find_navigation_progress_record(
            records,
            session_id="SID-1",
            identity=identity,
            lifecycle_name="DOMContentLoaded",
        )

        self.assertIsNotNone(progress)
        self.assertEqual(progress.kind if progress else None, "successor")
        self.assertEqual(progress.record.sequence if progress else None, 2)
        self.assertEqual(
            progress.identity if progress else None,
            NavigationIdentity(frame_id="FRAME", loader_id="LOADER-SUCCESSOR"),
        )

    def test_navigation_progress_keeps_lifecycle_when_it_precedes_successor(self) -> None:
        identity = NavigationIdentity(frame_id="FRAME", loader_id="LOADER-INITIAL")
        records = [
            lifecycle(1, frame_id="FRAME", loader_id="LOADER-INITIAL", name="DOMContentLoaded"),
            navigation_started(2, frame_id="FRAME", loader_id="LOADER-SUCCESSOR"),
        ]

        progress = find_navigation_progress_record(
            records,
            session_id="SID-1",
            identity=identity,
            lifecycle_name="DOMContentLoaded",
        )

        self.assertIsNotNone(progress)
        self.assertEqual(progress.kind if progress else None, "lifecycle")
        self.assertEqual(progress.record.sequence if progress else None, 1)

    def test_order_check_rejects_lifecycle_before_navigate_response(self) -> None:
        response = record(3, {"id": 7, "result": {}})
        dcl = lifecycle(2, frame_id="FRAME", loader_id="LOADER", name="DOMContentLoaded")
        load = lifecycle(4, frame_id="FRAME", loader_id="LOADER", name="load")

        self.assertEqual(
            navigation_order_violations(response, dcl, load),
            ["DOMContentLoaded was observable before Page.navigate response"],
        )

    def test_order_check_accepts_response_then_dcl_then_load(self) -> None:
        response = record(2, {"id": 7, "result": {}})
        dcl = lifecycle(3, frame_id="FRAME", loader_id="LOADER", name="DOMContentLoaded")
        load = lifecycle(4, frame_id="FRAME", loader_id="LOADER", name="load")

        self.assertEqual(navigation_order_violations(response, dcl, load), [])

    def test_recovery_url_carries_unique_marker(self) -> None:
        url = make_recovery_url("marker-123")

        self.assertTrue(url.startswith("data:text/html;charset=utf-8,"))
        self.assertIn("marker-123", url)

    def test_engine_selection_can_run_each_engine_or_both(self) -> None:
        self.assertEqual(selected_engines("moli"), ("moli",))
        self.assertEqual(selected_engines("chromium"), ("chromium",))
        self.assertEqual(selected_engines("both"), ("moli", "chromium"))

        with self.assertRaisesRegex(ValueError, "unsupported engine"):
            selected_engines("unknown")

    def test_network_summary_reports_terminal_and_inflight_requests(self) -> None:
        records = [
            record(
                1,
                {
                    "sessionId": "SID-1",
                    "method": "Network.requestWillBeSent",
                    "params": {
                        "requestId": "REQ-finished",
                        "frameId": "FRAME",
                        "loaderId": "LOADER",
                        "type": "Document",
                        "request": {"url": "https://example.test/"},
                        "initiator": {"type": "other"},
                    },
                },
            ),
            record(
                2,
                {
                    "sessionId": "SID-1",
                    "method": "Network.loadingFinished",
                    "params": {"requestId": "REQ-finished"},
                },
            ),
            record(
                3,
                {
                    "sessionId": "SID-1",
                    "method": "Network.requestWillBeSent",
                    "params": {
                        "requestId": "REQ-inflight",
                        "frameId": "FRAME",
                        "loaderId": "LOADER",
                        "type": "Script",
                        "request": {"url": "https://example.test/blocker.js"},
                        "initiator": {"type": "parser"},
                    },
                },
            ),
            record(
                4,
                {
                    "sessionId": "SID-other",
                    "method": "Network.loadingFailed",
                    "params": {"requestId": "REQ-inflight", "errorText": "ignored"},
                },
            ),
        ]

        summary = summarize_network_activity(records, session_id="SID-1", started=0.0)

        self.assertEqual(summary["request_count"], 2)
        self.assertEqual(summary["finished_count"], 1)
        self.assertEqual(summary["failed_count"], 0)
        self.assertEqual(summary["inflight_count"], 1)
        self.assertEqual(summary["requests"][1]["request_id"], "REQ-inflight")
        self.assertEqual(summary["requests"][1]["type"], "Script")
        self.assertIsNone(summary["requests"][1]["terminal"])

    def test_network_order_audit_accepts_success_failure_and_redirect_order(self) -> None:
        records = [
            network_event(1, method="Network.requestWillBeSent", request_id="REQ-ok"),
            network_event(2, method="Network.requestWillBeSent", request_id="REQ-ok"),
            network_event(3, method="Network.responseReceived", request_id="REQ-ok"),
            network_event(4, method="Network.loadingFinished", request_id="REQ-ok"),
            network_event(5, method="Network.requestWillBeSent", request_id="REQ-fail"),
            network_event(6, method="Network.loadingFailed", request_id="REQ-fail"),
            network_event(
                7,
                method="Network.loadingFinished",
                request_id="REQ-other-session",
                session_id="SID-other",
            ),
        ]

        self.assertEqual(
            network_event_order_violations(records, session_id="SID-1"),
            [],
        )

    def test_network_order_audit_reports_success_terminal_before_start_and_response(self) -> None:
        records = [
            network_event(1, method="Network.loadingFinished", request_id="REQ-late"),
            network_event(2, method="Network.requestWillBeSent", request_id="REQ-late"),
            network_event(3, method="Network.responseReceived", request_id="REQ-late"),
        ]

        violations = network_event_order_violations(records, session_id="SID-1")

        self.assertEqual(
            [violation["kind"] for violation in violations],
            ["terminal_before_start", "successful_terminal_before_response"],
        )
        self.assertTrue(
            all(violation["request_id"] == "REQ-late" for violation in violations)
        )

    def test_network_order_audit_reports_response_order_and_duplicate_terminal(self) -> None:
        records = [
            network_event(1, method="Network.responseReceived", request_id="REQ-response"),
            network_event(2, method="Network.requestWillBeSent", request_id="REQ-response"),
            network_event(3, method="Network.requestWillBeSent", request_id="REQ-duplicate"),
            network_event(4, method="Network.loadingFailed", request_id="REQ-duplicate"),
            network_event(5, method="Network.loadingFailed", request_id="REQ-duplicate"),
        ]

        violations = network_event_order_violations(records, session_id="SID-1")

        self.assertEqual(
            [(violation["request_id"], violation["kind"]) for violation in violations],
            [
                ("REQ-response", "response_before_start"),
                ("REQ-duplicate", "duplicate_terminal"),
            ],
        )


if __name__ == "__main__":
    unittest.main()
