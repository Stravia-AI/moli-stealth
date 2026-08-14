from __future__ import annotations

import asyncio
import logging
from dataclasses import dataclass
from typing import Any

from ..assertions import SmokeError, assert_equal, record
from ..raw_cdp import (
    RawCdpClient,
    connect_raw_cdp,
    connect_raw_cdp_websocket,
    discover_target_websocket_url,
)

LOGGER = logging.getLogger(__name__)
ORDERED_BURST_SIZE = 4


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


async def _create_browser_context(client: RawCdpClient) -> str:
    create_context_id = await client.send("Target.createBrowserContext")
    create_context, _ = await client.recv_until_id(create_context_id, timeout=5)
    browser_context_id = create_context.get("result", {}).get("browserContextId")
    if not isinstance(browser_context_id, str) or not browser_context_id:
        raise SmokeError(
            f"multi-client setup returned no browserContextId: {create_context}"
        )
    return browser_context_id


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


async def _run_fanout_case(
    endpoint: str,
    client_count: int,
    results: list[dict[str, Any]],
) -> None:
    clients: list[RawCdpClient] = []
    browser_context_id: str | None = None
    try:
        for _ in range(client_count):
            clients.append(await connect_raw_cdp(endpoint))
        await _fanout_root_command_collision(clients)

        discovery_clients = set(range(0, client_count, 2))
        await _enable_fanout_discovery(clients, discovery_clients)
        coordinator_index = client_count - 1
        coordinator = clients[coordinator_index]
        browser_context_id = await _create_browser_context(coordinator)
        target_id, create_seen = await _create_target(coordinator, browser_context_id)
        session_ids, attach_seen, attach_command_id = await _attach_fanout_clients(
            clients,
            target_id,
        )
        _assert_fanout_discovery_routes(
            target_id,
            discovery_clients,
            coordinator_index,
            create_seen,
            attach_seen,
        )

        burst_command_id = await _ordered_runtime_fanout(
            clients,
            session_ids,
            "browser",
            client_count,
        )
        page_websocket_url = await discover_target_websocket_url(endpoint, target_id)
        await _run_direct_page_fanout(page_websocket_url, client_count, results)

        disconnect_order = await _disconnect_fanout_peers(
            clients,
            session_ids,
            "browser",
        )
        survivor = clients[-1]
        version_id = await survivor.send("Browser.getVersion")
        version, _ = await survivor.recv_until_id(version_id, timeout=5)
        product = version.get("result", {}).get("product")
        if not isinstance(product, str) or not product:
            raise SmokeError(
                f"{client_count}-client browser survivor returned no product: {version}"
            )
        record(
            results,
            f"raw_cdp_browser_client_fanout_{client_count}",
            {
                "clients": client_count,
                "uniqueSessions": len(set(session_ids)),
                "discoverySubscribers": len(discovery_clients),
                "attachEventBeforeResponse": True,
                "attachCommandId": attach_command_id,
                "orderedBurstSize": ORDERED_BURST_SIZE,
                "burstCommandId": burst_command_id,
                "perClientOrder": True,
                "crossClientOrder": "unconstrained",
                "disconnectOrder": disconnect_order,
            },
        )
    finally:
        if browser_context_id is not None and clients:
            await _dispose_browser_context(
                browser_context_id,
                (clients[-1], *clients[:-1]),
            )
        await asyncio.gather(
            *(client.websocket.close() for client in clients),
            return_exceptions=True,
        )


async def _fanout_root_command_collision(clients: list[RawCdpClient]) -> None:
    command_ids = await asyncio.gather(
        *(client.send("Browser.getVersion") for client in clients)
    )
    if len(set(command_ids)) != 1:
        raise SmokeError(
            f"fan-out browser root command ids did not collide: {command_ids}"
        )
    responses = await asyncio.gather(
        *(
            client.recv_until_id(command_id, timeout=5)
            for client, command_id in zip(clients, command_ids, strict=True)
        )
    )
    for client_index, (response, _) in enumerate(responses):
        product = response.get("result", {}).get("product")
        if not isinstance(product, str) or not product:
            raise SmokeError(
                f"fan-out browser client {client_index} returned no product: {response}"
            )
        if "sessionId" in response:
            raise SmokeError(
                "fan-out browser response leaked its hidden base session: "
                f"client={client_index}, response={response}"
            )


async def _enable_fanout_discovery(
    clients: list[RawCdpClient],
    discovery_clients: set[int],
) -> None:
    subscribers = [clients[index] for index in sorted(discovery_clients)]
    command_ids = await asyncio.gather(
        *(
            client.send(
                "Target.setDiscoverTargets",
                {"discover": True, "filter": [{"type": "page"}]},
            )
            for client in subscribers
        )
    )
    await asyncio.gather(
        *(
            client.recv_until_id(command_id, timeout=5)
            for client, command_id in zip(subscribers, command_ids, strict=True)
        )
    )


async def _create_target(
    client: RawCdpClient,
    browser_context_id: str,
) -> tuple[str, list[dict[str, Any]]]:
    create_target_id = await client.send(
        "Target.createTarget",
        {"browserContextId": browser_context_id, "url": "about:blank"},
    )
    create_target, seen = await client.recv_until_id(create_target_id, timeout=5)
    target_id = create_target.get("result", {}).get("targetId")
    if not isinstance(target_id, str) or not target_id:
        raise SmokeError(f"fan-out setup returned no targetId: {create_target}")
    return target_id, seen


async def _attach_fanout_clients(
    clients: list[RawCdpClient],
    target_id: str,
) -> tuple[list[str], list[list[dict[str, Any]]], int]:
    collision_id = _align_client_command_ids(clients)
    command_ids = await asyncio.gather(
        *(
            client.send(
                "Target.attachToTarget",
                {"targetId": target_id, "flatten": True},
            )
            for client in clients
        )
    )
    if command_ids != [collision_id] * len(clients):
        raise SmokeError(
            f"fan-out attach command ids did not collide at {collision_id}: {command_ids}"
        )
    responses = await asyncio.gather(
        *(
            client.recv_until_id(command_id, timeout=5)
            for client, command_id in zip(clients, command_ids, strict=True)
        )
    )
    session_ids = [
        _required_session_id(response, f"fan-out browser attach {client_index}")
        for client_index, (response, _) in enumerate(responses)
    ]
    if len(set(session_ids)) != len(clients):
        raise SmokeError(f"fan-out browser sessions were not unique: {session_ids}")
    seen_by_client = [seen for _, seen in responses]
    for client_index, (session_id, seen) in enumerate(
        zip(session_ids, seen_by_client, strict=True)
    ):
        _assert_attach_event_precedes_response(
            seen,
            collision_id,
            session_id,
            f"fan-out browser attach {client_index}",
        )
        for foreign_session_id in session_ids:
            if foreign_session_id != session_id:
                _reject_foreign_session_output(
                    seen,
                    foreign_session_id,
                    f"fan-out browser attach {client_index}",
                )
    return session_ids, seen_by_client, collision_id


def _assert_fanout_discovery_routes(
    target_id: str,
    discovery_clients: set[int],
    coordinator_index: int,
    create_seen: list[dict[str, Any]],
    attach_seen: list[list[dict[str, Any]]],
) -> None:
    for client_index, attach_messages in enumerate(attach_seen):
        messages = list(attach_messages)
        if client_index == coordinator_index:
            messages = [*create_seen, *messages]
        event = _find_target_event(messages, "Target.targetCreated", target_id)
        if client_index in discovery_clients:
            if event is None:
                raise SmokeError(
                    f"fan-out discovery client {client_index} missed Target.targetCreated"
                )
            if "sessionId" in event:
                raise SmokeError(
                    "fan-out target discovery leaked a hidden base session: "
                    f"client={client_index}, event={event}"
                )
            attached_event = _find_target_event(
                messages,
                "Target.attachedToTarget",
                target_id,
            )
            if attached_event is None or messages.index(event) >= messages.index(
                attached_event
            ):
                raise SmokeError(
                    "fan-out target discovery did not precede attachment: "
                    f"client={client_index}, messages={messages}"
                )
        elif event is not None:
            raise SmokeError(
                f"fan-out discovery state leaked to client {client_index}: {event}"
            )


async def _ordered_runtime_fanout(
    clients: list[RawCdpClient],
    session_ids: list[str | None],
    client_kind: str,
    client_count: int,
) -> int:
    burst_start_id = _align_client_command_ids(clients)
    state_name = f"__moli{client_kind.title()}FanoutOrder{client_count}"
    sent_ids = await asyncio.gather(
        *(
            _send_ordered_runtime_burst(
                client,
                session_id,
                client_kind,
                client_count,
                client_index,
                state_name,
            )
            for client_index, (client, session_id) in enumerate(
                zip(clients, session_ids, strict=True)
            )
        )
    )
    expected_ids = list(range(burst_start_id, burst_start_id + ORDERED_BURST_SIZE))
    for client_index, client_sent_ids in enumerate(sent_ids):
        assert_equal(
            client_sent_ids,
            expected_ids,
            f"{client_kind} fan-out client {client_index} sent command order",
        )
    received_messages = await asyncio.gather(
        *(
            _receive_ordered_runtime_burst(
                client,
                session_id,
                client_kind,
                client_count,
                client_index,
                expected_ids,
            )
            for client_index, (client, session_id) in enumerate(
                zip(clients, session_ids, strict=True)
            )
        )
    )
    for client_index, messages in enumerate(received_messages):
        own_session_id = session_ids[client_index]
        if own_session_id is None:
            continue
        for foreign_session_id in session_ids:
            if foreign_session_id is not None and foreign_session_id != own_session_id:
                _reject_foreign_session_output(
                    messages,
                    foreign_session_id,
                    f"{client_kind} fan-out burst {client_index}",
                )
    await _assert_shared_runtime_order(
        clients[0],
        session_ids[0],
        client_kind,
        client_count,
        state_name,
    )
    return burst_start_id


async def _send_ordered_runtime_burst(
    client: RawCdpClient,
    session_id: str | None,
    client_kind: str,
    client_count: int,
    client_index: int,
    state_name: str,
) -> list[int]:
    command_ids: list[int] = []
    for sequence in range(ORDERED_BURST_SIZE):
        token = f"{client_kind}-{client_count}-{client_index}-{sequence}"
        expression = (
            "(() => {"
            f"globalThis.{state_name} ??= [];"
            f"globalThis.{state_name}.push('{token}');"
            f"return {{client: {client_index}, sequence: {sequence}}};"
            "})()"
        )
        command_ids.append(
            await client.send(
                "Runtime.evaluate",
                {"expression": expression, "returnByValue": True},
                session_id=session_id,
            )
        )
    return command_ids


async def _receive_ordered_runtime_burst(
    client: RawCdpClient,
    session_id: str | None,
    client_kind: str,
    client_count: int,
    client_index: int,
    expected_ids: list[int],
) -> list[dict[str, Any]]:
    expected_id_set = set(expected_ids)
    response_ids: list[int] = []
    messages: list[dict[str, Any]] = []
    deadline = asyncio.get_running_loop().time() + 10
    while len(response_ids) < len(expected_ids):
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            raise SmokeError(
                f"timed out waiting for {client_kind} fan-out client {client_index} "
                f"ordered responses; ids={response_ids}, messages={messages[-20:]}"
            )
        message = await asyncio.wait_for(client.recv(), timeout=remaining)
        messages.append(message)
        message_id = message.get("id")
        if message_id not in expected_id_set:
            continue
        if message_id in response_ids:
            raise SmokeError(
                f"{client_kind} fan-out client {client_index} received duplicate "
                f"response id {message_id}: {message}"
            )
        if "error" in message:
            raise SmokeError(
                f"{client_kind} fan-out client {client_index} command "
                f"{message_id} failed: {message['error']}"
            )
        response_ids.append(message_id)
        _assert_wire_session(
            message,
            session_id,
            f"{client_kind} fan-out client {client_index} response {message_id}",
        )
        sequence = expected_ids.index(message_id)
        assert_equal(
            _runtime_value(message),
            {"client": client_index, "sequence": sequence},
            f"{client_kind} fan-out client {client_index} response payload",
        )
    assert_equal(
        response_ids,
        expected_ids,
        f"{client_kind} fan-out client {client_index} response order",
    )
    return messages


async def _assert_shared_runtime_order(
    client: RawCdpClient,
    session_id: str | None,
    client_kind: str,
    client_count: int,
    state_name: str,
) -> None:
    read_id = await client.send(
        "Runtime.evaluate",
        {"expression": f"globalThis.{state_name}", "returnByValue": True},
        session_id=session_id,
    )
    response, _ = await client.recv_until_id(read_id, timeout=5)
    observed = _runtime_value(response)
    expected = [
        f"{client_kind}-{client_count}-{client_index}-{sequence}"
        for client_index in range(client_count)
        for sequence in range(ORDERED_BURST_SIZE)
    ]
    if not isinstance(observed, list):
        raise SmokeError(
            f"{client_kind} fan-out shared order was not an array: {observed!r}"
        )
    if len(observed) != len(expected) or set(observed) != set(expected):
        raise SmokeError(
            f"{client_kind} fan-out shared order was incomplete: {observed!r}"
        )
    positions = {token: position for position, token in enumerate(observed)}
    for client_index in range(client_count):
        client_positions = [
            positions[f"{client_kind}-{client_count}-{client_index}-{sequence}"]
            for sequence in range(ORDERED_BURST_SIZE)
        ]
        if client_positions != sorted(client_positions):
            raise SmokeError(
                f"{client_kind} fan-out reordered client {client_index}: "
                f"positions={client_positions}, observed={observed}"
            )


async def _run_direct_page_fanout(
    websocket_url: str,
    client_count: int,
    results: list[dict[str, Any]],
) -> None:
    clients: list[RawCdpClient] = []
    try:
        for _ in range(client_count):
            clients.append(await connect_raw_cdp_websocket(websocket_url))
        burst_command_id = await _ordered_runtime_fanout(
            clients,
            [None] * client_count,
            "page",
            client_count,
        )
        disconnect_order = await _disconnect_fanout_peers(
            clients,
            [None] * client_count,
            "page",
        )
        record(
            results,
            f"raw_cdp_page_client_fanout_{client_count}",
            {
                "clients": client_count,
                "orderedBurstSize": ORDERED_BURST_SIZE,
                "burstCommandId": burst_command_id,
                "perClientOrder": True,
                "crossClientOrder": "unconstrained",
                "disconnectOrder": disconnect_order,
            },
        )
    finally:
        await asyncio.gather(
            *(client.websocket.close() for client in clients),
            return_exceptions=True,
        )


async def _disconnect_fanout_peers(
    clients: list[RawCdpClient],
    session_ids: list[str | None],
    client_kind: str,
) -> list[int]:
    disconnect_order = [
        *range(0, len(clients) - 1, 2),
        *range(1, len(clients) - 1, 2),
    ]
    survivor = clients[-1]
    survivor_session_id = session_ids[-1]
    closed_session_ids: list[str] = []
    for sequence, client_index in enumerate(disconnect_order):
        await clients[client_index].websocket.close()
        closed_session_id = session_ids[client_index]
        if closed_session_id is not None:
            closed_session_ids.append(closed_session_id)
        token = f"{client_kind}-survivor-{sequence}"
        probe_id = await survivor.send(
            "Runtime.evaluate",
            {"expression": f"'{token}'", "returnByValue": True},
            session_id=survivor_session_id,
        )
        probe, seen = await survivor.recv_until_id(probe_id, timeout=5)
        assert_equal(
            _runtime_value(probe),
            token,
            f"{client_kind} fan-out survivor after disconnect {client_index}",
        )
        _assert_wire_session(
            probe,
            survivor_session_id,
            f"{client_kind} fan-out survivor response",
        )
        for foreign_session_id in closed_session_ids:
            _reject_foreign_session_output(
                seen,
                foreign_session_id,
                f"{client_kind} fan-out survivor after disconnect {client_index}",
            )
        if client_kind == "browser":
            for message in seen:
                if message.get("method") == "Target.detachedFromTarget":
                    raise SmokeError(
                        "browser fan-out survivor received another client's detach: "
                        f"closed={client_index}, message={message}"
                    )
    return disconnect_order


def _align_next_command_id(first: RawCdpClient, second: RawCdpClient) -> int:
    return _align_client_command_ids([first, second])


def _align_client_command_ids(clients: list[RawCdpClient]) -> int:
    command_id = max(client.next_id for client in clients)
    for client in clients:
        client.next_id = command_id
    return command_id


def _required_session_id(response: dict[str, Any], label: str) -> str:
    session_id = response.get("result", {}).get("sessionId")
    if not isinstance(session_id, str) or not session_id:
        raise SmokeError(f"{label} returned no sessionId: {response}")
    if "sessionId" in response:
        raise SmokeError(f"{label} leaked its hidden base session: {response}")
    return session_id


def _assert_attach_event_precedes_response(
    messages: list[dict[str, Any]],
    command_id: int,
    session_id: str,
    label: str,
) -> None:
    response_index: int | None = None
    event_index: int | None = None
    event: dict[str, Any] | None = None
    for index, message in enumerate(messages):
        if message.get("id") == command_id:
            response_index = index
        if (
            message.get("method") == "Target.attachedToTarget"
            and message.get("params", {}).get("sessionId") == session_id
        ):
            event_index = index
            event = message
    if event_index is None or response_index is None:
        raise SmokeError(
            f"{label} did not include its attach event and response: {messages}"
        )
    if event_index >= response_index:
        raise SmokeError(f"{label} delivered response before attach event: {messages}")
    if event is not None and "sessionId" in event:
        raise SmokeError(f"{label} attach event leaked a hidden base session: {event}")


def _assert_wire_session(
    message: dict[str, Any],
    expected_session_id: str | None,
    label: str,
) -> None:
    if expected_session_id is None:
        if "sessionId" in message:
            raise SmokeError(f"{label} leaked a private page session: {message}")
        return
    assert_equal(
        message.get("sessionId"),
        expected_session_id,
        f"{label} session route",
    )


def _runtime_value(response: dict[str, Any]) -> Any:
    return response.get("result", {}).get("result", {}).get("value")


def _find_target_event(
    messages: list[dict[str, Any]],
    method: str,
    target_id: str,
) -> dict[str, Any] | None:
    for message in messages:
        if message.get("method") != method:
            continue
        event_target_id = (
            message.get("params", {}).get("targetInfo", {}).get("targetId")
        )
        if event_target_id == target_id:
            return message
    return None


def _reject_foreign_session_output(
    messages: list[dict[str, Any]],
    foreign_session_id: str,
    label: str,
) -> None:
    for message in messages:
        params = message.get("params")
        if message.get("sessionId") == foreign_session_id or (
            isinstance(params, dict) and params.get("sessionId") == foreign_session_id
        ):
            raise SmokeError(
                f"{label} received output for another client's session: {message}"
            )


async def _recv_until_id_allow_error(
    client: RawCdpClient,
    message_id: int,
    *,
    timeout: float,
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    deadline = asyncio.get_running_loop().time() + timeout
    seen: list[dict[str, Any]] = []
    while True:
        remaining = deadline - asyncio.get_running_loop().time()
        if remaining <= 0:
            raise SmokeError(
                f"timed out waiting for CDP response id={message_id}; seen={seen[-20:]}"
            )
        message = await asyncio.wait_for(client.recv(), timeout=remaining)
        seen.append(message)
        if message.get("id") == message_id:
            return message, seen


async def _dispose_browser_context(
    browser_context_id: str,
    clients: tuple[RawCdpClient | None, ...],
) -> None:
    for client in clients:
        if client is None:
            continue
        try:
            dispose_id = await client.send(
                "Target.disposeBrowserContext",
                {"browserContextId": browser_context_id},
            )
            await client.recv_until_id(dispose_id, timeout=3)
            return
        except Exception:
            # Best-effort cleanup must not mask the original smoke failure.
            LOGGER.debug(
                "failed to dispose multi-client smoke browser context",
                exc_info=True,
            )
