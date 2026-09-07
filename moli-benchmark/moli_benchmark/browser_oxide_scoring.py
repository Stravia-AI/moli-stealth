from __future__ import annotations

from typing import TypedDict

INTERSTITIAL_MAX_BYTES = 30 * 1024
BLOCKED_WORD_MAX_BYTES = 5 * 1024
THIN_BODY_MAX_BYTES = 1000
STRICT_PASS_MIN_BYTES = 15_000

_UNAMBIGUOUS = (
    ("cf-browser-verification", "ManagedChallenge-CHL"),
    ("_cf_chl_opt", "ManagedChallenge-CHL"),
    ("/_sec/cp_challenge", "SecCpt-CHL"),
    ("ddcaptchaencoded", "Interstitial-CHL"),
)

_AWSWAF_MARKERS = ("gokuprops", "awswafcookiedomainlist")
_AWSWAF_ACTIVE_LOADER = (
    "token.awswaf.com",
    "awswafintegration",
    "checkforcerefresh",
)

_PHRASE = (
    ("just a moment", "ManagedChallenge-CHL"),
    ("checking your browser", "ManagedChallenge-CHL"),
    ("captcha-delivery.com", "Interstitial-CHL"),
    ("press &amp; hold", "HoldChallenge-PaH"),
    ("pardon our interruption", "SensorChallenge-CHL"),
)

_SMALL_BODY = (
    ("akam/13", "SensorChallenge-CHL"),
    ("_abck", "SensorChallenge-CHL"),
    ("_kpsdk", "ScriptChallenge-CHL"),
    ("ips.js", "ScriptChallenge-CHL"),
    ("_pxhd", "BehaviorChallenge-CHL"),
    ("px-captcha", "BehaviorChallenge-CHL"),
    ("captcha", "captcha-CHL"),
    ("403 forbidden", "BLOCKED"),
    ("access denied", "BLOCKED"),
)

_SENSOR_CHALLENGE_COSIGNAL = (
    "sensor_data",
    "bm-verify",
    "sec-if-cpt-container",
    "sec-cpt-if",
    "/_sec/cp_challenge",
    "pardon our interruption",
)

_INTERACTIVE_CAPTCHA_COSIGNAL = (
    "api2/bframe",
    "api2/anchor",
    "hcaptcha.com",
    "cf-turnstile",
    "challenges.cloudflare.com/turnstile",
    "i'm not a robot",
    "i’m not a robot",
    "verify you are human",
    "are you a robot",
    "select all images",
    "recaptcha challenge",
)


class Classification(TypedDict):
    tag: str
    len: int
    strict_pass: bool
    loose_pass: bool


def pass_flags(tag: str, length: int) -> tuple[bool, bool]:
    """Return (strict, loose) BrowserOxide pass flags for a classified row."""
    loose = tag == "L3-RENDERED"
    return loose and length >= STRICT_PASS_MIN_BYTES, loose


def _small_body_row_qualifies(needle: str, lower: str) -> bool:
    if needle == "akam/13":
        return any(cosignal in lower for cosignal in _SENSOR_CHALLENGE_COSIGNAL)
    if needle == "captcha":
        return any(cosignal in lower for cosignal in _INTERACTIVE_CAPTCHA_COSIGNAL)
    return True


def classify(body: str) -> Classification:
    """Port BrowserOxide's pinned ``engine_classify`` tag policy.

    Size gates use the original body's UTF-8 byte length, matching Rust
    ``str::len``. Marker matching uses a Unicode-lowercased copy.
    """
    lower = body.lower()
    length = len(body.encode("utf-8"))

    tag: str | None = None
    for needle, candidate in _UNAMBIGUOUS:
        if needle in lower:
            tag = candidate
            break

    if tag is None and (
        any(marker in lower for marker in _AWSWAF_MARKERS)
        and any(loader in lower for loader in _AWSWAF_ACTIVE_LOADER)
    ):
        tag = "AWS-WAF-CHL"

    if tag is None and length < INTERSTITIAL_MAX_BYTES:
        for needle, candidate in _PHRASE:
            if needle in lower:
                tag = candidate
                break

        if tag is None:
            for needle, candidate in _SMALL_BODY:
                if needle in lower and _small_body_row_qualifies(needle, lower):
                    tag = candidate
                    break

    if tag is None and length < BLOCKED_WORD_MAX_BYTES and "blocked" in lower:
        tag = "BLOCKED"

    if tag is None:
        tag = "THIN-BODY" if length < THIN_BODY_MAX_BYTES else "L3-RENDERED"

    strict_pass, loose_pass = pass_flags(tag, length)
    return {
        "tag": tag,
        "len": length,
        "strict_pass": strict_pass,
        "loose_pass": loose_pass,
    }
