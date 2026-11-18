#!/usr/bin/env bash
# docker-compose.external-db.yml: the server alone against an existing
# database given only as DATABASE_URL. A throwaway postgres:16-alpine
# container on its own project-scoped network stands in for the external
# database; smoke/external-db.network.yml attaches the server to that
# network. Nothing is published to the host. The container and network are
# labelled and removed on exit, after `compose down`.
SMOKE_COMPOSE_FILE="$(cd "$(dirname "$0")/.." && pwd)/docker-compose.external-db.yml"
SMOKE_COMPOSE_OVERRIDE="$(cd "$(dirname "$0")" && pwd)/external-db.network.yml"
source "$(dirname "$0")/lib.sh"

db_container="${SMOKE_PROJECT}-externaldb"
export SMOKE_EXTERNAL_DB_NETWORK="${SMOKE_PROJECT}-externaldb"
remove_db() {
  docker rm -f -v "$db_container"
  docker network rm "$SMOKE_EXTERNAL_DB_NETWORK"
}
SMOKE_EXTRA_CLEANUP=remove_db

ensure_image
docker network create --label runbook.smoke="$SMOKE_PROJECT" "$SMOKE_EXTERNAL_DB_NETWORK" >/dev/null
# -e NAME (no value) passes the password from this shell's environment,
# keeping it out of docker's argv.
docker run -d --name "$db_container" --label runbook.smoke="$SMOKE_PROJECT" \
  --network "$SMOKE_EXTERNAL_DB_NETWORK" --network-alias externaldb \
  -e POSTGRES_USER=external -e POSTGRES_PASSWORD -e POSTGRES_DB=external \
  postgres:16-alpine@sha256:aa6cb33cf4c19c765cefcf48d96176258800870a9a1e930ad126200f9d825139 >/dev/null
deadline=$((SECONDS + 60))
# TCP, so the entrypoint's temporary socket-only init server does not count.
until docker exec "$db_container" pg_isready -q -h 127.0.0.1 -U external -d external 2>/dev/null; do
  [ $SECONDS -lt $deadline ] || fail "external database did not start"
  sleep 0.5
done
export DATABASE_URL="postgres://external:${POSTGRES_PASSWORD}@externaldb:5432/external"
log "external database up on network $SMOKE_EXTERNAL_DB_NETWORK"

up
log "server healthy against the external DATABASE_URL"
bootstrap_owner
publish_wait_runbook smoke-external 1s
exe=$(start_run smoke-external)
wait_status "$exe" succeeded 20
tables=$(docker exec "$db_container" psql -U external -d external -Atqc \
  "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'runbook'")
[ "$tables" -gt 0 ] || fail "no runbook schema in the external database"
log "migrated the external database ($tables tables); run $exe succeeded"
pass
