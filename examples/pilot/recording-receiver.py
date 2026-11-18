#!/usr/bin/env python3
"""The Runbook pilot: a recording webhook receiver (standard library only).

Subscribe it to every event (`runbook webhook create --event '*' ...`) to
see what the server sends. For each POST it:

  * checks `Runbook-Signature` (`v1=<hex HMAC-SHA256 of
    "{Runbook-Timestamp}.{raw body}">`, one entry per secret during a
    rotation) with a constant-time compare, and refuses timestamps more than
    five minutes from this clock (401);
  * deduplicates on the event ID: a redelivery answers 200
    {"duplicate": true} and is not recorded again;
  * prints one line per event (type, ID, execution, sequence, and the step
    run or ready steps it names) and, with RECEIVER_LOG, appends one JSON
    record per event to that file (created 0600).

It never prints or records secrets, signatures or request headers. Event
payloads carry no credentials (PRD section 9).

With RECEIVER_MOCK_GITHUB=1 it also stands in for the GitHub API behind
`examples/webhooks/github-actions-relay/relay.py` (set the relay's
GITHUB_API_URL to this receiver): a POST to
/repos/{owner}/{repo}/actions/workflows/{workflow}/dispatches is recorded
(repo, workflow, ref, execution and event IDs; never the Authorization
header) and answered 204, so the relay can run without contacting GitHub.
The stand-in requires `Authorization: Bearer <RECEIVER_GITHUB_TOKEN>` (the
same dummy value you give the relay as GITHUB_TOKEN), compared in constant
time. The receiver listens on 127.0.0.1 unless RECEIVER_HOST says otherwise;
listen on another interface (for example so a server in a container can
reach it) only on a network you trust.

Configuration (environment):
  RUNBOOK_WEBHOOK_SECRET       signing secret(s) `whsec_...`, comma-separated
  RUNBOOK_WEBHOOK_SECRET_FILE  or a file holding them (re-read per request, so
                               the receiver may start before the subscription)
  RECEIVER_HOST / RECEIVER_PORT  listen address (default 127.0.0.1:9000; 0 picks a port)
  RECEIVER_LOG                 optional JSON-lines file of received events
  RECEIVER_MOCK_GITHUB         1 to serve the GitHub dispatch endpoint
  RECEIVER_GITHUB_TOKEN        bearer token the GitHub stand-in requires (required with it)
  RECEIVER_TOLERANCE_S         timestamp tolerance in seconds (default 300)
"""
import hashlib
import hmac
import json
import os
import re
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

MAX_BODY = 1 << 20
ULID = r"[0-7][0-9A-HJKMNP-TV-Z]{25}"
EVENT = re.compile(r"^evt_[0-9A-HJKMNP-TV-Z]{26}$")
EXECUTION = re.compile(r"^exe_" + ULID + r"$")
STEP_RUN = re.compile(r"^sr_" + ULID + r"$")
TYPE = re.compile(r"^[a-z_]+(\.[a-z_]+)*$")
DISPATCH = re.compile(r"^/repos/([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+)/actions/workflows/([A-Za-z0-9_.-]+\.ya?ml)/dispatches$")
REF = re.compile(r"^[A-Za-z0-9_./-]+$")
PATH = re.compile(r"^[A-Za-z0-9_.#\[\]-]{1,200}$")
RECIPIENT = re.compile(r"^(usr|team)_" + ULID + r"$")


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
        candidate = entry[3:].encode("utf-8", "surrogateescape")
        if any(hmac.compare_digest(candidate, e) for e in expected):
            return True
    return False


def body_length(headers):
    raw = headers.get("Content-Length") or "0"
    if not (raw.isascii() and raw.isdigit()):
        return None
    return int(raw)


def matching(pattern, value):
    """The value when it has the expected shape, else None (never echo junk)."""
    return value if isinstance(value, str) and pattern.match(value) else None


class Config:
    def __init__(self, env):
        self.inline = [s.strip() for s in env.get("RUNBOOK_WEBHOOK_SECRET", "").split(",") if s.strip()]
        self.secret_file = env.get("RUNBOOK_WEBHOOK_SECRET_FILE", "")
        if not self.inline and not self.secret_file:
            sys.exit("set RUNBOOK_WEBHOOK_SECRET or RUNBOOK_WEBHOOK_SECRET_FILE")
        self.tolerance = int(env.get("RECEIVER_TOLERANCE_S", "300"))
        self.mock_github = env.get("RECEIVER_MOCK_GITHUB", "") in ("1", "true")
        self.github_token = env.get("RECEIVER_GITHUB_TOKEN", "")
        if self.mock_github and not self.github_token:
            sys.exit("RECEIVER_MOCK_GITHUB=1 needs RECEIVER_GITHUB_TOKEN (the relay's GITHUB_TOKEN)")
        self.log_path = env.get("RECEIVER_LOG", "")
        self.lock = threading.Lock()
        self.seen = set()
        self.log = None
        if self.log_path:
            fd = os.open(self.log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
            os.fchmod(fd, 0o600)
            self.log = os.fdopen(fd, "a", encoding="utf-8")

    def secrets(self):
        if self.inline:
            return self.inline
        try:
            with open(self.secret_file, encoding="utf-8") as f:
                text = f.read()
        except OSError:
            return []
        return [s.strip() for s in re.split(r"[,\n]", text) if s.strip()]

    def record(self, entry, line):
        with self.lock:
            print(line, flush=True)
            if self.log:
                self.log.write(json.dumps(entry, sort_keys=True) + "\n")
                self.log.flush()


def well_formed(event):
    """The envelope shapes this receiver reads (PRD section 9)."""
    if not isinstance(event, dict):
        return False
    execution = event.get("execution")
    data = event.get("data")
    if execution is not None and not isinstance(execution, dict):
        return False
    if data is not None and not isinstance(data, dict):
        return False
    data = data or {}
    for key in ("ready_steps", "recipients"):
        items = data.get(key)
        if items is not None and not (isinstance(items, list) and all(isinstance(i, dict) for i in items)):
            return False
    return True


def summarize(event, delivery_id):
    """The non-secret facts worth keeping about one event."""
    execution = event.get("execution") or {}
    data = event.get("data") or {}
    entry = {
        "kind": "webhook",
        "type": event.get("type"),
        "id": event.get("id"),
        "execution": matching(EXECUTION, execution.get("id")),
        "sequence": event.get("sequence") if isinstance(event.get("sequence"), int) else None,
        "delivery": delivery_id if re.match(r"^whd_[0-9A-HJKMNP-TV-Z]{26}$", delivery_id) else None,
        "received_at": int(time.time()),
    }
    if isinstance(data.get("reason"), str) and TYPE.match(data["reason"]):
        entry["reason"] = data["reason"]
    if matching(STEP_RUN, data.get("step_run_id")):
        entry["step_run_id"] = data["step_run_id"]
    ready = data.get("ready_steps")
    if isinstance(ready, list):
        entry["ready_steps"] = [
            {"step_run_id": matching(STEP_RUN, s.get("step_run_id")), "path": matching(PATH, s.get("path"))}
            for s in ready
            if isinstance(s, dict)
        ]
    recipients = data.get("recipients")
    if isinstance(recipients, list):
        entry["recipients"] = [
            r.get("id") for r in recipients if isinstance(r, dict) and matching(RECIPIENT, r.get("id"))
        ]
    if isinstance(data.get("status"), str) and TYPE.match(data["status"]):
        entry["status"] = data["status"]
    return entry


def describe(entry):
    parts = ["event", entry["type"], entry["id"]]
    if entry.get("execution"):
        parts.append("exe=%s" % entry["execution"])
    if entry.get("sequence") is not None:
        parts.append("seq=%d" % entry["sequence"])
    if entry.get("reason"):
        parts.append("reason=%s" % entry["reason"])
    if entry.get("step_run_id"):
        parts.append("step_run=%s" % entry["step_run_id"])
    for step in entry.get("ready_steps", []):
        parts.append("ready=%s:%s" % (step["path"], step["step_run_id"]))
    if entry.get("recipients"):
        parts.append("recipients=%s" % ",".join(entry["recipients"]))
    return " ".join(parts)


def make_handler(config):
    class Handler(BaseHTTPRequestHandler):
        def reply(self, status, payload=None):
            data = b"" if payload is None else json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_POST(self):
            length = body_length(self.headers)
            if length is None:
                return self.reply(400, {"error": "invalid_length"})
            if length > MAX_BODY:
                return self.reply(413, {"error": "too_large"})
            body = self.rfile.read(length)
            path = self.path.split("?", 1)[0]
            dispatch = DISPATCH.match(path) if config.mock_github else None
            if dispatch:
                return self.github(dispatch, body)
            secrets = config.secrets()
            if not secrets:
                # Not configured yet: a retryable status brings the event back.
                return self.reply(503, {"error": "secret_not_configured"})
            if not verify(
                secrets,
                self.headers.get("Runbook-Timestamp", ""),
                body,
                self.headers.get("Runbook-Signature", ""),
                int(time.time()),
                config.tolerance,
            ):
                return self.reply(401, {"error": "invalid_signature"})
            try:
                event = json.loads(body)
            except ValueError:
                return self.reply(400, {"error": "invalid_json"})
            if not well_formed(event):
                return self.reply(400, {"error": "invalid_event"})
            event_id = event.get("id", "")
            if not matching(EVENT, event_id) or not matching(TYPE, event.get("type")):
                return self.reply(422, {"error": "invalid_event"})
            with config.lock:
                duplicate = event_id in config.seen
                config.seen.add(event_id)
            if duplicate:
                return self.reply(200, {"duplicate": True})
            entry = summarize(event, self.headers.get("Runbook-Delivery-Id", ""))
            config.record(entry, describe(entry))
            return self.reply(200, {"recorded": True})

        def github(self, dispatch, body):
            # Stand-in for GitHub's workflow_dispatch: only the configured
            # token is accepted; it is compared, never recorded.
            given = self.headers.get("Authorization", "").encode("utf-8", "surrogateescape")
            wanted = ("Bearer " + config.github_token).encode()
            if not hmac.compare_digest(given, wanted):
                return self.reply(401, {"message": "Bad credentials"})
            try:
                request = json.loads(body)
            except ValueError:
                return self.reply(422, {"message": "Problems parsing JSON"})
            inputs = request.get("inputs") if isinstance(request, dict) else None
            inputs = inputs if isinstance(inputs, dict) else {}
            entry = {
                "kind": "github",
                "repo": dispatch.group(1),
                "workflow": dispatch.group(2),
                "ref": matching(REF, request.get("ref")) if isinstance(request, dict) else None,
                "execution_id": matching(EXECUTION, inputs.get("execution_id")),
                "event_id": matching(EVENT, inputs.get("event_id")),
                "received_at": int(time.time()),
            }
            config.record(
                entry,
                "github workflow_dispatch %s %s ref=%s exe=%s event=%s"
                % (entry["repo"], entry["workflow"], entry["ref"], entry["execution_id"], entry["event_id"]),
            )
            return self.reply(204)

        def log_message(self, fmt, *args):
            # Request lines only; never headers or bodies.
            sys.stderr.write("receiver: %s\n" % (fmt % args))

    return Handler


def main():
    config = Config(os.environ)
    host = os.environ.get("RECEIVER_HOST", "127.0.0.1")
    port = int(os.environ.get("RECEIVER_PORT", "9000"))
    server = ThreadingHTTPServer((host, port), make_handler(config))
    print("listening on http://%s:%d" % (host, server.server_address[1]), flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
