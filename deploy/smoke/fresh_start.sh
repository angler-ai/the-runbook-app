#!/usr/bin/env bash
# Clean start: build, up, both healthchecks pass, the image runs as non-root
# and carries no configuration secrets, bootstrap the first owner inside the
# container, publish a tiny runbook with the CLI, start a run and watch its
# one timer complete it.
source "$(dirname "$0")/lib.sh"

ensure_image
up
log "stack healthy"

uid=$(compose exec -T server id -u)
[ "$uid" != 0 ] || fail "server runs as root"
image_env=$(docker image inspect -f '{{json .Config.Env}}' "$RUNBOOK_IMAGE")
for name in DATABASE_URL RUNBOOK_WEBHOOK_SECRET_KEY POSTGRES_PASSWORD RUNBOOK_TOKEN; do
  case $image_env in *"$name"*) fail "image bakes in $name" ;; esac
done
health_output=$(docker inspect -f '{{range .State.Health.Log}}{{.Output}}{{end}}' "$(compose ps -q server)")
[ -z "$health_output" ] || fail "healthcheck printed output"
host_meta | grep -q '"edition":"self_hosted"' || fail "/v1/meta not served on the host port"
log "non-root (uid $uid), no baked secrets, silent healthcheck, /v1/meta on host port"

# F81: both containers are hardened: read-only root, no-new-privileges,
# memory and process limits; the server drops every capability.
for service in server postgres; do
  read -r ro opts mem pids caps <<<"$(docker inspect -f '{{.HostConfig.ReadonlyRootfs}} {{json .HostConfig.SecurityOpt}} {{.HostConfig.Memory}} {{.HostConfig.PidsLimit}} {{json .HostConfig.CapDrop}}' "$(compose ps -q "$service")")"
  [ "$ro" = true ] && [ "$opts" = '["no-new-privileges:true"]' ] && [ "${mem:-0}" -gt 0 ] &&
    [ "${pids:-0}" != "<nil>" ] && [ "${pids:-0}" -gt 0 ] && [ "$caps" = '["ALL"]' ] ||
    fail "$service is not hardened: $ro $opts $mem $pids $caps"
done
[ "$(docker inspect -f '{{json .HostConfig.CapAdd}}' "$(compose ps -q server)")" = null ] || fail "server adds capabilities"
log "both containers: read-only root, no-new-privileges, memory and pid limits, capabilities dropped"

# F82: the server connects as an unprivileged role that owns only the
# runbook schema, never as the bundled database's superuser.
# The schema and the migration table the server created on start are owned
# by the role it connected as.
owners=$(psql_admin "SELECT n.nspowner::regrole || ',' || t.tableowner FROM pg_namespace n, pg_tables t WHERE n.nspname = 'runbook' AND t.schemaname = 'runbook' AND t.tablename = 'schema_migration'")
[ "$owners" = "${RUNBOOK_DB_USER},${RUNBOOK_DB_USER}" ] || fail "runbook schema and tables owned by '$owners', expected $RUNBOOK_DB_USER"
attrs=$(psql_admin "SELECT rolsuper::text || rolcreatedb::text || rolcreaterole::text || rolbypassrls::text FROM pg_roles WHERE rolname = '$RUNBOOK_DB_USER'")
[ "$attrs" = falsefalsefalsefalse ] || fail "$RUNBOOK_DB_USER is privileged ($attrs)"
log "server connects as unprivileged $RUNBOOK_DB_USER, owner of the runbook schema"

bootstrap_owner
server_admin bootstrap >/dev/null 2>&1 && fail "second bootstrap succeeded"
server_admin doctor | grep -q 'schema: current' || fail "doctor does not report a current schema"
log "bootstrapped the first owner once; doctor reports a current schema"

publish_wait_runbook smoke-fresh 2s
cli get smoke-fresh --project default --json | json_get runbook_id | grep -q '^rb_' || fail "runbook not readable"
exe=$(start_run smoke-fresh)
[[ $exe == exe_* ]] || fail "run start returned no execution ID"
cli run list --project default --json | grep -q "\"$exe\"" || fail "run not listed"
log "published smoke-fresh and started $exe"

wait_status "$exe" succeeded 30
[ "$(event_count "$exe" TimerFired)" = 1 ] || fail "timer did not fire exactly once"
cli run watch "$exe" | grep -q 'ExecutionCompleted' || fail "events not readable"
log "run succeeded after its timer fired once; events readable"
pass
