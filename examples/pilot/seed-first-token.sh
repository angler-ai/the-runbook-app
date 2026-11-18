#!/usr/bin/env bash
# INTERIM (pilot only): give one person their first personal access token.
#
#   examples/pilot/seed-first-token.sh <name> <org_id> <user_id> <project_id>
#
# Generates a fresh `trb_pat_` secret, inserts only its SHA-256 through
# seed-first-token.sql, and writes the secret to $PILOT_DIR/<name>.seed
# (mode 0600). The secret is never printed or passed on a command line.
# If $PILOT_DIR/<name>.seed already exists, nothing happens.
#
# Environment:
#   PILOT_DIR     directory for secrets (required; created 0700)
#   PILOT_PSQL    psql command to run, split on spaces, for the compose stack:
#                 "docker compose -f deploy/docker-compose.yml --env-file deploy/.env exec -T postgres psql -U runbook -d runbook"
#                 It must not carry a URL or password (refused): anything on a
#                 command line is visible to other users through `ps`.
#   DATABASE_URL  used when PILOT_PSQL is unset: parsed here into PGHOST,
#                 PGPORT, PGUSER, PGPASSWORD, PGDATABASE (and PGSSLMODE etc.
#                 from its query) in psql's environment, never its arguments
#
# Why: the pilot has no login flow that issues a person's first token
# (device login is deferred) and the token API only issues tokens to the
# caller. See docs/install.md, section 4.
set -euo pipefail

usage() {
  echo "usage: PILOT_DIR=<dir> $0 <name> <org_id> <user_id> <project_id>" >&2
  exit 2
}
[ "$#" -eq 4 ] || usage
name=$1 org_id=$2 user_id=$3 project_id=$4
ulid='[0-7][0-9A-HJKMNP-TV-Z]{25}'
[[ "$name" =~ ^[a-z][a-z0-9-]{0,31}$ ]] || { echo "invalid name: $name" >&2; exit 2; }
[[ "$org_id" =~ ^org_${ulid}$ ]] || { echo "invalid organization ID" >&2; exit 2; }
[[ "$user_id" =~ ^usr_${ulid}$ ]] || { echo "invalid user ID" >&2; exit 2; }
[[ "$project_id" =~ ^prj_${ulid}$ ]] || { echo "invalid project ID" >&2; exit 2; }
: "${PILOT_DIR:?set PILOT_DIR to the directory that holds the pilot secrets}"

here=$(cd "$(dirname "$0")" && pwd)
umask 077
mkdir -p "$PILOT_DIR"
chmod 700 "$PILOT_DIR"
target="$PILOT_DIR/$name.seed"
if [ -e "$target" ]; then
  echo "$name already has a seed token in $target; nothing to do"
  exit 0
fi

if [ -n "${PILOT_PSQL:-}" ]; then
  case "$PILOT_PSQL" in
    *://* | *password=* | *PGPASSWORD*)
      echo "PILOT_PSQL must not contain a URL or password (ps shows command lines); set DATABASE_URL instead" >&2
      exit 2
      ;;
  esac
  read -r -a psql_cmd <<< "$PILOT_PSQL"
else
  : "${DATABASE_URL:?set PILOT_PSQL or DATABASE_URL}"
  # libpq settings from the URL, into this process's environment only.
  to_env='
import os, shlex, sys
from urllib.parse import parse_qsl, unquote, urlsplit
url = urlsplit(os.environ["DATABASE_URL"])
if url.scheme not in ("postgres", "postgresql"):
    sys.exit("DATABASE_URL must be a postgres:// URL")
pairs = {"PGHOST": url.hostname, "PGPORT": url.port,
         "PGUSER": unquote(url.username) if url.username else None,
         "PGPASSWORD": unquote(url.password) if url.password else None,
         "PGDATABASE": unquote(url.path.lstrip("/")) or None}
names = {"sslmode": "PGSSLMODE", "sslrootcert": "PGSSLROOTCERT", "sslcert": "PGSSLCERT",
         "sslkey": "PGSSLKEY", "connect_timeout": "PGCONNECT_TIMEOUT",
         "application_name": "PGAPPNAME", "options": "PGOPTIONS"}
for key, value in parse_qsl(url.query):
    if key in names:
        pairs[names[key]] = value
for name, value in pairs.items():
    if value is not None and value != "":
        print("export %s=%s" % (name, shlex.quote(str(value))))
'
  pg_env=$(python3 -I -c "$to_env")
  eval "$pg_env"
  unset pg_env DATABASE_URL
  psql_cmd=(psql)
fi

pending="$target.pending"
rm -f "$pending"
trap 'rm -f "$pending"' EXIT
# The secret goes straight to a 0600 file; only its hash and a new token ID
# come back on stdout.
generate='
import base64, hashlib, os, secrets, sys, time
path = sys.argv[1]
secret = "trb_pat_" + base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
with os.fdopen(fd, "w") as f:
    f.write(secret + "\n")
alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
value = (int(time.time() * 1000) << 80) | int.from_bytes(secrets.token_bytes(10), "big")
print("tok_" + "".join(alphabet[(value >> (5 * i)) & 31] for i in reversed(range(26))),
      hashlib.sha256(secret.encode()).hexdigest())
'
generated=$(python3 -I -c "$generate" "$pending")
read -r token_id hash_hex <<< "$generated"

inserted=$("${psql_cmd[@]}" -X -q -t -A -v ON_ERROR_STOP=1 \
  -v token_id="$token_id" -v org_id="$org_id" -v user_id="$user_id" \
  -v project_id="$project_id" -v hash_hex="$hash_hex" \
  < "$here/seed-first-token.sql" | tr -d '[:space:]')
if [ "$inserted" != "$token_id" ]; then
  echo "no seed inserted: $user_id is not an active user of the organization that owns $project_id" >&2
  exit 1
fi
mv "$pending" "$target"
trap - EXIT
echo "seeded $name: token $token_id (secret in $target)"
