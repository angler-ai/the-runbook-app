ARG SANDBOX_BASE=node:26.8.1-bookworm-slim@sha256:367679cf9792759492a486e4aa4b421764d71a9546a6dae8aab81a99eb797b3e
FROM ${SANDBOX_BASE} AS helper
RUN apt-get update \
 && apt-get install -y --no-install-recommends golang-go \
 && rm -rf /var/lib/apt/lists/*
COPY runner/packages/sandbox-ctl/helper/main.go /src/main.go
RUN CGO_ENABLED=0 go build -trimpath -ldflags="-s -w" -o /out/helper /src/main.go

FROM ${SANDBOX_BASE}
COPY --from=helper /out/helper /runbook-trusted/helper
RUN apt-get update \
 && apt-get install -y --no-install-recommends bash coreutils git curl jq python3 python3-pip python3-venv ripgrep unzip ca-certificates procps util-linux \
 && rm -rf /var/lib/apt/lists/*
COPY runner/bin/sandbox.mjs /usr/local/lib/runbook-sandbox.mjs
