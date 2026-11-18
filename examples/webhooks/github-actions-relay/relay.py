#!/usr/bin/env python3
"""The Runbook -> GitHub Actions relay (example; you own and run it).

Receives The Runbook's `agent.work_available` webhooks and starts the
`runbook-resume.yml` workflow with `workflow_dispatch`, passing the
execution ID and the event ID as inputs. Standard library only.

Every request is checked before anything else happens:
  * `Runbook-Signature` must carry a `v1=` HMAC-SHA256 of
    "{Runbook-Timestamp}.{raw body}" under one of the configured secrets
    (during a 24-hour rotation the header lists one entry per secret);
  * `Runbook-Timestamp` must be within five minutes of this clock;
  * the event ID is deduplicated: a redelivered event is acknowledged
    without dispatching again. An event is remembered only after GitHub
    accepted the dispatch, so a failed dispatch (answered 502) is retried
    by The Runbook's delivery backoff.

Configuration (environment):
  RUNBOOK_WEBHOOK_SECRET  signing secret(s) `whsec_...`, comma-separated  (required)
  GITHUB_TOKEN            token allowed to dispatch workflows              (required)
  GITHUB_API_URL          default https://api.github.com
  GITHUB_REPOSITORY       owner/repo used when the runbook's spec.agents.params has no `repo`
  RELAY_ALLOWED_REPOS     comma-separated owner/repo allow-list (default: GITHUB_REPOSITORY)
  RELAY_ALLOWED_WORKFLOWS comma-separated workflow-file allow-list (default: RUNBOOK_WORKFLOW)
  RELAY_ALLOWED_REFS      comma-separated ref allow-list (default: GITHUB_REF)
  RUNBOOK_WORKFLOW        default workflow file when params have no `workflow` (runbook-resume.yml)
  GITHUB_REF              default ref when params have no `ref` (main)
  RELAY_DEDUP_FILE        optional file remembering handled event IDs across restarts
  RELAY_HOST / RELAY_PORT listen address (default 0.0.0.0:8080; port 0 picks a free port)
  RELAY_TOLERANCE_S       timestamp tolerance in seconds (default 300)

Secrets are never logged or echoed. Run it behind HTTPS (a load balancer,
a tunnel or a platform router): The Runbook delivers webhooks only to
public HTTPS URLs unless its operator allows private targets.
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
REPO = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
WORKFLOW = re.compile(r"^[A-Za-z0-9_.-]+\.ya?ml$")
REF = re.compile(r"^[A-Za-z0-9_./-]+$")
EXECUTION = re.compile(r"^exe_[0-7][0-9A-HJKMNP-TV-Z]{25}$")
EVENT = re.compile(r"^evt_[0-9A-HJKMNP-TV-Z]{26}$")


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


class Seen:
    """Handled event IDs, optionally persisted one per line."""

    def __init__(self, path):
        self.path = path
        self.ids = set()
        self.inflight = set()
        self.lock = threading.Lock()
        if path and os.path.exists(path):
            with open(path, encoding="utf-8") as f:
                self.ids = {line.strip() for line in f if line.strip()}

    def claim(self, event_id):
        """'duplicate', 'busy' (another request is dispatching it) or 'mine'."""
        with self.lock:
            if event_id in self.ids:
                return "duplicate"
            if event_id in self.inflight:
                return "busy"
            self.inflight.add(event_id)
            return "mine"

    def finish(self, event_id, handled):
        with self.lock:
            self.inflight.discard(event_id)
            if handled:
                self.ids.add(event_id)
                if self.path:
                    with open(self.path, "a", encoding="utf-8") as f:
                        f.write(event_id + "\n")


class Config:
    def __init__(self, env):
        self.secrets = [s.strip() for s in env.get("RUNBOOK_WEBHOOK_SECRET", "").split(",") if s.strip()]
        self.token = env.get("GITHUB_TOKEN", "")
        if not self.secrets or not self.token:
            sys.exit("RUNBOOK_WEBHOOK_SECRET and GITHUB_TOKEN are required")
        self.api = env.get("GITHUB_API_URL", "https://api.github.com").rstrip("/")
        self.repository = env.get("GITHUB_REPOSITORY", "")
        allowed = env.get("RELAY_ALLOWED_REPOS", self.repository)
        self.allowed = {r.strip() for r in allowed.split(",") if r.strip()}
        self.workflow = env.get("RUNBOOK_WORKFLOW", "runbook-resume.yml")
        self.ref = env.get("GITHUB_REF", "main")
        # repo, workflow and ref can all come from the runbook's params,
        # which project editors control: each is allow-listed, so an
        # editor cannot dispatch another workflow or ref with this token.
        workflows = env.get("RELAY_ALLOWED_WORKFLOWS", self.workflow)
        self.allowed_workflows = {w.strip() for w in workflows.split(",") if w.strip()}
        refs = env.get("RELAY_ALLOWED_REFS", self.ref)
        self.allowed_refs = {r.strip() for r in refs.split(",") if r.strip()}
        self.tolerance = int(env.get("RELAY_TOLERANCE_S", "300"))
        self.seen = Seen(env.get("RELAY_DEDUP_FILE"))


def dispatch(config, repo, workflow, ref, execution_id, event_id):
    """POST /repos/{owner}/{repo}/actions/workflows/{workflow}/dispatches."""
    url = f"{config.api}/repos/{repo}/actions/workflows/{workflow}/dispatches"
    body = json.dumps({"ref": ref, "inputs": {"execution_id": execution_id, "event_id": event_id}})
    request = urllib.request.Request(
        url,
        data=body.encode(),
        method="POST",
        headers={
            "Authorization": f"Bearer {config.token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Content-Type": "application/json",
            "User-Agent": "runbook-github-actions-relay",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=8) as response:
            return 200 <= response.status < 300
    except (urllib.error.URLError, OSError):
        return False


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
            if not verify(
                config.secrets,
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
            if not isinstance(event, dict):
                return self.reply(400, {"error": "invalid_event"})
            if event.get("type") != "agent.work_available":
                return self.reply(204)
            execution = event.get("execution")
            data = event.get("data")
            data = {} if data is None else data
            params = data.get("params") if isinstance(data, dict) else None
            params = {} if params is None and isinstance(data, dict) else params
            if not isinstance(execution, dict) or not isinstance(params, dict):
                return self.reply(422, {"error": "invalid_event"})
            event_id = event.get("id", "")
            execution_id = execution.get("id", "")
            if not (isinstance(event_id, str) and isinstance(execution_id, str)) or not (
                EVENT.match(event_id) and EXECUTION.match(execution_id)
            ):
                return self.reply(422, {"error": "invalid_event"})
            repo = params.get("repo") or config.repository
            workflow = params.get("workflow") or config.workflow
            ref = params.get("ref") or config.ref
            if not all(isinstance(v, str) for v in (repo, workflow, ref)) or not (
                REPO.match(repo) and WORKFLOW.match(workflow) and REF.match(ref) and ".." not in ref
            ):
                return self.reply(422, {"error": "invalid_params"})
            if repo not in config.allowed:
                return self.reply(403, {"error": "repository_not_allowed"})
            if workflow not in config.allowed_workflows:
                return self.reply(403, {"error": "workflow_not_allowed"})
            if ref not in config.allowed_refs:
                return self.reply(403, {"error": "ref_not_allowed"})
            state = config.seen.claim(event_id)
            if state == "duplicate":
                return self.reply(200, {"duplicate": True})
            if state == "busy":
                # Another delivery of this event is dispatching right now;
                # a retryable status lets The Runbook come back later.
                return self.reply(503, {"error": "in_progress"})
            handled = False
            try:
                handled = dispatch(config, repo, workflow, ref, execution_id, event_id)
            finally:
                config.seen.finish(event_id, handled)
            if not handled:
                return self.reply(502, {"error": "dispatch_failed"})
            return self.reply(200, {"dispatched": True})

        def log_message(self, fmt, *args):
            # Request lines only; never headers or bodies (they carry signatures).
            sys.stderr.write("relay: %s\n" % (fmt % args))

    return Handler


def main():
    config = Config(os.environ)
    host = os.environ.get("RELAY_HOST", "0.0.0.0")
    port = int(os.environ.get("RELAY_PORT", "8080"))
    server = ThreadingHTTPServer((host, port), make_handler(config))
    print(f"listening on http://{host}:{server.server_address[1]}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
