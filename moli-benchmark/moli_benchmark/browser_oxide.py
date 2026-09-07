from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import re
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Sequence

from .browser_oxide_scoring import classify, pass_flags
from .config import PROJECT_ROOT, moli_binary
from .versions import sha256_file


DEFAULT_CORPUS = PROJECT_ROOT / "fixtures" / "browser-oxide" / "corpus.json"
REPORT_SCHEMA_VERSION = 1
POST_LOAD_DELAY_MS = 8000


def _utc_timestamp() -> str:
    return dt.datetime.now(dt.UTC).isoformat()


def _read_json(path: Path) -> tuple[Any, bytes]:
    raw = path.read_bytes()
    try:
        return json.loads(raw), raw
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"invalid JSON in {path}: {error}") from error


def _write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def _require_string(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} must be a non-empty string")
    return value


def _load_corpus(path: Path) -> tuple[dict[str, Any], list[dict[str, str]], str]:
    payload, raw = _read_json(path)
    raw_sites = payload
    if not isinstance(raw_sites, list) or not raw_sites:
        raise ValueError(f"corpus {path} must be a non-empty JSON array")

    sites: list[dict[str, str]] = []
    seen_urls: set[str] = set()
    for index, raw_site in enumerate(raw_sites):
        if not isinstance(raw_site, dict):
            raise ValueError(f"corpus site {index} must be an object")
        site = {
            key: _require_string(raw_site.get(key), f"corpus site {index}.{key}")
            for key in ("cat", "name", "url")
        }
        if site["url"] in seen_urls:
            raise ValueError(f"corpus contains duplicate URL: {site['url']}")
        seen_urls.add(site["url"])
        sites.append(site)

    identity = {
        "path": str(path.resolve()),
        "sha256": hashlib.sha256(raw).hexdigest(),
        "site_count": len(sites),
    }
    return identity, sites, hashlib.sha256(raw).hexdigest()


def _select_sites(sites: list[dict[str, str]], selectors: list[str] | None) -> list[dict[str, str]]:
    if not selectors:
        return sites
    requested = set(selectors)
    selected = [site for site in sites if site["name"] in requested or site["url"] in requested]
    matched = {value for value in requested if any(value in (site["name"], site["url"]) for site in selected)}
    missing = sorted(requested - matched)
    if missing:
        raise ValueError(f"unknown --site selector(s): {', '.join(missing)}")
    return selected


def _artifact_stem(index: int, name: str) -> str:
    safe = re.sub(r"[^A-Za-z0-9._-]+", "-", name).strip(".-") or "site"
    return f"{index:03d}-{safe[:80]}"


def _failure_score() -> dict[str, Any]:
    return {"tag": "ERROR", "len": 0, "strict_pass": False, "loose_pass": False}


def _run_site(moli: Path, site: dict[str, str], timeout: float, html_path: Path, stderr_path: Path) -> dict[str, Any]:
    command = [
        str(moli),
        "fetch",
        "--stealth",
        "chrome",
        "--dump",
        "html",
        "--wait-until",
        "done",
        "--delay-ms",
        str(POST_LOAD_DELAY_MS),
        "--timeout",
        str(max(1, round(timeout * 1000))),
        "--",
        site["url"],
    ]
    started = time.perf_counter()
    stdout = b""
    stderr = b""
    err: str | None = None
    score: dict[str, Any]
    try:
        completed = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=timeout,
            check=False,
        )
        stdout = completed.stdout or b""
        stderr = completed.stderr or b""
        if completed.returncode != 0:
            err = f"moli exited with status {completed.returncode}"
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout or b""
        stderr = error.stderr or b""
        err = f"timed out after {timeout:g}s"
    except OSError as error:
        err = f"failed to launch moli: {error}"
        stderr = str(error).encode("utf-8", errors="replace")

    elapsed = time.perf_counter() - started
    if err is None and elapsed >= timeout:
        err = f"timed out after {timeout:g}s"
    if err is not None:
        score = _failure_score()
    else:
        try:
            body = stdout.decode("utf-8")
        except UnicodeDecodeError as error:
            err = f"moli stdout is not valid UTF-8 HTML: {error}"
            score = _failure_score()
        else:
            score = classify(body)

    html_path.write_bytes(stdout)
    stderr_path.write_bytes(stderr)
    return {
        **site,
        **score,
        "ms": round(elapsed * 1000),
        "err": err,
        "html_artifact": str(html_path),
        "stderr_artifact": str(stderr_path),
    }


def _run(args: argparse.Namespace) -> int:
    corpus_path = args.corpus.resolve()
    corpus, sites, corpus_sha256 = _load_corpus(corpus_path)
    sites = _select_sites(sites, args.site)
    output_dir = args.output_dir.resolve()
    artifacts_dir = output_dir / "artifacts"
    artifacts_dir.mkdir(parents=True, exist_ok=True)
    moli = moli_binary(args.moli_bin)
    timestamp = _utc_timestamp()
    report_path = output_dir / "report.json"
    checkpoint_path = output_dir / "results.jsonl"
    checkpoint_path.write_text("", encoding="utf-8")

    report: dict[str, Any] = {
        "schema_version": REPORT_SCHEMA_VERSION,
        "engine": "moli",
        "profile": "chrome",
        "timestamp": timestamp,
        "complete": False,
        "moli_bin": str(moli),
        "moli_sha256": sha256_file(moli),
        "timeout_seconds": args.timeout,
        "wait_until": "done",
        "post_load_delay_ms": POST_LOAD_DELAY_MS,
        "corpus": {**corpus, "sha256": corpus_sha256, "selected_site_count": len(sites)},
        "results": [],
    }
    _write_json_atomic(report_path, report)
    for index, site in enumerate(sites, 1):
        stem = _artifact_stem(index, site["name"])
        row = _run_site(
            moli,
            site,
            args.timeout,
            artifacts_dir / f"{stem}.html",
            artifacts_dir / f"{stem}.stderr",
        )
        report["results"].append(row)
        with checkpoint_path.open("a", encoding="utf-8") as checkpoint:
            checkpoint.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")
        _write_json_atomic(report_path, report)
        print(
            f"[{index}/{len(sites)}] {site['name']}: {row['tag']} len={row['len']} ms={row['ms']}"
            + (f" err={row['err']}" if row["err"] else ""),
            file=sys.stderr,
            flush=True,
        )

    report["complete"] = True
    report["finished_at"] = _utc_timestamp()
    _write_json_atomic(report_path, report)
    print(str(report_path))
    return 0


def _load_moli_report(path: Path) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    payload, _ = _read_json(path)
    if not isinstance(payload, dict) or payload.get("engine") != "moli":
        raise ValueError(f"{path} is not a Moli BrowserOxide report")
    if payload.get("profile") != "chrome":
        raise ValueError(f"{path} has unsupported Moli profile {payload.get('profile')!r}; expected 'chrome'")
    if payload.get("complete") is not True:
        raise ValueError(f"{path} is incomplete")
    indexed = _index_results(path, payload.get("results"))
    corpus = payload.get("corpus")
    if not isinstance(corpus, dict) or corpus.get("selected_site_count") != len(indexed):
        raise ValueError(f"{path} corpus.selected_site_count does not match its result count")
    for index, row in enumerate(indexed.values()):
        _require_string(row.get("tag"), f"{path} result {index}.tag")
        if not isinstance(row.get("len"), int) or row["len"] < 0:
            raise ValueError(f"{path} result {index}.len must be a non-negative integer")
        if "err" not in row or (row["err"] is not None and not isinstance(row["err"], str)):
            raise ValueError(f"{path} result {index}.err must be null or a string")
    return payload, indexed


def _index_results(path: Path, raw_results: Any) -> dict[str, dict[str, Any]]:
    if not isinstance(raw_results, list):
        raise ValueError(f"{path} results must be a list")
    indexed: dict[str, dict[str, Any]] = {}
    for index, row in enumerate(raw_results):
        if not isinstance(row, dict):
            raise ValueError(f"{path} result {index} must be an object")
        url = _require_string(row.get("url"), f"{path} result {index}.url")
        if url in indexed:
            raise ValueError(f"{path} contains duplicate result URL: {url}")
        indexed[url] = row
    return indexed


def _load_upstream_report(path: Path) -> tuple[dict[str, Any], dict[str, dict[str, Any]]]:
    payload, _ = _read_json(path)
    if not isinstance(payload, dict) or not isinstance(payload.get("summary"), dict):
        raise ValueError(f"{path} must have the BrowserOxide sweep_metrics summary/results shape")
    summary = payload["summary"]
    if summary.get("engine") != "browser_oxide":
        raise ValueError(f"{path} summary.engine must be 'browser_oxide'")
    _require_string(summary.get("profile"), f"{path} summary.profile")
    indexed = _index_results(path, payload.get("results"))
    if summary.get("n") != len(indexed):
        raise ValueError(f"{path} summary.n does not match its result count")
    for index, row in enumerate(indexed.values()):
        _require_string(row.get("cat"), f"{path} result {index}.cat")
        _require_string(row.get("name"), f"{path} result {index}.name")
        _require_string(row.get("tag"), f"{path} result {index}.tag")
        if not isinstance(row.get("len"), int) or row["len"] < 0:
            raise ValueError(f"{path} result {index}.len must be a non-negative integer")
        if "err" not in row or (row["err"] is not None and not isinstance(row["err"], str)):
            raise ValueError(f"{path} result {index}.err must be null or a string")
    return summary, indexed


def _row_passes(row: dict[str, Any]) -> tuple[bool, bool]:
    if row.get("err") is not None:
        return False, False
    tag = row.get("tag")
    length = row.get("len")
    if not isinstance(tag, str) or not isinstance(length, int) or length < 0:
        return False, False
    return pass_flags(tag, length)


def _coverage_error(reference: set[str], candidate: set[str], path: Path) -> str | None:
    missing = sorted(reference - candidate)
    extra = sorted(candidate - reference)
    if not missing and not extra:
        return None
    pieces = []
    if missing:
        pieces.append(f"missing {len(missing)} URL(s): {', '.join(missing[:5])}")
    if extra:
        pieces.append(f"extra {len(extra)} URL(s): {', '.join(extra[:5])}")
    return f"{path} has incompatible coverage ({'; '.join(pieces)})"


def _compare(args: argparse.Namespace) -> int:
    moli_payload, moli_results = _load_moli_report(args.moli_report.resolve())
    expected_urls = set(moli_results)
    if not expected_urls:
        raise ValueError("Moli report has no results")

    baselines: list[tuple[Path, dict[str, Any], dict[str, dict[str, Any]]]] = []
    seen_profiles: set[str] = set()
    for raw_path in args.browser_oxide_report:
        path = raw_path.resolve()
        summary, results = _load_upstream_report(path)
        coverage_error = _coverage_error(expected_urls, set(results), path)
        if coverage_error:
            raise ValueError(coverage_error)
        profile = summary["profile"]
        if profile in seen_profiles:
            raise ValueError(f"duplicate BrowserOxide profile: {profile}")
        seen_profiles.add(profile)
        baselines.append((path, summary, results))

    profile_rows: list[dict[str, Any]] = []
    routed_strict: set[str] = set()
    routed_loose: set[str] = set()
    strict_profiles_by_url: dict[str, list[str]] = {url: [] for url in expected_urls}
    for path, summary, results in baselines:
        strict_urls: set[str] = set()
        loose_urls: set[str] = set()
        for url, row in results.items():
            strict, loose = _row_passes(row)
            if strict:
                strict_urls.add(url)
                strict_profiles_by_url[url].append(summary["profile"])
            if loose:
                loose_urls.add(url)
        routed_strict.update(strict_urls)
        routed_loose.update(loose_urls)
        profile_rows.append(
            {
                "profile": summary["profile"],
                "mode": summary.get("mode"),
                "report": str(path),
                "n": len(results),
                "strict_pass": len(strict_urls),
                "loose_pass": len(loose_urls),
            }
        )

    moli_strict = {url for url, row in moli_results.items() if _row_passes(row)[0]}
    moli_loose = {url for url, row in moli_results.items() if _row_passes(row)[1]}
    differential = []
    for url in sorted(routed_strict - moli_strict):
        moli_row = moli_results[url]
        differential.append(
            {
                "cat": moli_row.get("cat"),
                "name": moli_row.get("name"),
                "url": url,
                "upstream_passing_profiles": strict_profiles_by_url[url],
                "moli": {
                    "tag": moli_row.get("tag"),
                    "len": moli_row.get("len"),
                    "err": moli_row.get("err"),
                },
            }
        )

    comparison = {
        "schema_version": REPORT_SCHEMA_VERSION,
        "timestamp": _utc_timestamp(),
        "moli_report": str(args.moli_report.resolve()),
        "moli_profile": "chrome",
        "corpus": moli_payload.get("corpus"),
        "n": len(expected_urls),
        "profiles": profile_rows,
        "four_profile_table": profile_rows if len(profile_rows) == 4 else None,
        "moli": {"strict_pass": len(moli_strict), "loose_pass": len(moli_loose)},
        "routed_union": {"strict_pass": len(routed_strict), "loose_pass": len(routed_loose)},
        "upstream_pass_moli_fail": differential,
    }
    if args.output is not None:
        _write_json_atomic(args.output.resolve(), comparison)
    print(json.dumps(comparison, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m moli_benchmark.browser_oxide",
        description="Run and compare the pinned BrowserOxide screenshot corpus.",
    )
    commands = parser.add_subparsers(dest="command", required=True)

    run = commands.add_parser("run", help="run Moli's chrome profile against the corpus")
    run.add_argument("--moli-bin")
    run.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    run.add_argument("--output-dir", type=Path, required=True)
    run.add_argument("--site", action="append", help="site name or URL; repeat to select a subset")
    run.add_argument("--timeout", type=float, default=240.0)
    run.set_defaults(handler=_run)

    compare = commands.add_parser("compare", help="compare a complete Moli report with actual BrowserOxide sweeps")
    compare.add_argument("moli_report", type=Path)
    compare.add_argument(
        "--browser-oxide-report",
        type=Path,
        action="append",
        required=True,
        help="sweep_metrics JSON report; repeat once per upstream profile",
    )
    compare.add_argument("--output", type=Path)
    compare.set_defaults(handler=_compare)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    try:
        if getattr(args, "timeout", 1) <= 0:
            raise ValueError("--timeout must be positive")
        return int(args.handler(args))
    except (OSError, ValueError, TypeError, KeyError) as error:
        print(f"browser-oxide: error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
