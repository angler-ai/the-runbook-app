#!/usr/bin/env bash
# UPD1: deploy/update.sh end to end, as an operator runs it from a checkout.
#
# A throwaway clone of this repository's HEAD (committed work only) gets
# three release tags:
#   vN    HEAD as it is (N is the workspace version);
#   vN+1  plus an additive Postgres migration and the version bumped;
#   vN+2  plus another additive migration and the version bumped again.
# Release N is installed with docker compose from the clone (its own compose
# project, an ephemeral loopback port, generated secrets) and given data.
# Then, with the clone's deploy/update.sh (--source git unless noted):
#   1. --to vN+1 updates: /v1/meta reports N+1 and the new schema, the
#      migration's table exists, the runs and the owner's token survive, a
#      new run works, a 0600 backup was written, the checkout is at vN+1;
#   2. --to vN (a downgrade) is refused and changes nothing;
#   3. --dry-run changes nothing;
#   4. to latest (vN+2) with the health check forced to fail rolls back:
#      N+1 runs again on N+2's migrated schema (expand/contract, with
#      RUNBOOK_ALLOW_NEWER_SCHEMA), its data intact, a new run works;
#      The compose override that sets it is in the backup directory;
#   5. --rollback --restore-backup restores the pre-N+2 backup: the schema
#      is N+1's again and what was written after the backup is gone;
#   6. --source image (the default; here a local repository standing in for
#      ghcr.io, RUNBOOK_UPDATE_TEST_NO_PULL=1 since nothing is published)
#      with RUNBOOK_UPDATE_PIN=X.Y in the env file installs that line's
#      newest image (N+2): refused while its provenance cannot be verified,
#      installed with --insecure-skip-verify; --keep-backups prunes dumps;
#   7. an external database (docker-compose.external-db.yml, --skip-backup):
#      a failed update rolls back onto the migrated schema (the override
#      comes from the recorded schemas, with no bundled database to ask).
# And the data-safety review's cases: a truncated backup stops the update
# (3b); a truncated recorded backup is never restored and an old one needs
# its ID (4b, 4c); the restore keeps the replaced database and a dump of it
# (5); N+3 drops a column with `contract(unused-since N+2)`, so its rollback
# floor is N+2 and updating N+1 -> N+3 needs --restore-backup (5b).
#
# Images: runbook-upd1-smoke:* and runbook-upd1-smoke-release:* only, all removed on exit with the clone,
# the env file and the stack (docker compose down -v). Builds three images,
# so it takes a while (minutes per build; the first is the longest). Not
# part of run_all.sh.
#
#   deploy/smoke/update.sh
export RUNBOOK_SMOKE_IMAGE=runbook-upd1-smoke:local
# shellcheck source=deploy/smoke/lib.sh
source "$(dirname "$0")/lib.sh"

work=$(mktemp -d "${TMPDIR:-/tmp}/runbook-upd1-smoke.XXXXXX")
clone=$work/repo
backups=$work/backups
envfile=$work/env
images=$work/images
SMOKE_REPO=runbook-upd1-smoke
RELEASE_REPO=runbook-upd1-smoke-release
EXT_REPO=runbook-upd1-smoke-ext
EXT_PROJECT="${SMOKE_PROJECT}-ext"
ext_db="${SMOKE_PROJECT}-externaldb"
remember_images() {
  for repo in "$SMOKE_REPO" "$RELEASE_REPO" "$EXT_REPO"; do
    docker image ls -q --no-trunc "$repo" >>"$images" 2>/dev/null || true
  done
}
update_cleanup() {
  remember_images
  if [ -f "$work/env-ext" ]; then
    docker compose -p "$EXT_PROJECT" -f "$clone/deploy/docker-compose.external-db.yml" \
      -f "$clone/deploy/smoke/external-db.network.yml" --env-file "$work/env-ext" down -v --remove-orphans --timeout 5 >/dev/null 2>&1 || true
  fi
  docker rm -f -v "$ext_db" >/dev/null 2>&1 || true
  docker network rm "$ext_db" >/dev/null 2>&1 || true
  for repo in "$SMOKE_REPO" "$RELEASE_REPO" "$EXT_REPO"; do
    docker image ls --format '{{.Repository}}:{{.Tag}}' "$repo" | xargs -r docker image rm -f >/dev/null 2>&1 || true
  done
  sort -u "$images" | xargs -r docker image rm -f >/dev/null 2>&1 || true
  rm -rf "$work"
}
SMOKE_EXTRA_CLEANUP=update_cleanup
SMOKE_COMPOSE_FILE=$clone/deploy/docker-compose.yml

bump() { awk -F. '{printf "%d.%d.%d", $1, $2, $3 + 1}' <<<"$1"; }
N=$(awk '/^\[workspace.package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{gsub(/[" ]/,""); sub(/^version=/,""); print; exit}' "$REPO_DIR/Cargo.toml")
N1=$(bump "$N")
N2=$(bump "$N1")
SCHEMA=$(find "$REPO_DIR/crates/runbook-store-postgres/migrations" -name '[0-9][0-9][0-9][0-9]_*.sql' | wc -l | tr -d ' ')
git_c() { git -C "$clone" -c user.name="Runbook smoke" -c user.email=smoke@example.invalid "$@"; }

# One release: an additive migration, the version bumped, committed, tagged.
release() {
  local version=$1 number=$2 sql=$3 name
  name=$(printf '%04d_upd1_smoke.sql' "$number")
  printf -- '-- UPD1 smoke test only: an additive migration.\n%s\n' "$sql" \
    >"$clone/crates/runbook-store-postgres/migrations/$name"
  python3 -I - "$clone" "$name" "$version" <<'PY'
import re, sys
root, name, version = sys.argv[1:4]
path = f"{root}/crates/runbook-store-postgres/src/migrate.rs"
src = open(path).read()
last = [m for m in re.finditer(r'    include_str!\("\.\./migrations/[^"]+"\),\n', src)][-1]
src = src[: last.end()] + f'    include_str!("../migrations/{name}"),\n' + src[last.end():]
open(path, "w").write(src)
path = f"{root}/Cargo.toml"
src = open(path).read()
head, rest = src.split("[workspace.package]", 1)
rest = re.sub(r'(?m)^version = "[^"]+"', f'version = "{version}"', rest, count=1)
open(path, "w").write(head + "[workspace.package]" + rest)
path = f"{root}/Cargo.lock"
src = open(path).read()
src = re.sub(r'(name = "runbook-[^"]+"\nversion = )"[^"]+"', lambda m: f'{m.group(1)}"{version}"', src)
open(path, "w").write(src)
PY
  git_c add -A
  git_c commit --quiet -m "release $version (smoke)"
  git_c tag "v$version"
}

log "release N=$N, N+1=$N1, N+2=$N2 in a throwaway clone (schema $SCHEMA)"
git clone --quiet --no-local "$REPO_DIR" "$clone"
git -C "$clone" tag -l | xargs -r git -C "$clone" tag -d >/dev/null
git -C "$clone" remote remove origin
git_c tag "v$N"
release "$N1" $((SCHEMA + 1)) 'CREATE TABLE runbook.upd1_smoke (id TEXT PRIMARY KEY);'
release "$N2" $((SCHEMA + 2)) 'ALTER TABLE runbook.upd1_smoke ADD COLUMN note TEXT;'
git -C "$clone" checkout --quiet --detach "v$N"

# The operator's env file: the values lib.sh generated (never printed).
(
  umask 077
  for name in POSTGRES_USER POSTGRES_DB POSTGRES_PASSWORD RUNBOOK_DB_USER RUNBOOK_DB_PASSWORD \
    RUNBOOK_WEBHOOK_SECRET_KEY RUNBOOK_ORIGIN RUNBOOK_BIND_ADDRESS RUNBOOK_HOST_PORT \
    RUNBOOK_USERCONTENT_HOST_PORT RUNBOOK_IMAGE; do
    printf '%s=%s\n' "$name" "${!name}"
  done >"$envfile"
)
# deploy/update.sh as the operator runs it: settings from the env file only.
update() {
  env -u RUNBOOK_IMAGE "$clone/deploy/update.sh" --project "$SMOKE_PROJECT" --env-file "$envfile" \
    --compose-file "$clone/deploy/docker-compose.yml" --backup-dir "$backups" --no-fetch "$@"
}
meta_field() { host_meta | json_get "$1"; }
# RUNBOOK_SMOKE_VERBOSE=1 prints each update's own output.
show() { if [ -n "${RUNBOOK_SMOKE_VERBOSE:-}" ]; then printf '%s\n' "$out" | sed 's/^/      | /'; fi; }
db_schema() { psql_admin "SELECT max(version) FROM runbook.schema_migration"; }

log "installing release $N (docker compose build from the clone)"
docker compose -p "$SMOKE_PROJECT" -f "$SMOKE_COMPOSE_FILE" --env-file "$envfile" build --quiet server >/dev/null ||
  fail "building release $N failed"
remember_images
# shellcheck disable=SC2119 # lib.sh: no services means all of them
up
bootstrap_owner
[ "$(meta_field version)" = "$N" ] || fail "/v1/meta does not report $N"
[ "$(meta_field schema.postgres)" = "$SCHEMA" ] || fail "/v1/meta does not report schema $SCHEMA"
publish_wait_runbook smoke-upd1 1s
exe=$(start_run smoke-upd1)
wait_status "$exe" succeeded 20
events_before=$(cli run watch "$exe")
log "release $N up with a finished run $exe"

log "1. update $N -> $N1"
out=$(update --source git --to "v$N1" --yes 2>&1) || fail "update to $N1 failed: $out"
show
remember_images
[ "$(meta_field version)" = "$N1" ] || fail "/v1/meta does not report $N1 after the update"
[ "$(meta_field schema.postgres)" = "$((SCHEMA + 1))" ] || fail "/v1/meta does not report schema $((SCHEMA + 1))"
[ "$(psql_admin "SELECT to_regclass('runbook.upd1_smoke') IS NOT NULL")" = t ] || fail "the N+1 migration did not run"
server_admin doctor | grep -q "schema: current ($((SCHEMA + 1)))" || fail "doctor does not report schema $((SCHEMA + 1))"
[ "$(run_status "$exe")" = succeeded ] || fail "the run was lost in the update"
[ "$(cli run watch "$exe")" = "$events_before" ] || fail "the run's events changed in the update"
next=$(start_run smoke-upd1)
wait_status "$next" succeeded 20
[ "$(git -C "$clone" describe --tags --exact-match)" = "v$N1" ] || fail "the checkout is not at v$N1"
dump=$(find "$backups" -name "runbook-$N-*.dump" -perm 600 | head -n1)
[ -n "$dump" ] || fail "no 0600 backup of release $N in $backups"
grep -q "Updated $N -> $N1" <<<"$out" || fail "no success line: $out"
log "updated: meta, schema, data, token, new run, backup ($(basename "$dump")) all good"

log "2. a downgrade is refused"
status=0
out=$(update --source git --to "v$N" --yes 2>&1) || status=$?
show
[ "$status" = 1 ] || fail "downgrade exited $status: $out"
grep -q 'downgrades are refused' <<<"$out" || fail "downgrade not refused: $out"
[ "$(meta_field version)" = "$N1" ] || fail "a refused downgrade changed the server"
[ "$(git -C "$clone" describe --tags --exact-match)" = "v$N1" ] || fail "a refused downgrade moved the checkout"

log "3. --dry-run changes nothing"
out=$(update --source git --dry-run 2>&1) || fail "dry run failed: $out"
show
grep -q "target: v$N2" <<<"$out" || fail "dry run did not pick v$N2 as latest: $out"
grep -q 'Dry run: nothing changed' <<<"$out" || fail "no dry-run line: $out"
[ "$(meta_field version)" = "$N1" ] || fail "the dry run changed the server"
[ "$(db_schema)" = "$((SCHEMA + 1))" ] || fail "the dry run changed the schema"

log "3b. a truncated backup stops the update before anything changes"
status=0
out=$(RUNBOOK_UPDATE_TEST_TRUNCATE_BACKUP=1 update --source git --yes 2>&1) || status=$?
show
[ "$status" = 1 ] || fail "an update with a truncated backup exited $status: $out"
grep -q 'does not read back in full' <<<"$out" || fail "the truncated dump was not caught: $out"
[ "$(meta_field version)" = "$N1" ] || fail "a refused update changed the server"
[ "$(db_schema)" = "$((SCHEMA + 1))" ] || fail "a refused update migrated"
[ "$(git -C "$clone" describe --tags --exact-match)" = "v$N1" ] || fail "a refused update moved the checkout"

log "4. update to latest ($N2) with a failing health check rolls back"
status=0
out=$(RUNBOOK_UPDATE_TEST_FAIL_HEALTH=1 update --source git --yes 2>&1) || status=$?
show
remember_images
[ "$status" = 3 ] || fail "a failed update exited $status, not 3: $out"
grep -q "Rolled back to $N1" <<<"$out" || fail "no rollback line: $out"
[ "$(meta_field version)" = "$N1" ] || fail "release $N1 is not running after the rollback"
[ "$(db_schema)" = "$((SCHEMA + 2))" ] || fail "the N+2 migration did not run before the failure"
[ "$(meta_field schema.postgres)" = "$((SCHEMA + 2))" ] || fail "meta does not report the database's schema"
server_admin doctor | grep -q "schema: ahead (database $((SCHEMA + 2)), code $((SCHEMA + 1))" ||
  fail "doctor does not report the schema ahead"
[ "$(run_status "$exe")" = succeeded ] || fail "the run was lost in the rollback"
after_rollback=$(start_run smoke-upd1)
wait_status "$after_rollback" succeeded 20
[ "$(git -C "$clone" describe --tags --exact-match)" = "v$N1" ] || fail "the rollback did not restore the checkout"
[ -f "$backups/allow-newer-schema.yml" ] || fail "no compose override for the newer schema"
log "rolled back: $N1 runs on schema $((SCHEMA + 2)) with its data; new run $after_rollback succeeded"

backup=$(sed -n 's/^BACKUP_FILE=//p' "$backups/last-update")
backup_id=$(basename "$backup" .dump)
log "4b. a truncated recorded backup is never restored"
cp -p "$backup" "$work/good.dump"
half=$(($(wc -c <"$work/good.dump") / 2))
head -c "$half" "$work/good.dump" >"$backup"
status=0
out=$(update --rollback --restore-backup --yes 2>&1) || status=$?
show
[ "$status" != 0 ] || fail "a truncated backup was restored: $out"
grep -q 'does not read back in full' <<<"$out" || fail "the truncated backup was not caught: $out"
[ "$(db_schema)" = "$((SCHEMA + 2))" ] || fail "the database changed"
[ "$(meta_field version)" = "$N1" ] || fail "the server changed"
cp -p "$work/good.dump" "$backup"

log "4c. a backup older than a day (by the time in its ID) needs its ID"
old="$backups/runbook-$N1-20260101T000000Z.dump"
mv "$backup" "$old"
sed -i.bak "s|^BACKUP_FILE=.*|BACKUP_FILE=$old|" "$backups/last-update" && rm -f "$backups/last-update.bak"
backup=$old
backup_id=$(basename "$backup" .dump)
status=0
out=$(update --rollback --restore-backup --yes 2>&1) || status=$?
show
[ "$status" != 0 ] || fail "an old backup was restored without its ID: $out"
grep -q "confirm with --confirm-backup $backup_id" <<<"$out" || fail "no confirmation asked: $out"
[ "$(db_schema)" = "$((SCHEMA + 2))" ] || fail "the database changed"

# A database setting the restore must carry over to the restored database.
psql_admin "ALTER DATABASE runbook SET work_mem TO '7MB'" >/dev/null
log "5. --rollback --restore-backup restores the backup taken before $N2"
out=$(update --rollback --restore-backup --yes --confirm-backup "$backup_id" 2>&1) || fail "rollback with restore failed: $out"
show
grep -qE "taken [0-9]+h ago" <<<"$out" || fail "the backup's age was not shown: $out"
ls "$backups"/runbook-"$N2"-pre-restore-*.dump >/dev/null 2>&1 || fail "no dump of the database as it was before the restore"
[ "$(psql_admin "SELECT count(*) FROM pg_database WHERE datname LIKE 'runbook_replaced_%'")" = 1 ] ||
  fail "the replaced database was not kept"
psql_admin "SELECT array_to_string(setconfig, ',') FROM pg_db_role_setting r JOIN pg_database d ON d.oid = r.setdatabase WHERE d.datname = 'runbook' AND r.setrole = 0" |
  grep -q 'work_mem=7MB' || fail "the restored database lost its ALTER DATABASE settings"
[ "$(db_schema)" = "$((SCHEMA + 1))" ] || fail "the restored database is not at schema $((SCHEMA + 1))"
[ "$(meta_field version)" = "$N1" ] || fail "release $N1 is not running after the restore"
server_admin doctor | grep -q "schema: current ($((SCHEMA + 1)))" || fail "doctor after the restore"
[ "$(run_status "$exe")" = succeeded ] || fail "the restore lost a run from before the backup"
cli run get "$after_rollback" --json >/dev/null 2>&1 && fail "a run written after the backup survived the restore"
final=$(start_run smoke-upd1)
wait_status "$final" succeeded 20
[ ! -e "$backups/allow-newer-schema.yml" ] || fail "the newer-schema override outlived the restore"
log "restored: schema $((SCHEMA + 1)), earlier runs kept, the later run gone, new runs work"

N3=$(bump "$N2")
log "5b. $N3 drops a column (rollback floor $N2): $N1 -> $N3 needs --restore-backup"
here=$(git -C "$clone" rev-parse HEAD)
git -C "$clone" checkout --quiet --detach "v$N2"
release "$N3" $((SCHEMA + 3)) "-- contract(unused-since $N2): $N2 stopped reading upd1_smoke.note
ALTER TABLE runbook.upd1_smoke DROP COLUMN note;"
git -C "$clone" checkout --quiet --detach "$here"
status=0
out=$(update --source git --to "v$N3" --yes 2>&1) || status=$?
show
remember_images
[ "$status" = 1 ] || fail "an update below the rollback floor exited $status: $out"
grep -q "$N1 is below $N3's rollback floor $N2" <<<"$out" || fail "the floor was not enforced: $out"
[ "$(meta_field version)" = "$N1" ] || fail "a refused update changed the server"
[ "$(db_schema)" = "$((SCHEMA + 1))" ] || fail "a refused update migrated"
[ "$(git -C "$clone" rev-parse HEAD)" = "$here" ] || fail "a refused update moved the checkout"

log "6. the image source, pinned to $(cut -d. -f1-2 <<<"$N2") in the env file"
line=$(cut -d. -f1-2 <<<"$N2")
for tag in "$N2" "$line" latest; do docker tag "$SMOKE_REPO:$N2" "$RELEASE_REPO:$tag"; done
printf 'RUNBOOK_UPDATE_PIN=%s\n' "$line" >>"$envfile"
status=0
out=$(RUNBOOK_UPDATE_TEST_NO_PULL=1 update --source image --image-repo "$RELEASE_REPO" --yes 2>&1) || status=$?
show
[ "$status" = 1 ] || fail "an unverifiable image was installed: $out"
grep -q 'cannot verify' <<<"$out" || fail "no verification refusal: $out"
[ "$(meta_field version)" = "$N1" ] || fail "a refused image update changed the server"
out=$(RUNBOOK_UPDATE_TEST_NO_PULL=1 update --source image --image-repo "$RELEASE_REPO" --insecure-skip-verify \
  --keep-backups 1 --yes 2>&1) || fail "the image update failed: $out"
show
grep -q 'is NOT verified' <<<"$out" || fail "no warning for an unverified image: $out"
[ "$(find "$backups" -name 'runbook-*.dump' | wc -l | tr -d ' ')" = 1 ] || fail "--keep-backups 1 left $(find "$backups" -name 'runbook-*.dump' | wc -l) dumps"
grep -q "target: $line (image, pinned by RUNBOOK_UPDATE_PIN) -> image $RELEASE_REPO:$line" <<<"$out" || fail "the pin was not used: $out"
[ "$(meta_field version)" = "$N2" ] || fail "/v1/meta does not report $N2 after the image update"
[ "$(meta_field schema.postgres)" = "$((SCHEMA + 2))" ] || fail "the image update did not migrate"
[ "$(run_status "$exe")" = succeeded ] || fail "the image update lost a run"
log "image update: $N1 -> $N2 from $RELEASE_REPO:$line"

log "7. an external database: a failed update rolls back onto the migrated schema"
docker network create --label runbook.smoke="$SMOKE_PROJECT" "$ext_db" >/dev/null
docker run -d --name "$ext_db" --label runbook.smoke="$SMOKE_PROJECT" --network "$ext_db" --network-alias externaldb \
  -e POSTGRES_USER=external -e POSTGRES_PASSWORD -e POSTGRES_DB=external \
  postgres:16-alpine@sha256:aa6cb33cf4c19c765cefcf48d96176258800870a9a1e930ad126200f9d825139 >/dev/null
deadline=$((SECONDS + 60))
until docker exec "$ext_db" pg_isready -q -h 127.0.0.1 -U external -d external 2>/dev/null; do
  [ $SECONDS -lt $deadline ] || fail "external database did not start"
  sleep 0.5
done
(
  umask 077
  printf '%s\n' "DATABASE_URL=postgres://external:${POSTGRES_PASSWORD}@externaldb:5432/external" \
    "RUNBOOK_WEBHOOK_SECRET_KEY=$RUNBOOK_WEBHOOK_SECRET_KEY" "RUNBOOK_ORIGIN=$RUNBOOK_ORIGIN" \
    "RUNBOOK_BIND_ADDRESS=127.0.0.1" "RUNBOOK_HOST_PORT=0" "RUNBOOK_USERCONTENT_HOST_PORT=0" \
    "RUNBOOK_IMAGE=$EXT_REPO:local" "SMOKE_EXTERNAL_DB_NETWORK=$ext_db" >"$work/env-ext"
)
docker tag "$SMOKE_REPO:$N1" "$EXT_REPO:local"
ext() {
  env -u RUNBOOK_IMAGE -u DATABASE_URL docker compose -p "$EXT_PROJECT" -f "$clone/deploy/docker-compose.external-db.yml" \
    -f "$clone/deploy/smoke/external-db.network.yml" --env-file "$work/env-ext" "$@"
}
ext up -d --no-build --wait --wait-timeout 90 >/dev/null 2>&1 || fail "the external-database stack did not start"
ext_meta() { ext exec -T server curl -fsS --max-time 5 http://127.0.0.1:8080/v1/meta | json_get "$1"; }
[ "$(ext_meta version)" = "$N1" ] || fail "the external stack is not $N1"
status=0
out=$(RUNBOOK_UPDATE_TEST_NO_PULL=1 RUNBOOK_UPDATE_TEST_FAIL_HEALTH=1 env -u RUNBOOK_IMAGE -u DATABASE_URL "$clone/deploy/update.sh" \
  --project "$EXT_PROJECT" --compose-file "$clone/deploy/docker-compose.external-db.yml" \
  --compose-file "$clone/deploy/smoke/external-db.network.yml" --env-file "$work/env-ext" --backup-dir "$work/backups-ext" \
  --skip-backup --source image --image-repo "$RELEASE_REPO" --to "$N2" --insecure-skip-verify --yes 2>&1) || status=$?
show
[ "$status" = 3 ] || fail "the external-database rollback exited $status: $out"
[ "$(ext_meta version)" = "$N1" ] || fail "$N1 is not running against the external database"
[ "$(ext_meta schema.postgres)" = "$((SCHEMA + 2))" ] || fail "the external database was not migrated by $N2"
[ -f "$work/backups-ext/allow-newer-schema.yml" ] || fail "no newer-schema override for the external database"
log "external database: $N1 runs again on schema $((SCHEMA + 2))"
pass
