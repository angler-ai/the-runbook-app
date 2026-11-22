# The Runbook: Cloudflare Edition

This directory contains the Cloudflare Workers edition of The Runbook (B2c, PRD §13–§14, §16, and plan decisions D55–D58, CF1–CF6).

**Plan:** this edition requires the **Workers Paid** plan ($5/month). Email sign-in codes need it, and it lifts the Free plan's CPU, subrequest and D1 query limits; §16 lists the limits that apply and what the edition does at each. To try The Runbook without it, run the container edition (`deploy/`) anywhere you can run a container and reach Postgres.

The application compiles natively to `wasm32-unknown-unknown` with `workers-rs`:
- The **api Worker** (`crates/runbook-worker`) routes incoming HTTP requests, bridges Durable Object WebSockets to standard SSE streams, plans webhook delivery, and schedules background cron tasks.
- **ExecutionDO** Durable Objects hold run-scoped state (event log, step runs, timers, leases, outbox, live SSE bridge) with single-threaded SQLite consistency per execution.
- **OrgDO** Durable Objects hold organization-scoped monthly execution quota counters and reservations.
- **WebhookDO** Durable Objects, one per webhook subscription, hold that subscription's delivery log, retry state and redelivery (CF11, §13).
- **D1** holds global and tenant-level entities: directory, credentials, catalog drafts and versions (by digest, §15), inbox tasks, admission queues, webhook subscriptions and events, memory entries with FTS5, and archive indexes. Every D1 access goes through the shard resolver (CF11, §14).
- **R2** stores immutable blobs (step log chunks, content-addressed snapshots and runbook sources, daily archives, and user evidence uploads).

---

## 0. Deploying From Scratch (Operator Steps)

The steps below are the order the first real deployment needed. Run them from `cloudflare/` with this directory's wrangler (`npx wrangler`).

1. **Create the resources.** Choose names (below uses the template's):
   ```bash
   npx wrangler d1 create runbook-production
   npx wrangler r2 bucket create runbook-blobs
   npx wrangler r2 bucket create runbook-user-content
   npx wrangler queues create runbook-notifications
   npx wrangler queues create runbook-notifications-dlq
   ```
2. **Write the two configs.** Copy `wrangler.jsonc.example` to `wrangler.prod.jsonc` and `wrangler.auth.jsonc.example` to `wrangler.auth.prod.jsonc`; both are git-ignored. Fill in the D1 ID, the bucket and queue names, and the origins. The api Worker's origin variable is **`ORIGIN`**; the auth Worker's is **`RUNBOOK_ORIGIN`**. Set both to the same app origin, and set `RUNBOOK_USERCONTENT_ORIGIN` on the api Worker to a different hostname (D50; production should use a separate registrable domain, not a sibling subdomain). Uncomment the api config's `AUTH` service and `assets` blocks. Keep the config in `cloudflare/`: its `build` command (the deploy build, no test seams) uses paths relative to this directory.
3. **Build.**
   ```bash
   node scripts/build-worker.mjs worker                       # api Worker -> build/ (wrangler deploy also runs this)
   (cd ../auth && node scripts/build.mjs)                     # auth Worker -> ../auth/dist/
   (cd ../web && node scripts/build-wasm.mjs && npm run build)
   node scripts/prepare-assets.mjs                            # web UI + _headers (CSP) -> build-assets/
   ```
4. **Apply the D1 migrations before traffic** (both sets go into the same database; see §2):
   ```bash
   npx wrangler d1 migrations apply DB --remote -c wrangler.prod.jsonc
   npx wrangler d1 migrations apply DB --remote -c wrangler.auth.prod.jsonc
   ```
5. **Deploy in this order.** The two Workers bind each other (`AUTH` and `API`), and a service binding to a Worker that does not exist yet fails the deploy:
   1. the auth Worker *without* its `services` block (comment it out once);
   2. the api Worker: `npx wrangler deploy -c wrangler.prod.jsonc`;
   3. the auth Worker again, with `services` restored: `npx wrangler deploy -c wrangler.auth.prod.jsonc`.
6. **Set the secrets** with `npx wrangler secret put NAME -c <config>`, piping each value in from a file outside the repository so it is never echoed: api Worker `BOOTSTRAP_SECRET` (§3), `WEBHOOK_SECRET_KEY` (§4), `RUNBOOK_UPLOAD_SIGNING_KEY` (§9) and `RUNBOOK_AUTH_INTERNAL_SECRET`; auth Worker `RUNBOOK_AUTH_SECRET` and the same `RUNBOOK_AUTH_INTERNAL_SECRET` (§7).
7. **Attach the two hostnames** as Workers custom domains of the api Worker (`routes` with `"custom_domain": true`, or the dashboard): the app origin and the user-content origin. First check the zone: a wildcard DNS record (`*`) already answers for every subdomain, and a custom domain would take that name over from it.
8. **Bootstrap** the instance (§3), then remove `BOOTSTRAP_SECRET`.

If the zone has **Browser Integrity Check** on, requests whose `User-Agent` it distrusts (Python's default `Python-urllib`, for one) get Cloudflare's `403 error code: 1010` before they reach the Worker; send an explicit `User-Agent` from scripts. The CLI and curl pass.

---

## 0a. Updating an Existing Deployment (UPD1)

From `cloudflare/`, with the two configs from §0 and your wrangler login:

```bash
npm run update -- --dry-run                       # the plan; read-only toward the deployment
npm run update                                    # the latest published release, asking first
npm run update -- --to 0.3.1 --yes                # a named release (--to 0.3: the newest 0.3.x)
npm run update -- --source checkout               # build and deploy this checkout as it is
npm run update -- --source checkout --to latest   # check the highest vX.Y.Z tag out, then that
```

**Sources.** By default the update fetches the matching GitHub Release of `angler-ai/the-runbook-app` (`--release-repo`, `RUNBOOK_RELEASE_REPO`; set `GITHUB_TOKEN` while the repository is private), checks `runbook-cloudflare-X.Y.Z.tar.gz` against the release's `SHA256SUMS` and its build provenance with `gh attestation verify --repo angler-ai/the-runbook-app --signer-workflow angler-ai/the-runbook-app/.github/workflows/release.yml --source-ref refs/tags/vX.Y.Z` (gh's own message is shown when it fails) (refused without `gh` unless `--insecure-skip-verify`; attestations need the repository public, which it will be before the first public release), unpacks it, installs its pinned wrangler (`npm ci`) and runs the bundle's own `scripts/update.mjs`, which deploys its prebuilt Workers and web UI with a copy of your configs beside them (without their `build` command): nothing is built. **No release is published yet:** until one is, use `--source checkout`, which builds here; with `--to` it checks the tag out (it refuses local changes to tracked files), runs `npm ci` in `cloudflare/`, `auth/` and `web/`, and hands over to that release's own script.

What it does (`scripts/update.mjs`, the §0 steps in update order):

1. **Preflight (read-only):** both configs (`--config`, `--auth-config`, or `--no-auth`); `wrangler whoami`; the deployed release from `/v1/meta` (`--url`, default the config's `ORIGIN`); the target release (`RELEASE.json` in a bundle, `Cargo.toml` in a checkout). A downgrade is refused; the same release is "nothing to do" (with `--source checkout --redeploy`, a test site tracking main is deployed anyway). The config's Durable Object `migrations` must include every tag in the release's `wrangler.jsonc.example` (it prints any to add). Each Worker's current version (`wrangler deployments status`) is recorded for rollback, and each D1 database's migration state is read.
2. **Build** (checkout only) the api Worker (deploy build), the sign-in Worker, and the web UI with its `_headers` when the config serves assets. A failed build changes nothing.
3. **Back up and migrate:** a D1 Time Travel bookmark per database, the sign-in Worker's too when it is separate (`wrangler d1 time-travel info`; an empty answer stops the update; the bookmarks are printed with their `wrangler d1 time-travel restore … --bookmark=…` commands and saved, 0600, in `.update/bookmarks-<time>.json`, `--state-dir`), then the pending D1 migrations with `wrangler d1 migrations apply --remote`: every api D1 binding with a `migrations_dir` (all shards, §14), then the sign-in Worker's tables. Files the Worker's own runner already applied (it migrates a database on first use and records that in `schema_migration`) are first recorded in wrangler's `d1_migrations`, so wrangler applies only what neither has; the runner in turn adopts what wrangler applied (§2). Durable Object schemas migrate inside each object on first use; their class migrations ride on `wrangler deploy`. Blobs in R2 are not touched by a migration and are not copied.
4. **Deploy** the sign-in Worker, then the api Worker.
5. **Verify** that `/v1/meta` reports the new `version` and its `schema` heads (`d1`, `execution_do`, `org_do`, `webhook_do`) and that discovery answers.

**Rollback.** If step 4 or 5 fails, each Worker whose deploy succeeded is rolled back with `wrangler rollback <recorded version>` (api first). The schema stays as migrated: every migration keeps the previous release working (expand/contract, `CONTRIBUTING.md`), and the previous release's D1 and Durable Object runners accept a newer release's schema as long as every file they know is applied unchanged. Restoring D1 itself (the printed Time Travel command) loses everything written since, and does **not** restore Durable Object or R2 state: runs, delivery logs and blobs written since stay and may refer to rows the restore removes. So the script restores D1 only with `--restore-backup`, which an update needs when the deployed release is below the target's **rollback floor** (`RELEASE.json`: the oldest release whose code is safe on its schema, for example after skipping a release that dropped a column). Floors are per store (`d1`, `execution_do`, `org_do`, `webhook_do`): a Durable Object floor cannot be met by restoring D1, so below one the update needs `--accept-no-rollback` and a failure is not rolled back. **Durable Object class migrations:** Cloudflare cannot roll a Worker back across a class lifecycle change ([Rollbacks](https://developers.cloudflare.com/workers/configuration/versions-and-deployments/rollbacks/), [Durable Object migrations](https://developers.cloudflare.com/durable-objects/reference/durable-objects-migrations/)). `migrations/do-classes.json` records the release each tag arrived in; an update that brings one says so up front and needs `--accept-no-rollback`, and then a failure is not rolled back. The sign-in and api Workers run adjacent releases for a moment during an update or a rollback; their internal contract only grows (`api/internal/`). Exit status: 0 updated, 1 refused (nothing changed), 3 failed and rolled back, 4 failed and not (fully) rolled back.

The edition requires the Workers Paid plan (decision 2026-11-13).

**Strict assignment and take-over across the LANG7 update (or a rollback past it).** LANG7 records two new run events, `StepAssigneeResolved` (a step with `assignment: strict`) and `StepTakenOver` (a person took over an agent step), and stores runbooks with `assignment: strict`. A release before LANG7 cannot read them: a run holding one fails to load there (its reads and actions answer an error) until the newer release serves it again; nothing is lost or rewritten. Deploy order: the update deploys the api Worker last (step 4), and people should not publish strict runbooks or take steps over until it has finished; while old and new isolates both serve during the deploy, an old one refuses both (publishing `assignment: strict` fails, a take-over is refused as a claim of an agent step, `claim.actor_mismatch`) and a request it serves for a run a new one just changed answers an error, which a retry clears. After a rollback past LANG7, runs holding these events stay unreadable until the next update; other runs are not affected, and an archive holding them does not import into the older release.

**Request replays across the CF12 update (or a rollback past it).** From CF12 a replay row keeps a request payload over 64 KiB as its digest and a response over 64 KiB in the project's R2 blobs (§16). A release before CF12 cannot read either. For up to 24 hours (a replay row's life) after a rollback to one, or while old and new isolates both serve during the deploy, a retry of a large request (a runbook created from a source over about 64 KiB, a large skill upload) whose row the newer release wrote answers `503 storage.unavailable` instead of its recorded response. It never applies the request twice; once the row expires the key is free again, as for any key. Small requests replay as before in both directions.

**Replay responses across the CF12b update (or a rollback past it).** CF12b keeps a large replay response under the project's `replay/` prefix in R2, outside its blobs (§16), instead of among them. CF12b reads rows CF12 wrote, so they keep replaying until they expire. A CF12 release cannot read a row CF12b wrote: for up to 24 hours after a rollback to CF12, a retry of a large create or skill upload whose row CF12b wrote answers `503 storage.unavailable` (it never applies the request twice), as for the CF12 update above.

**Run storage across the CF13 update (or a rollback past it).** CF13 changes how an ExecutionDO keeps its run, so a long or wide run never outgrows Durable Object SQLite's 2,000,000-byte row (before it, report 31 of a 40-step run with 65,000-byte summaries answered `503 storage.unavailable` forever). The object's migration `0006_split_rows.sql` (expand-only: two nullable columns and a new table, applied inside each object on first use) lets the head keep only the run-level fields while each step run, and each open inbox assignment of a derived row, is a row of its own. Runs stored before the update are read as they are and split by their next change; nothing needs migrating up front. A change one of whose rows would still pass the limit is refused with a non-retryable `422 execution.row_too_large` instead of a 503 (the CLI and MCP do not retry it). **Rollback:** a release before CF13 cannot read a run CF13 has written or changed. It refuses it rather than misread it (the head it requires is missing): that run's reads and actions answer `503 storage.unavailable` and its settling and D1 drain retry, until the newer release serves it again. Nothing is lost or rewritten, and runs CF13 never touched keep working. No rollback floor is set (as for LANG7: no release has shipped, and a floor would refuse every rollback past CF13 even for runs it never touched).

**CF13 review round 1 (engine version 5, the run budget, a lighter object protocol).** Runs started after the update record engine version 5 (a run's outputs together held to 900 KiB). A release before it refuses such a run (`execution.unsupported_engine_version`) rather than replay it under older rules: after a rollback its reads and actions answer an error until the newer release serves it again; runs started before the update are unaffected. The api Worker now commits a session with a new object operation (`commit_fenced_ref`, without the run's snapshot); an ExecutionDO of the previous release answers it with an error, which a retry clears once both run the new release (the update deploys them together). The ExecutionDO still serves the old operation, so a previous api Worker keeps working against a new object. **Purging after a rollback:** a release before CF13 does not know the `derived_assignment` table, so when it purges a deleted run's object (after the deletion committed in D1) it leaves that run's inbox assignment rows (their titles and paths) in the object. They are unreachable through the API (the run's head is gone) and are removed if a CF13 or newer release purges the object again; delete runs only after the update has finished, or expect such rows to remain in objects purged while rolled back.

**Runs started before engine version 5 (CF13b).** The outputs budget holds only runs that recorded engine version 5 or later, so a run started before the CF13 update replays under its own rules: if its runbook's outputs together pass what one row holds (for example 40 outputs that each copy a 60,000-byte step output), the commit that would close it still writes a head past 2,000,000 bytes and is refused with `422 execution.row_too_large`. Such a run cannot close on Workers: the report that would complete it and a cancel answer that 422, and a timer-driven close is refused the same way, so it stays open. Runs started after the update, and runs whose outputs fit, are unaffected; the native edition closes them all.

**CF13b (engine version 6, the run ceiling).** Runs started after the update record engine version 6: a run's event log has a hard ceiling (one and a half times `RUNBOOK_LIMIT_RUN_BYTES`, 12 MiB by default), past which the engine faults it with `limit.run_size_exceeded` and it closes in the same commit (the commit that starts it, when its first steps already pass the ceiling). The fault cancels every open step and nothing else runs: no `catch` handler, no group `finally` and no top-level `finally`. The run closes failed with reason `limit.run_size_exceeded`, except that a run already cancelled stays cancelled, and a run already failing keeps a reason it recorded (an `end` step's, for example); one failing from a step failure has recorded none, so it gets `limit.run_size_exceeded`. A release before CF13b refuses a version 6 run (`execution.unsupported_engine_version`) as for version 5 above: after a rollback its reads and actions answer an error until the newer release serves it again. Runs started before the update keep their own version and are not held to the ceiling (only to the request budget).

---

## 1. Cloudflare Resource Bindings & Configuration

The table below lists all bindings, objects, buckets, queues, schedules, variables, and secrets required by the Cloudflare edition:

| Name / Binding | Type | Resource / Class | Purpose & Contract |
| --- | --- | --- | --- |
| `DB` | D1 Database | `runbook-production` | Primary relational database: the control database and, by default, every organization's shard (§14). Migrations reside in `migrations/d1`. Applies migrations 0001–0029 (Postgres parity) and 1001–1010 (D1-only). |
| `DB_2`, … *(optional)* | D1 Database | one per extra shard | Further organization shards named by `RUNBOOK_D1_SHARDS` (CF11, §14). Each gets the same migrations. |
| `EXECUTIONS` | Durable Object | `ExecutionDO` (tag `v1`) | One SQLite Durable Object per execution (`exe_*`). Houses the execution projection head, event stream, alarms, leases, and outbox intents. |
| `ORGS` | Durable Object | `OrgDO` (tag `v2`) | One SQLite Durable Object per organization (`org_*`). Tracks monthly execution counters and reservations (`usage_monthly`, `quota_reservation`). |
| `WEBHOOKS` | Durable Object | `WebhookDO` (tag `v3`) | One SQLite Durable Object per webhook subscription (`wh_*`). Holds its delivery log (`webhook_delivery`, `webhook_attempt`, `migrations/webhookdo/`), retry and backoff state and redelivery; its alarm attempts due deliveries (CF11, §13). |
| `BLOBS` | R2 Bucket | `runbook-blobs` | Object storage for execution log chunks, content-addressed snapshots (`shared/{sha256}`), and daily archives (`archives/{runbook}/{date}/{digest}.zip`). |
| `USER_CONTENT` | R2 Bucket | `runbook-user-content` | Object storage for binary evidence uploads (`runs/{exe}/uploads/{evd}`). Served from a separate hostname with restrictive sandbox headers (D50, D57). |
| `NOTIFICATIONS` | Queue Producer | `runbook-notifications` | Queue producer binding used by ExecutionDO outbox to enqueue webhook notification intents. |
| Consumer | Queue Consumer | `runbook-notifications` | Queue consumer handled by the api Worker. Configured with `max_batch_size: 10`, `max_batch_timeout: 1s`, `max_retries: 100`, and dead-letter queue `runbook-notifications-dlq`. |
| Dead-Letter Queue | Queue (DLQ) | `runbook-notifications-dlq` | Dead-letter queue parking notification intents that fail fan-out delivery after 100 retries. |
| `* * * * *` | Cron Trigger | Every minute | Project events fan-out (`cron::project_events_pass`, e.g. `runbook.published`, CF9), admission and supersede notices, unsettled starts and unfinished purges, on every D1 shard. Webhook retries are each WebhookDO's own alarm (CF11). |
| `17 0 * * *` | Cron Trigger | Daily at 00:17 UTC | Daily archive and retention pass (`cron::archive_pass`): packages completed runs into deterministic daily ZIP archives and applies data retention policies; quota sweep; webhook delivery-log retention (90 days, as native, `cron::delivery_log_pass`, CF11). |
| `AUTH` *(optional)* | Service Binding | `runbook-auth` | Internal service binding connecting the api Worker to the Better Auth sign-in Worker (`auth/src/worker.ts`) for session resolution without public routes (CF7, D44). |
| `ASSETS` *(optional)* | Static Web Assets | `build-assets` (`web/dist`) | Static web assets binding for the web UI. Uses `run_worker_first` so API routes and SPA navigation route through the Worker while hashed bundles serve directly. |
| `ORIGIN` | Environment Variable | e.g. `https://api.runbooks.example.com` | Canonical public origin URL of the server. Required for link generation and CSRF validation. |
| `RUNBOOK_USERCONTENT_ORIGIN` *(optional)* | Environment Variable | e.g. `https://usercontent.runbooks.example.com` | Separate origin for binary evidence uploads. Must differ from `ORIGIN` by hostname or port (CF7, D49). |
| `RUNBOOK_D1_SHARDS` *(optional)* | Environment Variable | JSON string | The D1 shard mapping (CF11, §14), e.g. `{"control": "DB", "default": "DB", "orgs": {"org_…": "DB_2"}}`. Unset maps everything to `DB`; a value that does not parse refuses requests (`d1.shards_invalid`). |
| `RUNBOOK_LIMIT_RUN_BYTES` | Environment Variable | Positive integer (bytes) | The most a run's event log may hold (default `8388608`, 8 MiB). A request whose own event carries more than 4 KiB and would pass it is refused with HTTP 422 `execution.too_large`; claims, cancels and short reports always pass. It also sets the hard ceiling (CF13b): an open run whose log passes one and a half times it (12 MiB by default) through the engine's own events or small writes is faulted with `limit.run_size_exceeded` and closes failed. An invalid value keeps the default and is logged. |
| `RUNBOOK_LIMIT_STEP_RUNS` | Environment Variable | Positive integer or `"off"` | The most step runs one run may open. A run that would open one more fails with `limit.step_runs_exceeded`, keeping what it did. Default is no cap. An invalid value keeps no cap and is logged. |
| `RUNBOOK_LIMIT_EXECUTIONS_MONTHLY` | Environment Variable | Integer or `"off"` | Monthly execution quota per organization. Exceeding starts are refused with HTTP 429 `limit.executions_monthly`. Default is `"off"`. |
| `RUNBOOK_THEME_ACCENT`, `RUNBOOK_THEME_ACCENT_SECONDARY` *(optional)* | Environment Variable | `#rrggbb` | UI-1: the web app's accent and secondary accent colours in both themes. `/v1/meta` reports them as `theme` (`light` and `dark`, each colour `#rrggbb` lowercase; no `theme` when none is set). |
| `RUNBOOK_THEME_DARK_ACCENT`, `RUNBOOK_THEME_DARK_ACCENT_SECONDARY` *(optional)* | Environment Variable | `#rrggbb` | UI-1: dark-theme values; each, when set, replaces its base value in the dark theme only (light = base; dark = dark value, else base). Empty means unset. A value that is not `#rrggbb` (either case; `#rgb`, names and a missing `#` are refused) is logged once per isolate (`config.invalid_theme_color`, naming the variable) and the whole override is ignored: the default theme is served (the native server refuses to start instead; a Worker has no startup, and refusing every request over a cosmetic setting would be an outage). A colour below WCAG AA contrast (4.5:1 against `#ffffff`, or `#0e0f14` in the dark theme) is only logged, once per isolate. Same names as the native server. |
| `RUNBOOK_RETENTION_LIVE_DAYS` | Environment Variable | Integer or `"off"` | Days to keep live execution detail before purging (default: unset / `"off"`). Setting this enables deletion of live run detail. |
| `RUNBOOK_RETENTION_ARCHIVE_DAYS` | Environment Variable | Integer or `"off"` | Days to keep daily archive ZIP files in R2 (default: unset / `"off"` / indefinite). |
| `RUNBOOK_RETENTION_SUMMARY_DAYS` | Environment Variable | Integer or `"off"` | Days to keep `archive_run` summary rows in D1 (default: unset / `"off"` / indefinite). Must be `>= RUNBOOK_RETENTION_ARCHIVE_DAYS` and unset when archives are indefinite. |
| `BOOTSTRAP_SECRET` | Secret | 32+ characters | Secret required to authenticate first-run initialization via `POST /internal/bootstrap`. Unset after setup. |
| `WEBHOOK_SECRET_KEY` | Secret | 32 bytes (base64) | Key used to seal and unseal webhook signing secrets at rest with AES-256-GCM. |
| `WEBHOOK_PREVIOUS_SECRET_KEYS` | Secret *(optional)* | Comma-separated base64 keys | Previous keys allowed for decrypting stored secrets during rotation. |
| `RUNBOOK_UPLOAD_SIGNING_KEY` *(optional)* | Secret | 32 bytes (base64) | Secret key used to sign and verify temporary upload/download URLs for binary evidence (CF7). |
| `RUNBOOK_OPERATOR_SECRET` *(optional)* | Secret | 32+ characters | Secret enabling `/internal/portable/import` and `/internal/portable/export` (CF8). Absent / 404 otherwise. |
| `RUNBOOK_AUTH_INTERNAL_SECRET` *(optional)* | Secret | 32+ characters | Shared internal secret between api Worker and Better Auth Worker for `/internal/session` and `/internal/can-register` (CF7). |

---

## 2. D1 Migrations: Applying Before Traffic

D1 does not support interactive transactions. To maintain consistency, schema changes are organized into ordered migrations in `cloudflare/migrations/d1/`:
1. **0001–0029**: Mirror the core PostgreSQL schema (migrations 0001 through 0029), matching table structures, columns, and foreign keys.
2. **1001–1010**: D1-only extensions that address SQLite/D1 operational requirements:
   - `1001_d1_guard.sql`: Guard table and triggers implementing atomic conditional batches (CF3-2).
   - `1002_execution_token_route.sql`: Hash route mapping execution tokens to their ExecutionDO (CF2-9).
   - `1003_memory_fts_words.sql`: Memory entry full-text search indexing with FTS5 triggers (CF3-3).
   - `1004_run_search_words_and_admission_notice.sql`: Run summary full-text search and concurrency promotion notices.
   - `1005_admission_pool_revision.sql`: Optimistic pool revisions for concurrency queues.
   - `1006_object_purge.sql`: Tombstones and purge markers for completed runs.
   - `1007_archive_version.sql`: Daily archive versioning support.
   - `1008_supersede_notice.sql`: Notifications for evicted concurrency waiters.
   - `1009_webhook_log_to_objects.sql`: Drops the webhook delivery log from D1; it lives in each subscription's WebhookDO (CF11, §13).
   - `1010_runbook_source_digests.sql`: Digest and size columns for runbook source kept in R2 (CF11, §15).

With more than one D1 shard (§14), apply the same migrations to every shard binding. The Worker also migrates every database the mapping names on its first request.

### Migration Commands

Always apply D1 migrations **before** routing production traffic:

```bash
# 1. Apply all pending migrations to production D1:
npx wrangler d1 migrations apply DB --remote

# For local development / testing with wrangler dev:
npx wrangler d1 migrations apply DB --local
```

The Worker's own migration runner records applied versions in `schema_migration`; `wrangler d1 migrations apply` records them in its `d1_migrations` table. Whenever the runner finds files pending (the first deployment, and every update, UPD1), it adopts wrangler's record (each file wrangler applied is marked applied) and applies only what is left; `npm run update` does the reverse before it applies (§0a). A database a newer release migrated (after a `wrangler rollback`) is accepted when every file the code knows is applied unchanged. Before this fix (found in the first real deployment), the runner re-ran every file and every API request failed with `d1.migrate: duplicate column name`. Deployed workers also verify schema freshness via `ensure_schema`, but pre-applying migrations avoids cold-start latency on first requests.

---

## 3. Instance Bootstrap (`BOOTSTRAP_SECRET`)

To initialize an empty deployment without local CLI access to the database:

1. **Generate a strong secret (32+ characters):**
   ```bash
   openssl rand -hex 32
   ```

2. **Configure `BOOTSTRAP_SECRET`:**
   ```bash
   npx wrangler secret put BOOTSTRAP_SECRET
   ```

3. **Call the bootstrap endpoint:**
   Send an HTTP `POST` request to `/internal/bootstrap` with the secret in the `x-bootstrap-secret` header:
   ```bash
   curl -X POST https://api.runbooks.example.com/internal/bootstrap \
     -H "x-bootstrap-secret: <BOOTSTRAP_SECRET>" \
     -H "Content-Type: application/json" \
     -d '{
       "organization": "My Organization",
       "user_name": "Initial Admin",
       "project_slug": "default",
       "owner_email": "admin@example.com"
     }'
   ```

   **Response (`201 Created`):**
   ```json
   {
     "org_id": "org_01J...",
     "user_id": "usr_01J...",
     "project_id": "prj_01J...",
     "personal_token": "trb_pat_...",
     "service_token": "trb_svc_..."
   }
   ```
   *Note: Personal and service tokens are displayed once; only their SHA-256 hashes are persisted.*

4. **Security & Cleanup:**
   - The bootstrap route is rate-limited to 5 attempts per minute per isolate.
   - Subsequent calls return `409 Conflict` (`bootstrap.already_initialized`).
   - The body is read with the same cap as every JSON route (2 MiB + 64 KiB); more is `413 request.body_too_large`, before the rest is read.
   - Once bootstrap is completed, remove the secret to disable the route entirely (returns `404 Not Found`):
     ```bash
     npx wrangler secret delete BOOTSTRAP_SECRET
     ```

---

## 4. Webhook Sealing Key (`WEBHOOK_SECRET_KEY`)

Webhook subscription endpoints receive signing secrets that are encrypted at rest using AES-256-GCM before storage in D1:

1. **Generate the key:**
   ```bash
   openssl rand -base64 32
   ```

2. **Store as a Worker secret:**
   ```bash
   npx wrangler secret put WEBHOOK_SECRET_KEY
   ```

3. **Key Rotation:**
   To rotate the sealing key without invalidating existing stored webhook secrets:
   - Generate a new 32-byte base64 key.
   - Add the previous key to `WEBHOOK_PREVIOUS_SECRET_KEYS` (comma-separated if multiple):
     ```bash
     npx wrangler secret put WEBHOOK_PREVIOUS_SECRET_KEYS
     ```
   - Update `WEBHOOK_SECRET_KEY` with the new key.
   - Existing secrets continue to decrypt via previous keys, while new secrets are sealed with the current key.

---

## 5. Dead-Letter Queue Redrive

When an execution outbox emits a webhook notification, it is enqueued to `runbook-notifications`:
- The Worker attempts delivery to all active subscriptions matching the event.
- If delivery fails due to transient network issues (5xx, 429, timeouts), the message is retried with exponential backoff up to 100 times (`max_retries: 100`, ~100 minutes).
- If retries are exhausted, the message is parked in `runbook-notifications-dlq`.

### Redriving Messages

To reprocess messages from the dead-letter queue after resolving subscriber outages:

1. **Via Cloudflare Dashboard:**
   - Navigate to **Workers & Pages** > **Queues**.
   - Select `runbook-notifications-dlq`.
   - Click **Redrive Messages** and choose `runbook-notifications` as the destination.

2. **Via Wrangler CLI:**
   ```bash
   npx wrangler queues redrive runbook-notifications-dlq --queue runbook-notifications
   ```

---

## 6. Retention Policies & Quotas

### Retention Variables

**Retention is OFF by default**; nothing is deleted unless an operator explicitly configures a retention window. Setting `RUNBOOK_RETENTION_LIVE_DAYS` enables deletion of live run detail.

When enabling retention, configure these variables in `wrangler.jsonc` under `vars`:

```jsonc
"vars": {
  // Live run detail purged after 90 days (enables live detail deletion):
  "RUNBOOK_RETENTION_LIVE_DAYS": "90",
  // Daily archive ZIPs in R2 kept for 365 days (or "off" for indefinite):
  "RUNBOOK_RETENTION_ARCHIVE_DAYS": "365",
  // Run summary index rows in D1 kept for 730 days (or "off" for indefinite):
  "RUNBOOK_RETENTION_SUMMARY_DAYS": "730"
}
```

- `RUNBOOK_RETENTION_LIVE_DAYS`: Days live execution state (in ExecutionDO) is retained before daily archiving and purge (default `"off"` / unset). Setting a positive integer enables deletion of live execution state.
- `RUNBOOK_RETENTION_ARCHIVE_DAYS`: Days the daily archive ZIP file is retained in R2 (default `"off"` / unset / indefinite).
- `RUNBOOK_RETENTION_SUMMARY_DAYS`: Days high-level run summary rows are retained in D1 (default `"off"` / unset / indefinite).
- **Startup Rules & Constraints:**
  1. $$\text{live\_days} \le \text{archive\_days} \le \text{summary\_days}$$
  2. `RUNBOOK_RETENTION_SUMMARY_DAYS` must be greater than or equal to `RUNBOOK_RETENTION_ARCHIVE_DAYS`.
  3. When archives are kept indefinitely (`RUNBOOK_RETENTION_ARCHIVE_DAYS` is `"off"` or unset), `RUNBOOK_RETENTION_SUMMARY_DAYS` must also be unset or `"off"`.

### Run Size Budget (CF13 review round 1)

Every request on a run loads the whole run (its event log, step rows and the state rebuilt from them) into the api Worker and its ExecutionDO, each an isolate of at most 128 MB. A run's event log is therefore held to a budget, the same on the native edition:
- `RUNBOOK_LIMIT_RUN_BYTES` (default `8388608`, 8 MiB): a report, signal, decision, override or edit whose own event carries more than 4 KiB and would take the log past it is refused with a non-retryable HTTP 422 `execution.too_large`, and nothing is written. Engine work (timers, settling, a child's outcome) is never refused, and neither is a small write: a claim, a release, a cancel or a short report still passes, so a run at its budget can be finished or cancelled by a person.
- Why 8 MiB: measured natively with the real router over the object protocol (`crates/runbook-worker/tests/run_memory.rs`), one report request holds about 7.3 to 8 times the log at its peak (a 100-item `foreach` of 5 tasks with 64,000-byte summaries: 69.8 MB at an 8.8 MB log). Before CF13's review round 1 a commit also shipped the whole run back to the object and the session copied it, about 19 times the log (250.6 MB at 13.1 MB). Raise the budget only with that in mind.
- A run's outputs together are held to 900 KiB by the engine (engine version 5): a run that would succeed with more fails with `execution.outputs_too_large` instead.
- **The hard ceiling (CF13b, engine version 6).** The budget refuses only a request's large event; the engine's own events (each `call` step's result, up to 900 KiB, a `set` step's new values) and small writes still grow a run. At one and a half times the budget (12 MiB by default, derived from `RUNBOOK_LIMIT_RUN_BYTES`) an open run is faulted with `limit.run_size_exceeded` in the settle that passes it, its activation included, and closes in the same commit. Every open step is cancelled and no `catch` handler, group `finally` or top-level `finally` runs. It closes failed with that reason, except that a run already cancelled stays cancelled and a failing run keeps a reason it recorded (a plain step failure records none, so it gets this one). Memory writes settle before they commit, so they are held to it too. Not twice the budget: a request holds up to about 8 times the log, so 16 MiB and its closing commit would pass an isolate. Runs started before CF13b are not held to it.

### Monthly Execution Quotas

Set `RUNBOOK_LIMIT_EXECUTIONS_MONTHLY` to cap total run starts per organization per calendar month (UTC):
- `"off"` or unset: Unlimited executions (default).
- Positive integer (e.g. `"1000"`): Caps starts. When an organization reaches the limit, new runs fail immediately with HTTP 429 `limit.executions_monthly`.
- Quotas are enforced in the `OrgDO` before D1 commits. Replays and starts cancelled while queued are automatically refunded.

---

## 7. Sign-In & The Auth Worker (CF7)

The Cloudflare edition separates browser authentication from the main API engine:
- The **auth Worker** (`auth/src/worker.ts`) runs [Better Auth](https://better-auth.com) over the same D1 database (`database_id`), applying its schema from `auth/drizzle/sqlite`. It has no public route of its own.
- The **api Worker** forwards `/api/auth/*` requests across the internal `AUTH` service binding, preserving headers and cookies while stripping internal service headers (`x-runbook-internal-secret`, `x-runbook-client-ip`).
- **Session Verification (D44):** The api Worker verifies session cookies via `POST /internal/session` on the auth Worker using `RUNBOOK_AUTH_INTERNAL_SECRET` (32+ characters). Responses are cached for 30 seconds per isolate keyed by the cookie's SHA-256 digest (never the raw cookie). Proxied sign-out evicts the session from the cache of the isolate that handled it; other isolates may still honour the signed-out cookie until their cached entry expires, for up to 30 seconds (gate fix B6). Open event streams re-check the session at every keep-alive (every 30 s) through the same cache, so a signed-out stream can last up to about 60 seconds: one re-check interval plus one cache lifetime (gate round 2).
- **Email codes through Cloudflare Email Service (D48):** the auth Worker sends sign-in codes through a `send_email` binding named `EMAIL` when `RUNBOOK_AUTH_EMAIL_TRANSPORT=cloudflare` and `RUNBOOK_EMAIL_FROM` (`Name <address>` or an address, which must be in the binding's `allowed_sender_addresses`) are set; see `wrangler.auth.jsonc.example`. A send that fails voids the code, and neither codes nor provider errors are logged. Sending to arbitrary recipients needs the **Workers Paid** plan (3,000 messages a month included, then $0.35 per 1,000) and a sending domain onboarded once in the dashboard (Compute → Email Service → Email Sending → Onboard Domain, then review the SPF, DKIM, DMARC and bounce records before applying them). The native edition keeps SMTP.
- **Can-Register Callback (D45):** During sign-up, the auth Worker calls back into the api Worker through its `API` service binding (`POST /internal/can-register`) with the internal secret to verify tenant registration eligibility.
- **MFA notices:** factor changes and resets queue a security email in the same D1 transaction as their audit record. Give the auth Worker a one-minute Cron Trigger (`"triggers": {"crons": ["* * * * *"]}` in its config) so its `scheduled` handler delivers them.

### Operator MFA reset

An organization owner resets another member's second factors from the app (Members › Reset MFA). Nobody can do that for the **last owner**, or for a person who belongs to more than one organization (or whose sign-in is shared across them): the app answers `auth.mfa_operator_reset_required`. The operator resets them with wrangler, the Workers equivalent of the native `runbook-server admin mfa reset <email>`:

```bash
npm run mfa:reset -- owner@example.test --operator "Alex Doe"                     # shows the account, asks, resets
npm run mfa:reset -- owner@example.test --operator "Alex Doe" --yes --auth-config wrangler.auth.prod.jsonc
npm run mfa:reset -- owner@example.test --operator "Alex Doe" --local             # the local D1 of `npm run dev`
```

`scripts/mfa-reset.mjs` uses your wrangler login against the sign-in Worker's `DB` database. It looks the account up by its verified sign-in email and prints its passkeys, authenticator, sessions and OAuth tokens; after you confirm (or with `--yes`), it runs the reset as a single D1 SQL import, exactly as the native command does: it invalidates pending ceremonies, removes every factor and backup code, ends every session and OAuth token, records `mfa.reset` in `auth_mfa_audit` with the `--operator` name as its actor (the native command records `operator`) and queues the security email, which the Cron Trigger above sends within a minute. The script reads the audit row back and exits 0; an unknown email exits 1 without changing anything. The person then signs in with an email code and, where MFA is required, creates an allowed passkey. Release bundles include the script.

---

## 8. Static Web Assets & UI Routing (CF7)

When serving the web UI directly from Workers, configure the `assets` block:

```jsonc
"assets": {
  "directory": "build-assets",
  "binding": "ASSETS",
  "html_handling": "none",
  "not_found_handling": "none",
  "run_worker_first": ["/*", "!/assets/*", "!/manifest.webmanifest"]
}
```

- **`run_worker_first` Routing:** Every route except `/assets/*` and `/manifest.webmanifest` passes through the api Worker first. The Worker routes API requests (`/v1/*`, `/api/*`, `/mcp`, etc.) to its Rust handlers and serves `index.html` (`no-store`) from the `ASSETS` binding for SPA navigation paths.
- **Hashed Assets:** Immutable static bundles under `/assets/*` skip Worker execution entirely and are served directly by Cloudflare edge caching with 1-year immutable cache headers and Content Security Policy (CSP) defined in a generated `_headers` file.
- **Isolation:** Because HTML routing is handled by the Worker, the user-content hostname never serves the application UI.

---

## 9. Binary Evidence Uploads & User-Content Origin (CF7)

Binary evidence uploads and user attachments are hosted on an isolated origin (D49–D51):

1. **Activation:** Evidence uploads are enabled **only** when both:
   - `RUNBOOK_USERCONTENT_ORIGIN` is configured with an origin distinct from `ORIGIN` (different hostname or port).
   - `RUNBOOK_UPLOAD_SIGNING_KEY` is provided (32 random bytes in base64).
   If either is missing, evidence upload capabilities remain disabled (`capabilities.uploads = false`).
2. **Dedicated `/u/*` Listener:** The api Worker listens on `RUNBOOK_USERCONTENT_ORIGIN` serving only `/u/*` routes:
   - Signed `PUT` requests verify HMAC expiration and upload binary evidence into R2 `USER_CONTENT`.
   - Signed `GET` and `HEAD` requests stream stored evidence in 1 MiB chunks with restrictive sandbox headers (`Content-Security-Policy: default-src 'none'`, `sandbox`, `X-Content-Type-Options: nosniff`).
   - All other routes on the user-content hostname return `404 Not Found`. Cookies are never read or set.
3. **Sizes and counts.** Discovery advertises the self-hosted Cloudflare limits (`runbook_core::uploads::limits::cloudflare`, CF12-5): files up to 10 MB, but no per-run file counts and no storage quota — it is your own account. The file size stays because the signed `PUT` is read whole into the isolate, which has 128 MB on every plan (CF12).
4. **Format Support & Image Restrictions:**
   - Text, JSON, CSV, Markdown, and PDF uploads are supported.
   - **Images are currently refused on Workers** (`upload.type_unsupported`). Because Workers currently lack an image re-encoding pipeline to sanitize EXIF and metadata, image MIME types are omitted from upload limits.

---

## 10. Portable Project Archive Import & Export (CF8)

The Cloudflare edition supports the D3 portable project archive format (`runbook_core::portable`), allowing complete projects to move between native PostgreSQL and Cloudflare Workers:

### Operator Secret & Security

The import and export endpoints are protected by `RUNBOOK_OPERATOR_SECRET` (32+ characters):
- If `RUNBOOK_OPERATOR_SECRET` is unset, the routes do not exist and return `404 Not Found`.
- When set, requests must provide the secret in the `x-runbook-operator-secret` header.
- The secret is verified using constant-time digest comparison. Failed authentication is rate-limited to 5 failures per minute per isolate before returning `429 Too Many Requests`.

### Exporting a Project

Export downloads a deterministic ZIP archive containing `manifest.json`, NDJSON entity tables, and raw blobs:

```bash
curl -f -X GET "https://api.runbooks.example.com/internal/portable/export?org=<org_id>&project=<project_id_or_slug>" \
  -H "x-runbook-operator-secret: <RUNBOOK_OPERATOR_SECRET>" \
  -o project-export.zip
```

### Importing a Project

Import uploads an archive into an organization (optionally into an existing organization via `?into_existing_org=true`):

```bash
curl -f -X POST "https://api.runbooks.example.com/internal/portable/import?into_existing_org=true" \
  -H "x-runbook-operator-secret: <RUNBOOK_OPERATOR_SECRET>" \
  -H "Content-Type: application/zip" \
  --data-binary @project-export.zip
```

- **Atomic Commit Ordering:** D1, Durable Objects, and R2 do not share a distributed transaction. Import achieves atomicity by:
  1. Validating the archive schema, digests, and uniqueness constraints upfront.
  2. Installing runs into empty ExecutionDOs via the DO `Install` protocol and writing blobs to R2 under the project's prefix.
  3. Committing all D1 project rows in a single atomic batch.
  4. If the D1 batch fails or conflicts, installed Durable Objects and uploaded blobs are immediately purged.
- **Immediate Execution:** Imported runs resume immediately—pending notifications, due timers, and lease alarms are re-armed and scheduled upon commit.
- **Limits (CF12, Workers Paid).** The import is one request: the whole archive in memory, then one object request per run, one R2 write per blob or runbook source, and a constant number of D1 queries (at most 11: every write is a batch, which D1 counts as one query). So it is bounded three ways, and every refusal comes before anything is written:
  - **Size:** at most **10 MiB** of archive, as sent (`413 request.body_too_large`: decided from `Content-Length` before the body is read, or, for a body sent without one, as soon as it passes the cap) and as its files once inflated (`413 import.too_large`). The isolate has 128 MB on every plan; measured locally, an archive of mostly step logs takes about six times its size in wasm memory, one of mostly blobs about three times: about 84 MB at 10 MiB, counting the JavaScript side.
  - **Subrequests:** its worst case (a failed commit undoes everything it wrote) is two subrequests per run, blob and runbook source, plus the 11 D1 queries. It must fit 9,900, the Paid plan's 10,000 per invocation less 100 for the route's own calls: at most 4,944 runs, blobs and sources together (`413 import.too_large`). The figure is compiled in: a Worker cannot read its own `limits.subrequests`, so raising that setting does not raise the import's budget without a code change.
  - **D1 statements:** every row, guard and undo is a statement, and D1 holds each batch to its 30-second query limit, so no batch may hold more than **10,000** statements (`413 import.too_large`). Each run is one statement plus one per step, assignment, admission and start replay; each tombstone, memory revision and catalog row is one. The test fixture's runs take five statements each, so about 2,000 such runs fit; runs with many steps fit fewer.
  - **CPU:** the Paid default of 30 s per request is ample for a 10 MiB archive. `limits.cpu_ms` in the Worker's config raises it (to 5 minutes) if an import ever needs more.

  The 201 answer reports `subrequests` and `subrequest_budget`, `statements` and `statement_budget` (this archive's worst case and what it was allowed), and the commit's `bound_values` and `bound_bytes`. One import runs at a time in an isolate; a second one meanwhile is `409 import.busy` (two would share the 128 MB). The import refreshes its hold as it makes progress (each chunk of the archive as it arrives, each stage, each subrequest), and a hold with no progress for 15 minutes no longer refuses: if Cloudflare abandons an import whose client went away without ending it, the next import after that takes the isolate over instead of being refused until the isolate is evicted, while an import that is still working, however long, is never taken over. If D1's reply to the commit is lost, the import reads back whether it landed before undoing anything (including the directory's record of a project it created, so an import with no runs and no variables reads back as landed too); when even that read fails it answers `503 storage.unavailable` and leaves its held runs to their hourly expiry, which releases a committed run and purges an orphan.

  A larger project does not fit one request. Import it into the container edition instead; a chunked import with its own commit protocol is not built.
- **Export limits (CF12).** The export reads the project's D1 rows in a constant 4 queries (project-wide, not per run or memory entry), lists its R2 objects once, makes one object request per run and reads each listed R2 object at most once. That worst case is known before any run is read; past 9,900 subrequests the export answers `413 export.too_large` and reads nothing more. Anything an import admits, the export emits: at the import's item cap its worst case is about half the budget. The export holds the archive in memory too: a project that has grown past the import's bounds since (many uploads, say) may still fail to export on Workers (a read: nothing changes); a streaming export is not built.

---

## 11. Local Multi-Worker Development Stack

For local development matching the multi-Worker architecture, run:

```bash
npm run dev
```

`scripts/dev-stack.mjs` orchestrates three local Workers under `wrangler dev`:
1. **API Worker** (`wrangler.dev.jsonc`): Runs the core Rust Workers wasm engine, `assets` binding, and ExecutionDO / OrgDO Durable Objects on port `8787`.
2. **Auth Worker** (`wrangler.auth.jsonc`): Runs Better Auth on the shared local D1 database.
3. **Mail Stub** (`wrangler.mail.jsonc`): Intercepts email verification codes and prints them to the terminal console without network transmission.

### Routing & Origins
- **App Origin:** `http://127.0.0.1:8787` (serves UI assets and API routes).
- **User-Content Origin:** `http://localhost:8787` (serves evidence uploads via Host header routing to the same isolate).
- Secrets and `.dev.vars` configurations are automatically generated with 0600 permissions in `.dev/` state directories.

---

## 12. Testing & Verification

### Local workerd Integration Tests

Execute the Miniflare/workerd test suite:

```bash
# Build wasm and run integration tests:
npm test

# Run tests without rebuilding wasm:
npm run test:only
```

### Test-Only Seams (gate fix B1)

These variables exist only in the test build (`node scripts/build-worker.mjs worker-test`, `--features test-seams`, output `build-test/`), which the workerd tests, the local dev stack and the contract suite run. The deploy build (`node scripts/build-worker.mjs worker`, output `build/`) never reads them and answers every request with 503 `config.test_seam_set` (and skips its Cron and queue work) while any of them is set:

| Variable | What it does in the test build |
| --- | --- |
| `RUNBOOK_TEST_RETENTION_CLOCK_OFFSET_MS` | Runs the archive and retention pass ahead of the clock (deletes data early). |
| `RUNBOOK_TEST_HOLD_WORK` | Holds every run's background work (CF8's byte-identical export test). |
| `WEBHOOKS_ALLOW_PRIVATE` | Allows private and plain-http webhook targets (a local receiver). |
| `SSE_KEEPALIVE_MS`, `SSE_LIFETIME_MS` | The stream keep-alive (and so its D32 re-check) and lifetime; the deploy build keeps native's 30 s and 1 h. |
| `CF1_FAULT_INJECTION` | The CF1/CF2 spike routes and fault injection on the ExecutionDO. |
| `RUNBOOK_TEST_RELEASE` | The release `/v1/meta` reports (UPD1's local update test runs one build as two releases). |

### End-to-End Browser Testing (`npm run e2e:workers`)

To test the complete web UI and Workers stack with Playwright:

```bash
cd ../web
npm run e2e:workers
```

This command:
1. Builds the web UI, the api Worker, and the auth Worker.
2. Launches the local multi-Worker dev stack in `web/e2e/.run/workers`.
3. Bootstraps the test owner account via `/internal/bootstrap`.
4. Seeds authentication tokens via the local D1 psql shim (`cloudflare/scripts/d1-psql.mjs`).
5. Executes the flagship and sign-in browser test suites under strict CSP backstop assertions.

To see what grows in D1 for a realistic project, run `node scripts/d1-sizes.mjs`. It prints per-table row counts and stored bytes (CF11, §15).

---

## 13. Webhook Delivery Log in WebhookDOs (CF11)

Each webhook subscription has one `WebhookDO`, named by the subscription ID. It holds:

- that subscription's delivery log (deliveries and attempts, with the Postgres columns exactly);
- its retry and backoff state;
- the redelivery cursor;
- a copy of each event it delivers.

Subscriptions, the tenant's event records and replay rows stay in D1. Subscriptions are small, listed per organization, and admin-managed.

- **Fan-out.** The queue consumer and the project events pass write each delivery to its subscription's object. A delivery's ID is derived from the event and the subscription, so fan-out is idempotent.
- **Reads.** `GET …/webhooks/{id}/deliveries` and redelivery read and act on that object.
- **Retries.** The object's alarm attempts its own due deliveries.
- **Retention follows native.** Finished deliveries and attempts older than 90 days are dropped by the daily Cron. Pending deliveries are never dropped, and a human step's notified verdict is kept.
- **Deletion.** A deleted subscription purges its object, and a deleted run forgets its deliveries.

---

## 14. D1 Shards (CF11)

D1 databases are small: 10 GB each on Workers Paid. So every D1 access goes through one resolver, `D1Shards` (`crates/runbook-worker/src/shards.rs`), which answers two kinds of question:

- **`for_org(org)`** returns the database that holds that organization's data: catalog, runs' rows, inbox, uploads, memory, archives, webhook subscriptions and events, quotas, notices.
- **`control()`** returns the database for what is not org-scoped, or is looked up before the organization is known:
  - the directory: organizations, users and identities, projects, teams, members, verified emails, project variables, directory requests, invitations, device codes, auth links;
  - personal and service credentials and execution-token routes, which are found by a secret's hash;
  - the bootstrap marker;
  - the auth Worker's tables.

**Today there is one database.** `DB` is both the control database and every organization's shard. The mapping is the `RUNBOOK_D1_SHARDS` variable, a JSON string with every field optional; unset, everything is on `DB`:

```json
{"control": "DB", "default": "DB", "orgs": {"org_01…": "DB_2"}}
```

Each name is a D1 binding of the api Worker. Rules that follow from this:

- Every database gets the whole schema; unused tables stay empty.
- The Cron passes walk every data shard and act only on the organizations the mapping gives that shard.
- A mapping that does not parse refuses requests (`d1.shards_invalid`) rather than guess where an organization's rows are.

`tests/shards.test.mjs` runs two local bindings with one organization mapped to `DB_2`. It checks that:

- that organization's rows land on `DB_2`, and the control rows stay on `DB`;
- neither organization's IDs answer the other's requests;
- the Cron passes sweep both databases;
- a CF8 import into a non-control shard round-trips.

### Moving an Organization to a New Shard

Durable Objects and R2 are not sharded. An organization's run objects (ExecutionDOs), its webhook delivery logs (WebhookDOs), its OrgDO and its blobs stay where they are, and its control rows stay on the control database. Only its D1 shard rows move.

1. **Add the database.** Create it, add it as a D1 binding (for example `DB_2`), apply the migrations to it (§2) and deploy. Map nothing to it yet.
2. **Stop the organization's writes.** There is no per-organization write freeze yet, so use a maintenance window with the organization's clients stopped. A write between the export and the flip would be lost.
3. **Export each project** with CF8's operator route (§10), `GET /internal/portable/export?org=…&project=…`. It reads the old shard through the current mapping, and the directory from control.
4. **Import the rows into the new shard.** This needs a rows-only mode of CF8's import, which is not built yet (see the note below).
5. **Flip the mapping.** Add `"orgs": {"org_…": "DB_2"}` to `RUNBOOK_D1_SHARDS` and deploy. From then on, every read and write for the organization goes to `DB_2`.
6. **Delete the organization's rows from the old shard.** `runbook_store_d1::portable::forget_shard_rows` lists the statements for one project. The Cron passes already ignore those rows once the mapping says the organization lives elsewhere.

**What step 4 needs.** Today's CF8 import does three things a move must skip:

- it installs each run into its object, but the objects already hold the runs;
- it writes the directory and the project variables, but they are already on control;
- its control guards refuse a project whose control rows exist.

A shard move needs a rows-only mode: the shard guards and the shard batch (`ImportPlan::shard_guards`, `ImportPlan::shard`) written to a named binding before the flip, together with the R2 source puts. Those puts are idempotent, because the blobs already exist. Everything the mode needs is already in the import plan; building it is left for the package that adds the second database.

---

## 15. Runbook Source in R2 (CF11)

Runbook source lives in R2, not in D1:

- **Where.** A draft's, keyed draft's or published version's source is stored in the project's content-addressed R2 blobs, keyed by its SHA-256, beside the snapshots and skill files.
- **What D1 keeps.** The row keeps only the digest and size (`1010_runbook_source_digests.sql`).
- **Reads.** They stitch the text back after checking the digest. A missing or altered blob is an error, never a wrong source.
- **Older rows.** Rows written before 1010 keep their text inline and read as before.

CF8's export puts the text back into the archive's rows and leaves the source-only blobs out, so archives are the same as natively. Its import stores the text in R2 again.

Skill bundle files were already in R2. D1 keeps each skill version's manifest (paths, sizes, SHA-256s).

**Unreferenced draft blobs.** A draft save of changed text writes a new blob and leaves the previous one unreferenced. No sweep removes those yet.

**Replay responses (CF12b).** A request replay's response over 64 KiB (a large runbook create or skill upload) is kept under `t/{org}/{project}/replay/` in the blobs bucket, which is no part of the project: the export never carries it. It goes with its row: the next authoring write in the organization after the row expires deletes the object, then the row, and the daily cron pass (`replays_swept` in its log line, with the rows swept and the object deletes that failed) sweeps organizations with no later write, oldest first, in batches of 50 until a batch comes back short, up to 1,000 rows a day. A row whose object delete fails is kept and backs off (`sweep_after`, migration 1015): no sweep tries it again for an hour, doubling per failure up to a week, so failing rows never take the places of rows a sweep can remove. Each batch is one D1 query to read and one `batch()` (one DELETE, one UPDATE) to write; Cloudflare counts a `batch()` as one query toward an invocation's 1,000. Each replay object's name carries random bytes of the write that stored it, so two writes of one key never share an object. A key reused after its row expired replaces that row (and deletes its object) even when no sweep has reached it yet, and an object stored for a response no row ends up naming (a lost race for the key, or a failed write) is deleted at once. A response CF12 kept among the project's blobs (`shared/`) replays until its row expires and then stays there, unreferenced, like an old draft blob.

---

## 16. Workers Paid Limits (CF12)

The edition requires Workers Paid (decision 2026-11-13). Its limits, from Cloudflare's docs as read on 2026-11-13 ([Workers](https://developers.cloudflare.com/workers/platform/limits/), [D1](https://developers.cloudflare.com/d1/platform/limits/), [Queues](https://developers.cloudflare.com/queues/platform/limits/), [Durable Objects](https://developers.cloudflare.com/durable-objects/platform/limits/)), and what the edition does at each:

| Limit | Free | Paid | The edition |
| --- | --- | --- | --- |
| CPU per HTTP request | 10 ms | 30 s default, up to 5 min (`limits.cpu_ms`) | Default. The heaviest request, a 10 MiB import, fits easily. |
| Subrequests per invocation | 50 | 10,000 default, up to 10 million (`limits.subrequests`) | Default. A portable import refuses an archive whose worst case passes 9,900, and an export a project whose worst case does (§10). The figure is compiled in: raising `limits.subrequests` needs a code change too. |
| D1 queries per invocation (a batch counts as one) | 50 | 1,000 | An import makes at most 11, an export 4 (§10). |
| D1 query duration | 30 s | 30 s, for a whole batch too | An import's batches hold at most 10,000 statements each (§10). |
| D1 query result | 50 MB | 50 MB | The export's project-wide reads stay far under it for anything an import admits (10 MiB). |
| Memory per isolate | 128 MB | 128 MB | Unchanged by the plan. It bounds the import (10 MiB) and the uploaded file size (10 MB, §9). |
| Request body | set by the zone's plan: 100 MB on Free and Pro, 200 MB on Business, up to 5 GB on Enterprise | same | The api Worker reads at most **2 MiB + 64 KiB** on `/v1` and `/mcp` (native's JSON cap, `runbook_api::http::JSON_BODY_BYTES`), so a runbook source at the compiler's 1 MiB ceiling saves; it was 1 MiB. Skill uploads share that cap here (native: `RUNBOOK_MAX_REQUEST_BYTES`, 64 MiB by default). The sign-in proxy (`/api/auth/*`) reads at most 64 KiB, as natively. A larger declared body is `413 request.body_too_large` before it is read; a body sent without a length is read with a running count and refused as soon as it passes the cap. |
| D1 row, string or BLOB | 2 MB | 2 MB | A request replay row keeps a payload over 64 KiB as its SHA-256 and a response over 64 KiB in R2 (under the project's `replay/` prefix since CF12b), so a 2 MiB request and its 2 MiB response still replay from a small row. CF12b: every other request value a store keeps whole in one D1 or Durable Object row has a cap of its own, the same on both editions (`runbook_api::field_caps`, `422 request.value_too_large`): 64 KiB of free text (a summary, a comment or reason), 1 KiB for a name (a project, team or user, an invited email, an agent's session, name and model), 256 KiB of run data as JSON (a start's data, a signal, a patch, an override), 1 MiB of logs and 100 evidence links (8 KiB each, 200 KiB together) on one report, 200 files in a skill. A log line longer than 64 KiB is stored as several entries rather than refused, and a start whose record, with the version's defaults merged in, would pass one row is refused. The native test D1 and Durable Object stand-ins enforce the limit too. The full list is in the design notes, "CF12b: request limits". |
| Worker size | 64 MiB uncompressed; no compressed limit | same | `tests/size.test.mjs` checks the 64 MiB and keeps the plan's 4 MB gzip budget for cold starts (about 2.9 MB now). |
| Cron Triggers per account | 5 | 250 | Two. |
| CPU per Cron Trigger | 10 ms | 30 s under an hourly interval, 15 min at an hour or more | The minute and daily passes are bounded per page (as natively), not per invocation; nothing yet keeps one pass under 10,000 subrequests or 1,000 D1 queries with very many tenants. |
| Queue consumer batch | 100 messages max | same | `max_batch_size` 10, `max_batch_timeout` 1 s: unchanged, since Queues limits are the same on both plans. |
| Durable Object storage | 1 GB an object, 5 GB an account | 10 GB an object, unlimited an account | Nothing in the code depended on the Free values. |
| D1 database size | 500 MB | 10 GB | CF11's shards (§14). |

