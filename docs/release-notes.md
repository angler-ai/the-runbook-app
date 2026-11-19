# Release notes

Changes an operator should read before upgrading, newest first.

## Unreleased

Nothing yet.

## 0.1.0

The first release of The Runbook: the server and web UI (one container
image), the Cloudflare edition, the CLI for Linux and macOS, and the
deployment modules for Docker, Cloud Run-style platforms, GKE and
Cloudflare. Install the CLI with
`curl -fsSL https://github.com/angler-ai/the-runbook-app/releases/latest/download/install.sh | sh`;
start a server from `site/` (Getting started) or `docs/install.md`.

A new install needs nothing below. The rest is for anyone running a build
from before this release.

### Strict assignment and take-over (LANG7)

- A task may set `assignment: strict`: only its assignee may claim it
  (agents too, unless the task is `actor: agent`). A person may take over
  a ready agent step (`runbook step claim --take-over`, or Take over in the
  web).
- **Update every server instance (or the api Worker) before anyone
  publishes a strict runbook or takes a step over, and do not roll back
  past this release once they have:** older releases cannot read the new
  run events (`StepAssigneeResolved`, `StepTakenOver`), so such runs fail
  to load there until you update again. Nothing is lost. See
  `docs/install.md` §11 and `cloudflare/README.md` §0a.

### Updating and releases (UPD1)

- **One command updates an install.** Docker: `deploy/update.sh` (backup
  read back in full, health check, rollback; `docs/install.md` §11).
  Cloudflare: `npm run update` in `cloudflare/` (README §0a). Both install
  attested releases (`gh attestation verify`, pinned to the release
  workflow and tag) or build a tag from a checkout.
- `/v1/meta` reports `version` and `schema`; `runbook-server --version`
  prints the release, the Postgres schema and the rollback floor;
  `runbook version` shows the CLI's and the server's.
- **A server now refuses a database a newer release migrated**
  (`postgres.schema_newer_than_code`) unless
  `RUNBOOK_ALLOW_NEWER_SCHEMA=true`, which the update's rollback sets.
  Releases before this one cannot be rolled back to after a newer
  release's migrations without restoring a backup.

### Container edition on any platform (HOST1)

- **`sslmode` now has libpq's meaning, in the server and the sign-in
  service.** `require` encrypts the connection but no longer checks the
  database's certificate; `verify-full` checks the certificate and the host
  name, `verify-ca` the certificate only. **If your `DATABASE_URL` (or
  `DATABASE_URL_DIRECT`) says `sslmode=require` and you want the certificate
  checked, change it to `sslmode=verify-full`**, adding
  `sslrootcert=<CA file>` when your provider's CA is not a public one. The
  server warns at start about each URL that is encrypted but unverified.
- One port is enough: without `RUNBOOK_USERCONTENT_LISTEN_ADDR`, user
  content is served on the main port to requests for the user-content host.
  That host must differ from the app's by host name, not only by port.
- The image follows the platform's `PORT` (it sets
  `RUNBOOK_LISTEN_PORT_FROM_ENV=1`); it no longer sets `RUNBOOK_LISTEN_ADDR`.
- New: `/health/live` and `/health/ready`, graceful shutdown on SIGTERM
  (`RUNBOOK_SHUTDOWN_TIMEOUT_SECS`, default 8), `RUNBOOK_LOG_FORMAT=json`,
  `RUNBOOK_AUTH_DATABASE_POOL_SIZE`, Cloud SQL Unix sockets.
- Migration `0031` adds `runbook.rate_window`: rate limits are shared by
  every instance on the database.

See `deploy/platforms.md`.
