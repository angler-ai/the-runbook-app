---
title: Quick start with Docker
description: Run the server and PostgreSQL with docker compose and create the first owner.
sidebar:
  order: 1
  badge: Docker/Kubernetes
---

The container edition is one image that carries the server
(`runbook-server`), the CLI (`runbook`), the sign-in service and the web UI,
which the server hosts on its own origin. You build it locally from the
repository; there is no public image yet.

## Before you start

You need a checkout of the repository (run every command from its root) and
Docker with Compose.

## 1. Fill in the settings

```sh
cp deploy/.env.example deploy/.env && chmod 600 deploy/.env
```

Set at least these in `deploy/.env`:

| Variable | How to make it |
| --- | --- |
| `POSTGRES_PASSWORD` | `openssl rand -hex 24` |
| `RUNBOOK_DB_PASSWORD` | `openssl rand -hex 24` |
| `RUNBOOK_WEBHOOK_SECRET_KEY` | `openssl rand -base64 32`; keep it, it seals webhook secrets at rest |
| `RUNBOOK_AUTH_SECRET` | `openssl rand -hex 32`, for sign-in |
| `RUNBOOK_SMTP_URL`, `RUNBOOK_SMTP_FROM` | your mail server, for sign-in codes |

Every other setting is in [Environment variables](/reference/environment/).

## 2. Start the stack

```sh
dc() { docker compose -f deploy/docker-compose.yml --env-file deploy/.env "$@"; }
dc up -d --build --wait
```

`--wait` returns once the server and PostgreSQL report healthy. The server
applies its migrations before it listens.

## 3. Create the first owner

```sh
dc exec server runbook-server bootstrap --owner-email you@example.test --owner-name "Your Name"
dc exec server runbook-server doctor
curl -s http://127.0.0.1:8080/v1/meta
```

`bootstrap --owner-email` creates the organization, the owner with that
verified address and the `default` project, and prints their IDs as one JSON
line. The owner then signs in at `http://127.0.0.1:8080` with a code sent to
that address. It prints no secret unless you add `--with-token`, which also
prints an owner personal access token and a project service token, once.

Without sign-in configured, plain `bootstrap` is the headless path: it always
prints both tokens. A second `bootstrap` fails with
`bootstrap.already_initialized`.

`doctor` checks the database connection, schema version and blob tables.

## 4. Use the CLI

The CLI is in the same image:

```sh
read -rs RUNBOOK_TOKEN && export RUNBOOK_TOKEN   # paste a personal access token
dc exec -e RUNBOOK_TOKEN -e RUNBOOK_HOST=http://127.0.0.1:8080 server runbook whoami
```

To run it on your machine instead, [install the CLI](/getting-started/cli/)
and sign in:

```sh
runbook login http://127.0.0.1:8080
```

## Next

- [Your first runbook](/getting-started/first-runbook/)
- [Running on other container platforms](https://github.com/angler-ai/the-runbook-app/blob/main/deploy/platforms.md):
  Cloud Run, Fly.io, Render, Kubernetes or a VM.
- An external PostgreSQL 14+ database: use
  `deploy/docker-compose.external-db.yml` with `DATABASE_URL`.
