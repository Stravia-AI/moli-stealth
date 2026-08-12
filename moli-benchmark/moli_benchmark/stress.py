from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path
from typing import Sequence

from .config import PROJECT_ROOT
from .sequential_navigate import main as sequential_navigate_main
from .stress_report import write_stress_report


DEFAULT_NAVIGATIONS = 600
DEFAULT_URLS = (
    "https://www.csdn.net/",
    "https://segmentfault.com/",
    "https://huaban.com/",
    "https://example.com/",
)


def _default_output_dir() -> Path:
    timestamp = dt.datetime.now(dt.UTC).strftime("%Y-%m-%dT%H%M%SZ")
    return PROJECT_ROOT / "results" / f"stress-{timestamp}"


def _run(args: argparse.Namespace) -> int:
    urls = tuple(args.url or DEFAULT_URLS)
    if args.navigations < 4:
        raise ValueError("--navigations must be at least 4")
    rounds, remainder = divmod(args.navigations, len(urls))
    if remainder:
        raise ValueError(
            f"--navigations must be divisible by the {len(urls)} selected URLs; "
            f"got {args.navigations}"
        )

    output_dir = args.output_dir.resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    result_path = output_dir / "result.json"
    report_path = output_dir / "report.html"
    sequential_args = [
        "--engine",
        "moli",
        "--rounds",
        str(rounds),
        "--navigation-resource-samples",
        "--periodic-resource-samples",
        "--startup-timeout",
        str(args.startup_timeout),
        "--response-timeout",
        str(args.response_timeout),
        "--dcl-timeout",
        str(args.dcl_timeout),
        "--load-timeout",
        str(args.load_timeout),
        "--postcheck-timeout",
        str(args.postcheck_timeout),
        "--recovery-timeout",
        str(args.recovery_timeout),
        "--output",
        str(result_path),
    ]
    for url in urls:
        sequential_args.extend(("--url", url))
    if args.moli_bin:
        sequential_args.extend(("--moli-bin", args.moli_bin))
    if args.full_resources:
        sequential_args.append("--full-resources")
    if args.network_diagnostics:
        sequential_args.append("--network-diagnostics")

    exit_code = sequential_navigate_main(sequential_args)
    summary = write_stress_report(result_path, report_path)
    print(
        json.dumps(
            {
                "exit_code": exit_code,
                "status": summary["status"],
                "result": str(result_path),
                "report": str(report_path),
                "summary": str(output_dir / "summary.json"),
            },
            ensure_ascii=False,
        ),
        flush=True,
    )
    return exit_code


def _report(args: argparse.Namespace) -> int:
    result_path = args.result.resolve()
    output_path = (
        args.output.resolve()
        if args.output is not None
        else result_path.with_name("report.html")
    )
    summary = write_stress_report(result_path, output_path)
    print(
        json.dumps(
            {
                "status": summary["status"],
                "report": str(output_path),
                "summary": str(output_path.parent / "summary.json"),
            },
            ensure_ascii=False,
        ),
        flush=True,
    )
    return 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="moli-stress",
        description=(
            "Run long-lived sequential CDP navigation stress tests and generate "
            "offline RSS/PSS/CPU reports."
        ),
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    run = subparsers.add_parser(
        "run",
        help="run a stress test and generate result.json, summary.json, and report.html",
    )
    run.add_argument("--navigations", type=int, default=DEFAULT_NAVIGATIONS)
    run.add_argument(
        "--url",
        action="append",
        help="URL to navigate; repeat to replace the four-site default sequence",
    )
    run.add_argument("--output-dir", type=Path, default=_default_output_dir())
    run.add_argument("--moli-bin")
    run.add_argument("--full-resources", action="store_true")
    run.add_argument(
        "--network-diagnostics",
        action=argparse.BooleanOptionalAction,
        default=True,
    )
    run.add_argument("--startup-timeout", type=float, default=20.0)
    run.add_argument("--response-timeout", type=float, default=15.0)
    run.add_argument("--dcl-timeout", type=float, default=14.0)
    run.add_argument("--load-timeout", type=float, default=20.0)
    run.add_argument("--postcheck-timeout", type=float, default=5.0)
    run.add_argument("--recovery-timeout", type=float, default=8.0)
    run.set_defaults(handler=_run)

    report = subparsers.add_parser(
        "report",
        help="regenerate an offline HTML report from a retained stress result",
    )
    report.add_argument("result", type=Path)
    report.add_argument("--output", type=Path)
    report.set_defaults(handler=_report)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    try:
        return int(args.handler(args))
    except (OSError, RuntimeError, ValueError, KeyError, TypeError) as error:
        print(f"moli-stress: error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
