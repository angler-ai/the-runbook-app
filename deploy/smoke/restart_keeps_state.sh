#!/usr/bin/env bash
# `docker compose restart server` (SIGTERM, then a new process on the same
# container): a finished run and its events stay readable, and a run whose
# wait timer was still pending when the server stopped completes after the
# restart, with its timer fired exactly once.
source "$(dirname "$0")/lib.sh"

ensure_image
up
bootstrap_owner
publish_wait_runbook smoke-quick 1s
publish_wait_runbook smoke-pending 8s
done_exe=$(start_run smoke-quick)
wait_status "$done_exe" succeeded 20
pending_exe=$(start_run smoke-pending)
wait_status "$pending_exe" waiting 10
[ "$(event_count "$pending_exe" WaitStarted)" = 1 ] || fail "pending run never parked on its wait"
[ "$(event_count "$pending_exe" TimerFired)" = 0 ] || fail "timer fired before the restart"
before=$(cli run watch "$done_exe")
log "finished $done_exe; $pending_exe waits on an 8s timer"

compose restart --timeout 10 server >/dev/null 2>&1
wait_server_healthy 60
log "server restarted and healthy"

[ "$(run_status "$done_exe")" = succeeded ] || fail "finished run lost its status"
[ "$(cli run watch "$done_exe")" = "$before" ] || fail "finished run's events changed"
wait_status "$pending_exe" succeeded 30
[ "$(event_count "$pending_exe" TimerFired)" = 1 ] || fail "pending timer did not fire exactly once"
cli run watch "$pending_exe" | grep -q ExecutionCompleted || fail "pending run's events unreadable"
log "state kept; the pending timer fired once after the restart"
pass
