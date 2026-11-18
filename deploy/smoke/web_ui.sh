#!/usr/bin/env bash
# The image serves the built web UI on the API's origin (B2, I1): `/` and a
# client route are index.html with the CSP (GET and HEAD); a hashed asset is
# immutable; the browser compiler, once the bundle loads it, is
# application/wasm; API paths keep their problem+json 404; traversal is
# refused; no response carries CORS headers.
source "$(dirname "$0")/lib.sh"

CSP="default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'"
WORK=$(mktemp -d)
remove_work() { rm -rf "$WORK"; }
SMOKE_EXTRA_CLEANUP=remove_work

ensure_image
up
port=$(compose port server 8080 | sed 's/.*://')
base="http://127.0.0.1:${port}"
log "stack healthy on port $port"

# fetch <name> <method> <path>: body in $WORK/<name>.body, headers in
# $WORK/<name>.headers; prints the status. (curl reads no body after HEAD.)
fetch() {
  local flags=(-X "$2" -o "$WORK/$1.body")
  [ "$2" = HEAD ] && flags=(--head -o /dev/null)
  curl -sS --path-as-is --max-time 5 "${flags[@]}" -D "$WORK/$1.headers" -w '%{http_code}' "$base$3"
}
# header <name> <header>: the value, without the trailing CR.
header() { grep -i "^$2:" "$WORK/$1.headers" | head -1 | cut -d: -f2- | sed 's/^ //' | tr -d '\r'; }
expect() { # expect <name> <header> <value>
  local got
  got=$(header "$1" "$2")
  [ "$got" = "$3" ] || fail "$1: $2 is '$got', expected '$3'"
}
no_cors() {
  ! grep -qi '^access-control-' "$WORK/$1.headers" || fail "$1 sent Access-Control-* headers"
  expect "$1" x-content-type-options nosniff
}

[ "$(fetch root GET /)" = 200 ] || fail "GET / is not 200"
expect root content-type "text/html; charset=utf-8"
expect root content-security-policy "$CSP"
expect root referrer-policy no-referrer
expect root cache-control no-store
grep -q '<div id="root">' "$WORK/root.body" || fail "GET / is not the app's index.html"
no_cors root
log "GET / is index.html with the CSP, no-referrer, no-store"

[ "$(fetch tasks GET /tasks)" = 200 ] || fail "GET /tasks is not 200"
cmp -s "$WORK/root.body" "$WORK/tasks.body" || fail "GET /tasks is not index.html"
expect tasks content-security-policy "$CSP"
no_cors tasks
[ "$(fetch head HEAD /tasks)" = 200 ] || fail "HEAD /tasks is not 200"
expect head content-security-policy "$CSP"
expect head content-length "$(wc -c <"$WORK/root.body" | tr -d ' ')"
log "client route /tasks (GET and HEAD) is index.html"

script=$(grep -o 'src="/assets/[^"]*\.js"' "$WORK/root.body" | head -1 | sed 's/^src="//; s/"$//')
[ -n "$script" ] || fail "index.html references no /assets/*.js"
[ "$(fetch script GET "$script")" = 200 ] || fail "GET $script is not 200"
expect script cache-control "public, max-age=31536000, immutable"
expect script content-type "text/javascript; charset=utf-8"
no_cors script
log "GET $script is immutable-cached JavaScript"

# The browser compiler: every .wasm the image ships or a bundle references
# (assets/runbook-<hash>.wasm once the editor loads it) is served as
# application/wasm, immutable. Vite emits it only when the app imports it.
# Shipped .wasm files, plus hashed asset names the bundles reference (Vite's
# `name-<8 char hash>.wasm`); plain names inside messages are not requests.
wasm_names=$( {
  compose exec -T server sh -c 'cd /usr/share/runbook/web/assets && ls' 2>/dev/null |
    grep -E '\.wasm$'
  compose exec -T server sh -c 'cat /usr/share/runbook/web/assets/*.js' 2>/dev/null |
    grep -oE '[A-Za-z0-9_.]+-[A-Za-z0-9_-]{8}\.wasm'
} | sort -u || true)
if [ -z "$wasm_names" ]; then
  log "SKIP wasm: the bundle neither ships nor references a .wasm module yet"
fi
for wasm in $wasm_names; do
  [ "$(fetch wasm GET "/assets/$wasm")" = 200 ] || fail "GET /assets/$wasm is not 200"
  expect wasm content-type application/wasm
  expect wasm cache-control "public, max-age=31536000, immutable"
  [ "$(head -c 4 "$WORK/wasm.body" | od -An -tx1 | tr -d ' \n')" = 0061736d ] || fail "$wasm is not a wasm module"
  no_cors wasm
  log "GET /assets/$wasm is application/wasm ($(wc -c <"$WORK/wasm.body" | tr -d ' ') bytes)"
done

for path in /v1/does-not-exist /assets/index-missing.js /assets/../../etc/passwd /%2e%2e/etc/passwd; do
  [ "$(fetch api GET "$path")" = 404 ] || fail "GET $path is not 404"
  expect api content-type application/problem+json
  ! grep -q 'root:' "$WORK/api.body" || fail "GET $path leaked a file"
  no_cors api
done
log "API, missing-asset and traversal paths are problem+json 404s"

host_meta | grep -q '"edition":"self_hosted"' || fail "/v1/meta not served"
pass
