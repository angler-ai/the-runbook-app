#!/usr/bin/env bash
# The Runbook pilot: run the flagship `post-deploy` end to end with the
# identities created by setup.sh (docs/install.md section 8).
#
#   PILOT_DIR=... RUNBOOK_HOST=... examples/pilot/run-flagship.sh
#
# Alice starts a run for `api`. The trusted launcher (service token) mints
# an execution-scoped pair for the agent session, which does the agent
# steps with `runbook step` commands and nothing else. Alice does the human
# login check. With PILOT_SCENARIO=unhealthy (the default) eu-west's error
# rate fails, the stabilize loop recovers, and Bob (team oncall) decides
# `keep` on the approval with `runbook step decide`; with
# PILOT_SCENARIO=healthy every check passes and no approval opens. The
# agent then posts the report (`finally`). Prints the execution ID and the
# final status; secrets stay in their 0600 files.
#
# Environment: PILOT_DIR and RUNBOOK_HOST (as for setup.sh), PILOT_PROJECT
# (default pilot), PILOT_SCENARIO (unhealthy|healthy), RUNBOOK_BIN.
set -euo pipefail

: "${RUNBOOK_HOST:?set RUNBOOK_HOST}"
: "${PILOT_DIR:?set PILOT_DIR (the directory setup.sh used)}"
RUNBOOK_BIN=${RUNBOOK_BIN:-runbook}
PILOT_PROJECT=${PILOT_PROJECT:-pilot}
PILOT_SCENARIO=${PILOT_SCENARIO:-unhealthy}
unset RUNBOOK_TOKEN
here=$(cd "$(dirname "$0")" && pwd)
die() { echo "run-flagship: $*" >&2; exit 1; }
pj() { python3 -I "$here/pilot-json.py" "$@"; }
for who in alice bob launcher; do
  file="$PILOT_DIR/$who.pat"
  [ "$who" = launcher ] && file="$PILOT_DIR/launcher.svc"
  [ -s "$file" ] || die "$file is missing: run setup.sh first"
done
case "$PILOT_SCENARIO" in
  unhealthy) rates="0.002 0.003 0.05" ;;
  healthy) rates="0.002 0.003 0.004" ;;
  *) die "PILOT_SCENARIO must be unhealthy or healthy" ;;
esac

# A person: their own config directory and their own token (from a 0600
# file into the CLI's environment, never onto a command line).
person() {
  local who=$1
  shift
  RUNBOOK_TOKEN="$(head -n 1 "$PILOT_DIR/$who.pat")" XDG_CONFIG_HOME="$PILOT_DIR/config/$who" \
    "$RUNBOOK_BIN" "$@"
}
# The agent: only the execution-scoped pair stored by `run token`.
agent() { XDG_CONFIG_HOME="$PILOT_DIR/config/agent" "$RUNBOOK_BIN" "$@"; }

# Wait until <path> is ready work for this actor; print its step run ID.
ready() {
  local actor=$1 path=$2 sr="" i
  for i in $(seq 1 60); do
    if [ "$actor" = agent ]; then
      sr=$(agent step next "$exe" --json | pj find ready_work "path=$path" step_run_id || true)
    else
      sr=$(person "$actor" step next "$exe" --json | pj find ready_work "path=$path" step_run_id || true)
    fi
    [ -n "$sr" ] && { echo "$sr"; return 0; }
    sleep 1
  done
  die "$path did not become ready for $actor"
}

# One agent step: claim, read, report.
agent_step() {
  local path=$1 verdict=$2
  shift 2
  local sr
  sr=$(ready agent "$path")
  agent step claim "$sr" --session "$session" --agent-name pilot-agent --model manual > /dev/null
  agent step get "$sr" > /dev/null
  if [ "$verdict" = - ]; then
    agent step report "$sr" --status succeeded "$@" > /dev/null
  else
    agent step report "$sr" --status succeeded --verdict "$verdict" "$@" > /dev/null
  fi
  if [ "$verdict" = - ]; then
    echo "    agent reported $path"
  else
    echo "    agent reported $path ($verdict)"
  fi
}

echo "==> Alice starts post-deploy for api v2.0.0"
exe=$(person alice run start post-deploy --project "$PILOT_PROJECT" \
  --input service=api --input version=v2.0.0 --tag pilot --json | pj get id)
echo "    execution $exe"
# A new run is `queued` until admitted; the flagship admits one run per
# service at a time, so an unfinished earlier run keeps it queued. Execution
# credentials exist only once it is running.
for _ in $(seq 1 30); do
  [ "$(person alice run get "$exe" --json | pj get status)" != queued ] && break
  sleep 1
done
[ "$(person alice run get "$exe" --json | pj get status)" != queued ] \
  || die "$exe is still queued: an earlier post-deploy run for api is unfinished. Finish it, or cancel it (runbook run cancel <exe>) and have an agent report its finally step notify, then run this again"

echo "==> The launcher mints an execution-scoped pair for the agent session"
session="agent-$exe"
RUNBOOK_TOKEN="$(head -n 1 "$PILOT_DIR/launcher.svc")" XDG_CONFIG_HOME="$PILOT_DIR/config/agent" \
  "$RUNBOOK_BIN" run token "$exe" --session "$session" > /dev/null
agent whoami > /dev/null

echo "==> Smoke tests"
agent_step api_health pass --output status_code=200 --summary "GET /healthz returned 200"
sr=$(ready alice login_flow)
person alice step claim "$sr" --session alice-cli > /dev/null
person alice step checklist "$sr" --tick "Login page loads" --tick "Dashboard shows data" > /dev/null
person alice step report "$sr" --status succeeded --verdict pass --summary "Logged in; dashboard shows data" > /dev/null
echo "    Alice reported login_flow pass"

echo "==> Error rate per region ($PILOT_SCENARIO)"
i=0
for rate in $rates; do
  verdict=pass
  if python3 -I -c 'import sys; sys.exit(0 if float(sys.argv[1]) > 0.01 else 1)' "$rate"; then
    verdict=fail
  fi
  agent_step "regions[$i].error_rate" "$verdict" --output "rate=$rate" \
    --evidence "https://dashboards.example/errors?region=$i"
  i=$((i + 1))
done

if [ "$PILOT_SCENARIO" = unhealthy ]; then
  echo "==> Stabilize loop: the re-check recovers"
  agent_step "stabilize#1.recheck" pass --output rate=0.004
  echo "==> Bob (oncall) decides the approval"
  signoff=""
  for _ in $(seq 1 60); do
    signoff=$(person bob run report "$exe" --json | pj find sections path=signoff step_run_id || true)
    [ -n "$signoff" ] && break
    sleep 1
  done
  [ -n "$signoff" ] || die "the signoff approval did not open"
  person bob step decide "$signoff" --option keep --comment "Recovered after the re-check; keep v2.0.0" > /dev/null
  echo "    Bob decided keep on $signoff"
fi

echo "==> The agent posts the report (finally)"
agent_step notify - --summary "Report posted to #deploys"

status=$(person alice run get "$exe" --wait --json | pj get status || true)
result=$(person alice run get "$exe" --json | pj get result)
echo "==> $exe $status (result: $result)"
[ "$status" = succeeded ] || exit 3
