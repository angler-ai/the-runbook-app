#!/usr/bin/env bash
# The full local check set for the native pilot (Gate D).
#
#   scripts/check.sh                       # everything except the Postgres suite
#   DATABASE_URL=postgres://... scripts/check.sh   # plus the Postgres suite
#
# Steps, in order (the first failure stops the run):
#   1. shell syntax: `bash -n` on this script and every pilot/smoke script
#      and the UPD1 update and release scripts, plus shellcheck on those
#      (deploy/update.sh, its smoke test, scripts/release/binaries.sh) when
#      shellcheck is installed;
#   1b. migration safety (UPD1): `node scripts/check-migrations.mjs`, the
#      expand/contract rule for new Postgres, D1 and Durable Object
#      migrations (a DROP, RENAME, type change, new NOT NULL or constraint
#      needs a `-- contract(unused-since X.Y.Z): ...` annotation naming an
#      older release; see CONTRIBUTING.md), and its tests and the release
#      scripts' (scripts/tests/);
#   2. cargo fmt --all -- --check;
#   3. cargo clippy --workspace --all-targets --locked -- -D warnings;
#   4. cargo test --workspace --locked (Postgres tests are #[ignore]d);
#   5. when DATABASE_URL is set: cargo test --workspace --locked -- --ignored
#      (only the #[ignore]d Postgres tests; step 4 ran the rest), after
#      building auth/dist, which the real sign-in test runs (AUTH5);
#   6. wasm32-unknown-unknown compile checks for the portable crates
#      (model, expr, spec, engine, core, api, mcp, contract, conformance, web-wasm,
#      worker, store-do, store-r2, store-d1, worker-conformance, report-pdf) and for the tests of
#      runbook-core (the transactional store fake) and runbook-report-pdf
#      (D52's PDF writer and reader), plus a linked build of the browser
#      compiler module (runbook-web-wasm, B3) and strict clippy on the
#      wasm-only Workers code: runbook-worker (CF1), runbook-store-do's
#      SqlStorage backend and the conformance test Worker (CF2), and
#      runbook-store-d1's D1 backend (CF3);
#   7. the maintained CEL fork: its tests and formatting;
#   8. when CHECK_CLOUDFLARE=1: the local Workers suite (`cloudflare/`, CF1,
#      CF2): `npm ci --ignore-scripts`, then `npm test`, which builds
#      crates/runbook-worker and crates/runbook-worker-conformance with
#      worker-build and runs them under workerd via Miniflare (the CF1 gates
#      the execution-scoped and D1 store conformance cases, and CF3's
#      end-to-end test through the real router). Needs Node 22+ and `cargo install worker-build --version
#      '=0.8.7' --locked`. Local only: it never deploys or logs in.
#   9. when CHECK_CLOUDFLARE=1 and DATABASE_URL is set: CF8's Postgres ->
#      archive -> local Workers -> archive round trip, with the Worker the
#      suite just built (the general Postgres step skips it), then CF9's
#      edition contract suite (`cloudflare/contract/run.mjs`): the native
#      server on its own `rb_test_contract_*` database and the local Workers
#      stack answer every public route alike.
#
# Postgres suite (step 5): tests use OWNED databases only. Each test creates
# its own `rb_test_*` database with CREATE DATABASE, works only there and
# drops it afterwards, so DATABASE_URL must name a role that may create
# databases on a server you own (a local or CI Postgres 14+). Never point it
# at a shared or production server. Each fixture's stores and server share
# one pool of 4 connections (tests/support/database.rs in the store crate),
# plus maintenance and admin connections, so a fixture stays near six
# backends and Postgres's default 100 covers a full run. If you still see
# `too many clients` or spurious 503s (a busy shared server, or another
# suite running at the same time), raise `max_connections` or set
# CHECK_PG_TEST_THREADS (for example 4) to pass `--test-threads` to step 5.
#
# A wasm compile check is not a Cloudflare Workers runtime verification.
# Needs the toolchain from rust-toolchain.toml (it includes the wasm target);
# the native/Wasm parity tests also use `node` when it is installed.
set -euo pipefail

cd "$(dirname "$0")/.."

step() {
  echo
  echo "==> $*"
}

step "Shell syntax (bash -n)"
for script in scripts/*.sh scripts/release/*.sh examples/pilot/*.sh deploy/smoke/*.sh deploy/*.sh; do
  [ -e "$script" ] || continue
  bash -n "$script"
  echo "    $script"
done
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -x deploy/update.sh deploy/smoke/update.sh scripts/release/binaries.sh
  echo "    shellcheck: deploy/update.sh deploy/smoke/update.sh scripts/release/binaries.sh"
else
  echo "    shellcheck not installed: skipped"
fi

step "Migration safety (expand/contract, UPD1)"
node scripts/check-migrations.mjs
node --test scripts/tests/

step "Formatting"
cargo fmt --all -- --check

step "Clippy (strict)"
cargo clippy --workspace --all-targets --locked -- -D warnings

step "Workspace tests"
cargo test --workspace --locked

if [ -n "${DATABASE_URL:-}" ]; then
  # AUTH5: tests/auth_service.rs signs in through the real auth service.
  step "Auth service bundle for the Postgres suite (auth/dist)"
  (cd auth && npm ci --ignore-scripts --no-audit --no-fund && npm run build)
  step "Postgres suite (owned databases only)"
  if [ -n "${CHECK_PG_TEST_THREADS:-}" ]; then
    cargo test --workspace --locked -- --ignored --test-threads "$CHECK_PG_TEST_THREADS"
  else
    cargo test --workspace --locked -- --ignored
  fi
else
  step "Postgres suite skipped: DATABASE_URL is not set"
fi

step "WebAssembly: portable crates"
cargo check --workspace \
  --exclude runbook-store-postgres --exclude runbook-server \
  --exclude runbook-cli --exclude runbook-store-objects \
  --locked --target wasm32-unknown-unknown

step "WebAssembly: runbook-core tests (transactional store fake)"
cargo check -p runbook-core --tests --locked --target wasm32-unknown-unknown

step "WebAssembly: runbook-report-pdf tests (D52)"
cargo check -p runbook-report-pdf --tests --locked --target wasm32-unknown-unknown

step "WebAssembly: browser compiler module (runbook-web-wasm)"
cargo build -p runbook-web-wasm --lib --locked --target wasm32-unknown-unknown

step "WebAssembly: Workers clippy (runbook-worker CF1/CF3/CF4, runbook-store-do and the conformance Worker CF2, runbook-store-r2 CF4, runbook-store-d1 CF3)"
cargo clippy -p runbook-worker -p runbook-store-do -p runbook-worker-conformance -p runbook-store-r2 \
  -p runbook-store-d1 \
  --locked --target wasm32-unknown-unknown -- -D warnings

step "Maintained CEL fork"
cargo test --manifest-path vendor/cel-core/Cargo.toml --locked
cargo fmt --manifest-path vendor/cel-core/Cargo.toml -- --check

if [ "${CHECK_CLOUDFLARE:-}" = "1" ]; then
  step "Cloudflare Workers, local workerd only (cloudflare/, CF1/CF2)"
  (cd cloudflare && npm ci --ignore-scripts --no-audit --no-fund && npm test)
  if [ -n "${DATABASE_URL:-}" ]; then
    step "CF8: Postgres -> archive -> local Workers -> archive (owned databases only)"
    RUNBOOK_CF_ROUNDTRIP=1 cargo test -p runbook-store-postgres --locked --test portable \
      postgres_archive_round_trips_through_local_workers -- --ignored --exact
    step "CF9: the edition contract suite (native on Postgres and local Workers)"
    cargo build -p runbook-server --locked
    (cd cloudflare && node contract/run.mjs)
    (cd cloudflare && node contract/runner.mjs)
  fi
else
  step "Cloudflare Workers suite skipped: set CHECK_CLOUDFLARE=1 to run it"
fi

step "All checks passed"
