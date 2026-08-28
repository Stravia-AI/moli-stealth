from __future__ import annotations

import json
import os
import threading
from collections.abc import Sequence
from functools import partial
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlencode, urlsplit

from websockets.exceptions import ConnectionClosed
from websockets.sync.server import ServerConnection, serve


class _RealtimeState:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._websocket_active: dict[str, int] = {}
        self._websocket_opened: dict[str, int] = {}
        self._websocket_closed: dict[str, threading.Event] = {}
        self._event_source_active: dict[str, int] = {}
        self._event_source_opened: dict[str, int] = {}
        self._event_source_last_ids: dict[str, list[str]] = {}
        self._event_source_closed: dict[str, threading.Event] = {}

    @staticmethod
    def _event(events: dict[str, threading.Event], token: str) -> threading.Event:
        return events.setdefault(token, threading.Event())

    def websocket_opened(self, token: str) -> None:
        with self._lock:
            if self._websocket_active.get(token, 0) == 0:
                self._event(self._websocket_closed, token).clear()
            self._websocket_active[token] = self._websocket_active.get(token, 0) + 1
            self._websocket_opened[token] = self._websocket_opened.get(token, 0) + 1

    def websocket_closed(self, token: str) -> None:
        with self._lock:
            active = max(0, self._websocket_active.get(token, 0) - 1)
            self._websocket_active[token] = active
            if active == 0:
                self._event(self._websocket_closed, token).set()

    def websocket_status(self, token: str, *, timeout: float = 5) -> dict[str, object]:
        with self._lock:
            closed_event = self._event(self._websocket_closed, token)
        closed = closed_event.wait(timeout=timeout)
        with self._lock:
            active = self._websocket_active.get(token, 0)
            opened = self._websocket_opened.get(token, 0)
            result: dict[str, object] = {
                "active": active,
                "closed": closed and active == 0,
                "opened": opened,
                "token": token,
            }
            if result["closed"]:
                self._websocket_active.pop(token, None)
                self._websocket_opened.pop(token, None)
                self._websocket_closed.pop(token, None)
            return result

    def event_source_opened(self, token: str, last_event_id: str) -> int:
        with self._lock:
            if self._event_source_active.get(token, 0) == 0:
                self._event(self._event_source_closed, token).clear()
            self._event_source_active[token] = self._event_source_active.get(token, 0) + 1
            opened = self._event_source_opened.get(token, 0) + 1
            self._event_source_opened[token] = opened
            self._event_source_last_ids.setdefault(token, []).append(last_event_id)
            return opened

    def event_source_closed(self, token: str) -> None:
        with self._lock:
            active = max(0, self._event_source_active.get(token, 0) - 1)
            self._event_source_active[token] = active
            if active == 0:
                self._event(self._event_source_closed, token).set()

    def event_source_status(self, token: str, *, timeout: float = 5) -> dict[str, object]:
        with self._lock:
            closed_event = self._event(self._event_source_closed, token)
        closed = closed_event.wait(timeout=timeout)
        with self._lock:
            active = self._event_source_active.get(token, 0)
            opened = self._event_source_opened.get(token, 0)
            last_event_ids = list(self._event_source_last_ids.get(token, []))
            result: dict[str, object] = {
                "active": active,
                "closed": closed and active == 0,
                "lastEventIds": last_event_ids,
                "opened": opened,
                "token": token,
            }
            if result["closed"]:
                self._event_source_active.pop(token, None)
                self._event_source_opened.pop(token, None)
                self._event_source_last_ids.pop(token, None)
                self._event_source_closed.pop(token, None)
            return result


def _websocket_query(connection: ServerConnection) -> tuple[str, str]:
    request_path = connection.request.path if connection.request is not None else "/"
    request = urlsplit(request_path)
    query = parse_qs(request.query, keep_blank_values=True)
    return query.get("scenario", [""])[0], query.get("token", ["missing"])[0]


def _handle_websocket_connection(
    connection: ServerConnection,
    state: _RealtimeState,
) -> None:
    scenario, token = _websocket_query(connection)
    state.websocket_opened(token)
    # Client actions may follow a runner-controlled DOM checkpoint. Keep receives tied
    # to the connection lifetime; page and runner deadlines own fixture liveness.
    try:
        if scenario == "text-order":
            connection.send(f"server-open:{token}")
            for index in range(1, 3):
                message = connection.recv()
                connection.send(f"echo:{index}:{message}")
            connection.recv()
            return
        if scenario == "binary-arraybuffer":
            for _index in range(2):
                message = connection.recv(decode=False)
                assert isinstance(message, bytes)
                connection.send(bytes(reversed(message)))
            connection.recv()
            return
        if scenario == "binary-fragmented":
            connection.send([b"\x00\x01", b"\xfe\xff"], text=False)
            connection.recv()
            connection.send([b"\x10", b"\x20\x30"], text=False)
            connection.recv()
            return
        if scenario == "subprotocol":
            origin = ""
            if connection.request is not None:
                origin = connection.request.headers.get("Origin", "")
            origin_host = urlsplit(origin).hostname or ""
            connection.send(
                json.dumps(
                    {
                        "originHost": origin_host,
                        "protocol": connection.subprotocol or "",
                        "token": token,
                    },
                    ensure_ascii=False,
                    sort_keys=True,
                )
            )
            connection.recv()
            return
        if scenario == "client-close":
            connection.send(f"client-close-ready:{token}")
            connection.recv()
            return
        if scenario == "server-close":
            connection.send(f"server-close-ready:{token}")
            connection.close(3002, "server-done")
            return
        if scenario == "realm-teardown":
            connection.send(f"realm-ready:{token}")
            connection.recv()
            return
        if scenario == "coordinated":
            connection.send(f"websocket-ready:{token}")
            message = connection.recv()
            connection.send(f"coordinated:{message}")
            connection.recv()
            return
        connection.close(1008, "unknown scenario")
    except ConnectionClosed:
        pass
    finally:
        state.websocket_closed(token)


def _select_realtime_subprotocol(
    _connection: ServerConnection,
    offered: Sequence[str],
) -> str | None:
    for candidate in ("smoke.v2", "smoke.v1"):
        if candidate in offered:
            return candidate
    return None


class _FixtureHttpServer(ThreadingHTTPServer):
    def __init__(
        self,
        server_address: tuple[str, int],
        handler: Any,
        realtime_state: _RealtimeState,
    ) -> None:
        super().__init__(server_address, handler)
        self.realtime_state = realtime_state
        self.websocket_url = ""
        self._gate_lock = threading.Lock()
        self._gates: dict[str, threading.Event] = {}
        self._cors_preflight_lock = threading.Lock()
        self._cors_preflights: dict[str, dict[str, object]] = {}

    def response_gate(self, token: str) -> threading.Event:
        with self._gate_lock:
            return self._gates.setdefault(token, threading.Event())

    def release_response_gate(self, token: str) -> bool:
        with self._gate_lock:
            gate = self._gates.get(token)
        if gate is None:
            return False
        gate.set()
        return True

    def forget_response_gate(self, token: str, gate: threading.Event) -> None:
        with self._gate_lock:
            if self._gates.get(token) is gate:
                self._gates.pop(token, None)

    def release_all_response_gates(self) -> None:
        with self._gate_lock:
            gates = list(self._gates.values())
        for gate in gates:
            gate.set()

    def record_cors_preflight(self, token: str, observation: dict[str, object]) -> None:
        if not token:
            return
        with self._cors_preflight_lock:
            self._cors_preflights[token] = observation

    def take_cors_preflight(self, token: str) -> dict[str, object] | None:
        if not token:
            return None
        with self._cors_preflight_lock:
            return self._cors_preflights.pop(token, None)


class _FixtureHandler(SimpleHTTPRequestHandler):
    server_version = "MoliFrontendSmokeFixture/0.1"

    @property
    def fixture_server(self) -> _FixtureHttpServer:
        assert isinstance(self.server, _FixtureHttpServer)
        return self.server

    def _query(self) -> dict[str, list[str]]:
        return parse_qs(urlsplit(self.path).query, keep_blank_values=True)

    def _query_value(self, name: str, default: str = "") -> str:
        return self._query().get(name, [default])[0]

    def _request_body(self) -> bytes:
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def _send_bytes(
        self,
        status: int,
        body: bytes,
        *,
        content_type: str,
        headers: tuple[tuple[str, str], ...] = (),
    ) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for name, value in headers:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def _send_json(
        self,
        value: object,
        *,
        status: int = 200,
        headers: tuple[tuple[str, str], ...] = (),
    ) -> None:
        body = (json.dumps(value, ensure_ascii=False, sort_keys=True) + "\n").encode()
        self._send_bytes(
            status,
            body,
            content_type="application/json; charset=utf-8",
            headers=headers,
        )

    def _send_redirect(self, status: int, location: str) -> None:
        self.send_response(status)
        self.send_header("Location", location)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def _cookie_names(self) -> list[str]:
        names = []
        for item in (self.headers.get("Cookie") or "").split(";"):
            name, separator, _value = item.strip().partition("=")
            if separator and name:
                names.append(name)
        return sorted(names)

    def _cors_headers(
        self,
        *,
        credentials: bool = False,
        wildcard: bool = False,
        expose: str | None = None,
    ) -> tuple[tuple[str, str], ...]:
        origin = self.headers.get("Origin") or "null"
        headers: list[tuple[str, str]] = [
            ("Access-Control-Allow-Origin", "*" if wildcard else origin),
            ("Vary", "Origin"),
        ]
        if credentials:
            headers.append(("Access-Control-Allow-Credentials", "true"))
        if expose is not None:
            headers.append(("Access-Control-Expose-Headers", expose))
        return tuple(headers)

    def _record_preflight(self) -> None:
        self.fixture_server.record_cors_preflight(
            self._query_value("token"),
            {
                "method": self.headers.get("Access-Control-Request-Method") or "",
                "headers": self.headers.get("Access-Control-Request-Headers") or "",
                "origin": self.headers.get("Origin") or "",
            },
        )

    def _send_preflight(
        self,
        *,
        methods: str,
        headers: str,
        credentials: bool = False,
    ) -> None:
        self._record_preflight()
        cors_headers = list(self._cors_headers(credentials=credentials))
        cors_headers.extend(
            (
                ("Access-Control-Allow-Methods", methods),
                ("Access-Control-Allow-Headers", headers),
                ("Access-Control-Max-Age", "600"),
            )
        )
        self._send_bytes(204, b"", content_type="text/plain", headers=tuple(cors_headers))

    def _service_worker_script(self) -> bytes:
        token = json.dumps(self._query_value("token", "missing"), ensure_ascii=False)
        version = json.dumps(self._query_value("version", "v1"), ensure_ascii=False)
        source = r'''
const TOKEN = __TOKEN__;
const VERSION = __VERSION__;
const CACHE_NAME = `frontend-smoke-sw-${TOKEN}`;
const CACHE_KEY = `/support/service-worker/cache-key?token=${encodeURIComponent(TOKEN)}`;

self.addEventListener("install", (event) => {
  event.waitUntil((async () => {
    const cache = await caches.open(CACHE_NAME);
    await cache.put(
      CACHE_KEY,
      new Response(`precache:${TOKEN}:${VERSION}`, {
        headers: { "Content-Type": "text/plain;charset=utf-8", "X-SW-Version": VERSION },
      }),
    );
    await self.skipWaiting();
  })());
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (url.pathname === "/support/service-worker/synthetic") {
    event.respondWith((async () => {
      const body = await event.request.clone().text();
      return new Response(JSON.stringify({
        token: TOKEN,
        version: VERSION,
        method: event.request.method,
        mode: event.request.mode,
        credentials: event.request.credentials,
        destination: event.request.destination,
        header: event.request.headers.get("x-smoke-token") || "",
        body,
      }), {
        status: 201,
        statusText: "Synthetic Fixture",
        headers: {
          "Content-Type": "application/json;charset=utf-8",
          "X-Service-Worker": VERSION,
        },
      });
    })());
    return;
  }
  if (url.pathname === "/support/service-worker/cache-probe") {
    event.respondWith((async () => {
      const cached = await caches.match(CACHE_KEY);
      return cached || new Response("cache-miss", { status: 404 });
    })());
    return;
  }
  if (url.pathname === "/support/service-worker/stream") {
    const encoder = new TextEncoder();
    const chunks = [`stream:${TOKEN}:`, `${VERSION}:`, "café:東京"];
    event.respondWith(new Response(new ReadableStream({
      pull(controller) {
        const next = chunks.shift();
        if (next === undefined) {
          controller.close();
        } else {
          controller.enqueue(encoder.encode(next));
        }
      },
    }), { headers: { "Content-Type": "text/plain;charset=utf-8" } }));
    return;
  }
  if (url.pathname === "/support/service-worker/version") {
    event.respondWith(new Response(`version:${VERSION}:${TOKEN}`, {
      headers: { "X-SW-Version": VERSION },
    }));
    return;
  }
  if (url.pathname === "/support/service-worker/redirect") {
    event.respondWith(Response.redirect(`/support/service-worker/network?token=${encodeURIComponent(TOKEN)}`, 302));
  }
});

self.addEventListener("message", (event) => {
  const reply = (value, transfer = []) => {
    const port = event.ports[0];
    if (port) {
      port.postMessage(value, transfer);
    } else if (event.source) {
      event.source.postMessage(value, transfer);
    }
  };
  const data = event.data || {};
  if (data.command === "inspect") {
    event.waitUntil((async () => {
      const clients = await self.clients.matchAll({ includeUncontrolled: true, type: "window" });
      reply({
        command: "inspect",
        token: TOKEN,
        version: VERSION,
        clientCount: clients.length,
        clientPaths: clients.map((client) => new URL(client.url).pathname).sort(),
        scopePath: new URL(self.registration.scope).pathname,
      });
    })());
    return;
  }
  if (data.command === "transfer") {
    const buffer = data.buffer;
    const view = new Uint8Array(buffer);
    view.reverse();
    reply({ command: "transfer", buffer, byteLength: buffer.byteLength }, [buffer]);
    return;
  }
  if (data.command === "cache-write") {
    event.waitUntil((async () => {
      const cache = await caches.open(CACHE_NAME);
      const key = `/support/service-worker/message-cache?token=${encodeURIComponent(TOKEN)}`;
      await cache.put(key, new Response(String(data.value), {
        headers: { "X-Message-Cache": VERSION },
      }));
      reply({ command: "cache-write", key, cacheName: CACHE_NAME });
    })());
  }
});
'''
        return source.replace("__TOKEN__", token).replace("__VERSION__", version).encode()

    def _handle_service_worker(self, request_path: str) -> bool:
        if request_path == "/support/service-worker/worker.js":
            self._send_bytes(
                200,
                self._service_worker_script(),
                content_type="text/javascript; charset=utf-8",
                headers=(("Service-Worker-Allowed", "/"),),
            )
            return True
        if request_path in {
            "/support/service-worker/network",
            "/support/service-worker/fallback",
        }:
            token = self._query_value("token")
            label = request_path.rsplit("/", 1)[-1]
            self._send_bytes(
                200,
                f"network-{label}:{token}".encode(),
                content_type="text/plain; charset=utf-8",
                headers=(("X-Network-Source", label),),
            )
            return True
        return False

    def _script_source(self, request_path: str) -> bytes | None:
        token = self._query_value("token", "missing")
        version = self._query_value("version", "v1")
        token_literal = json.dumps(token, ensure_ascii=False)
        version_literal = json.dumps(version, ensure_ascii=False)
        if request_path == "/support/scripts/classic.js":
            label_literal = json.dumps(self._query_value("label", "classic"), ensure_ascii=False)
            source = r'''
(() => {
const TOKEN = __TOKEN__;
const LABEL = __LABEL__;
globalThis.__scriptLifecycleOrder ??= [];
globalThis.__scriptLifecycleOrder.push(`classic:${LABEL}`);
const target = document.querySelector("[data-script-target]") || document.body || document.documentElement;
const marker = document.createElement("i");
marker.dataset.classicScript = LABEL;
marker.dataset.currentScript = document.currentScript?.dataset.scriptId || "none";
marker.dataset.ownerTitle = document.title;
marker.textContent = `${LABEL}:${TOKEN}`;
target.append(marker);
})();
'''
            return (
                source.replace("__TOKEN__", token_literal)
                .replace("__LABEL__", label_literal)
                .encode()
            )
        if request_path == "/support/scripts/module-leaf.js":
            source = r'''
const TOKEN = __TOKEN__;
const VERSION = __VERSION__;
globalThis.__scriptModuleOrder ??= [];
globalThis.__scriptModuleOrder.push(`leaf:${VERSION}`);
export const leafValue = `leaf:${TOKEN}:${VERSION}`;
'''
            return (
                source.replace("__TOKEN__", token_literal)
                .replace("__VERSION__", version_literal)
                .encode()
            )
        if request_path == "/support/scripts/module-branch.js":
            leaf_url = "/support/scripts/module-leaf.js?" + urlencode(
                {"token": token, "version": version}
            )
            source = r'''
import { leafValue } from __LEAF_URL__;
globalThis.__scriptModuleOrder ??= [];
globalThis.__scriptModuleOrder.push("branch");
export const branchValue = `${leafValue}:branch`;
'''
            return source.replace(
                "__LEAF_URL__", json.dumps(leaf_url, ensure_ascii=False)
            ).encode()
        if request_path == "/support/scripts/module-entry.js":
            branch_url = "/support/scripts/module-branch.js?" + urlencode(
                {"token": token, "version": version}
            )
            source = r'''
import { branchValue } from __BRANCH_URL__;
await Promise.resolve();
globalThis.__scriptModuleOrder ??= [];
globalThis.__scriptModuleOrder.push("entry");
globalThis.dispatchEvent(new CustomEvent("smoke-module-ready", {
  detail: { branchValue, order: globalThis.__scriptModuleOrder.join("|") },
}));
export default branchValue;
'''
            return source.replace(
                "__BRANCH_URL__", json.dumps(branch_url, ensure_ascii=False)
            ).encode()
        if request_path == "/support/scripts/module-counter.js":
            source = r'''
const TOKEN = __TOKEN__;
const VERSION = __VERSION__;
globalThis.__scriptModuleCounters ??= Object.create(null);
const key = `${TOKEN}:${VERSION}`;
globalThis.__scriptModuleCounters[key] = (globalThis.__scriptModuleCounters[key] || 0) + 1;
export const count = globalThis.__scriptModuleCounters[key];
export const value = `counter:${key}:${count}`;
'''
            return (
                source.replace("__TOKEN__", token_literal)
                .replace("__VERSION__", version_literal)
                .encode()
            )
        if request_path == "/support/scripts/module-map.js":
            source = r'''
const TOKEN = __TOKEN__;
globalThis.__scriptModuleOrder ??= [];
globalThis.__scriptModuleOrder.push("mapped");
export const mappedValue = `mapped:${TOKEN}`;
'''
            return source.replace("__TOKEN__", token_literal).encode()
        if request_path == "/support/scripts/module-throw.js":
            source = r'''
const TOKEN = __TOKEN__;
globalThis.__scriptModuleOrder ??= [];
globalThis.__scriptModuleOrder.push("throw");
throw new Error(`module-failure:${TOKEN}`);
'''
            return source.replace("__TOKEN__", token_literal).encode()
        if request_path == "/support/scripts/module-recovery.js":
            source = r'''
const TOKEN = __TOKEN__;
globalThis.__scriptModuleOrder ??= [];
globalThis.__scriptModuleOrder.push("recovery");
export const recovered = `recovered:${TOKEN}`;
'''
            return source.replace("__TOKEN__", token_literal).encode()
        return None

    def _handle_scripts(self, request_path: str) -> bool:
        source = self._script_source(request_path)
        if source is None:
            return False
        self._send_bytes(
            200,
            source,
            content_type="text/javascript; charset=utf-8",
        )
        return True

    def _handle_event_source(self, scenario: str, token: str) -> None:
        last_event_id = self.headers.get("Last-Event-ID") or ""
        connection_index = self.fixture_server.realtime_state.event_source_opened(
            token, last_event_id
        )
        hold_open = True
        if scenario == "multiline-custom":
            payload = (
                f": comment:{token}\n"
                "retry: 5\n"
                f"id: custom-{token}\n"
                "event: update\n"
                "data: first line\n"
                "data: second café\n\n"
                f"id: default-{token}\n"
                "data: default 東京\n\n"
            ).encode()
        elif scenario == "reconnect-last-id" and connection_index == 1:
            payload = (
                "retry: 5\n"
                f"id: first-{token}\n"
                f"data: first:{token}\n\n"
            ).encode()
            hold_open = False
        elif scenario == "reconnect-last-id":
            payload = (
                f"id: second-{token}\n"
                f"data: second:last={last_event_id}\n\n"
            ).encode()
        elif scenario == "close-state":
            payload = (
                "retry: 1\n"
                f"id: close-{token}\n"
                f"data: close-state:{token}\n\n"
            ).encode()
        elif scenario == "coordinated":
            payload = f"id: coordinated-{token}\ndata: eventsource-ready:{token}\n\n".encode()
        else:
            payload = f"data: unknown:{scenario}:{token}\n\n".encode()

        gate_token = f"event-source:{token}"
        gate = self.fixture_server.response_gate(gate_token) if hold_open else None
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Connection", "close")
        self.send_header("X-Smoke-SSE-Connection", str(connection_index))
        self.end_headers()
        self.close_connection = True
        try:
            split = max(1, len(payload) // 2)
            self.wfile.write(payload[:split])
            self.wfile.flush()
            self.wfile.write(payload[split:])
            self.wfile.flush()
            if gate is not None:
                gate.wait(timeout=20)
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            if gate is not None:
                self.fixture_server.forget_response_gate(gate_token, gate)
            self.fixture_server.realtime_state.event_source_closed(token)

    def _handle_realtime(self, request_path: str) -> bool:
        if not request_path.startswith("/support/realtime/"):
            return False
        token = self._query_value("token", "missing")
        if request_path == "/support/realtime/config":
            self._send_json({"websocketUrl": self.fixture_server.websocket_url})
            return True
        if request_path == "/support/realtime/events":
            self._handle_event_source(self._query_value("scenario"), token)
            return True
        if request_path == "/support/realtime/release-event-source":
            gate_token = f"event-source:{token}"
            released = self.fixture_server.release_response_gate(gate_token)
            self._send_json({"released": released, "token": token})
            return True
        if request_path == "/support/realtime/websocket-status":
            self._send_json(self.fixture_server.realtime_state.websocket_status(token))
            return True
        if request_path == "/support/realtime/event-source-status":
            self._send_json(self.fixture_server.realtime_state.event_source_status(token))
            return True
        return False

    def _handle_cors(self, request_path: str) -> bool:
        if not request_path.startswith("/support/cors/"):
            return False
        token = self._query_value("token")

        if request_path == "/support/cors/deny":
            self._send_bytes(200, b"cors-denied", content_type="text/plain; charset=utf-8")
            return True
        if request_path == "/support/cors/no-cors":
            self._send_bytes(200, b"opaque-body", content_type="text/plain; charset=utf-8")
            return True
        if request_path == "/support/cors/wildcard":
            self._send_bytes(
                200,
                b"cors-wildcard",
                content_type="text/plain; charset=utf-8",
                headers=self._cors_headers(wildcard=True),
            )
            return True
        if request_path == "/support/cors/exposed":
            self._send_bytes(
                200,
                b"cors-exposed",
                content_type="text/plain; charset=utf-8",
                headers=self._cors_headers(expose="X-Smoke-Visible")
                + (("X-Smoke-Visible", f"visible-{token}"), ("X-Smoke-Hidden", "secret")),
            )
            return True
        if request_path == "/support/cors/credentials":
            wildcard = self._query_value("wildcard") == "1"
            headers = list(self._cors_headers(credentials=True, wildcard=wildcard))
            if self._query_value("set") == "1":
                headers.append(
                    (
                        "Set-Cookie",
                        f"cors_{token}=present; Path=/support/cors; SameSite=None; Secure",
                    )
                )
            self._send_json(
                {
                    "cookieNames": self._cookie_names(),
                    "origin": self.headers.get("Origin") or "",
                    "token": token,
                },
                headers=tuple(headers),
            )
            return True
        if request_path == "/support/cors/metadata":
            self._send_json(
                {
                    "origin": self.headers.get("Origin") or "",
                    "referer": self.headers.get("Referer") or "",
                    "secFetchMode": self.headers.get("Sec-Fetch-Mode") or "",
                    "secFetchSite": self.headers.get("Sec-Fetch-Site") or "",
                    "token": token,
                },
                headers=self._cors_headers(expose="X-Smoke-Metadata")
                + (("X-Smoke-Metadata", "visible"),),
            )
            return True

        preflight_routes = {
            "/support/cors/preflight/allow": ("POST, PUT", "content-type, x-smoke-token"),
            "/support/cors/preflight/deny-method": ("POST", "content-type, x-smoke-token"),
            "/support/cors/preflight/deny-header": ("POST, PUT", "content-type"),
            "/support/cors/preflight/redirect": ("POST, PUT", "content-type, x-smoke-token"),
            "/support/cors/preflight/final": ("POST, PUT", "content-type, x-smoke-token"),
        }
        if request_path in preflight_routes and self.command == "OPTIONS":
            methods, headers = preflight_routes[request_path]
            self._send_preflight(methods=methods, headers=headers)
            return True
        if request_path == "/support/cors/preflight/redirect":
            location = "/support/cors/preflight/final?" + urlencode({"token": token})
            self.send_response(307)
            self.send_header("Location", location)
            for name, value in self._cors_headers():
                self.send_header(name, value)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return True
        if request_path in preflight_routes:
            body = self._request_body().decode("utf-8", errors="replace")
            observation = self.fixture_server.take_cors_preflight(token)
            self._send_json(
                {
                    "body": body,
                    "header": self.headers.get("X-Smoke-Token") or "",
                    "method": self.command,
                    "preflight": observation,
                    "token": token,
                },
                headers=self._cors_headers(expose="X-Smoke-Actual")
                + (("X-Smoke-Actual", request_path.rsplit("/", 1)[-1]),),
            )
            return True
        if request_path == "/support/cors/allow":
            self._send_json(
                {
                    "method": self.command,
                    "origin": self.headers.get("Origin") or "",
                    "token": token,
                },
                headers=self._cors_headers(),
            )
            return True
        return False

    def _handle_gated_response(self) -> None:
        token = self._query_value("token", "missing")
        gate = self.fixture_server.response_gate(token)
        first = f"first:{token}\n".encode()
        second = f"second:{token}\n".encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain; charset=utf-8")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        try:
            self.wfile.write(first)
            self.wfile.flush()
            gate.wait(timeout=20)
            self.wfile.write(second)
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            self.fixture_server.forget_response_gate(token, gate)

    def _handle_network(self, request_path: str) -> bool:
        if request_path == "/support/network/redirect-307":
            body = self._request_body().decode("utf-8", errors="replace")
            location = "/support/network/redirect-303?" + urlencode(
                {
                    "token": self._query_value("token"),
                    "firstMethod": self.command,
                    "firstBody": body,
                }
            )
            self._send_redirect(307, location)
            return True
        if request_path == "/support/network/redirect-303":
            body = self._request_body().decode("utf-8", errors="replace")
            location = "/support/network/redirect-result?" + urlencode(
                {
                    "token": self._query_value("token"),
                    "firstMethod": self._query_value("firstMethod"),
                    "firstBody": self._query_value("firstBody"),
                    "middleMethod": self.command,
                    "middleBody": body,
                }
            )
            self._send_redirect(303, location)
            return True
        if request_path == "/support/network/redirect-result":
            self._send_json(
                {
                    "token": self._query_value("token"),
                    "firstMethod": self._query_value("firstMethod"),
                    "firstBody": self._query_value("firstBody"),
                    "middleMethod": self._query_value("middleMethod"),
                    "middleBody": self._query_value("middleBody"),
                    "finalMethod": self.command,
                    "finalBody": self._request_body().decode("utf-8", errors="replace"),
                    "trace": self.headers.get("X-Smoke-Trace") or "",
                }
            )
            return True
        if request_path == "/support/network/stream-payload":
            token = self._query_value("token")
            self._send_json(
                {
                    "token": token,
                    "items": ["alpha", "beta", "gamma"],
                    "text": "café-東京",
                }
            )
            return True
        if request_path == "/support/network/gated-response":
            self._handle_gated_response()
            return True
        if request_path == "/support/network/release-response":
            token = self._query_value("token")
            self._send_json(
                {"token": token, "released": self.fixture_server.release_response_gate(token)}
            )
            return True
        if request_path == "/support/network/xhr-payload":
            token = self._query_value("token")
            self._send_json(
                {"token": token, "state": "partial", "values": [3, 5, 8]},
                status=206,
                headers=(("X-Smoke-Trace", f"xhr:{token}"),),
            )
            return True
        if request_path == "/support/network/cache-item":
            token = self._query_value("token")
            self._send_bytes(
                200,
                f"network-cache:{token}".encode(),
                content_type="text/plain; charset=utf-8",
                headers=(("X-Smoke-Cache", token),),
            )
            return True
        if request_path == "/support/network/set-cookie":
            name = self._query_value("name", "smoke_http")
            clear = self._query_value("clear") == "1"
            value = "deleted; Max-Age=0" if clear else "http-only"
            self._send_json(
                {"name": name, "cleared": clear},
                headers=(("Set-Cookie", f"{name}={value}; Path=/support/network; HttpOnly; SameSite=Lax"),),
            )
            return True
        if request_path in {
            "/support/network/cookie-echo",
            "/support/network/scoped/cookie-echo",
        }:
            self._send_json(
                {"path": request_path, "cookieNames": self._cookie_names()}
            )
            return True
        return False

    def do_GET(self) -> None:
        request = urlsplit(self.path)
        if request.path == "/support/alternate-origin-frame":
            host = (self.headers.get("Host") or "").partition(":")[0].lower()
            alternate_host = "127.0.0.1" if host == "localhost" else "localhost"
            port = int(self.server.server_address[1])
            query = f"?{request.query}" if request.query else ""
            self.send_response(302)
            self.send_header(
                "Location",
                f"http://{alternate_host}:{port}/support/boundary-frame.html{query}",
            )
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if self._handle_scripts(request.path):
            return
        if self._handle_realtime(request.path):
            return
        if self._handle_network(request.path):
            return
        if self._handle_service_worker(request.path):
            return
        if self._handle_cors(request.path):
            return
        super().do_GET()

    def do_POST(self) -> None:
        request = urlsplit(self.path)
        if self._handle_network(request.path):
            return
        if self._handle_service_worker(request.path):
            return
        if self._handle_cors(request.path):
            return
        self.send_error(404)

    def do_PUT(self) -> None:
        request = urlsplit(self.path)
        if self._handle_cors(request.path):
            return
        self.send_error(404)

    def do_OPTIONS(self) -> None:
        request = urlsplit(self.path)
        if self._handle_cors(request.path):
            return
        self.send_error(404)

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        super().end_headers()

    def log_message(self, format: str, *args: object) -> None:
        if os.environ.get("MOLI_FRONTEND_SMOKE_TRACE_BG") == "1":
            super().log_message(format, *args)


class FixtureServer:
    def __init__(self, root: Path) -> None:
        if not root.is_dir():
            raise RuntimeError(f"fixture dist directory does not exist: {root}")
        self._realtime_state = _RealtimeState()
        handler = partial(_FixtureHandler, directory=str(root))
        self._server = _FixtureHttpServer(
            ("127.0.0.1", 0), handler, self._realtime_state
        )
        self._server.daemon_threads = True
        self._websocket_server = serve(
            partial(_handle_websocket_connection, state=self._realtime_state),
            "127.0.0.1",
            0,
            subprotocols=["smoke.v2", "smoke.v1"],
            select_subprotocol=_select_realtime_subprotocol,
            compression=None,
            ping_interval=None,
        )
        websocket_host, websocket_port = self._websocket_server.socket.getsockname()[:2]
        self._server.websocket_url = (
            f"ws://{websocket_host}:{websocket_port}/support/realtime/socket"
        )
        self._thread = threading.Thread(
            target=self._server.serve_forever,
            name="moli-frontend-smoke-fixture",
            daemon=True,
        )
        self._websocket_thread = threading.Thread(
            target=self._websocket_server.serve_forever,
            name="moli-frontend-smoke-websocket-fixture",
            daemon=True,
        )

    @property
    def url(self) -> str:
        host, port = self._server.server_address
        return f"http://{host}:{port}"

    @property
    def websocket_url(self) -> str:
        return self._server.websocket_url

    def start(self) -> None:
        self._thread.start()
        self._websocket_thread.start()

    def stop(self) -> None:
        self._server.release_all_response_gates()
        self._server.shutdown()
        self._server.server_close()
        self._websocket_server.shutdown()
        self._thread.join(timeout=5)
        self._websocket_thread.join(timeout=5)
