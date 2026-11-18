#!/bin/sh
# Create and await one named on-demand Cloud SQL backup before applying an
# upgrade. The finished backup id is the only line written to stdout, so a
# caller can record it with: BACKUP_ID=$(./pre-upgrade-backup.sh ID)
set -eu

usage() {
  echo "usage: $0 [backup-id]" >&2
  echo "Run from an initialized copy of this Terraform module." >&2
}

if [ "$#" -gt 1 ]; then
  usage
  exit 2
fi
for command in terraform gcloud; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "error: $command is required" >&2
    exit 127
  fi
done

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$SCRIPT_DIR"

BACKUP_ID=${1:-runbook-pre-upgrade-$(date -u +%Y%m%d%H%M%S)}
case "$BACKUP_ID" in
  [a-z]*[!a-z0-9_-]* | '' | *[!a-z0-9_-]*)
    echo "error: backup id must start with a lowercase letter and contain only lowercase letters, digits, hyphens and underscores" >&2
    exit 2
    ;;
esac
if [ "${#BACKUP_ID}" -gt 100 ]; then
  echo "error: backup id must be at most 100 characters" >&2
  exit 2
fi

CONNECTION_NAME=$(terraform output -raw cloud_sql_connection_name)
PROJECT=${CONNECTION_NAME%%:*}
INSTANCE=${CONNECTION_NAME##*:}
if [ -z "$PROJECT" ] || [ -z "$INSTANCE" ] || [ "$PROJECT" = "$CONNECTION_NAME" ]; then
  echo "error: terraform output cloud_sql_connection_name is not PROJECT:REGION:INSTANCE" >&2
  exit 1
fi

EXISTING=$(gcloud sql backups list \
  --instance="$INSTANCE" \
  --project="$PROJECT" \
  --filter="id=$BACKUP_ID" \
  --format='value(id)')
if [ -n "$EXISTING" ]; then
  echo "error: backup $BACKUP_ID already exists; choose a new id for this upgrade" >&2
  exit 1
fi

echo "Creating Cloud SQL backup $BACKUP_ID for $INSTANCE..." >&2
gcloud sql backups create \
  --instance="$INSTANCE" \
  --project="$PROJECT" \
  --backup-id="$BACKUP_ID" \
  --description='Runbook pre-upgrade backup' >&2

POLL_SECONDS=${RUNBOOK_BACKUP_POLL_SECONDS:-10}
TIMEOUT_SECONDS=${RUNBOOK_BACKUP_TIMEOUT_SECONDS:-1800}
case "$POLL_SECONDS:$TIMEOUT_SECONDS" in
  *[!0-9:]* | :* | *:)
    echo "error: backup poll and timeout settings must be whole seconds" >&2
    exit 2
    ;;
esac

elapsed=0
while :; do
  STATUS=$(gcloud sql backups describe "$BACKUP_ID" \
    --instance="$INSTANCE" \
    --project="$PROJECT" \
    --format='value(status)')
  case "$STATUS" in
    SUCCESSFUL)
      printf '%s\n' "$BACKUP_ID"
      exit 0
      ;;
    FAILED | DELETION_FAILED)
      echo "error: Cloud SQL backup $BACKUP_ID finished with status $STATUS" >&2
      exit 1
      ;;
  esac
  if [ "$elapsed" -ge "$TIMEOUT_SECONDS" ]; then
    echo "error: timed out waiting for Cloud SQL backup $BACKUP_ID; last status: ${STATUS:-unknown}" >&2
    exit 1
  fi
  sleep "$POLL_SECONDS"
  elapsed=$((elapsed + POLL_SECONDS))
  # A zero-second test poll still needs the timeout to make progress.
  if [ "$POLL_SECONDS" -eq 0 ]; then elapsed=$((elapsed + 1)); fi
done
