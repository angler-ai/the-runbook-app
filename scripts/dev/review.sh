#!/usr/bin/env bash
# The local review stack for the web UI (UI-1): a native runbook-server on
# its own database, seeded with realistic runbooks and runs, and Vite's dev
# server in front of it. One command:
#
#   scripts/dev/review.sh            start, print the sign-in, run until Ctrl-C
#   scripts/dev/review.sh --detach   start, print the sign-in, leave it running
#   scripts/dev/review.sh stop       stop a detached (or stray) run
#
# Every start resets: a previous run is stopped and the database
# (`rb_test_review_ui`, or REVIEW_DB_NAME) is dropped and created again. That is the only
# database this script creates or drops (the server holds other projects'
# data); the admin connection is REVIEW_PG_ADMIN_URL, by default
# postgres://runbook_dev@localhost:5432/runbook_dev.
#
# Secrets (the owner's bootstrap token, the service token, the browser
# token, the webhook key) live only in 0600 files under the private state
# directory ($REVIEW_STATE_DIR, default ${TMPDIR}/runbook-review-ui, 0700);
# they are never printed or put on a command line. The script creates that
# directory itself and marks it ($STATE_DIR/.runbook-review-ui); it never
# uses, empties or deletes a directory without the marker.
#
# Optional theme override (passed to the server as RUNBOOK_THEME_*):
#   REVIEW_THEME_ACCENT, REVIEW_THEME_ACCENT_SECONDARY,
#   REVIEW_THEME_DARK_ACCENT, REVIEW_THEME_DARK_ACCENT_SECONDARY  (#rrggbb)
#
# Other environment: REVIEW_API_PORT (default 18080, or a free port when
# that is taken), REVIEW_VITE_PORT (default 5173), REVIEW_DB_NAME (default
# rb_test_review_ui; must start with rb_test_, so a second stack can run
# beside the first on its own database), CARGO_TARGET_DIR (where the
# binaries are built), REVIEW_SKIP_BUILD=1 (use the existing binaries).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
web="$root/web"

DB_NAME=${REVIEW_DB_NAME:-rb_test_review_ui}
ADMIN_URL=${REVIEW_PG_ADMIN_URL:-postgres://runbook_dev@localhost:5432/runbook_dev}
tmp=${TMPDIR:-/tmp}
STATE_DIR=${REVIEW_STATE_DIR:-${tmp%/}/runbook-review-ui}
VITE_HOST=127.0.0.1
VITE_PORT=${REVIEW_VITE_PORT:-5173}
VITE_URL="http://$VITE_HOST:$VITE_PORT"
target=${CARGO_TARGET_DIR:-$root/target}
SERVER_BIN="$target/debug/runbook-server"
CLI_BIN="$target/debug/runbook"

say() { printf '[review] %s\n' "$*"; }
die() { printf '[review] error: %s\n' "$*" >&2; exit 1; }

# --- The state directory ------------------------------------------------------
# Created by this script (never an existing one) and marked; only a marked
# directory is ever removed, so a mistyped REVIEW_STATE_DIR cannot delete
# anything else.
MARKER=.runbook-review-ui
MARKER_TEXT="runbook-review-ui state directory (scripts/dev/review.sh)"
state_dir_is_ours() {
  [ -d "$STATE_DIR" ] && [ ! -L "$STATE_DIR" ] && [ -f "$STATE_DIR/$MARKER" ] \
    && [ ! -L "$STATE_DIR/$MARKER" ] && [ "$(head -n 1 "$STATE_DIR/$MARKER")" = "$MARKER_TEXT" ]
}
refuse_foreign_state_dir() {
  die "refusing to use or delete $STATE_DIR: it exists but was not created by this script (no $MARKER marker). Pick another REVIEW_STATE_DIR or remove it yourself."
}
remove_state_dir() {
  state_dir_is_ours || refuse_foreign_state_dir
  rm -rf "$STATE_DIR"
}
create_state_dir() {
  mkdir -p "$(dirname "$STATE_DIR")"
  mkdir -m 700 "$STATE_DIR" || die "could not create $STATE_DIR"
  printf '%s\n' "$MARKER_TEXT" > "$STATE_DIR/$MARKER"
}

usage() {
  cat <<'EOF'
usage: scripts/dev/review.sh [--detach]   start (reset) the review stack
       scripts/dev/review.sh stop         stop it
EOF
}

mode=foreground
case "${1:-}" in
  "") ;;
  --detach) mode=detach ;;
  stop) mode=stop ;;
  -h | --help) usage; exit 0 ;;
  *) usage >&2; exit 2 ;;
esac
[ "$#" -le 1 ] || { usage >&2; exit 2; }

# --- Process bookkeeping -------------------------------------------------------
# pid files: $STATE_DIR/<name>.pid, the pid and then the process's start time
# (`ps -o lstart=`, which exec keeps). A pid is only signalled when its start
# time is the recorded one and its command line still names what we started,
# so a recycled pid (even one running a similar command) is left alone.
expected_command() {
  case "$1" in
    server) echo "runbook-server serve" ;;
    vite) echo "vite/bin/vite.js" ;;
    heartbeat) echo "keep-claimed.mjs" ;;
  esac
}
start_time() {
  ps -p "$1" -o lstart= 2> /dev/null | sed 's/^ *//; s/ *$//'
}
record_pid() {
  local name=$1 pid=$2 started
  started=$(start_time "$pid") || started=
  [ -n "$started" ] || die "$name (pid $pid) exited at once; see $STATE_DIR/$name.log"
  printf '%s\n%s\n' "$pid" "$started" > "$STATE_DIR/$name.pid"
}
running_pid() {
  local name=$1 file pid recorded command
  file="$STATE_DIR/$name.pid"
  [ -f "$file" ] || return 1
  pid=$(sed -n 1p "$file")
  recorded=$(sed -n 2p "$file")
  [[ "$pid" =~ ^[0-9]+$ ]] && [ -n "$recorded" ] || return 1
  [ "$(start_time "$pid")" = "$recorded" ] || return 1
  command=$(ps -p "$pid" -o command= 2> /dev/null) || return 1
  case "$command" in
    *"$(expected_command "$name")"*) echo "$pid" ;;
    *) return 1 ;;
  esac
}
stop_one() {
  local name=$1 pid children
  if pid=$(running_pid "$name"); then
    children=$(pgrep -P "$pid" 2> /dev/null || true)
    kill -TERM "$pid" 2> /dev/null || true
    for _ in $(seq 1 50); do
      kill -0 "$pid" 2> /dev/null || break
      sleep 0.2
    done
    if kill -0 "$pid" 2> /dev/null; then
      kill -KILL "$pid" 2> /dev/null || true
    fi
    for cpid in $children; do
      if kill -0 "$cpid" 2> /dev/null; then
        kill -TERM "$cpid" 2> /dev/null || true
        for _ in $(seq 1 20); do
          kill -0 "$cpid" 2> /dev/null || break
          sleep 0.1
        done
        kill -KILL "$cpid" 2> /dev/null || true
      fi
    done
    say "stopped $name (pid $pid)"
  fi
  rm -f "$STATE_DIR/$name.pid"
}
stop_all() {
  state_dir_is_ours || return 0
  stop_one heartbeat
  stop_one vite
  stop_one server
}

# --- Postgres (admin connection; only $DB_NAME is ever created or dropped) --------
pg_env_file() {
  # libpq variables from the admin URL, as `export` lines (never argv).
  ADMIN_URL="$1" python3 -I -c '
import os, shlex, sys
from urllib.parse import parse_qsl, unquote, urlsplit
url = urlsplit(os.environ["ADMIN_URL"])
if url.scheme not in ("postgres", "postgresql"):
    sys.exit("REVIEW_PG_ADMIN_URL must be a postgres:// URL")
pairs = {"PGHOST": url.hostname, "PGPORT": url.port,
         "PGUSER": unquote(url.username) if url.username else None,
         "PGPASSWORD": unquote(url.password) if url.password else None,
         "PGDATABASE": unquote(url.path.lstrip("/")) or None}
for key, value in parse_qsl(url.query):
    if key == "sslmode":
        pairs["PGSSLMODE"] = value
for name, value in pairs.items():
    if value is not None and value != "":
        print("export %s=%s" % (name, shlex.quote(str(value))))
'
}
admin_psql() {
  [[ "$DB_NAME" =~ ^rb_test_[a-z0-9_]+$ ]] || die "refusing database name $DB_NAME (REVIEW_DB_NAME must match rb_test_[a-z0-9_]+)"
  (
    eval "$(pg_env_file "$ADMIN_URL")"
    PGOPTIONS="-c client_min_messages=warning" psql -X -q -v ON_ERROR_STOP=1 -c "$1"
  )
}
database_url() {
  ADMIN_URL="$ADMIN_URL" DB_NAME="$DB_NAME" python3 -I -c '
import os
from urllib.parse import urlsplit, urlunsplit, quote
u = urlsplit(os.environ["ADMIN_URL"])
print(urlunsplit((u.scheme, u.netloc, "/" + quote(os.environ["DB_NAME"]), u.query, u.fragment)))
'
}
drop_database() {
  admin_psql "DROP DATABASE IF EXISTS \"$DB_NAME\" WITH (FORCE)"
}

port_in_use() {
  lsof -nP -iTCP:"$1" -sTCP:LISTEN > /dev/null 2>&1
}
free_port() {
  python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()'
}

if [ "$mode" = stop ]; then
  if [ ! -e "$STATE_DIR" ]; then
    say "nothing to stop ($STATE_DIR does not exist)"
    exit 0
  fi
  state_dir_is_ours || refuse_foreign_state_dir
  stop_all
  drop_database && say "dropped database $DB_NAME"
  remove_state_dir
  say "stopped; removed $STATE_DIR"
  exit 0
fi

# --- Start ------------------------------------------------------------------------
# A directory this script did not create is refused before anything else.
if [ -e "$STATE_DIR" ] && ! state_dir_is_ours; then
  refuse_foreign_state_dir
fi
for tool in python3 psql curl node lsof openssl; do
  command -v "$tool" > /dev/null || die "$tool is not on PATH"
done

# A previous run of this script: stop it and forget its state.
if [ -e "$STATE_DIR" ]; then
  say "stopping a previous run"
  stop_all
  remove_state_dir
fi
if port_in_use "$VITE_PORT"; then
  die "port $VITE_PORT is in use by another process (Vite is fixed to $VITE_URL with strictPort). Stop it first: lsof -nP -iTCP:$VITE_PORT -sTCP:LISTEN"
fi
API_PORT=${REVIEW_API_PORT:-18080}
if port_in_use "$API_PORT"; then
  [ -z "${REVIEW_API_PORT:-}" ] || die "REVIEW_API_PORT $API_PORT is in use"
  API_PORT=$(free_port)
fi
API_URL="http://127.0.0.1:$API_PORT"

umask 077
create_state_dir


if [ "${REVIEW_SKIP_BUILD:-}" != 1 ]; then
  command -v cargo > /dev/null || die "cargo is not on PATH (or set REVIEW_SKIP_BUILD=1)"
  say "cargo build -p runbook-server -p runbook-cli --locked (log: $STATE_DIR/cargo-build.log)"
  (cd "$root" && cargo build -p runbook-server -p runbook-cli --locked) > "$STATE_DIR/cargo-build.log" 2>&1 \
    || { tail -n 25 "$STATE_DIR/cargo-build.log" >&2; die "cargo build failed"; }
fi
for bin in "$SERVER_BIN" "$CLI_BIN"; do
  [ -x "$bin" ] || die "$bin is missing; build it (cargo build -p runbook-server -p runbook-cli) or set CARGO_TARGET_DIR"
done

if [ ! -d "$web/node_modules" ]; then
  say "installing web dependencies (npm ci --ignore-scripts)"
  (cd "$web" && npm ci --ignore-scripts) > "$STATE_DIR/npm-ci.log" 2>&1 \
    || { tail -n 25 "$STATE_DIR/npm-ci.log" >&2; die "npm ci failed"; }
fi
if [ ! -d "$root/auth/node_modules" ]; then
  say "installing auth dependencies (npm ci --ignore-scripts)"
  (cd "$root/auth" && npm ci --ignore-scripts) > "$STATE_DIR/npm-ci-auth.log" 2>&1 \
    || { tail -n 25 "$STATE_DIR/npm-ci-auth.log" >&2; die "npm ci for auth failed"; }
fi
if [ ! -f "$root/auth/dist/server.mjs" ] || [ "${REVIEW_SKIP_BUILD:-}" != 1 ]; then
  say "building auth service (npm run build)"
  (cd "$root/auth" && npm run build) > "$STATE_DIR/auth-build.log" 2>&1 \
    || { tail -n 25 "$STATE_DIR/auth-build.log" >&2; die "auth build failed"; }
fi

forget_secrets() {
  rm -rf "$STATE_DIR/config"
  rm -f "$STATE_DIR"/*.pat "$STATE_DIR"/*.svc "$STATE_DIR"/*.token "$STATE_DIR"/*.header \
    "$STATE_DIR/bootstrap.json" "$STATE_DIR/server.env" "$STATE_DIR/webhook.key" \
    "$STATE_DIR/auth.secret" "$STATE_DIR/codes.jsonl"
}
# Foreground runs and failed starts tear everything down. Ctrl-C removes the
# state dir; a failure keeps its logs (secrets are deleted either way).
started=0
cleanup() {
  local code=$?
  trap - EXIT INT TERM
  if [ "$started" = 1 ]; then
    echo
    stop_all
    drop_database > /dev/null 2>&1 && say "dropped database $DB_NAME"
    if [ "$code" = 0 ] || [ "$code" = 130 ] || [ "$code" = 143 ]; then
      remove_state_dir
    else
      forget_secrets
      say "logs kept in $STATE_DIR (scripts/dev/review.sh stop removes them)"
    fi
  fi
  exit "$code"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
started=1

say "resetting database $DB_NAME"
drop_database
admin_psql "CREATE DATABASE \"$DB_NAME\""
openssl rand -base64 32 > "$STATE_DIR/webhook.key"
openssl rand -hex 32 > "$STATE_DIR/auth.secret"
chmod 0600 "$STATE_DIR/auth.secret"
touch "$STATE_DIR/codes.jsonl"
chmod 0600 "$STATE_DIR/codes.jsonl"

AUTH_SECRET=$(head -n 1 "$STATE_DIR/auth.secret")
OWNER_EMAIL="owner@review.local"

# The server's environment, in a 0600 file sourced by its launcher (so the
# database URL and webhook key never reach a command line).
{
  printf 'export DATABASE_URL=%q\n' "$(database_url)"
  printf 'export RUNBOOK_ORIGIN=%q\n' "$VITE_URL"
  printf 'export RUNBOOK_LISTEN_ADDR=%q\n' "127.0.0.1:$API_PORT"
  printf 'export RUNBOOK_WEBHOOK_SECRET_KEY=%q\n' "$(head -n 1 "$STATE_DIR/webhook.key")"
  printf 'export RUNBOOK_AUTH_SECRET=%q\n' "$AUTH_SECRET"
  printf 'export RUNBOOK_AUTH_METHODS=%q\n' "email"
  printf 'export RUNBOOK_AUTH_ALLOWED_DOMAINS=%q\n' "review.local"
  printf 'export RUNBOOK_AUTH_EMAIL_TRANSPORT=%q\n' "capture"
  printf 'export RUNBOOK_AUTH_CAPTURE_FILE=%q\n' "$STATE_DIR/codes.jsonl"
  printf 'export RUNBOOK_AUTH_COMMAND=%q\n' "node $root/auth/dist/server.mjs"
  for name in ACCENT ACCENT_SECONDARY DARK_ACCENT DARK_ACCENT_SECONDARY; do
    value=$(printenv "REVIEW_THEME_$name" || true)
    [ -z "$value" ] || printf 'export RUNBOOK_THEME_%s=%q\n' "$name" "$value"
  done
} > "$STATE_DIR/server.env"
unset RUNBOOK_TOKEN RUNBOOK_HOST

# Each long-running process gets its own session (setsid, then exec the
# command in the same process, so recorded pids stay valid): ending the
# terminal or agent session that ran this script must not take the stack
# with it.
DETACH=(python3 -I -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])')

say "starting runbook-server on $API_URL (log: $STATE_DIR/server.log)"
(
  # shellcheck disable=SC1091
  . "$STATE_DIR/server.env"
  exec nohup "${DETACH[@]}" "$SERVER_BIN" serve
) > "$STATE_DIR/server.log" 2>&1 < /dev/null &
record_pid server $!

wait_http() {
  local url=$1 name=$2
  for _ in $(seq 1 240); do
    if curl -fsS -o /dev/null --max-time 2 "$url" 2> /dev/null; then
      return 0
    fi
    running_pid "$name" > /dev/null || {
      tail -n 25 "$STATE_DIR/$name.log" >&2
      die "$name exited; see $STATE_DIR/$name.log"
    }
    sleep 0.25
  done
  die "$name did not answer $url within 60 s; see $STATE_DIR/$name.log"
}
wait_http "$API_URL/v1/meta" server
wait_http "$API_URL/api/auth/ok" server
grep -i '^theme:\|theme: ' "$STATE_DIR/server.log" 2> /dev/null | sed 's/^/[review] server: /' || true

say "bootstrapping the owner ($OWNER_EMAIL)"
(
  # shellcheck disable=SC1091
  . "$STATE_DIR/server.env"
  "$SERVER_BIN" bootstrap --owner-email "$OWNER_EMAIL" --owner-name "Riley Chen" --with-token
) > "$STATE_DIR/bootstrap.json"

say "seeding (scripts/dev/seed-review.mjs; log: $STATE_DIR/seed.log)"
REVIEW_STATE_DIR="$STATE_DIR" RUNBOOK_BIN="$CLI_BIN" REVIEW_API_URL="$API_URL" \
  node "$here/seed-review.mjs" > "$STATE_DIR/seed.log" 2>&1 \
  || { tail -n 30 "$STATE_DIR/seed.log" >&2; die "seeding failed; see $STATE_DIR/seed.log"; }
sed -n 's/^summary: /[review]   /p' "$STATE_DIR/seed.log"
REVIEW_STATE_DIR="$STATE_DIR" RUNBOOK_BIN="$CLI_BIN" REVIEW_API_URL="$API_URL" \
  nohup "${DETACH[@]}" node "$here/keep-claimed.mjs" > "$STATE_DIR/heartbeat.log" 2>&1 < /dev/null &
record_pid heartbeat $!

say "starting Vite on $VITE_URL (log: $STATE_DIR/vite.log)"
(
  cd "$web"
  RUNBOOK_DEV_API="$API_URL" BROWSER=none exec nohup "${DETACH[@]}" node "$web/node_modules/vite/bin/vite.js" \
    --host "$VITE_HOST" --port "$VITE_PORT" --strictPort
) > "$STATE_DIR/vite.log" 2>&1 < /dev/null &
record_pid vite $!
wait_http "$VITE_URL/" vite
wait_http "$VITE_URL/v1/meta" vite
wait_http "$VITE_URL/api/auth/ok" vite

ops_id=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["projects"]["ops"])' "$STATE_DIR/state.json")
sandbox_id=$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["projects"]["sandbox"])' "$STATE_DIR/state.json")

cat <<EOF

The Runbook review stack is up.

  Web UI     $VITE_URL
  API        $API_URL  (projects: ops $ops_id, sandbox $sandbox_id)
  State      $STATE_DIR  (logs, pid files, 0600 secrets)

Sign in: open $VITE_URL and sign in with email:
  $OWNER_EMAIL
Read the six-digit sign-in code from:
  $STATE_DIR/codes.jsonl
  (e.g.: tail -n 1 "$STATE_DIR/codes.jsonl" | jq -r .code)

Or paste the browser token from:
  $STATE_DIR/browser.token
  (macOS: pbcopy < "$STATE_DIR/browser.token")
It is Riley Chen's (the owner's) narrow browser token, valid for 30 days.

Query the API directly with the same token:
  curl -s -H @"$STATE_DIR/browser.header" "$API_URL/v1/projects/$ops_id/runbooks?limit=25"

Theme override: set any of REVIEW_THEME_ACCENT, REVIEW_THEME_ACCENT_SECONDARY,
REVIEW_THEME_DARK_ACCENT, REVIEW_THEME_DARK_ACCENT_SECONDARY (#rrggbb) and run
this script again, e.g. REVIEW_THEME_ACCENT='#4338ca' scripts/dev/review.sh

EOF

if [ "$mode" = detach ]; then
  started=0
  trap - EXIT INT TERM
  echo "Detached. Stop it with: scripts/dev/review.sh stop"
  exit 0
fi

echo "Press Ctrl-C to stop (server and Vite are stopped and the database dropped)."
while running_pid server > /dev/null && running_pid vite > /dev/null; do
  sleep 1
done
say "a process exited; see $STATE_DIR/server.log and $STATE_DIR/vite.log"
exit 1
