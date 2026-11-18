#!/usr/bin/env python3
"""The Runbook -> chat notifier (example; you own and run it).

Turns The Runbook's human events (`approval.requested`,
`approval.escalated`, `approval.expired`, `step.blocked`) into one message
POSTed to a chat incoming webhook (`{"text": ...}`, the shape Slack,
Mattermost, Rocket.Chat and Google Chat accept). Standard library only.

Each request must carry a valid `Runbook-Signature` over
"{Runbook-Timestamp}.{raw body}" with a timestamp within five minutes, and
events are deduplicated by ID so a redelivery never posts twice (events
without an ID are refused). Handled IDs live in memory only: after a
restart a redelivered event can post again; keep them in a file or store
if that matters. Event text (titles, reasons, unmapped recipient IDs) is
escaped for the chat's markup, so it cannot inject mentions or links:
CHAT_MARKUP=slack (default; Slack and Google Chat) escapes & < >, and
CHAT_MARKUP=markdown (Mattermost, Rocket.Chat) backslash-escapes Markdown
and breaks @-mentions. Handles configured in MENTIONS are inserted as
given, so they do mention. Other
event types are acknowledged with 204. If the chat webhook fails, the
notifier answers 502 so The Runbook retries with backoff.

Recipients are The Runbook's user and team IDs; map them to chat handles
here if you want mentions (the `MENTIONS` JSON variable maps an ID to a
handle, for example {"usr_...": "@alice"}).

Configuration (environment):
  RUNBOOK_WEBHOOK_SECRET  signing secret(s) `whsec_...`, comma-separated (required)
  CHAT_WEBHOOK_URL        the chat incoming-webhook URL                  (required)
  MENTIONS                optional JSON object mapping recipient IDs to chat handles
  CHAT_MARKUP             slack (default; Slack, Google Chat) or markdown (Mattermost, Rocket.Chat)
  RELAY_HOST / RELAY_PORT listen address (default 0.0.0.0:8080; 0 picks a free port)
  RELAY_TOLERANCE_S       timestamp tolerance in seconds (default 300)
"""
import hashlib
import hmac
import json
import os
import re
import sys
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MAX_BODY = 1 << 20
HEADLINES = {
    "approval.requested": "Approval requested",
    "approval.escalated": "Approval escalated",
    "approval.expired": "Approval expired",
    "step.blocked": "Step blocked, needs a person",
}


def verify(secrets, timestamp, body, header, now, tolerance):
    """True when the timestamp is fresh and one v1= entry matches a secret."""
    if not (timestamp.isascii() and timestamp.isdigit()) or abs(now - int(timestamp)) > tolerance:
        return False
    signed = timestamp.encode() + b"." + body
    expected = [hmac.new(s.encode(), signed, hashlib.sha256).hexdigest().encode() for s in secrets]
    for entry in header.split(","):
        entry = entry.strip()
        if not entry.startswith("v1="):
            continue
        # Compare bytes: compare_digest raises on non-ASCII str.
        candidate = entry[3:].encode("utf-8", "surrogateescape")
        if any(hmac.compare_digest(candidate, e) for e in expected):
            return True
    return False


def body_length(headers):
    """The declared body length, or None when missing, malformed or negative."""
    raw = headers.get("Content-Length") or "0"
    if not (raw.isascii() and raw.isdigit()):
        return None
    return int(raw)


MARKDOWN_SPECIALS = set("\\`*_{}[]()#+-.!|<>~")


def escape_slack(text):
    """Slack / Google Chat markup: untrusted text cannot add mentions such
    as <!channel> or disguised <https://...|links>."""
    return str(text).replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def escape_markdown(text):
    """Mattermost / Rocket.Chat Markdown: untrusted text cannot add links,
    emphasis or @-mentions (@channel, @all, @here, @user)."""
    escaped = "".join("\\" + c if c in MARKDOWN_SPECIALS else c for c in str(text))
    # A zero-width space after @ keeps the text readable but never a mention.
    return escaped.replace("@", "@\u200b")


ESCAPES = {"slack": escape_slack, "markdown": escape_markdown}


def plain_link(link):
    """An http(s) URL with nothing a chat could reinterpret, else None."""
    return link if isinstance(link, str) and re.fullmatch(r"https?://[A-Za-z0-9._~:/?#@!$&'*+,;=%-]+", link) and not re.search(r"[<>()\[\]|]", link) else None


def message(event, mentions, escape=escape_slack):
    """One line of chat text for a human event. Event text is escaped;
    handles configured in MENTIONS are inserted as given."""
    execution = event.get("execution")
    execution = execution if isinstance(execution, dict) else {}
    data = event.get("data")
    data = data if isinstance(data, dict) else {}
    recipients = data.get("recipients")
    who = [
        mentions[r.get("id")] if isinstance(mentions.get(r.get("id")), str) else escape(r.get("id", "?"))
        for r in (recipients if isinstance(recipients, list) else [])
        if isinstance(r, dict)
    ]
    parts = [escape(f"{HEADLINES[event['type']]}: {execution.get('title') or execution.get('id', 'run')}")]
    if event["type"] == "step.blocked" and data.get("reason"):
        parts.append(escape(f"(reported {data['reason']})"))
    if who:
        parts.append("for " + ", ".join(who))
    link = data.get("link") or execution.get("url")
    if link:
        parts.append("- " + (plain_link(link) or escape(link)))
    return " ".join(parts)


def post(url, text):
    request = urllib.request.Request(
        url,
        data=json.dumps({"text": text}).encode(),
        method="POST",
        headers={"Content-Type": "application/json", "User-Agent": "runbook-chat-notifier"},
    )
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            return 200 <= response.status < 300
    except (urllib.error.URLError, OSError):
        return False


def make_handler(secrets, chat_url, mentions, tolerance, escape=escape_slack):
    seen, inflight, lock = set(), set(), threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def reply(self, status):
            self.send_response(status)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def do_POST(self):
            length = body_length(self.headers)
            if length is None:
                return self.reply(400)
            if length > MAX_BODY:
                return self.reply(413)
            body = self.rfile.read(length)
            if not verify(
                secrets,
                self.headers.get("Runbook-Timestamp", ""),
                body,
                self.headers.get("Runbook-Signature", ""),
                int(time.time()),
                tolerance,
            ):
                return self.reply(401)
            try:
                event = json.loads(body)
            except ValueError:
                return self.reply(400)
            if not isinstance(event, dict):
                return self.reply(400)
            if event.get("type") not in HEADLINES:
                return self.reply(204)
            event_id = event.get("id")
            if not isinstance(event_id, str) or not event_id:
                # Without an ID a redelivery could not be recognized.
                return self.reply(400)
            with lock:
                if event_id in seen:
                    return self.reply(200)
                if event_id in inflight:
                    return self.reply(503)
                inflight.add(event_id)
            posted = False
            try:
                posted = post(chat_url, message(event, mentions, escape))
            finally:
                with lock:
                    inflight.discard(event_id)
                    if posted:
                        seen.add(event_id)
            return self.reply(200 if posted else 502)

        def log_message(self, fmt, *args):
            sys.stderr.write("notifier: %s\n" % (fmt % args))

    return Handler


def main():
    secrets = [s.strip() for s in os.environ.get("RUNBOOK_WEBHOOK_SECRET", "").split(",") if s.strip()]
    chat_url = os.environ.get("CHAT_WEBHOOK_URL", "")
    if not secrets or not chat_url:
        sys.exit("RUNBOOK_WEBHOOK_SECRET and CHAT_WEBHOOK_URL are required")
    mentions = json.loads(os.environ.get("MENTIONS", "{}"))
    markup = os.environ.get("CHAT_MARKUP", "slack")
    if markup not in ESCAPES:
        sys.exit("CHAT_MARKUP must be slack or markdown")
    tolerance = int(os.environ.get("RELAY_TOLERANCE_S", "300"))
    host = os.environ.get("RELAY_HOST", "0.0.0.0")
    port = int(os.environ.get("RELAY_PORT", "8080"))
    server = ThreadingHTTPServer((host, port), make_handler(secrets, chat_url, mentions, tolerance, ESCAPES[markup]))
    print(f"listening on http://{host}:{server.server_address[1]}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
