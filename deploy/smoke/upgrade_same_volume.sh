#!/usr/bin/env bash
# Upgrade path: stop and remove the server container, then start a fresh one
# (same image tag, same named volume). Its startup migrations are a no-op
# (same 22 rows, same checksums), the instance stays bootstrapped, and the
# directory, runbooks and runs persist and keep working.
source "$(dirname "$0")/lib.sh"

ensure_image
up
bootstrap_owner
publish_wait_runbook smoke-upgrade 1s
exe=$(start_run smoke-upgrade)
wait_status "$exe" succeeded 20
migrations() {
  compose exec -T postgres psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atq \
    -c "SELECT string_agg(version || ':' || checksum, ',' ORDER BY version) FROM runbook.schema_migration"
}
before=$(migrations)
events_before=$(cli run watch "$exe")
old=$(compose ps -q server)
log "baseline: $(printf '%s' "$before" | awk -F, '{print NF}') migrations, $exe succeeded"

compose stop --timeout 10 server >/dev/null 2>&1
compose rm -f server >/dev/null 2>&1
compose up -d --no-build --no-deps --force-recreate --wait --wait-timeout 60 server >/dev/null 2>&1 ||
  fail "fresh server container did not become healthy"
new=$(compose ps -q server)
[ "$new" != "$old" ] || fail "server container was not replaced"
log "fresh server container on the same volume is healthy"

[ "$(migrations)" = "$before" ] || fail "migration history changed on restart"
server_admin doctor | grep -q 'schema: current' || fail "doctor does not report a current schema"
server_admin bootstrap >/dev/null 2>&1 && fail "instance forgot it was bootstrapped"
[ "$(run_status "$exe")" = succeeded ] || fail "run lost after upgrade"
[ "$(cli run watch "$exe")" = "$events_before" ] || fail "run events changed after upgrade"
next=$(start_run smoke-upgrade)
wait_status "$next" succeeded 20
log "migrations idempotent; owner token, runbook and runs persist; new run succeeds"
pass
