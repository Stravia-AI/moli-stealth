from __future__ import annotations

import os
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from chatgpt_auth_bridge import (
    auth_proxy_options,
    chromium_cookie_params,
    default_moli_chatgpt_profile_dir,
    load_profile_user_agent,
    prepare_profile_dir,
    save_profile_user_agent,
)
from chatgpt_cdp_demo import CHATGPT_HELPER_JS
from chatgpt_playwright_tui import PlaywrightBackend, build_parser


class ChatGPTAuthBridgeTests(unittest.TestCase):
    def test_default_profile_uses_xdg_data_home(self) -> None:
        with mock.patch.dict(
            os.environ, {"XDG_DATA_HOME": "/tmp/moli-xdg-data"}, clear=False
        ):
            self.assertEqual(
                default_moli_chatgpt_profile_dir(),
                Path("/tmp/moli-xdg-data/moli/chatgpt-profile"),
            )

    def test_profile_metadata_round_trips_with_private_permissions(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            profile = Path(temporary) / "profile"
            prepared = prepare_profile_dir(profile)
            save_profile_user_agent(prepared, "Test Browser/1.0")

            self.assertEqual(load_profile_user_agent(prepared), "Test Browser/1.0")
            self.assertEqual(stat.S_IMODE(prepared.stat().st_mode), 0o700)
            self.assertEqual(
                stat.S_IMODE(
                    (prepared / ".chatgpt-playground-auth.json").stat().st_mode
                ),
                0o600,
            )

    def test_cookie_conversion_keeps_only_chatgpt_auth_domains(self) -> None:
        converted = chromium_cookie_params(
            [
                {
                    "name": "session",
                    "value": "secret",
                    "domain": ".chatgpt.com",
                    "path": "/",
                    "expires": 12345.0,
                    "httpOnly": True,
                    "secure": True,
                    "sameSite": "Lax",
                },
                {
                    "name": "auth",
                    "value": "secret-2",
                    "domain": "auth.openai.com",
                    "path": "/",
                    "expires": -1,
                    "httpOnly": True,
                    "secure": True,
                    "sameSite": "None",
                },
                {
                    "name": "unrelated",
                    "value": "ignored",
                    "domain": "example.com",
                    "path": "/",
                },
            ]
        )

        self.assertEqual([cookie["name"] for cookie in converted], ["session", "auth"])
        self.assertEqual(converted[0]["expires"], 12345.0)
        self.assertNotIn("expires", converted[1])

    def test_auth_chromium_inherits_proxy_environment(self) -> None:
        args = mock.Mock(http_proxy=None, http_no_proxy=None)
        with mock.patch.dict(
            os.environ,
            {
                "HTTPS_PROXY": "http://proxy.example:8080",
                "NO_PROXY": "localhost,127.0.0.1",
            },
            clear=True,
        ):
            self.assertEqual(
                auth_proxy_options(args),
                ("http://proxy.example:8080", "localhost,127.0.0.1"),
            )

    def test_helper_ignores_google_hidden_password_and_avoids_double_submit(
        self,
    ) -> None:
        self.assertIn("hiddenpassword", CHATGPT_HELPER_JS)
        self.assertIn("style?.display === 'none'", CHATGPT_HELPER_JS)
        self.assertIn(
            "return { ok: true, reactSubmit: true, clickedSubmit: false };",
            CHATGPT_HELPER_JS,
        )

    def test_playwright_tui_defaults_to_bridge_and_persistent_profile(self) -> None:
        with mock.patch.dict(
            os.environ, {"XDG_DATA_HOME": "/tmp/moli-tui-data"}, clear=False
        ):
            args = build_parser().parse_args([])

        self.assertEqual(args.auth_backend, "chromium-bridge")
        self.assertEqual(args.profile_dir, "/tmp/moli-tui-data/moli/chatgpt-profile")
        self.assertFalse(args.try_email_verification)
        self.assertFalse(PlaywrightBackend.requires_password)


if __name__ == "__main__":
    unittest.main()
