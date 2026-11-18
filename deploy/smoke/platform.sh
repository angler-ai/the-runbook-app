#!/usr/bin/env bash
# HOST1: the image as a container platform runs it (Cloud Run, Fly, Render,
# Kubernetes): two replicas start together on one empty database; each
# follows PORT (no RUNBOOK_LISTEN_ADDR), serves user content on the same port
# by host, logs JSON on stdout and answers /health/live and /health/ready; a
# run with a timer completes; `docker stop` (SIGTERM) ends a replica with
# exit status 0 well inside Cloud Run's 10 s.
SMOKE_COMPOSE_OVERRIDE="$(cd "$(dirname "$0")" && pwd)/platform.override.yml"
source "$(dirname "$0")/lib.sh"
RUNBOOK_UPLOAD_SIGNING_KEY=$(openssl rand -base64 32)
export RUNBOOK_UPLOAD_SIGNING_KEY

ensure_image
up
log "both replicas healthy (the image's HEALTHCHECK follows PORT)"

port_of() { compose port "$1" 9000 | sed 's/.*://'; }
for service in server replica; do
  port=$(port_of "$service")
  [ "$(curl -fsS --max-time 3 "http://127.0.0.1:$port/health/ready")" = '{"status":"ready"}' ] ||
    fail "$service is not ready on PORT 9000"
  [ "$(curl -fsS --max-time 3 "http://127.0.0.1:$port/health/live")" = '{"status":"live"}' ] ||
    fail "$service is not live"
  # One port, two origins: the user-content host answers only /u/*.
  headers=$(curl -sS -o /dev/null -D - --max-time 3 -H 'Host: files.example.test' "http://127.0.0.1:$port/v1/meta")
  printf '%s' "$headers" | head -1 | grep -q ' 404' || fail "$service serves the API on the user-content host"
  printf '%s' "$headers" | grep -qi '^content-security-policy:.*sandbox' || fail "$service user-content host lacks its sandbox"
  curl -fsS --max-time 3 "http://127.0.0.1:$port/v1/meta" | grep -q '"edition":"self_hosted"' ||
    fail "$service does not serve the API on the app host"
  logs=$(compose logs --no-log-prefix "$service" 2>/dev/null)
  printf '%s\n' "$logs" | grep -q '"message":"listening on 0.0.0.0:9000 (user content on the same port, by host)"' ||
    fail "$service did not log its single-port listener as JSON"
  printf '%s\n' "$logs" | python3 -I -c '
import json, sys
for line in sys.stdin:
    if line.strip():
        value = json.loads(line)
        assert value["severity"] in ("INFO", "WARNING", "ERROR"), line
' || fail "$service wrote a log line that is not JSON"
done
log "PORT, single-port user content, probes and JSON logs on both replicas"

bootstrap_owner
cli9000() {
  compose exec -T -e RUNBOOK_TOKEN -e RUNBOOK_HOST=http://127.0.0.1:9000 "$1" runbook "${@:2}" 2>/dev/null
}
printf 'apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: smoke-platform, title: Smoke platform}\nspec:\n  steps:\n    - {id: pause, wait: {for: 2s}}\n' |
  compose exec -T server sh -c 'cat > /tmp/smoke-platform.yaml'
cli9000 server push /tmp/smoke-platform.yaml --project default --draft --json >/dev/null || fail "push failed"
cli9000 server publish smoke-platform --project default --json >/dev/null || fail "publish failed"
exe=$(cli9000 replica run start smoke-platform --project default --json | json_get id)
[[ $exe == exe_* ]] || fail "run start on the replica returned no execution ID"
deadline=$((SECONDS + 30))
until [ "$(cli9000 server run get "$exe" --json | json_get status)" = succeeded ]; do
  [ $SECONDS -lt $deadline ] || fail "$exe did not succeed"
  sleep 0.5
done
[ "$(event_count "$exe" TimerFired)" = 1 ] || fail "timer did not fire exactly once"
log "published on one replica, started on the other; the timer fired once"

t0=$(python3 -I -c 'import time; print(time.time())')
compose stop --timeout 10 replica >/dev/null 2>&1
took=$(python3 -I -c "import time; print(round(time.time() - $t0, 1))")
code=$(docker inspect -f '{{.State.ExitCode}}' "$(compose ps -a -q replica)")
[ "$code" = 0 ] || fail "replica exited $code on SIGTERM"
python3 -I -c "import sys; sys.exit(0 if $took < 9 else 1)" || fail "replica took ${took}s to stop"
compose logs --no-log-prefix replica 2>/dev/null | grep -q '"message":"stopped in' || fail "replica did not log its shutdown"
log "docker stop: exit 0 in ${took}s"
pass
