#!/usr/bin/env bash
# `docker kill` (SIGKILL, no shutdown at all) while three runs hold pending
# wait timers, then start the server again: every timer fires exactly once
# and every run completes. Nothing is lost and nothing is applied twice,
# because timers and work leases live in PostgreSQL.
source "$(dirname "$0")/lib.sh"

ensure_image
up
bootstrap_owner
publish_wait_runbook smoke-crash-a 4s
publish_wait_runbook smoke-crash-b 6s
publish_wait_runbook smoke-crash-c 7s
runs=()
for slug in smoke-crash-a smoke-crash-b smoke-crash-c; do
  runs+=("$(start_run "$slug")")
done
for exe in "${runs[@]}"; do
  wait_status "$exe" waiting 10
  [ "$(event_count "$exe" TimerFired)" = 0 ] || fail "$exe fired before the crash"
done
log "three runs parked on pending timers"

server=$(compose ps -q server)
docker kill --signal KILL "$server" >/dev/null
[ "$(docker inspect -f '{{.State.Running}}' "$server")" = false ] || fail "server survived SIGKILL"
log "server killed"
sleep 1
compose start server >/dev/null 2>&1
wait_server_healthy 60
log "server started again"

for exe in "${runs[@]}"; do
  wait_status "$exe" succeeded 30
  [ "$(event_count "$exe" TimerFired)" = 1 ] || fail "$exe timer fired $(event_count "$exe" TimerFired) times"
  [ "$(event_count "$exe" ExecutionCompleted)" = 1 ] || fail "$exe completed more than once"
done
log "all three timers fired exactly once after the crash"
pass
