#!/bin/sh
# Kubernetes upgrade smoke test for the updating rules in CONTRIBUTING.md
# ("Migrations: expand/contract") and docs/install.md ("Updating").
#
# It builds two real images in a disposable kind cluster:
#   FROM_REF (default: HEAD^) serves and bootstraps one organization;
#   the TO_REF image migrates in a Job, then rolls with migration on
#   start disabled; the FROM image then rolls back on the newer schema
#   only with RUNBOOK_ALLOW_NEWER_SCHEMA=true; finally TO_REF rolls again.
# The organization data is still there after every stage.
#
# This is deliberately not part of the normal check: building two images
# takes minutes. Run it for release/migration changes:
#   scripts/dev/k8s-upgrade-smoke.sh
#   FROM_REF=v0.1.0 TO_REF=HEAD scripts/dev/k8s-upgrade-smoke.sh
# Set KEEP_UPGRADE_CLUSTER=1 to keep the cluster and build directories.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
FROM_REF=${FROM_REF:-HEAD^}
TO_REF=${TO_REF:-HEAD}
CLUSTER=${KIND_CLUSTER:-runbook-upgrade-$$}
NAMESPACE=runbook-upgrade
FROM_IMAGE="runbook-upgrade:from-$$"
TO_IMAGE="runbook-upgrade:to-$$"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/runbook-k8s-upgrade.XXXXXX")
PF_PID=

for command in git docker kind kubectl curl; do
  command -v "$command" >/dev/null 2>&1 || { echo "error: $command is required" >&2; exit 127; }
done

cleanup() {
  status=$?
  if [ -n "${PF_PID:-}" ]; then kill "$PF_PID" 2>/dev/null || true; fi
  if [ "${KEEP_UPGRADE_CLUSTER:-}" != 1 ]; then
    kind delete cluster --name "$CLUSTER" >/dev/null 2>&1 || true
    rm -rf "$WORK"
  else
    echo "Keeping cluster $CLUSTER and files in $WORK" >&2
  fi
  exit "$status"
}
trap cleanup EXIT INT TERM

say() { printf '\n==> %s\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }

export_ref() {
  ref=$1
  dest=$2
  mkdir -p "$dest"
  git -C "$ROOT" archive --format=tar "$ref" | tar -xf - -C "$dest"
}

build_image() {
  ref=$1
  image=$2
  dest="$WORK/src-$3"
  say "Building $ref as $image"
  export_ref "$ref" "$dest"
  # Images before the Dockerfile learned to copy the embedded CLI skill
  # need that one build-context line to compile at all.
  if [ -d "$dest/skills" ] && ! grep -q '^COPY skills ./skills$' "$dest/Dockerfile"; then
    perl -0pi -e 's/^COPY vendor \.\/vendor$/COPY vendor .\/vendor\nCOPY skills .\/skills/m' "$dest/Dockerfile"
    perl -0pi -e 's#^skills/$#skills/*\n!skills/therunbook\n!skills/therunbook/**#m' "$dest/.dockerignore"
  fi
  docker build --quiet --build-arg "RUNBOOK_CACHE_ID=upgrade-$3" --tag "$image" "$dest" >"$WORK/build-$3.log" 2>&1 || {
    tail -100 "$WORK/build-$3.log" >&2
    die "docker build failed for $ref"
  }
}

render() {
  template=$1
  image=$2
  migrate_on_start=$3
  allow_newer=$4
  output=$5
  sed \
    -e "s#__IMAGE__#$image#g" \
    -e "s#__MIGRATE_ON_START__#$migrate_on_start#g" \
    -e "s#__ALLOW_NEWER_SCHEMA__#$allow_newer#g" \
    "$template" >"$output"
}

apply_app() {
  image=$1
  migrate_on_start=$2
  allow_newer=$3
  render "$ROOT/scripts/dev/k8s-upgrade/base.yaml" "$image" "$migrate_on_start" "$allow_newer" "$WORK/app.yaml"
  kubectl --context "kind-$CLUSTER" apply -f "$WORK/app.yaml" >/dev/null
}

wait_app() {
  kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" rollout status deployment/runbook --timeout=300s >/dev/null
  kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" wait --for=condition=Available deployment/runbook --timeout=60s >/dev/null
}

run_migration_job() {
  image=$1
  allow_newer=$2
  kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" delete job runbook-migrate --ignore-not-found --wait=true >/dev/null
  render "$ROOT/scripts/dev/k8s-upgrade/migrate-job.yaml" "$image" false "$allow_newer" "$WORK/job.yaml"
  kubectl --context "kind-$CLUSTER" apply -f "$WORK/job.yaml" >/dev/null
  if ! kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" wait --for=condition=complete job/runbook-migrate --timeout=300s >/dev/null; then
    kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" logs job/runbook-migrate >&2 || true
    die "migration Job failed for $image"
  fi
}

supports_migrate() {
  output=$(docker run --rm "$1" migrate 2>&1 || true)
  case "$output" in
    *postgres.missing_database_url*) return 0 ;;
    *usage:*migrate*) return 0 ;;
    *) return 1 ;;
  esac
}

meta() {
  expected_schema=$1
  kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" port-forward service/runbook 18080:8080 >"$WORK/port-forward.log" 2>&1 &
  PF_PID=$!
  body=
  attempt=0
  while [ "$attempt" -lt 60 ]; do
    body=$(curl --silent --fail --max-time 2 http://127.0.0.1:18080/v1/meta 2>/dev/null || true)
    [ -n "$body" ] && break
    attempt=$((attempt + 1))
    sleep 1
  done
  kill "$PF_PID" 2>/dev/null || true
  wait "$PF_PID" 2>/dev/null || true
  PF_PID=
  [ -n "$body" ] || die "the app did not answer /v1/meta"
  case "$body" in
    *"\"postgres\":$expected_schema"* | *"\"postgres\": $expected_schema"*) ;;
    *) die "expected schema $expected_schema in /v1/meta, got: $body" ;;
  esac
  printf '%s\n' "$body"
}

org_count() {
  kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" exec deployment/postgres -- \
    psql -U runbook -d runbook -Atc 'SELECT count(*) FROM runbook.organization'
}

schema_of() {
  docker run --rm "$1" --version | sed -n 's/.*schema postgres \([0-9][0-9]*\).*/\1/p'
}

[ "$(git -C "$ROOT" rev-parse "$FROM_REF^{commit}")" != "$(git -C "$ROOT" rev-parse "$TO_REF^{commit}")" ] ||
  die "FROM_REF and TO_REF name the same commit"

build_image "$FROM_REF" "$FROM_IMAGE" from
build_image "$TO_REF" "$TO_IMAGE" to
FROM_SCHEMA=$(schema_of "$FROM_IMAGE")
TO_SCHEMA=$(schema_of "$TO_IMAGE")
[ -n "$FROM_SCHEMA" ] && [ -n "$TO_SCHEMA" ] || die "could not read a schema from both images"
[ "$TO_SCHEMA" -ge "$FROM_SCHEMA" ] || die "TO_REF schema $TO_SCHEMA is older than FROM_REF schema $FROM_SCHEMA"

say "Creating kind cluster $CLUSTER"
kind create cluster --name "$CLUSTER" --wait 180s >/dev/null
kind load docker-image --name "$CLUSTER" "$FROM_IMAGE" "$TO_IMAGE" >/dev/null
kubectl --context "kind-$CLUSTER" create namespace "$NAMESPACE" >/dev/null

say "Starting $FROM_REF (schema $FROM_SCHEMA)"
apply_app "$FROM_IMAGE" true false
kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" rollout status deployment/postgres --timeout=180s >/dev/null
wait_app
meta "$FROM_SCHEMA" >/dev/null
kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" exec deployment/runbook -c runbook -- \
  runbook-server bootstrap >"$WORK/bootstrap.json"
[ "$(org_count)" = 1 ] || die "bootstrap did not create exactly one organization"

say "Migrating and upgrading to $TO_REF (schema $TO_SCHEMA)"
run_migration_job "$TO_IMAGE" false
apply_app "$TO_IMAGE" false false
wait_app
meta "$TO_SCHEMA" >/dev/null
[ "$(org_count)" = 1 ] || die "organization data changed across the upgrade"
kubectl --context "kind-$CLUSTER" -n "$NAMESPACE" exec deployment/runbook -c runbook -- \
  runbook-server doctor >/dev/null

say "Rolling back to $FROM_REF on schema $TO_SCHEMA"
if supports_migrate "$FROM_IMAGE"; then
  run_migration_job "$FROM_IMAGE" true
fi
apply_app "$FROM_IMAGE" true true
wait_app
meta "$TO_SCHEMA" >/dev/null
[ "$(org_count)" = 1 ] || die "organization data changed across rollback"

say "Upgrading again to $TO_REF"
run_migration_job "$TO_IMAGE" false
apply_app "$TO_IMAGE" false false
wait_app
meta "$TO_SCHEMA" >/dev/null
[ "$(org_count)" = 1 ] || die "organization data changed across the second upgrade"

say "Kubernetes upgrade, rollback and re-upgrade all kept the data"
