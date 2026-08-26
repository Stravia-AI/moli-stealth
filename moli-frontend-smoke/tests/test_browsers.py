from __future__ import annotations

import asyncio
from collections import deque
from pathlib import Path

import moli_frontend_smoke.browsers as browsers


def test_endpoint_wait_is_driven_by_browser_output() -> None:
    async def scenario() -> tuple[str, list[str]]:
        class RunningProcess:
            returncode = None

            async def wait(self) -> int:
                await asyncio.Event().wait()
                raise AssertionError("unreachable")

        reader = asyncio.StreamReader()
        logs: deque[str] = deque(maxlen=10)
        endpoint_future = asyncio.get_running_loop().create_future()
        output_task = asyncio.create_task(
            browsers._collect_output(
                reader,
                logs,
                "chromium:stderr",
                endpoint_future,
                browsers._chromium_endpoint_from_log,
            )
        )
        reader.feed_data(
            b"DevTools listening on "
            b"ws://127.0.0.1:43125/devtools/browser/session\n"
        )
        endpoint = await browsers._wait_for_announced_endpoint(
            RunningProcess(),
            [output_task],
            endpoint_future,
            logs,
            timeout=0.5,
        )
        reader.feed_eof()
        await output_task
        return endpoint, list(logs)

    endpoint, logs = asyncio.run(scenario())
    assert endpoint == "http://127.0.0.1:43125"
    assert logs == [
        "chromium:stderr: "
        "DevTools listening on ws://127.0.0.1:43125/devtools/browser/session"
    ]


def test_browser_stop_drains_output_through_eof_before_returning() -> None:
    async def scenario() -> list[str]:
        reader = asyncio.StreamReader()
        logs: deque[str] = deque(maxlen=10)
        endpoint_future = asyncio.get_running_loop().create_future()
        output_task = asyncio.create_task(
            browsers._collect_output(
                reader,
                logs,
                "moli:stderr",
                endpoint_future,
                browsers._moli_endpoint_from_log,
            )
        )

        class StoppingProcess:
            returncode = None

            def terminate(self) -> None:
                self.returncode = 0
                reader.feed_data(b"2026-08-01T00:00:00Z ERROR final diagnostic\n")
                reader.feed_eof()

            def kill(self) -> None:
                raise AssertionError("graceful stop should not need kill")

            async def wait(self) -> int:
                return 0

        process = browsers.BrowserProcess(
            name="moli",
            endpoint="http://127.0.0.1:43125",
            process=StoppingProcess(),  # type: ignore[arg-type]
            logs=logs,
            tasks=[output_task],
            temp_dirs=[],
            version={},
        )
        await process.stop()
        return list(logs)

    assert asyncio.run(scenario()) == [
        "moli:stderr: 2026-08-01T00:00:00Z ERROR final diagnostic"
    ]


def test_chromium_endpoint_parser_accepts_real_loopback_announcement() -> None:
    assert (
        browsers._chromium_endpoint_from_log(
            "DevTools listening on "
            "ws://127.0.0.1:43125/devtools/browser/4fd8c833-a177-4118"
        )
        == "http://127.0.0.1:43125"
    )
    assert (
        browsers._chromium_endpoint_from_log(
            "DevTools listening on ws://[::1]:43126/devtools/browser/session"
        )
        == "http://[::1]:43126"
    )


def test_chromium_endpoint_parser_rejects_unusable_announcements() -> None:
    assert (
        browsers._chromium_endpoint_from_log(
            "DevTools listening on ws://192.0.2.1:43125/devtools/browser/session"
        )
        is None
    )
    assert (
        browsers._chromium_endpoint_from_log(
            "DevTools listening on ws://127.0.0.1:0/devtools/browser/session"
        )
        is None
    )
    assert (
        browsers._chromium_endpoint_from_log(
            "DevTools listening on ws://127.0.0.1:43125/devtools/page/session"
        )
        is None
    )


def test_moli_endpoint_parser_accepts_only_server_listening_log() -> None:
    assert (
        browsers._moli_endpoint_from_log(
            "2026-07-30T04:58:51Z INFO protocol server listening "
            "addr=127.0.0.1:44723"
        )
        == "http://127.0.0.1:44723"
    )
    assert (
        browsers._moli_endpoint_from_log(
            "\x1b[2m2026-08-26T21:42:18Z\x1b[0m \x1b[32m INFO\x1b[0m "
            "protocol server listening \x1b[3maddr\x1b[0m\x1b[2m=\x1b[0m"
            "127.0.0.1:33577"
        )
        == "http://127.0.0.1:33577"
    )
    assert (
        browsers._moli_endpoint_from_log(
            "INFO request complete addr=127.0.0.1:44723"
        )
        is None
    )
    assert (
        browsers._moli_endpoint_from_log(
            "INFO protocol server listening addr=127.0.0.1:0"
        )
        is None
    )


def test_launchers_delegate_atomic_port_selection_to_each_browser(
    monkeypatch, tmp_path: Path
) -> None:
    calls: list[dict[str, object]] = []

    async def fake_start_process(**kwargs):
        calls.append(kwargs)
        return object()

    next_temp_dir = 0

    def fake_mkdtemp(*, prefix: str) -> str:
        nonlocal next_temp_dir
        next_temp_dir += 1
        directory = tmp_path / f"{prefix}{next_temp_dir}"
        directory.mkdir()
        return str(directory)

    monkeypatch.setattr(browsers, "_start_process", fake_start_process)
    monkeypatch.setattr(browsers.tempfile, "mkdtemp", fake_mkdtemp)

    asyncio.run(browsers.start_chromium(Path("/test/chromium")))
    asyncio.run(browsers.start_moli(Path("/test/moli")))

    chromium = calls[0]
    assert "--remote-debugging-port=0" in chromium["command"]
    assert "endpoint" not in chromium
    assert (
        chromium["endpoint_parser"](
            "DevTools listening on ws://127.0.0.1:41001/devtools/browser/session"
        )
        == "http://127.0.0.1:41001"
    )

    moli = calls[1]
    command = moli["command"]
    assert command[command.index("--port") + 1] == "0"
    assert "--cdp-max-connections" not in command
    assert "endpoint" not in moli
    assert (
        moli["endpoint_parser"](
            "INFO protocol server listening addr=127.0.0.1:41002"
        )
        == "http://127.0.0.1:41002"
    )
