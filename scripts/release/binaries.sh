#!/usr/bin/env bash
# UPD1: the CLI and server binaries of a release, for each platform the
# image is built for, as runbook-cli-<version>-linux-<arch>.tar.gz and
# runbook-server-<version>-linux-<arch>.tar.gz in OUTDIR. Each platform's
# image filesystem is built with `docker buildx build --output type=local`
# (the release build's cache makes this cheap) and the two binaries are
# taken from it. Nothing is pushed or pulled.
#
#   scripts/release/binaries.sh VERSION OUTDIR [PLATFORM...]
#
# PLATFORM defaults to linux/amd64 linux/arm64. Run from anywhere; the build
# context is the repository.
set -euo pipefail

[ $# -ge 2 ] || { echo "usage: $0 VERSION OUTDIR [PLATFORM...]" >&2; exit 2; }
version=$1 out=$2
shift 2
[ $# -gt 0 ] || set -- linux/amd64 linux/arm64
repo=$(cd "$(dirname "$0")/../.." && pwd)
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for platform in "$@"; do
  case $platform in
    linux/amd64) arch=x86_64 ;;
    linux/arm64) arch=aarch64 ;;
    *) echo "unsupported platform $platform" >&2; exit 2 ;;
  esac
  docker buildx build --quiet --build-arg RUNBOOK_CACHE_ID="${RUNBOOK_CACHE_ID:-release}" --platform "$platform" --output "type=local,dest=$work/$arch" "$repo" >/dev/null
  for bin in runbook runbook-server; do
    case $bin in runbook) name=runbook-cli ;; *) name=$bin ;; esac
    mkdir -p "$work/$arch-$bin"
    cp "$work/$arch/usr/local/bin/$bin" "$work/$arch-$bin/$bin"
    tar -czf "$out/$name-$version-linux-$arch.tar.gz" -C "$work/$arch-$bin" "$bin"
    echo "$out/$name-$version-linux-$arch.tar.gz"
  done
done
