from __future__ import annotations

import asyncio
import logging
from typing import Any

from ..assertions import SmokeError, assert_equal
from ..raw_cdp import RawCdpClient

LOGGER = logging.getLogger(__name__)


async def _create_browser_context(client: RawCdpClient) -> str:
    create_context_id = await client.send("Target.createBrowserContext")
    create_context, _ = await client.recv_until_id(create_context_id, timeout=5)
    browser_context_id = create_context.get("result", {}).get("browserContextId")
    if not isinstance(browser_context_id, str) or not browser_context_id:
        raise SmokeError(
            f"multi-client setup returned no browserContextId: {create_context}"
        )
    return browser_context_id


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
