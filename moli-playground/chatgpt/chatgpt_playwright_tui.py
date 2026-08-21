#!/usr/bin/env python3
from __future__ import annotations

import argparse
import asyncio
import curses
import os
import queue
import threading

from chatgpt_auth_bridge import (
    create_authenticated_moli_session,
    default_moli_chatgpt_profile_dir,
)
from chatgpt_cdp_demo import (
    DEFAULT_ANSWER_TIMEOUT,
    DEFAULT_LOGIN_TIMEOUT,
    DEFAULT_MOLI_HTTP_TIMEOUT_MS,
    DEFAULT_STARTUP_TIMEOUT,
    DEFAULT_URL,
)
from chatgpt_cdp_tui import UiEvent, error_text, run_tui_with_backend
from chatgpt_playwright_core import PlaywrightChatGPTSession

TUI_LOGIN_TIMEOUT = max(300.0, DEFAULT_LOGIN_TIMEOUT)
TUI_TRY_EMAIL_VERIFICATION = False


class PlaywrightBackend:
    requires_password = False

    def __init__(
        self, args: argparse.Namespace, events: "queue.Queue[UiEvent]"
    ) -> None:
        self.args = args
        self.requires_password = not (
            args.backend == "moli" and args.auth_backend == "chromium-bridge"
        )
        self.events = events
        self.auth_codes: "queue.Queue[str]" = queue.Queue()
        self.loop = asyncio.new_event_loop()
        self.thread = threading.Thread(target=self._run_loop, daemon=True)
        self.started = threading.Event()
        self.session: PlaywrightChatGPTSession | None = None
        self.thread.start()
        if not self.started.wait(timeout=2):
            raise RuntimeError("Playwright backend event loop did not start")

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

    def submit_auth_code(self, code: str) -> None:
        self.auth_codes.put(code)

    async def read_auth_code_from_tui(self) -> str:
        self.emit("auth_code_request", "enter auth code")
        return await asyncio.to_thread(self.auth_codes.get)

    async def _login(self, email: str, password: str) -> None:
        try:
            self.args.auth_code_provider = self.read_auth_code_from_tui

            def reporter(message: str) -> None:
                self.emit("status", message)

            if (
                self.args.backend == "moli"
                and self.args.auth_backend == "chromium-bridge"
            ):
                self.session = await create_authenticated_moli_session(
                    self.args,
                    email,
                    password,
                    reporter=reporter,
                )
            else:
                self.session = PlaywrightChatGPTSession(self.args, reporter=reporter)
                await self.session.start()
                await self.session.login(email, password)
            self.emit("login_ok", "login ok")
        except Exception as error:  # noqa: BLE001 - show operational failures in the TUI.
            await self._cleanup()
            self.emit("error", error_text(error))

    async def _ask(self, prompt: str) -> None:
        if self.session is None:
            self.emit("error", "not logged in")
            return
        try:
            answer = await self.session.ask_once(
                prompt,
                answer_update=lambda text: self.emit("answer_update", text),
            )
            self.emit("status", f"answer source: {answer.source}")
            self.emit("answer", answer.text)
        except Exception as error:  # noqa: BLE001 - show operational failures in the TUI.
            self.emit("error", error_text(error))

    async def _cleanup(self) -> None:
        if self.session is not None:
            try:
                await self.session.close()
            except Exception:
                pass
            self.session = None

    def close(self) -> None:
        self.auth_codes.put("")
        try:
            future = asyncio.run_coroutine_threadsafe(self._cleanup(), self.loop)
            future.result(timeout=5)
        except Exception:
            pass
        self.loop.call_soon_threadsafe(self.loop.stop)
        self.thread.join(timeout=5)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Interactive Playwright TUI for ChatGPT over Moli CDP."
    )
    parser.add_argument(
        "--backend",
        choices=("moli", "chromium"),
        default="moli",
        help="browser backend, default: moli",
    )
    parser.add_argument(
        "--auth-backend",
        choices=("chromium-bridge", "moli"),
        default="chromium-bridge",
        help="fresh-login backend when chatting through Moli, default: chromium-bridge",
    )
    parser.add_argument(
        "--url", default=DEFAULT_URL, help=f"initial URL, default: {DEFAULT_URL}"
    )
    parser.add_argument(
        "--email", default=os.environ.get("CHATGPT_EMAIL", ""), help="pre-fill email"
    )
    parser.add_argument("--prompt", default="", help="pre-fill the prompt input")
    parser.add_argument(
        "--auth-code",
        default=os.environ.get("CHATGPT_AUTH_CODE", ""),
        help="verification code for email auth fallback",
    )
    parser.add_argument(
        "--try-email-verification",
        dest="try_email_verification",
        action="store_true",
        default=TUI_TRY_EMAIL_VERIFICATION,
        help="click Try with email on device approval pages, default: disabled",
    )
    parser.add_argument(
        "--no-try-email-verification",
        dest="try_email_verification",
        action="store_false",
        help="wait for device approval instead of switching to email-code verification",
    )
    parser.add_argument(
        "--no-reload-recovery",
        action="store_true",
        help="do not reload the conversation to recover persisted answers; exposes live DOM render failures",
    )
    parser.add_argument(
        "--live-trace",
        action="store_true",
        help="record in-page fetch/WebSocket/stream/DOM trace events for ChatGPT live-render debugging",
    )
    parser.add_argument(
        "--live-trace-output",
        help="append sanitized live-trace summaries as JSON Lines to this path",
    )
    parser.add_argument(
        "--moli-bin", help="path to the moli binary; alternatively set MOLI_BIN"
    )
    parser.add_argument(
        "--chromium-bin",
        help="path to a Chromium/Chrome executable for --backend chromium",
    )
    parser.add_argument(
        "--auth-chromium-bin",
        help="Chromium/Chrome executable used only for Moli login bootstrap",
    )
    parser.add_argument(
        "--headful",
        action="store_true",
        help="run Chromium with a visible browser window",
    )
    parser.add_argument(
        "--profile-dir",
        default=os.environ.get(
            "MOLI_CHATGPT_PROFILE_DIR", str(default_moli_chatgpt_profile_dir())
        ),
        help="persistent Moli login profile, default: $MOLI_CHATGPT_PROFILE_DIR or the user data directory",
    )
    parser.add_argument("--user-agent", help="optional user agent passed to moli serve")
    parser.add_argument("--http-proxy", help="optional proxy passed to moli serve")
    parser.add_argument(
        "--http-no-proxy", help="optional no-proxy list passed to moli serve"
    )
    parser.add_argument(
        "--http-timeout",
        type=int,
        default=DEFAULT_MOLI_HTTP_TIMEOUT_MS,
        help=f"Moli request timeout in milliseconds, default: {DEFAULT_MOLI_HTTP_TIMEOUT_MS}",
    )
    parser.add_argument(
        "--http-max-concurrent",
        type=int,
        help="optional max active fetch transfers passed to moli serve",
    )
    parser.add_argument(
        "--http-max-host-open",
        type=int,
        help="optional per-host fetch transfer cap passed to moli serve",
    )
    parser.add_argument(
        "--startup-timeout", type=float, default=DEFAULT_STARTUP_TIMEOUT
    )
    parser.add_argument("--login-timeout", type=float, default=TUI_LOGIN_TIMEOUT)
    parser.add_argument("--answer-timeout", type=float, default=DEFAULT_ANSWER_TIMEOUT)
    parser.add_argument(
        "--debug-snapshot",
        action="store_true",
        help="accepted for parity with the raw-CDP TUI",
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()
    args.tui_title = "Moli ChatGPT Playwright TUI"
    curses.wrapper(run_tui_with_backend, args, PlaywrightBackend)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
