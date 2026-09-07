from __future__ import annotations

import unittest

from moli_benchmark.browser_oxide_scoring import classify, pass_flags


def sized_ascii(seed: str, byte_length: int) -> str:
    encoded_length = len(seed.encode("utf-8"))
    if encoded_length > byte_length:
        raise ValueError("seed exceeds requested byte length")
    return seed + ("x" * (byte_length - encoded_length))


class BrowserOxideScoringTests(unittest.TestCase):
    def test_result_contract_and_thin_body_boundary(self) -> None:
        thin = classify("x" * 999)
        rendered = classify("x" * 1000)

        self.assertEqual(
            thin,
            {"tag": "THIN-BODY", "len": 999, "strict_pass": False, "loose_pass": False},
        )
        self.assertEqual(rendered["tag"], "L3-RENDERED")
        self.assertTrue(rendered["loose_pass"])
        self.assertFalse(rendered["strict_pass"])

    def test_strict_floor_is_decimal_15000_utf8_bytes(self) -> None:
        below = classify(("é" * 7499) + "a")
        at_floor = classify("é" * 7500)

        self.assertEqual(below["len"], 14_999)
        self.assertEqual(at_floor["len"], 15_000)
        self.assertFalse(below["strict_pass"])
        self.assertTrue(at_floor["strict_pass"])
        self.assertTrue(below["loose_pass"])
        self.assertTrue(at_floor["loose_pass"])
        self.assertEqual(pass_flags("L3-RENDERED", 15_000), (True, True))
        self.assertEqual(pass_flags("BLOCKED", 100_000), (False, False))

    def test_unicode_lowercase_drives_marker_matching(self) -> None:
        result = classify("<p>I’M NOT A ROBOT — CAPTCHA</p>")

        self.assertEqual(result["tag"], "captcha-CHL")
        self.assertEqual(result["len"], len("<p>I’M NOT A ROBOT — CAPTCHA</p>".encode("utf-8")))

    def test_any_size_structural_markers_remain_challenges(self) -> None:
        for marker, tag in (
            ("CF-BROWSER-VERIFICATION", "ManagedChallenge-CHL"),
            ("window._CF_CHL_OPT={}", "ManagedChallenge-CHL"),
            ("/_SEC/CP_CHALLENGE", "SecCpt-CHL"),
            ("DDCAPTCHAENCODED", "Interstitial-CHL"),
        ):
            with self.subTest(marker=marker):
                self.assertEqual(classify(sized_ascii(marker, 120_000))["tag"], tag)

    def test_large_challenge_phrases_and_sdk_literals_are_not_false_positives(self) -> None:
        for marker in (
            "Just a moment",
            "checking your browser",
            "captcha-delivery.com",
            "press &amp; hold",
            "pardon our interruption",
            "akam/13 sensor_data",
            "_abck",
            "_kpsdk",
            "ips.js",
            "_pxhd",
            "px-captcha",
            "captcha verify you are human",
            "403 forbidden",
            "access denied",
        ):
            with self.subTest(marker=marker):
                result = classify(sized_ascii(marker, 30 * 1024))
                self.assertEqual(result["tag"], "L3-RENDERED")
                self.assertTrue(result["strict_pass"])

    def test_phrase_and_small_body_gate_is_strictly_below_30_kib(self) -> None:
        self.assertEqual(
            classify(sized_ascii("checking your browser", (30 * 1024) - 1))["tag"],
            "ManagedChallenge-CHL",
        )
        self.assertEqual(
            classify(sized_ascii("checking your browser", 30 * 1024))["tag"],
            "L3-RENDERED",
        )
        self.assertEqual(
            classify(sized_ascii("_pxhd", (30 * 1024) - 1))["tag"],
            "BehaviorChallenge-CHL",
        )
        self.assertEqual(classify(sized_ascii("_pxhd", 30 * 1024))["tag"], "L3-RENDERED")

    def test_small_legitimate_sensor_bootstrap_requires_challenge_cosignal(self) -> None:
        analytics = sized_ascii('<script src="/akam/13/sdk.js"></script>', 7_900)
        challenge = sized_ascii('<script src="/akam/13/sdk.js"></script><form id="bm-verify">', 7_900)

        self.assertEqual(classify(analytics)["tag"], "L3-RENDERED")
        self.assertEqual(classify(challenge)["tag"], "SensorChallenge-CHL")

    def test_small_legitimate_invisible_captcha_badge_requires_interaction(self) -> None:
        badge = sized_ascii(
            '<style>.grecaptcha-badge{display:none}</style><textarea name="g-recaptcha-response">',
            9_600,
        )
        widget = sized_ascii("CAPTCHA: verify you are human", 9_600)

        self.assertEqual(classify(badge)["tag"], "L3-RENDERED")
        self.assertEqual(classify(widget)["tag"], "captcha-CHL")

    def test_aws_waf_envelope_requires_active_loader_at_any_size(self) -> None:
        solved = sized_ascii("window.awsWafCookieDomainList=['example.com']", 100_000)
        unsolved_small = "window.gokuProps={};https://token.awswaf.com/challenge.js"
        unsolved_large = sized_ascii(
            "window.awsWafCookieDomainList=[];AwsWafIntegration.checkForceRefresh()",
            100_000,
        )

        self.assertEqual(classify(solved)["tag"], "L3-RENDERED")
        self.assertEqual(classify(unsolved_small)["tag"], "AWS-WAF-CHL")
        self.assertEqual(classify(unsolved_large)["tag"], "AWS-WAF-CHL")

    def test_blocked_word_has_separate_five_kib_gate(self) -> None:
        self.assertEqual(classify(sized_ascii("blocked", (5 * 1024) - 1))["tag"], "BLOCKED")
        self.assertEqual(classify(sized_ascii("blocked", 5 * 1024))["tag"], "L3-RENDERED")

    def test_first_match_order_preserves_vendor_attribution(self) -> None:
        self.assertEqual(
            classify("_cf_chl_opt captcha-delivery.com px-captcha blocked")["tag"],
            "ManagedChallenge-CHL",
        )
        self.assertEqual(
            classify("captcha-delivery.com px-captcha access denied")["tag"],
            "Interstitial-CHL",
        )
        self.assertEqual(classify("px-captcha captcha")["tag"], "BehaviorChallenge-CHL")


if __name__ == "__main__":
    unittest.main()
