from __future__ import annotations

import asyncio
import json
import os
import signal
import sys
import unittest

from moli_cdp_smoke.assertions import SmokeError
from moli_cdp_smoke.groups.external_process import run_external_json_process
from moli_cdp_smoke.process import terminate_process_tree


class ExternalProcessTests(unittest.IsolatedAsyncioTestCase):
    async def test_successful_json_process_extends_results(self) -> None:
        expected = {"name": "external-process-probe", "ok": True}
        payload = json.dumps({"ok": True, "results": [expected]})
        results: list[dict[str, object]] = []

        await run_external_json_process(
            "test",
            [sys.executable, "-c", f"print({payload!r})"],
            results,
            timeout_seconds=5,
        )

        self.assertEqual(results, [expected])

    @unittest.skipUnless(os.name == "posix", "process-group regression is POSIX-specific")
    async def test_timeout_kills_descendants_holding_output_pipes(self) -> None:
        descendant = "import time; time.sleep(4)"
        parent = (
            "import subprocess, sys, time; "
            f"subprocess.Popen([sys.executable, '-c', {descendant!r}], "
            "stdout=sys.stdout, stderr=sys.stderr); "
            "print('descendant-started', flush=True); "
            "time.sleep(30)"
        )
        loop = asyncio.get_running_loop()
        started_at = loop.time()

        with self.assertRaisesRegex(SmokeError, "test smoke timed out") as raised:
            await run_external_json_process(
                "test",
                [sys.executable, "-c", parent],
                [],
                timeout_seconds=1,
            )

        elapsed = loop.time() - started_at
        self.assertIn("descendant-started", str(raised.exception))
        self.assertLess(
            elapsed,
            2.5,
            "timeout cleanup waited for the descendant to close inherited pipes",
        )

    @unittest.skipUnless(os.name == "posix", "signal regression is POSIX-specific")
    async def test_service_cleanup_escalates_to_sigkill_with_a_bound(self) -> None:
        process = await asyncio.create_subprocess_exec(
            sys.executable,
            "-c",
            (
                "import signal, time; "
                "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                "print('ready', flush=True); "
                "time.sleep(30)"
            ),
            stdout=asyncio.subprocess.PIPE,
            start_new_session=True,
        )
        self.assertIsNotNone(process.stdout)
        self.assertEqual(await process.stdout.readline(), b"ready\n")

        stopped = await terminate_process_tree(
            process,
            terminate_timeout_seconds=0.05,
            kill_timeout_seconds=1,
        )

        self.assertTrue(stopped)
        self.assertEqual(process.returncode, -signal.SIGKILL)


if __name__ == "__main__":
    unittest.main()
