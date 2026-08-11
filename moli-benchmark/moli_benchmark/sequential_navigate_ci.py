from __future__ import annotations

import argparse
import json
import math
import re
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


COMPARISON_SCHEMA = "moli.sequential-navigation.comparison.v1"
COMMENT_MARKER = "<!-- moli-sequential-navigation-soak -->"
EXPECTED_REPORT_SCHEMA_VERSION = 5
SHA_PATTERN = re.compile(r"^[0-9a-f]{40}$")
NUMERIC_METRICS = (
    "planned",
    "attempted",
    "observable_passes",
    "failures",
    "recovery_attempts",
    "recovery_passes",
    "recovery_failures",
    "order_violations",
    "network_order_violations",
    "resource_sample_count",
    "resource_sample_errors",
    "rss_observed_samples",
    "pss_observed_samples",
    "fd_observed_samples",
    "duration_seconds",
    "periodic_peak_rss_bytes",
    "periodic_peak_pss_bytes",
    "periodic_peak_fd_count",
    "periodic_peak_thread_count",
    "rss_first_window_average",
    "rss_last_window_average",
    "rss_first_to_last_window_delta",
    "rss_warm_slope_per_100_navigations",
    "pss_first_window_average",
    "pss_last_window_average",
    "pss_first_to_last_window_delta",
    "pss_warm_slope_per_100_navigations",
)


def _finite_number(value: Any) -> int | float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    if not math.isfinite(number) or abs(number) > 1e18:
        return None
    return value


def _integer(value: Any) -> int | None:
    value = _finite_number(value)
    if value is None or int(value) != value:
        return None
    return int(value)


def _nested(mapping: Any, *keys: str) -> Any:
    current = mapping
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current


def _duration_seconds(started_at: Any, finished_at: Any) -> float | None:
    if not isinstance(started_at, str) or not isinstance(finished_at, str):
        return None
    try:
        started = datetime.fromisoformat(started_at)
        finished = datetime.fromisoformat(finished_at)
    except ValueError:
        return None
    duration = (finished - started).total_seconds()
    return duration if math.isfinite(duration) and duration >= 0 else None


def _unavailable_run(exit_code: int, reason: str) -> dict[str, Any]:
    return {
        "available": False,
        "exit_code": exit_code,
        "reason": reason[:240],
        "metrics": None,
    }


def _sanitize_quarters(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list):
        return []
    quarters = []
    for raw in value[:4]:
        if not isinstance(raw, dict):
            continue
        quarter = _integer(raw.get("quarter"))
        if quarter not in {1, 2, 3, 4}:
            continue
        quarters.append(
            {
                "quarter": quarter,
                "start_index": _integer(raw.get("start_index")),
                "end_index": _integer(raw.get("end_index")),
                "sample_count": _integer(raw.get("sample_count")),
                "rss_average": _finite_number(_nested(raw, "rss_bytes", "average")),
                "rss_final": _finite_number(_nested(raw, "rss_bytes", "final")),
                "rss_peak": _finite_number(_nested(raw, "rss_bytes", "peak")),
                "pss_average": _finite_number(_nested(raw, "pss_bytes", "average")),
                "pss_final": _finite_number(_nested(raw, "pss_bytes", "final")),
                "pss_peak": _finite_number(_nested(raw, "pss_bytes", "peak")),
                "fd_peak": _finite_number(_nested(raw, "fd_count", "peak")),
            }
        )
    return sorted(quarters, key=lambda quarter: quarter["quarter"])


def read_sequential_navigation_report(path: Path, exit_code: int) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        return _unavailable_run(exit_code, f"report unavailable: {type(error).__name__}")
    if not isinstance(payload, dict):
        return _unavailable_run(exit_code, "report root is not an object")
    if payload.get("schema_version") != EXPECTED_REPORT_SCHEMA_VERSION:
        return _unavailable_run(exit_code, "unsupported sequential navigation report schema")
    results = payload.get("results")
    if not isinstance(results, list):
        return _unavailable_run(exit_code, "report has no result list")
    result = next(
        (
            candidate
            for candidate in results
            if isinstance(candidate, dict)
            and isinstance(candidate.get("target"), str)
            and candidate["target"].startswith("moli")
        ),
        None,
    )
    if result is None:
        return _unavailable_run(exit_code, "report has no Moli result")
    summary = result.get("summary")
    navigation_resources = result.get("navigation_resources")
    memory = _nested(navigation_resources, "summary")
    if not isinstance(summary, dict) or not isinstance(memory, dict):
        return _unavailable_run(exit_code, "report is missing summary or navigation resources")

    metrics = {
        "planned": _integer(summary.get("planned")),
        "attempted": _integer(summary.get("attempted")),
        "observable_passes": _integer(summary.get("observable_passes")),
        "failures": _integer(summary.get("failures")),
        "recovery_attempts": _integer(summary.get("recovery_attempts")),
        "recovery_passes": _integer(summary.get("recovery_passes")),
        "recovery_failures": _integer(summary.get("recovery_failures")),
        "order_violations": _integer(summary.get("order_violations")),
        "network_order_violations": _integer(summary.get("network_order_violations")),
        "aborted_after_index": _integer(summary.get("aborted_after_index")),
        "resource_sample_count": _integer(memory.get("sample_count")),
        "resource_sample_errors": _integer(memory.get("sample_errors")),
        "resource_initial_sample_present": memory.get("initial_sample_present") is True,
        "rss_observed_samples": _integer(
            _nested(memory, "rss_bytes", "observed_samples")
        ),
        "pss_observed_samples": _integer(
            _nested(memory, "pss_bytes", "observed_samples")
        ),
        "fd_observed_samples": _integer(
            _nested(memory, "fd_count", "observed_samples")
        ),
        "duration_seconds": _duration_seconds(
            result.get("started_at"), result.get("finished_at")
        ),
        "process_returncode": _integer(_nested(result, "process", "returncode")),
        "periodic_observer_error": _nested(memory, "periodic", "observer_error"),
        "periodic_peak_rss_bytes": _finite_number(
            _nested(memory, "periodic", "peak_rss_bytes")
        ),
        "periodic_peak_pss_bytes": _finite_number(
            _nested(memory, "periodic", "peak_pss_bytes")
        ),
        "periodic_peak_fd_count": _finite_number(
            _nested(memory, "periodic", "peak_fd_count")
        ),
        "periodic_peak_thread_count": _finite_number(
            _nested(memory, "periodic", "peak_thread_count")
        ),
        "rss_first_window_average": _finite_number(
            _nested(memory, "rss_bytes", "first_window_average")
        ),
        "rss_last_window_average": _finite_number(
            _nested(memory, "rss_bytes", "last_window_average")
        ),
        "rss_first_to_last_window_delta": _finite_number(
            _nested(memory, "rss_bytes", "first_to_last_window_delta")
        ),
        "rss_warm_slope_per_100_navigations": _finite_number(
            _nested(memory, "rss_bytes", "warm_slope_per_100_navigations")
        ),
        "pss_first_window_average": _finite_number(
            _nested(memory, "pss_bytes", "first_window_average")
        ),
        "pss_last_window_average": _finite_number(
            _nested(memory, "pss_bytes", "last_window_average")
        ),
        "pss_first_to_last_window_delta": _finite_number(
            _nested(memory, "pss_bytes", "first_to_last_window_delta")
        ),
        "pss_warm_slope_per_100_navigations": _finite_number(
            _nested(memory, "pss_bytes", "warm_slope_per_100_navigations")
        ),
        "quarters": _sanitize_quarters(memory.get("quarters")),
    }
    return {
        "available": True,
        "exit_code": exit_code,
        "reason": None,
        "metrics": metrics,
    }


def _validated_sha(value: str) -> str:
    if not SHA_PATTERN.fullmatch(value):
        raise ValueError(f"invalid commit SHA: {value!r}")
    return value


def build_comparison(
    *,
    base_sha: str,
    head_sha: str,
    execution_order: str,
    expected_navigations: int,
    base_run: dict[str, Any],
    head_run: dict[str, Any],
) -> dict[str, Any]:
    if execution_order not in {"base-first", "head-first"}:
        raise ValueError(f"invalid execution order: {execution_order!r}")
    if expected_navigations <= 0 or expected_navigations > 10_000:
        raise ValueError("expected navigations must be between 1 and 10000")
    delta: dict[str, int | float | None] = {}
    base_metrics = base_run.get("metrics") if base_run.get("available") else None
    head_metrics = head_run.get("metrics") if head_run.get("available") else None
    for metric in NUMERIC_METRICS:
        base_value = _finite_number(
            base_metrics.get(metric) if isinstance(base_metrics, dict) else None
        )
        head_value = _finite_number(
            head_metrics.get(metric) if isinstance(head_metrics, dict) else None
        )
        delta[metric] = (
            head_value - base_value
            if base_value is not None and head_value is not None
            else None
        )
    return {
        "schema": COMPARISON_SCHEMA,
        "created_at": datetime.now(UTC).isoformat(),
        "base": {"sha": _validated_sha(base_sha)},
        "head": {"sha": _validated_sha(head_sha)},
        "execution_order": execution_order,
        "expected_navigations": expected_navigations,
        "workload": {
            "single_process": True,
            "single_target": True,
            "single_session": True,
            "cycle_size": 4,
            "cycle_count": expected_navigations // 4,
        },
        "runs": {"base": base_run, "head": head_run},
        "delta": delta,
    }


def comparison_health_issues(comparison: dict[str, Any]) -> list[str]:
    if comparison.get("schema") != COMPARISON_SCHEMA:
        return ["unsupported comparison schema"]
    expected = _integer(comparison.get("expected_navigations"))
    if expected is None or expected <= 0 or expected > 10_000:
        return ["missing expected navigation count"]
    issues = []
    for side in ("base", "head"):
        run = _nested(comparison, "runs", side)
        if not isinstance(run, dict) or run.get("available") is not True:
            issues.append(f"{side} report unavailable")
    head = _nested(comparison, "runs", "head", "metrics")
    if not isinstance(head, dict):
        return issues or ["HEAD metrics unavailable"]
    required_equal = {
        "planned": expected,
        "attempted": expected,
        "resource_sample_count": expected,
        "rss_observed_samples": expected,
        "pss_observed_samples": expected,
        "fd_observed_samples": expected,
        "recovery_failures": 0,
        "order_violations": 0,
        "network_order_violations": 0,
        "resource_sample_errors": 0,
    }
    for metric, required in required_equal.items():
        if _integer(head.get(metric)) != required:
            issues.append(f"HEAD {metric} is not {required}")
    if head.get("aborted_after_index") is not None:
        issues.append("HEAD session aborted before all navigations")
    if head.get("resource_initial_sample_present") is not True:
        issues.append("HEAD initial resource sample is unavailable")
    if _integer(head.get("process_returncode")) not in {0, 143, -15}:
        issues.append("HEAD browser process had an unexpected exit status")
    observer_error = head.get("periodic_observer_error")
    if observer_error is not None and observer_error != "":
        issues.append("HEAD periodic resource observer failed")
    for metric in ("periodic_peak_rss_bytes", "periodic_peak_pss_bytes"):
        if _finite_number(head.get(metric)) is None:
            issues.append(f"HEAD {metric} is unavailable")
    return issues


def _format_integer(value: Any) -> str:
    value = _integer(value)
    return f"{value:,}" if value is not None else "—"


def _format_number(value: Any, digits: int = 1) -> str:
    value = _finite_number(value)
    return f"{float(value):,.{digits}f}" if value is not None else "—"


def _format_mib(value: Any) -> str:
    value = _finite_number(value)
    return f"{float(value) / 1024 / 1024:,.1f} MiB" if value is not None else "—"


def _format_signed_mib(value: Any) -> str:
    value = _finite_number(value)
    return f"{float(value) / 1024 / 1024:+,.1f} MiB" if value is not None else "—"


def _format_seconds(value: Any) -> str:
    value = _finite_number(value)
    return f"{float(value):,.1f} s" if value is not None else "—"


def _format_ratio(numerator: Any, denominator: Any) -> str:
    return f"{_format_integer(numerator)} / {_format_integer(denominator)}"


def _short_sha(value: Any) -> str:
    return value[:10] if isinstance(value, str) and SHA_PATTERN.fullmatch(value) else "unknown"


def _safe_run_url(value: str | None) -> str | None:
    if not isinstance(value, str):
        return None
    match = re.fullmatch(
        r"https://github\.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+/actions/runs/[0-9]+",
        value,
    )
    return value if match else None


def _safe_conclusion(value: str) -> str:
    return value if value in {"success", "failure", "cancelled", "timed_out", "local"} else "unknown"


def _run_metrics(comparison: dict[str, Any], side: str) -> dict[str, Any] | None:
    run = _nested(comparison, "runs", side)
    if not isinstance(run, dict) or run.get("available") is not True:
        return None
    metrics = run.get("metrics")
    return metrics if isinstance(metrics, dict) else None


def _memory_table(comparison: dict[str, Any]) -> str:
    base = _run_metrics(comparison, "base")
    head = _run_metrics(comparison, "head")
    delta = comparison.get("delta") if isinstance(comparison.get("delta"), dict) else {}

    def value(metrics: dict[str, Any] | None, key: str) -> Any:
        return metrics.get(key) if metrics is not None else None

    rows = [
        ("Boundary samples", "resource_sample_count", _format_integer, _format_integer),
        ("Boundary RSS samples", "rss_observed_samples", _format_integer, _format_integer),
        ("Boundary PSS samples", "pss_observed_samples", _format_integer, _format_integer),
        ("Periodic peak RSS", "periodic_peak_rss_bytes", _format_mib, _format_signed_mib),
        ("Periodic peak PSS", "periodic_peak_pss_bytes", _format_mib, _format_signed_mib),
        ("RSS · first 10 avg", "rss_first_window_average", _format_mib, _format_signed_mib),
        ("RSS · last 10 avg", "rss_last_window_average", _format_mib, _format_signed_mib),
        ("RSS · last − first window", "rss_first_to_last_window_delta", _format_signed_mib, _format_signed_mib),
        ("RSS · warm slope / 100 nav", "rss_warm_slope_per_100_navigations", _format_signed_mib, _format_signed_mib),
        ("PSS · first 10 avg", "pss_first_window_average", _format_mib, _format_signed_mib),
        ("PSS · last 10 avg", "pss_last_window_average", _format_mib, _format_signed_mib),
        ("PSS · last − first window", "pss_first_to_last_window_delta", _format_signed_mib, _format_signed_mib),
        ("PSS · warm slope / 100 nav", "pss_warm_slope_per_100_navigations", _format_signed_mib, _format_signed_mib),
        ("Peak file descriptors", "periodic_peak_fd_count", _format_integer, _format_integer),
        ("Peak threads", "periodic_peak_thread_count", _format_integer, _format_integer),
    ]
    lines = ["| Metric | Base | HEAD | Δ |", "| --- | ---: | ---: | ---: |"]
    for label, key, formatter, delta_formatter in rows:
        lines.append(
            f"| {label} | {formatter(value(base, key))} | "
            f"{formatter(value(head, key))} | {delta_formatter(delta.get(key))} |"
        )
    return "\n".join(lines)


def _correctness_table(comparison: dict[str, Any]) -> str:
    base = _run_metrics(comparison, "base")
    head = _run_metrics(comparison, "head")

    def value(metrics: dict[str, Any] | None, key: str) -> Any:
        return metrics.get(key) if metrics is not None else None

    return "\n".join(
        [
            "| Metric | Base | HEAD |",
            "| --- | ---: | ---: |",
            (
                "| Attempted / planned | "
                f"{_format_ratio(value(base, 'attempted'), value(base, 'planned'))} | "
                f"{_format_ratio(value(head, 'attempted'), value(head, 'planned'))} |"
            ),
            (
                "| Direct observable pass | "
                f"{_format_ratio(value(base, 'observable_passes'), value(base, 'attempted'))} | "
                f"{_format_ratio(value(head, 'observable_passes'), value(head, 'attempted'))} |"
            ),
            (
                "| Failures / recovered | "
                f"{_format_ratio(value(base, 'failures'), value(base, 'recovery_passes'))} | "
                f"{_format_ratio(value(head, 'failures'), value(head, 'recovery_passes'))} |"
            ),
            (
                "| Recovery failures | "
                f"{_format_integer(value(base, 'recovery_failures'))} | "
                f"{_format_integer(value(head, 'recovery_failures'))} |"
            ),
            (
                "| Lifecycle order violations | "
                f"{_format_integer(value(base, 'order_violations'))} | "
                f"{_format_integer(value(head, 'order_violations'))} |"
            ),
            (
                "| Network order violations | "
                f"{_format_integer(value(base, 'network_order_violations'))} | "
                f"{_format_integer(value(head, 'network_order_violations'))} |"
            ),
            (
                "| Wall time | "
                f"{_format_seconds(value(base, 'duration_seconds'))} | "
                f"{_format_seconds(value(head, 'duration_seconds'))} |"
            ),
        ]
    )


def _head_quarter_table(comparison: dict[str, Any]) -> str:
    head = _run_metrics(comparison, "head")
    quarters = head.get("quarters") if isinstance(head, dict) else None
    if not isinstance(quarters, list) or not quarters:
        return "_HEAD navigation-quarter memory is unavailable._"
    lines = [
        "| HEAD range | Avg RSS | Final RSS | Peak RSS | Avg PSS | Final PSS | Peak FD |",
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: |",
    ]
    rendered_quarters = 0
    for quarter in quarters[:4]:
        if not isinstance(quarter, dict):
            continue
        rendered_quarters += 1
        start = _format_integer(quarter.get("start_index"))
        end = _format_integer(quarter.get("end_index"))
        lines.append(
            "| "
            f"{start}–{end} | "
            f"{_format_mib(quarter.get('rss_average'))} | "
            f"{_format_mib(quarter.get('rss_final'))} | "
            f"{_format_mib(quarter.get('rss_peak'))} | "
            f"{_format_mib(quarter.get('pss_average'))} | "
            f"{_format_mib(quarter.get('pss_final'))} | "
            f"{_format_integer(quarter.get('fd_peak'))} |"
        )
    return (
        "\n".join(lines)
        if rendered_quarters
        else "_HEAD navigation-quarter memory is unavailable._"
    )


def render_comparison_comment(
    comparison: dict[str, Any],
    *,
    run_url: str | None,
    conclusion: str,
) -> str:
    if comparison.get("schema") != COMPARISON_SCHEMA:
        raise ValueError("unsupported sequential navigation comparison schema")
    expected = _integer(comparison.get("expected_navigations"))
    if expected is None or expected <= 0 or expected > 10_000:
        raise ValueError("comparison is missing expected navigation count")
    run_link = _safe_run_url(run_url)
    artifact = (
        f"[workflow run and full `sequential-navigation-soak-results` artifact]({run_link})"
        if run_link
        else "local output or `sequential-navigation-soak-results` artifact"
    )
    issues = comparison_health_issues(comparison)
    assessment = (
        f"✅ HEAD completed the {expected}-navigation resilience and memory observation."
        if not issues
        else "⚠️ HEAD did not produce a complete resilience/memory observation: "
        + "; ".join(issues[:6])
        + "."
    )
    order = comparison.get("execution_order")
    rendered_order = order if order in {"base-first", "head-first"} else "unknown"
    return "\n".join(
        [
            COMMENT_MARKER,
            "## Sequential Navigation Soak A/B",
            "",
            assessment,
            "",
            "One browser process, one target, and one CDP session navigate "
            "CSDN → SegmentFault → Huaban → example.com repeatedly. "
            f"This run issued {expected} `Page.navigate` commands per binary.",
            "",
            (
                f"Common ancestor `{_short_sha(_nested(comparison, 'base', 'sha'))}` → "
                f"HEAD `{_short_sha(_nested(comparison, 'head', 'sha'))}`; "
                f"benchmark order: `{rendered_order}`; "
                f"workflow: `{_safe_conclusion(conclusion)}`."
            ),
            "",
            "### Session resilience",
            "",
            _correctness_table(comparison),
            "",
            "A public-page failure is reported separately from an unrecoverable session. "
            "The soak requires all navigation attempts, zero failed recovery, zero "
            "lifecycle/network ordering violations, and complete resource evidence.",
            "",
            "### Process-tree memory",
            "",
            _memory_table(comparison),
            "",
            "### HEAD memory by 50-navigation quarter",
            "",
            _head_quarter_table(comparison),
            "",
            f"Raw reports and per-navigation boundary samples: {artifact}.",
            "",
            "_Public-site timing and memory are observational. Use the A/B deltas and "
            "trend shape as evidence, not as a deterministic performance threshold._",
        ]
    )


def render_infrastructure_comment(*, run_url: str | None, conclusion: str) -> str:
    run_link = _safe_run_url(run_url)
    workflow = f"[workflow run]({run_link})" if run_link else "workflow run"
    return "\n".join(
        [
            COMMENT_MARKER,
            "## Sequential Navigation Soak A/B",
            "",
            "⚠️ Benchmark infrastructure failed before a comparison could be produced.",
            "",
            f"Workflow: `{_safe_conclusion(conclusion)}`. Inspect the {workflow} for the "
            "failing build, harness, or artifact step.",
            "",
            "_No navigation resilience or memory conclusion is available for this run._",
        ]
    )


def _write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _write_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text.rstrip() + "\n", encoding="utf-8")


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Compare and render sequential navigation soak evidence.")
    commands = parser.add_subparsers(dest="command", required=True)

    compare = commands.add_parser("compare")
    compare.add_argument("--base-report", type=Path, required=True)
    compare.add_argument("--head-report", type=Path, required=True)
    compare.add_argument("--base-exit-code", type=int, required=True)
    compare.add_argument("--head-exit-code", type=int, required=True)
    compare.add_argument("--base-sha", required=True)
    compare.add_argument("--head-sha", required=True)
    compare.add_argument("--execution-order", choices=("base-first", "head-first"), required=True)
    compare.add_argument("--expected-navigations", type=int, default=200)
    compare.add_argument("--output", type=Path, required=True)
    compare.add_argument("--comment-output", type=Path, required=True)
    compare.add_argument("--run-url")
    compare.add_argument("--conclusion", default="local")

    comment = commands.add_parser("comment")
    comment.add_argument("--input", type=Path, required=True)
    comment.add_argument("--output", type=Path, required=True)
    comment.add_argument("--run-url")
    comment.add_argument("--conclusion", required=True)

    infrastructure = commands.add_parser("infrastructure-comment")
    infrastructure.add_argument("--output", type=Path, required=True)
    infrastructure.add_argument("--run-url")
    infrastructure.add_argument("--conclusion", required=True)

    check = commands.add_parser("check")
    check.add_argument("--input", type=Path, required=True)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    if args.command == "compare":
        comparison = build_comparison(
            base_sha=args.base_sha,
            head_sha=args.head_sha,
            execution_order=args.execution_order,
            expected_navigations=args.expected_navigations,
            base_run=read_sequential_navigation_report(
                args.base_report, args.base_exit_code
            ),
            head_run=read_sequential_navigation_report(
                args.head_report, args.head_exit_code
            ),
        )
        _write_json(args.output, comparison)
        _write_text(
            args.comment_output,
            render_comparison_comment(
                comparison,
                run_url=args.run_url,
                conclusion=args.conclusion,
            ),
        )
        return 0
    if args.command == "comment":
        comparison = json.loads(args.input.read_text(encoding="utf-8"))
        _write_text(
            args.output,
            render_comparison_comment(
                comparison,
                run_url=args.run_url,
                conclusion=args.conclusion,
            ),
        )
        return 0
    if args.command == "infrastructure-comment":
        _write_text(
            args.output,
            render_infrastructure_comment(
                run_url=args.run_url,
                conclusion=args.conclusion,
            ),
        )
        return 0
    if args.command == "check":
        comparison = json.loads(args.input.read_text(encoding="utf-8"))
        issues = comparison_health_issues(comparison)
        print(json.dumps({"healthy": not issues, "issues": issues}, indent=2))
        return 1 if issues else 0
    raise AssertionError(f"unhandled command: {args.command}")


if __name__ == "__main__":
    raise SystemExit(main())
