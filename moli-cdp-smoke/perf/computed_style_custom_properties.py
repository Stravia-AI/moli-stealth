"""Reproduce CDP computed-style scaling with inherited custom properties.

The page contains one target element. The only variable is the number of
custom properties inherited from ``:root``. Each measurement sends one
``CSS.getComputedStyleForNode`` command, so DOM size, network activity, layout,
and paint do not contribute to the result.

Usage:
    uv run --project moli-cdp-smoke python \
        moli-cdp-smoke/perf/computed_style_custom_properties.py
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import socket
import subprocess
import time
import urllib.parse
import urllib.request
from contextlib import closing
from pathlib import Path
from typing import Any

import websockets  # type: ignore

REPO_ROOT = Path(__file__).resolve().parents[2]


def free_port() -> int:
    with closing(socket.socket(socket.AF_INET, socket.SOCK_STREAM)) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def resolve_moli_bin() -> str:
    return os.environ.get("MOLI_BIN", str(REPO_ROOT / "target/release/moli"))


def local_json(url: str) -> Any:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(url, timeout=0.5) as response:
        return json.loads(response.read())


async def wait_page_websocket(port: int, timeout_s: float = 15.0) -> str:
    deadline = time.monotonic() + timeout_s
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            targets = await asyncio.to_thread(
                local_json, f"http://127.0.0.1:{port}/json/list"
            )
            for target in targets:
                if target.get("type") == "page" and target.get("webSocketDebuggerUrl"):
                    return target["webSocketDebuggerUrl"]
        except Exception as error:  # The process may still be binding its socket.
            last_error = error
        await asyncio.sleep(0.05)
    raise RuntimeError(f"CDP page target did not become ready: {last_error}")


class CdpClient:
    def __init__(self, websocket: Any) -> None:
        self.websocket = websocket
        self.next_id = 1

    async def command(
        self, method: str, params: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        request_id = self.next_id
        self.next_id += 1
        message: dict[str, Any] = {"id": request_id, "method": method}
        if params is not None:
            message["params"] = params
        await self.websocket.send(json.dumps(message, separators=(",", ":")))
        while True:
            response = json.loads(await self.websocket.recv())
            if response.get("id") != request_id:
                continue
            if "error" in response:
                raise RuntimeError(f"{method} failed: {response['error']}")
            return response.get("result", {})


def fixture_url(custom_property_count: int) -> str:
    declarations = "".join(
        f"--property-{index:04d}:{index};" for index in range(custom_property_count)
    )
    html = (
        "<!doctype html><meta charset=utf-8>"
        f"<style>:root{{{declarations}}}</style>"
        '<div id="target">target</div>'
    )
    return "data:text/html;charset=utf-8," + urllib.parse.quote(html, safe="")


async def wait_until_loaded(client: CdpClient) -> None:
    for _ in range(200):
        result = await client.command(
            "Runtime.evaluate",
            {"expression": "document.readyState", "returnByValue": True},
        )
        if result.get("result", {}).get("value") == "complete":
            return
        await asyncio.sleep(0.01)
    raise RuntimeError("fixture navigation did not complete")


async def measure(port: int, counts: list[int], runs: int) -> None:
    websocket_url = await wait_page_websocket(port)
    async with websockets.connect(
        websocket_url, max_size=192 * 1024 * 1024, proxy=None
    ) as ws:
        client = CdpClient(ws)
        await client.command("Page.enable")
        await client.command("DOM.enable")
        await client.command("CSS.enable")

        print(
            "custom_properties  "
            + "  ".join(f"run_{run + 1}_ms" for run in range(runs))
        )
        for count in counts:
            await client.command("Page.navigate", {"url": fixture_url(count)})
            await wait_until_loaded(client)
            document = await client.command("DOM.getDocument", {"depth": 1})
            target = await client.command(
                "DOM.querySelector",
                {"nodeId": document["root"]["nodeId"], "selector": "#target"},
            )

            samples: list[float] = []
            returned_custom_count = 0
            for _ in range(runs):
                started = time.perf_counter()
                result = await client.command(
                    "CSS.getComputedStyleForNode", {"nodeId": target["nodeId"]}
                )
                samples.append((time.perf_counter() - started) * 1000.0)
                returned_custom_count = sum(
                    property_.get("name", "").startswith("--")
                    for property_ in result.get("computedStyle", [])
                )
            if returned_custom_count != count:
                raise RuntimeError(
                    f"requested {count} custom properties, got {returned_custom_count}"
                )
            print(f"{count:17d}  " + "  ".join(f"{sample:8.2f}" for sample in samples))


async def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--counts", type=int, nargs="+", default=[0, 250, 500, 1000, 2000]
    )
    parser.add_argument("--runs", type=int, default=2)
    args = parser.parse_args()

    port = free_port()
    environment = {
        key: value
        for key, value in os.environ.items()
        if "PROXY" not in key.upper()
    }
    environment["NO_PROXY"] = "127.0.0.1,localhost"
    process = subprocess.Popen(
        [
            resolve_moli_bin(),
            "serve",
            "-lr",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
        ],
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        await measure(port, args.counts, args.runs)
    finally:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


if __name__ == "__main__":
    asyncio.run(main())
