"""真实浏览器与 Moli 共用的本地传输观测场景。"""
from __future__ import annotations

import argparse
import asyncio
import base64
import json
from contextlib import AsyncExitStack
from pathlib import Path
from typing import Any

from .raw_cdp import RawCdpClient, connect_raw_cdp
from .transport_fixture import (
    ConnectProxyFixture,
    TransportFixture,
    _offered_psks,
    _trust_anchor_ids,
)


class CaptureFailure(RuntimeError):
    def __init__(self, report: dict[str, Any], error: Exception):
        super().__init__(str(error))
        self.report = report
        self.report["error"] = f"{type(error).__name__}: {error}"


async def _command(client: RawCdpClient, method: str, params: dict[str, Any] | None = None,
                   session: str | None = None) -> dict[str, Any]:
    request = await client.send(method, params, session_id=session)
    response, _ = await client.recv_until_id(request, timeout=30)
    return response["result"]


async def _evaluate(client: RawCdpClient, session: str, expression: str) -> Any:
    result = await _command(client, "Runtime.evaluate", {
        "expression": expression, "awaitPromise": True, "returnByValue": True,
    }, session)
    if "exceptionDetails" in result:
        raise AssertionError(f"browser transport scenario failed: {result['exceptionDetails']}")
    return result["result"].get("value")


async def capture(endpoint: str, *, repeats: int = 3, openssl: str = "openssl",
                  proxy: bool = False) -> dict[str, Any]:
    client = await connect_raw_cdp(endpoint)
    observations = []
    report: dict[str, Any] = {
        "repeats": repeats, "proxy": proxy, "observations": observations,
        "fixture": "transport_fixture.py: Python SSL MemoryBIO, raw ClientHello and decrypted HTTP/2 frame/HPACK capture",
        "security": "isolated local self-signed endpoint; certificate errors explicitly ignored for this capture only",
    }
    try:
        version = await _command(client, "Browser.getVersion")
        report["browser"] = version
        await _command(client, "Security.setIgnoreCertificateErrors", {"ignore": True})
        for repetition in range(repeats):
            async with AsyncExitStack() as stack:
                fixture = await stack.enter_async_context(TransportFixture(openssl))
                observation: dict[str, Any] = {"repetition": repetition, "connections": fixture.connections}
                observations.append(observation)
                target_params = {"url": "about:blank"}
                proxy_fixture = None
                context_id = None
                if proxy:
                    proxy_fixture = await stack.enter_async_context(ConnectProxyFixture(fixture.port))
                    context = await _command(client, "Target.createBrowserContext", {
                        "proxyServer": f"http://127.0.0.1:{proxy_fixture.port}",
                        "proxyBypassList": "<-loopback>",
                    })
                    context_id = context["browserContextId"]
                    target_params["browserContextId"] = context_id
                target = await _command(client, "Target.createTarget", target_params)
                target_id = target["targetId"]
                try:
                    attached = await _command(client, "Target.attachToTarget", {"targetId": target_id, "flatten": True})
                    session = attached["sessionId"]
                    await _command(client, "Page.enable", session=session)
                    await _command(client, "Runtime.enable", session=session)
                    navigation_id = await client.send("Page.navigate", {"url": fixture.url + "/page"}, session_id=session)
                    response, seen = await client.recv_until_id(navigation_id, timeout=30)
                    if response["result"].get("errorText"):
                        raise AssertionError(response["result"])
                    loaded = any(event.get("method") == "Page.loadEventFired" and event.get("sessionId") == session for event in seen)
                    async with asyncio.timeout(30):
                        while not loaded:
                            event = await client.recv()
                            loaded = event.get("method") == "Page.loadEventFired" and event.get("sessionId") == session
                    identity = await _evaluate(client, session, "(async()=>({userAgent:navigator.userAgent,platform:navigator.platform,languages:navigator.languages,metadata:navigator.userAgentData?await navigator.userAgentData.getHighEntropyValues(['architecture','bitness','model','platformVersion','uaFullVersion','fullVersionList','wow64']):null}))()")
                    data = await _evaluate(client, session, "(async()=>{const results=[]; for(const [method,path] of [['GET','/first'],['GET','/reused'],['PATCH','/binary'],['HEAD','/head']]) {const response=await fetch(path,{method,body:method==='PATCH'?new Uint8Array([0,255,128,1]):undefined}); results.push({method,path,status:response.status,bytes:[...new Uint8Array(await response.arrayBuffer())]});} return results;})()")
                    for result in data:
                        expected = [] if result["method"] == "HEAD" else [0, 255, 1, 128] + list(result["path"].encode())
                        if result["status"] != 200 or result["bytes"] != expected:
                            raise AssertionError(f"binary method contract failed: {result}")
                    websocket = await _evaluate(client, session, "new Promise((resolve,reject)=>{const events=[];const payloads=['transport-echo',new Uint8Array([0,255,128,1]),'transport-echo'.repeat(200)];let index=0;const socket=new WebSocket('wss://'+location.host+'/socket','probe');socket.binaryType='arraybuffer';socket.onopen=()=>{events.push('open:'+socket.protocol);socket.send(payloads[index]);};socket.onmessage=e=>{events.push(typeof e.data==='string'?'text:'+e.data:'binary:'+JSON.stringify([...new Uint8Array(e.data)]));if(++index<payloads.length)socket.send(payloads[index]);else socket.close(1000,'done');};socket.onerror=()=>reject(new Error('WebSocket failed'));socket.onclose=e=>{events.push('close:'+e.code+':'+e.wasClean);resolve({events,extensions:socket.extensions});};})")
                    if websocket != {"events": ["open:probe", "text:transport-echo",
                                                "binary:[0,255,128,1]", "text:" + "transport-echo" * 200,
                                                "close:1000:true"], "extensions": "permessage-deflate"}:
                        raise AssertionError(f"WebSocket contract failed: {websocket}")
                    if fixture.errors:
                        raise AssertionError(f"protocol fixture failed: {fixture.errors}")
                    exchanges = [connection for connection in fixture.connections if connection["requests"]]
                    uploads = [request.get("body_base64") for connection in exchanges
                               for request in connection["requests"] if request["path"] == "/binary"]
                    if uploads != ["AP+AAQ=="]:
                        raise AssertionError(f"binary upload bytes changed on the wire: {uploads}")
                    reused = any({"/first", "/reused"}.issubset({request["path"] for request in connection["requests"]}) for connection in exchanges)
                    if not reused:
                        raise AssertionError("sequential HTTP requests did not reuse a connection")
                    if proxy_fixture:
                        ports = {tunnel["source_port"] for tunnel in proxy_fixture.tunnels}
                        if proxy_fixture.errors or any(connection["peer_port"] not in ports for connection in exchanges):
                            raise AssertionError(f"HTTPS/WSS bypassed CONNECT: {proxy_fixture.errors}")
                    observation.update({"identity": identity, "responses": data, "websocket": websocket,
                                        "proxy_tunnels": proxy_fixture.tunnels if proxy_fixture else [],
                                        "completed": True})
                finally:
                    await _command(client, "Target.closeTarget", {"targetId": target_id})
                    if context_id:
                        await _command(client, "Target.disposeBrowserContext", {"browserContextId": context_id})
        return report
    except Exception as error:
        raise CaptureFailure(report, error) from error
    finally:
        await client.websocket.close()


def _raw_client_hello_extension(hello: dict[str, Any], wanted: int) -> bytes:
    handshake = base64.b64decode(hello["raw_client_hello_base64"])
    if len(handshake) < 4 or handshake[0] != 1:
        raise ValueError("expected raw ClientHello handshake")
    size = int.from_bytes(handshake[1:4], "big")
    if len(handshake) != size + 4:
        raise ValueError("malformed raw ClientHello handshake")
    data = handshake[4:]
    position = 34
    position += 1 + data[position]
    cipher_size = int.from_bytes(data[position:position + 2], "big")
    position += 2 + cipher_size
    position += 1 + data[position]
    extensions_end = position + 2 + int.from_bytes(data[position:position + 2], "big")
    position += 2
    if extensions_end != len(data):
        raise ValueError("malformed raw ClientHello extension vector")
    while position < extensions_end:
        kind = int.from_bytes(data[position:position + 2], "big")
        extension_size = int.from_bytes(data[position + 2:position + 4], "big")
        position += 4
        body = data[position:position + extension_size]
        position += extension_size
        if position > extensions_end:
            raise ValueError("malformed raw ClientHello extension")
        if kind == wanted:
            return body
    raise ValueError(f"raw ClientHello lacks extension {wanted}")


def _hello_semantics(hello: dict[str, Any]) -> dict[str, Any]:
    extensions = []
    for original in hello["extensions"]:
        extension = dict(original)
        kind = extension["type"]
        if kind in ("GREASE", 0, 21, 65037):
            extension.pop("length", None)
        elif kind == 41:
            offered = extension.get("offered_psks") or _offered_psks(
                _raw_client_hello_extension(hello, 41))
            # The accepted identity is an opaque ticket emitted by this fixture's TLS
            # server. Its bytes, age mask, and encoded length vary in repeated Chrome
            # captures; the offer count and binder hash lengths remain client structure.
            extension = {"type": kind, "offered_psks": {
                "identity_count": len(offered["identities"]),
                "binder_lengths": offered["binder_lengths"],
            }}
        elif kind == 51764:
            identities = extension.get("trust_anchor_ids") or _trust_anchor_ids(
                _raw_client_hello_extension(hello, 51764))
            extension.pop("hex", None)
            extension["trust_anchor_ids"] = sorted(identities)
        extensions.append(extension)
    return {key: hello[key] for key in ("legacy_version", "session_id_length", "ciphers", "compression")} | {
        "extensions": sorted(extensions, key=lambda item: str(item["type"]))}


_SCENARIO_PATHS = {"/page", "/first", "/reused", "/binary", "/head", "/socket"}


def _request_priorities(connection: dict[str, Any]) -> dict[str, Any]:
    priorities = {frame["stream"]: frame.get("priority")
                  for frame in connection["frames"] if frame["type"] == 1}
    return {request["path"]: priorities.get(request["stream"])
            for request in connection["requests"]
            if request["path"] in _SCENARIO_PATHS and "stream" in request}


def _request_header_order(connection: dict[str, Any]) -> dict[str, Any]:
    return {request["path"]: [name.lower() for name, _ in request["headers"]]
            for request in connection["requests"] if request["path"] in _SCENARIO_PATHS}


def _request_identity_headers(connection: dict[str, Any]) -> dict[str, Any]:
    return {request["path"]: [(name.lower(), value) for name, value in request["headers"]
                             if name.lower() in ("user-agent", "accept-language")
                             or name.lower().startswith("sec-ch-ua")]
            for request in connection["requests"] if request["path"] in _SCENARIO_PATHS}


def compare(reference: dict[str, Any], candidate: dict[str, Any]) -> list[dict[str, Any]]:
    """比较语义特征，原始有序观测仍保留在报告中供审查。"""
    differences = []
    for purpose in ("http", "websocket"):
        def selected(capture: dict[str, Any]) -> list[dict[str, Any]]:
            return [connection for observation in capture["observations"] for connection in observation["connections"]
                    if connection["requests"] and (any(request["path"] == "/socket" for request in connection["requests"]) == (purpose == "websocket"))]
        expected, actual = selected(reference), selected(candidate)
        if not expected or not actual:
            differences.append({"purpose": purpose, "field": "connections", "reference": len(expected), "candidate": len(actual)})
            continue
        for field, extractor in (
            ("client_hello", lambda item: _hello_semantics(item["client_hello"])),
            ("alpn", lambda item: item["alpn"]),
            ("tls_cipher", lambda item: list(item["cipher"])),
            ("tls_session_reused", lambda item: item.get("session_reused")),
            ("h2_settings", lambda item: [frame["settings"] for frame in item["frames"] if "settings" in frame]),
            ("h2_connection_window", lambda item: [frame["increment"] for frame in item["frames"] if frame["type"] == 8 and frame["stream"] == 0]),
            ("h2_priority", _request_priorities),
            ("request_header_order", _request_header_order),
            ("identity_headers", _request_identity_headers),
        ):
            baseline = [extractor(item) for item in expected]
            for index, connection in enumerate(actual):
                observed = extractor(connection)
                if observed not in baseline:
                    differences.append({"purpose": purpose, "connection": index, "field": field,
                                        "reference": baseline, "candidate": observed})
    baseline_identities = [observation["identity"] for observation in reference["observations"]]
    for observation in candidate["observations"]:
        if observation["identity"] not in baseline_identities:
            differences.append({"field": "javascript_identity",
                                "repetition": observation["repetition"],
                                "reference": baseline_identities,
                                "candidate": observation["identity"]})
    return differences


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--reference", type=Path)
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--openssl", default="openssl")
    parser.add_argument("--proxy", action="store_true", help="exercise HTTPS and WSS through the same local CONNECT proxy")
    args = parser.parse_args()
    if args.repeats < 1:
        parser.error("--repeats must be positive")
    try:
        report = asyncio.run(capture(args.endpoint, repeats=args.repeats, openssl=args.openssl, proxy=args.proxy))
    except CaptureFailure as error:
        report = error.report
    if args.reference and "error" not in report:
        report["differences"] = compare(json.loads(args.reference.read_text(encoding="utf-8")), report)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"browser": report.get("browser"), "repeats": args.repeats,
                      "output": str(args.output), "error": report.get("error"),
                      "differences": len(report["differences"]) if "differences" in report else None}, ensure_ascii=False))
    if report.get("differences") or report.get("error"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
