#!/bin/sh
# Create and await one labelled on-demand Cloud SQL backup before applying an
# upgrade. Cloud SQL assigns backup ids itself (numbers), so the label goes in
# the backup's description and the assigned id is read back from it. The
# finished backup's id is the only line written to stdout, so a caller can
# record it with: BACKUP_ID=$(./pre-upgrade-backup.sh LABEL)
set -eu

usage() {
  echo "usage: $0 [label]" >&2
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

LABEL=${1:-runbook-pre-upgrade-$(date -u +%Y%m%d%H%M%S)}
case "$LABEL" in
  [a-z]*[!a-z0-9_-]* | '' | *[!a-z0-9_-]*)
    echo "error: the label must start with a lowercase letter and contain only lowercase letters, digits, hyphens and underscores" >&2
    exit 2
    ;;
esac
if [ "${#LABEL}" -gt 100 ]; then
  echo "error: the label must be at most 100 characters" >&2
  exit 2
fi

CONNECTION_NAME=$(terraform output -raw cloud_sql_connection_name)
PROJECT=${CONNECTION_NAME%%:*}
INSTANCE=${CONNECTION_NAME##*:}
if [ -z "$PROJECT" ] || [ -z "$INSTANCE" ] || [ "$PROJECT" = "$CONNECTION_NAME" ]; then
  echo "error: terraform output cloud_sql_connection_name is not PROJECT:REGION:INSTANCE" >&2
  exit 1
fi

# The backups carrying this label: none before the create, one after.
labelled() {
  gcloud sql backups list \
    --instance="$INSTANCE" \
    --project="$PROJECT" \
    --filter="description=$LABEL" \
    --format='value(id)'
}
if [ -n "$(labelled)" ]; then
  echo "error: a backup labelled $LABEL already exists; choose a new label for this upgrade" >&2
  exit 1
fi

echo "Creating Cloud SQL backup $LABEL for $INSTANCE..." >&2
gcloud sql backups create \
  --instance="$INSTANCE" \
  --project="$PROJECT" \
  --description="$LABEL" \
  --async >&2

POLL_SECONDS=${RUNBOOK_BACKUP_POLL_SECONDS:-10}
TIMEOUT_SECONDS=${RUNBOOK_BACKUP_TIMEOUT_SECONDS:-1800}
case "$POLL_SECONDS:$TIMEOUT_SECONDS" in
  *[!0-9:]* | :* | *:)
    echo "error: backup poll and timeout settings must be whole seconds" >&2
    exit 2
    ;;
esac

elapsed=0
BACKUP_ID=
STATUS=
while :; do
  if [ -z "$BACKUP_ID" ]; then
    # The new backup may take a moment to be listed.
    IDS=$(labelled)
    case "$IDS" in
      '') ;;
      *[!0-9]*)
        echo "error: expected one numeric backup id labelled $LABEL, got: $IDS" >&2
        exit 1
        ;;
      *) BACKUP_ID=$IDS ;;
    esac
  fi
  if [ -n "$BACKUP_ID" ]; then
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
        echo "error: Cloud SQL backup $BACKUP_ID ($LABEL) finished with status $STATUS" >&2
        exit 1
        ;;
    esac
  fi
  if [ "$elapsed" -ge "$TIMEOUT_SECONDS" ]; then
    echo "error: timed out waiting for Cloud SQL backup $LABEL (id ${BACKUP_ID:-not yet listed}); last status: ${STATUS:-unknown}" >&2
    exit 1
  fi
  sleep "$POLL_SECONDS"
  elapsed=$((elapsed + POLL_SECONDS))
  # A zero-second test poll still needs the timeout to make progress.
  if [ "$POLL_SECONDS" -eq 0 ]; then elapsed=$((elapsed + 1)); fi
done
