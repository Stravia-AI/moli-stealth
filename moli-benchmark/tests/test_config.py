from __future__ import annotations

import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import moli_benchmark.config as config


class ConfigTests(unittest.TestCase):
    def test_moli_binary_prefers_release_build(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            repo_root = Path(temp)
            binary_name = "moli.exe" if os.name == "nt" else "moli"
            debug_bin = repo_root / "target" / "debug" / binary_name
            release_bin = repo_root / "target" / "release" / binary_name
            debug_bin.parent.mkdir(parents=True)
            release_bin.parent.mkdir(parents=True)
            debug_bin.write_text("debug", encoding="utf-8")
            release_bin.write_text("release", encoding="utf-8")
            os.utime(release_bin, (1, 1))
            os.utime(debug_bin, (2, 2))

            with patch.object(config, "REPO_ROOT", repo_root), patch.dict(os.environ, {}, clear=True):
                self.assertEqual(config.moli_binary(), release_bin)


if __name__ == "__main__":
    unittest.main()
