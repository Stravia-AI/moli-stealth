from __future__ import annotations

import json
import subprocess
import unittest
from contextlib import redirect_stderr, redirect_stdout
from io import StringIO
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from moli_benchmark.browser_oxide import main


def _write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value), encoding="utf-8")


def _moli_report(urls: list[str]) -> dict:
    return {
        "engine": "moli",
        "profile": "chrome",
        "complete": True,
        "corpus": {"sha256": "abc", "selected_site_count": len(urls)},
        "results": [
            {
                "cat": "web",
                "name": f"site-{index}",
                "url": url,
                "tag": "L3-RENDERED",
                "len": 16000,
                "strict_pass": True,
                "loose_pass": True,
                "ms": 1,
                "err": None,
            }
            for index, url in enumerate(urls)
        ],
    }


def _upstream_report(urls: list[str], profile: str = "chrome_148_windows") -> dict:
    return {
        "summary": {
            "engine": "browser_oxide",
            "profile": profile,
            "mode": "cold",
            "n": len(urls),
        },
        "results": [
            {
                "cat": "web",
                "name": f"site-{index}",
                "url": url,
                "tag": "L3-RENDERED",
                "len": 16000,
                "ms": 1,
                "rss_mb": 1.0,
                "err": None,
            }
            for index, url in enumerate(urls)
        ],
    }


class BrowserOxideRunnerTests(unittest.TestCase):
    def test_nonzero_fetch_is_never_classified_as_pass(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            corpus = root / "corpus.json"
            output = root / "output"
            (root / "moli").write_bytes(b"test executable")
            _write_json(
                corpus,
                [{"cat": "web", "name": "example", "url": "https://example.test/"}],
            )
            rendered = b"<html>" + b"x" * 20000 + b"</html>"
            completed = subprocess.CompletedProcess([], 7, rendered, b"fetch failed")
            with (
                patch("moli_benchmark.browser_oxide.moli_binary", return_value=root / "moli"),
                patch("moli_benchmark.browser_oxide.subprocess.run", return_value=completed),
                redirect_stdout(StringIO()),
                redirect_stderr(StringIO()),
            ):
                exit_code = main(
                    ["run", "--corpus", str(corpus), "--output-dir", str(output)]
                )

            self.assertEqual(exit_code, 0)
            report = json.loads((output / "report.json").read_text(encoding="utf-8"))
            row = report["results"][0]
            self.assertEqual(row["tag"], "ERROR")
            self.assertFalse(row["strict_pass"])
            self.assertFalse(row["loose_pass"])

    def test_timeout_is_never_classified_as_pass(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            corpus = root / "corpus.json"
            output = root / "output"
            (root / "moli").write_bytes(b"test executable")
            _write_json(
                corpus,
                [{"cat": "web", "name": "slow", "url": "https://slow.test/"}],
            )
            timeout = subprocess.TimeoutExpired(
                ["moli"], 2, output=b"<html>" + b"x" * 20000 + b"</html>", stderr=b"partial error"
            )
            with (
                patch("moli_benchmark.browser_oxide.moli_binary", return_value=root / "moli"),
                patch("moli_benchmark.browser_oxide.subprocess.run", side_effect=timeout),
                redirect_stdout(StringIO()),
                redirect_stderr(StringIO()),
            ):
                exit_code = main(
                    [
                        "run",
                        "--corpus",
                        str(corpus),
                        "--output-dir",
                        str(output),
                        "--timeout",
                        "2",
                    ]
                )

            self.assertEqual(exit_code, 0)
            row = json.loads((output / "report.json").read_text(encoding="utf-8"))["results"][0]
            self.assertEqual(row["tag"], "ERROR")
            self.assertFalse(row["strict_pass"])
            self.assertFalse(row["loose_pass"])

    def test_successful_exit_after_budget_is_not_a_pass(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            corpus = root / "corpus.json"
            output = root / "output"
            (root / "moli").write_bytes(b"test executable")
            _write_json(
                corpus,
                [{"cat": "web", "name": "late", "url": "https://late.test/"}],
            )
            completed = subprocess.CompletedProcess(
                [], 0, b"<html>" + b"x" * 20000 + b"</html>", b""
            )
            with (
                patch("moli_benchmark.browser_oxide.moli_binary", return_value=root / "moli"),
                patch("moli_benchmark.browser_oxide.subprocess.run", return_value=completed),
                patch("moli_benchmark.browser_oxide.time.perf_counter", side_effect=[0, 2.01]),
                redirect_stdout(StringIO()),
                redirect_stderr(StringIO()),
            ):
                exit_code = main(
                    ["run", "--corpus", str(corpus), "--output-dir", str(output), "--timeout", "2"]
                )

            self.assertEqual(exit_code, 0)
            row = json.loads((output / "report.json").read_text(encoding="utf-8"))["results"][0]
            self.assertEqual(row["tag"], "ERROR")
            self.assertFalse(row["strict_pass"])
            self.assertFalse(row["loose_pass"])

    def test_compare_rejects_missing_or_extra_baseline_coverage(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            moli_report = root / "moli.json"
            upstream_report = root / "upstream.json"
            _write_json(moli_report, _moli_report(["https://a.test/", "https://b.test/"]))
            _write_json(upstream_report, _upstream_report(["https://a.test/", "https://c.test/"]))

            stderr = StringIO()
            with redirect_stdout(StringIO()), redirect_stderr(stderr):
                exit_code = main(
                    [
                        "compare",
                        str(moli_report),
                        "--browser-oxide-report",
                        str(upstream_report),
                    ]
                )

            self.assertEqual(exit_code, 2)
            self.assertIn("incompatible coverage", stderr.getvalue())
            self.assertIn("missing 1 URL", stderr.getvalue())
            self.assertIn("extra 1 URL", stderr.getvalue())

    def test_compare_routes_per_url_without_counting_shells_or_failed_output(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            urls = ["https://a.test/", "https://b.test/", "https://c.test/"]
            moli = _moli_report(urls)
            moli["results"][1]["len"] = 14999
            moli["results"][2]["err"] = "navigation failed"
            chrome = _upstream_report(urls, "chrome_148_macos")
            chrome["results"][2]["len"] = 14999
            firefox = _upstream_report(urls, "firefox_135_macos")
            firefox["results"][1]["len"] = 14999
            for name, payload in (("moli", moli), ("chrome", chrome), ("firefox", firefox)):
                _write_json(root / f"{name}.json", payload)
            stdout = StringIO()
            with redirect_stdout(stdout), redirect_stderr(StringIO()):
                exit_code = main([
                    "compare", str(root / "moli.json"),
                    "--browser-oxide-report", str(root / "chrome.json"),
                    "--browser-oxide-report", str(root / "firefox.json"),
                ])
            self.assertEqual(exit_code, 0)
            comparison = json.loads(stdout.getvalue())
            self.assertEqual(comparison["moli"], {"strict_pass": 1, "loose_pass": 2})
            self.assertEqual(comparison["routed_union"], {"strict_pass": 3, "loose_pass": 3})
            differences = {
                row["url"]: row["upstream_passing_profiles"]
                for row in comparison["upstream_pass_moli_fail"]
            }
            self.assertEqual(differences, {
                urls[1]: ["chrome_148_macos"],
                urls[2]: ["firefox_135_macos"],
            })

    def test_compare_rejects_malformed_shape_and_duplicate_results(self) -> None:
        with TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            moli_report = root / "moli.json"
            malformed = root / "malformed.json"
            duplicate = root / "duplicate.json"
            _write_json(moli_report, _moli_report(["https://a.test/"]))
            _write_json(malformed, {"results": []})
            duplicate_payload = _upstream_report(["https://a.test/"])
            duplicate_payload["summary"]["n"] = 2
            duplicate_payload["results"].append(dict(duplicate_payload["results"][0]))
            _write_json(duplicate, duplicate_payload)

            for path, message in (
                (malformed, "summary/results shape"),
                (duplicate, "duplicate result URL"),
            ):
                with self.subTest(path=path.name):
                    stderr = StringIO()
                    with redirect_stdout(StringIO()), redirect_stderr(stderr):
                        exit_code = main(
                            [
                                "compare",
                                str(moli_report),
                                "--browser-oxide-report",
                                str(path),
                            ]
                        )
                    self.assertEqual(exit_code, 2)
                    self.assertIn(message, stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
