"""Deterministic HTTP source for exact-Document navigation traces."""

from __future__ import annotations

import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import parse_qs, urlparse


class NavigationTraceFixture:
    def __init__(self) -> None:
        self._condition = threading.Condition()
        self._requests: list[dict[str, Any]] = []
        self._sequence = 0
        self.httpd = _NavigationTraceHttpServer(("127.0.0.1", 0), _NavigationTraceHandler)
        self.httpd.fixture = self
        self.port = int(self.httpd.server_address[1])
        self.thread = threading.Thread(
            target=self.httpd.serve_forever,
            name="moli-navigation-trace-fixture",
            daemon=True,
        )

    @property
    def base_url(self) -> str:
        return f"http://127.0.0.1:{self.port}"

    def url(self, stage: str, run_token: str) -> str:
        return f"{self.base_url}/navigation-trace/{stage}?run={run_token}"

    def __enter__(self) -> NavigationTraceFixture:
        self.thread.start()
        return self

    def __exit__(self, exc_type: object, exc: object, tb: object) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=2)

    def record(self, run_token: str, stage: str, status: int) -> None:
        with self._condition:
            self._sequence += 1
            self._requests.append(
                {
                    "sequence": self._sequence,
                    "run": run_token,
                    "stage": stage,
                    "status": status,
                    "completed_monotonic_ns": time.monotonic_ns(),
                }
            )
            self._condition.notify_all()

    def wait_for(self, run_token: str, stage: str, timeout_seconds: float) -> None:
        deadline = time.monotonic() + timeout_seconds
        with self._condition:
            while not any(
                request["run"] == run_token and request["stage"] == stage
                for request in self._requests
            ):
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"timed out waiting for fixture stage {stage!r} in run {run_token!r}")
                self._condition.wait(remaining)

    def request_records(self, run_token: str) -> list[dict[str, Any]]:
        with self._condition:
            return [dict(request) for request in self._requests if request["run"] == run_token]

    def navigation_request_stages(self, run_token: str) -> list[str]:
        stages = [request["stage"] for request in self.request_records(run_token)]
        try:
            first_navigation = stages.index("a")
        except ValueError:
            return stages
        return stages[first_navigation:]


class _NavigationTraceHttpServer(ThreadingHTTPServer):
    fixture: NavigationTraceFixture


class _NavigationTraceHandler(BaseHTTPRequestHandler):
    server: _NavigationTraceHttpServer

    def do_GET(self) -> None:
        parsed = urlparse(self.path)
        run_token = parse_qs(parsed.query).get("run", [""])[0]
        stage = parsed.path.removeprefix("/navigation-trace/")
        if not run_token or stage not in {"bootstrap", "ready", "a", "b", "complete"}:
            self.send_error(404)
            return
        if stage == "bootstrap":
            self._send_html(_bootstrap_document())
            self.server.fixture.record(run_token, stage, 200)
            return
        if stage == "a":
            self._send_html(_navigation_a_document())
            self.server.fixture.record(run_token, stage, 200)
            return
        if stage == "b":
            self._send_html(_navigation_b_document())
            self.server.fixture.record(run_token, stage, 200)
            return
        self.send_response(204)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", "0")
        self.end_headers()
        self.server.fixture.record(run_token, stage, 204)

    def _send_html(self, body: bytes) -> None:
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except BrokenPipeError:
            return

    def log_message(self, format: str, *args: object) -> None:
        return


def _bootstrap_document() -> bytes:
    return _trace_document(
        "Trace Bootstrap",
        "bootstrap",
        """
        addEventListener('load', () => {
          const run = new URL(location.href).searchParams.get('run') || '';
          void fetch('/navigation-trace/ready?run=' + encodeURIComponent(run), {cache: 'no-store'});
        }, {once: true});
        """,
    )


def _navigation_a_document() -> bytes:
    return _trace_document(
        "Trace A",
        "a",
        """
        addEventListener('load', () => {
          const run = new URL(location.href).searchParams.get('run') || '';
          location.href = '/navigation-trace/b?run=' + encodeURIComponent(run);
        }, {once: true});
        """,
    )


def _navigation_b_document() -> bytes:
    return _trace_document(
        "Trace B",
        "b",
        """
        addEventListener('load', () => {
          const run = new URL(location.href).searchParams.get('run') || '';
          void fetch('/navigation-trace/complete?run=' + encodeURIComponent(run), {cache: 'no-store'});
        }, {once: true});
        """,
    )


def _trace_document(title: str, phase: str, script: str) -> bytes:
    return (
        "<!doctype html><meta charset=utf-8>"
        f"<title>{title}</title>"
        f'<body data-trace-phase="{phase}"><main>{title}</main><script>{script}</script></body>'
    ).encode("utf-8")
