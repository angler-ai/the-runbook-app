# Release notes

Changes an operator should read before upgrading, newest first.

## 0.1.2

### Fixes

- **Claude can connect to remote MCP.** Client registration now accepts
  `client_secret_post`, which Claude uses. The sign-in service's metadata
  lists exactly the client authentication methods that registration
  accepts (`none`, `client_secret_basic` and `client_secret_post`), and no
  longer lists `private_key_jwt`.

### Upgrade order

No migrations. Upgrade the core in place.

## 0.1.1

The optional runner executes agent steps on your infrastructure. Both
editions also gain multi-factor sign-in.

### Highlights

- **Run agents from a runbook.** Organization owners and admins configure
  model providers and profiles, MCP servers, credentials, runner groups,
  policy and budgets. Projects define agents and select them by name on
  steps. The runner leases work, starts a sandbox when a step needs code
  execution, and records evidence, transcripts and usage. The new harness
  streams supported models, validates structured results and tracks
  model and sandbox usage against limits. See the
  [runner guide](https://docs.therunbook.app/runner/).
- **Let agents decide routine approvals.** An approval with
  `allow_agents: true` can name a project runner agent with
  `agent: { name: release-reviewer, after: 15m }`. People can decide at any
  time. The first decision wins, and a runner decision lease ends when
  the approval closes or the run is paused or ends. Decision runs get
  read-labeled tools, credentials and hosts, with no sandbox or memory
  proposals. The agent selects a listed option and gives a reason with
  tool-call references, or abstains. Review it on the approval card,
  in the agent's activity, with `runbook run get` or `runbook run report`,
  or through MCP's `get_execution`. `lease.revoked` is a new webhook
  event. Agent decisions cannot lead to destructive steps.
- **Multi-factor sign-in.** Add passkeys or an authenticator app with
  backup codes in **Settings › Manage security**. A passkey can also
  sign you in directly. Owners can require MFA for owners and admins or
  for everyone, choose allowed factors and set the freshness window
  (5 to 60 minutes, default 10). Sensitive changes prompt for a fresh
  check. Owners can reset factors for eligible members. The operator
  handles recovery for the last owner and accounts shared across
  organizations. See [recovery commands for both editions](https://docs.therunbook.app/security/multi-factor/#if-you-lose-your-factors).
- **MCP research tools.** ChatGPT's research modes can use read-only
  `search` and `fetch` to find and read runbooks, runs and active memory
  within the caller's access.

### Behavior changes

- Use the CLI to configure runner providers, profiles, MCP servers, groups,
  budgets and credential access. Sign in with `runner:read` and
  `runner:admin` scopes to manage the organization runner.

- New publications that gate a destructive runner step must name the
  authorizing options with `approves: [ship]`, unless the task's guard
  already requires the approval's decision to equal a specific option.
  The core requires a human decision from the run and checks that it is
  one of the allowed options. Existing published versions keep their
  behavior. Add explicit options when you republish, and use a guard to
  skip the task on a negative decision. Put the gate inside a loop if you
  need a new approval each iteration.
- New model provider credentials default to `read`. Existing labels
  carry over. Other built-in credential types still default to
  `destructive`. Review credential and tool labels before allowing an
  agent to decide approvals.
- MFA starts **off**. Enrolled accounts still need fresh browser checks
  on sensitive routes. When policy requires MFA, a personal or service
  token issued without a fresh check keeps its ordinary access and is
  refused on covered sensitive actions with
  `auth.mfa_token_replacement_required`. Reissue it through the web app
  or replace your CLI login by approving a new `runbook login` in the
  browser. MCP OAuth grants cannot perform sensitive actions that need
  a browser check.
- The GKE module enables Cloud SQL deletion protection and connector
  enforcement, hardens pods and network policy, and pins the Cloud SQL
  Auth Proxy. It supports external Kubernetes Secrets and IAM database
  authentication. Before applying to an existing installation, review
  the plan and the
  [module's migration instructions](https://github.com/angler-ai/the-runbook-app/blob/main/deploy/terraform/gcp-gke/README.md#cloud-sql-and-network-upgrades).

### Security and reliability

- The browser and CLI seal runner credentials to an ingest key
  authenticated by the registered vault identity. The vault stores
  destination-bound, envelope-encrypted values and releases them to the
  gateway under live claims. The gateway authenticates upstream requests
  outside the agent sandbox. See
  [what each component is trusted with](https://docs.therunbook.app/runner/security/).
- Hardened credential entry and policy changes, grant expiry and
  revocation, MCP request authorization, sandbox egress, redaction and
  transcript attribution. Abandoned claims and sandboxes are reaped.
  The model ledger accounts for interrupted calls and records unresolved
  usage for operator reconciliation.
- Hardened MFA enrollment, recovery, verification attempt limits and
  session invalidation. Factor changes and resets record an audit event
  and queue a security email. Sensitive browser actions fail closed
  when enrollment cannot be determined.
- HTTPS-configured app and file hosts send HSTS on both editions. Fixed
  runner handoffs, output-schema validation, runner error reporting in the
  CLI and MCP, and the task inbox for members without project grants.

### Upgrade order

1. **Back up and update the core and sign-in service.**
   - **Containers:** Run `deploy/update.sh` for Compose, or roll out the
     new image on your platform. Core PostgreSQL migrations run before
     traffic (or via `runbook-server migrate` if configured separately).
     The sign-in service applies its own PostgreSQL migrations
     (`0002`–`0006` for MFA) in the `auth` schema before logging
     `auth service ready`. Finish replacing old replicas before testing
     sensitive actions.
   - **Cloudflare:** From `cloudflare/`, run `npm run update -- --to 0.1.1`.
     The updater bookmarks D1, applies API and sign-in migrations, then
     deploys the sign-in Worker before the API Worker. For a manual
     update, apply both sets before traffic. The sign-in Worker needs
     `npx wrangler d1 migrations apply DB --remote -c wrangler.auth.prod.jsonc`
     (use your own auth config if different). Keep its
     `"triggers": { "crons": ["* * * * *"] }` so it sends security emails.
   - Check that `GET /v1/meta` reports the new release and
     `runner_contract_version` of at least **4**.
2. **Update the runner if you have one.** Stop old runner API instances
   and let active writes finish before running the runner database
   migrations. Migration `0018` removes the old credential writer's
   permissions. Runner migration `0027_task_claim_ledger.sql` links
   reservations to claims so settled task costs can reach the core even
   when a claim ends without a report. Refresh generated Compose settings with
   `node deploy/runner-install/setup.mjs --upgrade` and your install's
   arguments. Update the runner components together. Have an organization
   owner register the vault identity before entering credentials from
   the updated browser or CLI. See
   [runner setup](https://docs.therunbook.app/runner/setup/).
3. **Enable MFA.** Check sign-in first. An owner enrolls a factor,
   previews policy lockouts and then enables the desired requirement.
   Members it covers enroll or verify a factor at their next sign-in.
   A first enrollment under required MFA starts with a passkey. Prepare
   members before switching to an authenticator-only policy.

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
