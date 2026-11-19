#!/usr/bin/env bash
# The CLI for macOS, as runbook-cli-<version>-macos-{aarch64,x86_64}.tar.gz
# in OUTDIR. Runs on a macOS machine (the release's macos job); either
# architecture builds both. Each binary is ad-hoc signed, which Apple
# Silicon requires; it is not notarized, so install it with install.sh or
# curl (a browser download is quarantined by Gatekeeper).
#
#   scripts/release/macos.sh VERSION OUTDIR [TARGET...]
#
# TARGET defaults to aarch64-apple-darwin x86_64-apple-darwin.
set -euo pipefail

[ $# -ge 2 ] || { echo "usage: $0 VERSION OUTDIR [TARGET...]" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] || { echo "$0 runs on macOS" >&2; exit 2; }
version=$1 out=$2
shift 2
[ $# -gt 0 ] || set -- aarch64-apple-darwin x86_64-apple-darwin
repo=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for target in "$@"; do
  case $target in
    aarch64-apple-darwin) arch=aarch64 ;;
    x86_64-apple-darwin) arch=x86_64 ;;
    *) echo "unsupported target $target" >&2; exit 2 ;;
  esac
  rustup target add "$target" >/dev/null
  (cd "$repo" && MACOSX_DEPLOYMENT_TARGET=12.0 cargo build --release --locked -p runbook-cli --target "$target")
  target_dir=$(cd "$repo" && cargo metadata --format-version 1 --no-deps | node -e 'process.stdout.write(JSON.parse(require("fs").readFileSync(0)).target_directory)')
  mkdir -p "$work/$arch"
  cp "$target_dir/$target/release/runbook" "$work/$arch/runbook"
  codesign --force --sign - "$work/$arch/runbook"
  got=$(lipo -archs "$work/$arch/runbook")
  case $arch:$got in
    aarch64:arm64 | x86_64:x86_64) ;;
    *) echo "$target built a $got binary" >&2; exit 1 ;;
  esac
  tar -czf "$out/runbook-cli-$version-macos-$arch.tar.gz" -C "$work/$arch" runbook
  echo "$out/runbook-cli-$version-macos-$arch.tar.gz"
done
