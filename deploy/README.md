# Container and docker compose (native pilot)

One image, `runbook-pilot:<tag>`, carries the server (`runbook-server`), the
CLI (`runbook`), the sign-in service (`auth/`, run by `serve` under Bun on a
loopback port and proxied at `/api/auth`) and the built web UI, which the
server hosts on its own origin: open `RUNBOOK_ORIGIN` (by default `http://127.0.0.1:8080`) in a
browser. It is built locally from the repository; there is no
public image or registry release (pilot non-claim).

| File | Purpose |
| --- | --- |
| `../Dockerfile` | Multi-stage build: Rust 1.99.1 builder (server, CLI, wasm browser compiler), Node 26.8.1 web and sign-in service builds, Bun 1.3.12 (binary only), `debian:bookworm-slim` runtime |
| `docker-compose.yml` | Server plus PostgreSQL 16 with a named volume; Mailpit only under the `smoke` profile |
| `postgres-init/` | First-start script: the server's unprivileged database role and its `runbook` schema |
| `docker-compose.external-db.yml` | Server alone against any PostgreSQL 14+ `DATABASE_URL` |
| `.env.example` | Every setting, placeholders only |
| `smoke/` | Restart and failure smoke tests (`smoke/run_all.sh`) |
| `platforms.md` | Running the image on Cloud Run, Fly.io, Render, Kubernetes or a VM: one port, several replicas, managed PostgreSQL (HOST1) |

## Quick start

```sh
cp deploy/.env.example deploy/.env && chmod 600 deploy/.env
# Fill in POSTGRES_PASSWORD and RUNBOOK_DB_PASSWORD (openssl rand -hex 24
# each), RUNBOOK_WEBHOOK_SECRET_KEY (openssl rand -base64 32), and for
# sign-in RUNBOOK_AUTH_SECRET (openssl rand -hex 32), RUNBOOK_SMTP_URL and
# RUNBOOK_SMTP_FROM.
dc() { docker compose -f deploy/docker-compose.yml --env-file deploy/.env "$@"; }
dc up -d --build --wait
dc exec server runbook-server bootstrap --owner-email you@example.test --owner-name "Your Name"
dc exec server runbook-server doctor
curl -s http://127.0.0.1:8080/v1/meta
```

`bootstrap --owner-email` creates the organization, the owner with that
verified address and the `default` project, and prints their IDs as one
JSON line; the owner then signs in with a code sent to that address. It
prints no secret unless you add `--with-token` (an owner personal access
token, `pat`, and a project service token, `service`, shown once: store
them in a password manager). Without sign-in configured, plain `bootstrap`
is the headless path: it always prints both tokens. A second `bootstrap`
fails with `bootstrap.already_initialized`. The sign-in variables are in
`docs/install.md` ("Sign-in settings (D48)") and `.env.example`.

The CLI is in the same image, so you can use it without installing anything:

```sh
read -rs RUNBOOK_TOKEN && export RUNBOOK_TOKEN   # paste a pat (bootstrap --with-token)
dc exec -e RUNBOOK_TOKEN -e RUNBOOK_HOST=http://127.0.0.1:8080 server runbook whoami
```

`-e RUNBOOK_TOKEN` (name only) passes the value from your shell without
putting it on a command line.

## Settings

All configuration is environment; nothing is baked into the image.

| Variable | Default | Notes |
| --- | --- | --- |
| `POSTGRES_PASSWORD` | required | Bundled database only: the administrator (superuser), for backups and maintenance. |
| `POSTGRES_USER`, `POSTGRES_DB` | `runbook` | Bundled database only. |
| `RUNBOOK_DB_USER` | `runbook_app` | Bundled database only: the unprivileged role the server connects as. |
| `RUNBOOK_DB_PASSWORD` | required | Bundled database only. URL-safe characters (`openssl rand -hex 24`). |
| `DATABASE_URL` | built from `RUNBOOK_DB_*` | Required for the external-database file. PostgreSQL 14+. |
| `DATABASE_URL_DIRECT` | unset | Direct endpoint for `LISTEN` behind a transaction-mode pooler. Never set it empty. |
| `RUNBOOK_WEBHOOK_SECRET_KEY` | required | 32 random bytes, base64 (`openssl rand -base64 32`). Seals webhook secrets at rest; keep it stable. |
| `RUNBOOK_ORIGIN` | `http://localhost:8080` | Public URL (scheme, host, port; no path). A plain `http://` origin on a non-loopback host gets a startup warning (tokens in clear text). |
| `RUNBOOK_BIND_ADDRESS` | `127.0.0.1` | Host interface the port is published on. |
| `RUNBOOK_TRUSTED_PROXIES` | unset | Comma list of addresses or CIDR blocks of the reverse proxies whose `X-Forwarded-For` / `Forwarded` names the client (per-IP sign-in limits, the device-login requester shown on the approval page). Empty trusts none. Behind a host TLS proxy on the loopback port, trust the Docker bridge (e.g. `172.16.0.0/12`). See `docs/install.md`, "Sign-in settings (D48)". |
| `RUNBOOK_HOST_PORT` | `8080` | Host port (`0` picks a free one). |
| `RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS` | `false` | Allow private, loopback and plain-http webhook targets. |
| `RUNBOOK_ALLOW_LINK_LOCAL_WEBHOOK_TARGETS` | `false` | Also allow link-local (cloud metadata) targets. |
| `RUNBOOK_MEMORY_LIMIT`, `POSTGRES_MEMORY_LIMIT` | `1g` | Memory caps per container (each is also capped at 200 processes). |
| `RUNBOOK_WEB_DIR` | `/usr/share/runbook/web` | The web UI the image ships, served on the API's origin with a strict CSP and no CORS: hashed assets immutable for a year, `index.html` `no-store`, other files revalidated by `ETag`/`Last-Modified` (304). Set it empty (`RUNBOOK_WEB_DIR=`) to serve the API alone. |
| `RUNBOOK_AUTH_SECRET`, `RUNBOOK_AUTH_METHODS`, `RUNBOOK_AUTH_ALLOWED_DOMAINS`, `RUNBOOK_SMTP_URL`, `RUNBOOK_SMTP_FROM`, `RUNBOOK_GOOGLE_CLIENT_ID`, `RUNBOOK_GOOGLE_CLIENT_SECRET` | unset | Sign-in (D48): off until `RUNBOOK_AUTH_SECRET` is set; empty means unset. See `docs/install.md`, "Sign-in settings (D48)". |
| `RUNBOOK_UPLOAD_SIGNING_KEY`, `RUNBOOK_USERCONTENT_ORIGIN` | unset | Evidence uploads (D49–D51): off until both are set; empty means unset. The origin must differ from `RUNBOOK_ORIGIN`, and beyond local testing by host name, not only by port (cookies ignore ports; the server warns); prefer another domain. See `docs/install.md`, "Upload settings (D49–D51)". |
| `RUNBOOK_USERCONTENT_HOST_PORT` | `8081` | Host port of the user-content listener (8081 in the container), published on `RUNBOOK_BIND_ADDRESS`. It serves only signed `/u/*` file URLs. |
| `RUNBOOK_IMAGE` | `runbook-pilot:local` | Tag compose builds and runs. |
| `RUNBOOK_LOG_FORMAT` | `text` | `json` writes one JSON object per line on stdout (`severity`, `message`, `time`, `component`). |
| `RUNBOOK_DATABASE_POOL_SIZE`, `RUNBOOK_AUTH_DATABASE_POOL_SIZE` | `16`, `5` | PostgreSQL connections per instance for the server and the sign-in service; lower them on small managed tiers (`platforms.md`, "Connection budget"). |
| `RUNBOOK_SHUTDOWN_TIMEOUT_SECS` | `8` | Longest stop after SIGTERM. |

| `RUNBOOK_CACHE_ID` | `default` | Build-time cache namespace (`--build-arg RUNBOOK_CACHE_ID=<unique-id>`). Isolates Cargo target-dir BuildKit cache mounts across worktrees. |

Compose sets `RUNBOOK_LISTEN_ADDR=0.0.0.0:8080` and the user-content
listener on 8081. Without `RUNBOOK_LISTEN_ADDR` the image listens on
`0.0.0.0:$PORT` (the image sets `PORT=8080` and
`RUNBOOK_LISTEN_PORT_FROM_ENV=1`; outside the image `PORT` alone is
ignored), and without
`RUNBOOK_USERCONTENT_LISTEN_ADDR` user content is served on that same port
to requests whose `Host` is `RUNBOOK_USERCONTENT_ORIGIN`'s, which is how a
one-port platform runs it (`platforms.md`). The healthcheck
(`/health/live`) follows the port; `/health/ready` also checks the database.
`RUNBOOK_MAX_REQUEST_BYTES`, `RUNBOOK_MAX_SOURCE_BYTES` and
`RUNBOOK_MAX_SKILL_BYTES` can be added to the server's `environment:` if you
need non-default limits.

## Operations

- **Upgrade:** rebuild the image and `dc up -d --build --wait`. `serve`
  applies pending migrations (forward-only, checksummed, under a database
  advisory lock) before it listens, so a new container on the same volume is
  the upgrade path and a restart re-applies nothing. A history that does not
  match the binary (`postgres.migration_history_mismatch`) stops the server
  instead of guessing; downgrades are not supported. Read
  `docs/release-notes.md` first. Upgrading to the HOST1 release: an external
  `DATABASE_URL` with `sslmode=require` no longer checks the database's
  certificate (libpq's meaning); use `sslmode=verify-full` for that.
- **Restarts and crashes:** timers, activations, claim expiry and webhook
  deliveries live in PostgreSQL behind generation-fenced leases, so a
  restart, `docker kill` or host reboot loses nothing and applies nothing
  twice. Work leased at the moment of a crash waits for its lease to expire
  (30 s for timers, 60 s for webhook deliveries) before another pass takes it.
- **Database restarts:** the server keeps running; pooled connections that
  the database closed are discarded and new ones opened on the next request
  or drain pass. Requests in flight during the restart fail and can be
  retried (mutations accept `Idempotency-Key`).
- **Database unavailable at start:** `serve` exits non-zero and the
  `unless-stopped` restart policy retries with Docker's backoff.
- **Stop:** `docker stop` sends SIGTERM through tini. The server stops
  answering readiness, ends live event streams, stops its background
  workers (releasing leases it has not processed), lets in-flight requests
  finish, stops the sign-in service with SIGTERM and exits 0, all within
  `RUNBOOK_SHUTDOWN_TIMEOUT_SECS` (8 s); anything still running then is
  abandoned and retried from PostgreSQL. A second signal exits at once.
- **Backups:** back up the `pgdata` volume or use `pg_dump` against the
  database (`dc exec postgres pg_dump -U runbook runbook`). Also keep
  `RUNBOOK_WEBHOOK_SECRET_KEY`; without it restored webhook secrets cannot be
  unsealed.
- **Logs:** `dc logs server`. The server prints error codes, never URLs or
  secrets; `RUNBOOK_LOG_FORMAT=json` makes each line a JSON object.
- **Several instances:** any number of server containers can share one
  database (leases, event IDs and shared rate limits keep work single);
  see `platforms.md`, "Several instances".

## External database

```sh
dc() { docker compose -f deploy/docker-compose.external-db.yml --env-file deploy/.env "$@"; }
dc up -d --build --wait && dc exec server runbook-server bootstrap --owner-email you@example.test
```

`DATABASE_URL` names a database the role can create the `runbook` schema in.
Beyond a private network use `sslmode=verify-full` (the image has CA
certificates; add `sslrootcert=<file>` for a provider CA that is not a public
one). `sslmode` has libpq's meaning in both the server and the sign-in
service: `require` encrypts but does not check the certificate, and the
server warns about it at start. Cloud SQL's Unix socket works as
`postgres://USER:PASSWORD@/DB?host=/cloudsql/PROJECT:REGION:INSTANCE`.
See `platforms.md`, "Database URLs".

- **Database on the Docker host:** use `host.docker.internal` as the host.
  Docker Desktop resolves it to the host. On native Linux the file's
  `extra_hosts: host.docker.internal:host-gateway` maps it to the bridge
  gateway, so PostgreSQL must listen on that interface (or `0.0.0.0`
  behind a firewall), not only on `127.0.0.1`, and `pg_hba.conf` must admit
  the bridge subnet.
- **Database in another container:** attach the server to that
  container's network with an extra `-f` file and use the container name in
  `DATABASE_URL` (as `smoke/external_db.sh` does with
  `smoke/external-db.network.yml`).

## Webhook receivers on the host

Both compose files map `host.docker.internal` to the host gateway, so a
receiver on the Docker host is `http://host.docker.internal:<port>/...` with
`RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS=true`. On native Linux the receiver
must listen on a non-loopback interface (`0.0.0.0` or the bridge address);
one bound to `127.0.0.1` is reachable only on Docker Desktop.

## Smoke tests

```sh
deploy/smoke/run_all.sh          # builds runbook-pilot:smoke once, ~2 minutes
deploy/smoke/crash_recovers.sh   # any single test
```

Needs docker with compose v2, `openssl`, `curl` and `python3`. Each script
uses its own compose project (`runbook-smoke-<test>-<pid>`), an ephemeral
loopback host port and freshly generated secrets, and always runs
`docker compose -p <that project> down -v --remove-orphans` on exit. It never
touches other containers, volumes or images; the `runbook-pilot:smoke` image
is left for reuse.

| Script | Verifies |
| --- | --- |
| `platform.sh` | The image as a platform runs it (`platform.override.yml`): two replicas start together on an empty database; each follows `PORT=9000` with no `RUNBOOK_LISTEN_ADDR`, serves user content on the same port by host (404 and the sandbox CSP for the API there), answers `/health/live` and `/health/ready` and logs JSON; a run published on one and started on the other fires its timer once; `docker stop` ends a replica with exit 0 in under 9 s |
| `fresh_start.sh` | Healthy stack; non-root; no secrets in the image or health log; `/v1/meta` on the host port; bootstrap once; CLI publish, start; the run's timer fires once and it succeeds |
| `sign_in.sh` | With the `smoke` profile's Mailpit: bootstrap by email prints no token; discovery lists `email`; a code requested through `/api/auth` arrives over SMTP and is read from Mailpit's local API; signing in sets an HttpOnly session cookie; `/v1/me` is a session for the bootstrapped owner; a cookie POST without `X-Runbook-CSRF` is 403 `auth.csrf` |
| `uploads.sh` | Uploads on (a signing key and a user-content origin on port 8081): a GPS-tagged JPEG generated by the script uploads to the user-content port, finalizes and comes back from its signed URL without EXIF or GPS, with `nosniff`, the sandbox CSP, an inline `Content-Disposition`, no cookie and a thumbnail; the API port refuses the PUT; text declared as PNG is refused at finalize (`upload.type_mismatch`) and deleted; the user-content port answers `/`, `/v1/meta` and discovery with 404 |
| `web_ui.sh` | `/` and a client route (`/tasks`, GET and HEAD) are `index.html` with the CSP, `no-referrer` and `no-store`; a hashed `/assets/*.js` is immutable; any `.wasm` the bundle ships is `application/wasm`; `/v1/does-not-exist`, a missing asset and traversal attempts are problem+json 404s; no `Access-Control-*` headers |
| `restart_keeps_state.sh` | `compose restart server` keeps a finished run and its events byte-identical; a pending timer fires exactly once after the restart |
| `crash_recovers.sh` | SIGKILL with three pending timers, then start: each fires exactly once, each run completes once |
| `db_restart.sh` | `compose restart postgres`: same server process, API recovers, a timer pending across the restart and a new run both complete |
| `upgrade_same_volume.sh` | Fresh server container on the same volume: identical migration history, still bootstrapped, runs and events persist, new runs work |
| `external_db.sh` | `docker-compose.external-db.yml` against a throwaway PostgreSQL on its own project-scoped network, given only `DATABASE_URL`: migrates it and runs |

Each script uses the CLI inside the server container and checks "exactly
once" against the stored event log (`TimerFired` and `ExecutionCompleted`
counts in `runbook.execution_event`), not only the run status.

`retention.sh` (ARC2) is the exception: it moves the archive job's clock with
`RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS`, which only debug builds accept (a
release build, like the image, refuses to start with it). So it builds a
debug `runbook-server`, runs it on the host against its own
`rb_test_retention_<pid>` database on `DATABASE_URL`'s server (created and
dropped by the script) and is not part of `run_all.sh`:

```sh
DATABASE_URL=postgres://<role that may create databases>@localhost:5432/<db> deploy/smoke/retention.sh
```

It checks that a finished run is archived only after its UTC day ends, that
the day's file holds the run with the secret from its logs redacted, and that
91 days later with `RUNBOOK_RETENTION_LIVE_DAYS=90` the live detail is gone
(report 410, rerun 422 `execution.inputs_expired`) while the archived run
still opens and the same bytes still download.

## Web e2e against the image

The browser suite (`web/e2e`, Playwright) also runs against this compose
stack, so the UI is tested as the image serves it (its CSP and caching
headers, the SPA fallback, the production bundle) rather than through Vite:

```sh
cargo build -p runbook-cli       # the specs drive the host CLI against the published port
cd web && npm ci --ignore-scripts && npx playwright install chromium
npm run e2e:container            # builds runbook-pilot:e2e (layer cache), ~45 s warm
E2E_IMAGE_REUSE=1 npm run e2e:container -- e2e/sign-in.spec.ts   # reuse the tag as is
```

It brings up `deploy/docker-compose.yml` as project `runbook-e2e-<pid>`
on a free loopback port with generated secrets (environment only, never
argv), bootstraps and seeds the pilot org (people's first tokens through
`docker compose exec -T postgres psql`), runs the same specs with
`E2E_BASE_URL` at the container, and always runs `docker compose -p
runbook-e2e-<pid> down -v --remove-orphans`, Ctrl-C included. It touches no
other container, volume or image and never pushes. Before each build it
checks with docker that `.dockerignore` keeps the harness's tokens and logs
(`web/e2e/.tokens/`, `web/e2e/.run/`; the file ignores every `web/e2e/.*`)
out of the build context. This is the CSP gate: any refusal under the
image's real CSP (a `securitypolicyviolation` event, or a page or worker
console report) fails the test that caused it and the run. See
`web/e2e/README.md` ("Container mode").

## Design decisions

- **Image:** two binaries, the built web UI (`/usr/share/runbook/web`,
  root-owned, read-only), CA certificates, `libssl3` (native-tls to
  PostgreSQL and webhook receivers), `curl` (healthcheck only) and `tini`
  (PID 1, signal forwarding, reaping); about 155 MB. The web stage runs
  `npm ci --ignore-scripts` and `vite build` after the wasm stage builds
  the browser compiler into `web/src/wasm/runbook.wasm`; neither Node nor
  the Rust toolchain reaches the runtime image. User `runbook`, uid/gid
  10001, no shell login. Builder pinned to `rust-toolchain.toml`'s 1.99.1;
  cache mounts keep the registry and target dir between local builds without
  landing in the image. Pass `--build-arg RUNBOOK_CACHE_ID=<unique-id>` (or set
  `RUNBOOK_CACHE_ID` in the environment when using compose) to isolate the Cargo
  target caches across concurrent checkouts or worktrees.
- **No entrypoint script:** `serve` already migrates idempotently before
  listening, and bootstrap must stay an explicit one-time command because it
  prints secrets. Admin subcommands run as `exec server runbook-server <cmd>`.
- **Network:** PostgreSQL is on an `internal: true` network and never
  published (a developer database often owns 5432). The server is on that
  network plus `edge` for its published port and outbound webhooks; the port
  binds to loopback by default because the server speaks plain HTTP. Put a
  TLS reverse proxy in front for anything else, set `RUNBOOK_ORIGIN` to
  its `https://` URL and `RUNBOOK_TRUSTED_PROXIES` to the address the proxy
  connects from (through Docker's published port, the bridge gateway), so
  per-IP limits see each client rather than the proxy.
- **Required secrets fail closed:** compose refuses to start without
  `POSTGRES_PASSWORD`, `RUNBOOK_DB_PASSWORD` and `RUNBOOK_WEBHOOK_SECRET_KEY`
  (`${VAR:?}`), so no default credential or empty key can ship.
- **Least-privilege database role:** the server connects as
  `RUNBOOK_DB_USER`, which is not a superuser, cannot create databases or
  roles, and owns the `runbook` schema and its objects
  (`postgres-init/10-runbook-app-role.sh`, run by the postgres image on the
  volume's first start). It keeps `CREATE` on this one database because
  the first migration's `CREATE SCHEMA IF NOT EXISTS runbook` is
  authorized against the database even when the schema exists.
  `POSTGRES_USER` stays the administrator.
  `fresh_start.sh` checks the role and the schema's owner.
- **Container hardening:** both containers run with a read-only root
  filesystem (small `tmpfs` mounts for `/tmp`, the PostgreSQL socket
  directory and the CLI's home), `no-new-privileges`, and memory and
  process limits. The server drops every capability; PostgreSQL keeps only
  what its entrypoint needs to fix volume ownership and drop to the
  `postgres` user (`CHOWN`, `DAC_OVERRIDE`, `FOWNER`, `SETGID`, `SETUID`).
- **Pinned images:** `postgres:16-alpine` is pinned by the digest it
  resolved to (the multi-platform index); bump the tag and digest together.
  The Dockerfile's `node:26.8.1-bookworm-slim` is pinned the same way.
  Its `rust:1.99.1-slim-bookworm` and `debian:bookworm-slim`
  are pinned by tag only: no local copy was available to read their
  digests from, and none is guessed. Pin them by digest when you build
  release images.

### Upgrading a stack created before the unprivileged role

The init script runs only on an empty volume. On an existing volume the
server would now connect as a role that does not exist. Once, before
`up` with the new files (as the administrator; the password is read from
your shell, never put on a command line):

```sh
dc up -d postgres
read -rs RUNBOOK_DB_PASSWORD && export RUNBOOK_DB_PASSWORD   # also put it in deploy/.env
dc exec -T -e RUNBOOK_DB_PASSWORD postgres sh -c 'psql -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" -v pw="$RUNBOOK_DB_PASSWORD"' <<'SQL'
CREATE ROLE runbook_app LOGIN PASSWORD :'pw' NOSUPERUSER NOCREATEDB NOCREATEROLE;
GRANT CONNECT, TEMPORARY, CREATE ON DATABASE runbook TO runbook_app;
ALTER SCHEMA runbook OWNER TO runbook_app;
SELECT format('ALTER TABLE runbook.%I OWNER TO runbook_app', tablename)
  FROM pg_tables WHERE schemaname = 'runbook' \gexec
SELECT format('ALTER SEQUENCE runbook.%I OWNER TO runbook_app', sequencename)
  FROM pg_sequences WHERE schemaname = 'runbook' \gexec
SQL
```

(Adjust `runbook_app` and `runbook` if you changed `RUNBOOK_DB_USER` or
`POSTGRES_DB`.) Then `dc up -d --wait` as usual.
- **No graceful drain on SIGTERM:** correctness never depends on an orderly
  shutdown (the crash test proves SIGKILL is safe), so stopping is immediate.
