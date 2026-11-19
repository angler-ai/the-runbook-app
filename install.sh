#!/bin/sh
# Install the Runbook CLI (`runbook`) from a GitHub Release.
#
#   curl -fsSL https://github.com/angler-ai/the-runbook-app/releases/latest/download/install.sh | sh
#
# It picks the build for this machine (Linux or macOS, x86_64 or arm64),
# checks it against the release's SHA256SUMS and puts `runbook` in
# ~/.local/bin. Nothing runs as root.
#
# Settings (environment):
#   RUNBOOK_VERSION      a version such as 0.1.0 (default: the latest release)
#   RUNBOOK_INSTALL_DIR  where `runbook` goes (default: ~/.local/bin)
#   RUNBOOK_REPO         owner/name of the GitHub repository
#                        (default: angler-ai/the-runbook-app)
#   RUNBOOK_DOWNLOAD_BASE  a mirror holding the release assets, used as
#                        $RUNBOOK_DOWNLOAD_BASE/<asset>; needs RUNBOOK_VERSION
#
# To also check the build provenance, with the GitHub CLI signed in:
#   gh attestation verify <tarball> --repo angler-ai/the-runbook-app
set -eu

repo=${RUNBOOK_REPO:-angler-ai/the-runbook-app}
dir=${RUNBOOK_INSTALL_DIR:-"$HOME/.local/bin"}
version=${RUNBOOK_VERSION:-}

fail() { echo "runbook install: $*" >&2; exit 1; }
say() { echo "runbook install: $*" >&2; }

command -v curl >/dev/null 2>&1 || fail "needs curl"
command -v tar >/dev/null 2>&1 || fail "needs tar"

case $(uname -s) in
  Linux) os=linux ;;
  Darwin) os=macos ;;
  *) fail "no build for $(uname -s); build from source: cargo install --locked --git https://github.com/$repo runbook-cli" ;;
esac
case $(uname -m) in
  x86_64 | amd64) arch=x86_64 ;;
  arm64 | aarch64) arch=aarch64 ;;
  *) fail "no build for $(uname -m)" ;;
esac
# A shell under Rosetta reports x86_64 on Apple Silicon: prefer the native build.
if [ "$os" = macos ] && [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
  arch=aarch64
fi

if [ -z "$version" ]; then
  [ -z "${RUNBOOK_DOWNLOAD_BASE:-}" ] || fail "RUNBOOK_DOWNLOAD_BASE needs RUNBOOK_VERSION"
  # /releases/latest redirects to /releases/tag/vX.Y.Z (no API rate limit).
  url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") ||
    fail "could not reach github.com/$repo"
  version=${url##*/tag/v}
  [ "$version" != "$url" ] || fail "no release found at github.com/$repo"
fi
version=${version#v}
case $version in
  *[!0-9A-Za-z.+-]* | "") fail "bad version: $version" ;;
esac

base=${RUNBOOK_DOWNLOAD_BASE:-"https://github.com/$repo/releases/download/v$version"}
asset="runbook-cli-$version-$os-$arch.tar.gz"

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t runbook)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading runbook $version for $os/$arch"
curl -fsSL -o "$tmp/$asset" "$base/$asset" || fail "could not download $base/$asset"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || fail "could not download $base/SHA256SUMS"

expected=$(awk -v f="$asset" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || fail "$asset is not in SHA256SUMS"
if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$asset" | awk '{ print $1 }')
elif command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$asset" | awk '{ print $1 }')
else
  fail "needs sha256sum or shasum to check the download"
fi
[ "$actual" = "$expected" ] || fail "checksum mismatch for $asset (expected $expected, got $actual)"

tar -xzf "$tmp/$asset" -C "$tmp" runbook || fail "$asset holds no runbook binary"
mkdir -p "$dir"
# Write beside the target, then rename: a running `runbook` is never half-written.
cp "$tmp/runbook" "$dir/.runbook.new"
chmod 755 "$dir/.runbook.new"
mv -f "$dir/.runbook.new" "$dir/runbook"

say "installed $("$dir/runbook" --version 2>/dev/null || echo "runbook $version") to $dir/runbook"
case ":$PATH:" in
  *":$dir:"*) ;;
  *) say "$dir is not on your PATH; add it, for example: export PATH=\"$dir:\$PATH\"" ;;
esac
say "next: runbook login https://your-server"
say "then: runbook skill install --therunbook   (adds the agent skill to Claude Code, Codex and Cursor)"
