#!/usr/bin/env bash
# ARC2 retention smoke (PRD §14 "Retention and daily archives"; D53–D54)
# with an accelerated clock.
#
# The clock offset (RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS) is test-only: it
# exists in debug builds, and a release build refuses to start with it
# (config.test_clock_refused, like UploadConfig::test_direct_put). So unlike
# the container smokes this one runs a debug runbook-server on the host,
# against its own rb_test_retention_<pid> database on DATABASE_URL's server,
# which it creates and drops (EXIT trap). It touches no other database, no
# container and no network beyond loopback.
#
# Flow:
#   1. An agent run finishes, with a secret in its logs.
#   2. Restart one day later: the day's file is written, the run is still
#      live, the secret is redacted in the file, files never expire by default.
#   3. Restart 91 days later with RUNBOOK_RETENTION_LIVE_DAYS=90: live detail
#      is deleted (report 410, rerun 422 execution.inputs_expired); the
#      archived run still opens read-only and the same bytes still download.
#
# Needs: cargo, psql, curl, python3, and DATABASE_URL naming a role that may
# CREATE DATABASE (for example postgres://runbook_dev@localhost:5432/runbook_dev).
set -euo pipefail
cd "$(dirname "$0")/../.."

started=$SECONDS
log() { printf '[retention +%ss] %s\n' "$((SECONDS - started))" "$*"; }
fail() {
  printf '[retention] FAIL: %s\n' "$*" >&2
  exit 1
}
: "${DATABASE_URL:?set DATABASE_URL to a Postgres role that may create databases}"

admin_url=$DATABASE_URL
db="rb_test_retention_$$"
work=$(mktemp -d "${TMPDIR:-/tmp}/runbook-retention.XXXXXX")
server_pid=
cleanup() {
  local status=$?
  if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
  if [ "$status" -ne 0 ] && [ -f "$work/server.log" ]; then
    printf '[retention] server log tail:\n' >&2
    tail -n 30 "$work/server.log" >&2 || true
  fi
  psql "$admin_url" -q -v ON_ERROR_STOP=1 -c "DROP DATABASE IF EXISTS $db WITH (FORCE)" >/dev/null 2>&1 ||
    printf '[retention] could not drop %s; drop it by hand\n' "$db" >&2
  rm -rf "$work"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

log "building runbook-server (debug: the test clock exists only there)"
cargo build -q --locked -p runbook-server
bin="${CARGO_TARGET_DIR:-target}/debug/runbook-server"
[ -x "$bin" ] || fail "no debug runbook-server at $bin"

psql "$admin_url" -q -v ON_ERROR_STOP=1 -c "CREATE DATABASE $db TEMPLATE template0" >/dev/null
db_url=$(python3 -I -c 'import sys, urllib.parse as u; p = u.urlsplit(sys.argv[1]); print(u.urlunsplit(p._replace(path="/" + sys.argv[2])))' "$admin_url" "$db")
port=$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
origin="http://127.0.0.1:$port"
export DATABASE_URL="$db_url" RUNBOOK_ORIGIN="$origin" RUNBOOK_LISTEN_ADDR="127.0.0.1:$port"
unset RUNBOOK_AUTH_SECRET RUNBOOK_RETENTION_LIVE_DAYS RUNBOOK_RETENTION_ARCHIVE_DAYS \
  RUNBOOK_RETENTION_SUMMARY_DAYS RUNBOOK_RETENTION_PROJECT_OVERRIDES RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS

json() { python3 -I -c 'import json, sys; v = json.load(sys.stdin)
for k in sys.argv[1:]: v = v[int(k)] if isinstance(v, list) else v[k]
print(v if not isinstance(v, (dict, list)) else json.dumps(v))' "$@"; }

# api METHOD PATH [BODY] [KEY] [TOKEN]: body to $work/body, status on stdout.
api() {
  local args=(-s -o "$work/body" -w '%{http_code}' -X "$1" "$origin$2" -H "authorization: Bearer ${5:-$owner}")
  [ -n "${3:-}" ] && args+=(-H 'content-type: application/json' --data "$3")
  [ -n "${4:-}" ] && args+=(-H "idempotency-key: $4")
  curl "${args[@]}"
}

start_server() { # offset_days live_days
  stop_server
  RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS=$(($1 * 86400000)) \
    RUNBOOK_RETENTION_LIVE_DAYS="$2" "$bin" serve >>"$work/server.log" 2>&1 &
  server_pid=$!
  for _ in $(seq 1 100); do
    curl -sf "$origin/v1/meta" >/dev/null 2>&1 && return
    kill -0 "$server_pid" 2>/dev/null || fail "server exited"
    sleep 0.1
  done
  fail "server did not answer /v1/meta"
}
stop_server() {
  if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
  server_pid=
}
wait_for() { # description, command...
  local what=$1
  shift
  for _ in $(seq 1 150); do
    "$@" && return
    sleep 0.2
  done
  fail "timed out waiting for $what"
}

"$bin" bootstrap >"$work/issued.json" 2>>"$work/server.log"
owner=$(json pat <"$work/issued.json")
project=$(json project_id <"$work/issued.json")
log "bootstrapped an owned instance ($db)"

start_server 0 ""
spec=$'apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: deploy, title: Deploy}\nspec:\n  steps:\n    - id: ship\n      title: Ship it\n      actor: agent\n'
body=$(python3 -I -c 'import json, sys; print(json.dumps({"source": sys.argv[1]}))' "$spec")
[ "$(api POST "/v1/projects/$project/runbooks" "$body" create)" = 201 ] || fail "create: $(cat "$work/body")"
rb=$(json runbook_id <"$work/body")
etag=$(curl -s -D - -o /dev/null "$origin/v1/runbooks/$rb/draft" -H "authorization: Bearer $owner" | tr -d '\r' | awk -F': ' 'tolower($1) == "etag" {print $2}')
status=$(curl -s -o "$work/body" -w '%{http_code}' -X POST "$origin/v1/runbooks/$rb/versions" -H "authorization: Bearer $owner" \
  -H 'content-type: application/json' -H 'idempotency-key: publish' -H "if-match: $etag" --data '{}')
[ "$status" = 201 ] || fail "publish: $(cat "$work/body")"
[ "$(api POST "/v1/projects/$project/executions" '{"runbook":"deploy"}' start)" = 201 ] || fail "start: $(cat "$work/body")"
exe=$(json id <"$work/body")
activated() { api GET "/v1/executions/$exe" >/dev/null && [ "$(json status <"$work/body")" = running ]; }
wait_for "activation" activated
[ "$(api POST "/v1/executions/$exe/tokens" '{"session_id":"smoke-agent"}' mint)" = 201 ] || fail "mint: $(cat "$work/body")"
agent=$(json access_token <"$work/body")
api GET "/v1/executions/$exe/next" "" "" "$agent" >/dev/null
sr=$(json ready_work 0 step_run_id <"$work/body")
[ "$(api POST "/v1/step-runs/$sr/claim" '{"session_id":"smoke-agent","actor":{"kind":"agent","agent_name":"smoke","model":"none"}}' claim "$agent")" = 200 ] ||
  fail "claim: $(cat "$work/body")"
claim=$(cat "$work/body")
secret="hunter2-$(openssl rand -hex 8)"
report=$(python3 -I -c 'import json, sys; print(json.dumps({"step_run_id": sys.argv[1], "claim": json.loads(sys.argv[2]),
  "status": "succeeded", "verdict": "pass", "summary": "shipped", "evidence": [],
  "logs": ["deploying build 42", "password=" + sys.argv[3]]}))' "$sr" "$claim" "$secret")
[ "$(api POST "/v1/step-runs/$sr/report" "$report" report "$agent")" = 200 ] || fail "report: $(cat "$work/body")"
succeeded() { api GET "/v1/executions/$exe" >/dev/null && [ "$(json status <"$work/body")" = succeeded ]; }
wait_for "the run to succeed" succeeded
[ "$(api GET "/v1/runbooks/$rb/archives")" = 200 ] && [ "$(json items <"$work/body")" = "[]" ] || fail "archived before the day ended"
log "run $exe succeeded; nothing archived during its own day"

start_server 1 ""
archived() { api GET "/v1/runbooks/$rb/archives" >/dev/null && [ "$(json items <"$work/body")" != "[]" ]; }
wait_for "the day's file" archived
date=$(json items 0 date <"$work/body")
[ "$(json items 0 run_count <"$work/body")" = 1 ] || fail "run count: $(cat "$work/body")"
[ "$(json items 0 expires_at_ms <"$work/body")" = None ] || fail "files expire by default: $(cat "$work/body")"
[ "$(api GET "/v1/runbooks/$rb/archives/$date")" = 200 ] || fail "download: $(cat "$work/body")"
cp "$work/body" "$work/day.zip"
python3 -I - "$work/day.zip" "$secret" "$exe" <<'PY' || fail "archive contents"
import json, sys, zipfile
path, secret, exe = sys.argv[1:]
with zipfile.ZipFile(path) as z:
    text = "\n".join(z.read(n).decode("utf-8", "replace") for n in z.namelist())
    manifest = json.loads(z.read("manifest.json"))
    assert z.testzip() is None
assert secret not in text, "the secret reached the archive"
assert "[REDACTED:password]" in text, "no redaction marker"
assert [r["execution_id"] for r in manifest["runs"]] == [exe], manifest["runs"]
PY
[ "$(api GET "/v1/executions/$exe")" = 200 ] || fail "live detail gone after one day"
log "day $date archived: one run, secret redacted, run still live"

start_server 91 90
expired() { [ "$(api GET "/v1/executions/$exe/report")" = 410 ]; }
wait_for "retention to delete live detail" expired
[ "$(json code <"$work/body")" = execution.history_expired ] || fail "report: $(cat "$work/body")"
[ "$(api POST "/v1/executions/$exe/rerun" '{}' rerun)" = 422 ] && [ "$(json code <"$work/body")" = execution.inputs_expired ] ||
  fail "rerun after expiry: $(cat "$work/body")"
[ "$(api GET "/v1/executions/$exe/archive")" = 200 ] && [ "$(json live <"$work/body")" = False ] ||
  fail "archived run: $(cat "$work/body")"
[ "$(api GET "/v1/runbooks/$rb/archives/$date")" = 200 ] && cmp -s "$work/body" "$work/day.zip" ||
  fail "the file changed when live detail was deleted"
log "91 days later: live detail deleted (410, rerun 422), archive unchanged and readable"
log "PASS in $((SECONDS - started))s"
