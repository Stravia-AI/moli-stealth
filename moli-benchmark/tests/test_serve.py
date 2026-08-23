from __future__ import annotations

import io
import unittest
from pathlib import Path
from unittest.mock import patch

from moli_benchmark.serve import start_moli_serve, stop_moli_serve


class _FakeProcess:
    pid = 12345

    def __init__(self) -> None:
        self.returncode: int | None = None
        self.stdout = io.BytesIO()
        self.stderr = io.BytesIO()

    def poll(self) -> int | None:
        return self.returncode

    def terminate(self) -> None:
        self.returncode = -15

    def wait(self, timeout: float | None = None) -> int | None:
        del timeout
        return self.returncode


class _FakeSampler:
    def __init__(self, pid: int, *, interval_seconds: float = 0.1) -> None:
        self.pid = pid
        self.interval_seconds = interval_seconds

    def start(self) -> None:
        return None

    def stop(self) -> dict[str, int]:
        return {"pid": self.pid}


class ServeTests(unittest.TestCase):
    def test_start_serve_applies_diagnostic_environment_override(self) -> None:
        process = _FakeProcess()
        with (
            patch("moli_benchmark.serve.subprocess.Popen", return_value=process) as popen,
            patch("moli_benchmark.serve.ResourceSampler", _FakeSampler),
            patch("moli_benchmark.serve.probe_url", return_value=True),
        ):
            handle = start_moli_serve(
                Path("/bin/moli"),
                1.0,
                env_overrides={"MOLI_BROWSER_OWNER_TRACE_JSONL": "/tmp/trace.jsonl"},
                resource_sample_interval_seconds=0.01,
            )
            child_env = popen.call_args.kwargs["env"]

            self.assertEqual(
                child_env["MOLI_BROWSER_OWNER_TRACE_JSONL"],
                "/tmp/trace.jsonl",
            )
            self.assertEqual(handle.sampler.interval_seconds, 0.01)
            stop_moli_serve(handle)


if __name__ == "__main__":
    unittest.main()
