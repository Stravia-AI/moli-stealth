from __future__ import annotations

import http.cookiejar
import json
import threading
import urllib.error
import urllib.request
from pathlib import Path
from types import SimpleNamespace
from urllib.parse import urljoin, urlsplit

from moli_frontend_smoke.fixture_server import (
    FixtureServer,
    _handle_websocket_connection,
    _RealtimeState,
)
from websockets.sync.client import connect


def _open_without_proxy(url: str) -> tuple[str, bytes]:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(url, timeout=2) as response:
        return response.geturl(), response.read()


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: object,
        code: int,
        msg: str,
        headers: object,
        newurl: str,
    ) -> None:
        return None


def _redirect_response(request: urllib.request.Request) -> urllib.error.HTTPError:
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}),
        _NoRedirect(),
    )
    try:
        opener.open(request, timeout=2)
    except urllib.error.HTTPError as error:
        return error
    raise AssertionError("expected a redirect response")


def test_alternate_origin_route_toggles_between_ipv4_and_localhost(tmp_path: Path) -> None:
    support = tmp_path / "support"
    support.mkdir()
    (support / "boundary-frame.html").write_text("boundary fixture", encoding="utf-8")
    server = FixtureServer(tmp_path)
    server.start()
    try:
        ipv4_result, ipv4_body = _open_without_proxy(
            f"{server.url}/support/alternate-origin-frame?scenario=ipv4-parent"
        )
        ipv4_url = urlsplit(ipv4_result)
        assert ipv4_url.hostname == "localhost"
        assert ipv4_url.path == "/support/boundary-frame.html"
        assert ipv4_url.query == "scenario=ipv4-parent"
        assert ipv4_body == b"boundary fixture"

        localhost_url = server.url.replace("127.0.0.1", "localhost")
        localhost_result, localhost_body = _open_without_proxy(
            f"{localhost_url}/support/alternate-origin-frame?scenario=localhost-parent"
        )
        parsed_localhost_result = urlsplit(localhost_result)
        assert parsed_localhost_result.hostname == "127.0.0.1"
        assert parsed_localhost_result.path == "/support/boundary-frame.html"
        assert parsed_localhost_result.query == "scenario=localhost-parent"
        assert localhost_body == b"boundary fixture"
    finally:
        server.stop()


def test_network_redirect_routes_record_each_method_and_body(tmp_path: Path) -> None:
    server = FixtureServer(tmp_path)
    server.start()
    try:
        first = _redirect_response(
            urllib.request.Request(
                f"{server.url}/support/network/redirect-307?token=redirect-case",
                data=b"stage=created",
                method="POST",
            )
        )
        assert first.code == 307
        middle_url = urljoin(server.url, first.headers["Location"])

        middle = _redirect_response(
            urllib.request.Request(middle_url, data=b"stage=created", method="POST")
        )
        assert middle.code == 303
        final_url = urljoin(server.url, middle.headers["Location"])
        _url, body = _open_without_proxy(final_url)
        result = json.loads(body)

        assert result == {
            "finalBody": "",
            "finalMethod": "GET",
            "firstBody": "stage=created",
            "firstMethod": "POST",
            "middleBody": "stage=created",
            "middleMethod": "POST",
            "token": "redirect-case",
            "trace": "",
        }
    finally:
        server.stop()


def test_network_payload_cookie_and_gated_response_routes(tmp_path: Path) -> None:
    server = FixtureServer(tmp_path)
    server.start()
    reader_done = threading.Event()
    chunks: list[bytes] = []

    def read_gated_response() -> None:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(
            f"{server.url}/support/network/gated-response?token=gated-case",
            timeout=5,
        ) as response:
            chunks.append(response.readline())
            reader_done.set()
            chunks.append(response.readline())

    reader = threading.Thread(target=read_gated_response, daemon=True)
    try:
        _url, payload_body = _open_without_proxy(
            f"{server.url}/support/network/stream-payload?token=payload-case"
        )
        assert json.loads(payload_body) == {
            "items": ["alpha", "beta", "gamma"],
            "text": "café-東京",
            "token": "payload-case",
        }

        cookie_jar = http.cookiejar.CookieJar()
        cookie_opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(cookie_jar),
        )
        cookie_name = "fixture_http_only"
        cookie_opener.open(
            f"{server.url}/support/network/set-cookie?name={cookie_name}", timeout=2
        ).read()
        with cookie_opener.open(
            f"{server.url}/support/network/cookie-echo", timeout=2
        ) as response:
            assert cookie_name in json.loads(response.read())["cookieNames"]

        reader.start()
        assert reader_done.wait(timeout=2), "gated response should publish its first chunk"
        _url, release_body = _open_without_proxy(
            f"{server.url}/support/network/release-response?token=gated-case"
        )
        assert json.loads(release_body) == {"released": True, "token": "gated-case"}
        reader.join(timeout=2)
        assert not reader.is_alive()
        assert chunks == [b"first:gated-case\n", b"second:gated-case\n"]
    finally:
        server.stop()


def test_service_worker_script_and_network_fallback_routes(tmp_path: Path) -> None:
    server = FixtureServer(tmp_path)
    server.start()
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(
            f"{server.url}/support/service-worker/worker.js?token=sw-case&version=v7",
            timeout=2,
        ) as response:
            source = response.read().decode()
            assert response.headers["Service-Worker-Allowed"] == "/"
            assert response.headers.get_content_type() == "text/javascript"
        assert 'const TOKEN = "sw-case";' in source
        assert 'const VERSION = "v7";' in source
        assert 'self.addEventListener("install"' in source
        assert 'self.addEventListener("fetch"' in source
        assert 'self.addEventListener("message"' in source

        _url, fallback = _open_without_proxy(
            f"{server.url}/support/service-worker/fallback?token=sw-case"
        )
        assert fallback == b"network-fallback:sw-case"
    finally:
        server.stop()


def test_cors_preflight_records_and_consumes_request_metadata(tmp_path: Path) -> None:
    server = FixtureServer(tmp_path)
    server.start()
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        url = f"{server.url}/support/cors/preflight/allow?token=cors-case"
        preflight = urllib.request.Request(url, method="OPTIONS")
        preflight.add_header("Origin", "http://client.test")
        preflight.add_header("Access-Control-Request-Method", "PUT")
        preflight.add_header(
            "Access-Control-Request-Headers", "content-type, x-smoke-token"
        )
        with opener.open(preflight, timeout=2) as response:
            assert response.status == 204
            assert response.headers["Access-Control-Allow-Origin"] == "http://client.test"
            assert "PUT" in response.headers["Access-Control-Allow-Methods"]
            assert "x-smoke-token" in response.headers["Access-Control-Allow-Headers"]

        actual = urllib.request.Request(
            url,
            data=b'{"stage":"actual"}',
            headers={
                "Content-Type": "application/json",
                "Origin": "http://client.test",
                "X-Smoke-Token": "cors-case",
            },
            method="PUT",
        )
        with opener.open(actual, timeout=2) as response:
            result = json.loads(response.read())
            assert response.headers["Access-Control-Allow-Origin"] == "http://client.test"
            assert response.headers["X-Smoke-Actual"] == "allow"
        assert result == {
            "body": '{"stage":"actual"}',
            "header": "cors-case",
            "method": "PUT",
            "preflight": {
                "headers": "content-type, x-smoke-token",
                "method": "PUT",
                "origin": "http://client.test",
            },
            "token": "cors-case",
        }
    finally:
        server.stop()


def test_cors_header_and_credential_response_contracts(tmp_path: Path) -> None:
    server = FixtureServer(tmp_path)
    server.start()
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        exposed = urllib.request.Request(
            f"{server.url}/support/cors/exposed?token=headers-case",
            headers={"Origin": "http://client.test"},
        )
        with opener.open(exposed, timeout=2) as response:
            assert response.read() == b"cors-exposed"
            assert response.headers["Access-Control-Expose-Headers"] == "X-Smoke-Visible"
            assert response.headers["X-Smoke-Visible"] == "visible-headers-case"
            assert response.headers["X-Smoke-Hidden"] == "secret"

        credentials = urllib.request.Request(
            f"{server.url}/support/cors/credentials?token=cookie-case&set=1",
            headers={"Origin": "http://client.test"},
        )
        with opener.open(credentials, timeout=2) as response:
            result = json.loads(response.read())
            assert response.headers["Access-Control-Allow-Credentials"] == "true"
            assert "cors_cookie-case=present" in response.headers["Set-Cookie"]
        assert result == {
            "cookieNames": [],
            "origin": "http://client.test",
            "token": "cookie-case",
        }
    finally:
        server.stop()


def test_script_and_websocket_fixture_routes(tmp_path: Path) -> None:
    server = FixtureServer(tmp_path)
    server.start()
    try:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(
            f"{server.url}/support/scripts/classic.js?token=script-case&label=alpha",
            timeout=2,
        ) as response:
            source = response.read().decode()
            assert response.headers.get_content_type() == "text/javascript"
        assert 'const TOKEN = "script-case";' in source
        assert 'const LABEL = "alpha";' in source
        assert "__scriptLifecycleOrder" in source
        assert "document.currentScript?.dataset.scriptId" in source
        assert "marker.dataset.ownerTitle = document.title" in source

        _url, config_body = _open_without_proxy(
            f"{server.url}/support/realtime/config"
        )
        assert json.loads(config_body) == {"websocketUrl": server.websocket_url}

        socket_url = f"{server.websocket_url}?scenario=text-order&token=socket-case"
        with connect(socket_url, proxy=None) as websocket:
            assert websocket.recv() == "server-open:socket-case"
            websocket.send("alpha")
            assert websocket.recv() == "echo:1:alpha"
            websocket.send("beta")
            assert websocket.recv() == "echo:2:beta"

        protocol_url = f"{server.websocket_url}?scenario=subprotocol&token=protocol-case"
        with connect(
            protocol_url,
            subprotocols=["smoke.v2", "smoke.v1"],
            proxy=None,
        ) as websocket:
            assert websocket.subprotocol == "smoke.v2"
            assert json.loads(websocket.recv()) == {
                "originHost": "",
                "protocol": "smoke.v2",
                "token": "protocol-case",
            }

        _url, status_body = _open_without_proxy(
            f"{server.url}/support/realtime/websocket-status?token=socket-case"
        )
        assert json.loads(status_body) == {
            "active": 0,
            "closed": True,
            "opened": 1,
            "token": "socket-case",
        }
    finally:
        server.stop()


def test_websocket_fixture_waits_across_runner_checkpoints() -> None:
    class CheckpointDelayedConnection:
        def __init__(self) -> None:
            self.request = SimpleNamespace(
                path="/support/realtime/socket?scenario=text-order&token=delayed-case"
            )
            self.sent: list[str] = []
            self._received = iter(["alpha", "beta", "client-close"])

        def send(self, message: str) -> None:
            self.sent.append(message)

        def recv(
            self,
            timeout: float | None = None,
            *,
            decode: bool | None = None,
        ) -> str:
            del decode
            if timeout is not None:
                raise TimeoutError("runner checkpoint exceeded fixture-local deadline")
            return next(self._received)

    connection = CheckpointDelayedConnection()
    _handle_websocket_connection(connection, _RealtimeState())  # type: ignore[arg-type]

    assert connection.sent == [
        "server-open:delayed-case",
        "echo:1:alpha",
        "echo:2:beta",
    ]


def test_event_source_stream_gate_and_status_are_consumable(tmp_path: Path) -> None:
    server = FixtureServer(tmp_path)
    server.start()
    reader_ready = threading.Event()
    payload = bytearray()

    def read_stream() -> None:
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(
            f"{server.url}/support/realtime/events?scenario=multiline-custom&token=sse-case",
            timeout=5,
        ) as response:
            blank_lines = 0
            while blank_lines < 2:
                line = response.readline()
                payload.extend(line)
                blank_lines = blank_lines + 1 if line == b"\n" else blank_lines
            reader_ready.set()
            payload.extend(response.read())

    reader = threading.Thread(target=read_stream, daemon=True)
    try:
        reader.start()
        assert reader_ready.wait(timeout=2)
        _url, release_body = _open_without_proxy(
            f"{server.url}/support/realtime/release-event-source?token=sse-case"
        )
        assert json.loads(release_body) == {"released": True, "token": "sse-case"}
        reader.join(timeout=2)
        assert not reader.is_alive()
        assert b"event: update\n" in payload
        assert "data: second café\n".encode() in payload
        assert "data: default 東京\n".encode() in payload

        _url, status_body = _open_without_proxy(
            f"{server.url}/support/realtime/event-source-status?token=sse-case"
        )
        assert json.loads(status_body) == {
            "active": 0,
            "closed": True,
            "lastEventIds": [""],
            "opened": 1,
            "token": "sse-case",
        }
    finally:
        server.stop()
