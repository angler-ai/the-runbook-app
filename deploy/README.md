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

## Optional Docker runner

Start the core with its normal Compose file and signing key. Configure an
HTTPS reverse proxy from the runner's public base URL to port 8090 on the
Compose host. Publish only `/runner/v1/` to the core and, if using webhook
wakes, `/runner/v1/webhooks/core`. The core must resolve and reach that URL.
If it resolves to a private address, enable
`RUNBOOK_RUNNER_PRIVATE_NETWORKS=true` on the core.

With Node.js 22.18 or newer, run setup from the repository checkout. Build the sandbox image on the same Docker
daemon and run the one-time registration as an org admin. The
bootstrap URL and the registration URL must match (use the origin with no
trailing slash). Use the same Compose project name for setup and startup.

Before you register, check these:

- **A token with `runner:admin`.** No token gets the runner scopes unless
  they are asked for, including the owner token from `bootstrap --with-token`
  and a plain `runbook login`. `runbook runner bootstrap` then fails with
  `auth.scope_missing`. Sign in as an org owner or admin with
  `runbook login --scope runner:read --scope runner:admin` (add the other
  scopes you need with more `--scope` flags) and approve it in the browser.
- **`CORE_URL` is the core's public origin.** Use `RUNBOOK_ORIGIN` for
  Docker or `ORIGIN` for the Cloudflare API Worker. The runner checks that the core's
  signed grants come from `CORE_URL`, so use the core's public origin. Setup
  calls it from this host, and the runner containers call it from the
  `runner-egress` network. A core on plain http also needs
  `RUNNER_INSECURE_CORE_HTTP=true` (above).
- **A trusted certificate on the runner URL.** The core checks the runner's
  certificate against the public web roots, and setup accepts only an https
  `RUNNER_HTTPS_URL`. A private CA is not trusted. On a machine with no
  public host name, a tunnel that gives you a public https name works,
  for example a Cloudflare Tunnel to `http://127.0.0.1:8090`.
- **`RUNNER_VERSION`** is the runner release you run, the `version` in
  `runner/package.json` for a local build (for example `0.1.2`).

```sh
export COMPOSE_PROJECT_NAME=runbook
export RUNNER_VERSION=0.1.2
export RUNNER_IMAGE="runbook-runner:$RUNNER_VERSION"
export RUNNER_SANDBOX_IMAGE="runbook-sandbox:$RUNNER_VERSION"
docker build -f runner/Dockerfile -t "$RUNNER_IMAGE" .
docker build -f runner/sandbox.Dockerfile -t "$RUNNER_SANDBOX_IMAGE" .
runbook runner bootstrap --org "$ORG_ID" --url "$RUNNER_HTTPS_URL" |
  node deploy/runner-install/setup.mjs "$CORE_URL" "$RUNNER_HTTPS_URL" "$ORG_ID" "$RUNNER_VERSION"

export RUNNER_KEK_UID="$(id -u)" RUNNER_KEK_GID="$(id -g)"
# Linux: set RUNNER_DOCKER_GID to the group that owns the daemon socket.
# Docker Desktop ordinarily uses group 0 inside the VM.
docker compose -f deploy/docker-compose.yml --env-file deploy/.env --profile runner up -d
runbook runner status --org "$ORG_ID" --json
```

On a single machine with the core on `http://localhost` or a `*.localhost`
name, the runner containers can't reach that origin. Add a Compose override
that gives the server that name on the runner network, and use the same
port inside and outside the container (for example
`RUNBOOK_ORIGIN=http://runbook.localhost:8080`):

```yaml
services:
  server:
    networks:
      internal: {}
      edge: {}
      runner-egress:
        aliases: [runbook.localhost]
```

The commands above build both images from the release checkout. To use a
registry image instead, set `RUNNER_IMAGE` to the corresponding versioned
image. The external-database Compose variant includes the same runner profile
and gives the runner its own database.

Bootstrap writes the registration and separate process credentials to
`deploy/runner-config/` with owner-only permissions. Store that directory
and the database volume backups securely together. Only the vault mounts
the local KEK file, and only sandbox-ctl mounts the Docker socket. Docker
is one trust domain. The internal service calls use plain HTTP on the
Compose network. Core HTTPS is required by default. For a local core over
HTTP, set `RUNNER_INSECURE_CORE_HTTP=true` before registration.

Port 8090 binds to loopback by default. If the TLS proxy cannot reach
loopback, set `RUNNER_API_BIND` to the proxy-facing interface address.
Use `RUNNER_GATEWAY_CONTROL_IP` and `RUNNER_INTERNAL_SUBNET` together to
avoid a local subnet conflict. If configuring signed core webhook wakes,
set `RUNNER_CORE_WEBHOOK_SECRET` in `deploy/runner-config/api.env` to the
signing secret from the core subscription. The scheduler also sweeps
every 60 seconds.

Model profiles can set `options.max_response_bytes` to cap successful streaming
provider responses. The default is 16 MiB plus 1 KiB per allowed output
token, which accounts for per-token stream framing. Individual stream events
remain capped at 1 MiB. Error bodies and non-streaming responses have a
16 MiB ceiling.

### Upgrade to 0.1.1

1. Back up and update the core and sign-in service first. Check
   `curl -fsS "$CORE_URL/v1/meta"` for `runner_contract_version` **4** or higher.
2. Back up the runner database and configuration. Stop all old runner
   services and let their writes finish before migrating the database.
   Migration `0018` removes permissions used by the old credential writer.
   Migration `0027_task_claim_ledger.sql` links reservations to task claims
   so the core can record settled charges after an interrupted claim.
   Refresh the environments and start the new runner components together.
   Compose waits for the runner migrations before starting the API.
3. Check runner status and the vault identity, then verify sign-in before
   enabling [MFA](https://docs.therunbook.app/security/multi-factor/).
   An owner should enroll a factor and preview policy lockouts first. If a
   CLI token needs replacement, approve a new login with the required scopes
   in the browser after a fresh MFA check.

Services that verify core JWTs refresh the JWKS every five minutes on demand.
During a core outage they can use a previously fetched key for up to one hour
after the last successful fetch. Set `RUNNER_JWKS_MAX_AGE_SECONDS` on verifier
services to change this deadline, up to 24 hours. A shorter deadline limits how long a removed
key can remain trusted during an outage and can interrupt valid traffic sooner.
Unknown keys and known keys past the deadline fail closed.

With the runner stopped, update the generated Compose environments using
`node deploy/runner-install/setup.mjs --upgrade "$CORE_URL" "$RUNNER_HTTPS_URL" "$ORG_ID" "$RUNNER_VERSION"`.
Setup checks the environment for each runner process before saving it. The
upgrade preserves the existing database passwords, process tokens, and KEK.
Set `RUNNER_IMAGE` when starting Compose if using a locally built runner.
Set `RUNNER_SANDBOX_IMAGE` during `--upgrade` to adopt the new sandbox image
in the worker and sandbox-controller configurations.
Runner peers use port 8094 on the Compose network. The reverse proxy continues
to use the published port 8090.

### Vault identity enrollment and rotation

Fresh setup registers the vault's Ed25519 public key using the one-time
bootstrap token and prints its SHA-256 fingerprint. Keep the installer
trusted during initial enrollment. The vault derives its initial identity
from the first local KEK with a separate HKDF context, then stores it
KEK-wrapped in the runner database. Compare the installer fingerprint with
`vault_identity_fingerprint` from `runbook runner status --org "$ORG_ID" --json`
or with the fingerprint in the web overview before entering credentials.

For a previously installed runner, read its public key directly on the
trusted vault host. Have an organization owner register that key with the
core before anyone enters credentials.

```sh
VAULT_PUBLIC_KEY="$(docker compose -f deploy/docker-compose.yml --env-file deploy/.env --profile runner \
  exec -T runner-vault node /app/bin/runner.mjs vault-identity)"
printf '%s' "$VAULT_PUBLIC_KEY" |
  node -e 'let key = require("node:fs").readFileSync(0, "utf8").trim(); console.log(require("node:crypto").createHash("sha256").update(Buffer.from(key, "base64url")).digest("hex"))'
runbook runner vault-identity --org "$ORG_ID" --public-key "$VAULT_PUBLIC_KEY"
runbook runner status --org "$ORG_ID" --json
```

Compare the fingerprint computed from the public key with runner status.
Resolve any mismatch on the trusted vault host before entering credentials.

To rotate, stop every vault replica and pause credential entry. Use
`docker compose ... run --rm --no-deps runner-vault vault-identity --rotate`
with the same Compose options and database. This replaces the stored signing
key and prints the replacement public key. Register that key as an
organization owner, then restart the vault. If owner registration fails, run
`vault-identity` again without `--rotate` to recover the current public key.
Update any independent client pins before resuming credential entry.

Open reservations created before migration 0019 can settle through the
authenticated gateway or sandbox controller for their reservation kind.
Reservations created by the new API require the original opening capability.

### Credential creation

Upgrade runner-api and the vault before using the updated browser or CLI to
create credentials. The vault signs a creation receipt with its identity key.
The browser and CLI verify it against the key registered with the core before
using the returned credential ID. When the CLI submits a change for approval,
it returns a `proposal_id`.

Use `runbook runner credential types` to list config and secret fields.
`runbook runner credential set CREDENTIAL FIELD` takes `key` for `api-key`,
`api_key` for `model-provider`, and `token` for `mcp-bearer`. Keep secret
values out of metadata files. See [sealed entry](#credential-rotation-from-the-cli)
for the prompt and standard input options.

### Configure a runner with the CLI

Use the runner-scoped CLI login from setup. For each JSON file, use
configuration metadata without secret values.

```sh
runbook runner group list
runbook runner group projects "$GROUP_ID" --file projects.json
runbook runner policy update --file policy.json
runbook runner credential create --file credential.json --json
runbook runner credential set "$CREDENTIAL_ID" api_key
runbook runner credential test "$CREDENTIAL_ID"
runbook runner credential label "$CREDENTIAL_ID" read
runbook runner credential share "$CREDENTIAL_ID" --file sharing.json
runbook runner provider create --file provider.json
runbook runner profile create --file profile.json
runbook agents create --project "$PROJECT_ID" --file agent.json
runbook runner agent-profiles "$AGENT_ID" --project "$PROJECT_ID" --file profiles.json
```

In the example, the credential type is `model-provider`. Use its ID in
provider metadata and the returned provider ID in the profile. Share all
three with the project. For `group projects`, provide the complete
`project_ids` array. For `credential share`, provide `sharing.projects`.
For `agent-profiles`, supply the complete `profiles` array of profile IDs.
Check the policy with `runbook runner policy get` and choose
[a small initial step budget with headroom for the model](https://docs.therunbook.app/runner/limits/).

For an upstream MCP server, use `runbook runner mcp-server create --file mcp.json`
and `runbook runner mcp-server discover "$SERVER_ID" --json`. Review the tools,
pin the returned snapshot with `runbook runner mcp-server pin "$SERVER_ID" "$SNAPSHOT_ID"`,
and set their labels with `runbook runner mcp-server label-tools "$SERVER_ID" --file labels.json`.
The labels file contains a `tool_labels` map. Use `never_allowed` on the server
to exclude tools and grant the agent only the tools it needs.

The [runner setup guide](https://docs.therunbook.app/runner/setup/#configure-the-runner)
has provider and profile examples and a first-run checklist. Sign in
separately to use the web app, which refuses CLI tokens with runner scopes
or `project:admin`.

### A second runner stack on one Docker daemon

Each core permits one runner installation per organization. Register the
second stack to a different organization or a separate core instance.
Each stack needs its own copy of the Compose file and `runner-install/`
(setup writes `../runner-config/` next to `setup.mjs` and imports
`../../runner/`, so keep that layout), and its own `COMPOSE_PROJECT_NAME`,
`RUNNER_API_PORT` and HTTPS origin, `RUNNER_INTERNAL_SUBNET` with `RUNNER_GATEWAY_CONTROL_IP`,
and `RUNNER_SANDBOX_SUBNET_POOL` (add it to `runner-config/sandbox-ctl.env`;
setup doesn't write it). For example, if the first stack uses the default
subnets, the second can use port `8091`, internal subnet `172.31.90.0/24`,
gateway IP `172.31.90.10`, and sandbox pool `10.204.0.0/16` if those ranges
are free on your host. If both stacks run a core, use separate core listen
ports and public origins. Every sandbox controller removes all containers
labeled `runbook.sandbox` when it starts, so starting one stack ends the
other stack's running sandboxes. Restart a stack only while the other is idle.

### Credential rotation from the CLI

For unattended `runbook runner credential set`, redirect standard input
from a protected secret file and pass either
`--binding-pin` with the verified destination hash or `--expected-config`
with a local JSON document containing the expected `type`, `config`, and
`groups`. Interactive rotation displays the normalized destination and
asks for confirmation before it reads the secret from a hidden prompt.

```sh
runbook runner credential set "$CREDENTIAL_ID" "$FIELD" \
  --expected-config credential.json < "$SECRET_FILE"
# Or use the independently verified destination hash.
runbook runner credential set "$CREDENTIAL_ID" "$FIELD" \
  --binding-pin "$BINDING_HASH" < "$SECRET_FILE"
```

The CLI uses the registered vault identity to authenticate the ingest key.
An independently verified ingest-key fingerprint can be supplied with
`--pin`. It differs from the enrolled vault-identity fingerprint.

For a draft credential created before the atomic policy upgrade, open the
credential and save its intended label and sharing before sealing a secret.
New direct creates apply the policy in the vault transaction that creates
the credential. A credential created by an approved agent proposal also
needs an administrator to confirm its label before first activation.

### Reconcile model usage from the Compose host

The runner API exports `runbook_runner_pending_reservations` on its metrics endpoint.
A host operator with access to the runner API container can reconcile
pending charges using the container's scheduler database credentials.
Replace the IDs and measured usage in the example with the record returned
by `list` and the provider's billing evidence.

```sh
dc exec -T runner-api node /app/bin/runner.mjs ledger-reconcile list
dc exec -T runner-api node /app/bin/runner.mjs ledger-reconcile apply <<'JSON'
{"reservation_id":"RESERVATION","step_run_id":"STEP_RUN","runner_id":"RUNNER","actual_usd":0.01,"usage":{"input_tokens":300,"output_tokens":50,"cached_tokens":0,"cache_write_tokens":0}}
JSON
```

The local command matches the reservation, step run and runner, and works
with pre-0019 reservations if the charge was provisionally settled and its
usage event is linked. It audits corrections and accepts an identical retry.
If the measured charge exceeds the original hold, pass `apply --override-hold`.
The audit row records that the operator used an override.

If `list` shows `usage_event_linked: false`, no usage event can be attributed
to that reservation automatically. Identical historical settlements can
produce this result. The provisional charge remains pending. A database
operator must use provider billing evidence to identify the correct usage
event, verify any sibling links, and set its `reservation_id` before running
`apply`. Leave the charge pending if the evidence cannot distinguish the events.

### Recover an interrupted Compose registration

If setup reports an invalid runner join reply, repeat setup with the original
arguments. It uses the private `deploy/runner-config/registration.json` to
retry the same join without a bootstrap token.

If setup reports an invalid registration reply before that file was saved,
run `runbook runner status --org "$ORG_ID"`. If no install appears, generate a
new bootstrap token for the same URL and repeat setup. If the install appears,
send the organization and install IDs to the core operator. The registration
tokens are stored as hashes on the core and cannot be recovered from status.

For a confirmed orphan with no joined runners, the core database operator
should back up the database, then run the following in `psql` with `org_id` and
`install_id` set to the confirmed IDs. Roll back if the first query fails to
find the expected install or if the final DELETE affects anything other than
one row.

```sql
BEGIN;
SELECT id, org_id, url FROM runbook.runner_installs
  WHERE org_id = :'org_id' AND id = :'install_id' FOR UPDATE;
DELETE FROM runbook.runner_vault_identity_history
  WHERE install_id = :'install_id' AND EXISTS (
    SELECT 1 FROM runbook.runner_installs WHERE org_id = :'org_id' AND id = :'install_id'
  ) AND NOT EXISTS (
    SELECT 1 FROM runbook.runner_runners WHERE install_id = :'install_id'
  );
DELETE FROM runbook.runner_installs
  WHERE org_id = :'org_id' AND id = :'install_id' AND NOT EXISTS (
    SELECT 1 FROM runbook.runner_runners WHERE install_id = :'install_id'
  );
-- COMMIT only after verifying the results.
```

After the operator clears the orphan, generate a fresh bootstrap token with
`runbook runner bootstrap` for the original runner URL and repeat setup.
