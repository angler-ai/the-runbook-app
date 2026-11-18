# Clean install: the native pilot

This guide installs The Runbook's native pilot from nothing and runs the
flagship `post-deploy` runbook end to end with real identities:

- the server on PostgreSQL, either with `docker compose` (path A) or as a
  native binary against any PostgreSQL 14+ database (path B);
- sign-in by email code (D48), and the first owner, created by the
  server's local `bootstrap --owner-email` command, who signs in with that
  address;
- a `pilot` project, three people (Alice, Bob, Carol) with their own
  tokens, teams `oncall` (Alice, Bob) and `eng-leads` (Carol);
- a launcher service token, a recording webhook receiver and the GitHub
  Actions relay (dispatcher) with its allow-lists;
- the flagship published with its skills, then run: an agent session works
  through `runbook run token` and `runbook step`, Alice ticks the human
  checklist, Bob approves with `runbook step decide`.

What the pilot supports and does not claim is in
[pilot-profile.md](pilot-profile.md). The same steps are automated by
[`examples/pilot/setup.sh`](../examples/pilot/setup.sh) and
[`run-flagship.sh`](../examples/pilot/run-flagship.sh) (section 9).

## 0. Before you start

You need a checkout of this repository (run every command from its root),
`python3` 3.9+, `curl` 7.76+ and `openssl`, the Rust toolchain from
`rust-toolchain.toml` (for the `runbook` CLI, which every section after 2
runs on your machine), plus either Docker with Compose (path A) or a
PostgreSQL 14+ database you own (path B). The walkthrough plays every person on one
machine: each gets a private CLI config directory under `$PILOT_DIR/config/`
so their logins stay apart. On real machines, people skip
`XDG_CONFIG_HOME` and use their own default config.

Secrets never go on a command line or into this guide's output. Every token
and signing secret is written once to a 0600 file in `$PILOT_DIR`, using
[`pilot-json.py`](../examples/pilot/pilot-json.py) on the CLI's `--json`
output (`keep` stores a field and prints the rest).

```sh
export PILOT_DIR="$HOME/runbook-pilot"
export PILOT_HOST=http://127.0.0.1:8080
mkdir -p "$PILOT_DIR" && chmod 700 "$PILOT_DIR"
pj() { python3 -I examples/pilot/pilot-json.py "$@"; }
```

Open a new terminal for each person later on; repeat these four lines in
each, with the `PATH` export from section 1 (they hold no secrets).

## 1. Start the server

### Path A: docker compose

The compose file `deploy/docker-compose.yml` runs `server` and `postgres`.
Fill in `deploy/.env` as `deploy/README.md` describes (at least the
Postgres administrator's password `POSTGRES_PASSWORD`, the password of the
unprivileged role the server connects as, `RUNBOOK_DB_PASSWORD`, and
`RUNBOOK_WEBHOOK_SECRET_KEY`, 32 random bytes in
base64 that seal webhook secrets at rest; keep it). The receivers in this
guide run on your machine, so also set
`RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS=true` there. Then build and start the
stack; `--wait` returns once both services report healthy, so the server has
applied its migrations before the next commands run. The image also carries
the CLI, but this guide runs `runbook` on your machine (it reads local files
and keeps each person's login in a local config directory), so build that
first:

```sh
cargo build --release --locked -p runbook-cli
export PATH="$PWD/target/release:$PATH"
docker compose -f deploy/docker-compose.yml --env-file deploy/.env up -d --build --wait
docker compose -f deploy/docker-compose.yml --env-file deploy/.env exec -T server runbook-server doctor
docker compose -f deploy/docker-compose.yml --env-file deploy/.env exec -T server runbook-server migrate
curl -fsS "$PILOT_HOST/v1/meta"
```

`doctor` checks the database connection, schema version and blob tables.

### Path B: native binary and an external PostgreSQL

Any PostgreSQL 14+ works (a local server, RDS, Cloud SQL, Neon, ...). The
server creates the `runbook` schema (migration 0001 runs `CREATE SCHEMA`),
so the role in `DATABASE_URL` needs the `CREATE` privilege on the database
(`GRANT CREATE ON DATABASE runbook TO runbook;`), or must own it. Add `?sslmode=verify-full` for a managed database
(`sslmode` has libpq's meaning: `require` encrypts without checking the
certificate; add `sslrootcert=<file>` for a provider CA that is not a public
one; see `deploy/platforms.md`, "Database URLs"). If `DATABASE_URL`
goes through a transaction-mode pooler, also set `DATABASE_URL_DIRECT` to a
direct connection for the live-event listener.

```sh
cargo build --release --locked -p runbook-server -p runbook-cli
export PATH="$PWD/target/release:$PATH"
(umask 077 && openssl rand -base64 32 > "$PILOT_DIR/webhook-secret-key")
export DATABASE_URL='postgres://runbook:CHANGE_ME@db.example.internal:5432/runbook?sslmode=verify-full'
export RUNBOOK_ORIGIN="$PILOT_HOST"
export RUNBOOK_LISTEN_ADDR=127.0.0.1:8080
export RUNBOOK_WEBHOOK_SECRET_KEY="$(cat "$PILOT_DIR/webhook-secret-key")"
export RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS=true
runbook-server serve
```

Leave `serve` running (it applies migrations, then serves `/v1`, runs
timers and delivers webhooks). In a second terminal with the same
environment:

```sh
runbook-server doctor
runbook-server migrate
curl -fsS "$PILOT_HOST/v1/meta"
```

`runbook-server migrate` applies only the schema; `serve` also migrates at start by default.

Server settings, all from the environment:

| Variable | Meaning |
| --- | --- |
| `DATABASE_URL` | PostgreSQL 14+ connection URL (required) |
| `DATABASE_URL_DIRECT` | Direct connection for `LISTEN` when `DATABASE_URL` is a transaction pooler |
| `RUNBOOK_DATABASE_POOL_SIZE` | Most pooled Postgres connections the server holds (default 16, 1 to 1000); keep it at 12 or more, since each of the 8 concurrent requests may hold a run lock while it reads. Event streams add one `LISTEN` connection each |
| `RUNBOOK_LIMIT_RUN_BYTES` | Most a run's event log may hold before a request's data-carrying write (a report, signal, decision or override over 4 KiB) is refused with 422 `execution.too_large` (default 8388608, 8 MiB; a positive number of bytes). Every request on a run loads all of it. Small writes (claims, a cancel, a short report) always pass. One and a half times it is the hard ceiling (CF13b): a run whose log passes it through the engine's own events (call results, `set` steps) or small writes is faulted with `limit.run_size_exceeded` and closes failed |
| `RUNBOOK_LIMIT_STEP_RUNS` | Most step runs one run may open (a positive number; unset or `off`, the default, is no cap). A run that would open one more fails with `limit.step_runs_exceeded`, keeping what it did, and the run page says why. `/v1/meta` reports it as `limits.step_runs`, and the editor warns when a runbook's worst case (its step budget) is over it; publishing is still allowed. Loops, `foreach` and `parallel` keep their fixed ceilings (50, 100, 20) so a runbook runs the same everywhere |
| `RUNBOOK_ORIGIN` | Public origin (`scheme://host[:port]`, no path); required |
| `RUNBOOK_LISTEN_ADDR` | Listen address (default `127.0.0.1:8080`) |
| `RUNBOOK_WEBHOOK_SECRET_KEY` | 32 bytes, base64; without it webhook routes answer `503 webhooks.not_configured` |
| `RUNBOOK_ALLOW_PRIVATE_WEBHOOK_TARGETS` | `true` allows private, loopback and plain-http webhook URLs (self-hosted only) |
| `RUNBOOK_ALLOW_LINK_LOCAL_WEBHOOK_TARGETS` | `true` also allows link-local targets (cloud metadata addresses); leave unset |
| `RUNBOOK_MAX_REQUEST_BYTES`, `RUNBOOK_MAX_SOURCE_BYTES`, `RUNBOOK_MAX_SKILL_BYTES` | Optional lower limits ([pilot-profile.md](pilot-profile.md#runtime-limits)) |
| `RUNBOOK_MIGRATE_ON_START` | `true` (default) applies pending migrations before serving; `false` only checks that the schema is current and refuses to start when it is not. Use `false` when a separate `runbook-server migrate` step (for example a Kubernetes Job) runs first |
| `RUNBOOK_WEB_DIR` | The built web UI (`web/dist`) to serve on the same origin; unset or empty serves the API alone. The container image sets it |

### Upload settings (D49–D51)

Evidence uploads (photos, screenshots, logs and documents attached to a
step) are off until a signing key and a user-content origin are set; the
API then advertises `capabilities.uploads: true` in `/v1/meta`. Uploaded
files are never served from `RUNBOOK_ORIGIN`: `serve` starts a second
listener for the user-content origin that answers only signed `/u/*` URLs,
with `Content-Disposition`, `X-Content-Type-Options: nosniff`,
`Content-Security-Policy: sandbox; default-src 'none'; frame-ancestors 'none'`
and `X-Frame-Options: DENY`, and never reads or sets a cookie. Images are re-encoded at finalize, which removes EXIF
(including GPS location) and every other piece of metadata, and get a
320 px thumbnail; images wider or taller than 8,000 px, or larger than
4,096 × 4,096 pixels in total, are refused (clients shrink them first, as
`/v1/meta` advertises). One image is processed at a time; a finalize that
would wait answers `503 server.busy` with `Retry-After`.

| Variable | Meaning |
| --- | --- |
| `RUNBOOK_UPLOAD_SIGNING_KEY` | 32 bytes, base64 (`openssl rand -base64 32`); signs upload and file URLs (15 minutes to upload, one to two hours to read). Keep it stable; changing it invalidates outstanding URLs only. Empty means unset |
| `RUNBOOK_USERCONTENT_ORIGIN` | Public origin of user files (`scheme://host[:port]`, no path), e.g. `https://files.runbooks.example`. Must differ from `RUNBOOK_ORIGIN` or uploads stay off (startup warning). Beyond local testing it must also differ by host name, not only by port: cookies ignore ports, so the app's cookies would reach user content (startup warning). Prefer a separate domain to a subdomain of the app. Empty means unset |
| `RUNBOOK_USERCONTENT_LISTEN_ADDR` | A second listener for that origin, e.g. `127.0.0.1:8081`; route the user-content origin to it through your TLS proxy. Unset (HOST1), the main listener serves user content to requests whose `Host` is the user-content origin's, so one port carries both (the proxy must pass `Host` through; the user-content host must differ from the app's). Ignored without `RUNBOOK_USERCONTENT_ORIGIN` |

Path B, with the proxy routing `https://files.runbooks.example` to port 8081:

```sh
(umask 077 && openssl rand -base64 32 > "$PILOT_DIR/upload-signing-key")
export RUNBOOK_UPLOAD_SIGNING_KEY="$(cat "$PILOT_DIR/upload-signing-key")"
export RUNBOOK_USERCONTENT_ORIGIN=https://files.runbooks.example
export RUNBOOK_USERCONTENT_LISTEN_ADDR=127.0.0.1:8081
```

Path A: set `RUNBOOK_UPLOAD_SIGNING_KEY` and `RUNBOOK_USERCONTENT_ORIGIN` in
`deploy/.env`; compose runs the listener on 8081 in the container and
publishes it as `RUNBOOK_USERCONTENT_HOST_PORT` (default 8081).

### Blob storage settings (STO1)

By default (`RUNBOOK_BLOB_STORE=postgres`), all blobs (archives, evidence uploads, image thumbnails, snapshots) live directly in PostgreSQL chunks (`runbook.blob_chunk`).

Switching an existing install to GCS or S3 does not move blobs already in
Postgres: they keep reading from there. To move them, run the one-off job
once with the new store configured — it copies each Postgres-held blob to
object storage, verifies its hash, flips its location and drops its chunks,
and is safe to rerun:

```
RUNBOOK_BLOB_STORE=gcs RUNBOOK_BLOB_BUCKET=my-bucket runbook-server copy-blobs
```

To offload blob bytes to an object bucket (Google Cloud Storage or Amazon S3 / MinIO):

| Variable | Meaning |
| --- | --- |
| `RUNBOOK_BLOB_STORE` | Blob backend: `postgres` (default), `gcs`, or `s3`. Empty means `postgres` |
| `RUNBOOK_BLOB_BUCKET` | The bucket name (required when `RUNBOOK_BLOB_STORE` is `gcs` or `s3`) |
| `RUNBOOK_BLOB_PREFIX` | Key prefix for all objects in the bucket; defaults to `runbook/` |
| `RUNBOOK_S3_ENDPOINT` | Custom S3 endpoint URL (for MinIO, LocalStack, or other S3-compatible providers) |
| `RUNBOOK_S3_REGION` | S3 region name (e.g. `us-east-1`, `eu-west-1`) |
| `RUNBOOK_S3_FORCE_PATH_STYLE` | `true` or `1` for path-style requests (`s3.endpoint/bucket/key`), used by MinIO and LocalStack |

**IAM permissions:**
- **GCS:** The instance service account or Workload Identity requires `roles/storage.objectAdmin` on **that bucket only** (no long-lived service account HMAC keys needed).
- **S3:** The instance IAM role needs `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject`, and `s3:ListBucket` on the bucket and prefix.

**Mixed instances and orphan cleanup:**
- PostgreSQL remains the authority for ownership, tenancy, and immutability. Each blob retains a row in `runbook.blob` with `location` (`'postgres'` or `'object'`), `size`, and `sha256`.
- Mixed reads are fully supported: existing blobs stored in PostgreSQL continue to be read from chunks, while new writes go to the object bucket. Or move them with the copy job above.
- A background sweeper running in the drain worker uses an advisory lock (`0x72625f7377656570`) to periodically sweep orphaned objects older than 1 hour (from cascaded run deletions or interrupted writes) that have no corresponding row in `runbook.blob`.
- When deploying with the GKE Terraform module (`deploy/terraform/gcp-gke/`), set `blob_store = "gcs"` to automatically create a private, versioned bucket with uniform access and Workload Identity bindings.

### Sign-in settings (D48)

People sign in with a one-time code sent to their email address, or with
Google; there are no passwords. `serve` runs the sign-in service (`auth/`)
as a child process on a loopback port and proxies `/api/auth/*` to it, so
there is still one origin and one port. Sign-in is off until
`RUNBOOK_AUTH_SECRET` is set; without it `serve` still starts and the
headless bootstrap token (section 2) still works.

| Variable | Meaning |
| --- | --- |
| `RUNBOOK_AUTH_SECRET` | 32+ random characters (`openssl rand -hex 32`). Required to enable any method; keep it stable (it signs session cookies) |
| `RUNBOOK_AUTH_METHODS` | `email`, `google` or both, comma separated. Unset or empty: every method configured below is on. Set: each listed method must be fully configured, or the sign-in service refuses to start |
| `RUNBOOK_AUTH_ALLOWED_DOMAINS` | Comma list of email domains allowed to sign in, exact match (`example.test` does not admit `sub.example.test`); empty means any. `bootstrap --owner-email` refuses an owner outside it |
| `RUNBOOK_SMTP_URL` | `smtp://` or `smtps://` relay URL, credentials included, for sign-in codes. Email codes are on only when this and `RUNBOOK_SMTP_FROM` are set |
| `RUNBOOK_SMTP_FROM` | The codes' From address, e.g. `The Runbook <login@example.test>` |
| `RUNBOOK_GOOGLE_CLIENT_ID`, `RUNBOOK_GOOGLE_CLIENT_SECRET` | A Google OAuth web client, both or neither. Its redirect URI is `{RUNBOOK_ORIGIN}/api/auth/callback/google`. Accounts whose email Google has not verified are refused |
| `RUNBOOK_TRUSTED_PROXIES` | Comma list of addresses or CIDR blocks (`10.0.0.0/8, ::1`) of the reverse proxies in front of `serve`; empty trusts none. When a request comes from one of them, the client is the right-most `X-Forwarded-For` hop that is not itself a trusted proxy (or, without `X-Forwarded-For`, the right-most such `Forwarded: for=`); otherwise those headers are ignored and the client is the TCP peer. `X-Real-IP` and `CF-Connecting-IP` are never read. Per-IP limits (sign-in codes, device codes) and the device-login requester the approval page shows use this address; IPv6 clients are limited by their /64. Without it, every client behind a TLS proxy shares the proxy's limits. Trust a proxy only if nothing else can reach `serve`'s port |
| `RUNBOOK_AUTH_COMMAND` | How `serve` starts the sign-in service. Default `bun auth/dist/server.mjs` (relative to the working directory); the container image sets its own |
| `RUNBOOK_AUTH_EMAIL_TRANSPORT`, `RUNBOOK_AUTH_CAPTURE_FILE` | Tests only: `capture` writes codes to a JSONL file instead of sending them, and is refused unless `RUNBOOK_ORIGIN` is loopback |

Path A: put the values in `deploy/.env` (`deploy/.env.example` lists them
with placeholders); compose passes them to the server, and the image carries
the sign-in service and Bun. Path B: build the service once and run it with
[Bun](https://bun.sh) or Node 22+ (`RUNBOOK_AUTH_COMMAND="node auth/dist/server.mjs"`):

```sh
(cd auth && npm ci --ignore-scripts && npm run build)
(umask 077 && openssl rand -hex 32 > "$PILOT_DIR/auth-secret")
export RUNBOOK_AUTH_SECRET="$(cat "$PILOT_DIR/auth-secret")"
export RUNBOOK_SMTP_URL='smtps://login-user:CHANGE_ME@smtp.example.test:465'
export RUNBOOK_SMTP_FROM='The Runbook <login@example.test>'
runbook-server serve
```

`serve` logs `auth service ready` once sign-in is up, and
`/.well-known/runbook` lists the enabled methods under `auth.methods`. The
sign-in service keeps its tables in its own `auth` schema of the same
database, so the database role also creates that schema on first start.
It inherits only the variables above, `DATABASE_URL` (and the standard
`PG*` connection variables), and what Bun or Node needs (`PATH`, `HOME`,
`TMPDIR`, locale and time zone, `SSL_CERT_FILE`/`SSL_CERT_DIR`,
`NODE_EXTRA_CA_CERTS`, and the `HTTP(S)_PROXY`/`NO_PROXY` pair); no other
server secret reaches it.

**Behind a TLS proxy.** Set `RUNBOOK_ORIGIN` to the proxy's `https://` URL
and `RUNBOOK_TRUSTED_PROXIES` to the address the proxy connects from (for a
proxy on the same host, `127.0.0.1`; through Docker's published port, the
bridge gateway, e.g. `172.16.0.0/12`). The proxy must append the client to
`X-Forwarded-For` (Caddy, nginx `proxy_add_x_forwarded_for` and Traefik do).
On https the session cookie is `__Host-runbook.session_token` (Secure,
`Path=/`, no `Domain`); the earlier `__Secure-runbook.session_token` is still
accepted for one release, so people signed in before an upgrade stay signed
in.

**`runbook login`** asks for the scopes an operator or agent CLI needs
(`runbooks:read`, `runbooks:write`, `runbooks:publish`, `executions:read`,
`executions:start`, `executions:act`, `skills:read`, `skills:write`).
`runbook login --admin` adds `tokens:manage`, `webhooks:manage`,
`project:admin` and `executions:admin`; `--scope <name>` (repeatable) asks for
exactly the scopes named. The approver can only narrow what was asked.

### Auth error codes

Authentication and authorization errors return RFC 7807/9457 problem documents (`Content-Type: application/problem+json` or `{"code", "message"}`) with one of these codes:

| Code | HTTP status | Meaning |
| --- | --- | --- |
| `auth.required` | 401 | Missing authentication on a protected route |
| `auth.invalid_credential` | 401 | Bearer token or secret is malformed, unrecognized, expired, or revoked |
| `auth.forbidden` | 403 | Principal is authenticated but lacks permission for the resource or organization |
| `auth.scope_missing` | 403 | Personal access token or service token lacks the required scope (e.g. `runbooks:write`, `tokens:read`) |
| `auth.csrf` | 403 | Cookie-authenticated request failed CSRF check (`X-Runbook-CSRF: 1` missing or origin mismatch) |
| `auth.unavailable` | 502 / 503 | Sign-in service child process or service binding is down, timed out, or unreachable |
| `auth.not_configured` | 503 | Sign-in requested but no sign-in method is configured on the server |
| `auth.misconfigured` | 500 | Sign-in service environment or options are invalid |
| `auth.not_found` | 404 | Unknown route under `/api/auth/*` |
| `auth.invalid_email` | 400 | Email address missing, empty, or invalid format |
| `auth.invalid_code` | 400 | OTP sign-in code is not a 6-digit string |
| `auth.invalid_request` | 400 | Request body missing, not JSON, or unsupported sign-in parameters |
| `auth.invalid_callback` | 400 | OAuth callback URL is not a local path on this origin |
| `auth.domain_not_allowed` | 403 | Email domain is not permitted by `RUNBOOK_AUTH_ALLOWED_DOMAINS` |
| `auth.registration_refused` | 403 | Email address has no account or pending invitation in this organization |
| `auth.email_unverified` | 403 | Google account email is not marked verified by Google |
| `auth.email_unavailable` | 503 | Failed to dispatch sign-in email via SMTP or capture transport |
| `auth.rate_limited` | 429 | Rate limit reached (60 s resend cooldown, 5 codes/email/hr, or 10 codes/IP/hr). A client address that has signed in with an email before has its own 5 codes an hour for it, so others asking for that email's codes cannot lock its owner out |
| `auth.internal_unauthorized` | 401 | Missing or incorrect internal secret on `/internal/session` |
| `auth.no_session` | 401 | No live session found for the provided session cookie |

In addition, Better Auth's OTP verification endpoints return `INVALID_OTP` (wrong code), `OTP_EXPIRED` (older than 10 minutes), or `TOO_MANY_ATTEMPTS` (after 5 failed attempts, which voids the code).

### Archive and retention settings (D53–D54)

Daily archive files are written automatically for completed runbook executions. **Retention is off by default**; nothing is deleted unless an operator explicitly configures a retention window. Setting `RUNBOOK_RETENTION_LIVE_DAYS` enables deletion of live run detail once archived.

| Variable | Meaning |
| --- | --- |
| `RUNBOOK_RETENTION_LIVE_DAYS` | Integer (1–36,500) or `off`. Days to keep live execution detail before purging (default: unset / `off`). Setting this enables deletion of live run detail once a daily archive file is written. |
| `RUNBOOK_RETENTION_ARCHIVE_DAYS` | Integer (1–36,500) or `off`. Days to keep daily archive ZIP files (default: unset / `off` / indefinite). When set, `RUNBOOK_RETENTION_LIVE_DAYS` must also be set and cannot exceed `RUNBOOK_RETENTION_ARCHIVE_DAYS`. |
| `RUNBOOK_RETENTION_SUMMARY_DAYS` | Integer (1–36,500) or `off`. Days to keep high-level run summary rows in the database (default: unset / `off` / indefinite). Must be `>= RUNBOOK_RETENTION_ARCHIVE_DAYS`, and must be unset or `off` when archives are kept forever (`config.retention_summary_before_archive`). |
| `RUNBOOK_RETENTION_PROJECT_OVERRIDES` | Semicolon-separated per-project policy overrides, e.g. `prj_…:live=30,archive=365;prj_…:live=off`. Supported fields are `live`, `archive`, and `summary`; setting a field to `off` keeps records forever, while omitted fields inherit the default policy. |

Startup constraints:
- `RUNBOOK_RETENTION_LIVE_DAYS` must not exceed `RUNBOOK_RETENTION_ARCHIVE_DAYS` (`config.retention_archive_before_live`), preventing files from expiring before live records they back up.
- `RUNBOOK_RETENTION_SUMMARY_DAYS` must be `>= RUNBOOK_RETENTION_ARCHIVE_DAYS` (`config.retention_summary_before_archive`), and must be unset when archives are kept forever.
- Values must be between 1 and 36,500; zero is refused (`config.retention_zero_days`).

### Theme colours (UI-1)

The web app's accent colours can be set to your own. `/v1/meta` reports
them as `theme` (`{"light": {"accent": …, "accent_secondary": …}, "dark":
{…}}`, each `#rrggbb` in lowercase, a colour left out when not set; no
`theme` at all when none is set), and the web app applies them.

| Variable | Meaning |
| --- | --- |
| `RUNBOOK_THEME_ACCENT` | Accent colour for both themes, `#rrggbb` |
| `RUNBOOK_THEME_ACCENT_SECONDARY` | Secondary accent colour for both themes, `#rrggbb` |
| `RUNBOOK_THEME_DARK_ACCENT` | Accent colour for the dark theme only; replaces `RUNBOOK_THEME_ACCENT` there |
| `RUNBOOK_THEME_DARK_ACCENT_SECONDARY` | Secondary accent for the dark theme only; replaces `RUNBOOK_THEME_ACCENT_SECONDARY` there |

The light theme uses the base values; the dark theme uses each dark value
when set, else the base value. Empty or blank means unset. Hex digits may be
upper or lower case; `#rgb`, colour names and a value without `#` are
refused, and `serve` does not start (`config.invalid_theme_color:
RUNBOOK_THEME_ACCENT must be #rrggbb`, naming the variable). (On Cloudflare
Workers, which have no startup step, an invalid value is logged and the
default theme served instead.) A colour whose
WCAG 2 contrast is below 4.5:1 against its theme's background (`#ffffff`
light, `#0e0f14` dark) is allowed, with a warning in the startup log: the
web app adjusts its text shades, but buttons in that colour may be hard to
read. Path A: set them in `deploy/.env` (see `deploy/.env.example`).

## 2. Bootstrap the first owner

`bootstrap` is a local command on the server host, never an HTTP route. It
runs once per database: it creates the organization (`Runbook`), its owner
and a `default` project. A second run fails with
`bootstrap.already_initialized`.

With sign-in configured, name the owner by email. The owner gets that
address as a verified email, so their first sign-in with a code sent to it
is the owner:

```sh
export OWNER_EMAIL=you@example.test   # your address
# Path A:
docker compose -f deploy/docker-compose.yml --env-file deploy/.env exec -T server \
  runbook-server bootstrap --owner-email "$OWNER_EMAIL" --owner-name "Your Name" --with-token \
  | pj keep "$PILOT_DIR" pat=owner.pat service=bootstrap.svc
# Path B:
runbook-server bootstrap --owner-email "$OWNER_EMAIL" --owner-name "Your Name" --with-token \
  | pj keep "$PILOT_DIR" pat=owner.pat service=bootstrap.svc
```

The output shows `org_id`, `user_id` (the owner), `project_id` (`default`)
and `owner_email`. Without `--with-token` it prints no secret at all and
issues none: the owner only signs in. This guide drives everything from the
`runbook` CLI, which does not sign in by email yet, so it asks for the
owner's personal access token (`owner.pat`) and a `default` service token
(`bootstrap.svc`), each printed once. The guide does not use the service
token; section 5 revokes it. An address outside `RUNBOOK_AUTH_ALLOWED_DOMAINS`
is refused (`bootstrap.owner_domain_not_allowed`), since that owner could
never sign in.

To sign in, request a code and use it, from the web app or directly. The
`Origin` header must be your `RUNBOOK_ORIGIN` exactly (path A's default is
`http://localhost:8080`; path B's is `$PILOT_HOST`):

```sh
export PILOT_ORIGIN=http://localhost:8080   # your RUNBOOK_ORIGIN
curl -fsS -X POST "$PILOT_HOST/api/auth/email-otp/send-verification-otp" -H "Origin: $PILOT_ORIGIN" \
  -H 'Content-Type: application/json' --data "{\"email\":\"$OWNER_EMAIL\",\"type\":\"sign-in\"}"
# then, with the six-digit code from the email:
curl -sS -c "$PILOT_DIR/cookies" -X POST "$PILOT_HOST/api/auth/sign-in/email-otp" -H "Origin: $PILOT_ORIGIN" \
  -H 'Content-Type: application/json' --data "{\"email\":\"$OWNER_EMAIL\",\"otp\":\"CODE\"}" >/dev/null
curl -fsS -b "$PILOT_DIR/cookies" "$PILOT_HOST/v1/me"   # credential.kind is "session"
```

Unsafe requests with the session cookie also need `X-Runbook-CSRF: 1` and
the server's `Origin` (or `403 auth.csrf`); bearer tokens need neither.

**Headless fallback.** Without sign-in (no `RUNBOOK_AUTH_SECRET`), plain
`runbook-server bootstrap` creates the owner (`Administrator`) with no email
and always prints the owner's personal access token and the service token:

```sh
runbook-server bootstrap | pj keep "$PILOT_DIR" pat=owner.pat service=bootstrap.svc
```

## 3. Configure the CLI and create the project

As the owner:

```sh
export XDG_CONFIG_HOME="$PILOT_DIR/config/owner"
runbook context add pilot --host "$PILOT_HOST"
runbook context use pilot
runbook login --token-stdin < "$PILOT_DIR/owner.pat"
runbook whoami
ORG_ID=$(runbook whoami --json | pj get organizations.0.id)
PROJECT_ID=$(runbook project create --slug pilot --title "Pilot" --json | pj get id)
echo "$ORG_ID $PROJECT_ID"
```

`login` without flags runs the OAuth device authorization grant: prints the
verification URL and user code to stderr, attempts to open the browser, polls
for approval, and stores the resulting personal access token for this host.
Alternatively, `login --token-stdin` pipes an existing token.
Release builds keep login tokens in the OS keychain on macOS (Keychain) and
Windows (Credential Manager), under the service `therunbook-cli`; Linux, and
any keychain that refuses the write, use a 0600 file in the CLI's config
directory. `RUNBOOK_CREDENTIAL_STORE=file|keychain|auto` overrides the choice
(debug builds default to the file). Logins stored in the file by an earlier
CLI keep working; `runbook login --migrate-to-keychain` moves them.
Execution pairs (`run token`) always stay in the 0600 file.
The CLI resolves the host from a positional argument (`runbook login [host]`),
`--host`, `RUNBOOK_HOST`, `--context`, then the current context. The owner
becomes an admin of the project they create.

A context names a host and an identity, so one machine can hold several
logins for the same host: `runbook context add pilot-admin --host
"$PILOT_HOST"`, then `runbook --context pilot-admin login`. A context's login
is used whenever the resolved host is that context's host and is never sent
to another host; a context without one falls back to the host's login.
`runbook context remove <name>` forgets the context and its login, and
`runbook context list --json` shows which contexts are logged in.

## 4. People, teams and their own tokens

Create the three people, make them project operators (they can start runs,
work steps and decide approvals they are eligible for), and form the two
teams the flagship names (`owners: [team:oncall]`, approvers
`teams: [oncall]`, escalation to `team:eng-leads`):

```sh
ALICE_ID=$(runbook user create --name Alice --json | pj get id)
BOB_ID=$(runbook user create --name Bob --json | pj get id)
CAROL_ID=$(runbook user create --name Carol --json | pj get id)
runbook project member add pilot "$ALICE_ID" --role operator
runbook project member add pilot "$BOB_ID" --role operator
runbook project member add pilot "$CAROL_ID" --role operator
runbook team create --slug oncall --title "On-call"
runbook team create --slug eng-leads --title "Engineering leads"
runbook team member add oncall "$ALICE_ID"
runbook team member add oncall "$BOB_ID"
runbook team member add eng-leads "$CAROL_ID"
```

Project roles are `viewer`, `operator`, `editor` and `admin`; give someone
who authors runbooks `--role editor`.

### Invite people by email (sign-in)

With sign-in configured, the way to add a person is an invitation: an
owner or admin names their email address, an org role and project roles,
and the person's first sign-in with a code sent to that address creates
their account with those roles (an address with no invitation and no
account is refused after the code). The CLI has no invitation command yet:

```sh
(umask 077 && printf 'Authorization: Bearer %s\n' "$(cat "$PILOT_DIR/owner.pat")" > "$PILOT_DIR/.owner-auth-header")
curl -sS --fail-with-body -X POST "$PILOT_HOST/v1/orgs/$ORG_ID/invitations" \
  -H @"$PILOT_DIR/.owner-auth-header" -H 'Content-Type: application/json' -H 'Idempotency-Key: invite-dana' \
  --data '{"email":"dana@example.test","role":"member","projects":[{"project":"pilot","role":"operator"}]}'
```

`GET` on the same route lists invitations, and `DELETE .../invitations/{id}`
revokes one. People created with `runbook user create` (as below) have no
email and cannot sign in by email.

### Each person's first CLI token (headless fallback)

This walkthrough plays Alice, Bob and Carol from the CLI on one machine,
and the CLI cannot sign in by email yet: `POST /v1/tokens`
(`runbook token create`) issues tokens only to the caller. So the operator
seeds one first token per person with
[`seed-first-token.sh`](../examples/pilot/seed-first-token.sh): it
generates the secret locally, writes it to `$PILOT_DIR/<name>.seed` (0600)
and inserts only its SHA-256 through
[`seed-first-token.sql`](../examples/pilot/seed-first-token.sql). The seed
is limited to the `pilot` project, `executions:read|start|act` and
`tokens:manage`, for 90 days. This mirrors how the C4 flagship gate
provisions people; it is the fallback for headless installs and CLI-only
people, not the onboarding path (invitations are).

Tell the script how to reach the database. Path A runs `psql` inside the
Postgres container (with your `POSTGRES_USER` and `POSTGRES_DB` if you
changed them), so no password is involved:

```sh
export PILOT_PSQL="docker compose -f deploy/docker-compose.yml --env-file deploy/.env exec -T postgres psql -U runbook -d runbook"
```

Path B: leave `PILOT_PSQL` unset and run the seeds in a terminal where
`DATABASE_URL` is exported. The script splits the URL into `PGHOST`,
`PGPORT`, `PGUSER`, `PGPASSWORD`, `PGDATABASE` (and `PGSSLMODE`) in
`psql`'s environment, so neither the URL nor the password appears on a
command line, where `ps` would show it to other users. For the same reason
it refuses a `PILOT_PSQL` that contains a URL or password.

Then seed:

```sh
examples/pilot/seed-first-token.sh alice "$ORG_ID" "$ALICE_ID" "$PROJECT_ID"
examples/pilot/seed-first-token.sh bob "$ORG_ID" "$BOB_ID" "$PROJECT_ID"
examples/pilot/seed-first-token.sh carol "$ORG_ID" "$CAROL_ID" "$PROJECT_ID"
```

Now each person logs in with their seed and mints their own working token
through the token route. In Alice's terminal (after the section 0 lines):

```sh
export XDG_CONFIG_HOME="$PILOT_DIR/config/alice"
runbook context add pilot --host "$PILOT_HOST"
runbook context use pilot
runbook login --token-stdin < "$PILOT_DIR/alice.seed"
runbook token create --name alice-cli --scope runbooks:read --scope executions:read \
  --scope executions:start --scope executions:act --project pilot --expires 30d --json | pj keep "$PILOT_DIR" secret=alice.pat
runbook token list
export RUNBOOK_TOKEN="$(cat "$PILOT_DIR/alice.pat")"
runbook whoami
```

`token list` runs with the stored seed login (`tokens:manage`); the working
token in `RUNBOOK_TOKEN` has `runbooks:read` and the `executions:*` scopes, which
is also what the web app's sign-in page asks for.

Do the same for Bob (`config/bob`, `bob.seed`, `bob-cli`, `bob.pat`) and
Carol (`config/carol`, `carol.seed`, `carol-cli`, `carol.pat`) in their own
terminals. `RUNBOOK_TOKEN` takes precedence over the stored login; the
`step` commands (including `step decide`) use only `RUNBOOK_TOKEN` or an
execution pair, never a stored human login.

### Sign in to the web UI

The server also hosts the web app on its own origin, with a strict CSP and
no CORS. Path A has it on by default: open `http://127.0.0.1:8080` (your
`RUNBOOK_ORIGIN`). For path B, build it once
(`cd web && npm ci --ignore-scripts && npm run wasm:build && npm run build`)
and start `serve` with `RUNBOOK_WEB_DIR="$PWD/web/dist"`. This build's
sign-in page does not have the email-code form yet (the API above does);
until it does, it takes a pasted personal token, over https or on `localhost`/`127.0.0.1`/`[::1]`
only (elsewhere the field is disabled), and refuses service tokens,
agent-kind tokens, expired tokens and any token holding `tokens:manage` or an
`*:admin` scope. Mint a narrow, short-lived one for the browser with your
stored login (the command the sign-in page suggests):

```sh
runbook token create --name browser --scope runbooks:read --scope executions:read --scope executions:start --scope executions:act --expires 7d
```

Paste the printed secret into the sign-in page. It lives only in that tab's
memory and session storage (never a cookie, `localStorage` or a URL), and
signing out revokes it (`DELETE /v1/me/token`). A token can carry only scopes
its creator holds; the interim seeds above include `runbooks:read`, so Alice,
Bob and Carol can mint this token from their stored login.

That token reads, starts and acts on runs. Editing and publishing runbooks in
the browser's editor also needs the `editor` (or `admin`) project role and a
token with `--scope runbooks:write --scope runbooks:publish --scope skills:read`
added, minted from a login that holds those scopes.

## 5. The launcher's service token

The trusted launcher (the GitHub Actions workflow, or the agent terminal in
this guide) holds a project service token that can only mint
execution-scoped credentials. The CLI cannot create service tokens yet, so
the owner calls the token route directly, with the token read from a 0600
header file:

```sh
(umask 077 && printf 'Authorization: Bearer %s\n' "$(cat "$PILOT_DIR/owner.pat")" > "$PILOT_DIR/.owner-auth-header")
curl -sS --fail-with-body -X POST "$PILOT_HOST/v1/tokens" \
  -H @"$PILOT_DIR/.owner-auth-header" -H 'Content-Type: application/json' \
  -H "Idempotency-Key: launcher-$PROJECT_ID" \
  --data "{\"kind\":\"service\",\"name\":\"launcher\",\"scopes\":[\"executions:read\",\"executions:act\"],\"projects\":[\"$PROJECT_ID\"],\"role\":\"operator\"}" \
  | pj keep "$PILOT_DIR" secret=launcher.svc
```

Only an org owner or admin can issue service tokens; the role and scopes
can be at most the issuer's. The owner sees service tokens in
`runbook token list`. Two tokens there have an empty name and every scope:
your own (`"kind": "personal"`, keep it) and the bootstrap service token
(`"kind": "service"`). Revoke the service one unless you need it:

```sh
runbook token list --json
runbook token revoke tok_REPLACE_WITH_THE_BOOTSTRAP_SERVICE_TOKEN_ID
```

## 6. Publish the flagship and its skills

The flagship (`conformance/spec/post-deploy.yaml`, the PRD example byte for
byte) names two skills, `./skills/check-error-rates` and
`./skills/rollback-service`. Upload the fixtures from
[`examples/pilot/skills/`](../examples/pilot/skills/) first; publication pins
each alias to the latest uploaded version whose name matches its last path
segment:

```sh
runbook skill push examples/pilot/skills/check-error-rates --project pilot
runbook skill push examples/pilot/skills/rollback-service --project pilot
```

Then, as the owner (`runbooks:publish`), save the draft and publish it. The
first runbook of a project is pushed with `--draft` (a publishing `push`
first confirms the scope on an existing runbook):

```sh
runbook push conformance/spec/post-deploy.yaml --project pilot --draft
runbook publish post-deploy --project pilot
runbook get post-deploy --project pilot
```

The spec needs no project variables; its `vars` are declared inline. Offline,
`runbook spec validate conformance/spec/post-deploy.yaml` stops at
`spec.unresolved_team` because teams and skills resolve against the
server's directory at publish time.

## 7. Webhooks: recording receiver and GitHub Actions relay

Subscribe the recording receiver to every event and the relay to
`agent.work_available`. Each signing secret is shown once and goes to a
0600 file:

```sh
RECORDER_ID=$(runbook webhook create --project pilot --name pilot-recorder \
  --url http://127.0.0.1:9000/ --event '*' --json \
  | pj keep "$PILOT_DIR" secret=receiver.whsec | pj get webhook.id)
RELAY_ID=$(runbook webhook create --project pilot --name github-relay \
  --url http://127.0.0.1:8081/ --event agent.work_available --json \
  | pj keep "$PILOT_DIR" secret=relay.whsec | pj get webhook.id)
runbook webhook list --project pilot
```

With compose, the server delivers from inside its container, where
`127.0.0.1` is the container itself. The compose file maps
`host.docker.internal` to your machine (`extra_hosts:
host.docker.internal:host-gateway`), so subscribe
`http://host.docker.internal:9000/` and `http://host.docker.internal:8081/`
instead. The receiver and the relay must then listen on an interface the
container reaches, not on loopback: on Linux, the Docker bridge address
(`RECEIVER_HOST=172.17.0.1` / `RELAY_HOST=172.17.0.1`; check with
`ip -4 addr show docker0`), and point the relay's `GITHUB_API_URL` at the
same address. `0.0.0.0` also works but exposes both programs on every
interface: use it only on a machine whose firewall keeps those ports
private (deliveries are still signature-checked and the GitHub stand-in
still requires its token).

Start the [recording receiver](../examples/pilot/recording-receiver.py) in
its own terminal. It verifies `Runbook-Signature` with a constant-time
compare, refuses timestamps older than five minutes, deduplicates by event
ID and prints one line per event (type, ID, execution, the step runs it
names); `RECEIVER_LOG` keeps a 0600 JSON-lines copy. With
`RECEIVER_MOCK_GITHUB=1` it also answers the relay's GitHub API calls, so
nothing contacts GitHub; it accepts only `Authorization: Bearer
<RECEIVER_GITHUB_TOKEN>`, a dummy value you also give the relay as
`GITHUB_TOKEN`. It listens on `127.0.0.1:9000` unless `RECEIVER_HOST` /
`RECEIVER_PORT` say otherwise:

```sh
RUNBOOK_WEBHOOK_SECRET_FILE="$PILOT_DIR/receiver.whsec" RECEIVER_LOG="$PILOT_DIR/events.jsonl" \
  RECEIVER_MOCK_GITHUB=1 RECEIVER_GITHUB_TOKEN=pilot-dry-run \
  python3 -I examples/pilot/recording-receiver.py
```

Start the [GitHub Actions relay](../examples/webhooks/github-actions-relay/)
in another terminal. The flagship's `spec.agents.params` name
`repo: acme/api` and `workflow: runbook-resume.yml`; the relay refuses
anything outside its allow-lists before calling GitHub. Here it dispatches
to the receiver's GitHub stand-in:

```sh
RUNBOOK_WEBHOOK_SECRET="$(cat "$PILOT_DIR/relay.whsec")" GITHUB_TOKEN=pilot-dry-run \
  GITHUB_API_URL=http://127.0.0.1:9000 GITHUB_REPOSITORY=acme/api \
  RELAY_ALLOWED_REPOS=acme/api RELAY_ALLOWED_WORKFLOWS=runbook-resume.yml RELAY_ALLOWED_REFS=main \
  RELAY_HOST=127.0.0.1 RELAY_PORT=8081 RELAY_DEDUP_FILE="$PILOT_DIR/relay-seen.txt" \
  python3 -I examples/webhooks/github-actions-relay/relay.py
```

Check both deliveries arrive (the receiver prints `event webhook.test ...`;
the relay acknowledges `webhook.test` with 204):

```sh
runbook webhook test "$RECORDER_ID" --project pilot
runbook webhook test "$RELAY_ID" --project pilot
runbook webhook deliveries "$RECORDER_ID" --project pilot
```

For real GitHub Actions: put the relay behind HTTPS reachable by the
server, set `GITHUB_TOKEN` to a token with `actions:write` on the target
repository, drop `GITHUB_API_URL`, and allow-list that repository (the
flagship's params name `acme/api`; a copy of the runbook with your own
`spec.agents.params.repo` is needed for another repository). Copy
[`runbook-resume.yml`](../examples/webhooks/github-actions-relay/runbook-resume.yml)
into that repository's `.github/workflows/`, store the contents of
`launcher.svc` as `secrets.RUNBOOK_SERVICE_TOKEN`, and set
`vars.RUNBOOK_HOST` to a server URL the runner can reach. The workflow then
does what the agent terminal does in section 8.

## 8. Run the flagship end to end

**Alice starts a run** (her terminal, with `RUNBOOK_TOKEN` set as in
section 4):

```sh
EXE=$(runbook run start post-deploy --project pilot --input service=api \
  --input version=v2.0.0 --tag pilot --json | pj get id)
echo "$EXE"
```

The receiver prints `execution.started` and `agent.work_available` (with the
ready steps), and `github workflow_dispatch acme/api runbook-resume.yml ...`
for the relay's dispatch.

**The launcher mints an execution pair; the agent works the steps.** In the
agent's terminal, the service token is used for one command only; the agent
commands that follow hold nothing but the execution-scoped pair, which the
CLI stores 0600 and refreshes itself:

```sh
export XDG_CONFIG_HOME="$PILOT_DIR/config/agent"
runbook context add pilot --host "$PILOT_HOST"
runbook context use pilot
EXE=exe_REPLACE_WITH_THE_EXECUTION_ID
RUNBOOK_TOKEN="$(cat "$PILOT_DIR/launcher.svc")" runbook run token "$EXE" --session agent-1
runbook whoami
runbook step next "$EXE"
```

Give an agent an execution pair (as here) or an agent-kind token
(`runbook token create --agent ...`), never a person's token. The server
tells principals apart by token kind: an agent-kind token or an execution
pair is recorded as an agent (its memory writes name the agent's token or
run, so people can see and revert them, and it never casts human
approvals), while a person's token in an agent's `RUNBOOK_TOKEN` makes the
agent indistinguishable from that person.

`step next` prints the resume brief: `next: claim_ready_work` and the ready
agent steps (`next: wait` means nothing is ready for this actor yet; run it
again). For each step: claim, read, report, in that order (an execution
pair can read only the step it has claimed; `step get` before `step claim`
is refused with 403). The smoke check first:

```sh
SR=$(runbook step next "$EXE" --json | pj find ready_work path=api_health step_run_id)
runbook step claim "$SR" --session agent-1 --agent-name pilot-agent --model manual
runbook step get "$SR"
runbook step report "$SR" --status succeeded --verdict pass --output status_code=200
```

**Alice does the human smoke check** (her terminal):

```sh
SR=$(runbook step next "$EXE" --json | pj find ready_work path=login_flow step_run_id)
runbook step claim "$SR" --session alice-cli
runbook step checklist "$SR" --tick "Login page loads" --tick "Dashboard shows data"
runbook step report "$SR" --status succeeded --verdict pass --summary "Dashboard shows data"
```

**The agent checks the three regions.** eu-west fails, which opens the
stabilize loop and, after it, the approval:

```sh
SR=$(runbook step next "$EXE" --json | pj find ready_work "path=regions[0].error_rate" step_run_id)
runbook step claim "$SR" --session agent-1 --agent-name pilot-agent --model manual
runbook step report "$SR" --status succeeded --verdict pass --output rate=0.002 --evidence https://dashboards.example/errors/us-west
SR=$(runbook step next "$EXE" --json | pj find ready_work "path=regions[1].error_rate" step_run_id)
runbook step claim "$SR" --session agent-1 --agent-name pilot-agent --model manual
runbook step report "$SR" --status succeeded --verdict pass --output rate=0.003 --evidence https://dashboards.example/errors/us-east
SR=$(runbook step next "$EXE" --json | pj find ready_work "path=regions[2].error_rate" step_run_id)
runbook step claim "$SR" --session agent-1 --agent-name pilot-agent --model manual
runbook step report "$SR" --status succeeded --verdict fail --output rate=0.05 --evidence https://dashboards.example/errors/eu-west
SR=$(runbook step next "$EXE" --json | pj find ready_work "path=stabilize#1.recheck" step_run_id)
runbook step claim "$SR" --session agent-1 --agent-name pilot-agent --model manual
runbook step report "$SR" --status succeeded --verdict pass --output rate=0.004
runbook step next "$EXE"
```

The re-check recovers (the loop is satisfied, its worst finding stays
`fail`), so the `signoff` approval opens for team `oncall`; the agent's
brief now says `next: wait`. The receiver prints `approval.requested` with
its step run and the recipients (Alice and Bob).

**Bob decides** (his terminal, `RUNBOOK_TOKEN` from `bob.pat`). Human
approvals are not in the agent brief; take the step run ID from the
receiver's `approval.requested` line or from the run report:

```sh
SR=$(runbook run report "$EXE" --json | pj find sections path=signoff step_run_id)
runbook step decide "$SR" --option keep --comment "Recovered after the re-check; keep v2.0.0"
```

Carol is not an approver until the approval escalates to `eng-leads` after
30 minutes without a decision; before that, her `step decide` is refused
with `403 approval.not_eligible`. Deciding `rollback` instead runs the
destructive `rollback` agent step (skill `rollback-service`).

**The agent finishes.** The decision sends `agent.work_available` (reason
`approval_decided`) and the relay dispatches again; the agent resumes and
posts the report (`finally`):

```sh
SR=$(runbook step next "$EXE" --json | pj find ready_work path=notify step_run_id)
runbook step claim "$SR" --session agent-1 --agent-name pilot-agent --model manual
runbook step report "$SR" --status succeeded --summary "Report posted to #deploys"
```

That report ends the run, and the server revokes the execution pair when a
run ends. The CLI knows the agent's own report just finished its work, so
the report prints `report ok for … ; next: exit`, and a further
`runbook step next "$EXE"` prints `next: exit (… its credentials were
revoked when the run ended)` with exit 0: the work is over, stop. (A
revocation the agent did not cause with its own report still fails with
`credentials.revoked`.) **Alice checks the outcome**:

```sh
runbook run get "$EXE" --wait
runbook run report "$EXE" --format md
runbook run list --project pilot --field severity=major
```

The run `succeeded` with result `kept` and `severity: major` (a successful
run that kept a failing deploy is still a succeeded execution). The
receiver has recorded `approval.decided` and `execution.completed`; the
relay's dispatches appear as `github workflow_dispatch` lines.

### Agents over MCP

An agent with a shell uses the CLI as above (the `therunbook` skill tells it
to). An MCP client can use the same tools instead, two ways:

- **Local (stdio):** the client runs `runbook mcp serve`, which uses the
  CLI's context and stored credentials, exactly like the commands above:

  ```sh
  runbook mcp serve
  ```

- **Remote (Streamable HTTP):** the server answers MCP at
  `$RUNBOOK_ORIGIN/mcp` (here `$PILOT_HOST/mcp`). Every request carries
  `Authorization: Bearer <token>`: a personal token (an `--agent` one for
  agent work), a service token, or an execution access token from
  `runbook run token`. The token has exactly the scopes it has on the REST
  API; an execution token reaches only its own run. Cookies are ignored, a
  browser `Origin` other than `RUNBOOK_ORIGIN` is refused, and the server
  keeps no MCP session (no `Mcp-Session-Id`, no event stream: `GET /mcp` is
  405). OAuth sign-in for MCP clients comes later; until then, configure the
  header in the client. For example, in Claude Code:

  ```sh
  claude mcp add --transport http runbook "$PILOT_HOST/mcp" \
    --header "Authorization: Bearer $RUNBOOK_TOKEN"
  ```

  A quick check with curl (`initialize`, then `tools/list`):

  ```sh
  curl -sS "$PILOT_HOST/mcp" -H "Authorization: Bearer $RUNBOOK_TOKEN" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"curl","version":"1"}}}'
  curl -sS "$PILOT_HOST/mcp" -H "Authorization: Bearer $RUNBOOK_TOKEN" \
    -H 'Content-Type: application/json' -H 'MCP-Protocol-Version: 2025-06-18' \
    -d '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  ```

Both transports also list the published runbooks the token can read as MCP
resources (`runbook://runbooks/<rb_ id>`, read for the published YAML) and
offer the `run_runbook` prompt (arguments `runbook`, `project` and optional
`inputs` as a JSON object), which starts a run and walks the agent loop.
`/v1/meta` reports `capabilities.mcp: true` when the endpoint is served.

## 9. Automate it

[`examples/pilot/setup.sh`](../examples/pilot/setup.sh) does sections 3 to
7 (and the people's tokens) with the CLI, reusing anything that already
exists, printing created IDs and writing secrets only to 0600 files in
`$PILOT_DIR`. [`run-flagship.sh`](../examples/pilot/run-flagship.sh) then
does section 8 (`PILOT_SCENARIO=healthy` for the short path without an
approval). After section 2:

```sh
RUNBOOK_HOST="$PILOT_HOST" RUNBOOK_BOOTSTRAP_TOKEN_FILE="$PILOT_DIR/owner.pat" examples/pilot/setup.sh
RUNBOOK_HOST="$PILOT_HOST" examples/pilot/run-flagship.sh
```

`setup.sh` needs `PILOT_PSQL` (path A) or an exported `DATABASE_URL`
(path B) for the interim seeds;
without it, it does everything else and says how to finish. On path A, also
give it the receiver and relay URLs as the server container reaches them
(section 7): add `RECEIVER_URL=http://host.docker.internal:9000/
RELAY_URL=http://host.docker.internal:8081/` to the `setup.sh` line. Start the
receiver and the relay (section 7) before `run-flagship.sh`.

## 10. Operations and reset

- **Health:** `runbook-server doctor` (schema version and blob tables),
  `GET /v1/meta`. Restarting the server keeps all state; timers and
  deliveries resume from the database.
- **Lost owner:** on the server host,
  `runbook-server assign-owner --org "$ORG_ID" --user "$USER_ID"` records a
  new owner (local only; it is not an HTTP route).
- **Rotate a webhook secret:** `runbook webhook rotate-secret "$RECORDER_ID"
  --project pilot --json | pj keep "$PILOT_DIR" secret=receiver-next.whsec`.
  Deliveries carry signatures for both secrets for 24 hours only: within
  that window, restart the receiver with
  `RUNBOOK_WEBHOOK_SECRET_FILE="$PILOT_DIR/receiver-next.whsec"` (the file
  may also list both secrets, one per line, while you switch over).
  After the window, deliveries signed only with the new secret fail
  against the old file. The same applies to the relay and `relay.whsec`.
- **Start over:** path A, `docker compose -f deploy/docker-compose.yml --env-file deploy/.env down -v`
  deletes the database volume; path B, drop and recreate the database. Then
  delete `$PILOT_DIR`.

## 11. Updating

One command updates a docker compose install (path A) to the latest
release:

```sh
deploy/update.sh             # the latest release, asking before changing anything
deploy/update.sh --dry-run   # the plan only
```

**Where releases come from.** Each release is a `vX.Y.Z` tag with a GitHub
Release (CLI and server binaries, the Cloudflare edition bundle,
`SHA256SUMS`, release notes with the upgrade notes and schema version) and
the image `ghcr.io/angler-ai/runbook`, tagged `X.Y.Z`, `X.Y` and
`latest` for linux/amd64 and linux/arm64. `deploy/update.sh` installs that
image by default (`--image-repo` or `RUNBOOK_IMAGE_REPO` for another
repository): it resolves the tag to a digest, verifies the digest's build
provenance with `gh attestation verify --repo angler-ai/the-runbook-app`, pinned
to the release workflow (`--signer-workflow`) and the release's tag
(`--source-ref refs/tags/vX.Y.Z`; for `latest` or `X.Y` the release named by
the image's version label, which `runbook-server --version` must then
match)
(install the [GitHub CLI](https://cli.github.com) and `gh auth login`), and
pulls by digest; nothing from the image runs before that. Without `gh` it
refuses unless you pass `--insecure-skip-verify`. GitHub attestations work
for public repositories; the repository goes public before the first
public release. **Nothing is published yet:** until the first release is,
update from a git checkout with `--source git`, which fetches the tags from
`origin`, checks the release tag out and builds it with `docker compose
build`.

**Choosing and pinning a release.** `--to 0.3.1` installs that release;
`--to 0.3` the newest `0.3.x`; `--to latest` (the default) the newest. Put
`RUNBOOK_UPDATE_PIN=0.3` (or `0.3.1`) in `deploy/.env` to make that the
default, so a plain `deploy/update.sh` only takes patch releases of 0.3 (or
stays on 0.3.1). Keep `RUNBOOK_IMAGE` a local name (the default
`runbook-pilot:local`): the update points it at the release it installs,
so a later `docker compose up` runs the same release.

What it does, stopping at the first failure:

1. **Preflight:** the running server's `runbook-server doctor`, its release
   (`runbook-server --version`), the target release, free disk space. A
   target older than what runs is refused (a downgrade); the same release
   is "nothing to do".
2. **The target**, verified and pulled (or built), then checked against
   your database by its own `doctor` before anything stops: a schema it
   cannot run is refused. Its **rollback floor** (`runbook-server --version`)
   is the oldest release whose code is safe on its schema: if yours is
   older (you skip releases past a contract migration), a failed update
   could not just restart your release, so the update needs
   `--restore-backup` (a failure then restores the backup) or
   `--no-rollback`.
3. **Backup:** `pg_dump` of the whole database (blobs and uploads included;
   they live in PostgreSQL) to a 0600 file in `deploy/backups/`
   (`--backup-dir` or `RUNBOOK_BACKUP_DIR`), after checking there is room,
   then read back in full (`pg_restore -f /dev/null`): an empty or truncated
   dump stops the update before anything changes. Keep
   `RUNBOOK_WEBHOOK_SECRET_KEY` with it. With an external database
   (`docker-compose.external-db.yml`, add your overrides with more
   `--compose-file`s) back up with your provider's tools and pass
   `--skip-backup`. After a successful update only the newest
   `--keep-backups` dumps (default 5) stay.
4. **Restart** on the new image; the server applies the new migrations as
   it starts. The previous image stays as `<image>:rollback-<release>`.
5. **Health:** the container healthcheck, `/health/ready`, `/v1/meta`
   reporting the new `version` and `schema`, `doctor`, discovery, the web
   UI and the CLI.

**Rollback.** If step 4 or 5 fails, the script offers to roll back (it does
so without asking under `--yes`): it starts the previous image again on the
migrated database, which is safe because every migration keeps the previous
release working (expand/contract, see `CONTRIBUTING.md`). That server runs
with `RUNBOOK_ALLOW_NEWER_SCHEMA=true`, set by a compose override the script
writes to `deploy/backups/allow-newer-schema.yml`; until the next update,
start the stack with `-f deploy/docker-compose.yml -f
deploy/backups/allow-newer-schema.yml`, or the server refuses the newer
schema (`postgres.schema_newer_than_code`). The next successful update
removes the file. With an external database the script cannot read the
schema, so it uses the recorded one. The script restores the backup only
if you pass `--restore-backup`, since that loses everything written after
the backup; it never drops your database first: it reads the backup in
full, checks that `POSTGRES_DB` is not `postgres` and that PostgreSQL's data
directory has room for a second copy, stops the server (or gives up), dumps
the current database (`runbook-<release>-pre-restore-<time>.dump`),
restores into a scratch database with the same owner, access and
`ALTER DATABASE ... SET` settings, and swaps
the two by rename. The replaced database stays as `runbook_replaced_<time>`;
drop it when you no longer need it. A problem found later:
`deploy/update.sh --rollback` (optionally with `--restore-backup`) undoes
the last update; it refuses a release below the floor without
`--restore-backup`, shows the backup's age (from the time in its ID), and for a backup older than a
day asks you to type its ID (`--confirm-backup <ID>` when nobody can). Exit status: 0 updated, 1
refused (nothing changed), 3 failed and rolled back, 4 failed and not
rolled back.

**Strict assignment and take-over (LANG7) across an update or a rollback.**
This release records two new run events, `StepAssigneeResolved` (a step
with `assignment: strict`) and `StepTakenOver` (a person took over an agent
step), and stores runbooks with `assignment: strict`. Releases before it
cannot read them: such a run fails to load there (its page, reads and
actions answer an error) until this release serves it again. Nothing is
lost or rewritten. So:

- **Order.** Update every server instance before people publish strict
  runbooks or take steps over. If you run several instances behind a load
  balancer, finish the update on all of them first. Until then the older
  ones refuse both: publishing `assignment: strict` fails, and a take-over
  is refused as a claim of an agent step (`claim.actor_mismatch`).
- **Rollback.** Rolling back to an older release leaves runs that hold
  these events unreadable until you update again; other runs are not
  affected. A project archive exported with such runs does not import into
  an older release.

Check versions any time:

```sh
docker compose -f deploy/docker-compose.yml --env-file deploy/.env exec server runbook-server --version
# this CLI's release and the server's:
runbook version
# "version" and "schema":
curl -s http://127.0.0.1:8080/v1/meta
```

Path B (native binary): stop the server, back up with `pg_dump`, install the
new `runbook-server` (the release's `runbook-server-X.Y.Z-linux-<arch>.tar.gz`,
checked with `sha256sum -c SHA256SUMS`, or `cargo build --release` at the
tag), run `runbook-server migrate` (or let `serve` do it at start), and
start it. To go back within the rollback floor printed by
`runbook-server --version`, start the previous binary with
`RUNBOOK_ALLOW_NEWER_SCHEMA=true`.

## Known gaps in the pilot CLI and server

Found while writing this guide; each step above works around them.

| Where | Gap |
| --- | --- |
| First credential | People invited by email sign in with a code; `runbook login` runs the browser/device flow (AUTH4) to issue a scoped CLI personal access token. Headless fallback: `seed-first-token.sh` (operator SQL) or `runbook login --token-stdin`. |
| Invitations | No CLI command: `POST /v1/orgs/{org}/invitations` (section 4). People made with `runbook user create` have no email, so they cannot sign in by email. |
| `runbook token create` | No `--kind service` / `--role`: service tokens need `POST /v1/tokens` (section 5). |
| Directory reads | No `user list`, `team list`, `team member list` or `project member list` commands; `setup.sh` reads the list routes with curl. |
| Inbox | No CLI for `GET /v1/me/tasks`: a person finds an approval's step run in `run report --json` or the `approval.requested` webhook (section 8). |
| `runbook run report` | Text output omits step run IDs (`--json` has them). |
| `runbook spec validate` | Offline it cannot resolve teams or skills, so the flagship stops at `spec.unresolved_team`; it exits 1 for an invalid spec, where PRD section 10 says 2. |
| `runbook login` | One stored login per host: several people on one machine need separate `XDG_CONFIG_HOME` directories. |
| Webhook relay | The flagship's `spec.agents.params.repo` is fixed to `acme/api`; dispatching elsewhere needs an edited copy of the runbook. |
