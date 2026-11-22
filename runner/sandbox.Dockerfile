FROM node:26.8.1-bookworm-slim@sha256:367679cf9792759492a486e4aa4b421764d71a9546a6dae8aab81a99eb797b3e
RUN apt-get update \
 && apt-get install -y --no-install-recommends bash coreutils git curl jq python3 python3-pip python3-venv ripgrep unzip ca-certificates procps \
 && rm -rf /var/lib/apt/lists/*
