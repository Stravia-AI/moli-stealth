from __future__ import annotations

import unittest
from pathlib import Path

from moli_benchmark.synthetic_compare import (
    CDP_TARGETS,
    FETCH_TARGETS,
    TARGETS,
    WEBFETCH_TARGETS,
    _command_for_target,
    normalize_cdp_target,
    run_synthetic_compare_suite,
    target_metadata,
    target_uses_external_fixture,
)


class SyntheticCompareTargetTests(unittest.TestCase):
    def test_obscura_is_a_default_horizontal_target(self) -> None:
        self.assertIn("obscura", FETCH_TARGETS)
        self.assertIn("obscura-cdp", CDP_TARGETS)
        self.assertIn("obscura-cdp", WEBFETCH_TARGETS)
        self.assertIn("obscura", TARGETS)
        self.assertIn("moli-full", FETCH_TARGETS)
        self.assertIn("moli-full-cdp", CDP_TARGETS)
        self.assertIn("moli-full", TARGETS)

    def test_obscura_timeout_truncates_fractional_seconds(self) -> None:
        command = _command_for_target("obscura", Path("/bin/obscura"), "http://127.0.0.1:1/static-html", 4.8)

        self.assertEqual(command[command.index("--timeout") + 1], "4")

    def test_lightpanda_command_aligns_fetch_timeouts(self) -> None:
        command = _command_for_target("lightpanda", Path("/bin/lightpanda"), "http://127.0.0.1:1/static-html", 30.0)

        self.assertEqual(command[command.index("--wait-ms") + 1], "30000")
        self.assertEqual(command[command.index("--http-timeout") + 1], "30000")
        self.assertEqual(command[command.index("--terminate-ms") + 1], "30000")

    def test_moli_full_enables_layout_and_resources_only_for_full_target(self) -> None:
        default = _command_for_target(
            "moli",
            Path("/bin/moli"),
            "http://127.0.0.1:1/static-html",
            30.0,
        )
        full = _command_for_target(
            "moli-full",
            Path("/bin/moli"),
            "http://127.0.0.1:1/static-html",
            30.0,
        )

        self.assertNotIn("--layout", default)
        self.assertNotIn("--resource", default)
        self.assertIn("--layout", full)
        self.assertIn("--resource", full)

    def test_target_metadata_splits_engine_and_driver(self) -> None:
        self.assertEqual(target_metadata("moli")["label"], "moli / fetch")
        self.assertEqual(target_metadata("moli-cdp")["label"], "moli / cdp")
        self.assertEqual(target_metadata("moli-full")["label"], "moli full / fetch")
        self.assertEqual(target_metadata("moli-full-cdp")["binary_key"], "moli")
        self.assertEqual(normalize_cdp_target("moli-full"), "moli-full-cdp")
        self.assertEqual(target_metadata("chrome")["driver"], "dump-dom")
        self.assertEqual(target_metadata("chrome-cdp")["binary_key"], "chrome")
        self.assertEqual(normalize_cdp_target("obscura"), "obscura-cdp")

    def test_fetch_command_rejects_cdp_variant(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "CDP target"):
            _command_for_target("moli-cdp", Path("/bin/moli"), "http://127.0.0.1:1/static-html", 1.0)
        with self.assertRaisesRegex(RuntimeError, "CDP target"):
            _command_for_target(
                "moli-full-cdp",
                Path("/bin/moli"),
                "http://127.0.0.1:1/static-html",
                1.0,
            )

    def test_synthetic_compare_rejects_unmeasured_gate_target(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "must be included"):
            run_synthetic_compare_suite(
                output_dir=Path("/tmp/unused"),
                target_matrix={},
                targets=("lightpanda",),
                runs=1,
                timeout_seconds=1.0,
                cases=("static-html",),
                concurrency=1,
                gate_target="moli",
            )

    def test_obscura_uses_external_fixture_url(self) -> None:
        self.assertTrue(target_uses_external_fixture("obscura"))
        self.assertTrue(target_uses_external_fixture("obscura-cdp"))
        self.assertFalse(target_uses_external_fixture("moli"))


if __name__ == "__main__":
    unittest.main()
