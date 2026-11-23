# syntax=docker/dockerfile:1.7
# The runner image (design §5.3): one image, one process per container,
# picked by RUNNER_ROLE (api, worker, sandbox-ctl, vault, gateway) or the
# first argument; `migrate` applies the runner database migrations.
# Build from the repository root: docker build -f runner/Dockerfile .
# No secrets are baked in; everything is configured by environment.

# Pinned like the server image (Dockerfile, web/.node-version 26.8.1); the
# runner supports Node 22.18 and later.
ARG NODE_IMAGE=node:26.8.1-bookworm-slim@sha256:367679cf9792759492a486e4aa4b421764d71a9546a6dae8aab81a99eb797b3e
# Pinned to rust-toolchain.toml (channel 1.99.1), as in the server image.
ARG RUST_VERSION=1.99.1

# The core's redactor compiled to WebAssembly (crates/runbook-redact-wasm,
# R1-F3): the same artifact as `npm run wasm:build`.
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-bookworm AS wasm
ARG RUST_VERSION
ARG RUNBOOK_CACHE_ID=default
ENV RUSTUP_TOOLCHAIN=${RUST_VERSION} \
    CARGO_TERM_COLOR=never \
    CARGO_INCREMENTAL=0
RUN rustup target add --toolchain "${RUST_VERSION}" wasm32-unknown-unknown
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY vendor ./vendor
RUN --mount=type=cache,id=runbook-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=runbook-cargo-target-redact-${RUNBOOK_CACHE_ID},target=/src/target,sharing=locked \
    cargo build --release --locked -p runbook-redact-wasm --lib --target wasm32-unknown-unknown \
 && install -D -m 0644 target/wasm32-unknown-unknown/release/runbook_redact_wasm.wasm /out/runbook_redact.wasm

FROM ${NODE_IMAGE} AS build
WORKDIR /src/runner
COPY runner/package.json runner/package-lock.json runner/.npmrc ./
COPY runner/packages/shared/package.json packages/shared/
COPY runner/packages/api/package.json packages/api/
COPY runner/packages/worker/package.json packages/worker/
COPY runner/packages/sandbox-ctl/package.json packages/sandbox-ctl/
COPY runner/packages/vault/package.json packages/vault/
COPY runner/packages/gateway/package.json packages/gateway/
COPY runner/packages/evals/package.json packages/evals/
# No dependency lifecycle scripts (F102, .npmrc).
RUN --mount=type=cache,id=runbook-npm-cache,target=/root/.npm,sharing=locked \
    npm ci --ignore-scripts --no-audit --no-fund
COPY runner/ ./
RUN npx tsc -b \
 && npm prune --omit=dev --ignore-scripts --no-audit --no-fund \
 && mkdir -p packages/worker/node_modules

FROM ${NODE_IMAGE} AS runtime
# tini: PID 1 that forwards SIGTERM and reaps children.
RUN apt-get update \
 && apt-get install -y --no-install-recommends tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 runner \
 && useradd --system --uid 10001 --gid runner --home-dir /home/runner --create-home --shell /usr/sbin/nologin runner
WORKDIR /app
COPY --from=build /src/runner/package.json ./
COPY --from=build /src/runner/node_modules ./node_modules
COPY --from=build /src/runner/bin ./bin
COPY --from=build /src/runner/packages/api/package.json ./packages/api/
COPY --from=build /src/runner/packages/api/migrations ./packages/api/migrations
COPY --from=build /src/runner/packages/api/dist ./packages/api/dist
COPY --from=build /src/runner/packages/shared/package.json ./packages/shared/
COPY --from=build /src/runner/packages/shared/dist ./packages/shared/dist
COPY --from=wasm /out/runbook_redact.wasm ./packages/shared/wasm/runbook_redact.wasm
COPY --from=build /src/runner/packages/worker/package.json ./packages/worker/
COPY --from=build /src/runner/packages/worker/dist ./packages/worker/dist
# npm can keep the worker's Ajv version under the workspace after pruning.
COPY --from=build /src/runner/packages/worker/node_modules ./packages/worker/node_modules
COPY --from=build /src/runner/packages/sandbox-ctl/package.json ./packages/sandbox-ctl/
COPY --from=build /src/runner/packages/sandbox-ctl/dist ./packages/sandbox-ctl/dist
COPY --from=build /src/runner/packages/vault/package.json ./packages/vault/
COPY --from=build /src/runner/packages/vault/dist ./packages/vault/dist
COPY --from=build /src/runner/packages/gateway/package.json ./packages/gateway/
COPY --from=build /src/runner/packages/gateway/dist ./packages/gateway/dist
# Node writes no diagnostic reports or heap snapshots unless asked to
# (design §7.3: no crash dumps in the vault or gateway); nothing here asks.
ENV NODE_ENV=production \
    HOME=/home/runner
RUN install -d -m 0700 -o runner -g runner /var/lib/runbook/runner-archives
USER 10001:10001
RUN node --input-type=module -e 'for (const role of ["worker", "sandbox-ctl", "api", "gateway", "vault"]) await import("./packages/" + role + "/dist/main.js")'
ENTRYPOINT ["/usr/bin/tini", "--", "node", "/app/bin/runner.mjs"]
