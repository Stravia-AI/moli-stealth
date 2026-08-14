from __future__ import annotations

import asyncio
from typing import Any

from ..assertions import SmokeError, assert_equal, record
from ..raw_cdp import (
    RawCdpClient,
    connect_raw_cdp,
    connect_raw_cdp_websocket,
    discover_target_websocket_url,
)
from .multi_client_support import (
    _align_client_command_ids,
    _assert_attach_event_precedes_response,
    _assert_wire_session,
    _create_browser_context,
    _dispose_browser_context,
    _find_target_event,
    _reject_foreign_session_output,
    _required_session_id,
    _runtime_value,
)

ORDERED_BURST_SIZE = 4


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
