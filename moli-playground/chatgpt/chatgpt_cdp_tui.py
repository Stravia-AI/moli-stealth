#!/usr/bin/env python3
from __future__ import annotations

import argparse
import asyncio
import curses
import os
import queue
import textwrap
import threading
import time
from dataclasses import dataclass, field
from typing import Any

from chatgpt_cdp_demo import (
    DEFAULT_ANSWER_TIMEOUT,
    DEFAULT_MOLI_HTTP_TIMEOUT_MS,
    DEFAULT_LOGIN_TIMEOUT,
    DEFAULT_STARTUP_TIMEOUT,
    DEFAULT_URL,
    CdpPage,
    DemoError,
    MoliServe,
    RawCdpClient,
    ask_once,
    connect_cdp,
    login,
    start_moli,
    stop_moli,
)


ENTER_KEYS = {curses.KEY_ENTER, 10, 13, "\n", "\r"}
BACKSPACE_KEYS = {curses.KEY_BACKSPACE, 8, 127, "\b", "\x7f"}


def error_text(error: BaseException) -> str:
    text = str(error)
    return text if text else repr(error)


@dataclass
class UiEvent:
    kind: str
    text: str


@dataclass
class TextField:
    name: str
    label: str
    value: str = ""
    secret: bool = False

    def put(self, ch: str | int) -> None:
        if isinstance(ch, str):
            if ch.isprintable():
                self.value += ch
            return
        if 32 <= ch <= 126:
            self.value += chr(ch)

    def backspace(self) -> None:
        self.value = self.value[:-1]

    def rendered(self, width: int) -> str:
        display = "*" * len(self.value) if self.secret else self.value
        if len(display) > width:
            display = display[-width:]
        return display


@dataclass
class TuiState:
    fields: dict[str, TextField]
    title: str = "Moli ChatGPT CDP TUI"
    focus: str = "email"
    logged_in: bool = False
    busy: bool = False
    awaiting_auth_code: bool = False
    status: str = "Enter email/password, then press Ctrl-S or Enter to login."
    messages: list[tuple[str, str]] = field(default_factory=list)


class Backend:
    def __init__(self, args: argparse.Namespace, events: "queue.Queue[UiEvent]") -> None:
        self.args = args
        self.args.quiet = True
        self.events = events
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self._run_loop, daemon=True)
        self.started = threading.Event()
        self.serve: MoliServe | None = None
        self.client: RawCdpClient | None = None
        self.page: CdpPage | None = None
        self.thread.start()
        if not self.started.wait(timeout=2):
            raise RuntimeError("raw CDP backend event loop did not start")

    def _run_loop(self) -> None:
        asyncio.set_event_loop(self.loop)
        self.started.set()
        self.loop.run_forever()

    def emit(self, kind: str, text: str) -> None:
        self.events.put(UiEvent(kind=kind, text=text))

    def login(self, email: str, password: str) -> None:
        asyncio.run_coroutine_threadsafe(self._login(email, password), self.loop)

    def ask(self, prompt: str) -> None:
        asyncio.run_coroutine_threadsafe(self._ask(prompt), self.loop)

    async def _login(self, email: str, password: str) -> None:
        try:
            self.emit("status", "starting moli serve")
            self.serve = start_moli(self.args)
            self.emit("status", f"connected CDP at {self.serve.endpoint}")
            self.client = await connect_cdp(self.serve.endpoint)
            self.page = await CdpPage.create(self.client)
            await login(
                self.page,
                url=self.args.url,
                email=email,
                password=password,
                timeout=self.args.login_timeout,
                debug_snapshot=self.args.debug_snapshot,
                reporter=lambda message: self.emit("status", message),
            )
            self.emit("login_ok", "login ok")
        except Exception as error:  # noqa: BLE001 - show operational failures in the TUI.
            await self._cleanup()
            self.emit("error", error_text(error))

    async def _ask(self, prompt: str) -> None:
        if self.page is None:
            self.emit("error", "not logged in")
            return
        try:
            answer = await ask_once(
                self.page,
                prompt,
                answer_timeout=self.args.answer_timeout,
                reporter=lambda message: self.emit("status", message),
                answer_update=lambda text: self.emit("answer_update", text),
            )
            self.emit("status", f"answer source: {answer.source}")
            self.emit("answer", answer.text)
        except Exception as error:  # noqa: BLE001 - show operational failures in the TUI.
            self.emit("error", error_text(error))

    async def _cleanup(self) -> None:
        if self.page is not None:
            try:
                await self.page.close()
            except Exception:
                pass
            self.page = None
        if self.client is not None:
            try:
                await self.client.websocket.close()
            except Exception:
                pass
            self.client = None
        if self.serve is not None:
            stop_moli(self.serve)
            self.serve = None

    def close(self) -> None:
        try:
            future = asyncio.run_coroutine_threadsafe(self._cleanup(), self.loop)
            future.result(timeout=5)
        except Exception:
            if self.serve is not None:
                stop_moli(self.serve)
                self.serve = None
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(timeout=5)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Interactive TUI for ChatGPT over Moli CDP.")
    parser.add_argument("--url", default=DEFAULT_URL, help=f"initial URL, default: {DEFAULT_URL}")
    parser.add_argument("--email", default=os.environ.get("CHATGPT_EMAIL", ""), help="pre-fill email")
    parser.add_argument("--prompt", default="", help="pre-fill the prompt input")
    parser.add_argument("--moli-bin", help="path to the moli binary; alternatively set MOLI_BIN")
    parser.add_argument("--profile-dir", help="optional Moli profile dir for cookies/localStorage")
    parser.add_argument("--user-agent", help="optional user agent passed to moli serve")
    parser.add_argument("--http-proxy", help="optional proxy passed to moli serve")
    parser.add_argument("--http-no-proxy", help="optional no-proxy list passed to moli serve")
    parser.add_argument(
        "--http-timeout",
        type=int,
        default=DEFAULT_MOLI_HTTP_TIMEOUT_MS,
        help=f"Moli request timeout in milliseconds, default: {DEFAULT_MOLI_HTTP_TIMEOUT_MS}",
    )
    parser.add_argument("--http-max-concurrent", type=int, help="optional max active fetch transfers passed to moli serve")
    parser.add_argument("--http-max-host-open", type=int, help="optional per-host fetch transfer cap passed to moli serve")
    parser.add_argument("--startup-timeout", type=float, default=DEFAULT_STARTUP_TIMEOUT)
    parser.add_argument("--login-timeout", type=float, default=DEFAULT_LOGIN_TIMEOUT)
    parser.add_argument("--answer-timeout", type=float, default=DEFAULT_ANSWER_TIMEOUT)
    parser.add_argument("--debug-snapshot", action="store_true", help="show sanitized login snapshots in the status log")
    return parser


def safe_add(stdscr: Any, y: int, x: int, text: str, attr: int = 0) -> None:
    height, width = stdscr.getmaxyx()
    if y < 0 or y >= height or x < 0 or x >= width:
        return
    try:
        stdscr.addnstr(y, x, text, max(0, width - x - 1), attr)
    except curses.error:
        pass


def init_colors() -> dict[str, int]:
    if not curses.has_colors():
        return {"title": curses.A_BOLD, "focus": curses.A_REVERSE, "error": curses.A_BOLD, "dim": curses.A_DIM}
    curses.start_color()
    curses.use_default_colors()
    curses.init_pair(1, curses.COLOR_CYAN, -1)
    curses.init_pair(2, curses.COLOR_GREEN, -1)
    curses.init_pair(3, curses.COLOR_RED, -1)
    curses.init_pair(4, curses.COLOR_YELLOW, -1)
    return {
        "title": curses.color_pair(1) | curses.A_BOLD,
        "focus": curses.color_pair(2) | curses.A_REVERSE,
        "error": curses.color_pair(3) | curses.A_BOLD,
        "dim": curses.color_pair(4),
    }


def focusable_fields(state: TuiState) -> list[str]:
    if state.awaiting_auth_code:
        return ["auth_code"]
    return ["prompt"] if state.logged_in else ["email", "password"]


def move_focus(state: TuiState) -> None:
    fields = focusable_fields(state)
    if state.focus not in fields:
        state.focus = fields[0]
        return
    state.focus = fields[(fields.index(state.focus) + 1) % len(fields)]


def current_field(state: TuiState) -> TextField:
    if state.focus not in state.fields:
        state.focus = focusable_fields(state)[0]
    return state.fields[state.focus]


def add_message(state: TuiState, kind: str, text: str) -> None:
    state.messages.append((kind, text))
    if len(state.messages) > 200:
        del state.messages[: len(state.messages) - 200]


def upsert_assistant_message(state: TuiState, kind: str, text: str) -> None:
    if state.messages and state.messages[-1][0] in {"assistant", "assistant_stream"}:
        state.messages[-1] = (kind, text)
        return
    add_message(state, kind, text)


def wrap_messages(messages: list[tuple[str, str]], width: int) -> list[tuple[str, str]]:
    rows: list[tuple[str, str]] = []
    labels = {
        "status": "[status] ",
        "error": "[error] ",
        "you": "[you] ",
        "assistant": "[chatgpt] ",
        "assistant_stream": "[chatgpt] ",
    }
    for kind, text in messages:
        label = labels.get(kind, "[log] ")
        available = max(10, width - len(label) - 2)
        parts = text.splitlines() or [""]
        for part in parts:
            wrapped = textwrap.wrap(part, width=available, replace_whitespace=False) or [""]
            for index, line in enumerate(wrapped):
                rows.append((kind, (label if index == 0 else " " * len(label)) + line))
    return rows


def draw(stdscr: Any, state: TuiState, colors: dict[str, int]) -> None:
    stdscr.erase()
    height, width = stdscr.getmaxyx()
    if height < 12 or width < 50:
        safe_add(stdscr, 0, 0, "terminal too small; need at least 50x12", colors["error"])
        stdscr.refresh()
        return

    safe_add(stdscr, 0, 2, state.title, colors["title"])
    safe_add(stdscr, 1, 2, state.status[: width - 4], colors["dim"] if not state.busy else 0)
    safe_add(stdscr, 2, 0, "-" * (width - 1))

    y = 3
    visible = focusable_fields(state)
    for name in visible:
        field_obj = state.fields[name]
        label = f"{field_obj.label}: "
        field_width = max(8, width - len(label) - 6)
        rendered = field_obj.rendered(field_width)
        attr = colors["focus"] if state.focus == name and not state.busy else 0
        safe_add(stdscr, y, 2, label)
        safe_add(stdscr, y, 2 + len(label), rendered + " " * max(0, field_width - len(rendered)), attr)
        y += 1

    y += 1
    mode = "logged in" if state.logged_in else "login"
    busy = "busy" if state.busy else "idle"
    safe_add(stdscr, y, 2, f"mode: {mode} | state: {busy}", colors["dim"])
    y += 1
    safe_add(stdscr, y, 0, "-" * (width - 1))
    y += 1

    footer = "Tab focus | Enter/Ctrl-S submit | Ctrl-L clear | Ctrl-Q quit"
    log_height = max(1, height - y - 2)
    rows = wrap_messages(state.messages, width)
    for index, (kind, line) in enumerate(rows[-log_height:]):
        attr = colors["error"] if kind == "error" else 0
        safe_add(stdscr, y + index, 2, line, attr)
    safe_add(stdscr, height - 1, 2, footer[: width - 4], colors["dim"])
    stdscr.refresh()


def drain_events(events: "queue.Queue[UiEvent]", state: TuiState) -> None:
    while True:
        try:
            event = events.get_nowait()
        except queue.Empty:
            return
        if event.kind == "status":
            state.status = event.text
            add_message(state, "status", event.text)
        elif event.kind == "login_ok":
            state.logged_in = True
            state.busy = False
            state.awaiting_auth_code = False
            state.focus = "prompt"
            state.status = event.text
            add_message(state, "status", event.text)
        elif event.kind == "auth_code_request":
            state.busy = False
            state.awaiting_auth_code = True
            state.focus = "auth_code"
            state.status = event.text
            add_message(state, "status", event.text)
        elif event.kind == "answer":
            state.busy = False
            state.status = "answer received"
            upsert_assistant_message(state, "assistant", event.text)
        elif event.kind == "answer_update":
            state.status = "receiving answer"
            upsert_assistant_message(state, "assistant_stream", event.text)
        elif event.kind == "error":
            state.busy = False
            state.awaiting_auth_code = False
            state.status = "error: " + event.text
            add_message(state, "error", event.text)


def submit_current(state: TuiState, backend: Backend) -> None:
    if state.busy:
        return
    if state.awaiting_auth_code:
        code = state.fields["auth_code"].value.strip()
        if not code:
            state.status = "auth code is required"
            add_message(state, "error", state.status)
            return
        state.fields["auth_code"].value = ""
        state.awaiting_auth_code = False
        state.busy = True
        state.status = "submitting auth code"
        add_message(state, "status", "submitting auth code")
        if hasattr(backend, "submit_auth_code"):
            backend.submit_auth_code(code)
        else:
            state.busy = False
            state.status = "auth code input is not supported by this backend"
            add_message(state, "error", state.status)
        return
    if not state.logged_in:
        email = state.fields["email"].value.strip()
        password = state.fields["password"].value
        if not email:
            state.status = "email is required"
            add_message(state, "error", state.status)
            return
        if not password and bool(getattr(backend, "requires_password", True)):
            state.status = "password is required"
            add_message(state, "error", state.status)
            return
        state.busy = True
        state.status = "logging in"
        add_message(state, "status", "logging in")
        backend.login(email, password)
        return

    prompt = state.fields["prompt"].value.strip()
    if not prompt:
        state.status = "prompt is empty"
        return
    state.fields["prompt"].value = ""
    state.busy = True
    state.status = "sending prompt"
    add_message(state, "you", prompt)
    backend.ask(prompt)


def run_tui_with_backend(stdscr: Any, args: argparse.Namespace, backend_cls: Any = Backend) -> None:
    try:
        curses.curs_set(0)
    except curses.error:
        pass
    stdscr.nodelay(True)
    stdscr.keypad(True)
    colors = init_colors()
    events: "queue.Queue[UiEvent]" = queue.Queue()
    state = TuiState(
        fields={
            "email": TextField("email", "Email", args.email),
            "password": TextField("password", "Password", os.environ.get("CHATGPT_PASSWORD", ""), secret=True),
            "auth_code": TextField("auth_code", "Auth Code"),
            "prompt": TextField("prompt", "Prompt", args.prompt),
        },
        title=str(getattr(args, "tui_title", "Moli ChatGPT CDP TUI")),
    )
    backend = backend_cls(args, events)
    try:
        while True:
            drain_events(events, state)
            draw(stdscr, state, colors)
            try:
                ch = stdscr.get_wch()
            except curses.error:
                time.sleep(0.05)
                continue
            if ch in (17, "\x11"):  # Ctrl-Q
                return
            if ch in (12, "\x0c"):  # Ctrl-L
                state.messages.clear()
                state.status = "cleared"
                continue
            if ch in (9, "\t", curses.KEY_BTAB):
                move_focus(state)
                continue
            if ch in ENTER_KEYS or ch in (19, "\x13"):  # Enter or Ctrl-S
                submit_current(state, backend)
                continue
            if state.busy:
                continue
            field_obj = current_field(state)
            if ch in BACKSPACE_KEYS:
                field_obj.backspace()
            elif isinstance(ch, str):
                field_obj.put(ch)
            elif 0 <= ch < 256:
                field_obj.put(ch)
    finally:
        backend.close()


def run_tui(stdscr: Any, args: argparse.Namespace) -> None:
    run_tui_with_backend(stdscr, args, Backend)


def main() -> int:
    args = build_parser().parse_args()
    try:
        curses.wrapper(run_tui, args)
    except DemoError as error:
        print(f"error: {error}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
