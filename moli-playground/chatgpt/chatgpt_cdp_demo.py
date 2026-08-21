#!/usr/bin/env python3
from __future__ import annotations

import argparse
import asyncio
import getpass
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable

try:
    import websockets
    from websockets.asyncio.client import ClientConnection
except ImportError as error:  # pragma: no cover - this is a CLI setup error.
    raise SystemExit(
        "missing dependency: websockets\n"
        "run: uv run --with websockets python chatgpt_cdp_demo.py --help"
    ) from error


DEFAULT_URL = "https://chatgpt.com/"
DEFAULT_STARTUP_TIMEOUT = 10.0
DEFAULT_LOGIN_TIMEOUT = 90.0
DEFAULT_ANSWER_TIMEOUT = 300.0
DEFAULT_MOLI_HTTP_TIMEOUT_MS = 120_000


CHATGPT_HELPER_JS = r"""
(() => {
  function textOf(el) {
    if (!el) return '';
    return String(
      el.value ||
      el.textContent ||
      el.getAttribute?.('aria-label') ||
      el.getAttribute?.('title') ||
      el.innerText ||
      ''
    ).trim();
  }

  function lowerTextOf(el) {
    return textOf(el).toLowerCase();
  }

  function bodyText() {
    const body = document.body;
    if (!body) return '';
    const skipTags = new Set(['SCRIPT', 'STYLE', 'TEMPLATE', 'NOSCRIPT', 'SVG']);
    const walker = document.createTreeWalker(body, NodeFilter.SHOW_TEXT, {
      acceptNode(node) {
        let current = node.parentElement;
        while (current) {
          if (skipTags.has(current.tagName)) return NodeFilter.FILTER_REJECT;
          current = current.parentElement;
        }
        return NodeFilter.FILTER_ACCEPT;
      },
    });
    const parts = [];
    while (walker.nextNode()) {
      const value = String(walker.currentNode.nodeValue || '').trim();
      if (value) parts.push(value);
    }
    return parts.join(' ');
  }

  function hasCloudflareChallengeScript() {
    return all([
      'script[src*="/cdn-cgi/challenge-platform/"]',
      'script[src*="challenge-platform/scripts/jsd"]',
    ]).length > 0 || all(['script']).some((script) => {
      const text = String(script.textContent || '').toLowerCase();
      return text.includes('__cf$cv$params') || text.includes('challenge-platform/scripts/jsd');
    });
  }

  function isUsable(el) {
    if (!el) return false;
    if (el.disabled) return false;
    if (el.getAttribute?.('disabled') !== null) return false;
    if (el.type === 'hidden') return false;
    if (String(el.name || '').toLowerCase() === 'hiddenpassword') return false;
    if (el.getAttribute?.('aria-disabled') === 'true') return false;
    if (el.getAttribute?.('hidden') !== null) return false;
    const style = globalThis.getComputedStyle?.(el);
    if (style?.display === 'none' || style?.visibility === 'hidden') return false;
    return true;
  }

  function all(selectors) {
    const out = [];
    for (const selector of selectors) {
      try {
        out.push(...document.querySelectorAll(selector));
      } catch (_) {}
    }
    return out;
  }

  function first(selectors) {
    return all(selectors).find(isUsable) || null;
  }

  function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
  }

  async function waitFor(fn, timeoutMs) {
    const deadline = Date.now() + timeoutMs;
    let value = fn();
    while (!value && Date.now() < deadline) {
      await sleep(100);
      value = fn();
    }
    return value || null;
  }

  function fire(el, name, init) {
    let event;
    try {
      if (name === 'input' && typeof InputEvent !== 'undefined') {
        event = new InputEvent('input', Object.assign({ bubbles: true, inputType: 'insertText' }, init || {}));
      } else if (name.startsWith('mouse') || name === 'click') {
        event = new MouseEvent(name, Object.assign({ bubbles: true, cancelable: true }, init || {}));
      } else {
        event = new Event(name, Object.assign({ bubbles: true, cancelable: true }, init || {}));
      }
    } catch (_) {
      event = document.createEvent('Event');
      event.initEvent(name, true, true);
    }
    el.dispatchEvent(event);
  }

  function setControlValue(el, value) {
    el.focus?.();
    if (el.isContentEditable || el.getAttribute?.('contenteditable') === 'true') {
      try {
        const range = document.createRange();
        range.selectNodeContents(el);
        const selection = window.getSelection?.();
        selection?.removeAllRanges();
        selection?.addRange(range);
        if (document.queryCommandSupported?.('insertText')) {
          document.execCommand('insertText', false, value);
        } else {
          el.textContent = value;
        }
      } catch (_) {
        el.textContent = value;
      }
      if (!textOf(el).includes(value)) {
        el.textContent = value;
      }
    } else {
      const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
      const descriptor = Object.getOwnPropertyDescriptor(proto, 'value');
      if (descriptor && descriptor.set) {
        descriptor.set.call(el, value);
      } else {
        el.value = value;
      }
    }
    fire(el, 'beforeinput', { data: value });
    fire(el, 'input', { data: value });
    fire(el, 'change');
  }

  function reactPropsOf(el) {
    if (!el) return null;
    const key = Object.getOwnPropertyNames(el)
      .find((name) => name.startsWith('__reactProps'));
    return key ? el[key] : null;
  }

  function reactEventLike(target) {
    return {
      target,
      currentTarget: target,
      nativeEvent: { isComposing: false },
      defaultPrevented: false,
      preventDefault() { this.defaultPrevented = true; },
      stopPropagation() {},
      isDefaultPrevented() { return this.defaultPrevented; },
      isPropagationStopped() { return false; },
      persist() {},
    };
  }

  function submitReactForm(control) {
    const controlProps = reactPropsOf(control);
    if (controlProps?.onChange) {
      controlProps.onChange(reactEventLike(control));
    }
    const form = control?.form || control?.closest?.('form');
    if (form) {
      setTimeout(() => {
        try {
          const latestForm = control?.form || control?.closest?.('form') || form;
          const latestFormProps = reactPropsOf(latestForm);
          if (latestFormProps?.onSubmit) {
            latestFormProps.onSubmit(reactEventLike(latestForm));
          }
        } catch (error) {
          window.__lmChatGPTDemoLastSubmitError = String(error?.stack || error || '');
        }
      }, 1000);
      return true;
    }
    return false;
  }

  function elementInfo(el) {
    if (!el) return null;
    return {
      tag: el.tagName?.toLowerCase?.() || '',
      id: el.id || '',
      type: el.type || '',
      text: textOf(el).slice(0, 120),
      ariaLabel: String(el.getAttribute?.('aria-label') || ''),
      testid: String(el.getAttribute?.('data-testid') || ''),
      disabled: !!el.disabled || el.getAttribute?.('disabled') !== null || el.getAttribute?.('aria-disabled') === 'true',
      contenteditable: String(el.getAttribute?.('contenteditable') || ''),
    };
  }

  function clickElement(el) {
    if (!el) return false;
    const before = location.href;
    const href = el.href || el.getAttribute?.('href') || '';
    el.scrollIntoView?.({ block: 'center', inline: 'center' });
    el.focus?.();
    for (const name of ['mouseover', 'mousedown', 'mouseup']) {
      fire(el, name);
    }
    if (typeof el.click === 'function') {
      el.click();
    } else {
      fire(el, 'click');
    }
    if (href && href[0] !== '#' && location.href === before) {
      setTimeout(() => {
        if (location.href === before) location.href = href;
      }, 0);
    }
    return true;
  }

  function findClickable(labels) {
    const candidates = all([
      'button',
      '[role="button"]',
      'a',
      'input[type="submit"]',
      'input[type="button"]',
    ]);
    return candidates.find((el) => {
      if (!isUsable(el)) return false;
      const label = lowerTextOf(el);
      const aria = String(el.getAttribute?.('aria-label') || '').toLowerCase();
      const testid = String(el.getAttribute?.('data-testid') || '').toLowerCase();
      return labels.some((token) => label.includes(token) || aria.includes(token) || testid.includes(token));
    }) || null;
  }

  function findLoginButton() {
    return first([
      '[data-testid="login-button"]',
      '[data-testid*="login"]',
      'a[href*="/auth/login"]',
      'a[href*="auth.openai.com"]',
    ]) || findClickable(['log in', 'login', 'sign in', 'signin', '登录', '登入']);
  }

  function findEmailInput() {
    return first([
      'input[type="email"]',
      'input[name="email"]',
      'input[name="username"]',
      'input[id*="email" i]',
      'input[id*="username" i]',
      'input[autocomplete="email"]',
      'input[autocomplete="username"]',
      'input[inputmode="email"]',
    ]);
  }

  function findPasswordInput() {
    return first([
      'input[type="password"]',
      'input[name="password"]',
      'input[id*="password" i]',
      'input[autocomplete="current-password"]',
    ]);
  }

  function findSubmitButton() {
    const exactLabels = new Set([
      'continue',
      'next',
      'log in',
      'login',
      'sign in',
      'submit',
      '继续',
      '下一步',
      '登录',
      '登入',
    ]);
    const buttons = all([
      'button',
      '[role="button"]',
      'input[type="submit"]',
      'input[type="button"]',
      '[data-testid*="submit"]',
      '[data-testid*="login"]',
      '[data-testid*="continue"]',
    ]);
    const exact = buttons.find((el) => isUsable(el) && exactLabels.has(lowerTextOf(el)));
    if (exact) return exact;
    const generic = buttons.find((el) => {
      if (!isUsable(el)) return false;
      const label = lowerTextOf(el);
      if (label.includes('google') || label.includes('apple') || label.includes('phone')) return false;
      return ['continue', 'next', 'log in', 'login', 'sign in', 'submit'].some((token) => label.includes(token));
    });
    if (generic) return generic;
    return first(['button[type="submit"]', 'input[type="submit"]']);
  }

  function findComposer() {
    return first([
      '#prompt-textarea',
      '[data-testid="prompt-textarea"]',
      'textarea[data-testid*="prompt" i]',
      'textarea[placeholder*="Message" i]',
      'textarea',
      'div.ProseMirror[contenteditable="true"]',
      'form [contenteditable="true"]',
      '[contenteditable="plaintext-only"]',
      '[contenteditable="true"][id*="prompt" i]',
      '[contenteditable="true"][data-testid*="prompt" i]',
      '[contenteditable="true"]',
    ]);
  }

  function sendButtonCandidates() {
    return all([
      'button[data-testid="send-button"]',
      'button[data-testid="composer-submit-button"]',
      'button[data-testid*="send" i]',
      'button[data-testid*="submit" i]',
      'button[aria-label*="Send" i]',
      'button[aria-label*="send" i]',
      'button[aria-label*="Submit" i]',
      'button[aria-label*="submit" i]',
      'button[aria-label*="发送" i]',
    ]);
  }

  function labelBlobOf(el) {
    if (!el) return '';
    return [
      lowerTextOf(el),
      String(el.getAttribute?.('aria-label') || '').toLowerCase(),
      String(el.getAttribute?.('data-testid') || '').toLowerCase(),
      String(el.getAttribute?.('title') || '').toLowerCase(),
    ].join(' ');
  }

  function looksLikeStopButton(el) {
    const label = labelBlobOf(el);
    return ['stop', '停止', 'cancel response', '停止生成'].some((token) => label.includes(token));
  }

  function stopButtonCandidates() {
    return all([
      'button[data-testid*="stop" i]',
      'button[aria-label*="Stop" i]',
      'button[aria-label*="stop" i]',
      'button[aria-label*="Cancel" i]',
      'button[aria-label*="cancel" i]',
      'button[aria-label*="停止" i]',
      'button[title*="Stop" i]',
      'button[title*="stop" i]',
      'button[title*="停止" i]',
      'button[data-testid="composer-submit-button"]',
    ]);
  }

  function findStopButton() {
    return stopButtonCandidates().find((el) => isUsable(el) && looksLikeStopButton(el))
      || findClickable(['stop', '停止', 'cancel response', '停止生成']);
  }

  function findSendButton() {
    return sendButtonCandidates().find((el) => isUsable(el) && !looksLikeStopButton(el))
      || findClickable(['send', 'submit', '发送']);
  }

  function findAnySendButton() {
    return sendButtonCandidates()[0] || findClickable(['send', 'submit', '发送']);
  }

  function assistantTexts() {
    const candidates = all([
      '[data-message-author-role="assistant"]',
      '[data-testid*="assistant" i]',
      '[class*="assistant" i] .markdown',
      '.markdown',
    ]).map(textOf).filter(Boolean);
    return [...new Set(candidates)];
  }

  function userTexts() {
    const candidates = all([
      '[data-message-author-role="user"]',
      '[data-testid*="user" i]',
    ]).map(textOf).filter(Boolean);
    return [...new Set(candidates)];
  }

  function conversationState() {
    const assistants = assistantTexts();
    const users = userTexts();
    const composer = findComposer();
    const usableSend = findSendButton();
    const anySend = findAnySendButton();
    const stopButton = findStopButton();
    return {
      url: location.href,
      title: document.title || '',
      assistantCount: assistants.length,
      latestAssistantText: assistants.length ? assistants[assistants.length - 1] : '',
      userCount: users.length,
      latestUserText: users.length ? users[users.length - 1] : '',
      composer: elementInfo(composer),
      composerText: textOf(composer),
      sendButton: elementInfo(usableSend || anySend),
      sendUsable: !!usableSend,
      stopButton: elementInfo(stopButton),
      isGenerating: !!stopButton,
      blockingReason: blockingReason(),
      cloudflareScriptPresent: hasCloudflareChallengeScript(),
      bodyTail: bodyText().slice(-1200),
    };
  }

  function acceptCookieConsent() {
    const button = findClickable(['accept all', 'reject non-essential']);
    if (!button) return { ok: false, reason: 'cookie-consent-not-found', snapshot: snapshot() };
    return { ok: clickElement(button), text: textOf(button), snapshot: snapshot() };
  }

  function blockingReason() {
    const href = String(location.href || '').toLowerCase();
    if (href.includes('auth.openai.com/email-verification')) return 'email-verification';
    const text = bodyText().toLowerCase();
    const checks = [
      ['captcha', 'captcha'],
      ['are you human', 'human-check'],
      ['verify you are human', 'human-check'],
      ['checking if the site connection is secure', 'cloudflare-challenge'],
      ['enable javascript and cookies to continue', 'cloudflare-challenge'],
      ['verification code', 'verification-code'],
      ['enter code', 'verification-code'],
      ['check your email', 'email-verification'],
      ['approve on your', 'device-approval'],
      ["we've sent a notification to your device", 'device-approval'],
      ['open the chatgpt app', 'device-approval'],
      ['resend prompt', 'device-approval'],
      ['two-factor', 'mfa'],
      ['authenticator', 'mfa'],
      ['passkey', 'passkey'],
      ['验证码', 'verification-code'],
      ['验证', 'verification'],
      ['两步', 'mfa'],
    ];
    const found = checks.find(([needle]) => text.includes(needle));
    return found ? found[1] : '';
  }

  function scrubDiagnosticText(value) {
    return String(value || '')
      .replace(/[\w.+-]+@[\w.-]+\.\w+/g, '<email>')
      .slice(0, 1000);
  }

  function reactRouterDiagnostics() {
    const state = window.__reactRouterContext?.state;
    if (!state) return null;
    const errors = {};
    for (const [key, error] of Object.entries(state.errors || {})) {
      errors[key] = {
        status: error?.status || null,
        statusText: scrubDiagnosticText(error?.statusText),
        message: scrubDiagnosticText(error?.message || error),
      };
    }
    return {
      location: scrubDiagnosticText(state.location?.pathname || location.pathname),
      navigationState: scrubDiagnosticText(state.navigation?.state),
      loaderDataKeys: Object.keys(state.loaderData || {}),
      errors,
    };
  }

  function cookieNames() {
    return String(document.cookie || '')
      .split(';')
      .map((entry) => entry.trim().split('=')[0])
      .filter(Boolean)
      .sort();
  }

  function hasAccountCookie(names) {
    return names.some((name) =>
      name === '_account' ||
      name === 'oai-auth-token' ||
      name === 'oai-client-auth-session' ||
      name === 'auth-session-minimized-client-checksum'
    );
  }

  function formDataShape(form, submitter) {
    try {
      return Array.from(new FormData(form, submitter || undefined).entries()).map(([name, value]) => {
        if (typeof value === 'string') return [name, `string:${value.length}`];
        return [name, value?.name ? `file:${value.name}` : 'file'];
      });
    } catch (error) {
      return { error: scrubDiagnosticText(error?.message || error) };
    }
  }

  function installPasswordSubmitDiagnostics(input) {
    const form = input?.form || input?.closest?.('form');
    if (!form || form.__lmChatGPTDemoSubmitDiagnosticsInstalled) return;
    form.__lmChatGPTDemoSubmitDiagnosticsInstalled = true;
    form.addEventListener('submit', (event) => {
      const submitter = event.submitter || event.nativeEvent?.submitter || null;
      const diagnostic = {
        isTrusted: !!event.isTrusted,
        cancelable: !!event.cancelable,
        defaultPreventedAtCapture: !!event.defaultPrevented,
        submitter: elementInfo(submitter),
        formDataShape: formDataShape(form, submitter),
      };
      window.__lmChatGPTDemoLastPasswordSubmit = diagnostic;
      setTimeout(() => {
        diagnostic.defaultPreventedAfterDispatch = !!event.defaultPrevented;
      }, 0);
    }, { capture: true });
  }

  function snapshot() {
    const emailInput = findEmailInput();
    const passwordInput = findPasswordInput();
    const form = emailInput?.form || passwordInput?.form || document.querySelector('form');
    const hasReactContainer = Object.getOwnPropertyNames(document)
      .some((name) => name.startsWith('__reactContainer'));
    const hasReactForm = !!form && Object.getOwnPropertyNames(form)
      .some((name) => name.startsWith('__reactFiber') || name.startsWith('__reactProps'));
    const reactRouterHydrated = !!window.__reactRouterContext?.state;
    const names = cookieNames();
    const inputs = all(['input', 'textarea']).slice(0, 20).map((el) => ({
      tag: el.tagName.toLowerCase(),
      type: el.type || '',
      name: el.name || '',
      id: el.id || '',
      autocomplete: el.autocomplete || '',
      placeholder: el.placeholder || '',
    }));
    const buttons = all(['button', '[role="button"]', 'a']).slice(0, 30).map((el) => textOf(el).slice(0, 80));
    return {
      url: location.href,
      title: document.title || '',
      readyState: document.readyState,
      hasEmailInput: !!emailInput,
      hasPasswordInput: !!passwordInput,
      hasComposer: !!findComposer(),
      hasLoginButton: !!findLoginButton(),
      hasAccountCookie: hasAccountCookie(names),
      hasReactContainer,
      hasReactForm,
      reactRouterHydrated,
      loginFormHydrated: reactRouterHydrated || hasReactForm,
      blockingReason: blockingReason(),
      reactRouter: reactRouterDiagnostics(),
      lastPasswordSubmit: window.__lmChatGPTDemoLastPasswordSubmit || null,
      cookieNames: names,
      text: bodyText().slice(0, 1200),
      inputs,
      buttons: buttons.filter(Boolean),
    };
  }

  function loginState() {
    const snap = snapshot();
    const text = snap.text.toLowerCase();
    const composerShell =
      snap.hasComposer ||
      text.includes('message chatgpt') ||
      text.includes('ask anything') ||
      text.includes('new chat');
    const authenticatedComposer =
      composerShell &&
      snap.hasAccountCookie &&
      !snap.hasEmailInput &&
      !snap.hasPasswordInput;
    const loggedOutLanding =
      (!authenticatedComposer && snap.hasLoginButton) ||
      text.includes('sign up for free') ||
      text.includes('log in to get answers') ||
      text.includes('log inlog in');
    const loggedIn = authenticatedComposer || (!loggedOutLanding && composerShell);
    return Object.assign({ loggedIn }, snap);
  }

  window.__lmChatGPTDemo = {
    snapshot,
    loginState,
    clickLogin() {
      const button = findLoginButton();
      const before = location.href;
      const ok = clickElement(button);
      if (ok) {
        setTimeout(() => {
          if (location.href !== before) return;
          if (findEmailInput() || findPasswordInput()) return;
          try {
            location.href = new URL('/auth/login', location.href).href;
          } catch (_) {
            location.href = '/auth/login';
          }
        }, 250);
      }
      return { ok, reason: button ? '' : 'login-button-not-found', snapshot: snapshot() };
    },
    fillEmail(email) {
      const input = findEmailInput();
      if (!input) return { ok: false, reason: 'email-input-not-found', snapshot: snapshot() };
      setControlValue(input, email);
      if (submitReactForm(input)) {
        return { ok: true, reactSubmit: true, clickedSubmit: false };
      }
      const submit = findSubmitButton();
      if (submit) clickElement(submit);
      return { ok: true, clickedSubmit: !!submit, snapshot: snapshot() };
    },
    fillPassword(password) {
      const input = findPasswordInput();
      if (!input) return { ok: false, reason: 'password-input-not-found', snapshot: snapshot() };
      installPasswordSubmitDiagnostics(input);
      setControlValue(input, password);
      const reactSubmit = submitReactForm(input);
      if (reactSubmit) {
        return { ok: true, reactSubmit: true, clickedSubmit: false };
      }
      const submit = findSubmitButton();
      if (submit) {
        clickElement(submit);
      } else {
        const form = input.closest?.('form');
        try {
          if (form?.requestSubmit) form.requestSubmit();
          else if (form?.submit) form.submit();
        } catch (_) {}
      }
      return { ok: true, reactSubmit, clickedSubmit: !!submit, snapshot: snapshot() };
    },
    acceptCookieConsent,
    fillPrompt(promptText) {
      const composer = findComposer();
      if (!composer) return { ok: false, reason: 'composer-not-found', snapshot: snapshot() };
      const before = conversationState();
      setControlValue(composer, promptText);
      return {
        ok: true,
        before,
        after: conversationState(),
        snapshot: snapshot(),
      };
    },
    clickSendPrompt() {
      const send = findSendButton();
      const anySend = findAnySendButton();
      if (!send) {
        return {
          ok: false,
          reason: 'send-button-not-usable',
          sendButton: elementInfo(anySend),
          snapshot: snapshot(),
        };
      }
      const clickedSend = clickElement(send);
      return {
        ok: clickedSend,
        clickedSend,
        sendButton: elementInfo(send),
        reason: clickedSend ? '' : 'send-click-failed',
        snapshot: snapshot(),
      };
    },
    async submitPrompt(promptText) {
      const composer = findComposer();
      if (!composer) return { ok: false, reason: 'composer-not-found', snapshot: snapshot() };
      const before = conversationState();
      setControlValue(composer, promptText);
      await sleep(0);
      await sleep(100);
      const send = await waitFor(findSendButton, 5000);
      let clickedSend = false;
      let submittedForm = false;
      if (send) {
        clickedSend = clickElement(send);
      }
      await sleep(250);
      const after = conversationState();
      return {
        ok: clickedSend || submittedForm,
        clickedSend,
        submittedForm,
        before,
        after,
        reason: clickedSend || submittedForm ? '' : 'send-button-not-usable',
        snapshot: snapshot(),
      };
    },
    latestAssistantText() {
      const candidates = assistantTexts();
      if (candidates.length) return candidates[candidates.length - 1];
      return bodyText().slice(-4000);
    },
    conversationState,
  };
  return true;
})();
"""


class DemoError(RuntimeError):
    pass


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def resolve_moli_binary(raw: str | None) -> Path:
    if raw:
        path = Path(raw).expanduser().resolve()
        if not path.exists():
            raise DemoError(f"moli binary does not exist: {path}")
        return path
    env_path = os.environ.get("MOLI_BIN")
    if env_path:
        return resolve_moli_binary(env_path)
    root = repo_root()
    candidates = [
        root / "target" / "release" / "moli",
        root / "target" / "debug" / "moli",
    ]
    existing = [path for path in candidates if path.exists()]
    if existing:
        return max(existing, key=lambda path: path.stat().st_mtime)
    path = shutil.which("moli")
    if path:
        return Path(path).resolve()
    raise DemoError("missing moli binary; run `cargo build -p moli` or set MOLI_BIN")


def reserve_local_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def read_json_url_no_proxy(url: str, timeout: float = 2.0) -> dict[str, Any]:
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(url, timeout=timeout) as response:
        payload = json.loads(response.read().decode("utf-8"))
    if not isinstance(payload, dict):
        raise DemoError(f"unexpected JSON response from {url}: {payload!r}")
    return payload


@dataclass
class MoliServe:
    process: subprocess.Popen[bytes]
    endpoint: str
    command: list[str]
    logs: list[str]
    threads: list[threading.Thread]


@dataclass(frozen=True)
class AnswerResult:
    text: str
    source: str

    def __str__(self) -> str:
        return self.text


def append_log(logs: list[str], line: str) -> None:
    logs.append(line)
    if len(logs) > 200:
        del logs[: len(logs) - 200]


def drain_stream(stream: Any, label: str, logs: list[str]) -> None:
    if stream is None:
        return
    try:
        while True:
            line = stream.readline()
            if not line:
                return
            append_log(logs, f"{label}: {line.decode('utf-8', errors='replace').rstrip()}")
    except OSError:
        return


def start_moli(args: argparse.Namespace) -> MoliServe:
    binary = resolve_moli_binary(args.moli_bin)
    port = reserve_local_port()
    command = [str(binary), "serve", "--host", "127.0.0.1", "--port", str(port)]
    if args.profile_dir:
        command.extend(["--profile-dir", str(Path(args.profile_dir).expanduser().resolve())])
    if args.user_agent:
        command.extend(["--user-agent", args.user_agent])
    if args.http_proxy:
        command.extend(["--http-proxy", args.http_proxy])
    if args.http_no_proxy:
        command.extend(["--http-no-proxy", args.http_no_proxy])
    http_timeout = getattr(args, "http_timeout", None)
    if http_timeout is not None:
        command.extend(["--http-timeout", str(http_timeout)])
    if getattr(args, "http_max_concurrent", None) is not None:
        command.extend(["--http-max-concurrent", str(args.http_max_concurrent)])
    if getattr(args, "http_max_host_open", None) is not None:
        command.extend(["--http-max-host-open", str(args.http_max_host_open)])

    process = subprocess.Popen(
        command,
        cwd=repo_root(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        start_new_session=True,
    )
    logs: list[str] = []
    threads = [
        threading.Thread(target=drain_stream, args=(process.stdout, "stdout", logs), daemon=True),
        threading.Thread(target=drain_stream, args=(process.stderr, "stderr", logs), daemon=True),
    ]
    for thread in threads:
        thread.start()

    endpoint = f"http://127.0.0.1:{port}"
    deadline = time.monotonic() + args.startup_timeout
    last_error: BaseException | None = None
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise DemoError(f"moli serve exited early rc={process.returncode}: {'; '.join(logs[-20:])}")
        try:
            read_json_url_no_proxy(endpoint + "/json/version")
            if not getattr(args, "quiet", False):
                print(f"moli CDP: {endpoint}")
            return MoliServe(process=process, endpoint=endpoint, command=command, logs=logs, threads=threads)
        except BaseException as error:  # noqa: BLE001 - report the final startup error.
            last_error = error
            time.sleep(0.05)
    stop_moli(MoliServe(process=process, endpoint=endpoint, command=command, logs=logs, threads=threads))
    raise DemoError(f"timed out waiting for {endpoint}/json/version; last_error={last_error!r}")


def stop_moli(serve: MoliServe | None) -> None:
    if serve is None:
        return
    process = serve.process
    if process.poll() is None:
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except OSError:
            pass
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except OSError:
                pass
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                pass
    for thread in serve.threads:
        thread.join(timeout=0.2)


@dataclass
class RawCdpClient:
    websocket: ClientConnection
    next_id: int = 1

    async def send(self, method: str, params: dict[str, Any] | None = None, *, session_id: str | None = None) -> int:
        message_id = self.next_id
        self.next_id += 1
        message: dict[str, Any] = {"id": message_id, "method": method}
        if params is not None:
            message["params"] = params
        if session_id is not None:
            message["sessionId"] = session_id
        await self.websocket.send(json.dumps(message, separators=(",", ":")))
        return message_id

    async def recv(self) -> dict[str, Any]:
        raw = await self.websocket.recv()
        if isinstance(raw, bytes):
            raw = raw.decode("utf-8")
        payload = json.loads(raw)
        if not isinstance(payload, dict):
            raise DemoError(f"unexpected CDP payload: {payload!r}")
        return payload

    async def recv_until_id(self, message_id: int, *, timeout: float) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        seen: list[dict[str, Any]] = []
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise DemoError(f"timed out waiting for CDP response id={message_id}; seen={seen[-10:]}")
            try:
                message = await asyncio.wait_for(self.recv(), timeout=remaining)
            except TimeoutError as error:
                raise DemoError(f"timed out waiting for CDP response id={message_id}; seen={seen[-10:]}") from error
            seen.append(message)
            if message.get("id") != message_id:
                continue
            if "error" in message:
                raise DemoError(f"CDP command {message_id} failed: {message['error']}")
            return message, seen

    async def command(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        session_id: str | None = None,
        timeout: float = 10.0,
    ) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        message_id = await self.send(method, params, session_id=session_id)
        return await self.recv_until_id(message_id, timeout=timeout)


async def connect_cdp(endpoint: str) -> RawCdpClient:
    payload = await asyncio.to_thread(read_json_url_no_proxy, endpoint.rstrip("/") + "/json/version")
    websocket_url = payload.get("webSocketDebuggerUrl")
    if not isinstance(websocket_url, str) or not websocket_url:
        raise DemoError(f"missing webSocketDebuggerUrl in discovery payload: {payload!r}")
    try:
        websocket = await websockets.connect(websocket_url, open_timeout=5, max_size=None, proxy=None)
    except TypeError:
        websocket = await websockets.connect(websocket_url, open_timeout=5, max_size=None)
    return RawCdpClient(websocket=websocket)


@dataclass
class CdpPage:
    client: RawCdpClient
    target_id: str
    session_id: str

    @classmethod
    async def create(cls, client: RawCdpClient) -> "CdpPage":
        response, _ = await client.command("Target.createTarget", {"url": "about:blank"}, timeout=10)
        target_id = str(response["result"]["targetId"])
        attach, _ = await client.command("Target.attachToTarget", {"targetId": target_id, "flatten": True}, timeout=10)
        session_id = str(attach["result"]["sessionId"])
        page = cls(client=client, target_id=target_id, session_id=session_id)
        for method in ("Page.enable", "Runtime.enable", "Network.enable"):
            await page.command(method, timeout=10)
        try:
            await page.command("Page.setLifecycleEventsEnabled", {"enabled": True}, timeout=10)
        except DemoError:
            pass
        return page

    async def command(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        timeout: float = 10.0,
    ) -> tuple[dict[str, Any], list[dict[str, Any]]]:
        return await self.client.command(method, params, session_id=self.session_id, timeout=timeout)

    async def evaluate(self, expression: str, *, timeout: float = 10.0, await_promise: bool = False) -> Any:
        response, _ = await self.command(
            "Runtime.evaluate",
            {
                "expression": expression,
                "returnByValue": True,
                "awaitPromise": await_promise,
            },
            timeout=timeout,
        )
        result = response.get("result", {})
        if "exceptionDetails" in result:
            raise DemoError(f"Runtime.evaluate exception: {result['exceptionDetails']}")
        remote = result.get("result", {})
        if isinstance(remote, dict) and "value" in remote:
            return remote["value"]
        return remote

    async def install_helpers(self) -> None:
        installed = await self.evaluate(
            "!!window.__lmChatGPTDemo",
            timeout=5,
            await_promise=False,
        )
        if installed is True:
            return
        await self.evaluate(CHATGPT_HELPER_JS, timeout=10)

    async def helper(self, name: str, *args: Any, timeout: float = 10.0) -> Any:
        await self.install_helpers()
        arglist = ",".join(json.dumps(arg) for arg in args)
        return await self.evaluate(f"window.__lmChatGPTDemo.{name}({arglist})", timeout=timeout, await_promise=True)

    async def navigate(self, url: str, *, timeout: float) -> None:
        response, seen = await self.command("Page.navigate", {"url": url}, timeout=timeout)
        frame_id = response.get("result", {}).get("frameId")
        await self.wait_domcontentloaded(
            frame_id=str(frame_id) if frame_id is not None else None,
            seen=seen,
            timeout=min(30.0, timeout),
        )
        await self.wait_ready(timeout=min(15.0, timeout))

    def is_domcontentloaded_event(self, message: dict[str, Any], frame_id: str | None) -> bool:
        if message.get("sessionId") != self.session_id:
            return False
        method = message.get("method")
        if method == "Page.domContentEventFired":
            return True
        if method != "Page.lifecycleEvent":
            return False
        params = message.get("params")
        if not isinstance(params, dict):
            return False
        if frame_id is not None and params.get("frameId") != frame_id:
            return False
        return params.get("name") in {"DOMContentLoaded", "domContentLoaded"}

    async def wait_domcontentloaded(
        self,
        *,
        frame_id: str | None,
        seen: list[dict[str, Any]],
        timeout: float,
    ) -> None:
        if any(self.is_domcontentloaded_event(message, frame_id) for message in seen):
            return
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                message = await asyncio.wait_for(self.client.recv(), timeout=max(0.1, deadline - time.monotonic()))
            except TimeoutError:
                break
            if self.is_domcontentloaded_event(message, frame_id):
                return
        raise DemoError("timed out waiting for DOMContentLoaded")

    async def wait_ready(self, *, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        last_error: BaseException | None = None
        while time.monotonic() < deadline:
            try:
                state = await self.evaluate("document.readyState", timeout=2)
                if state in ("interactive", "complete"):
                    return
            except BaseException as error:  # noqa: BLE001 - retry while navigation swaps documents.
                last_error = error
            await asyncio.sleep(0.2)
        raise DemoError(f"timed out waiting for document readiness; last_error={last_error!r}")

    async def close(self) -> None:
        try:
            await self.client.command("Target.closeTarget", {"targetId": self.target_id}, timeout=2)
        except Exception:
            pass


async def wait_for_state(page: CdpPage, predicate: Any, *, timeout: float, label: str) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_state: dict[str, Any] | None = None
    last_error: BaseException | None = None
    while time.monotonic() < deadline:
        try:
            state = await page.helper("loginState", timeout=5)
            if isinstance(state, dict):
                last_state = state
                if predicate(state):
                    return state
                if state.get("blockingReason"):
                    return state
        except BaseException as error:  # noqa: BLE001 - report if the condition never appears.
            last_error = error
        await asyncio.sleep(0.5)
    raise DemoError(f"timed out waiting for {label}; last_state={last_state!r}; last_error={last_error!r}")


async def login(
    page: CdpPage,
    *,
    url: str,
    email: str,
    password: str,
    timeout: float,
    debug_snapshot: bool,
    reporter: Callable[[str], None] | None = print,
) -> dict[str, Any]:
    def report(message: str) -> None:
        if reporter is not None:
            reporter(message)

    report(f"navigate: {url}")
    await page.navigate(url, timeout=timeout)
    state = await page.helper("loginState", timeout=10)
    if isinstance(state, dict) and state.get("loggedIn"):
        report("already logged in")
        return state

    report("open login form")
    click_result = await page.helper("clickLogin", timeout=10)
    if debug_snapshot:
        report("clickLogin: " + json.dumps(redact_snapshot(click_result), indent=2))

    state = await wait_for_state(
        page,
        lambda value: bool(value.get("hasEmailInput") or value.get("hasPasswordInput") or value.get("loggedIn")),
        timeout=min(30.0, timeout),
        label="login form",
    )
    check_human_gate(state)
    if state.get("loggedIn"):
        return state

    if state.get("hasEmailInput"):
        report("fill email")
        result = await page.helper("fillEmail", email, timeout=10)
        if debug_snapshot:
            report("fillEmail: " + json.dumps(redact_snapshot(result), indent=2))
        await asyncio.sleep(1.5)
        state = await wait_for_state(
            page,
            lambda value: bool(value.get("hasPasswordInput") or value.get("loggedIn")),
            timeout=min(45.0, timeout),
            label="password form",
        )
        check_human_gate(state)

    if state.get("loggedIn"):
        return state

    if not state.get("hasPasswordInput"):
        raise DemoError(f"password input did not appear; state={redact_snapshot(state)!r}")

    report("fill password")
    result = await page.helper("fillPassword", password, timeout=10)
    if debug_snapshot:
        report("fillPassword: " + json.dumps(redact_snapshot(result), indent=2))
    await asyncio.sleep(1.5)

    state = await wait_for_state(
        page,
        lambda value: bool(value.get("loggedIn")),
        timeout=timeout,
        label="logged-in ChatGPT composer",
    )
    check_human_gate(state)
    if not state.get("loggedIn"):
        raise DemoError(f"login did not reach composer; state={redact_snapshot(state)!r}")
    report("login ok")
    return state


def check_human_gate(state: dict[str, Any]) -> None:
    reason = state.get("blockingReason")
    if reason:
        raise DemoError(f"login reached a human-verification step ({reason}); this demo does not bypass it")


def is_transient_assistant_text(text: str) -> bool:
    normalized = re.sub(r"\s+", " ", text).strip().lower()
    return (
        normalized in {"", "thinking", "thinking...", "generating", "searching"}
        or normalized.endswith(" thinking")
        or normalized.endswith(" thinking...")
    )


def is_retryable_read_only_helper_error(error: BaseException) -> bool:
    text = str(error) or repr(error)
    return "timed out" in text and (
        "conversationState" in text
        or "latestAssistantText" in text
        or "CDP response" in text
        or "Runtime.evaluate" in text
    )


def redact_sensitive_text(text: str) -> str:
    text = re.sub(r"[\w.+-]+@[\w.-]+\.\w+", "<email>", text)
    text = re.sub(
        r"(push-auth-verification/)[^/?#\s]+",
        r"\1<redacted>",
        text,
        flags=re.IGNORECASE,
    )
    text = re.sub(
        r"(/ws/user/)[^/?#\s]+",
        r"\1<redacted>",
        text,
        flags=re.IGNORECASE,
    )
    text = re.sub(
        r"\buser-[A-Za-z0-9_%+-]{12,}",
        "user-<redacted>",
        text,
    )
    text = re.sub(
        r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}",
        lambda match: f"{match.group(0)[:8]}...{match.group(0)[-4:]}",
        text,
        flags=re.IGNORECASE,
    )
    return re.sub(r"([?&][A-Za-z0-9_.-]{1,64}=)[^&\s'\",\\\]\)]+", r"\1<redacted>", text)


def redact_snapshot(value: Any) -> Any:
    if isinstance(value, dict):
        out = {}
        for key, item in value.items():
            if key.lower() in {"password", "value"}:
                out[key] = "<redacted>"
            else:
                out[key] = redact_snapshot(item)
        return out
    if isinstance(value, list):
        return [redact_snapshot(item) for item in value]
    if isinstance(value, str):
        return redact_sensitive_text(value)
    return value


async def ask_once(
    page: CdpPage,
    prompt: str,
    *,
    answer_timeout: float,
    reporter: Callable[[str], None] | None = print,
    answer_update: Callable[[str], None] | None = None,
) -> AnswerResult:
    try:
        before = await page.helper("conversationState", timeout=10)
    except DemoError as error:
        if not is_retryable_read_only_helper_error(error):
            raise
        before = {}
    if not isinstance(before, dict):
        before = {}
    if reporter is not None:
        reporter("send prompt")
    result = await page.helper("submitPrompt", prompt, timeout=min(30.0, max(10.0, answer_timeout)))
    if not isinstance(result, dict) or not result.get("ok"):
        raise DemoError(f"failed to submit prompt: {redact_snapshot(result)!r}")
    if reporter is not None:
        reporter("waiting for response")

    deadline = time.monotonic() + answer_timeout
    last = ""
    stable_since: float | None = None
    before_count = int(before.get("assistantCount") or 0)
    before_text = str(before.get("latestAssistantText") or "")
    while time.monotonic() < deadline:
        try:
            state = await page.helper("conversationState", timeout=10)
        except DemoError as error:
            if not is_retryable_read_only_helper_error(error):
                raise
            await asyncio.sleep(1.0)
            continue
        if not isinstance(state, dict):
            state = {}
        reason = state.get("blockingReason")
        if reason:
            raise DemoError(f"ChatGPT reached a blocking step while waiting for answer ({reason})")
        current = str(state.get("latestAssistantText") or "")
        count = int(state.get("assistantCount") or 0)
        is_generating = bool(state.get("isGenerating"))
        if current and not is_transient_assistant_text(current) and (count > before_count or current != before_text):
            if current != last:
                last = current
                stable_since = time.monotonic()
                if answer_update is not None:
                    answer_update(current)
            elif stable_since is not None and time.monotonic() - stable_since >= 2.0 and not is_generating:
                return AnswerResult(text=current, source="live-dom")
        await asyncio.sleep(1.0)
    try:
        final_state = await page.helper("conversationState", timeout=10)
    except DemoError as error:
        final_state = {"error": str(error)}
    raise DemoError(
        "timed out waiting for ChatGPT response; "
        f"last={last[-1000:]!r}; state={redact_snapshot(final_state)!r}"
    )


def read_credentials(args: argparse.Namespace) -> tuple[str, str]:
    email = args.email or os.environ.get("CHATGPT_EMAIL")
    if not email:
        email = input("ChatGPT email: ").strip()
    if not email:
        raise DemoError("email is required")

    if args.password_stdin:
        password = sys.stdin.readline().rstrip("\n")
    else:
        password = os.environ.get("CHATGPT_PASSWORD")
        if password is None:
            password = getpass.getpass("ChatGPT password: ")
    if not password:
        raise DemoError("password is required")
    return email, password


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Drive chatgpt.com through Moli CDP.")
    parser.add_argument("--url", default=DEFAULT_URL, help=f"initial URL, default: {DEFAULT_URL}")
    parser.add_argument("--email", help="ChatGPT account email; alternatively set CHATGPT_EMAIL")
    parser.add_argument("--password-stdin", action="store_true", help="read the password from stdin instead of getpass")
    parser.add_argument("--prompt", help="send one prompt after login and print the response")
    parser.add_argument("--login-only", action="store_true", help="stop after login succeeds")
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
    parser.add_argument("--debug-snapshot", action="store_true", help="print sanitized DOM snapshots around login steps")
    parser.add_argument("--keep-open", action="store_true", help="leave moli serve running when the script exits")
    return parser


async def async_main(args: argparse.Namespace) -> int:
    email, password = read_credentials(args)
    serve: MoliServe | None = None
    client: RawCdpClient | None = None
    page: CdpPage | None = None
    try:
        serve = start_moli(args)
        client = await connect_cdp(serve.endpoint)
        page = await CdpPage.create(client)
        await login(
            page,
            url=args.url,
            email=email,
            password=password,
            timeout=args.login_timeout,
            debug_snapshot=args.debug_snapshot,
        )

        if args.login_only:
            return 0

        if args.prompt:
            result = await ask_once(page, args.prompt, answer_timeout=args.answer_timeout)
            print(f"answer source: {result.source}")
            print("\n--- ChatGPT ---")
            print(result.text)
            return 0

        print("interactive prompt mode; Ctrl-D to exit")
        while True:
            try:
                prompt = input("\nchatgpt> ")
            except EOFError:
                print()
                return 0
            if not prompt.strip():
                continue
            result = await ask_once(page, prompt, answer_timeout=args.answer_timeout)
            print(f"answer source: {result.source}")
            print("\n--- ChatGPT ---")
            print(result.text)
    finally:
        if page is not None and not args.keep_open:
            await page.close()
        if client is not None and not args.keep_open:
            await client.websocket.close()
        if serve is not None:
            if args.keep_open:
                print(f"leaving moli running: pid={serve.process.pid} endpoint={serve.endpoint}")
            else:
                stop_moli(serve)


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    try:
        return asyncio.run(async_main(args))
    except KeyboardInterrupt:
        return 130
    except DemoError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
