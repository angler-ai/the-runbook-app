#!/usr/bin/env bash
# The Runbook: update a docker compose install to a newer release (UPD1).
#
#   deploy/update.sh                  # to the latest release, asking first
#   deploy/update.sh --to 0.3.1 --yes # a named release
#   deploy/update.sh --to 0.3         # the newest 0.3.x (pin a minor line)
#   deploy/update.sh --dry-run        # print the plan, change nothing
#   deploy/update.sh --rollback       # undo the last update (previous image)
#   deploy/update.sh --source git     # build a release tag of this checkout
#
# What it does, in order (each step stops the update on failure):
#   1. Preflight: docker compose and the running stack; `runbook-server
#      doctor`; the current release (`runbook-server --version`, or
#      /v1/meta); the target release; refuses a downgrade; free disk space.
#   2. The target release (--to, else RUNBOOK_UPDATE_PIN from the env file,
#      else latest; each `latest`, `X.Y` or `X.Y.Z`), from a SOURCE:
#        image  (default) the release image ghcr.io/angler-ai/runbook:
#               <X.Y.Z | X.Y | latest> (--image-repo / RUNBOOK_IMAGE_REPO),
#               resolved to its digest, its build provenance verified with
#               `gh attestation verify --repo angler-ai/the-runbook-app`
#               (--attestation-repo), then pulled by digest. Without gh the
#               update refuses unless --insecure-skip-verify. Nothing from
#               the image runs before it is verified;
#        git    a `vX.Y.Z` tag of this checkout (`latest`: the highest;
#               `X.Y`: the highest vX.Y.*), checked out, then
#               `docker compose build`.
#      Your RUNBOOK_IMAGE (default runbook-pilot:local) stays a local name:
#      the script points it at the release it installs.
#      Then the target's own `runbook-server doctor` against the database:
#      refuses a schema the target cannot run (a newer one, or a changed
#      history). The target's rollback floor (`runbook-server --version`:
#      the oldest release whose code is safe on its schema) must not be
#      newer than the running release, or the update needs --restore-backup
#      (a failure then restores the backup) or --no-rollback. Nothing has
#      stopped yet.
#   3. Backup: pg_dump of the whole database (blobs and uploads live in
#      PostgreSQL too) to a timestamped 0600 file in the backup directory
#      (--backup-dir, RUNBOOK_BACKUP_DIR, default deploy/backups/), after
#      checking there is room, then read back in full (`pg_restore -f
#      /dev/null`): an empty or truncated dump stops the update. Keep
#      RUNBOOK_WEBHOOK_SECRET_KEY with it: restored webhook secrets need it.
#      After a successful update only the newest --keep-backups dumps stay.
#   4. Tag the running image <repo>:rollback-<release>, then restart the
#      server on the new image; `serve` applies the pending migrations
#      before it listens.
#   5. Health: the container healthcheck, /health/ready (the database
#      answers; /v1/meta on a release without the probe), /v1/meta
#      reporting the target release and schema, `doctor`, discovery, the
#      web UI and the CLI.
#   6. On failure: roll back (asks first unless --yes; --no-rollback skips
#      it): the previous image again, which runs on the migrated schema
#      because every migration keeps the previous release working
#      (expand/contract, CONTRIBUTING.md), with RUNBOOK_ALLOW_NEWER_SCHEMA
#      set when the schema moved (through a compose override file in the
#      backup directory, used until the next update). Only with
#      --restore-backup is the database restored from the backup as well,
#      which loses everything written after it: the current database is
#      dumped first, the backup restored into a scratch database, and the
#      two swapped by rename (the replaced one is kept).
#
# Options:
#   --to latest|X.Y|X.Y.Z   target release (default: RUNBOOK_UPDATE_PIN, else latest)
#   --source image|git      where the release comes from (default: image)
#   --image-repo REPO       image source repository (default ghcr.io/angler-ai/runbook)
#   --env-file FILE         compose env file (default: deploy/.env)
#   --compose-file FILE     compose file (default: deploy/docker-compose.yml);
#                           repeat it for overrides, in compose's order
#   --project NAME          compose project name (default: the file's `name:`)
#   --backup-dir DIR        where backups and the update record go
#   --skip-backup           no pg_dump (required with an external database:
#                           back it up with your provider's tools first)
#   --restore-backup        on failure, also restore the backup (loses newer data)
#   --no-rollback           on failure, leave it as it is and print how to roll back
#   --no-fetch              git source: do not `git fetch --tags` first
#   --health-timeout SECS   wait this long for the new server (default: 180)
#   --yes                   do not ask
#   --dry-run               preflight and plan only; changes nothing
#   --rollback              undo the last update recorded in the backup directory
#   --confirm-backup ID     with --rollback --restore-backup: the backup's ID
#                           (its file name without .dump), required when it is
#                           older than a day and nobody can type it
#   --keep-backups N        dumps to keep after an update (default 5; 0 keeps all)
#   --attestation-repo O/N  the repository the image must be attested by
#   --insecure-skip-verify  install an image without verifying its provenance
#
# Exit status: 0 updated (or already current), 1 refused or failed before
# anything changed, 2 usage, 3 the update failed and was rolled back, 4 the
# update failed and was not (or could not be) rolled back.
#
# Needs docker with compose v2, git (git source) and a POSIX userland. It
# never prints secrets, never pushes an image and never touches another
# compose project.
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

say() { printf '==> %s\n' "$*"; }
note() { printf '    %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
  printf 'error: %s\n' "$*" >&2
  exit "${2:-1}"
}

usage() {
  sed -n '2,/^set -euo pipefail/p' "$0" | sed -e '/^set -euo/d' -e 's/^# \{0,1\}//'
}

# --- options ---------------------------------------------------------------

TO=
SOURCE=image
IMAGE_REPO=${RUNBOOK_IMAGE_REPO:-ghcr.io/angler-ai/runbook}
ENV_FILE=$SCRIPT_DIR/.env
COMPOSE_FILES=()
PROJECT=
BACKUP_DIR=${RUNBOOK_BACKUP_DIR:-$SCRIPT_DIR/backups}
SKIP_BACKUP=0
RESTORE_BACKUP=0
NO_ROLLBACK=0
NO_FETCH=0
HEALTH_TIMEOUT=180
YES=0
DRY_RUN=0
ROLLBACK_ONLY=0
PINNED=0
ALLOW_FILE=
DC_OVERRIDE=
CONFIRM_BACKUP=
KEEP_BACKUPS=${RUNBOOK_KEEP_BACKUPS:-5}
ATTESTATION_REPO=${RUNBOOK_ATTESTATION_REPO:-angler-ai/the-runbook-app}
INSECURE_SKIP_VERIFY=0

parse_args() {
  while [ $# -gt 0 ]; do
    case $1 in
      --to) TO=${2:?--to needs a value}; shift ;;
      --source) SOURCE=${2:?--source needs a value}; shift ;;
      --image-repo) IMAGE_REPO=${2:?--image-repo needs a value}; shift ;;
      --env-file) ENV_FILE=${2:?--env-file needs a value}; shift ;;
      --compose-file) COMPOSE_FILES+=("${2:?--compose-file needs a value}"); shift ;;
      --project) PROJECT=${2:?--project needs a value}; shift ;;
      --backup-dir) BACKUP_DIR=${2:?--backup-dir needs a value}; shift ;;
      --skip-backup) SKIP_BACKUP=1 ;;
      --restore-backup) RESTORE_BACKUP=1 ;;
      --no-rollback) NO_ROLLBACK=1 ;;
      --no-fetch) NO_FETCH=1 ;;
      --health-timeout) HEALTH_TIMEOUT=${2:?--health-timeout needs a value}; shift ;;
      --yes | -y) YES=1 ;;
      --dry-run) DRY_RUN=1 ;;
      --rollback) ROLLBACK_ONLY=1 ;;
      --confirm-backup) CONFIRM_BACKUP=${2:?--confirm-backup needs a value}; shift ;;
      --keep-backups) KEEP_BACKUPS=${2:?--keep-backups needs a value}; shift ;;
      --attestation-repo) ATTESTATION_REPO=${2:?--attestation-repo needs a value}; shift ;;
      --insecure-skip-verify) INSECURE_SKIP_VERIFY=1 ;;
      -h | --help) usage; exit 0 ;;
      *) die "unknown option: $1 (see --help)" 2 ;;
    esac
    shift
  done
  case $SOURCE in git | image) ;; *) die "--source must be image or git" 2 ;; esac
  case $HEALTH_TIMEOUT in '' | *[!0-9]*) die "--health-timeout must be a number of seconds" 2 ;; esac
  case $KEEP_BACKUPS in '' | *[!0-9]*) die "--keep-backups must be a number" 2 ;; esac
  [ ${#COMPOSE_FILES[@]} -gt 0 ] || COMPOSE_FILES=("$SCRIPT_DIR/docker-compose.yml")
  local i
  for i in "${!COMPOSE_FILES[@]}"; do
    [ -f "${COMPOSE_FILES[$i]}" ] || die "no compose file at ${COMPOSE_FILES[$i]} (--compose-file)"
    COMPOSE_FILES[i]=$(cd "$(dirname "${COMPOSE_FILES[$i]}")" && pwd)/$(basename "${COMPOSE_FILES[$i]}")
  done
  [ -f "$ENV_FILE" ] || die "no env file at $ENV_FILE (--env-file; see deploy/.env.example)"
  ENV_FILE=$(cd "$(dirname "$ENV_FILE")" && pwd)/$(basename "$ENV_FILE")
  # A pinned release line or version in the env file, unless --to says otherwise.
  if [ -z "$TO" ]; then
    TO=$(env_file_value RUNBOOK_UPDATE_PIN)
    [ -n "$TO" ] && PINNED=1
  fi
  TO=${TO:-latest}
  TO=${TO#v}
  case $TO in
    latest) ;;
    *) printf '%s' "$TO" | grep -Eq '^[0-9]+\.[0-9]+(\.[0-9]+)?$' ||
      die "--to (or RUNBOOK_UPDATE_PIN) must be latest, X.Y or X.Y.Z, not $TO" 2 ;;
  esac
  ALLOW_FILE=$BACKUP_DIR/allow-newer-schema.yml
}

# KEY's value in the env file (read, never sourced), without quotes.
env_file_value() {
  sed -nE "s/^[[:space:]]*$1[[:space:]]*=[[:space:]]*['\"]?([^'\"#[:space:]]*).*$/\1/p" "$ENV_FILE" | tail -n1
}

# --- helpers -----------------------------------------------------------------

# docker compose for this install. With DC_OVERRIDE set (a rollback onto a
# newer schema), its file is added after the compose file.
dc() {
  local args=() file
  [ -n "$PROJECT" ] && args=(-p "$PROJECT")
  for file in "${COMPOSE_FILES[@]}"; do args+=(-f "$file"); done
  [ -n "${DC_OVERRIDE:-}" ] && args+=(-f "$DC_OVERRIDE")
  docker compose "${args[@]}" --env-file "$ENV_FILE" "$@"
}

# semver_cmp A B prints -1, 0 or 1. Versions are MAJOR.MINOR.PATCH;
# "unknown" (a release before UPD1) is older than every release.
semver_cmp() {
  local a=$1 b=$2 i x y
  [ "$a" = "$b" ] && { echo 0; return; }
  [ "$a" = unknown ] && { echo -1; return; }
  [ "$b" = unknown ] && { echo 1; return; }
  for i in 1 2 3; do
    x=$(printf '%s' "$a" | cut -d. -f"$i")
    y=$(printf '%s' "$b" | cut -d. -f"$i")
    if [ "$x" -lt "$y" ]; then echo -1; return; fi
    if [ "$x" -gt "$y" ]; then echo 1; return; fi
  done
  echo 0
}

is_semver() { printf '%s' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; }

# "runbook-server 0.2.0 (schema postgres 31; rollback floor 0.1.0)" ->
# "0.2.0 31 0.1.0"; the floor is "-" when there is none (or the binary
# predates the floor).
parse_version_line() {
  sed -nE 's/^runbook-server ([0-9]+\.[0-9]+\.[0-9]+) \(schema postgres ([0-9]+)(; rollback floor ([0-9]+\.[0-9]+\.[0-9]+|none))?\).*$/\1 \2 \4/p' |
    awk '{ print $1, $2, ($3 == "" || $3 == "none") ? "-" : $3 }'
}

# Whether release $1 is below floor $2 (code not safe on that schema). A
# release before UPD1 ("unknown") refuses any newer schema, so it always is.
below_floor() {
  [ "$1" = unknown ] && return 0
  [ "$2" = - ] && return 1
  [ "$(semver_cmp "$1" "$2")" = -1 ]
}

# The first "key":value (a string or a number) in compact JSON (serde_json
# output, which /v1/meta is), without the quotes.
json_field() {
  grep -oE "\"$1\":(\"[^\"]*\"|[0-9]+)" | head -n1 | sed -E "s/^\"$1\"://; s/\"//g"
}

# An image reference without its tag ("host:5000/x/runbook:1.0" ->
# "host:5000/x/runbook"; a digest is dropped too).
image_repo() {
  local ref=${1%@*} last
  last=${ref##*/}
  case $last in *:*) printf '%s' "${ref%:*}" ;; *) printf '%s' "$ref" ;; esac
}

# The highest vMAJOR.MINOR.PATCH tag of the checkout; with an argument
# X.Y, the highest vX.Y.*.
latest_tag() {
  local tag best='' best_version=unknown
  for tag in $(git -C "$REPO_DIR" tag -l 'v*'); do
    is_semver "${tag#v}" || continue
    case ${1:-} in '') ;; *) case ${tag#v} in "$1".*) ;; *) continue ;; esac ;; esac
    if [ "$(semver_cmp "${tag#v}" "$best_version")" = 1 ]; then
      best=$tag
      best_version=${tag#v}
    fi
  done
  printf '%s' "$best"
}

confirm() {
  [ "$YES" = 1 ] && return 0
  if [ ! -t 0 ]; then
    die "not a terminal: pass --yes to proceed without asking"
  fi
  local answer
  printf '%s [y/N] ' "$1"
  read -r answer || answer=
  case $answer in y | Y | yes | YES) return 0 ;; *) return 1 ;; esac
}

server_id() { dc ps -q server 2>/dev/null | head -n1; }

# GET a path from the server inside its container, on the port it listens
# on (RUNBOOK_LISTEN_ADDR as compose sets it, else the image's PORT when
# it opts in with RUNBOOK_LISTEN_PORT_FROM_ENV, else 8080).
# Prints the body; fails on an HTTP error. With --status, prints only the
# status code (000 when nothing answers).
server_get() {
  local mode=body
  [ "$1" = --status ] && { mode=status; shift; }
  # shellcheck disable=SC2016 # expanded by the container's shell
  dc exec -T -e UPDATE_PATH="$1" -e UPDATE_MODE="$mode" server sh -c '
    # The server listens on RUNBOOK_LISTEN_ADDR when set; else on PORT when
    # the image opts in (RUNBOOK_LISTEN_PORT_FROM_ENV, HOST1); else 8080.
    port=${RUNBOOK_LISTEN_ADDR:+${RUNBOOK_LISTEN_ADDR##*:}}
    case ${RUNBOOK_LISTEN_PORT_FROM_ENV:-} in 1 | true) port=${port:-${PORT:-}} ;; esac
    url="http://127.0.0.1:${port:-8080}$UPDATE_PATH"
    if [ "$UPDATE_MODE" = status ]; then
      curl -s -o /dev/null -w "%{http_code}" --max-time 5 "$url" || true
    else
      curl -fsS --max-time 5 "$url"
    fi'
}

# Wait until the server says it is ready: /health/ready answers 200 (the
# database answers). A release without the probe (404) counts as ready once
# /v1/meta answers.
wait_ready() {
  local deadline=$((SECONDS + $1)) status
  while :; do
    status=$(server_get --status /health/ready 2>/dev/null || echo 000)
    case $status in
      200) return 0 ;;
      404) server_get /v1/meta >/dev/null 2>&1 && return 0 ;;
    esac
    [ $SECONDS -ge $deadline ] && {
      warn "/health/ready did not answer 200 (last: $status)"
      return 1
    }
    sleep 1
  done
}

# The running server's release and schema: "VERSION SCHEMA" (or "unknown -"
# for a release before UPD1, which has neither --version nor meta.version).
running_release() {
  local line meta version
  line=$(dc exec -T server runbook-server --version 2>/dev/null | parse_version_line || true)
  if [ -n "$line" ]; then
    line=${line% *}
    printf '%s\n' "$line"
    return
  fi
  meta=$(server_get /v1/meta 2>/dev/null || true)
  version=$(printf '%s' "$meta" | json_field version)
  if is_semver "${version:-}"; then
    printf '%s %s\n' "$version" "$(printf '%s' "$meta" | json_field postgres)"
  else
    printf 'unknown -\n'
  fi
}

# The schema the database holds, read as the bundled database's
# administrator (empty without the bundled database).
database_schema() {
  # shellcheck disable=SC2016 # expanded by the container's shell
  dc exec -T postgres sh -c \
    'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atq -c "SELECT coalesce(max(version), 0) FROM runbook.schema_migration"' \
    2>/dev/null || true
}

# --- the update record (no secrets; KEY=VALUE, read without `source`) --------

record_file() { printf '%s/last-update' "$BACKUP_DIR"; }

write_record() {
  local file
  file=$(record_file)
  (
    umask 077
    {
      printf 'FROM_VERSION=%s\n' "$CURRENT_VERSION"
      printf 'FROM_SCHEMA=%s\n' "$CURRENT_SCHEMA"
      printf 'TO_VERSION=%s\n' "$TARGET_VERSION"
      printf 'TO_SCHEMA=%s\n' "$TARGET_SCHEMA"
      printf 'TO_FLOOR=%s\n' "$TARGET_FLOOR"
      printf 'IMAGE_REF=%s\n' "$IMAGE_REF"
      printf 'ROLLBACK_IMAGE=%s\n' "$ROLLBACK_IMAGE"
      printf 'GIT_PREVIOUS_REF=%s\n' "$GIT_PREVIOUS_REF"
      printf 'BACKUP_FILE=%s\n' "$BACKUP_FILE"
      printf 'SOURCE=%s\n' "$SOURCE"
      printf 'VERIFIED=%s\n' "$VERIFIED"
    } >"$file"
  )
}

read_record() {
  local file key value
  file=$(record_file)
  [ -f "$file" ] || die "no update recorded in $BACKUP_DIR (--backup-dir)"
  while IFS='=' read -r key value; do
    case $key in
      FROM_VERSION) CURRENT_VERSION=$value ;;
      FROM_SCHEMA) CURRENT_SCHEMA=$value ;;
      TO_VERSION) TARGET_VERSION=$value ;;
      TO_SCHEMA) TARGET_SCHEMA=$value ;;
      TO_FLOOR) TARGET_FLOOR=$value ;;
      IMAGE_REF) IMAGE_REF=$value ;;
      ROLLBACK_IMAGE) ROLLBACK_IMAGE=$value ;;
      GIT_PREVIOUS_REF) GIT_PREVIOUS_REF=$value ;;
      BACKUP_FILE) BACKUP_FILE=$value ;;
      SOURCE) SOURCE=$value ;;
    esac
  done <"$file"
}

# --- state -------------------------------------------------------------------

CURRENT_VERSION=
CURRENT_SCHEMA=
TARGET_VERSION=
TARGET_SCHEMA=
TARGET_FLOOR=-
VERIFIED=no
VERIFIED_VERSION=
TARGET_TAG=
TARGET_IMAGE=
IMAGE_REF=
ROLLBACK_IMAGE=
GIT_PREVIOUS_REF=
GIT_CHECKED_OUT=0
BACKUP_FILE=
HAS_BUNDLED_DB=0
RESTORE_FAILED=0
REPLACED_DB=
REPO_DIR=

# --- 1. preflight --------------------------------------------------------------

preflight() {
  say "Preflight"
  command -v docker >/dev/null || die "docker is not installed"
  docker compose version >/dev/null 2>&1 || die "docker compose v2 is required"
  local id
  id=$(server_id)
  [ -n "$id" ] || die "the server is not running in this compose project (start it with 'docker compose ... up -d --wait', or pass --project / --compose-file)"
  [ "$(docker inspect -f '{{.State.Running}}' "$id")" = true ] || die "the server container is not running"
  IMAGE_REF=$(docker inspect -f '{{.Config.Image}}' "$id")
  note "server image: $IMAGE_REF"

  read -r CURRENT_VERSION CURRENT_SCHEMA _ <<<"$(running_release)"
  note "current release: $CURRENT_VERSION (schema ${CURRENT_SCHEMA})"

  local doctor
  if ! doctor=$(dc exec -T server runbook-server doctor 2>&1); then
    printf '%s\n' "$doctor" | sed 's/^/    doctor: /' >&2
    die "doctor fails on the running install: fix that before updating"
  fi
  printf '%s\n' "$doctor" | sed 's/^/    doctor: /'

  if [ -n "$(dc ps -q postgres 2>/dev/null)" ]; then
    HAS_BUNDLED_DB=1
  elif [ "$SKIP_BACKUP" = 0 ]; then
    die "no bundled PostgreSQL in this project: back the database up with your provider's tools, then pass --skip-backup"
  fi

  if [ "$SOURCE" = git ]; then
    command -v git >/dev/null || die "git is not installed (or use --source image)"
    REPO_DIR=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel 2>/dev/null) ||
      die "$SCRIPT_DIR is not in a git checkout: the git source updates a checkout"
    if [ -n "$(git -C "$REPO_DIR" status --porcelain --untracked-files=no)" ]; then
      die "the checkout has local changes to tracked files: commit or stash them first"
    fi
    GIT_PREVIOUS_REF=$(git -C "$REPO_DIR" symbolic-ref -q --short HEAD || git -C "$REPO_DIR" rev-parse HEAD)
  fi
  resolve_target
  check_disk
  if [ "$RESTORE_BACKUP" = 1 ] && [ "$HAS_BUNDLED_DB" = 1 ]; then
    restore_preconditions || die "--restore-backup could not restore here (see above); nothing was changed"
  fi
}

resolve_target() {
  case $SOURCE in
    git)
      if [ "$NO_FETCH" = 0 ] && [ "$DRY_RUN" = 0 ] && git -C "$REPO_DIR" remote get-url origin >/dev/null 2>&1; then
        note "fetching release tags from origin"
        git -C "$REPO_DIR" fetch --quiet --tags origin || die "git fetch --tags failed (or pass --no-fetch)"
      fi
      if [ "$TO" = latest ]; then
        TARGET_TAG=$(latest_tag)
        [ -n "$TARGET_TAG" ] || die "no release tags (vX.Y.Z) in this checkout"
      elif ! is_semver "$TO"; then
        TARGET_TAG=$(latest_tag "$TO")
        [ -n "$TARGET_TAG" ] || die "no v$TO.* release tag in this checkout"
      else
        TARGET_TAG=v$TO
        git -C "$REPO_DIR" rev-parse -q --verify "refs/tags/$TARGET_TAG" >/dev/null || die "no tag $TARGET_TAG in this checkout"
      fi
      TARGET_VERSION=${TARGET_TAG#v}
      is_semver "$TARGET_VERSION" || die "$TARGET_TAG is not a vMAJOR.MINOR.PATCH release tag"
      local cargo
      cargo=$(git -C "$REPO_DIR" show "$TARGET_TAG:Cargo.toml" | awk '/^\[workspace.package\]/{p=1;next} /^\[/{p=0} p && /^version *=/{gsub(/[" ]/,"",$0); sub(/^version=/,"",$0); print; exit}')
      [ "$cargo" = "$TARGET_VERSION" ] || die "$TARGET_TAG's Cargo.toml says version ${cargo:-none}, not $TARGET_VERSION"
      TARGET_IMAGE=$(image_repo "$IMAGE_REF"):$TARGET_VERSION
      ;;
    image)
      if [ "$(image_repo "$IMAGE_REF")" = "$IMAGE_REPO" ]; then
        die "RUNBOOK_IMAGE ($IMAGE_REF) names the release repository itself: set it to a local name such as runbook-pilot:local (the default); the update points that name at each release it installs"
      fi
      TARGET_TAG=$TO
      TARGET_IMAGE=$IMAGE_REPO:$TO
      is_semver "$TO" && TARGET_VERSION=$TO
      ;;
  esac
  note "target: $TARGET_TAG ($SOURCE$([ "$PINNED" = 1 ] && echo ', pinned by RUNBOOK_UPDATE_PIN')) -> image $TARGET_IMAGE"
  if [ -n "$TARGET_VERSION" ]; then refuse_downgrade; fi
}

refuse_downgrade() {
  case $(semver_cmp "$TARGET_VERSION" "$CURRENT_VERSION") in
    -1) die "$TARGET_VERSION is older than the running $CURRENT_VERSION: downgrades are refused (roll back with --rollback instead)" ;;
    0)
      say "Already at $CURRENT_VERSION: nothing to do"
      exit 0
      ;;
  esac
}

check_disk() {
  local db_bytes=0 need_kb free_kb probe=$BACKUP_DIR
  if [ "$HAS_BUNDLED_DB" = 1 ]; then
    # shellcheck disable=SC2016 # expanded by the container's shell
    db_bytes=$(dc exec -T postgres sh -c 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atq -c "SELECT pg_database_size(current_database())"' 2>/dev/null || echo 0)
  fi
  while [ ! -d "$probe" ]; do probe=$(dirname "$probe"); done
  # The dump is usually smaller than the database; allow twice its size
  # plus 1 GiB for the build's layers.
  need_kb=$((db_bytes * 2 / 1024 + 1048576))
  free_kb=$(df -Pk "$probe" | awk 'NR==2 {print $4}')
  note "database $((db_bytes / 1048576)) MiB; free $((free_kb / 1024)) MiB at $BACKUP_DIR (needs $((need_kb / 1024)) MiB)"
  [ "$free_kb" -ge "$need_kb" ] || die "not enough free disk space at $BACKUP_DIR"
}

print_plan() {
  say "Plan"
  note "1. obtain $TARGET_TAG: $([ "$SOURCE" = git ] && echo "git checkout $TARGET_TAG (from $GIT_PREVIOUS_REF), docker compose build" || echo "docker pull $TARGET_IMAGE")"
  note "2. check the target against the database (its runbook-server doctor); refuse an incompatible schema"
  if [ "$SKIP_BACKUP" = 1 ]; then
    note "3. backup: skipped (--skip-backup)"
  else
    note "3. backup: pg_dump to $BACKUP_DIR/runbook-<release>-<UTC time>.dump (0600)"
  fi
  note "4. keep the running image as $(image_repo "$IMAGE_REF"):rollback-$CURRENT_VERSION; restart the server on $TARGET_IMAGE (migrations run on start)"
  note "5. health: container healthcheck, /v1/meta release and schema, doctor, discovery, web UI, CLI"
  if [ "$NO_ROLLBACK" = 1 ]; then
    note "6. on failure: stop and print how to roll back (--no-rollback)"
  else
    note "6. on failure: roll back to $CURRENT_VERSION's image$([ "$RESTORE_BACKUP" = 1 ] && echo ' and restore the backup (--restore-backup)')"
  fi
  if [ "$SOURCE" = image ] && [ "$INSECURE_SKIP_VERIFY" = 1 ]; then
    warn "--insecure-skip-verify: the image's provenance will NOT be verified"
  fi
  if [ "$CURRENT_VERSION" = unknown ]; then
    warn "the running release predates UPD1: its code refuses a newer schema, so rolling back after the migrations needs --restore-backup"
  fi
}

# --- 2. obtain the target --------------------------------------------------------

obtain_target() {
  say "Obtaining $TARGET_TAG"
  case $SOURCE in
    git)
      git -C "$REPO_DIR" checkout --quiet --detach "$TARGET_TAG"
      GIT_CHECKED_OUT=1
      RUNBOOK_IMAGE=$TARGET_IMAGE dc build --quiet server || target_refused "docker compose build failed"
      ;;
    image) obtain_verified_image ;;
  esac
  local line
  line=$(RUNBOOK_IMAGE=$TARGET_IMAGE dc run --rm --no-deps -T server --version 2>/dev/null | parse_version_line || true)
  [ -n "$line" ] || target_refused "$TARGET_IMAGE does not report its release (runbook-server --version)"
  local version
  read -r version TARGET_SCHEMA TARGET_FLOOR <<<"$line"
  if [ -n "$TARGET_VERSION" ] && [ "$version" != "$TARGET_VERSION" ]; then
    target_refused "$TARGET_IMAGE is release $version, not $TARGET_VERSION"
  fi
  if [ "$SOURCE" = image ] && ! release_matches_request "$TO" "$version"; then
    target_refused "$TARGET_IMAGE is release $version, not one of --to $TO"
  fi
  if [ -n "$VERIFIED_VERSION" ] && [ "$version" != "$VERIFIED_VERSION" ]; then
    target_refused "$TARGET_IMAGE reports release $version but was attested for v$VERIFIED_VERSION"
  fi
  if [ -z "$TARGET_VERSION" ]; then
    TARGET_VERSION=$version
    case $(semver_cmp "$TARGET_VERSION" "$CURRENT_VERSION") in
      -1) target_refused "$TARGET_VERSION is older than the running $CURRENT_VERSION: downgrades are refused" ;;
      0)
        say "Already at $CURRENT_VERSION: nothing to do"
        exit 0
        ;;
    esac
  fi
  note "target image: release $TARGET_VERSION, schema postgres $TARGET_SCHEMA, rollback floor $TARGET_FLOOR"
  if below_floor "$CURRENT_VERSION" "$TARGET_FLOOR"; then
    if [ "$RESTORE_BACKUP" = 1 ] && [ "$SKIP_BACKUP" = 0 ]; then
      note "$CURRENT_VERSION's code is not safe on $TARGET_VERSION's schema (floor $TARGET_FLOOR): a failure restores the backup (--restore-backup)"
    elif [ "$NO_ROLLBACK" = 1 ]; then
      warn "$CURRENT_VERSION's code is not safe on $TARGET_VERSION's schema (floor $TARGET_FLOOR): a failed update cannot be rolled back automatically (--no-rollback)"
    else
      target_refused "$CURRENT_VERSION is below $TARGET_VERSION's rollback floor ${TARGET_FLOOR/-/(none; $CURRENT_VERSION predates UPD1)}: its code is not safe on the new schema, so a failed update could not just restart it. Pass --restore-backup (a failure restores the backup taken now) or --no-rollback"
    fi
  fi

  # The target's own doctor against the database, read-only.
  local doctor status=0
  doctor=$(RUNBOOK_IMAGE=$TARGET_IMAGE dc run --rm --no-deps -T server doctor 2>&1) || status=$?
  if [ "$status" = 0 ]; then
    note "target doctor: schema current, no migrations to apply"
  elif grep -q 'postgres.schema_out_of_date' <<<"$doctor"; then
    note "target doctor: migrations to apply (schema ${CURRENT_SCHEMA} -> $TARGET_SCHEMA)"
  elif grep -q 'postgres.schema_newer_than_code' <<<"$doctor"; then
    target_refused "the database's schema is newer than $TARGET_VERSION's (a downgrade)"
  elif grep -q 'postgres.migration_history_mismatch' <<<"$doctor"; then
    target_refused "$TARGET_VERSION cannot run on this database: its migration history differs (postgres.migration_history_mismatch)"
  else
    printf '%s\n' "$doctor" | sed 's/^/    target doctor: /' >&2
    target_refused "the target's doctor failed"
  fi
}

# The image source: resolve the tag to a digest, verify that digest's build
# provenance (GitHub attestation by the release workflow), then pull it by
# digest. TARGET_IMAGE becomes REPO@sha256:...; nothing from the image runs
# before this.
obtain_verified_image() {
  local digest
  if [ "${RUNBOOK_UPDATE_TEST_NO_PULL:-}" = 1 ]; then
    # Tests only: a local image (nothing is published), which cannot be verified.
    warn "RUNBOOK_UPDATE_TEST_NO_PULL=1: using the local $TARGET_IMAGE (tests only)"
    docker image inspect "$TARGET_IMAGE" >/dev/null 2>&1 || target_refused "no local $TARGET_IMAGE"
    [ "$INSECURE_SKIP_VERIFY" = 1 ] || target_refused "cannot verify a local image's provenance (--insecure-skip-verify to install it anyway)"
    warn "--insecure-skip-verify: $TARGET_IMAGE is NOT verified"
    return
  fi
  digest=$(docker buildx imagetools inspect "$TARGET_IMAGE" 2>/dev/null | sed -nE 's/^Digest:[[:space:]]+(sha256:[0-9a-f]{64})$/\1/p' | head -n1)
  [ -n "$digest" ] || target_refused "cannot resolve $TARGET_IMAGE to a digest (is that release published? --source git builds one from this checkout)"
  note "$TARGET_IMAGE is $IMAGE_REPO@$digest"
  if [ "$INSECURE_SKIP_VERIFY" = 1 ]; then
    warn "--insecure-skip-verify: $IMAGE_REPO@$digest is NOT verified"
  elif ! command -v gh >/dev/null 2>&1; then
    target_refused "verifying the image needs the GitHub CLI (gh attestation verify); install gh, or pass --insecure-skip-verify"
  else
    # The release the digest claims to be (registry metadata; nothing runs),
    # which the attestation must have been made for.
    local claimed=${TARGET_VERSION:-}
    [ -n "$claimed" ] || claimed=$(image_version_label "$IMAGE_REPO@$digest")
    is_semver "${claimed:-}" || target_refused "$IMAGE_REPO@$digest names no release (label org.opencontainers.image.version)"
    release_matches_request "$TO" "$claimed" || target_refused "$TARGET_IMAGE is release $claimed, not one of --to $TO"
    verify_image_attestation "$IMAGE_REPO" "$digest" "$claimed" ||
      target_refused "$IMAGE_REPO@$digest has no valid build attestation from $ATTESTATION_REPO/.github/workflows/release.yml at refs/tags/v$claimed"
    note "provenance: attested by $ATTESTATION_REPO/.github/workflows/release.yml at refs/tags/v$claimed"
    VERIFIED=yes
    VERIFIED_VERSION=$claimed
  fi
  TARGET_IMAGE=$IMAGE_REPO@$digest
  docker pull --quiet "$TARGET_IMAGE" >/dev/null || target_refused "docker pull $TARGET_IMAGE failed"
}

# UPD1 re-check #1: verify DIGEST of REPO was attested by the release
# workflow of ATTESTATION_REPO for the tag vVERSION, not merely by any
# workflow of that repository (any workflow can attest, and a tag on
# another commit runs that commit's release.yml). gh's own message is shown
# on failure, so an unauthenticated gh is not mistaken for a bad attestation.
verify_image_attestation() {
  local repo=$1 digest=$2 version=$3 out
  if ! out=$(gh attestation verify "oci://$repo@$digest" --repo "$ATTESTATION_REPO" \
    --signer-workflow "$ATTESTATION_REPO/.github/workflows/release.yml" \
    --source-ref "refs/tags/v$version" 2>&1 >/dev/null); then
    printf '%s\n' "$out" | sed 's/^/    gh: /' >&2
    return 1
  fi
}

# Whether release VERSION is what --to REQUEST asked for: latest takes any,
# X.Y only an X.Y.* release, X.Y.Z only itself (UPD1-31: a moved X.Y tag
# must not hand over another line).
release_matches_request() {
  case $1 in
    latest) return 0 ;;
    *.*.*) [ "$2" = "$1" ] ;;
    *) case $2 in "$1".*) return 0 ;; *) return 1 ;; esac ;;
  esac
}

# The release an image says it is: its org.opencontainers.image.version
# label, read from the registry's metadata (nothing is pulled or run).
image_version_label() {
  docker buildx imagetools inspect "$1" --format '{{json .Image}}' 2>/dev/null |
    grep -oE '"org\.opencontainers\.image\.version":"[^"]*"' | head -n1 | cut -d'"' -f4
}

# Nothing has stopped yet: put the checkout back and refuse.
target_refused() {
  if [ "$GIT_CHECKED_OUT" = 1 ]; then
    git -C "$REPO_DIR" checkout --quiet "$GIT_PREVIOUS_REF" || warn "could not check $GIT_PREVIOUS_REF out again"
  fi
  die "$1 (nothing was changed; the server still runs $CURRENT_VERSION)"
}

# --- 3. backup -------------------------------------------------------------------

backup() {
  if [ "$SKIP_BACKUP" = 1 ]; then
    say "Backup skipped (--skip-backup)"
    return
  fi
  say "Backup"
  BACKUP_FILE=$BACKUP_DIR/runbook-$CURRENT_VERSION-$(date -u +%Y%m%dT%H%M%SZ).dump
  dump_database "$BACKUP_FILE" || target_refused "the backup failed"
  note "database (with its blobs and uploads): $BACKUP_FILE ($(du -k "$BACKUP_FILE" | awk '{print $1}') KiB, 0600, read back in full)"
  note "keep RUNBOOK_WEBHOOK_SECRET_KEY with it: restored webhook secrets need it"
}

# The bundled database's size in bytes (0 when it cannot be read).
database_bytes() {
  # shellcheck disable=SC2016 # expanded by the container's shell
  dc exec -T postgres sh -c 'psql -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Atq -c "SELECT pg_database_size(current_database())"' 2>/dev/null || echo 0
}

# Dump the bundled database to FILE (0600) and read it back in full. Checks
# for room first; removes FILE and fails on any problem.
dump_database() {
  local file=$1 bytes need_kb free_kb out
  bytes=$(database_bytes)
  need_kb=$((bytes / 1024 + 65536))
  free_kb=$(df -Pk "$(dirname "$file")" | awk 'NR==2 {print $4}')
  if [ "$free_kb" -lt "$need_kb" ]; then
    warn "not enough room for a dump at $(dirname "$file"): $((free_kb / 1024)) MiB free, $((need_kb / 1024)) MiB needed"
    return 1
  fi
  if ! (
    umask 077
    # shellcheck disable=SC2016 # expanded by the container's shell
    dc exec -T postgres sh -c 'pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" --format=custom' >"$file"
  ); then
    rm -f "$file"
    warn "pg_dump failed"
    return 1
  fi
  chmod 600 "$file"
  if [ "${RUNBOOK_UPDATE_TEST_TRUNCATE_BACKUP:-}" = 1 ]; then
    warn "RUNBOOK_UPDATE_TEST_TRUNCATE_BACKUP=1: truncating the dump on purpose (tests only)"
    head -c "$(($(wc -c <"$file") / 2))" "$file" >"$file.cut" && mv "$file.cut" "$file"
  fi
  if ! out=$(verify_dump "$file"); then
    warn "the dump does not read back in full: $out"
    rm -f "$file"
    return 1
  fi
}

# A dump is usable when it is not empty and pg_restore reads all of it
# (every table's data, not only the table of contents).
verify_dump() {
  [ -s "$1" ] || {
    echo "empty"
    return 1
  }
  dc exec -T postgres pg_restore -f /dev/null <"$1" 2>&1 | tail -n 2
  return "${PIPESTATUS[0]}"
}

# --- 4. restart, 5. health ----------------------------------------------------------

# Point the configured image name at IMAGE and recreate the server on it.
# With allow_newer=true (a rollback onto a newer schema) the server gets
# RUNBOOK_ALLOW_NEWER_SCHEMA=true from a compose override file in the
# backup directory, which stays until the next update.
switch_to() {
  local image=$1 allow_newer=$2
  docker tag "$image" "$IMAGE_REF"
  if [ "$allow_newer" = true ]; then
    mkdir -p "$BACKUP_DIR"
    printf '%s\n' '# deploy/update.sh: the previous release runs on a newer schema (expand/contract).' \
      '# Remove after the next update; deploy/update.sh does.' \
      'services:' '  server:' '    environment:' '      RUNBOOK_ALLOW_NEWER_SCHEMA: "true"' >"$ALLOW_FILE"
    DC_OVERRIDE=$ALLOW_FILE
  else
    DC_OVERRIDE=
  fi
  dc up -d --no-build --no-deps --force-recreate \
    --wait --wait-timeout "$HEALTH_TIMEOUT" server >/dev/null 2>&1
}

health() {
  local meta version schema doctor
  if [ "${RUNBOOK_UPDATE_TEST_FAIL_HEALTH:-}" = 1 ]; then
    warn "RUNBOOK_UPDATE_TEST_FAIL_HEALTH=1: failing the health check on purpose (tests only)"
    return 1
  fi
  wait_ready 60 || return 1
  note "ready: /health/ready (or /v1/meta on a release without it)"
  meta=$(server_get /v1/meta) || {
    warn "/v1/meta does not answer"
    return 1
  }
  version=$(printf '%s' "$meta" | json_field version)
  schema=$(printf '%s' "$meta" | json_field postgres)
  [ "$version" = "$TARGET_VERSION" ] || {
    warn "/v1/meta reports release ${version:-none}, expected $TARGET_VERSION"
    return 1
  }
  [ "$schema" = "$TARGET_SCHEMA" ] || {
    warn "/v1/meta reports schema postgres ${schema:-none}, expected $TARGET_SCHEMA"
    return 1
  }
  note "/v1/meta: release $version, schema postgres $schema"
  doctor=$(dc exec -T server runbook-server doctor 2>&1) || {
    printf '%s\n' "$doctor" | sed 's/^/    doctor: /' >&2
    return 1
  }
  grep -q "schema: current ($TARGET_SCHEMA)" <<<"$doctor" || {
    warn "doctor does not report schema $TARGET_SCHEMA as current"
    return 1
  }
  note "doctor: database ok, schema current ($TARGET_SCHEMA)"
  server_get /.well-known/runbook >/dev/null || {
    warn "discovery (/.well-known/runbook) does not answer"
    return 1
  }
  note "discovery: ok"
  # shellcheck disable=SC2016 # expanded by the container's shell
  if dc exec -T server sh -c '[ -n "${RUNBOOK_WEB_DIR:-}" ]'; then
    server_get / >/dev/null || {
      warn "the web UI (/) does not answer"
      return 1
    }
    note "web UI: ok"
  fi
  dc exec -T server runbook --version | grep -q " $TARGET_VERSION\$" || {
    warn "the image's CLI is not release $TARGET_VERSION"
    return 1
  }
  note "CLI: runbook $TARGET_VERSION"
}

# --- 6. rollback ----------------------------------------------------------------------

rollback() {
  say "Rolling back to $CURRENT_VERSION"
  if [ "$GIT_PREVIOUS_REF" != "" ] && [ -n "$REPO_DIR" ]; then
    git -C "$REPO_DIR" checkout --quiet "$GIT_PREVIOUS_REF" || warn "could not check $GIT_PREVIOUS_REF out again"
    note "checkout: $GIT_PREVIOUS_REF"
  fi
  if [ "$RESTORE_BACKUP" = 1 ]; then
    if restore_backup; then
      switch_to "$ROLLBACK_IMAGE" false || return 1
      rm -f "$ALLOW_FILE"
      note "server: $CURRENT_VERSION on the restored database"
      return 0
    fi
    if below_floor "$CURRENT_VERSION" "$TARGET_FLOOR"; then
      warn "the restore failed and $CURRENT_VERSION's code is not safe on the migrated schema: the server stays stopped"
      return 1
    fi
    warn "the restore failed (the database is as the update left it): starting $CURRENT_VERSION on it instead"
    RESTORE_FAILED=1
  fi
  local schema allow=false
  schema=$(database_schema)
  if [ -n "$schema" ] && [ "$CURRENT_SCHEMA" != - ] && [ "$schema" -gt "$CURRENT_SCHEMA" ]; then
    allow=true
    note "the schema moved to $schema; $CURRENT_VERSION runs on it with RUNBOOK_ALLOW_NEWER_SCHEMA=true (expand/contract)"
  elif [ -z "$schema" ] && [ "$CURRENT_SCHEMA" != - ] && [ -n "$TARGET_SCHEMA" ] && [ "$TARGET_SCHEMA" -gt "$CURRENT_SCHEMA" ]; then
    # No bundled database to ask (an external one): assume the target's
    # migrations ran. The override only allows a newer schema.
    allow=true
    note "the target migrates to schema $TARGET_SCHEMA; $CURRENT_VERSION runs with RUNBOOK_ALLOW_NEWER_SCHEMA=true (expand/contract)"
  fi
  switch_to "$ROLLBACK_IMAGE" "$allow" || return 1
  note "server: $CURRENT_VERSION again ($ROLLBACK_IMAGE)"
  if [ "$allow" = true ]; then
    warn "until the next update, start the stack with '-f $ALLOW_FILE' after the compose file(s) (it sets RUNBOOK_ALLOW_NEWER_SCHEMA=true); without it the server refuses the newer schema (postgres.schema_newer_than_code)"
  fi
  [ "$RESTORE_FAILED" = 0 ]
}

# Restore BACKUP_FILE without ever dropping the live database first: read
# the backup in full, stop the server, dump the current database (kept),
# restore into a scratch database with the live one's owner and access, then
# swap the two by rename in one transaction. The replaced database is kept
# under its new name; a failure before the swap leaves everything as it was.
restore_backup() {
  if [ -z "$BACKUP_FILE" ] || [ ! -f "$BACKUP_FILE" ]; then
    warn "no backup to restore"
    return 1
  fi
  [ "$HAS_BUNDLED_DB" = 1 ] || {
    warn "--restore-backup restores only the bundled database"
    return 1
  }
  local out stamp pre
  if ! out=$(verify_dump "$BACKUP_FILE"); then
    warn "$BACKUP_FILE does not read back in full ($out): not restoring it"
    return 1
  fi
  restore_preconditions || return 1
  note "restoring $BACKUP_FILE (everything written after it is lost)"
  # A server still writing would make the swap lose its writes: stop or give up.
  if ! dc stop --timeout 10 server >/dev/null 2>&1; then
    warn "could not stop the server: not restoring"
    return 1
  fi
  stamp=$(date -u +%Y%m%d%H%M%S)
  pre=$BACKUP_DIR/runbook-$TARGET_VERSION-pre-restore-$(date -u +%Y%m%dT%H%M%SZ).dump
  dump_database "$pre" || {
    warn "could not dump the current database first: not restoring"
    return 1
  }
  note "the database as it was: $pre"
  # shellcheck disable=SC2016 # expanded by the container's shell
  local psql_admin='psql -U "$POSTGRES_USER" -d postgres -v ON_ERROR_STOP=1 -q -v db="$POSTGRES_DB" -v scratch="${POSTGRES_DB}_restore_$STAMP" -v replaced="${POSTGRES_DB}_replaced_$STAMP"'
  if ! dc exec -T -e STAMP="$stamp" postgres sh -c "$psql_admin" >/dev/null <<'SQL'
CREATE DATABASE :"scratch" TEMPLATE template0;
SELECT format('ALTER DATABASE %I OWNER TO %I', :'scratch', pg_get_userbyid(datdba)) FROM pg_database WHERE datname = :'db' \gexec
SELECT format('REVOKE ALL ON DATABASE %I FROM PUBLIC', :'scratch') FROM pg_database WHERE datname = :'db' AND datacl IS NOT NULL \gexec
SELECT format('GRANT %s ON DATABASE %I TO %s', a.privilege_type, :'scratch',
              CASE WHEN a.grantee = 0 THEN 'PUBLIC' ELSE quote_ident(pg_get_userbyid(a.grantee)) END)
  FROM pg_database d, aclexplode(d.datacl) a WHERE d.datname = :'db' \gexec
SELECT CASE WHEN r.setrole = 0
         THEN format('ALTER DATABASE %I SET %I TO %L', :'scratch', split_part(c, '=', 1), substr(c, strpos(c, '=') + 1))
         ELSE format('ALTER ROLE %I IN DATABASE %I SET %I TO %L', pg_get_userbyid(r.setrole), :'scratch',
                     split_part(c, '=', 1), substr(c, strpos(c, '=') + 1)) END
  FROM pg_db_role_setting r JOIN pg_database d ON d.oid = r.setdatabase, unnest(r.setconfig) c
  WHERE d.datname = :'db' \gexec
SQL
  then
    warn "could not create the scratch database: nothing was restored"
    drop_scratch "$stamp"
    return 1
  fi
  # shellcheck disable=SC2016 # expanded by the container's shell
  if ! dc exec -T -e STAMP="$stamp" postgres sh -c 'pg_restore -U "$POSTGRES_USER" -d "${POSTGRES_DB}_restore_$STAMP" --exit-on-error' <"$BACKUP_FILE"; then
    warn "the restore into the scratch database failed: nothing was replaced"
    drop_scratch "$stamp"
    return 1
  fi
  if ! dc exec -T -e STAMP="$stamp" postgres sh -c "$psql_admin" >/dev/null <<'SQL'
SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname IN (:'db', :'scratch') AND pid <> pg_backend_pid();
BEGIN;
ALTER DATABASE :"db" RENAME TO :"replaced";
ALTER DATABASE :"scratch" RENAME TO :"db";
COMMIT;
SQL
  then
    warn "could not swap in the restored database: nothing was replaced"
    drop_scratch "$stamp"
    return 1
  fi
  # shellcheck disable=SC2016 # expanded by the container's shell
  REPLACED_DB=$(dc exec -T -e STAMP="$stamp" postgres sh -c 'printf "%s" "${POSTGRES_DB}_replaced_$STAMP"')
  note "database restored; the replaced one is kept as $REPLACED_DB (drop it when you no longer need it)"
}

# Before anything stops (re-check #4): the bundled database must not be the
# maintenance database (it cannot be swapped by rename), and its data
# directory needs room for a whole second copy, or filling it could take
# the live database down.
restore_preconditions() {
  local db bytes free_kb need_kb
  [ "$HAS_BUNDLED_DB" = 1 ] || {
    warn "--restore-backup restores only the bundled database"
    return 1
  }
  # shellcheck disable=SC2016 # expanded by the container's shell
  db=$(dc exec -T postgres sh -c 'printf "%s" "$POSTGRES_DB"' 2>/dev/null || true)
  if [ -z "$db" ] || [ "$db" = postgres ]; then
    warn "--restore-backup needs POSTGRES_DB to name a database other than 'postgres' (it swaps databases by rename)"
    return 1
  fi
  bytes=$(database_bytes)
  # shellcheck disable=SC2016 # expanded by the container's shell
  free_kb=$(dc exec -T postgres sh -c 'df -Pk "${PGDATA:-/var/lib/postgresql/data}"' 2>/dev/null | awk 'NR==2 {print $4}')
  need_kb=$((bytes * 3 / 2 / 1024 + 262144))
  if [ -z "$free_kb" ] || [ "$free_kb" -lt "$need_kb" ]; then
    warn "not enough room in PostgreSQL's data directory for the restored copy: ${free_kb:-?} KiB free, $need_kb KiB needed"
    return 1
  fi
}

drop_scratch() {
  # shellcheck disable=SC2016 # expanded by the container's shell
  dc exec -T -e STAMP="$1" postgres sh -c \
    'psql -U "$POSTGRES_USER" -d postgres -q -c "DROP DATABASE IF EXISTS \"${POSTGRES_DB}_restore_$STAMP\""' >/dev/null 2>&1 || true
}

manual_rollback_hint() {
  note "to roll back: deploy/update.sh --rollback$([ -n "$PROJECT" ] && printf ' --project %s' "$PROJECT") --backup-dir $BACKUP_DIR"
  note "(add --restore-backup to restore $BACKUP_FILE too; that loses everything written after it)"
}

# --- main ---------------------------------------------------------------------------------

update() {
  preflight
  print_plan
  if [ "$DRY_RUN" = 1 ]; then
    say "Dry run: nothing changed"
    return 0
  fi
  confirm "Update from $CURRENT_VERSION to $TARGET_TAG?" || die "not updated"
  ROLLBACK_IMAGE=$(image_repo "$IMAGE_REF"):rollback-$CURRENT_VERSION
  mkdir -p "$BACKUP_DIR"
  chmod 700 "$BACKUP_DIR"
  docker tag "$(docker inspect -f '{{.Image}}' "$(server_id)")" "$ROLLBACK_IMAGE"
  obtain_target
  backup
  write_record

  say "Restarting on $TARGET_VERSION"
  local ok=1
  if ! switch_to "$TARGET_IMAGE" false; then
    warn "the new server did not become healthy within ${HEALTH_TIMEOUT}s"
    dc logs --no-color --tail 20 server 2>/dev/null | sed 's/^/    log: /' >&2 || true
    ok=0
  fi
  if [ "$ok" = 1 ]; then
    say "Health check"
    health || ok=0
  fi
  if [ "$ok" = 1 ]; then
    prune_rollback_images
    prune_backups
    rm -f "$ALLOW_FILE"
    say "Updated $CURRENT_VERSION -> $TARGET_VERSION"
    if [ "$SOURCE" = image ] && [ "$VERIFIED" != yes ]; then
      warn "$TARGET_IMAGE was NOT verified (--insecure-skip-verify); recorded in $(record_file)"
    fi
    [ -n "$BACKUP_FILE" ] && note "backup: $BACKUP_FILE"
    manual_rollback_hint
    return 0
  fi

  warn "the update to $TARGET_VERSION failed"
  if [ "$NO_ROLLBACK" = 1 ] || ! confirm "Roll back to $CURRENT_VERSION?"; then
    manual_rollback_hint
    exit 4
  fi
  if rollback; then
    say "Rolled back to $CURRENT_VERSION"
    exit 3
  fi
  warn "the rollback failed"
  manual_rollback_hint
  exit 4
}

# Keep the newest KEEP_BACKUPS dumps (0 keeps all); never the one this
# update recorded.
prune_backups() {
  [ "$KEEP_BACKUPS" -gt 0 ] || return 0
  local file n=0
  # shellcheck disable=SC2012 # newest first; the names are ours (runbook-<release>-<time>.dump)
  ls -1t "$BACKUP_DIR"/runbook-*.dump 2>/dev/null | while IFS= read -r file; do
    n=$((n + 1))
    [ "$n" -le "$KEEP_BACKUPS" ] && continue
    [ "$file" = "$BACKUP_FILE" ] && continue
    rm -f "$file" && note "pruned $(basename "$file") (--keep-backups $KEEP_BACKUPS)"
  done
}

# Keep only this update's rollback tag of this image repository (each is
# one release's image); other repositories' are not ours to touch.
prune_rollback_images() {
  local repo ref
  repo=$(image_repo "$IMAGE_REF")
  for ref in $(docker image ls --format '{{.Repository}}:{{.Tag}}' "$repo"); do
    case $ref in
      "$repo":rollback-*) [ "$ref" = "$ROLLBACK_IMAGE" ] || docker image rm "$ref" >/dev/null 2>&1 || true ;;
    esac
  done
}

# Show the backup's age; older than a day, require its ID typed (or given
# with --confirm-backup), even under --yes: restoring loses everything since.
# The epoch seconds of the UTC time a backup ID ends with
# (...-YYYYMMDDTHHMMSSZ); fails for an ID without one.
backup_epoch() {
  printf '%s' "$1" | sed -nE 's/.*-([0-9]{8})T([0-9]{6})Z$/\1 \2/p' |
    awk 'NF == 2 {
      y = substr($1, 1, 4) + 0; m = substr($1, 5, 2) + 0; d = substr($1, 7, 2) + 0
      h = substr($2, 1, 2) + 0; mi = substr($2, 3, 2) + 0; s = substr($2, 5, 2) + 0
      if (m <= 2) { y -= 1 }
      era = int(y / 400); yoe = y - era * 400
      mp = (m + 9) % 12; doy = int((153 * mp + 2) / 5) + d - 1
      doe = yoe * 365 + int(yoe / 4) - int(yoe / 100) + doy
      days = era * 146097 + doe - 719468
      printf "%d\n", ((days * 24 + h) * 60 + mi) * 60 + s; found = 1
    } END { exit found ? 0 : 1 }'
}

confirm_backup_age() {
  local id age_h answer taken
  id=$(basename "$BACKUP_FILE" .dump)
  # From the time in its ID (re-check, low): a copy or a touch changes the
  # mtime, not when the data was taken. No time in the ID counts as old.
  if taken=$(backup_epoch "$id"); then
    age_h=$((($(date +%s) - taken) / 3600))
  else
    age_h=99999
  fi
  note "restoring backup $id, taken ${age_h}h ago (everything written since is lost)"
  [ "$age_h" -lt 24 ] && return 0
  [ "$CONFIRM_BACKUP" = "$id" ] && return 0
  if [ -t 0 ]; then
    printf 'The backup is %s hours old. Type its ID (%s) to restore it: ' "$age_h" "$id"
    read -r answer || answer=
    [ "$answer" = "$id" ] && return 0
  fi
  die "the backup is ${age_h}h old: confirm with --confirm-backup $id"
}

rollback_last() {
  read_record
  local id
  id=$(server_id)
  [ -n "$id" ] && IMAGE_REF=$(docker inspect -f '{{.Config.Image}}' "$id")
  [ -n "$(dc ps -q postgres 2>/dev/null)" ] && HAS_BUNDLED_DB=1
  docker image inspect "$ROLLBACK_IMAGE" >/dev/null 2>&1 || die "the rollback image $ROLLBACK_IMAGE is gone"
  if [ "$SOURCE" = git ]; then
    REPO_DIR=$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)
  fi
  say "Rolling back $TARGET_VERSION -> $CURRENT_VERSION"
  if below_floor "$CURRENT_VERSION" "$TARGET_FLOOR" && [ "$RESTORE_BACKUP" = 0 ]; then
    die "$CURRENT_VERSION is below $TARGET_VERSION's rollback floor ($TARGET_FLOOR): its code is not safe on this schema. Roll back with --restore-backup"
  fi
  if [ "$RESTORE_BACKUP" = 1 ]; then
    [ -f "$BACKUP_FILE" ] || die "the recorded backup $BACKUP_FILE is gone"
    restore_preconditions || die "cannot restore here (see above); nothing was changed"
    confirm_backup_age
  fi
  if [ "$DRY_RUN" = 1 ]; then
    say "Dry run: nothing changed"
    return 0
  fi
  confirm "Roll back to $CURRENT_VERSION?" || die "not rolled back"
  rollback || die "the rollback failed" 4
  say "Rolled back to $CURRENT_VERSION"
}

main() {
  parse_args "$@"
  if [ "$ROLLBACK_ONLY" = 1 ]; then
    rollback_last
  else
    update
  fi
}

# The whole file is read before main runs, so checking out another release
# (step 2) cannot change the script under a running update.
# RUNBOOK_UPDATE_LIB=1 loads the functions only (scripts/tests/update-sh.test.mjs).
if [ "${RUNBOOK_UPDATE_LIB:-}" != 1 ]; then
  main "$@"
  exit $?
fi
