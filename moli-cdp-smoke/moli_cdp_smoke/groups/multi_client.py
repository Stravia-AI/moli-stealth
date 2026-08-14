from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

from ..assertions import SmokeError, assert_equal, record
from ..raw_cdp import (
    RawCdpClient,
    connect_raw_cdp,
    connect_raw_cdp_websocket,
    discover_target_websocket_url,
)
from .multi_client_fanout import _run_fanout_case
from .multi_client_support import (
    _align_next_command_id,
    _assert_attach_event_precedes_response,
    _create_browser_context,
    _dispose_browser_context,
    _find_target_event,
    _recv_until_id_allow_error,
    _reject_foreign_session_output,
    _required_session_id,
    _runtime_value,
)


@dataclass(frozen=True)
class AttachedTarget:
    browser_context_id: str
    target_id: str
    first_session_id: str
    second_session_id: str


async def run_multi_client_group(
    endpoint: str,
    _fixture: str,
    results: list[dict[str, Any]],
) -> None:
    first = await connect_raw_cdp(endpoint)
    second: RawCdpClient | None = None
    browser_context_id: str | None = None
    try:
        second = await connect_raw_cdp(endpoint)
        await _colliding_browser_root_commands(first, second)
        browser_context_id = await _create_browser_context(second)
        attached = await _create_and_attach_shared_target(
            first,
            second,
            browser_context_id,
        )
        await _browser_session_isolation(first, second, attached, results)

        page_websocket_url = await discover_target_websocket_url(
            endpoint,
            attached.target_id,
        )
        await _direct_page_client_isolation(page_websocket_url, results)

        await first.websocket.close()
        await _surviving_browser_client(second, attached, results)
    finally:
        if browser_context_id is not None:
            await _dispose_browser_context(browser_context_id, (second, first))
        await asyncio.gather(
            *(
                client.websocket.close()
                for client in (first, second)
                if client is not None
            ),
            return_exceptions=True,
        )
    for client_count in (3, 7):
        await _run_fanout_case(endpoint, client_count, results)


async def _colliding_browser_root_commands(
    first: RawCdpClient,
    second: RawCdpClient,
) -> None:
    first_id, second_id = await asyncio.gather(
        first.send("Browser.getVersion"),
        second.send("Browser.getVersion"),
    )
    assert_equal(first_id, second_id, "multi-client browser root command id collision")
    (first_response, _), (second_response, _) = await asyncio.gather(
        first.recv_until_id(first_id, timeout=5),
        second.recv_until_id(second_id, timeout=5),
    )
    for label, response in (
        ("first", first_response),
        ("second", second_response),
    ):
        product = response.get("result", {}).get("product")
        if not isinstance(product, str) or not product:
            raise SmokeError(f"{label} browser client returned no product: {response}")
        if "sessionId" in response:
            raise SmokeError(
                f"{label} browser response leaked its hidden base session: {response}"
            )


async def _create_and_attach_shared_target(
    first: RawCdpClient,
    second: RawCdpClient,
    browser_context_id: str,
) -> AttachedTarget:
    discover_id = await first.send(
        "Target.setDiscoverTargets",
        {"discover": True, "filter": [{"type": "page"}]},
    )
    await first.recv_until_id(discover_id, timeout=5)

    create_target_id = await second.send(
        "Target.createTarget",
        {"browserContextId": browser_context_id, "url": "about:blank"},
    )
    create_target, second_create_seen = await second.recv_until_id(
        create_target_id,
        timeout=5,
    )
    target_id = create_target.get("result", {}).get("targetId")
    if not isinstance(target_id, str) or not target_id:
        raise SmokeError(f"multi-client setup returned no targetId: {create_target}")
    if (
        _find_target_event(second_create_seen, "Target.targetCreated", target_id)
        is not None
    ):
        raise SmokeError(
            "target discovery state leaked from the first browser client to the second"
        )

    collision_id = _align_next_command_id(first, second)
    first_attach_id, second_attach_id = await asyncio.gather(
        first.send(
            "Target.attachToTarget",
            {"targetId": target_id, "flatten": True},
        ),
        second.send(
            "Target.attachToTarget",
            {"targetId": target_id, "flatten": True},
        ),
    )
    assert_equal(first_attach_id, collision_id, "first colliding attach command id")
    assert_equal(second_attach_id, collision_id, "second colliding attach command id")
    (first_attach, first_seen), (second_attach, second_seen) = await asyncio.gather(
        first.recv_until_id(first_attach_id, timeout=5),
        second.recv_until_id(second_attach_id, timeout=5),
    )
    first_session_id = _required_session_id(first_attach, "first browser attach")
    second_session_id = _required_session_id(second_attach, "second browser attach")
    if first_session_id == second_session_id:
        raise SmokeError(
            "two browser clients attached to the same target with one shared sessionId"
        )
    _assert_attach_event_precedes_response(
        first_seen,
        first_attach_id,
        first_session_id,
        "first browser attach",
    )
    _assert_attach_event_precedes_response(
        second_seen,
        second_attach_id,
        second_session_id,
        "second browser attach",
    )
    target_created = _find_target_event(first_seen, "Target.targetCreated", target_id)
    if target_created is None:
        raise SmokeError(
            "the browser client with discovery enabled did not receive Target.targetCreated"
        )
    if "sessionId" in target_created:
        raise SmokeError(
            f"target discovery leaked the browser client's hidden base session: {target_created}"
        )
    _reject_foreign_session_output(
        first_seen,
        second_session_id,
        "first browser attach",
    )
    _reject_foreign_session_output(
        second_seen,
        first_session_id,
        "second browser attach",
    )
    return AttachedTarget(
        browser_context_id=browser_context_id,
        target_id=target_id,
        first_session_id=first_session_id,
        second_session_id=second_session_id,
    )


async def _browser_session_isolation(
    first: RawCdpClient,
    second: RawCdpClient,
    attached: AttachedTarget,
    results: list[dict[str, Any]],
) -> None:
    collision_id = _align_next_command_id(first, second)
    first_evaluate_id, second_evaluate_id = await asyncio.gather(
        first.send(
            "Runtime.evaluate",
            {"expression": "'first-browser-client'", "returnByValue": True},
            session_id=attached.first_session_id,
        ),
        second.send(
            "Runtime.evaluate",
            {"expression": "'second-browser-client'", "returnByValue": True},
            session_id=attached.second_session_id,
        ),
    )
    assert_equal(first_evaluate_id, collision_id, "first colliding evaluate command id")
    assert_equal(
        second_evaluate_id, collision_id, "second colliding evaluate command id"
    )
    (first_response, first_seen), (second_response, second_seen) = await asyncio.gather(
        first.recv_until_id(first_evaluate_id, timeout=5),
        second.recv_until_id(second_evaluate_id, timeout=5),
    )
    assert_equal(
        _runtime_value(first_response),
        "first-browser-client",
        "first colliding browser response route",
    )
    assert_equal(
        _runtime_value(second_response),
        "second-browser-client",
        "second colliding browser response route",
    )
    assert_equal(
        first_response.get("sessionId"),
        attached.first_session_id,
        "first browser response session route",
    )
    assert_equal(
        second_response.get("sessionId"),
        attached.second_session_id,
        "second browser response session route",
    )
    _reject_foreign_session_output(
        first_seen,
        attached.second_session_id,
        "first browser evaluate",
    )
    _reject_foreign_session_output(
        second_seen,
        attached.first_session_id,
        "second browser evaluate",
    )

    write_id = await first.send(
        "Runtime.evaluate",
        {
            "expression": "globalThis.__moliMultiClientState = 41",
            "returnByValue": True,
        },
        session_id=attached.first_session_id,
    )
    write, _ = await first.recv_until_id(write_id, timeout=5)
    assert_equal(_runtime_value(write), 41, "first browser shared-target write")

    read_id = await second.send(
        "Runtime.evaluate",
        {
            "expression": "globalThis.__moliMultiClientState",
            "returnByValue": True,
        },
        session_id=attached.second_session_id,
    )
    read, _ = await second.recv_until_id(read_id, timeout=5)
    assert_equal(_runtime_value(read), 41, "second browser shared-target read")

    foreign_id = await second.send(
        "Runtime.evaluate",
        {
            "expression": "globalThis.__moliForeignSessionRan = true",
            "returnByValue": True,
        },
        session_id=attached.first_session_id,
    )
    foreign, _ = await _recv_until_id_allow_error(second, foreign_id, timeout=5)
    assert_equal(
        foreign.get("error", {}).get("code"),
        -32001,
        "foreign flattened session rejection code",
    )

    foreign_detach_id = await second.send(
        "Target.detachFromTarget",
        {"sessionId": attached.first_session_id},
    )
    foreign_detach, _ = await _recv_until_id_allow_error(
        second,
        foreign_detach_id,
        timeout=5,
    )
    assert_equal(
        foreign_detach.get("error", {}).get("code"),
        -32602,
        "foreign legacy session rejection code",
    )

    mutation_probe_id = await second.send(
        "Runtime.evaluate",
        {
            "expression": "typeof globalThis.__moliForeignSessionRan",
            "returnByValue": True,
        },
        session_id=attached.second_session_id,
    )
    mutation_probe, _ = await second.recv_until_id(mutation_probe_id, timeout=5)
    assert_equal(
        _runtime_value(mutation_probe),
        "undefined",
        "foreign session command did not execute",
    )
    record(
        results,
        "raw_cdp_concurrent_browser_clients",
        {
            "clients": 2,
            "sameTarget": True,
            "collidingCommandId": collision_id,
            "discoveryStateIsolated": True,
            "attachEventBeforeResponse": True,
            "foreignSessionRejected": True,
        },
    )


async def _direct_page_client_isolation(
    websocket_url: str,
    results: list[dict[str, Any]],
) -> None:
    first, second = await asyncio.gather(
        connect_raw_cdp_websocket(websocket_url),
        connect_raw_cdp_websocket(websocket_url),
    )
    try:
        first_id, second_id = await asyncio.gather(
            first.send(
                "Runtime.evaluate",
                {"expression": "'first-page-client'", "returnByValue": True},
            ),
            second.send(
                "Runtime.evaluate",
                {"expression": "'second-page-client'", "returnByValue": True},
            ),
        )
        assert_equal(first_id, second_id, "direct-page command id collision")
        (first_response, _), (second_response, _) = await asyncio.gather(
            first.recv_until_id(first_id, timeout=5),
            second.recv_until_id(second_id, timeout=5),
        )
        assert_equal(
            _runtime_value(first_response),
            "first-page-client",
            "first direct-page response route",
        )
        assert_equal(
            _runtime_value(second_response),
            "second-page-client",
            "second direct-page response route",
        )
        if "sessionId" in first_response or "sessionId" in second_response:
            raise SmokeError(
                "a direct-page response leaked its private flattened session: "
                f"first={first_response}, second={second_response}"
            )

        write_id = await first.send(
            "Runtime.evaluate",
            {
                "expression": "globalThis.__moliDirectPageClientState = 41",
                "returnByValue": True,
            },
        )
        write, _ = await first.recv_until_id(write_id, timeout=5)
        assert_equal(_runtime_value(write), 41, "first direct-page shared-target write")

        read_id = await second.send(
            "Runtime.evaluate",
            {
                "expression": "globalThis.__moliDirectPageClientState",
                "returnByValue": True,
            },
        )
        read, _ = await second.recv_until_id(read_id, timeout=5)
        assert_equal(_runtime_value(read), 41, "second direct-page shared-target read")

        await first.websocket.close()
        surviving_id = await second.send(
            "Runtime.evaluate",
            {
                "expression": "globalThis.__moliDirectPageClientState + 1",
                "returnByValue": True,
            },
        )
        surviving, _ = await second.recv_until_id(surviving_id, timeout=5)
        assert_equal(
            _runtime_value(surviving),
            42,
            "surviving direct-page client after peer disconnect",
        )
        record(
            results,
            "raw_cdp_concurrent_page_clients",
            {"clients": 2, "sameTarget": True, "peerDisconnectSurvived": True},
        )
    finally:
        await asyncio.gather(
            first.websocket.close(),
            second.websocket.close(),
            return_exceptions=True,
        )


async def _surviving_browser_client(
    client: RawCdpClient,
    attached: AttachedTarget,
    results: list[dict[str, Any]],
) -> None:
    evaluate_id = await client.send(
        "Runtime.evaluate",
        {
            "expression": "globalThis.__moliMultiClientState + 1",
            "returnByValue": True,
        },
        session_id=attached.second_session_id,
    )
    evaluate, seen = await client.recv_until_id(evaluate_id, timeout=5)
    assert_equal(
        _runtime_value(evaluate),
        42,
        "surviving browser session after peer disconnect",
    )
    _reject_foreign_session_output(
        seen,
        attached.first_session_id,
        "surviving browser after peer disconnect",
    )

    version_id = await client.send("Browser.getVersion")
    version, _ = await client.recv_until_id(version_id, timeout=5)
    product = version.get("result", {}).get("product")
    if not isinstance(product, str) or not product:
        raise SmokeError(
            f"surviving browser root session returned no product: {version}"
        )
    record(
        results,
        "raw_cdp_browser_client_disconnect_isolation",
        {"peerSessionSurvived": True, "rootSessionSurvived": True},
    )
