from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from .external_process import run_external_json_process


PUPPETEER_SCRIPT = Path(__file__).resolve().parents[1] / "puppeteer_smoke.mjs"


async def run_puppeteer_group(endpoint: str, fixture: str, results: list[dict[str, Any]]) -> None:
    await run_external_json_process(
        "Puppeteer",
        [
            os.environ.get("NODE", "node"),
            str(PUPPETEER_SCRIPT),
            endpoint,
            fixture,
        ],
        results,
        timeout_seconds=30,
    )
