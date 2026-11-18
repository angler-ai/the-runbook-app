# Shared helpers for the D1 container smoke tests. Source it; do not run it.
#
# Each script gets its own compose project (runbook-smoke-<name>-<pid>) and an
# EXIT trap that runs `docker compose -p <that project> down -v
# --remove-orphans`, so a run never touches other containers or volumes. The
# image is built once and tagged runbook-pilot:smoke (RUNBOOK_SMOKE_IMAGE);
# set RUNBOOK_SMOKE_SKIP_BUILD=1 to reuse it as is (run_all.sh does).
#
# Secrets (database password, webhook key, bootstrap tokens) are generated per
# run, live only in this shell's environment, and are never printed. Tokens
# reach the CLI with `exec -e RUNBOOK_TOKEN` (name only), never on a command
# line.
#
# Needs: docker with compose v2, openssl, curl, python3.
set -euo pipefail

SMOKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DEPLOY_DIR=$(dirname "$SMOKE_DIR")
REPO_DIR=$(dirname "$DEPLOY_DIR")
SMOKE_NAME=$(basename "$0" .sh | tr '_' '-')
SMOKE_PROJECT="runbook-smoke-${SMOKE_NAME}-$$"
SMOKE_STARTED=$SECONDS

# Everything compose interpolates, set explicitly so an operator's deploy/.env
# or shell never leaks into a smoke run (--env-file /dev/null below).
export RUNBOOK_IMAGE=${RUNBOOK_SMOKE_IMAGE:-runbook-pilot:smoke}
RUNBOOK_CACHE_ID="${RUNBOOK_CACHE_ID:-$(git -C "$REPO_DIR" rev-parse --abbrev-ref HEAD 2>/dev/null | tr -c 'a-zA-Z0-9_-' '-' | sed 's/^-*//; s/-*$//' || true)}"
export RUNBOOK_CACHE_ID="${RUNBOOK_CACHE_ID:-default}"
export POSTGRES_USER=runbook POSTGRES_DB=runbook RUNBOOK_DB_USER=runbook_app
POSTGRES_PASSWORD=$(openssl rand -hex 24)
RUNBOOK_DB_PASSWORD=$(openssl rand -hex 24)
RUNBOOK_WEBHOOK_SECRET_KEY=$(openssl rand -base64 32)
export POSTGRES_PASSWORD RUNBOOK_DB_PASSWORD RUNBOOK_WEBHOOK_SECRET_KEY
export RUNBOOK_ORIGIN=http://localhost:8080
export RUNBOOK_BIND_ADDRESS=127.0.0.1 RUNBOOK_HOST_PORT=0 # ephemeral host port
export RUNBOOK_USERCONTENT_HOST_PORT=0 # the user-content listener's, too
export RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS=false RUNBOOK_ALLOW_LINK_LOCAL_WEBHOOK_TARGETS=false
unset RUNBOOK_TOKEN RUNBOOK_WEB_DIR # the image's web UI default applies
# Sign-in (D48) is off unless a script turns it on (sign_in.sh), and only
# sign_in.sh starts the smoke profile's Mailpit.
unset RUNBOOK_AUTH_SECRET RUNBOOK_AUTH_METHODS RUNBOOK_AUTH_ALLOWED_DOMAINS \
  RUNBOOK_SMTP_URL RUNBOOK_SMTP_FROM RUNBOOK_GOOGLE_CLIENT_ID RUNBOOK_GOOGLE_CLIENT_SECRET \
  RUNBOOK_MAILPIT_PORT COMPOSE_PROFILES
# Uploads (D49–D51) are off unless a script turns them on (uploads.sh).
unset RUNBOOK_UPLOAD_SIGNING_KEY RUNBOOK_USERCONTENT_ORIGIN

log() { printf '[%s +%ss] %s\n' "$SMOKE_NAME" "$((SECONDS - SMOKE_STARTED))" "$*"; }
fail() {
  printf '[%s] FAIL: %s\n' "$SMOKE_NAME" "$*" >&2
  exit 1
}

# A script may set SMOKE_COMPOSE_FILE (another stack) and
# SMOKE_COMPOSE_OVERRIDE (an extra -f file) before sourcing this file.
SMOKE_COMPOSE_FILE=${SMOKE_COMPOSE_FILE:-$DEPLOY_DIR/docker-compose.yml}
compose() {
  if [ -n "${SMOKE_COMPOSE_OVERRIDE:-}" ]; then
    docker compose -p "$SMOKE_PROJECT" -f "$SMOKE_COMPOSE_FILE" -f "$SMOKE_COMPOSE_OVERRIDE" --env-file /dev/null "$@"
  else
    docker compose -p "$SMOKE_PROJECT" -f "$SMOKE_COMPOSE_FILE" --env-file /dev/null "$@"
  fi
}

smoke_cleanup() {
  local status=$?
  if [ "$status" -ne 0 ]; then
    printf '[%s] server log tail:\n' "$SMOKE_NAME" >&2
    compose logs --no-color --tail 40 server >&2 2>/dev/null || true
  fi
  compose down -v --remove-orphans --timeout 5 >/dev/null 2>&1 || true
  if [ -n "${SMOKE_EXTRA_CLEANUP:-}" ]; then "$SMOKE_EXTRA_CLEANUP" >/dev/null 2>&1 || true; fi
  exit "$status"
}
trap smoke_cleanup EXIT
trap 'exit 130' INT TERM

ensure_image() {
  if [ "${RUNBOOK_SMOKE_SKIP_BUILD:-0}" = 1 ] && docker image inspect "$RUNBOOK_IMAGE" >/dev/null 2>&1; then
    return
  fi
  log "building $RUNBOOK_IMAGE"
  docker build -q --build-arg RUNBOOK_CACHE_ID="$RUNBOOK_CACHE_ID" -t "$RUNBOOK_IMAGE" "$REPO_DIR" >/dev/null
}

# Start (or restart) the stack and wait until both healthchecks pass.
up() {
  compose up -d --no-build --wait --wait-timeout 90 "$@" >/dev/null 2>&1 ||
    fail "stack did not become healthy"
}

# Wait for the server container's own HEALTHCHECK (curl /health/live) to pass.
wait_server_healthy() {
  local deadline=$((SECONDS + ${1:-60})) id state
  id=$(compose ps -q server)
  while [ $SECONDS -lt $deadline ]; do
    state=$(docker inspect -f '{{.State.Health.Status}}' "$id" 2>/dev/null || true)
    [ "$state" = healthy ] && return 0
    sleep 0.5
  done
  fail "server not healthy (last state: ${state:-unknown})"
}

# /v1/meta through the published host port.
host_meta() {
  local port
  port=$(compose port server 8080 | sed 's/.*://')
  curl -fsS --max-time 3 "http://127.0.0.1:${port}/v1/meta"
}

# runbook-server administrative subcommand inside the server container.
server_admin() { compose exec -T server runbook-server "$@"; }

# Bootstrap the first owner. Sets RUNBOOK_TOKEN (the owner's PAT) without
# printing it.
bootstrap_owner() {
  local output
  output=$(server_admin bootstrap) || fail "bootstrap failed"
  RUNBOOK_TOKEN=$(printf '%s' "$output" | json_get pat)
  [ -n "$RUNBOOK_TOKEN" ] || fail "bootstrap printed no token"
  export RUNBOOK_TOKEN
}

# The runbook CLI inside the server container, as the bootstrapped owner.
cli() {
  compose exec -T -e RUNBOOK_TOKEN -e RUNBOOK_HOST=http://127.0.0.1:8080 server runbook "$@" 2>/dev/null
}

# json_get <key>[.<key>...] reads JSON on stdin and prints the value.
json_get() {
  python3 -I -c '
import json, sys
value = json.load(sys.stdin)
for key in sys.argv[1].split("."):
    value = value[int(key)] if isinstance(value, list) else value[key]
print(value if isinstance(value, str) else json.dumps(value))
' "$1"
}

# Publish a one-step runbook whose only step waits <duration>, so a run of it
# holds exactly one pending timer and then succeeds.
publish_wait_runbook() {
  local slug=$1 duration=$2
  printf 'apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: %s, title: Smoke %s}\nspec:\n  steps:\n    - {id: pause, wait: {for: %s}}\n' \
    "$slug" "$slug" "$duration" | compose exec -T server sh -c "cat > /tmp/$slug.yaml"
  cli push "/tmp/$slug.yaml" --project default --draft --json >/dev/null || fail "push $slug failed"
  cli publish "$slug" --project default --json >/dev/null || fail "publish $slug failed"
}

# Start a run of <slug>; prints its execution ID.
start_run() {
  cli run start "$1" --project default --json | json_get id
}

run_status() {
  cli run get "$1" --json | json_get status
}

# wait_status <exe> <status> <seconds>
wait_status() {
  local deadline=$((SECONDS + $3)) status=""
  while [ $SECONDS -lt $deadline ]; do
    status=$(run_status "$1" 2>/dev/null || true)
    [ "$status" = "$2" ] && return 0
    sleep 0.5
  done
  fail "$1 is '${status:-unreadable}', expected '$2' within $3s"
}

# One value from the bundled PostgreSQL, as its superuser (checks only).
psql_admin() {
  compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atq -v ON_ERROR_STOP=1 -c "$1"
}

# Count stored events of <type> for <exe>, straight from PostgreSQL, so
# "exactly once" is checked against the durable log, not a projection.
event_count() {
  compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atq -v ON_ERROR_STOP=1 \
    -c "SELECT count(*) FROM runbook.execution_event WHERE execution_id = '$1' AND body::jsonb #>> '{event,type}' = '$2'"
}

pass() { log "PASS ($((SECONDS - SMOKE_STARTED))s)"; }
