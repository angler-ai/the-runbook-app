---
title: Operating it
description: Install, configure, update, back up and monitor a self-hosted server.
sidebar:
  order: 1
---

## Configure

- Container edition: every setting is an environment variable; see
  [environment variables](/reference/environment/).
- Cloudflare edition: `wrangler.prod.jsonc`, `wrangler.auth.prod.jsonc` and
  `wrangler secret put`; see the [Cloudflare quick start](/getting-started/cloudflare/).

## Update

- **Docker/Kubernetes:** `deploy/update.sh` backs up the database and reads
  the backup back, updates, health-checks and rolls back on failure.
  `serve` applies pending migrations, forward-only and under a database
  advisory lock, before it listens.
- **Cloudflare:** `npm run update` in `cloudflare/`; start with
  `--dry-run`.

`/v1/meta` reports the running `version` and `schema`; `runbook version`
shows the CLI's and the server's. Read the [changelog](/changelog/) before
updating.

## Back up

Back up PostgreSQL (`pg_dump`, or your provider's backups) and keep
`RUNBOOK_WEBHOOK_SECRET_KEY`: without it, restored webhook secrets can't be
unsealed. On Cloudflare, the update takes a D1 Time Travel bookmark first.

## Monitor

- `/health/live` answers while the process runs; `/health/ready` also
  checks the database.
- `RUNBOOK_LOG_FORMAT=json` writes one JSON object per log line. Logs carry
  error codes, never URLs or secrets.
- `runbook-server doctor` checks the database connection, schema version and
  blob tables.

## Scale

Any number of server instances can share one database: leases, event IDs
and shared rate limits keep work single. See
[running on other platforms](https://github.com/angler-ai/the-runbook-app/blob/main/deploy/platforms.md).

The full operator guide, with sign-in, uploads, storage, archives and
retention, is
[`docs/install.md`](https://github.com/angler-ai/the-runbook-app/blob/main/docs/install.md).
