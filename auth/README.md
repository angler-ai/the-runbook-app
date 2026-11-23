# auth: The Runbook sign-in service (AUTH1)

Better Auth behind a small Hono gateway (D43). It authenticates and does nothing
else: organizations, users, roles and invitations live in the Rust directory.
Two sign-in methods, never passwords (PRD §18): email one-time codes and Google.

| Entry | Runtime | Database | Codes sent by | Client IP header |
| --- | --- | --- | --- | --- |
| `src/server.ts` → `dist/server.mjs` | Node 22+ or Bun (Docker, self-hosted) | Postgres, schema `auth` | SMTP (nodemailer) or `capture` | `x-runbook-client-ip` |
| `src/worker.ts` → `dist/worker.mjs` | Cloudflare Workers | D1, tables `auth_*` | the `EMAIL` `send_email` binding | `cf-connecting-ip` |

```
npm ci --ignore-scripts
npm run build        # esbuild bundles: dist/worker.mjs, dist/server.mjs
npm run check        # tsc, once for the Worker types and once for Node
npm test             # build, then node --test tests/*.test.mjs
DATABASE_URL=postgres://runbook_dev@localhost:5432/runbook_dev \
  lockf ../../.locks/pg.lock npm test   # also the Postgres suites
```

Without `DATABASE_URL` the Postgres suites are skipped. With it, the harness
creates owned databases named `rb_test_auth_<pid>_<n>`, uses only those and drops
them; the role needs `CREATEDB`. Tests make no network requests: Google's token
and JWKS endpoints are answered from a local RSA key (Miniflare's
`outboundService` on D1, a replaced global `fetch` on Node), can-register is a
loopback stub, and SMTP is a loopback fake.

## Configuration

Read from the environment (Worker: bindings and vars). A bad configuration lists
every problem at once: `server.ts` prints them and exits with status 78
(`EX_CONFIG`); the Worker logs them and answers every request with
`503 auth.misconfigured`.

| Variable | Meaning |
| --- | --- |
| `RUNBOOK_AUTH_SECRET` | Better Auth's signing secret, 32+ characters. Required to enable any method (D48). |
| `RUNBOOK_AUTH_METHODS` | `email`, `google` or both, comma separated. When set (and not empty) it is authoritative and every listed method must be fully configured. When unset, each method whose settings are present is on. |
| `RUNBOOK_AUTH_ALLOWED_DOMAINS` | Comma list of email domains, exact match (`example.test` does not admit `sub.example.test`). Empty means any. |
| `RUNBOOK_SMTP_URL` | `smtp://` or `smtps://` URL, credentials included. Turns on email codes (server). |
| `RUNBOOK_SMTP_FROM` | From address for codes, e.g. `The Runbook <login@example.test>`. The Worker uses it with its `EMAIL` binding. |
| `RUNBOOK_GOOGLE_CLIENT_ID`, `RUNBOOK_GOOGLE_CLIENT_SECRET` | Google OAuth client. Both or neither. Redirect URI: `{RUNBOOK_ORIGIN}/api/auth/callback/google`. |
| `RUNBOOK_AUTH_EMAIL_TRANSPORT` | `smtp` (server default), `binding` (Worker default) or `capture` (test-only, server-only). |
| `RUNBOOK_AUTH_CAPTURE_FILE` | With `capture`: the JSONL file codes are appended to. Refused unless `RUNBOOK_ORIGIN` is loopback (`localhost`, `*.localhost`, `127.0.0.0/8`, `[::1]`). |
| `RUNBOOK_ORIGIN` | The public origin, e.g. `https://runbooks.example.test`. Required with any method. `https` gives `__Secure-` cookies. |
| `RUNBOOK_AUTH_INTERNAL_SECRET` | Per-boot shared secret, 32+ characters, always required. Rust presents it to `/internal/*`; this service presents it to can-register. |
| `RUNBOOK_AUTH_CAN_REGISTER_URL` | Rust's internal can-register URL. Required with any method. |
| `DATABASE_URL` | Server only, required. The same database as `runbook-server`. |
| `RUNBOOK_AUTH_PORT` | Server only. Loopback port; `0` (default) picks a free one. The host is always `127.0.0.1`. `runbook-server` always passes the port it chose. |
| `RUNBOOK_AUTH_MIGRATIONS_DIR` | Server only. Defaults to `../drizzle/pg` next to `dist/server.mjs`. |

With no method configured the service still starts, `/internal/health` reports
`methods: []`, and `/api/auth/*` answers `503 auth.not_configured` (D48).

## Contract for the Rust host

This section is the single source of truth. Its Rust half is the constants in
`crates/runbook-server/src/auth_child.rs`; `crates/runbook-server/tests/auth_service.rs`
runs the real `runbook-server serve` with this service (Bun when installed,
otherwise Node) and signs in through the proxy, and the AUTH2 stub
(`tests/support/auth_stub.rs`) speaks the same contract.

**Process.** `serve` runs `RUNBOOK_AUTH_COMMAND` (default `bun auth/dist/server.mjs`,
relative to its working directory; the image sets
`bun /usr/share/runbook/auth/dist/server.mjs`). The child inherits only an
allowlist of the server's variables (`INHERITED_ENV` in `auth_child.rs`: the D48
variables, `DATABASE_URL` and the `PG*` connection variables, and what Bun or Node
needs to run, reach TLS and an outbound proxy) and also gets
`RUNBOOK_AUTH_PORT=0` (it binds a free loopback port itself), a fresh
`RUNBOOK_AUTH_INTERNAL_SECRET` per boot, `RUNBOOK_AUTH_CAN_REGISTER_URL` (`http://<loopback listen
address>/internal/can-register`) and `RUNBOOK_ORIGIN` as `serve` normalized it, so
both sides derive the same cookie name. The child applies its migrations (under a
Postgres advisory lock, so replicas may start together), then writes one line to
stdout:

```json
{"event":"ready","port":41234,"methods":["email","google"]}
```

`serve` reads it (any other stdout line is passed through), calls the child on
the port it names from then on, and logs `auth service ready`. Every ready line is
read again after a restart, so `serve` never calls a port an earlier child had
(something else may have taken it). Exit status 78 is a configuration error (stderr says what);
`serve` keeps running without sign-in and does not restart the child. Any other
exit is a crash: restart with backoff. `SIGTERM` stops it cleanly. Sessions live
in Postgres, so a restart loses none.

**Proof of the secret.** Every call `serve` makes to the child carries
`x-runbook-auth-nonce` (32 lowercase hex characters, fresh per call), and every
answer to such a request carries `x-runbook-auth-proof`: lowercase hex of
HMAC-SHA-256(`RUNBOOK_AUTH_INTERNAL_SECRET`, `"runbook-auth-proof\n" + nonce`).
`serve` refuses an answer without the right proof (`502 auth.unavailable` on the
proxy, `503 auth.unavailable` for a session), so a listener that is not the child
cannot answer for it. The service adds the header for any nonce of 16–128 hex
characters; the proxy strips it before the browser sees the response.

**Proxy.** Forward `/api/auth/*` to `http://127.0.0.1:{port}` unchanged: method,
path, query, body, `Cookie`, `Origin`, `Content-Type`. Refuse (404) any path with
a `.` or `..` segment, an empty segment, a backslash, or a percent-encoded dot,
slash or backslash. Set `x-runbook-client-ip` to the client address (the TCP peer,
or, when the peer is in `RUNBOOK_TRUSTED_PROXIES`, the right-most untrusted
`X-Forwarded-For` / `Forwarded` hop) and drop every client-supplied address header
(`x-runbook-client-ip`, `X-Forwarded-*`, `Forwarded`, `X-Real-IP`,
`CF-Connecting-IP`, …), `x-runbook-internal-secret` and the nonce and proof
headers. Return the response unchanged, including every `Set-Cookie` and
`Location`. Never proxy `/internal/*`. The service checks
`Origin` itself on unsafe methods (`403 auth.csrf`), so the proxy does not need
the D46 `X-Runbook-CSRF` header on `/api/auth`. A proxied
`POST /api/auth/sign-out` is the cue to evict the session cache entry (D44); a
successful `revoke-session`, `revoke-sessions` or `revoke-other-sessions` clears
the whole cache (the proxy cannot tell which cookies they ended).

**`POST /internal/session`** (D44)

```
POST /internal/session
x-runbook-internal-secret: <RUNBOOK_AUTH_INTERNAL_SECRET>
cookie: <the session cookie, name=value>          (no body)

200 {"auth_user_id":"...","email":"ada@example.test","email_verified":true,
     "session_id":"...","expires_at":1767225600000,
     "mfa_verified_at":null,"mfa_method":null,"mfa_enrolled":false}
401 {"code":"auth.internal_unauthorized","message":"..."}   missing or wrong secret
401 {"code":"auth.no_session","message":"..."}              no live session in the cookie
```

`expires_at` is integer milliseconds since the Unix epoch
(`ResolvedSession.expires_at_ms`). The call also slides the session's expiry in
the database once it is a day old. Older answers without the MFA fields can be cached for 30 s by cookie digest.
Resolve MFA-capable sessions on every request so enrollment and revocation
take effect across replicas and isolates.

**`GET /internal/health`**, same secret header:
`200 {"status":"ok","methods":["email","google"]}`. Use it for `auth.methods` in
`/.well-known/runbook`.

**can-register** (D45). Before Better Auth creates a user, and only after the code
or Google has proven the mailbox:

```
POST {RUNBOOK_AUTH_CAN_REGISTER_URL}
content-type: application/json
x-runbook-internal-secret: <RUNBOOK_AUTH_INTERNAL_SECRET>

{"email":"ada@example.test"}        (lowercased, domain already checked)

200 {"allowed": true | false}
```

Anything other than `200 {"allowed": true}` refuses, including an error or a 5 s
timeout. The user then gets `403 auth.registration_refused` (email) or a redirect
to `{origin}/sign-in?error=...` (Google), and no user row is written. Existing
users are never re-checked; linking happens in Rust on the session's first use.

**Cookie.** `__Host-runbook.session_token` on an https origin (Secure),
`runbook.session_token` on plain-http loopback. httpOnly, `SameSite=Lax`,
`Path=/`, host-only, `Max-Age=2592000` (30 days). Until the next release the
service also accepts the earlier https name `__Secure-runbook.session_token`
(on `/api/auth/*` and `/internal/session`, read as the new name when the new one
is absent) and expires it whenever it sets the new one; Rust's cookie lookup does
the same. Sliding renewal: Better Auth
re-issues the cookie with a fresh 30 days when `GET /api/auth/get-session` sees a
session older than a day; the web app (AUTH6) calls it on load. The short-lived
OAuth state cookies `/sign-in/social` sets must pass through the proxy too.

**Public routes** (everything else under `/api/auth` is `404 auth.not_found`,
every password route included):

| Route | When |
| --- | --- |
| `POST /api/auth/email-otp/send-verification-otp` `{email, type:"sign-in"}` | email on |
| `POST /api/auth/sign-in/email-otp` `{email, otp}` | email on |
| `POST /api/auth/sign-in/social` `{provider:"google", callbackURL?}` → `{url}` | google on |
| `GET /api/auth/callback/google` | google on |
| `GET /api/auth/get-session`, `POST /api/auth/sign-out`, `GET /api/auth/list-sessions`, `POST /api/auth/revoke-session`, `/revoke-sessions`, `/revoke-other-sessions`, `GET /api/auth/ok` | always |

Callback URLs must be paths on this origin. Codes: 6 digits, 10 minutes, one use,
5 wrong attempts void the code, stored hashed. Sends: one per address per 60 s,
5 per address per hour, 10 per client IP per hour (`429 auth.rate_limited` with
`Retry-After`). Sending a code answers the same for known and unknown addresses.

**Error codes** (`{"code","message"}` bodies; OAuth endpoints answer RFC 6749 `{error, error_description}`): `auth.csrf`, `auth.not_found`,
`auth.not_configured`, `auth.misconfigured`, `auth.invalid_email`,
`auth.invalid_code`, `auth.invalid_request`, `auth.invalid_callback`,
`auth.domain_not_allowed`, `auth.rate_limited`, `auth.email_unavailable`,
`auth.registration_refused`, `auth.email_unverified`,
`auth.internal_unauthorized`, `auth.no_session`, plus Better Auth's own
`INVALID_OTP`, `OTP_EXPIRED` and `TOO_MANY_ATTEMPTS` from sign-in.

## Multi-factor authentication

All paths below are relative to `/api/auth`. Mutations require the configured
origin. Account routes require a browser session. Successful factor
verification rotates the cookie. The browser should use the new cookie for
its next request. An email code or Google sign-in starts with no MFA time.

| Method and path | Request and result |
| --- | --- |
| `GET /passkey/generate-register-options` | WebAuthn creation options with required user verification |
| `POST /passkey/verify-registration` | `{response, name?}` with the WebAuthn registration response → `{status:true}` |
| `GET /passkey/generate-authenticate-options` | WebAuthn request options with required user verification. Works without a session. |
| `POST /passkey/verify-authentication` | `{response}` with the WebAuthn assertion → session cookie. Works without a session. |
| `GET /passkey/list-user-passkeys` | Array of credentials, with optional `name` and `lastUsedAt` |
| `POST /passkey/update-passkey` | `{id,name}` → `{passkey}` |
| `POST /passkey/delete-passkey` | `{id}` → `{status:true}` |
| `POST /two-factor/disable` | `{}` → `{status:true}` |
| `POST /two-factor/enable` | `{}` → `{totpURI,backupCodes}`. Enrollment finishes after TOTP verification. |
| `POST /two-factor/verify-totp` | `{code}` → `{status:true,enrolled:true}` for setup confirmation, or `{status:true}` and a new cookie for step-up |
| `POST /two-factor/verify-backup-code` | `{code}` → `{status:true}` |
| `POST /two-factor/generate-backup-codes` | `{}` → `{status:true,backupCodes}` |

`mfa_method` is `passkey`, `totp`, `backup_code`, or null. The verification
time uses Unix epoch milliseconds. Pending TOTP setup does not set
`mfa_enrolled`. Authenticator and backup-code checks reserve five attempts in
ten minutes per account, factor and client address, with a per-session fallback
when the address is unavailable. Exhausting the allowance locks that bucket
for 15 minutes (`429 auth.mfa_locked`). Passkey challenges expire after five
minutes and are consumed by the first submission. The same TOTP code cannot
verify another session within its acceptance window.

Enrollment and factor changes look up the directory policy through
`POST /internal/mfa-policy` on the Rust host. Only the auth service can make
this request. It sends the same verified identity and session facts as
`/internal/session` and receives the person's MFA policy and partial status.
A partial OAuth browser session continues through
`/settings/security?return_to=<encoded local OAuth path>`. The consent page
checks policy again when submitted. OAuth token introspection includes the
authorization's MFA time and method while the factor remains enrolled. Removing a factor clears the corresponding browser and OAuth proofs and refresh rechecks the remaining enrollment.

`POST /internal/mfa/factors` with `{auth_user_ids}` (1–256 IDs) answers
`{factors: [{auth_user_id, email, passkeys, authenticator}]}` for the known
IDs: the passkey count and whether a confirmed authenticator exists (a
pending setup does not count). Rust uses it for `authenticator_enrolled` and
`passkey_count` in `GET /v1/me/security` (both null when this service cannot
answer) and for the organization policy preview,
`PUT /v1/orgs/{org}/security?dry_run=true`. The preview saves nothing and
answers `{dry_run:true, policy, affected, must_enroll}`: `affected` lists
members the proposed policy covers who hold no allowed factor and cannot
enroll one themselves (`reason` `factor_not_allowed` or `cannot_enroll`);
`must_enroll` lists members with no factor who will create a passkey at
their next sign-in. Each entry has `user_id`, `name`, `email` and `role`.

An owner reset calls `POST /internal/mfa/reset` on the auth service with
`{auth_user_ids, actor}` after authorizing the owner in Rust. The server's
operator invokes `runbook-server admin mfa reset <email>`; on Cloudflare,
`npm run mfa:reset -- <email> --operator <name>` from `cloudflare/`. Both revoke sessions
and OAuth tokens, delete factors and record a reset audit event in the auth schema.
The user receives a security notification when mail transport is configured.

## OAuth 2.1 for remote MCP clients (MCP2)

`src/oauth.ts`: `@better-auth/oauth-provider` (Better Auth's OAuth provider plugin, the
same version as `better-auth`) makes this service the authorization server for the Rust
`/mcp`. Issuer `{RUNBOOK_ORIGIN}/api/auth`; one resource, `{RUNBOOK_ORIGIN}/mcp`.

- **Clients.** Open dynamic client registration (RFC 7591), 20 a day per client address.
  Only `none` and `client_secret_basic`; `jwks`, `jwks_uri` and software statements are
  refused, so the service never fetches an address a registrant chose. A registration with no
  `application_type` whose redirect URIs are all loopback `http` or private-use schemes is
  registered as `native` (RFC 8252), as MCP clients on a person's machine expect. Clients
  that never completed a grant are deleted after 7 days (swept on registration).
- **Grants.** Authorization code with PKCE (S256 only, always) and refresh tokens (with
  `offline_access`, rotated on every use; reusing a spent one revokes the family). No
  client credentials, no OpenID Connect.
- **Tokens.** Opaque, stored hashed: access `trb_oat_` (1 hour), refresh `trb_ort_`
  (30 days). No JWT plugin.
- **Scopes.** `runbooks:read`, `runbooks:write`, `runbooks:publish`, `executions:read`,
  `executions:start`, `executions:act`, `skills:read`, `offline_access`. Rust narrows a
  grant to the person's role.
- **Resource (RFC 8707).** An authorize request without `resource` gets `{origin}/mcp`
  added; any other resource is `invalid_target`.
- **Pages.** `GET/POST /api/auth/oauth/sign-in` (email code; the plugin's login page) and
  `GET/POST /api/auth/oauth/consent` are server-rendered, unstyled HTML with
  `default-src 'none'`, `frame-ancestors 'none'`, `X-Frame-Options: DENY` and
  `Referrer-Policy: same-origin` (never `no-referrer`: browsers would post the forms with
  `Origin: null`, which the Origin check refuses). The consent page names where the person
  is sent back (the redirect URI's host) and shows the client's self-registered name as
  "unverified (self-registered)", stripped of control, bidi and zero-width characters and
  cut to 64 characters. It lists every requested scope as a checkbox, ticked except
  `runbooks:publish`, which is granted only when the person ticks it. The posted scopes are
  always explicit (never the plugin's "nothing posted means everything").
- **Consent every time.** The gateway sets `prompt=consent` on every authorization
  (`none` is dropped), so a remembered consent never re-authorizes silently.
- **Grants and the web session.** A token's life is the grant's, not the browser
  session's: introspection checks the token row (expiry, revocation, client, audience,
  user). Signing out of the web app revokes the access tokens minted under that session
  (the plugin does that), but an `offline_access` refresh token stays and the tokens it
  mints work; a session that merely expires ends nothing.

| Route | Notes |
| --- | --- |
| `GET /.well-known/oauth-protected-resource`, `…/oauth-protected-resource/mcp` | RFC 9728 for `/mcp`; the host proxies them here |
| `GET /.well-known/oauth-authorization-server`, `…/oauth-authorization-server/api/auth` | RFC 8414 |
| `POST /api/auth/oauth2/register`, `/oauth2/token`, `/oauth2/revoke`, `/oauth2/introspect` | Client endpoints: no `Origin` check, cookies dropped |
| `GET /api/auth/oauth2/authorize` | Always answers with a redirect |
| `GET/POST /api/auth/oauth/sign-in`, `/api/auth/oauth/consent` | The pages; their POSTs need the trusted `Origin` |

Every other plugin route (client and consent administration, userinfo, the plugin's own
`/oauth2/consent`) stays `404 auth.not_found`.

**`POST /internal/oauth/introspect`** (D44 for OAuth tokens), same secret header:

```
{"token": "trb_oat_...", "resource": "https://runbooks.example.test/mcp"}

200 {"auth_user_id","email","email_verified","session_id":"oauth:<hex>",
     "expires_at":<ms>,"scope":"runbooks:read ...","client_id":"..."}
401 {"code":"auth.no_session"}   unknown, expired, revoked, or not for that resource
```

It is `/internal/session`'s shape plus `scope`, so Rust caches and links it like a
session (30 s, by a digest of the token); a 401 is remembered for 5 s. A backend failure
is `503 {"code":"auth.unavailable"}`, never "not active". A token is active only while its
grant lives: the consent still exists and is under 90 days old (the refresh grant checks
the same).

**Connected apps** (for Rust's `/v1/me/apps`), same secret header:

```
POST /internal/oauth/grants          {"auth_user_ids": [...]}
  200 {"grants": [{"client_id","client_name","redirect_hosts","scopes",
                   "created_at","last_used_at","expires_at"}]}     (ms)
POST /internal/oauth/grants/revoke   {"auth_user_ids": [...], "client_id": "..."}
  200 {"revoked": true | false}      consent deleted, its tokens revoked
```

**Workers note.** The plugin's init writes to the database (it seeds the resource row),
so every request awaits Better Auth's context: on Workers, I/O started by one request and
left pending cannot be awaited by another.

## Capture file (test-only)

One JSON object per line:
`{"at":"2026-11-12T08:00:00.000Z","to":"ada@example.test","type":"sign-in","code":"123456","subject":"..."}`.

## Schema and migrations

`src/schema/sqlite.ts` (tables `auth_user`, `auth_session`, `auth_account`,
`auth_verification`, `auth_rate_limit`, `auth_send_limit`, and MCP2's
`auth_oauth_client`, `auth_oauth_resource`, `auth_oauth_client_resource`,
`auth_oauth_access_token`, `auth_oauth_refresh_token`, `auth_oauth_consent`,
`auth_oauth_client_assertion` in `0001_oauth_provider.sql`) and `src/schema/pg.ts`
(the same tables in schema `auth`). `auth.config.ts` is the Better Auth CLI input
for regenerating the expected tables; `npm run db:generate` writes
`drizzle/{sqlite,pg}/`. Better Auth checks the Drizzle schema against what it needs
on first use, so drift fails both suites. The Postgres migration uses
`CREATE SCHEMA IF NOT EXISTS` because drizzle's migrator creates the schema first
for its own `auth.__drizzle_migrations` table.

## Dependencies

Exact pins. `better-auth`, `@better-auth/drizzle-adapter` and `@better-auth/passkey` 1.7.5, `drizzle-orm`
0.45.2, `hono` 4.13.8, `drizzle-kit` 0.31.10, `esbuild` 0.28.2, `miniflare`
5.20260918.0-alpha, `typescript` 7.0.2 and `@cloudflare/workers-types`
5.20260920.1 are Shelf's versions, and `package-lock.json` was seeded from Shelf's
lockfile so every shared transitive package resolves to Shelf's version. Not in
Shelf: `nodemailer` 10.2.1 (SMTP; MIT-0, no dependencies), `postgres` 3.4.9
(postgres.js, the Postgres driver; Unlicense, no dependencies),
`@better-auth/oauth-provider` 1.7.5 (MCP2; MIT; its dependencies `jose` and `zod` and its
peers were already in the lockfile; installed with `--legacy-peer-deps` because npm would
otherwise move `@better-auth/core` to the newest 1.7.x, which its exact peer pins refuse),
and
`@types/node` 26.6.5 with `undici-types` 8.9.0 (the versions `web/` already locks).
The Node HTTP adapter is ~40 lines in `server.ts` instead of `@hono/node-server`.

### Authenticator setup confirmation

`POST /api/auth/two-factor/enable` starts setup for the current browser session and
expires after ten minutes. When the account already has a factor or the
organization requires MFA, setup requires fresh verification with an allowed
factor. A user without factors in an MFA-required organization can bootstrap
with a passkey when permitted.

The first `POST /api/auth/two-factor/verify-totp` during setup returns
`{"status":true,"enrolled":true}` and does not rotate or upgrade the session.
Refresh the security view, then prompt for a new code or another allowed factor
if the next action needs step-up. Normal verification returns `{"status":true}`
and rotates the cookie. On `auth.mfa_setup_changed`, start setup again.

Owner reset returns HTTP 409 with `auth.mfa_operator_reset_required` when the target's auth identities cannot be reset within that organization. Show operator recovery guidance for this response.

### Browser session management

For enrolled accounts, listing and revoking sessions require a session with a
verified factor allowed by current policy. On `auth.mfa_required`, prompt for
MFA before showing session details or retrying revocation. Session listing
returns `id` and omits `token`. Send `{"session_id":"…"}` to
`POST /api/auth/revoke-session`.

Passkey registration requires a browser session created within 24 hours. On
HTTP 403 with `auth.sign_in_again`, prompt the user to sign in again and restart
the passkey registration ceremony.

Passkey assertions with an unknown credential or an invalid signature return HTTP 401 with `auth.passkey_assertion_failed`. Request fresh assertion options before retrying.

### API token issuance proof

Token creation and `GET /v1/tokens` expose `fresh_mfa_at_issue`. Treat a
missing value on an older server as false. Show false entries as needing
replacement for sensitive operations when the token is subject to required
MFA. When a sensitive bearer request returns
`auth.mfa_token_replacement_required`, create a replacement from a browser
after fresh MFA or approve a new CLI device login after fresh MFA.

Passkey registration challenges belong to the browser session that started them. On `auth.mfa_setup_changed`, begin registration again with the current session. Account reset fences pending ceremonies and invalidates all existing browser sessions.

MFA factor enrollment, removal and reset queue security mail in the same
transaction as the audit event. Native auth retries pending delivery once a
minute. Cloudflare installations should configure the auth Worker's
`scheduled` handler with a one-minute Cron Trigger (`* * * * *`). When mail
is unavailable, pending audit entries remain eligible for retry.


### Cloudflare operator recovery

An operator with write access to the installation's auth D1 database can
recover the last owner (or a shared account) without a public endpoint. From
`cloudflare/`, `npm run mfa:reset -- <email> --operator <name>` does it with wrangler: it shows
the account, asks for confirmation, and runs the reset as a single D1 SQL
import. It invalidates pending ceremonies, removes factors, revokes sessions
and OAuth tokens, records the operator reset and queues the security
notification. It then reads back the operator audit row. See
cloudflare/README.md §7, "Operator MFA reset". Re-enroll with an allowed
passkey when MFA is required and no factors remain.
