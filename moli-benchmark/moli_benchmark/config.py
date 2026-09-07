from __future__ import annotations

import os
import random
import socket
import shutil
import threading
from dataclasses import dataclass
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
PROJECT_ROOT = REPO_ROOT / "moli-benchmark"
RESULTS_ROOT = PROJECT_ROOT / "results"
FORMAL_RESULTS_ROOT = REPO_ROOT / "benchmarks" / "results"

PROXY_ENV_KEYS = (
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
)

_RESERVED_PORTS: set[int] = set()
_RESERVED_PORTS_LOCK = threading.Lock()
_SERVER_PORT_RANDOM = random.SystemRandom()
_SERVER_PORT_FALLBACK_RANGE = (10_000, 32_767)


def _linux_ephemeral_port_range() -> tuple[int, int] | None:
    try:
        raw = Path("/proc/sys/net/ipv4/ip_local_port_range").read_text(encoding="utf-8").strip()
        low_raw, high_raw = raw.split()
        low = int(low_raw)
        high = int(high_raw)
    except (OSError, ValueError):
        return None
    if 0 < low <= high <= 65_535:
        return low, high
    return None


def _server_port_range() -> tuple[int, int]:
    ephemeral = _linux_ephemeral_port_range()
    if ephemeral is None:
        return _SERVER_PORT_FALLBACK_RANGE
    low, _ = ephemeral
    candidate = (_SERVER_PORT_FALLBACK_RANGE[0], min(_SERVER_PORT_FALLBACK_RANGE[1], low - 1))
    if candidate[0] <= candidate[1]:
        return candidate
    return _SERVER_PORT_FALLBACK_RANGE


def clear_proxy_env(env: dict[str, str]) -> dict[str, str]:
    next_env = dict(env)
    for key in PROXY_ENV_KEYS:
        next_env.pop(key, None)
    next_env["NO_PROXY"] = "*"
    next_env["no_proxy"] = "*"
    return next_env


def clear_current_proxy_env() -> None:
    next_env = clear_proxy_env(os.environ)
    os.environ.clear()
    os.environ.update(next_env)


@dataclass
class ReservedPort:
    socket: socket.socket | None
    _port: int
    _closed: bool = False

    @property
    def port(self) -> int:
        return self._port

    def release_socket(self) -> None:
        sock = self.socket
        if sock is not None:
            self.socket = None
            sock.close()

    def close(self) -> None:
        if self._closed:
            return
        self.release_socket()
        with _RESERVED_PORTS_LOCK:
            _RESERVED_PORTS.discard(self._port)
        self._closed = True

    def __enter__(self) -> "ReservedPort":
        return self

    def __exit__(self, exc_type: object, exc: object, tb: object) -> None:
        self.close()


def reserve_port() -> ReservedPort:
    port_low, port_high = _server_port_range()
    last_error: BaseException | None = None
    for _ in range(512):
        port = _SERVER_PORT_RANDOM.randint(port_low, port_high)
        with _RESERVED_PORTS_LOCK:
            if port in _RESERVED_PORTS:
                continue
            _RESERVED_PORTS.add(port)
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            sock.bind(("127.0.0.1", port))
            bound_port = int(sock.getsockname()[1])
            if bound_port != port:
                raise RuntimeError(f"reserved unexpected benchmark port {bound_port}, expected {port}")
            return ReservedPort(sock, port)
        except BaseException as error:
            with _RESERVED_PORTS_LOCK:
                _RESERVED_PORTS.discard(port)
            last_error = error
            sock.close()
            if isinstance(error, OSError):
                continue
            raise
    raise RuntimeError(f"failed to reserve a unique local benchmark port in {port_low}-{port_high}") from last_error


def moli_binary(override: str | None = None) -> Path:
    raw_override = override or os.environ.get("MOLI_BIN")
    if raw_override:
        path = Path(raw_override).expanduser().resolve()
        if not path.exists():
            raise RuntimeError(f"MOLI_BIN does not exist: {path}")
        return path

    candidates = [
        REPO_ROOT / "target" / "release" / ("moli.exe" if os.name == "nt" else "moli"),
    ]
    existing = [candidate for candidate in candidates if candidate.exists()]
    if existing:
        return max(existing, key=lambda path: path.stat().st_mtime)
    path_candidate = shutil.which("moli")
    if path_candidate:
        return Path(path_candidate).resolve()
    raise RuntimeError("missing moli binary; run `cargo build --release` or set MOLI_BIN")


def optional_binary(env_key: str, names: tuple[str, ...], override: str | None = None) -> Path | None:
    raw_override = override or os.environ.get(env_key)
    if raw_override:
        path = Path(raw_override).expanduser().resolve()
        if not path.exists():
            raise RuntimeError(f"{env_key} does not exist: {path}")
        return path
    for name in names:
        candidate = shutil.which(name)
        if candidate:
            return Path(candidate).resolve()
    return None


def lightpanda_binary(override: str | None = None) -> Path | None:
    return optional_binary("LIGHTPANDA_BIN", ("lightpanda",), override)


def obscura_binary(override: str | None = None) -> Path | None:
    return optional_binary("OBSCURA_BIN", ("obscura",), override)


def chrome_binary(override: str | None = None) -> Path | None:
    return optional_binary(
        "CHROME_BIN",
        ("google-chrome", "google-chrome-stable", "chrome", "chromium", "chromium-browser"),
        override,
    )
