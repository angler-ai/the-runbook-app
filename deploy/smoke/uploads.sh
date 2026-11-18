#!/usr/bin/env bash
# UP2: evidence uploads in the container (D49–D51). Turn uploads on with a
# signing key and a user-content origin, claim a human step that needs a
# screenshot, and upload through the user-content listener (port 8081):
# - a GPS-tagged JPEG, generated here (no download), comes back from its
#   signed URL re-encoded without EXIF, with the user-content safety headers
#   and a thumbnail;
# - a mislabeled file (text declared as PNG) is refused at finalize;
# - the API port does not take the upload PUT, and the user-content port
#   answers nothing but /u/*.
# The token reaches curl through a header file, never a command line.
source "$(dirname "$0")/lib.sh"

export RUNBOOK_USERCONTENT_ORIGIN=http://localhost:8081
RUNBOOK_UPLOAD_SIGNING_KEY=$(openssl rand -base64 32)
export RUNBOOK_UPLOAD_SIGNING_KEY

scratch=$(mktemp -d)
remove_scratch() { rm -rf "$scratch"; }
SMOKE_EXTRA_CLEANUP=remove_scratch

# A 64x48 gray baseline JPEG carrying an EXIF block whose GPS IFD holds a
# latitude ("N", 37° 25' 19.32"), built byte by byte.
python3 -I - "$scratch/gps.jpg" <<'PY'
import struct, sys
def seg(marker, payload):
    return bytes([0xFF, marker]) + struct.pack(">H", len(payload) + 2) + payload
t = b"II*\x00" + struct.pack("<I", 8)
t += struct.pack("<H", 1) + struct.pack("<HHII", 0x8825, 4, 1, 26) + struct.pack("<I", 0)
t += struct.pack("<H", 2)
t += struct.pack("<HHI", 1, 2, 2) + b"N\x00\x00\x00"
t += struct.pack("<HHII", 2, 5, 3, 56) + struct.pack("<I", 0)
t += struct.pack("<6I", 37, 1, 25, 1, 1932, 100)
width, height = 64, 48
# Every sample is 128, so each 8x8 block is "DC difference 0, end of block":
# two one-bit Huffman codes.
blocks = ((width + 7) // 8) * ((height + 7) // 8)
bits = "00" * blocks
bits += "1" * (-len(bits) % 8)
data = bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))
jpeg = (b"\xff\xd8" + seg(0xE1, b"Exif\x00\x00" + t)
        + seg(0xDB, b"\x00" + bytes([1] * 64))
        + seg(0xC0, struct.pack(">BHHB", 8, height, width, 1) + b"\x01\x11\x00")
        + seg(0xC4, b"\x00" + bytes([1] + [0] * 15) + b"\x00")
        + seg(0xC4, b"\x10" + bytes([1] + [0] * 15) + b"\x00")
        + seg(0xDA, b"\x01\x01\x00\x00\x3f\x00") + data + b"\xff\xd9")
open(sys.argv[1], "wb").write(jpeg)
PY
grep -q Exif "$scratch/gps.jpg" || fail "the fixture has no EXIF"
printf 'not an image\n' >"$scratch/fake.png"

ensure_image
up
bootstrap_owner
(umask 077 && printf 'Authorization: Bearer %s\n' "$RUNBOOK_TOKEN" >"$scratch/auth")
api="http://127.0.0.1:$(compose port server 8080 | sed 's/.*://')"
files="http://127.0.0.1:$(compose port server 8081 | sed 's/.*://')"
log "stack healthy; API and user-content listener published"

# request <method> <url> [json]: the body goes to $scratch/body, headers to
# $scratch/headers; prints the status code.
request() {
  local args=(-sS -o "$scratch/body" -D "$scratch/headers" -w '%{http_code}' --max-time 20
    -X "$1" -H "@$scratch/auth" -H "Idempotency-Key: smoke-$RANDOM-$RANDOM")
  [ -n "${3:-}" ] && args+=(-H 'Content-Type: application/json' --data "$3")
  curl "${args[@]}" "$2"
}
# upload <url> <type> <file>: PUT the file to a signed URL on the
# user-content port (the URL names the configured origin; the smoke stack
# publishes it on an ephemeral port). Prints the status code.
upload() {
  curl -sS -o "$scratch/body" -D "$scratch/headers" -w '%{http_code}' --max-time 20 \
    -X PUT -H "Content-Type: $2" --data-binary "@$3" "$files${1#"$RUNBOOK_USERCONTENT_ORIGIN"}"
}
header() { grep -i "^$1:" "$scratch/headers" | head -1 | cut -d' ' -f2- | tr -d '\r'; }

[ "$(curl -fsS --max-time 3 "$api/v1/meta" | json_get capabilities.uploads)" = true ] ||
  fail "capabilities.uploads is not true"

printf 'apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: smoke-uploads, title: Smoke uploads}\nspec:\n  steps:\n    - id: photo\n      title: Photograph the rack\n      actor: human\n      evidence: {required: [screenshot]}\n' |
  compose exec -T server sh -c "cat > /tmp/smoke-uploads.yaml"
cli push /tmp/smoke-uploads.yaml --project default --draft --json >/dev/null || fail "push failed"
cli publish smoke-uploads --project default --json >/dev/null || fail "publish failed"
exe=$(start_run smoke-uploads)
sr=""
deadline=$((SECONDS + 20))
while [ -z "$sr" ] && [ $SECONDS -lt $deadline ]; do
  request GET "$api/v1/executions/$exe/next" >/dev/null
  sr=$(python3 -I -c 'import json,sys; w=json.load(open(sys.argv[1])).get("ready_work") or []; print(w[0]["step_run_id"] if w else "")' "$scratch/body" 2>/dev/null || true)
  [ -n "$sr" ] || sleep 0.5
done
[ -n "$sr" ] || fail "the photo step never became ready"
status=$(request POST "$api/v1/step-runs/$sr/claim" '{"session_id":"smoke"}')
[ "$status" = 200 ] || fail "claim: $status $(cat "$scratch/body")"
log "run $exe: the owner holds the photo step's claim"

# The GPS-tagged JPEG.
size=$(wc -c <"$scratch/gps.jpg" | tr -d ' ')
status=$(request POST "$api/v1/step-runs/$sr/evidence" "{\"name\":\"rack.jpg\",\"type\":\"image/jpeg\",\"size\":$size}")
[ "$status" = 201 ] || fail "slot: $status $(cat "$scratch/body")"
evd=$(json_get evidence_id <"$scratch/body")
url=$(json_get upload_url <"$scratch/body")
case $url in "$RUNBOOK_USERCONTENT_ORIGIN/u/$exe/$evd?"*) ;; *) fail "upload URL is not on the user-content origin" ;; esac
status=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 20 -X PUT -H 'Content-Type: image/jpeg' \
  --data-binary "@$scratch/gps.jpg" "$api${url#"$RUNBOOK_USERCONTENT_ORIGIN"}")
[ "$status" != 204 ] || fail "the API port took the upload PUT"
status=$(upload "$url" image/jpeg "$scratch/gps.jpg")
[ "$status" = 204 ] || fail "PUT: $status $(cat "$scratch/body")"
[ "$(header x-content-type-options)" = nosniff ] || fail "PUT response without nosniff"
status=$(request POST "$api/v1/step-runs/$sr/evidence/$evd/finalize")
[ "$status" = 200 ] || fail "finalize: $status $(cat "$scratch/body")"
file_url=$(json_get url <"$scratch/body")
thumb_url=$(json_get thumbnail_url <"$scratch/body")
[ "$(json_get width <"$scratch/body")x$(json_get height <"$scratch/body")" = 64x48 ] ||
  fail "dimensions: $(cat "$scratch/body")"
log "uploaded and finalized a GPS-tagged JPEG ($size bytes)"

status=$(curl -sS -o "$scratch/stored.jpg" -D "$scratch/headers" -w '%{http_code}' --max-time 20 \
  -H 'Cookie: runbook.session_token=smoke' "$files${file_url#"$RUNBOOK_USERCONTENT_ORIGIN"}")
[ "$status" = 200 ] || fail "GET file: $status"
python3 -I - "$scratch/stored.jpg" <<'PY' || fail "the stored JPEG still carries metadata"
import sys
data = open(sys.argv[1], "rb").read()
assert data[:2] == b"\xff\xd8", "not a JPEG"
assert b"Exif" not in data and b"N\x00\x00\x00" not in data, "EXIF or GPS left"
at = 2
while data[at] == 0xFF and data[at + 1] != 0xDA:
    marker = data[at + 1]
    assert not (0xE1 <= marker <= 0xEF or marker == 0xFE), f"segment {marker:#x} left"
    at += 2 + int.from_bytes(data[at + 2:at + 4], "big")
PY
[ "$(header content-type)" = image/jpeg ] || fail "content-type: $(header content-type)"
[ "$(header x-content-type-options)" = nosniff ] || fail "file without nosniff"
[ "$(header content-security-policy)" = "sandbox; default-src 'none'; frame-ancestors 'none'" ] || fail "file without the sandbox CSP"
[ "$(header x-frame-options)" = DENY ] || fail "file without X-Frame-Options: DENY"
case $(header content-disposition) in inline\;*) ;; *) fail "content-disposition: $(header content-disposition)" ;; esac
grep -qi '^set-cookie:' "$scratch/headers" && fail "the user-content origin set a cookie"
status=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 20 "$files${thumb_url#"$RUNBOOK_USERCONTENT_ORIGIN"}")
[ "$status" = 200 ] || fail "GET thumbnail: $status"
log "the stored JPEG has no EXIF or GPS; sandbox CSP, nosniff, inline disposition, no cookie"

# A mislabeled file: text declared as PNG.
size=$(wc -c <"$scratch/fake.png" | tr -d ' ')
status=$(request POST "$api/v1/step-runs/$sr/evidence" "{\"name\":\"fake.png\",\"type\":\"image/png\",\"size\":$size}")
[ "$status" = 201 ] || fail "slot: $status $(cat "$scratch/body")"
fake=$(json_get evidence_id <"$scratch/body")
status=$(upload "$(json_get upload_url <"$scratch/body")" image/png "$scratch/fake.png")
[ "$status" = 204 ] || fail "PUT fake: $status $(cat "$scratch/body")"
status=$(request POST "$api/v1/step-runs/$sr/evidence/$fake/finalize")
[ "$status" = 422 ] && [ "$(json_get code <"$scratch/body")" = upload.type_mismatch ] ||
  fail "mislabeled file: $status $(cat "$scratch/body")"
[ "$(psql_admin "SELECT count(*) FROM runbook.blob WHERE key LIKE '%$fake%'")" = 0 ] ||
  fail "the mislabeled object was kept"
log "a mislabeled file is refused (upload.type_mismatch) and deleted"

for path in / /v1/meta /.well-known/runbook; do
  status=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 5 "$files$path")
  [ "$status" = 404 ] || fail "user-content port answered $path with $status"
done
log "the user-content port answers nothing but /u/*"
pass
