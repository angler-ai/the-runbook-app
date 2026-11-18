#!/usr/bin/env bash
# Restart PostgreSQL while the server keeps running: the server process
# survives, requests succeed again without manual action, a timer pending
# across the database restart still fires once, and a run started after the
# restart is drained too.
source "$(dirname "$0")/lib.sh"

ensure_image
up
bootstrap_owner
publish_wait_runbook smoke-db 6s
pending_exe=$(start_run smoke-db)
wait_status "$pending_exe" waiting 10
server=$(compose ps -q server)
started_at=$(docker inspect -f '{{.State.StartedAt}}' "$server")
log "$pending_exe waits on a 6s timer"

compose restart --timeout 10 postgres >/dev/null 2>&1
log "postgres restarted"

deadline=$((SECONDS + 30))
until cli run list --project default --json >/dev/null; do
  [ $SECONDS -lt $deadline ] || fail "API did not recover within 30s"
  sleep 0.5
done
log "API answers database-backed requests again"
[ "$(docker inspect -f '{{.State.StartedAt}}' "$server")" = "$started_at" ] || fail "server process restarted"

wait_status "$pending_exe" succeeded 30
[ "$(event_count "$pending_exe" TimerFired)" = 1 ] || fail "pending timer did not fire exactly once"
after_exe=$(start_run smoke-db)
wait_status "$after_exe" succeeded 30
[ "$(event_count "$after_exe" TimerFired)" = 1 ] || fail "post-restart run's timer did not fire once"
log "same server process; both runs completed with one timer each"
pass
