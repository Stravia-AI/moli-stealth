"""CLI, CDP, BiDi, and Classic consumers for the navigation trace fixture."""

from __future__ import annotations

import asyncio
import json
import os
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Callable
from urllib.parse import urlparse

import websockets

from .config import REPO_ROOT, clear_proxy_env
from .navigation_trace_fixture import NavigationTraceFixture
from .process import run_process
from .raw_cdp import RawCdpClient, connect_raw_cdp
from .serve import start_moli_serve, stop_moli_serve

TRACE_ENV = "MOLI_BROWSER_OWNER_TRACE_JSONL"
TRACE_RESOURCE_SAMPLE_INTERVAL_SECONDS = 0.01
FINAL_OBSERVATION_EXPRESSION = (
    "JSON.stringify({url: location.href, title: document.title, "
    "phase: document.body && document.body.dataset.tracePhase})"
)


def _trace_offset(path: Path) -> int:
    try:
        return path.stat().st_size
    except OSError:
        return 0


def _prepare_trace_path(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.unlink(missing_ok=True)


def _url_stage(url: str) -> str | None:
    path = urlparse(url).path
    prefix = "/navigation-trace/"
    if not path.startswith(prefix):
        return None
    stage = path.removeprefix(prefix)
    return stage if stage in {"bootstrap", "a", "b"} else None


def _final_observation_from_html(final_url: str, html: str) -> dict[str, str]:
    title = "Trace B" if "<title>Trace B</title>" in html else ""
    return {"url": final_url, "title": title, "phase": "b" if "data-trace-phase=\"b\"" in html else ""}


def _run_cli_frontend(
    moli_bin: Path,
    fixture: NavigationTraceFixture,
    run_token: str,
    trace_path: Path,
    timeout_seconds: float,
) -> dict[str, Any]:
    env = clear_proxy_env(os.environ)
    env[TRACE_ENV] = str(trace_path)
    result = run_process(
        [
            str(moli_bin),
            "fetch",
            "--dump",
            "json",
            "--wait-until",
            "done",
            "--timeout",
            str(int(timeout_seconds * 1000)),
            fixture.url("a", run_token),
        ],
        cwd=REPO_ROOT,
        timeout_seconds=timeout_seconds + 1,
        env=env,
        resource_sample_interval_seconds=TRACE_RESOURCE_SAMPLE_INTERVAL_SECONDS,
    )
    if result.returncode != 0 or result.timed_out:
        raise RuntimeError(f"CLI navigation failed: {result.json_summary(include_output=True)}")
    payload = json.loads(result.stdout)
    if not isinstance(payload, dict):
        raise RuntimeError(f"CLI fetch returned unexpected JSON: {payload!r}")
    fixture.wait_for(run_token, "complete", timeout_seconds)
    observation = _final_observation_from_html(str(payload.get("final_url") or ""), str(payload.get("html") or ""))
    return {
        "navigation_elapsed_ms": result.elapsed_ms,
        "latency_scope": "cold-process-to-successor-load",
        "final_observation": observation,
        "protocol_lifecycle": [],
        "trace_offset": 0,
        "resources": result.resources,
        "process": result.json_summary(include_output=False),
        "followup_commands_before_terminal": 0,
    }


async def _run_cdp_frontend(
    moli_bin: Path,
    fixture: NavigationTraceFixture,
    run_token: str,
    trace_path: Path,
    timeout_seconds: float,
) -> dict[str, Any]:
    handle = start_moli_serve(
        moli_bin,
        timeout_seconds,
        env_overrides={TRACE_ENV: str(trace_path)},
        resource_sample_interval_seconds=TRACE_RESOURCE_SAMPLE_INTERVAL_SECONDS,
    )
    client: RawCdpClient | None = None
    stop_summary: dict[str, Any] = {}
    result_payload: dict[str, Any] | None = None
    try:
        client = await connect_raw_cdp(handle.endpoint)
        target_id = await _cdp_create_target(client, timeout_seconds)
        session_id = await _cdp_attach_target(client, target_id, timeout_seconds)
        for method, params in (
            ("Page.enable", None),
            ("Page.setLifecycleEventsEnabled", {"enabled": True}),
            ("Runtime.enable", None),
        ):
            command_id = await client.send(method, params, session_id=session_id)
            await client.recv_until_id(command_id, timeout=timeout_seconds)

        bootstrap_id = await client.send(
            "Page.navigate",
            {"url": fixture.url("bootstrap", run_token)},
            session_id=session_id,
        )
        bootstrap_response, bootstrap_messages = await client.recv_until_id(
            bootstrap_id, timeout=timeout_seconds
        )
        bootstrap_loader = bootstrap_response.get("result", {}).get("loaderId")
        if not isinstance(bootstrap_loader, str):
            raise RuntimeError(f"CDP bootstrap navigation lacked loaderId: {bootstrap_response}")
        await _recv_cdp_until(
            client,
            bootstrap_messages,
            lambda messages: _cdp_has_stage_load(messages, {bootstrap_loader: "bootstrap"}, "bootstrap"),
            timeout_seconds,
        )
        await asyncio.to_thread(fixture.wait_for, run_token, "ready", timeout_seconds)
        # This pre-navigation command is only a bootstrap trace barrier. No
        # command is sent after the measured A navigation until B reaches load.
        barrier_id = await client.send(
            "Runtime.evaluate",
            {"expression": "0", "returnByValue": True},
            session_id=session_id,
        )
        await client.recv_until_id(barrier_id, timeout=timeout_seconds)
        trace_offset = _trace_offset(trace_path)

        started = time.perf_counter()
        navigate_id = await client.send(
            "Page.navigate",
            {"url": fixture.url("a", run_token)},
            session_id=session_id,
        )
        navigate_response, navigation_messages = await client.recv_until_id(
            navigate_id, timeout=timeout_seconds
        )
        loader_a = navigate_response.get("result", {}).get("loaderId")
        if not isinstance(loader_a, str):
            raise RuntimeError(f"CDP navigation lacked loaderId: {navigate_response}")
        await _recv_cdp_until(
            client,
            navigation_messages,
            lambda messages: _cdp_has_stage_load(messages, {loader_a: "a"}, "b"),
            timeout_seconds,
        )
        await asyncio.to_thread(fixture.wait_for, run_token, "complete", timeout_seconds)
        navigation_elapsed_ms = (time.perf_counter() - started) * 1000.0
        protocol_lifecycle = _cdp_lifecycle_shape(navigation_messages, {loader_a: "a"})

        evaluate_id = await client.send(
            "Runtime.evaluate",
            {
                "expression": FINAL_OBSERVATION_EXPRESSION,
                "returnByValue": True,
            },
            session_id=session_id,
        )
        evaluate, _ = await client.recv_until_id(evaluate_id, timeout=timeout_seconds)
        value = evaluate.get("result", {}).get("result", {}).get("value")
        if not isinstance(value, str):
            raise RuntimeError(f"CDP final observation was not a string: {evaluate}")
        final_observation = json.loads(value)
        close_id = await client.send("Target.closeTarget", {"targetId": target_id})
        await client.recv_until_id(close_id, timeout=timeout_seconds)
        result_payload = {
            "navigation_elapsed_ms": navigation_elapsed_ms,
            "latency_scope": "warm-host-command-to-successor-load",
            "final_observation": final_observation,
            "protocol_lifecycle": protocol_lifecycle,
            "trace_offset": trace_offset,
            "ready_ms": handle.ready_ms,
            "followup_commands_before_terminal": 0,
        }
    finally:
        if client is not None:
            await client.websocket.close()
        stop_summary = stop_moli_serve(handle)
    if result_payload is None:
        raise RuntimeError("CDP navigation completed without a result")
    result_payload["resources"] = stop_summary.get("resources", {})
    result_payload["serve"] = stop_summary
    return result_payload


async def _cdp_create_target(client: RawCdpClient, timeout_seconds: float) -> str:
    command_id = await client.send("Target.createTarget", {"url": "about:blank"})
    response, _ = await client.recv_until_id(command_id, timeout=timeout_seconds)
    target_id = response.get("result", {}).get("targetId")
    if not isinstance(target_id, str) or not target_id:
        raise RuntimeError(f"Target.createTarget lacked targetId: {response}")
    return target_id


async def _cdp_attach_target(client: RawCdpClient, target_id: str, timeout_seconds: float) -> str:
    command_id = await client.send("Target.attachToTarget", {"targetId": target_id, "flatten": True})
    response, _ = await client.recv_until_id(command_id, timeout=timeout_seconds)
    session_id = response.get("result", {}).get("sessionId")
    if not isinstance(session_id, str) or not session_id:
        raise RuntimeError(f"Target.attachToTarget lacked sessionId: {response}")
    return session_id


async def _recv_cdp_until(
    client: RawCdpClient,
    messages: list[dict[str, Any]],
    predicate: Callable[[list[dict[str, Any]]], bool],
    timeout_seconds: float,
) -> None:
    deadline = asyncio.get_running_loop().time() + timeout_seconds
    while not predicate(messages):
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            raise TimeoutError(f"timed out waiting for CDP lifecycle; tail={messages[-20:]}")
        messages.append(await asyncio.wait_for(client.recv(), timeout=remaining))


def _cdp_loader_stages(
    messages: list[dict[str, Any]], seed: dict[str, str]
) -> dict[str, str]:
    loader_stages = dict(seed)
    for message in messages:
        if message.get("method") != "Page.frameNavigated":
            continue
        frame = message.get("params", {}).get("frame", {})
        loader_id = frame.get("loaderId")
        stage = _url_stage(str(frame.get("url") or ""))
        if isinstance(loader_id, str) and stage is not None:
            loader_stages[loader_id] = stage
    return loader_stages


def _cdp_has_stage_load(
    messages: list[dict[str, Any]], seed: dict[str, str], expected_stage: str
) -> bool:
    loader_stages = _cdp_loader_stages(messages, seed)
    return any(
        message.get("method") == "Page.lifecycleEvent"
        and message.get("params", {}).get("name") == "load"
        and loader_stages.get(message.get("params", {}).get("loaderId")) == expected_stage
        for message in messages
    )


def _cdp_lifecycle_shape(
    messages: list[dict[str, Any]], seed: dict[str, str]
) -> list[str]:
    loader_stages = _cdp_loader_stages(messages, seed)
    lifecycle: list[str] = []
    for message in messages:
        if message.get("method") != "Page.lifecycleEvent":
            continue
        params = message.get("params", {})
        stage = loader_stages.get(params.get("loaderId"))
        name = params.get("name")
        if stage in {"a", "b"} and name in {"DOMContentLoaded", "load"}:
            lifecycle.append(f"{stage}:{'dom-content-loaded' if name == 'DOMContentLoaded' else 'load'}")
    return lifecycle


async def _run_bidi_frontend(
    moli_bin: Path,
    fixture: NavigationTraceFixture,
    run_token: str,
    trace_path: Path,
    timeout_seconds: float,
) -> dict[str, Any]:
    handle = start_moli_serve(
        moli_bin,
        timeout_seconds,
        env_overrides={TRACE_ENV: str(trace_path)},
        resource_sample_interval_seconds=TRACE_RESOURCE_SAMPLE_INTERVAL_SECONDS,
    )
    websocket_url = handle.endpoint.replace("http://", "ws://", 1).rstrip("/") + "/session"
    stop_summary: dict[str, Any] = {}
    result_payload: dict[str, Any] | None = None
    try:
        async with websockets.connect(websocket_url, proxy=None, max_size=2**24) as websocket:
            await _bidi_command(websocket, 1, "session.status", {}, timeout_seconds)
            await _bidi_command(websocket, 2, "session.new", {"capabilities": {}}, timeout_seconds)
            create, _ = await _bidi_command(
                websocket, 3, "browsingContext.create", {"type": "tab"}, timeout_seconds
            )
            context = create.get("result", {}).get("context")
            if not isinstance(context, str) or not context:
                raise RuntimeError(f"BiDi browsingContext.create lacked context: {create}")
            await _bidi_command(
                websocket,
                4,
                "session.subscribe",
                {
                    "events": [
                        "browsingContext.navigationStarted",
                        "browsingContext.domContentLoaded",
                        "browsingContext.load",
                    ],
                    "contexts": [context],
                },
                timeout_seconds,
            )
            _, bootstrap_messages = await _bidi_command(
                websocket,
                5,
                "browsingContext.navigate",
                {"context": context, "url": fixture.url("bootstrap", run_token), "wait": "complete"},
                timeout_seconds,
            )
            await _recv_bidi_until(
                websocket,
                bootstrap_messages,
                lambda messages: _bidi_has_stage_load(messages, "bootstrap"),
                timeout_seconds,
            )
            await asyncio.to_thread(fixture.wait_for, run_token, "ready", timeout_seconds)
            # Keep bootstrap records outside the measured trace without using
            # a post-navigation heartbeat to advance Browser Owner work.
            await _bidi_command(
                websocket,
                6,
                "script.evaluate",
                {
                    "expression": "0",
                    "target": {"context": context},
                    "awaitPromise": False,
                },
                timeout_seconds,
            )
            trace_offset = _trace_offset(trace_path)

            started = time.perf_counter()
            _, navigation_messages = await _bidi_command(
                websocket,
                7,
                "browsingContext.navigate",
                {"context": context, "url": fixture.url("a", run_token), "wait": "complete"},
                timeout_seconds,
            )
            await _recv_bidi_until(
                websocket,
                navigation_messages,
                lambda messages: _bidi_has_stage_load(messages, "b"),
                timeout_seconds,
            )
            await asyncio.to_thread(fixture.wait_for, run_token, "complete", timeout_seconds)
            navigation_elapsed_ms = (time.perf_counter() - started) * 1000.0
            protocol_lifecycle = _bidi_lifecycle_shape(navigation_messages)

            evaluate, _ = await _bidi_command(
                websocket,
                8,
                "script.evaluate",
                {
                    "expression": FINAL_OBSERVATION_EXPRESSION,
                    "target": {"context": context},
                    "awaitPromise": False,
                },
                timeout_seconds,
            )
            value = evaluate.get("result", {}).get("result", {}).get("value")
            if not isinstance(value, str):
                raise RuntimeError(f"BiDi final observation was not a string: {evaluate}")
            final_observation = json.loads(value)
            await _bidi_command(websocket, 9, "session.end", {}, timeout_seconds)
            result_payload = {
                "navigation_elapsed_ms": navigation_elapsed_ms,
                "latency_scope": "warm-host-command-to-successor-load",
                "final_observation": final_observation,
                "protocol_lifecycle": protocol_lifecycle,
                "trace_offset": trace_offset,
                "ready_ms": handle.ready_ms,
                "followup_commands_before_terminal": 0,
            }
    finally:
        stop_summary = stop_moli_serve(handle)
    if result_payload is None:
        raise RuntimeError("BiDi navigation completed without a result")
    result_payload["resources"] = stop_summary.get("resources", {})
    result_payload["serve"] = stop_summary
    return result_payload


async def _bidi_command(
    websocket: Any,
    command_id: int,
    method: str,
    params: dict[str, Any],
    timeout_seconds: float,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    await websocket.send(
        json.dumps({"id": command_id, "method": method, "params": params}, separators=(",", ":"))
    )
    messages: list[dict[str, Any]] = []
    deadline = asyncio.get_running_loop().time() + timeout_seconds
    while True:
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            raise TimeoutError(f"timed out waiting for BiDi command {method}; tail={messages[-20:]}")
        raw = await asyncio.wait_for(websocket.recv(), timeout=remaining)
        message = json.loads(raw)
        if not isinstance(message, dict):
            raise RuntimeError(f"unexpected BiDi message: {message!r}")
        messages.append(message)
        if message.get("id") == command_id:
            if message.get("type") != "success":
                raise RuntimeError(f"BiDi command {method} failed: {message}")
            return message, messages


async def _recv_bidi_until(
    websocket: Any,
    messages: list[dict[str, Any]],
    predicate: Callable[[list[dict[str, Any]]], bool],
    timeout_seconds: float,
) -> None:
    deadline = asyncio.get_running_loop().time() + timeout_seconds
    while not predicate(messages):
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            raise TimeoutError(f"timed out waiting for BiDi lifecycle; tail={messages[-20:]}")
        raw = await asyncio.wait_for(websocket.recv(), timeout=remaining)
        message = json.loads(raw)
        if isinstance(message, dict):
            messages.append(message)


def _bidi_has_stage_load(messages: list[dict[str, Any]], expected_stage: str) -> bool:
    return any(
        message.get("method") == "browsingContext.load"
        and _url_stage(str(message.get("params", {}).get("url") or "")) == expected_stage
        for message in messages
    )


def _bidi_lifecycle_shape(messages: list[dict[str, Any]]) -> list[str]:
    lifecycle: list[str] = []
    for message in messages:
        method = message.get("method")
        if method not in {"browsingContext.domContentLoaded", "browsingContext.load"}:
            continue
        stage = _url_stage(str(message.get("params", {}).get("url") or ""))
        if stage in {"a", "b"}:
            lifecycle.append(
                f"{stage}:{'dom-content-loaded' if method.endswith('domContentLoaded') else 'load'}"
            )
    return lifecycle


def _run_classic_frontend(
    moli_bin: Path,
    fixture: NavigationTraceFixture,
    run_token: str,
    trace_path: Path,
    timeout_seconds: float,
) -> dict[str, Any]:
    handle = start_moli_serve(
        moli_bin,
        timeout_seconds,
        env_overrides={TRACE_ENV: str(trace_path)},
        resource_sample_interval_seconds=TRACE_RESOURCE_SAMPLE_INTERVAL_SECONDS,
    )
    session_id: str | None = None
    stop_summary: dict[str, Any] = {}
    result_payload: dict[str, Any] | None = None
    try:
        session = _webdriver_request(
            handle.endpoint, "POST", "/session", {"capabilities": {"alwaysMatch": {}}}, timeout_seconds
        )
        value = session.get("value")
        session_id = value.get("sessionId") if isinstance(value, dict) else None
        if not isinstance(session_id, str) or not session_id:
            raise RuntimeError(f"Classic session creation lacked sessionId: {session}")
        _webdriver_request(
            handle.endpoint,
            "POST",
            f"/session/{session_id}/url",
            {"url": fixture.url("bootstrap", run_token)},
            timeout_seconds,
        )
        fixture.wait_for(run_token, "ready", timeout_seconds)
        # Classic has no lifecycle subscription, so a pre-navigation title
        # read provides the same bootstrap/trace cut as the protocol runners.
        _webdriver_request(
            handle.endpoint,
            "GET",
            f"/session/{session_id}/title",
            None,
            timeout_seconds,
        )
        trace_offset = _trace_offset(trace_path)

        started = time.perf_counter()
        _webdriver_request(
            handle.endpoint,
            "POST",
            f"/session/{session_id}/url",
            {"url": fixture.url("a", run_token)},
            timeout_seconds,
        )
        fixture.wait_for(run_token, "complete", timeout_seconds)
        navigation_elapsed_ms = (time.perf_counter() - started) * 1000.0
        final_url = _webdriver_request(
            handle.endpoint, "GET", f"/session/{session_id}/url", None, timeout_seconds
        ).get("value")
        final_title = _webdriver_request(
            handle.endpoint, "GET", f"/session/{session_id}/title", None, timeout_seconds
        ).get("value")
        source = _webdriver_request(
            handle.endpoint, "GET", f"/session/{session_id}/source", None, timeout_seconds
        ).get("value")
        result_payload = {
            "navigation_elapsed_ms": navigation_elapsed_ms,
            "latency_scope": "warm-host-command-to-successor-load",
            "final_observation": {
                "url": final_url,
                "title": final_title,
                "phase": "b" if isinstance(source, str) and "data-trace-phase=\"b\"" in source else "",
            },
            "protocol_lifecycle": [],
            "trace_offset": trace_offset,
            "ready_ms": handle.ready_ms,
            "followup_commands_before_terminal": 0,
        }
    finally:
        if session_id is not None:
            try:
                _webdriver_request(
                    handle.endpoint,
                    "DELETE",
                    f"/session/{session_id}",
                    None,
                    timeout_seconds,
                )
            except Exception:
                pass
        stop_summary = stop_moli_serve(handle)
    if result_payload is None:
        raise RuntimeError("Classic navigation completed without a result")
    result_payload["resources"] = stop_summary.get("resources", {})
    result_payload["serve"] = stop_summary
    return result_payload


def _webdriver_request(
    endpoint: str,
    method: str,
    path: str,
    body: dict[str, Any] | None,
    timeout_seconds: float,
) -> dict[str, Any]:
    data = json.dumps(body).encode("utf-8") if body is not None else None
    headers = {"Accept": "application/json"}
    if body is not None:
        headers["Content-Type"] = "application/json"
    request = urllib.request.Request(endpoint.rstrip("/") + path, data=data, headers=headers, method=method)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    try:
        with opener.open(request, timeout=timeout_seconds) as response:
            payload = response.read()
            status = response.status
    except urllib.error.HTTPError as error:
        payload = error.read()
        status = error.code
    parsed = json.loads(payload.decode("utf-8"))
    if status != 200 or not isinstance(parsed, dict):
        raise RuntimeError(f"WebDriver {method} {path} failed with {status}: {parsed!r}")
    return parsed


def _run_frontend(
    frontend: str,
    moli_bin: Path,
    fixture: NavigationTraceFixture,
    run_token: str,
    trace_path: Path,
    timeout_seconds: float,
) -> dict[str, Any]:
    if frontend == "cli":
        return _run_cli_frontend(moli_bin, fixture, run_token, trace_path, timeout_seconds)
    if frontend == "cdp":
        return asyncio.run(
            _run_cdp_frontend(moli_bin, fixture, run_token, trace_path, timeout_seconds)
        )
    if frontend == "bidi":
        return asyncio.run(
            _run_bidi_frontend(moli_bin, fixture, run_token, trace_path, timeout_seconds)
        )
    if frontend == "classic":
        return _run_classic_frontend(moli_bin, fixture, run_token, trace_path, timeout_seconds)
    raise RuntimeError(f"unknown navigation trace frontend: {frontend}")
