# syntax=docker/dockerfile:1.7
# The Runbook native pilot image: runbook-server (axum on Postgres), the
# runbook CLI, the built web UI, which the server hosts on its own origin
# (RUNBOOK_WEB_DIR, B2), and the sign-in service (auth/, D43), which `serve`
# runs as a supervised loopback child under Bun and proxies at /api/auth.
# Build: docker build -t runbook-pilot:local .
# No secrets are baked in; everything is configured by environment.

# Pinned to rust-toolchain.toml (channel 1.99.1).
ARG RUST_VERSION=1.99.1
# Pinned to web/.node-version (26.8.1), by the multi-platform digest it
# resolved to when this file was last updated; bump both together.
ARG NODE_IMAGE=node:26.8.1-bookworm-slim@sha256:367679cf9792759492a486e4aa4b421764d71a9546a6dae8aab81a99eb797b3e
# The auth service's runtime (D43, PRD section 15): only the bun binary is
# copied out. Bun over Node because it is smaller (about 100 MB against
# about 149 MB for node) and needs only glibc, where node also needs
# libstdc++ and, on arm64, libatomic. Pinned to the multi-platform digest
# 1.3.12-slim resolved to when this file was last updated; bump both together.
ARG BUN_IMAGE=oven/bun:1.3.12-slim@sha256:d3c7094c144dd3975d183a4dbc4ec0a764223995bff73290d983edb47043a75f

FROM rust:${RUST_VERSION}-slim-bookworm AS rust-base
# rust-toolchain.toml also asks for clippy and rustfmt; the release build
# needs neither, so pin the image's toolchain instead of letting rustup
# download extras. The wasm target is the browser compiler's (B3, F97).
ARG RUST_VERSION
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} \
    CARGO_TERM_COLOR=never \
    CARGO_INCREMENTAL=0
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add --toolchain "${RUST_VERSION}" wasm32-unknown-unknown
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY vendor ./vendor
COPY skills ./skills
# UPD1: runbook-server's build.rs computes the rollback floor from these too.
COPY auth/drizzle/pg ./auth/drizzle/pg

FROM rust-base AS builder
ARG RUNBOOK_CACHE_ID=default
# Cache mounts keep the registry and target dir between builds (local only;
# nothing from them lands in the image except the two copied binaries).
RUN --mount=type=cache,id=runbook-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=runbook-cargo-target-${RUNBOOK_CACHE_ID},target=/src/target \
    cargo build --release --locked -p runbook-server -p runbook-cli \
 && install -D -m 0755 target/release/runbook-server /out/runbook-server \
 && install -D -m 0755 target/release/runbook /out/runbook

# The browser compiler, built beside the server with its own target cache so
# neither build waits on the other's lock. Same artifact as
# `npm run wasm:build` (web/scripts/build-wasm.mjs) without wasm-opt.
FROM rust-base AS wasm
ARG RUNBOOK_CACHE_ID=default
RUN --mount=type=cache,id=runbook-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=runbook-cargo-target-wasm-${RUNBOOK_CACHE_ID},target=/src/target \
    cargo build --release --locked -p runbook-web-wasm --lib --target wasm32-unknown-unknown \
 && install -D -m 0644 target/wasm32-unknown-unknown/release/runbook_web_wasm.wasm /out/runbook.wasm

# The web UI (vite build into web/dist). Dependencies install with no
# lifecycle scripts (F102); the lockfile carries the per-platform native
# binaries Vite and Tailwind need.
FROM ${NODE_IMAGE} AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json web/.npmrc ./
RUN --mount=type=cache,id=runbook-npm-cache,target=/root/.npm \
    npm ci --ignore-scripts --no-audit --no-fund
# The UI imports the API contract and its examples from the repository root.
COPY api /src/api
COPY schemas /src/schemas
COPY web ./
COPY --from=wasm /out/runbook.wasm src/wasm/runbook.wasm
RUN npm run build

# The auth service (auth/, AUTH1): esbuild bundles src/server.ts into one
# dist/server.mjs with its dependencies; the image needs only that file and
# the Postgres migrations it applies at start (../drizzle/pg next to it).
# Dependencies install with no lifecycle scripts, as for the web UI.
FROM ${NODE_IMAGE} AS auth
WORKDIR /src/auth
COPY auth/package.json auth/package-lock.json ./
RUN --mount=type=cache,id=runbook-npm-cache,target=/root/.npm \
    npm ci --ignore-scripts --no-audit --no-fund
COPY auth ./
RUN npm run build

FROM ${BUN_IMAGE} AS bun

FROM debian:bookworm-slim AS runtime
# ca-certificates: TLS to managed Postgres and webhook receivers.
# libssl3: native-tls for Postgres. curl: the healthcheck only.
# tini: PID 1 that forwards SIGTERM and reaps children.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 curl tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 runbook \
 && useradd --system --uid 10001 --gid runbook --home-dir /home/runbook --create-home --shell /usr/sbin/nologin runbook
COPY --from=builder /out/runbook-server /out/runbook /usr/local/bin/
# Root-owned and read-only to the server user. Set RUNBOOK_WEB_DIR empty to
# serve the API alone.
COPY --from=web /src/web/dist /usr/share/runbook/web
# Sign-in (D43): Bun runs the bundled auth service. `serve` starts it only
# when RUNBOOK_AUTH_SECRET is set (D48); it is root-owned and read-only too.
# BUN_RUNTIME_TRANSPILER_CACHE_PATH=0: Bun writes no cache to the small
# in-memory home.
COPY --from=bun /usr/local/bin/bun /usr/local/bin/bun
COPY --from=auth /src/auth/dist/server.mjs /usr/share/runbook/auth/dist/server.mjs
COPY --from=auth /src/auth/drizzle/pg /usr/share/runbook/auth/drizzle/pg
# HOST1: with RUNBOOK_LISTEN_ADDR unset, RUNBOOK_LISTEN_PORT_FROM_ENV=1 makes
# the server listen on every interface on $PORT, so a platform that sets PORT
# (Cloud Run, Fly, Render) is followed; 8080 otherwise. Outside the image a
# PORT variable alone binds nothing public.
ENV RUNBOOK_LISTEN_PORT_FROM_ENV=1 \
    PORT=8080 \
    RUNBOOK_WEB_DIR=/usr/share/runbook/web \
    RUNBOOK_AUTH_COMMAND="/usr/local/bin/bun /usr/share/runbook/auth/dist/server.mjs" \
    BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 \
    HOME=/home/runbook
USER runbook:runbook
WORKDIR /home/runbook
EXPOSE 8080 8081
# /health/live (HOST1) answers without the database and names nothing; curl
# prints nothing (-o /dev/null, -s), so `docker inspect` health logs stay
# empty. The port follows RUNBOOK_LISTEN_ADDR, else PORT. Probes every second
# while starting, then every 10s. Orchestrators use /health/ready (database
# reachable, not shutting down) for traffic; see deploy/platforms.md.
HEALTHCHECK --interval=10s --timeout=3s --start-period=60s --start-interval=1s --retries=3 \
  CMD ["/bin/sh", "-c", "a=${RUNBOOK_LISTEN_ADDR:-:${PORT:-8080}}; curl -fs -o /dev/null --max-time 2 \"http://127.0.0.1:${a##*:}/health/live\""]
# `serve` applies pending migrations (forward-only, checksummed, idempotent,
# under a database advisory lock, so replicas starting together are safe)
# before it listens, so a restart or a new image on the same database is the
# upgrade path; if the database is not reachable it exits non-zero and the
# restart policy retries. tini forwards SIGTERM (`docker stop`) and reaps;
# `serve` then drains and exits 0 within RUNBOOK_SHUTDOWN_TIMEOUT_SECS (8). Other subcommands (bootstrap, doctor, migrate,
# assign-owner) run the same binary:
#   docker compose exec server runbook-server bootstrap --owner-email you@example.test
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/runbook-server"]
CMD ["serve"]
