#!/usr/bin/env bash
# AUTH5: sign in by email in the container. Bootstrap the owner by email (no
# token printed), request a code, read it from Mailpit's local HTTP API (the
# smoke compose profile), sign in through /api/auth, and call /v1/me with the
# session cookie; a cookie write without X-Runbook-CSRF is refused (D46).
# Cookies and codes are never printed.
source "$(dirname "$0")/lib.sh"

OWNER=owner@example.test
export COMPOSE_PROFILES=smoke RUNBOOK_MAILPIT_PORT=0
RUNBOOK_AUTH_SECRET=$(openssl rand -hex 32)
export RUNBOOK_AUTH_SECRET RUNBOOK_AUTH_METHODS=email RUNBOOK_AUTH_ALLOWED_DOMAINS=example.test
export RUNBOOK_SMTP_URL=smtp://mailpit:1025 RUNBOOK_SMTP_FROM='The Runbook <login@example.test>'

scratch=$(mktemp -d)
remove_scratch() { rm -rf "$scratch"; }
SMOKE_EXTRA_CLEANUP=remove_scratch

ensure_image
up
log "stack healthy (server, postgres, mailpit)"

server_port=$(compose port server 8080 | sed 's/.*://')
mail_port=$(compose port mailpit 8025 | sed 's/.*://')
base="http://127.0.0.1:${server_port}"
mail="http://127.0.0.1:${mail_port}"

# request <method> <path> [json] [header...]: the body goes to $scratch/body,
# the headers to $scratch/headers; prints the status code. Origin is always
# the configured RUNBOOK_ORIGIN, as a browser on it would send.
request() {
  local method=$1 path=$2 data=${3:-}
  shift 3 || shift $#
  local args=(-sS -o "$scratch/body" -D "$scratch/headers" -w '%{http_code}' --max-time 20
    -X "$method" -H "Origin: $RUNBOOK_ORIGIN")
  [ -n "$data" ] && args+=(-H 'Content-Type: application/json' --data "$data")
  for header in "$@"; do args+=(-H "$header"); done
  curl "${args[@]}" "$base$path"
}

output=$(server_admin bootstrap --owner-email "$OWNER" --owner-name Owner) || fail "bootstrap by email failed"
case $output in *trb_*) fail "bootstrap printed a token without --with-token" ;; esac
owner_id=$(printf '%s' "$output" | json_get user_id)
org_id=$(printf '%s' "$output" | json_get org_id)
log "bootstrapped the owner by email; no token printed"

deadline=$((SECONDS + 30))
until [ "$(curl -s -o /dev/null -w '%{http_code}' --max-time 3 "$base/api/auth/ok")" = 200 ]; do
  [ $SECONDS -lt $deadline ] || fail "the auth service did not come up"
  sleep 0.5
done
curl -fsS --max-time 3 "$base/.well-known/runbook" | json_get auth.methods | grep -q '"email"' ||
  fail "discovery does not list email"
log "auth service up behind /api/auth; discovery lists email"

status=$(request POST /api/auth/email-otp/send-verification-otp "{\"email\":\"$OWNER\",\"type\":\"sign-in\"}")
[ "$status" = 200 ] || fail "send code: $status $(cat "$scratch/body")"

# The newest message to the owner, from Mailpit's API; the code is the first
# six-digit run of its text.
code=""
deadline=$((SECONDS + 30))
while [ -z "$code" ] && [ $SECONDS -lt $deadline ]; do
  code=$(curl -fsS --max-time 3 "$mail/api/v1/search?query=to:$OWNER" 2>/dev/null | python3 -I -c '
import json, re, sys, urllib.request
messages = json.load(sys.stdin).get("messages") or []
if messages:
    with urllib.request.urlopen(sys.argv[1] + "/api/v1/message/" + messages[0]["ID"], timeout=3) as r:
        found = re.search(r"\b(\d{6})\b", json.load(r)["Text"])
        print(found.group(1) if found else "")
' "$mail" || true)
  [ -n "$code" ] || sleep 0.5
done
[ -n "$code" ] || fail "no sign-in code reached Mailpit"
log "code delivered over SMTP and read from Mailpit's API"

status=$(request POST /api/auth/sign-in/email-otp "{\"email\":\"$OWNER\",\"otp\":\"$code\"}")
[ "$status" = 200 ] || fail "sign in: $status $(cat "$scratch/body")"
cookie=$(python3 -I -c '
import sys
for line in open(sys.argv[1]):
    name, _, value = line.partition(":")
    if name.strip().lower() == "set-cookie" and value.strip().startswith("runbook.session_token="):
        print(value.strip().split(";")[0])
' "$scratch/headers")
[ -n "$cookie" ] || fail "sign-in set no session cookie"
grep -i '^set-cookie: runbook.session_token=' "$scratch/headers" | grep -qi 'httponly' || fail "session cookie is not HttpOnly"
log "signed in; HttpOnly session cookie set"

status=$(request GET /v1/me "" "Cookie: $cookie")
[ "$status" = 200 ] || fail "/v1/me: $status $(cat "$scratch/body")"
[ "$(json_get credential.kind <"$scratch/body")" = session ] || fail "/v1/me is not a session"
[ "$(json_get user.id <"$scratch/body")" = "$owner_id" ] || fail "/v1/me is not the bootstrapped owner"
[ "$(json_get org.role <"$scratch/body")" = owner ] || fail "/v1/me is not an owner"
log "/v1/me: a session for the bootstrapped owner"

status=$(request POST "/v1/orgs/$org_id/teams" '{"slug":"oncall","title":"On call"}' "Cookie: $cookie" "Idempotency-Key: smoke-csrf")
[ "$status" = 403 ] && [ "$(json_get code <"$scratch/body")" = auth.csrf ] ||
  fail "cookie POST without X-Runbook-CSRF: $status $(cat "$scratch/body")"
log "cookie POST without X-Runbook-CSRF: 403 auth.csrf"
pass
