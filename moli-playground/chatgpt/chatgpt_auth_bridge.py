from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import shutil
import signal
import subprocess
import tempfile
import time
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from chatgpt_cdp_demo import DemoError, read_json_url_no_proxy, reserve_local_port
from chatgpt_playwright_core import (
    PlaywrightChatGPTSession,
    auth_blocking_reason_from_url,
)

Reporter = Callable[[str], None]
PROFILE_METADATA_FILE = ".chatgpt-playground-auth.json"


@dataclass(frozen=True)
class ChromiumAuthState:
    cookies: list[dict[str, object]]
    local_storage: list[list[str]]
    user_agent: str


def default_moli_chatgpt_profile_dir() -> Path:
    data_home = os.environ.get("XDG_DATA_HOME")
    root = (
        Path(data_home).expanduser() if data_home else Path.home() / ".local" / "share"
    )
    return root / "moli" / "chatgpt-profile"


def prepare_profile_dir(profile_dir: str | os.PathLike[str]) -> Path:
    path = Path(profile_dir).expanduser().resolve()
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.chmod(0o700)
    return path


def load_profile_user_agent(profile_dir: str | os.PathLike[str]) -> str:
    path = Path(profile_dir).expanduser().resolve() / PROFILE_METADATA_FILE
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, TypeError):
        return ""
    if not isinstance(payload, dict):
        return ""
    user_agent = payload.get("user_agent")
    return user_agent if isinstance(user_agent, str) else ""


def save_profile_user_agent(
    profile_dir: str | os.PathLike[str], user_agent: str
) -> None:
    directory = prepare_profile_dir(profile_dir)
    target = directory / PROFILE_METADATA_FILE
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f"{PROFILE_METADATA_FILE}.", dir=directory
    )
    temporary = Path(temporary_name)
    try:
        os.fchmod(descriptor, 0o600)
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(
                {"user_agent": user_agent}, handle, ensure_ascii=False, sort_keys=True
            )
            handle.write("\n")
        temporary.replace(target)
        target.chmod(0o600)
    except BaseException:
        try:
            os.close(descriptor)
        except OSError:
            pass
        temporary.unlink(missing_ok=True)
        raise


def chromium_cookie_params(cookies: list[dict[str, Any]]) -> list[dict[str, object]]:
    result: list[dict[str, object]] = []
    for cookie in cookies:
        domain = str(cookie.get("domain") or "")
        if "chatgpt.com" not in domain and "openai.com" not in domain:
            continue
        item: dict[str, object] = {
            "name": str(cookie.get("name") or ""),
            "value": str(cookie.get("value") or ""),
            "domain": domain,
            "path": str(cookie.get("path") or "/"),
            "secure": bool(cookie.get("secure")),
            "httpOnly": bool(cookie.get("httpOnly")),
        }
        expires = cookie.get("expires")
        if isinstance(expires, (int, float)) and expires > 0:
            item["expires"] = expires
        same_site = cookie.get("sameSite")
        if same_site in {"Strict", "Lax", "None"}:
            item["sameSite"] = same_site
        result.append(item)
    return result


def resolve_auth_chromium(args: argparse.Namespace) -> Path:
    configured = str(
        getattr(args, "auth_chromium_bin", "")
        or getattr(args, "chromium_bin", "")
        or os.environ.get("CHROMIUM_BIN")
        or ""
    )
    if configured:
        candidate = Path(configured).expanduser().resolve()
        if candidate.is_file():
            return candidate
        raise DemoError(f"authentication Chromium binary does not exist: {candidate}")
    for name in (
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ):
        resolved = shutil.which(name)
        if resolved:
            return Path(resolved).resolve()
    raise DemoError(
        "Chromium is required for ChatGPT authentication; pass --auth-chromium-bin"
    )


def resolve_xvfb() -> Path:
    configured = os.environ.get("XVFB_BIN") or ""
    if configured:
        candidate = Path(configured).expanduser().resolve()
        if candidate.is_file():
            return candidate
        raise DemoError(f"Xvfb binary does not exist: {candidate}")
    resolved = shutil.which("Xvfb")
    if resolved:
        return Path(resolved).resolve()
    raise DemoError("Xvfb is required for headful ChatGPT authentication")


def auth_proxy_options(args: argparse.Namespace) -> tuple[str, str]:
    proxy = str(
        getattr(args, "http_proxy", "")
        or os.environ.get("HTTPS_PROXY")
        or os.environ.get("https_proxy")
        or os.environ.get("HTTP_PROXY")
        or os.environ.get("http_proxy")
        or ""
    )
    no_proxy = str(
        getattr(args, "http_no_proxy", "")
        or os.environ.get("NO_PROXY")
        or os.environ.get("no_proxy")
        or ""
    )
    return proxy, no_proxy


def terminate_process_group(process: subprocess.Popen[Any] | None) -> None:
    if process is None or process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=3)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait(timeout=3)


def remove_auth_profile(path: Path | None) -> None:
    if path is None or not path.exists():
        return
    resolved = path.resolve()
    temporary_root = Path(tempfile.gettempdir()).resolve()
    if resolved.parent != temporary_root or not resolved.name.startswith(
        "moli-chatgpt-auth-chromium."
    ):
        raise DemoError(
            f"refusing to remove unexpected authentication profile: {resolved}"
        )
    shutil.rmtree(resolved)


def start_xvfb() -> tuple[subprocess.Popen[str], str]:
    process = subprocess.Popen(  # noqa: S603 - executable is resolved from an explicit trusted path.
        [
            str(resolve_xvfb()),
            "-displayfd",
            "1",
            "-screen",
            "0",
            "1440x1000x24",
            "-nolisten",
            "tcp",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        start_new_session=True,
    )
    if process.stdout is None:
        terminate_process_group(process)
        raise DemoError("Xvfb stdout pipe is unavailable")
    display_number = process.stdout.readline().strip()
    if not display_number or process.poll() is not None:
        terminate_process_group(process)
        raise DemoError("Xvfb did not allocate a display for ChatGPT authentication")
    return process, f":{display_number}"


async def wait_for_chromium_cdp(
    endpoint: str, process: subprocess.Popen[bytes], timeout: float
) -> str:
    deadline = time.monotonic() + timeout
    last_error: BaseException | None = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return_code = process.returncode
            raise DemoError(
                f"authentication Chromium exited before CDP startup: rc={return_code}"
            )
        try:
            version = await asyncio.to_thread(
                read_json_url_no_proxy, endpoint + "/json/version"
            )
            websocket_url = version.get("webSocketDebuggerUrl")
            if isinstance(websocket_url, str) and websocket_url:
                return websocket_url
        except BaseException as error:  # noqa: BLE001 - report the final startup error.
            last_error = error
        await asyncio.sleep(0.1)
    raise DemoError(
        "timed out waiting for authentication Chromium CDP; "
        f"last_error={type(last_error).__name__ if last_error else 'none'}"
    )


async def click_visible_text(page: Any, patterns: tuple[str, ...]) -> bool:
    interactive = page.locator("button, [role='button'], a, label")
    for pattern in patterns:
        expression = re.compile(pattern, re.IGNORECASE)
        for candidates in (
            interactive.filter(has_text=expression),
            page.get_by_text(expression),
        ):
            for index in range(min(await candidates.count(), 12)):
                candidate = candidates.nth(index)
                try:
                    if await candidate.is_visible(
                        timeout=500
                    ) and await candidate.is_enabled(timeout=500):
                        await candidate.click(timeout=5000)
                        return True
                except Exception:  # noqa: BLE001, S112 - probe the next visible candidate.
                    continue
    return False


async def select_push_mfa(
    session: PlaywrightChatGPTSession, reporter: Reporter
) -> None:
    if session.page is None:
        raise DemoError("MFA page is unavailable")
    push_patterns = (
        r"push notification",
        r"chatgpt app",
        r"approve.*device",
        r"push",
    )
    clicked = await click_visible_text(session.page, push_patterns)
    if not clicked:
        expanded = await click_visible_text(
            session.page,
            (
                r"try another method",
                r"use another method",
                r"different method",
                r"other (?:options|methods)",
            ),
        )
        if expanded:
            reporter("expand MFA methods")
            await asyncio.sleep(1)
            clicked = await click_visible_text(session.page, push_patterns)
    if not clicked:
        raise DemoError("Push approval is not available on the ChatGPT MFA page")
    reporter("select Push approval")
    await asyncio.sleep(1)
    await click_visible_text(
        session.page, (r"^continue$", r"send push", r"send notification")
    )


async def submit_external_login_once(
    session: PlaywrightChatGPTSession,
    email: str,
    password: str,
) -> None:
    state = await session.navigate_to_initial_login_state()
    if state.get("loggedIn"):
        return
    state = await session.navigate_to_login_form()
    if state.get("blockingReason"):
        raise DemoError(
            f"login blocked before credentials: {state.get('blockingReason')}"
        )
    if state.get("hasEmailInput"):
        session.report("fill email in authentication Chromium")
        await session.accept_cookie_consent()
        await session.fill_email_native(email)
        state = await session.wait_for_state_after_email_submit(
            timeout=60, label="password form"
        )
    if state.get("loggedIn"):
        return
    if not state.get("hasPasswordInput"):
        raise DemoError(
            f"password input did not appear; blocking={state.get('blockingReason')!r}"
        )
    await session.wait_for_auth_password_runtime_ready(timeout=20)
    session.report("submit password once in authentication Chromium")
    await session.fill_password_native(password)


async def wait_for_external_login(
    session: PlaywrightChatGPTSession,
    args: argparse.Namespace,
    reporter: Reporter,
) -> None:
    deadline = time.monotonic() + max(1.0, float(args.login_timeout))
    selected_push = False
    tried_email_verification = False
    reported_device_approval = False
    while time.monotonic() < deadline:
        try:
            state = await session.helper("loginState", timeout=15)
        except Exception:  # noqa: BLE001 - navigation can invalidate the helper realm.
            await asyncio.sleep(0.5)
            continue
        if state.get("loggedIn"):
            reporter("authentication approved")
            return
        reason = state.get("blockingReason") or auth_blocking_reason_from_url(
            str(state.get("url") or "")
        )
        if reason == "mfa" and not selected_push:
            await select_push_mfa(session, reporter)
            selected_push = True
            await asyncio.sleep(1)
            continue
        if reason == "device-approval":
            if (
                bool(getattr(args, "try_email_verification", False))
                and not tried_email_verification
            ):
                tried_email_verification = True
                if await session.click_try_with_email_verification():
                    await asyncio.sleep(1)
                    continue
            if not reported_device_approval:
                reporter("approve the new login in the ChatGPT app")
                reported_device_approval = True
            await asyncio.sleep(1)
            continue
        if reason in {"email-verification", "verification-code"}:
            if not session.has_auth_code_source():
                raise DemoError(
                    "ChatGPT requested an email code but no auth-code input "
                    "is configured"
                )
            code = await session.read_auth_code()
            reporter("submit ChatGPT email verification code")
            await session.fill_auth_code_native(code)
            session._auth_code_cache = None
            await asyncio.sleep(1.5)
            continue
        if reason:
            raise DemoError(
                f"ChatGPT authentication reached an unsupported step: {reason}"
            )
        await asyncio.sleep(0.5)
    if reported_device_approval:
        raise DemoError("timed out waiting for ChatGPT device approval")
    raise DemoError("timed out waiting for ChatGPT authentication")


async def export_auth_state(session: PlaywrightChatGPTSession) -> ChromiumAuthState:
    if session.page is None or session.context is None:
        raise DemoError("authentication Chromium page is unavailable")
    if not session.page.url.startswith("https://chatgpt.com/"):
        await session.page.goto(
            "https://chatgpt.com/", wait_until="domcontentloaded", timeout=120_000
        )
    await session.wait_for_document_settle(timeout=10)
    cookies = chromium_cookie_params(await session.context.cookies())
    local_storage = await session.page.evaluate("Object.entries(localStorage)")
    user_agent = await session.page.evaluate("navigator.userAgent")
    if not isinstance(local_storage, list) or not all(
        isinstance(entry, list)
        and len(entry) == 2
        and all(isinstance(value, str) for value in entry)
        for entry in local_storage
    ):
        raise DemoError("authentication Chromium returned invalid localStorage state")
    if not isinstance(user_agent, str) or not user_agent:
        raise DemoError("authentication Chromium returned an empty user agent")
    if not cookies:
        raise DemoError("authentication Chromium returned no ChatGPT cookies")
    return ChromiumAuthState(
        cookies=cookies, local_storage=local_storage, user_agent=user_agent
    )


async def authenticate_in_external_chromium(
    args: argparse.Namespace,
    email: str,
    password: str,
    reporter: Reporter,
) -> ChromiumAuthState:
    from playwright.async_api import async_playwright

    profile: Path | None = None
    xvfb: subprocess.Popen[str] | None = None
    chromium: subprocess.Popen[bytes] | None = None
    session: PlaywrightChatGPTSession | None = None
    try:
        profile = Path(tempfile.mkdtemp(prefix="moli-chatgpt-auth-chromium."))
        profile.chmod(0o700)
        xvfb, display = start_xvfb()
        port = reserve_local_port()
        endpoint = f"http://127.0.0.1:{port}"
        command = [
            str(resolve_auth_chromium(args)),
            f"--remote-debugging-port={port}",
            "--remote-debugging-address=127.0.0.1",
            "--remote-allow-origins=*",
            f"--user-data-dir={profile}",
            "--no-first-run",
            "--no-default-browser-check",
            "--password-store=basic",
            "--window-size=1440,1000",
        ]
        proxy, no_proxy = auth_proxy_options(args)
        if proxy:
            command.append(f"--proxy-server={proxy}")
        if no_proxy:
            command.append(f"--proxy-bypass-list={no_proxy.replace(',', ';')}")
        if hasattr(os, "geteuid") and os.geteuid() == 0:
            command.append("--no-sandbox")
        command.append("about:blank")
        environment = os.environ.copy()
        environment["DISPLAY"] = display
        reporter("start authentication Chromium under Xvfb")
        chromium = subprocess.Popen(  # noqa: ASYNC220, S603 - trusted binary; explicit process-group cleanup.
            command,
            env=environment,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        websocket_url = await wait_for_chromium_cdp(
            endpoint, chromium, float(args.startup_timeout)
        )
        playwright = await async_playwright().start()
        auth_args = argparse.Namespace(**vars(args))
        auth_args.backend = "chromium"
        session = PlaywrightChatGPTSession(auth_args, reporter=reporter)
        session.playwright = playwright
        session.browser = await playwright.chromium.connect_over_cdp(websocket_url)
        session.context = session.browser.contexts[0]
        await session.finish_page_setup()
        if session.page is None:
            raise DemoError("authentication Chromium did not expose a page target")
        webdriver = await session.page.evaluate("navigator.webdriver")
        if webdriver:
            raise DemoError(
                "authentication Chromium unexpectedly exposes navigator.webdriver"
            )
        await submit_external_login_once(session, email, password)
        await wait_for_external_login(session, auth_args, reporter)
        state = await export_auth_state(session)
        reporter(f"captured ChatGPT session ({len(state.cookies)} cookies)")
        return state
    finally:
        if session is not None:
            await session.close()
        terminate_process_group(chromium)
        terminate_process_group(xvfb)
        remove_auth_profile(profile)


async def import_auth_state(
    session: PlaywrightChatGPTSession,
    state: ChromiumAuthState,
) -> None:
    if session.page is None or session.context is None:
        raise DemoError("Moli page is unavailable for authentication import")
    cdp = await session.context.new_cdp_session(session.page)
    await cdp.send("Network.enable")
    await cdp.send("Network.setCookies", {"cookies": state.cookies})
    await session.page.goto(
        "https://chatgpt.com/", wait_until="domcontentloaded", timeout=180_000
    )
    await session.page.evaluate(
        """entries => {
          for (const [key, value] of entries) localStorage.setItem(key, value);
        }""",
        state.local_storage,
    )
    await session.page.reload(wait_until="domcontentloaded", timeout=180_000)
    await session.require_existing_session()


async def create_authenticated_moli_session(
    args: argparse.Namespace,
    email: str,
    password: str,
    reporter: Reporter = print,
) -> PlaywrightChatGPTSession:
    profile_dir = str(getattr(args, "profile_dir", "") or "")
    if profile_dir:
        prepare_profile_dir(profile_dir)
        if not getattr(args, "user_agent", None):
            stored_user_agent = load_profile_user_agent(profile_dir)
            if stored_user_agent:
                args.user_agent = stored_user_agent

    existing = PlaywrightChatGPTSession(args, reporter=reporter)
    try:
        await existing.start()
    except BaseException:
        await existing.close()
        raise
    try:
        await existing.require_existing_session()
    except DemoError:
        await existing.close()
        reporter("Moli profile needs ChatGPT authentication")
    else:
        reporter("reuse logged-in Moli profile")
        return existing

    if not password:
        raise DemoError(
            "password is required because the Moli profile is not logged in"
        )

    state = await authenticate_in_external_chromium(args, email, password, reporter)
    if not getattr(args, "user_agent", None):
        args.user_agent = state.user_agent

    authenticated = PlaywrightChatGPTSession(args, reporter=reporter)
    try:
        await authenticated.start()
        await import_auth_state(authenticated, state)
        if profile_dir:
            save_profile_user_agent(profile_dir, state.user_agent)
        reporter("ChatGPT authentication imported into Moli")
        return authenticated
    except BaseException:
        await authenticated.close()
        raise
