#!/usr/bin/env bash
# The Runbook pilot: provision a clean install with the CLI.
#
# Automates docs/install.md sections 3 to 7 against a running server that
# was bootstrapped with `runbook-server bootstrap`:
#
#   project `pilot`; Alice, Bob and Carol (project operators) with their own
#   tokens; teams `oncall` (Alice, Bob) and `eng-leads` (Carol); a launcher
#   service token; the flagship's two skills; the flagship `post-deploy`
#   published; webhook subscriptions for the recording receiver (every
#   event) and the GitHub Actions relay (`agent.work_available`).
#
# Re-running is safe: everything already created (recorded in
# $PILOT_DIR/state.env or found by name) is reused, and no stored secret
# is ever overwritten. Created IDs are printed; secrets are only written to
# 0600 files in $PILOT_DIR and never printed.
#
# Environment:
#   RUNBOOK_HOST                 server URL, e.g. http://127.0.0.1:8080 (required)
#   PILOT_DIR                    directory for secrets and state (required; made 0700)
#   RUNBOOK_BOOTSTRAP_TOKEN_FILE file holding the owner PAT from bootstrap, or
#   RUNBOOK_BOOTSTRAP_TOKEN      the owner PAT itself (first run only; stored as owner.pat)
#   PILOT_PSQL / DATABASE_URL    psql access for the interim first-token seeds (a
#                                PILOT_PSQL holding a URL or password is refused)
#                                (see seed-first-token.sh); without either, people's
#                                tokens are skipped and the script says how to finish
#   PILOT_PROJECT                project slug (default pilot)
#   RECEIVER_URL                 recording receiver URL as the server reaches it
#                                (default http://127.0.0.1:9000/)
#   RELAY_URL                    GitHub Actions relay URL as the server reaches it
#                                (default http://127.0.0.1:8081/)
#   PILOT_REUSE_USERS            1 to adopt existing users named Alice, Bob or Carol
#                                that this script did not create (default: refuse)
#   RUNBOOK_BIN                  CLI to run (default runbook)
#
# Needs bash, python3 and curl 7.76 or newer on PATH.
set -euo pipefail

: "${RUNBOOK_HOST:?set RUNBOOK_HOST to the server URL, e.g. http://127.0.0.1:8080}"
: "${PILOT_DIR:?set PILOT_DIR to a private directory for the pilot secrets}"
RUNBOOK_BIN=${RUNBOOK_BIN:-runbook}
PILOT_PROJECT=${PILOT_PROJECT:-pilot}
RECEIVER_URL=${RECEIVER_URL:-http://127.0.0.1:9000/}
RELAY_URL=${RELAY_URL:-http://127.0.0.1:8081/}
RUNBOOK_HOST=${RUNBOOK_HOST%/}
# Each person's commands use their own credentials, never an inherited one.
unset RUNBOOK_TOKEN

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
umask 077
mkdir -p "$PILOT_DIR"
chmod 700 "$PILOT_DIR"
PILOT_DIR=$(cd "$PILOT_DIR" && pwd)
state="$PILOT_DIR/state.env"
touch "$state"

die() { echo "setup: $*" >&2; exit 1; }
note() { echo "==> $*"; }
pj() { python3 -I "$here/pilot-json.py" "$@"; }
get_state() { sed -n "s/^$1=//p" "$state" | tail -n 1; }
set_state() { echo "$1=$2" >> "$state"; echo "    $1=$2"; }
# `rb <who> <args>`: the CLI with that person's private config directory.
rb() {
  local who=$1
  shift
  XDG_CONFIG_HOME="$PILOT_DIR/config/$who" "$RUNBOOK_BIN" "$@"
}
for tool in python3 curl "$RUNBOOK_BIN"; do
  command -v "$tool" > /dev/null || die "$tool is not on PATH"
done

# --- Owner credentials -------------------------------------------------------
owner_pat="$PILOT_DIR/owner.pat"
if [ ! -s "$owner_pat" ]; then
  if [ -n "${RUNBOOK_BOOTSTRAP_TOKEN_FILE:-}" ]; then
    python3 -I "$here/pilot-json.py" get pat < "$RUNBOOK_BOOTSTRAP_TOKEN_FILE" > "$owner_pat" 2> /dev/null \
      || cp "$RUNBOOK_BOOTSTRAP_TOKEN_FILE" "$owner_pat"
  elif [ -n "${RUNBOOK_BOOTSTRAP_TOKEN:-}" ]; then
    printf '%s\n' "$RUNBOOK_BOOTSTRAP_TOKEN" > "$owner_pat"
  else
    die "set RUNBOOK_BOOTSTRAP_TOKEN_FILE (or RUNBOOK_BOOTSTRAP_TOKEN) to the owner PAT from bootstrap"
  fi
  chmod 600 "$owner_pat"
fi
grep -q '^trb_pat_' "$owner_pat" || die "$owner_pat does not hold a personal access token (trb_pat_...)"
auth_header="$PILOT_DIR/.owner-auth-header"
if [ ! -s "$auth_header" ]; then
  printf 'Authorization: Bearer %s\n' "$(head -n 1 "$owner_pat")" > "$auth_header"
fi
# `api GET <path>` or `api POST <path> <body-file>`: the REST API as the
# owner, for what the CLI does not cover yet (lists, service tokens, skills).
# The token is read by curl from a 0600 header file, never from argv.
api() {
  local method=$1 path=$2
  if [ "$method" = GET ]; then
    curl -sS --fail-with-body -H @"$auth_header" "$RUNBOOK_HOST$path"
  else
    curl -sS --fail-with-body -X "$method" -H @"$auth_header" \
      -H 'Content-Type: application/json' -H "Idempotency-Key: $(random_key)" \
      --data-binary @"$3" "$RUNBOOK_HOST$path"
  fi
}
# `api_all <path>`: every page of a cursor-paginated list as one
# {"items": [...]} document.
api_all() {
  local path=$1 cursor="" n=0 sep
  local pages="$PILOT_DIR/.pages"
  rm -rf "$pages"
  mkdir -p "$pages"
  while :; do
    n=$((n + 1))
    if [ -n "$cursor" ]; then
      case "$path" in *\?*) sep='&' ;; *) sep='?' ;; esac
      api GET "$path${sep}cursor=$cursor" > "$pages/$n.json"
    else
      api GET "$path" > "$pages/$n.json"
    fi
    cursor=$(pj get next_cursor < "$pages/$n.json" 2> /dev/null || true)
    [ -n "$cursor" ] && [ "$cursor" != null ] || break
    [ "$n" -lt 1000 ] || die "too many pages from $path"
  done
  local files=() i
  for i in $(seq 1 "$n"); do files+=("$pages/$i.json"); done
  pj items "${files[@]}"
  rm -rf "$pages"
}
random_key() { python3 -I -c 'import secrets; print(secrets.token_hex(16))'; }
# Point a person's CLI config at the server and store their login token.
configure() {
  local who=$1 token_file=${2:-}
  mkdir -p "$PILOT_DIR/config/$who"
  rb "$who" context add pilot --host "$RUNBOOK_HOST" > /dev/null
  rb "$who" context use pilot > /dev/null
  if [ -n "$token_file" ]; then
    rb "$who" login --token-stdin < "$token_file" > /dev/null
  fi
}

note "Owner login and organization"
configure owner "$owner_pat"
org=$(rb owner whoami --json | pj get organizations.0.id)
echo "    organization $org"

# --- Project -----------------------------------------------------------------
note "Project '$PILOT_PROJECT'"
project=$(get_state PROJECT_ID)
if [ -z "$project" ]; then
  project=$(api_all "/v1/orgs/$org/projects" | pj find items "slug=$PILOT_PROJECT" id || true)
  if [ -z "$project" ]; then
    project=$(rb owner project create --slug "$PILOT_PROJECT" --title "Pilot" --json | pj get id)
  fi
  set_state PROJECT_ID "$project"
else
  echo "    PROJECT_ID=$project (existing)"
fi

# --- People --------------------------------------------------------------------
note "People: Alice, Bob and Carol (project operators)"
for name in Alice Bob Carol; do
  key="$(echo "$name" | tr '[:lower:]' '[:upper:]')_ID"
  user=$(get_state "$key")
  if [ -z "$user" ]; then
    # Display names are not unique: an existing "Alice" may be a real
    # person. Reuse one only when asked to; users this script created are
    # recorded in state.env and reused on later runs.
    user=$(api_all "/v1/orgs/$org/users" | pj find items "name=$name" id || true)
    if [ -n "$user" ] && [ "${PILOT_REUSE_USERS:-}" != 1 ]; then
      die "a user named $name ($user) already exists and is not recorded in $state; refusing to reuse it. Set PILOT_REUSE_USERS=1 to adopt existing users by name, or use a fresh organization."
    fi
    [ -n "$user" ] || user=$(rb owner user create --name "$name" --json | pj get id)
    set_state "$key" "$user"
  else
    echo "    $key=$user (existing)"
  fi
  role=$(api_all "/v1/projects/$project/members" | pj find items "user_id=$user" role || true)
  if [ "$role" != operator ]; then
    rb owner project member add "$project" "$user" --role operator > /dev/null
    echo "    $name is an operator of $PILOT_PROJECT"
  fi
done
alice=$(get_state ALICE_ID)
bob=$(get_state BOB_ID)
carol=$(get_state CAROL_ID)

# --- Teams ---------------------------------------------------------------------
note "Teams: oncall (Alice, Bob) and eng-leads (Carol)"
team() {
  local slug=$1 title=$2 key=$3
  shift 3
  local id
  id=$(get_state "$key")
  if [ -z "$id" ]; then
    id=$(api_all "/v1/orgs/$org/teams" | pj find items "slug=$slug" id || true)
    [ -n "$id" ] || id=$(rb owner team create --slug "$slug" --title "$title" --json | pj get id)
    set_state "$key" "$id"
  else
    echo "    $key=$id (existing)"
  fi
  local member
  for member in "$@"; do
    if ! api_all "/v1/orgs/$org/teams/$id/members" | pj find items "user_id=$member" user_id > /dev/null; then
      rb owner team member add "$id" "$member" > /dev/null
      echo "    added $member to $slug"
    fi
  done
}
team oncall "On-call" ONCALL_TEAM_ID "$alice" "$bob"
team eng-leads "Engineering leads" ENG_LEADS_TEAM_ID "$carol"

# --- Launcher service token ------------------------------------------------------
note "Launcher service token (executions:read, executions:act; operator)"
if [ ! -s "$PILOT_DIR/launcher.svc" ]; then
  body="$PILOT_DIR/.request.json"
  printf '{"kind":"service","name":"launcher","scopes":["executions:read","executions:act"],"projects":["%s"],"role":"operator"}\n' \
    "$project" > "$body"
  launcher=$(api POST /v1/tokens "$body" | pj keep "$PILOT_DIR" secret=launcher.svc | pj get token.id)
  rm -f "$body"
  set_state LAUNCHER_TOKEN_ID "$launcher"
else
  echo "    LAUNCHER_TOKEN_ID=$(get_state LAUNCHER_TOKEN_ID) (existing; secret in launcher.svc)"
fi

# --- Skills and the flagship -------------------------------------------------------
note "Skills used by the flagship"
for dir in "$here"/skills/*/; do
  skill=$(basename "$dir")
  existing=$(api_all "/v1/projects/$project/skills" | pj find items "name=$skill" hash || true)
  if [ -n "$existing" ]; then
    echo "    $skill (existing)"
    continue
  fi
  body="$PILOT_DIR/.request.json"
  python3 -I -c '
import base64, json, sys
content = open(sys.argv[1], "rb").read()
print(json.dumps({"files": [{"path": "SKILL.md", "content": base64.b64encode(content).decode()}]}))
' "$dir/SKILL.md" > "$body"
  hash=$(api POST "/v1/projects/$project/skills" "$body" | pj get hash)
  rm -f "$body"
  echo "    uploaded $skill ($hash)"
done

note "Flagship runbook post-deploy"
if ! version=$(rb owner get post-deploy --project "$PILOT_PROJECT" --json 2> /dev/null \
  | pj get latest_version.runbook_version 2> /dev/null); then
  rb owner push "$root/conformance/spec/post-deploy.yaml" --project "$PILOT_PROJECT" --draft > /dev/null
  version=$(rb owner publish post-deploy --project "$PILOT_PROJECT" --json | pj get runbook_version)
  echo "    published post-deploy version $version"
else
  echo "    post-deploy version $version (existing)"
fi

# --- Webhook subscriptions -------------------------------------------------------
note "Webhook subscriptions"
subscribe() {
  local name=$1 url=$2 file=$3 key=$4
  shift 4
  local id
  id=$(rb owner webhook list --project "$PILOT_PROJECT" --json | pj find items "name=$name" id || true)
  if [ -z "$id" ]; then
    local events=()
    local event
    for event in "$@"; do events+=(--event "$event"); done
    id=$(rb owner webhook create --project "$PILOT_PROJECT" --name "$name" --url "$url" "${events[@]}" --json \
      | pj keep "$PILOT_DIR" "secret=$file" | pj get webhook.id)
    set_state "$key" "$id"
  elif [ ! -s "$PILOT_DIR/$file" ]; then
    # The subscription exists but its secret was not kept: rotate it.
    rb owner webhook rotate-secret "$id" --project "$PILOT_PROJECT" --json \
      | pj keep "$PILOT_DIR" "secret=$file" > /dev/null
    echo "    $key=$id (existing; secret rotated into $file)"
  else
    echo "    $key=$id (existing)"
  fi
}
subscribe pilot-recorder "$RECEIVER_URL" receiver.whsec RECORDER_WEBHOOK_ID '*'
subscribe github-relay "$RELAY_URL" relay.whsec RELAY_WEBHOOK_ID agent.work_available

# --- People's own tokens -----------------------------------------------------------
note "People's own tokens"
configure agent
if [ -z "${PILOT_PSQL:-}" ] && [ -z "${DATABASE_URL:-}" ]; then
  echo "    skipped: set PILOT_PSQL (or DATABASE_URL) so the operator can seed each"
  echo "    person's first token (docs/install.md section 4), then run this again."
else
  for name in alice bob carol; do
    key="$(echo "$name" | tr '[:lower:]' '[:upper:]')_ID"
    PILOT_DIR="$PILOT_DIR" "$here/seed-first-token.sh" "$name" "$org" "$(get_state "$key")" "$project"
    configure "$name" "$PILOT_DIR/$name.seed"
    if [ ! -s "$PILOT_DIR/$name.pat" ]; then
      token=$(rb "$name" token create --name "$name-cli" \
        --scope runbooks:read --scope executions:read --scope executions:start --scope executions:act \
        --project "$PILOT_PROJECT" --expires 30d --json \
        | pj keep "$PILOT_DIR" "secret=$name.pat" | pj get token.id)
      echo "    $name minted $token (secret in $name.pat)"
    else
      echo "    $name already has $name.pat"
    fi
  done
fi

note "Done. IDs are in $state; secrets are 0600 files in $PILOT_DIR:"
(cd "$PILOT_DIR" && ls -1 ./*.pat ./*.seed ./*.svc ./*.whsec 2> /dev/null | sed 's|^\./|    |') || true
cat <<EOF

Next (docs/install.md section 8):
  RUNBOOK_WEBHOOK_SECRET_FILE="$PILOT_DIR/receiver.whsec" RECEIVER_LOG="$PILOT_DIR/events.jsonl" \\
    RECEIVER_MOCK_GITHUB=1 RECEIVER_GITHUB_TOKEN=pilot-dry-run python3 -I examples/pilot/recording-receiver.py
  PILOT_DIR="$PILOT_DIR" RUNBOOK_HOST="$RUNBOOK_HOST" examples/pilot/run-flagship.sh
EOF
