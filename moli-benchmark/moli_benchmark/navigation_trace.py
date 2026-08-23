"""Exact-navigation differential fixture for Moli frontends.

Document A requests Document B synchronously from its load handler. That is a
FollowBeforeReply boundary covered by standalone fetch; it deliberately does
not ask wait-until done to wait for an unknown future timer. CDP and BiDi send
no command after the A navigation until B has independently reached load.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

from .artifacts import write_csv, write_json
from .navigation_trace_fixture import NavigationTraceFixture, _navigation_a_document
from .navigation_trace_frontends import _prepare_trace_path, _run_frontend, _url_stage
from .navigation_trace_records import (
    load_browser_owner_trace,
    normalize_lifecycle_milestones,
    normalize_owner_shape,
)
from .stats import summarize

NAVIGATION_TRACE_FRONTENDS = ("cli", "cdp", "bidi", "classic")
EXPECTED_REQUEST_STAGES = ("a", "b", "complete")
EXPECTED_LIFECYCLE_MILESTONES = (
    "D1:dom-content-loaded",
    "D1:load",
    "D2:dom-content-loaded",
    "D2:load",
)
EXPECTED_PROTOCOL_LIFECYCLE = (
    "a:dom-content-loaded",
    "a:load",
    "b:dom-content-loaded",
    "b:load",
)
EXPECTED_OWNER_STAGE_SEQUENCE = (
    "browser_action_published",
    "browser_owner_accepted",
    "navigation_request_started",
    "network_request_admitted",
    "response_commit_ready",
    "page_replacement_committed",
    "browser_domcontentloaded_observed",
    "renderer_intent_published",
    "browser_action_published",
    "browser_load_observed",
    "browser_owner_accepted",
    "navigation_request_started",
    "network_request_admitted",
    "response_commit_ready",
    "page_replacement_committed",
    "browser_domcontentloaded_observed",
    "browser_load_observed",
)


def _finalize_frontend_result(
    frontend: str,
    fixture: NavigationTraceFixture,
    run_token: str,
    trace_path: Path,
    raw: dict[str, Any],
) -> dict[str, Any]:
    records = load_browser_owner_trace(trace_path, int(raw.get("trace_offset") or 0))
    request_stages = fixture.navigation_request_stages(run_token)
    lifecycle_milestones = normalize_lifecycle_milestones(records)
    owner_shape = normalize_owner_shape(records)
    final_observation = raw.get("final_observation") or {}
    errors: list[str] = []
    if request_stages != list(EXPECTED_REQUEST_STAGES):
        errors.append(f"request stages were {request_stages!r}")
    if lifecycle_milestones != list(EXPECTED_LIFECYCLE_MILESTONES):
        errors.append(f"lifecycle milestones were {lifecycle_milestones!r}")
    if frontend in {"cdp", "bidi"} and raw.get("protocol_lifecycle") != list(
        EXPECTED_PROTOCOL_LIFECYCLE
    ):
        errors.append(f"protocol lifecycle was {raw.get('protocol_lifecycle')!r}")
    if _url_stage(str(final_observation.get("url") or "")) != "b":
        errors.append(f"final URL was {final_observation.get('url')!r}")
    if final_observation.get("title") != "Trace B" or final_observation.get("phase") != "b":
        errors.append(f"final document was {final_observation!r}")
    if raw.get("followup_commands_before_terminal") != 0:
        errors.append("a frontend follow-up command was sent before successor load")
    if frontend != "cli":
        if not any(record.get("browser_instance_id") is not None for record in records):
            errors.append("protocol frontend trace lacked Browser Owner correlation")
        owner_stages = [record["stage"] for record in owner_shape]
        if owner_stages != list(EXPECTED_OWNER_STAGE_SEQUENCE):
            errors.append(f"Browser Owner stages were {owner_stages!r}")
    return {
        **raw,
        "frontend": frontend,
        "run_token": run_token,
        "ok": not errors,
        "errors": errors,
        "trace_path": str(trace_path),
        "trace_record_count": len(records),
        "trace_schema_versions": sorted({record.get("schema_version") for record in records}),
        "request_records": fixture.request_records(run_token),
        "request_stages": request_stages,
        "lifecycle_milestones": lifecycle_milestones,
        "owner_shape": owner_shape,
        "visible_shape": {
            "request_stages": request_stages,
            "lifecycle_milestones": lifecycle_milestones,
            "final_stage": _url_stage(str(final_observation.get("url") or "")),
            "final_title": final_observation.get("title"),
            "final_phase": final_observation.get("phase"),
        },
    }


def _shape_digest(value: Any) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def run_navigation_trace_suite(
    *,
    moli_bin: Path,
    output_dir: Path,
    runs: int,
    timeout_seconds: float,
) -> dict[str, Any]:
    if runs <= 0:
        raise RuntimeError("navigation trace runs must be positive")
    suite_dir = output_dir / "navigation-trace"
    details: list[dict[str, Any]] = []
    cross_frontend_rows: list[dict[str, Any]] = []
    with NavigationTraceFixture() as fixture:
        for run_id in range(1, runs + 1):
            run_details: dict[str, dict[str, Any]] = {}
            for frontend in NAVIGATION_TRACE_FRONTENDS:
                run_token = f"{frontend}-{run_id}"
                trace_path = suite_dir / "traces" / f"{run_token}.jsonl"
                _prepare_trace_path(trace_path)
                try:
                    raw = _run_frontend(
                        frontend,
                        moli_bin,
                        fixture,
                        run_token,
                        trace_path,
                        timeout_seconds,
                    )
                    detail = _finalize_frontend_result(
                        frontend, fixture, run_token, trace_path, raw
                    )
                except Exception as error:
                    detail = {
                        "frontend": frontend,
                        "run_token": run_token,
                        "ok": False,
                        "errors": [f"{type(error).__name__}: {error}"],
                        "trace_path": str(trace_path),
                        "request_records": fixture.request_records(run_token),
                    }
                details.append(detail)
                run_details[frontend] = detail

            visible_shapes = {
                frontend: detail.get("visible_shape")
                for frontend, detail in run_details.items()
                if detail.get("ok")
            }
            owner_shapes = {
                frontend: run_details[frontend].get("owner_shape")
                for frontend in ("cdp", "bidi", "classic")
                if run_details[frontend].get("ok")
            }
            visible_match = len(visible_shapes) == len(NAVIGATION_TRACE_FRONTENDS) and len(
                {_shape_digest(shape) for shape in visible_shapes.values()}
            ) == 1
            owner_match = len(owner_shapes) == 3 and len(
                {_shape_digest(shape) for shape in owner_shapes.values()}
            ) == 1
            cross_frontend_rows.append(
                {
                    "run": run_id,
                    "ok": visible_match and owner_match,
                    "visible_match": visible_match,
                    "owner_match": owner_match,
                    "visible_digests": {
                        frontend: _shape_digest(shape) for frontend, shape in visible_shapes.items()
                    },
                    "owner_digests": {
                        frontend: _shape_digest(shape) for frontend, shape in owner_shapes.items()
                    },
                }
            )

    rows = [
        {
            "frontend": detail["frontend"],
            "run_token": detail["run_token"],
            "ok": detail.get("ok", False),
            "navigation_elapsed_ms": detail.get("navigation_elapsed_ms"),
            "latency_scope": detail.get("latency_scope"),
            "peak_pss_bytes": (detail.get("resources") or {}).get("peak_pss_bytes"),
            "peak_rss_bytes": (detail.get("resources") or {}).get("peak_rss_bytes"),
            "peak_cpu_percent": (detail.get("resources") or {}).get("peak_cpu_percent"),
            "resource_sample_count": (detail.get("resources") or {}).get("sample_count"),
            "trace_record_count": detail.get("trace_record_count"),
            "visible_shape_sha256": (
                _shape_digest(detail["visible_shape"]) if detail.get("visible_shape") else None
            ),
            "owner_shape_sha256": (
                _shape_digest(detail["owner_shape"]) if detail.get("owner_shape") else None
            ),
            "errors": " | ".join(detail.get("errors") or []),
        }
        for detail in details
    ]
    individual_failures = sum(1 for detail in details if not detail.get("ok"))
    cross_frontend_failures = sum(1 for row in cross_frontend_rows if not row["ok"])
    summary: dict[str, Any] = {
        "suite": "navigation-trace",
        "runs": runs,
        "timeout_seconds": timeout_seconds,
        "frontends": {},
        "cross_frontend": cross_frontend_rows,
        "individual_failures": individual_failures,
        "cross_frontend_failures": cross_frontend_failures,
        "total_failures": individual_failures + cross_frontend_failures,
        "gate_failures": individual_failures + cross_frontend_failures,
    }
    for frontend in NAVIGATION_TRACE_FRONTENDS:
        frontend_rows = [row for row in rows if row["frontend"] == frontend]
        summary["frontends"][frontend] = {
            "navigation_elapsed_ms": summarize(
                row["navigation_elapsed_ms"]
                for row in frontend_rows
                if row.get("ok") and row.get("navigation_elapsed_ms") is not None
            ),
            "peak_pss_bytes": summarize(
                row["peak_pss_bytes"]
                for row in frontend_rows
                if row.get("ok") and row.get("peak_pss_bytes") is not None
            ),
            "peak_rss_bytes": summarize(
                row["peak_rss_bytes"]
                for row in frontend_rows
                if row.get("ok") and row.get("peak_rss_bytes") is not None
            ),
            "peak_cpu_percent": summarize(
                row["peak_cpu_percent"]
                for row in frontend_rows
                if row.get("ok") and row.get("peak_cpu_percent") is not None
            ),
            "resource_sample_count": summarize(
                row["resource_sample_count"]
                for row in frontend_rows
                if row.get("ok") and row.get("resource_sample_count") is not None
            ),
            "failures": sum(1 for row in frontend_rows if not row.get("ok")),
        }

    write_csv(suite_dir / "runs.csv", rows)
    write_json(suite_dir / "runs.json", details)
    write_json(suite_dir / "cross-frontend.json", cross_frontend_rows)
    write_json(suite_dir / "summary.json", summary)
    return summary
