---
title: Install the CLI
description: Install the runbook command on macOS or Linux with one line, then sign in to your server.
sidebar:
  order: 4
---

The CLI (`runbook`) is how people, scripts, CI and agents use a Runbook
server from a terminal. Each release has builds for macOS and Linux, on
Apple Silicon or Intel/AMD (x86_64) and arm64.

## Install

```sh
curl -fsSL https://github.com/angler-ai/the-runbook-app/releases/latest/download/install.sh | sh
```

The script picks the build for your machine, checks it against the
release's `SHA256SUMS` and puts `runbook` in `~/.local/bin`. It needs only
`curl` and `tar`, and never runs as root. If `~/.local/bin` isn't on your
`PATH`, it tells you the line to add.

Settings, as environment variables before `sh`:

| Variable | Default | |
|---|---|---|
| `RUNBOOK_VERSION` | the latest release | a version such as `0.1.2`, to pin one |
| `RUNBOOK_INSTALL_DIR` | `~/.local/bin` | where `runbook` goes |
| `RUNBOOK_DOWNLOAD_BASE` | the GitHub Release | a mirror holding the release assets (needs `RUNBOOK_VERSION`) |

For example, to pin a version in CI:

```sh
curl -fsSL https://github.com/angler-ai/the-runbook-app/releases/latest/download/install.sh | RUNBOOK_VERSION=0.1.2 sh
```

To update, run the same line again. To uninstall, delete `~/.local/bin/runbook`
and, if you want, its logins (`~/.config/runbook`, and on macOS the
`runbook` items in Keychain).

## Sign in

```sh
runbook login https://runbook.example.com
runbook whoami
```

`runbook login` prints a code and opens the browser, where you sign in and
approve the CLI. With `--no-browser` it only prints the link, for an SSH
session. Each host you sign in to is a context; see
[the CLI reference](/reference/cli/).

## Check the download yourself

Every asset carries a build provenance attestation. With the
[GitHub CLI](https://cli.github.com/) signed in:

```sh
gh attestation verify runbook-cli-0.1.2-macos-aarch64.tar.gz --repo angler-ai/the-runbook-app
```

## Other ways

- **Manual download.** Take `runbook-cli-<version>-<os>-<arch>.tar.gz` from
  the [releases page](https://github.com/angler-ai/the-runbook-app/releases)
  and compare `shasum -a 256 <file>` with its line in `SHA256SUMS`. On macOS a
  browser download is quarantined by Gatekeeper, since the binary isn't
  notarized: clear it with `xattr -d com.apple.quarantine runbook`, or use
  the script, which downloads with `curl`.
- **In the container image.** The server image includes the CLI:
  `docker run --rm --entrypoint runbook ghcr.io/angler-ai/runbook:latest --version`.
- **From source**, with Rust installed:
  `cargo install --locked --git https://github.com/angler-ai/the-runbook-app runbook-cli`.
- **Windows** has no build yet; use WSL with the Linux build.
