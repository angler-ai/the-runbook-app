#!/usr/bin/env bash
# Build runbook-pilot:smoke once, then run every container smoke test in
# sequence (each with its own compose project) and print a summary.
set -euo pipefail
cd "$(dirname "$0")"
repo=$(cd ../.. && pwd)
image=${RUNBOOK_SMOKE_IMAGE:-runbook-pilot:smoke}
tests=(fresh_start web_ui sign_in uploads restart_keeps_state crash_recovers db_restart upgrade_same_volume external_db platform)

started=$SECONDS
echo "building $image"
RUNBOOK_CACHE_ID="${RUNBOOK_CACHE_ID:-$(git -C "$repo" rev-parse --abbrev-ref HEAD 2>/dev/null | tr -c 'a-zA-Z0-9_-' '-' | sed 's/^-*//; s/-*$//' || true)}"
export RUNBOOK_CACHE_ID="${RUNBOOK_CACHE_ID:-default}"
docker build -q --build-arg RUNBOOK_CACHE_ID="$RUNBOOK_CACHE_ID" -t "$image" "$repo" >/dev/null
echo "built in $((SECONDS - started))s"
export RUNBOOK_SMOKE_SKIP_BUILD=1 RUNBOOK_SMOKE_IMAGE=$image RUNBOOK_CACHE_ID

results=()
failed=0
for test in "${tests[@]}"; do
  t0=$SECONDS
  if "./$test.sh"; then
    results+=("PASS  $test  $((SECONDS - t0))s")
  else
    results+=("FAIL  $test  $((SECONDS - t0))s")
    failed=1
  fi
done

echo
echo "Container smoke summary ($(docker image inspect -f '{{.Size}}' "$image" | awk '{printf "%.0f MB", $1/1000000}') image):"
printf '  %s\n' "${results[@]}"
echo "  total $((SECONDS - started))s"
exit "$failed"
