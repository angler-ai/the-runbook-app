# Cloudflare Edition Database Schema Mapping (CF3a)

This document specifies the storage split and schema mapping from the self-hosted PostgreSQL database (`crates/runbook-store-postgres`) to the Cloudflare edition storage services (D1, ExecutionDO SQLite, and R2), following decisions D56 and D57 in the design notes.

## 1. Storage Split Architecture

Per **D56** and **D57**:
- **D1 (Relational Database):** Holds global and tenant-level entities: directory, organization/project metadata, credentials, catalog drafts and published versions, run summary rows, task assignments (inbox), concurrency admission pools and queues, webhooks, memory entries with FTS5 search, authentication links, daily archive indexes, and tombstones.
- **ExecutionDO (Durable Object SQLite):** (0004, gate fix D2, adds `import_hold`: a run a portable import installed stays held until the import's D1 commit.) Holds execution-local state in a dedicated SQLite database per execution instance (`cloudflare/migrations/do/0001_execution.sql` from CF2, authoritative): the execution projection head (`meta`), event stream (`event`), step runs (`step_run`), timers (`timer`), timer history (`timer_history`), outbox (`outbox`), derived outbox (`derived_outbox`), claims (`claim`), claim logs (`claim_log`), start record (`start_record`), work leases (`work_lease`), execution credentials (`credential_family`, `execution_credential`), actor replay buffers (`actor_replay`), and outbox retry backoff (`outbox_retry`). Every run-owned row carries `org_id`, `project_id`, and `execution_id` (PRD §14 Rules).
- **WebhookDO (CF11, Durable Object SQLite):** One object per webhook subscription holds its delivery log: `webhook_delivery` and `webhook_attempt` (the Postgres columns exactly, checked by the parity test), a copy of each event it delivers (`delivery_event`, with the human flag the retention rule needs) and its owner. Subscriptions, event records (`webhook_event`) and replay rows stay in D1. The object's alarm attempts its due deliveries; retention follows the native 90-day rule.
- **R2 (Object Storage):** Holds large binary data: blob chunks, content-addressed snapshots, upload evidence files, skill files, and execution archives. D1 never stores byte payloads, only metadata references.

---

## 2. Type Mapping Rules (Postgres → SQLite / D1 / DO)

| Postgres Type | SQLite / D1 / DO Type | Notes |
|---|---|---|
| `text`, `varchar` | `TEXT` | UTF-8 encoded text. |
| `jsonb` | `TEXT` | Serialized JSON. Avoids key reordering and normalization. |
| `bytea` | `BLOB` | Raw binary bytes. `octet_length(b) = 32` maps to `length(b) = 32`. |
| `bigint` (epoch ms) | `INTEGER` | Stored as millisecond timestamp integer. |
| `timestamptz` | `INTEGER` | Epoch milliseconds integer. |
| `integer`, `smallint` | `INTEGER` | 64-bit signed integer affinity. |
| `boolean` | `INTEGER` | SQLite representation (0 = false, 1 = true). |
| `text[]`, `ARRAY` | `TEXT` | JSON-encoded array string. |
| `tsvector` + GIN | `FTS5` | Column declared as `FTS5`, backed by FTS5 virtual table with `AFTER INSERT/UPDATE/DELETE` triggers. |
| `GENERATED ALWAYS AS IDENTITY`, `SEQUENCE` | `INTEGER` | Counter table or `INTEGER PRIMARY KEY AUTOINCREMENT`. |

Additional schema conventions:
- **No schema qualifiers:** The `runbook.` schema prefix is removed.
- **Foreign keys:** Maintained in D1; runtime requires `PRAGMA foreign_keys = ON;`.
- **Partial indexes:** Preserved using SQLite's `WHERE` clause support on indexes.
- **No ALTER TABLE DROP CONSTRAINT:** SQLite lacks constraint alteration; migrations use table recreation when constraints change.
- **Run-owned scoping:** Every table owned by an execution in ExecutionDO includes `org_id`, `project_id`, and `execution_id` to allow exports, archives, and deletes as an atomic unit (PRD §14).

---

## 3. Table Mapping (Postgres → Target)

Total tables in PostgreSQL schema: **65** (47 in D1, 14 in ExecutionDO, 2 in WebhookDO, 2 in R2; CF6's OrgDO tables are listed in the manifest as `orgdo`).
No table is duplicated between D1 and ExecutionDO.

| PostgreSQL Table | Migration | Target | Target Table Name | Notes / Placement Rationale |
|---|---|---|---|---|
| `schema_migration` | 0001 | D1 | `schema_migration` | Migration history and checksums. |
| `execution_owner` | 0002 | D1 | `execution_owner` | Directory-level execution tenant ownership anchor. |
| `execution` | 0002 | ExecutionDO | `meta` | Execution head and projection row inside the execution's Durable Object. |
| `execution_event` | 0002 | ExecutionDO | `event` | Chronological event stream for execution replay. |
| `execution_timer` | 0002 | ExecutionDO | `timer` | Active due timers for DO alarm scheduling. |
| `execution_timer_history` | 0002 | ExecutionDO | `timer_history` | Monotonic generation history per timer key. |
| `execution_outbox` | 0002 | ExecutionDO | `outbox` | Event notification outbox dispatched from DO. |
| `execution_idempotency` | 0002 | ExecutionDO | `idempotency` | Run-scoped idempotency keys. |
| `execution_claim` | 0002, 0017 | ExecutionDO | `claim` | Step run execution leases held by actors. |
| `execution_derived_outbox` | 0002 | ExecutionDO | `derived_outbox` | Summary projection updates queued for D1 sync. |
| `draft` | 0002 | D1 | `draft` | Project-scoped authoring drafts. CF11 (1010): the source is in the project's R2 blobs by SHA-256; D1 keeps `source_digest` and `source_size` (D1-only columns). |
| `run_summary` | 0002, 0012 | D1 | `run_summary` | Run list and search summary rows (with `run_summary_fts`). |
| `assignment` | 0002, 0025 | D1 | `assignment` | User/team inbox tasks and approval requests. |
| `admission_pool` | 0002, 0014 | D1 | `admission_pool` | Concurrency pools per project and runbook key. |
| `admission` | 0002, 0014 | D1 | `admission` | Concurrency admission state and queue positions. |
| `start_idempotency` | 0002 | D1 | `start_idempotency` | Start request idempotency keys per project. |
| `organization` | 0003 | D1 | `organization` | Directory organization root. |
| `directory_revision` | 0003 | D1 | `directory_revision` | Organization revision counter for atomic directory updates. |
| `user_identity` | 0003 | D1 | `user_identity` | Global user identities. |
| `org_user` | 0003, 0006 | D1 | `org_user` | Organization membership and roles (`owner`, `admin`, `member`). |
| `project` | 0003, 0004 | D1 | `project` | Projects within organizations. |
| `team` | 0003, 0004 | D1 | `team` | Teams within organizations. |
| `project_member` | 0003, 0004 | D1 | `project_member` | Project role assignments. |
| `team_member` | 0003, 0004 | D1 | `team_member` | Team memberships. |
| `verified_email` | 0003 | D1 | `verified_email` | Verified email address lookups. |
| `work_lease` | 0005, 0014, 0017 | ExecutionDO | `work_lease` | Lease fencing for timers, notifications, claims, and activations. |
| `blob` | 0007, 0029 | R2 | *(R2 bucket)* | Content-addressed snapshots and blob manifests. Not ported to D1. |
| `blob_chunk` | 0007 | R2 | *(R2 bucket)* | Raw blob byte chunks. Not ported to D1. |
| `tombstone` | 0007 | D1 | `tombstone` | Deleted execution IDs preventing resurrection. |
| `credential` | 0008 | D1 | `credential` | Organization API credentials (SHA-256 hashes only). |
| `runbook` | 0009, 0015 | D1 | `runbook` | Runbook catalog items and active draft source (BLOB). CF11 (1010): the draft's text is in the project's R2 blobs by SHA-256; D1 keeps `draft_digest` and `draft_size` (D1-only columns). |
| `runbook_version` | 0009 | D1 | `runbook_version` | Immutable published runbook versions and digests. CF11 (1010): the source is in the project's R2 blobs beside the snapshot; D1 keeps `source_digest` and `source_size` (D1-only columns). |
| `skill_version` | 0010 | D1 | `skill_version` | Immutable published skill manifests and hashes. |
| `instance_bootstrap` | 0011 | D1 | `instance_bootstrap` | Singleton marker for initial instance setup. |
| `project_variables` | 0013 | D1 | `project_variables` | Project environment variables and secret references. |
| `directory_request` | 0013 | D1 | `directory_request` | Idempotency log for directory mutations. |
| `execution_start` | 0014 | ExecutionDO | `start_record` | Pinned start facts and activation time in execution's DO. |
| `start_replay` | 0014, 0018 | D1 | `start_replay` | Replay cache for start requests (24h retention). |
| `owner_recovery_audit` | 0016 | D1 | `owner_recovery_audit` | Audit log for CLI-based organization owner recovery. |
| `claim_log` | 0017 | ExecutionDO | `claim_log` | Heartbeat and log entries for leased step claims. |
| `credential_family` | 0019 | ExecutionDO | `credential_family` | Run-scoped credential families. |
| `execution_credential` | 0019 | ExecutionDO | `execution_credential` | Run-scoped execution and refresh tokens. |
| `step_run_index` | 0020 | D1 | `step_run_index` | Index mapping `step_run_id` to `execution_id` for actor routing. |
| `actor_replay` | 0020 | ExecutionDO | `actor_replay` | Bounded replay cache for actor HTTP calls. |
| `webhook_subscription` | 0021 | D1 | `webhook_subscription` | Webhook endpoints and sealed signing secrets. |
| `webhook_event` | 0021 | D1 | `webhook_event` | Recorded webhook event occurrences. |
| `webhook_delivery` | 0021, 0022 | WebhookDO | `webhook_delivery` | CF11 (the user's decision, superseding D56): each subscription's deliveries and retry state, in its own WebhookDO (`migrations/webhookdo/0001_delivery_log.sql`); same columns. D1 dropped it in 1009. |
| `webhook_attempt` | 0021, 0022 | WebhookDO | `webhook_attempt` | CF11: HTTP attempt logs, beside their deliveries in the subscription's WebhookDO; same columns. D1 dropped it in 1009. |
| `webhook_replay` | 0021 | D1 | `webhook_replay` | Ingress idempotency cache for webhook POSTs. |
| `project_event_outbox` | 0022 | D1 | `project_event_outbox` | Project outbox for runbook publication webhooks. |
| `step_run_tombstone` | 0024 | D1 | `step_run_tombstone` | Step run tombstones for 410 responses after run purge. |
| `memory_entry` | 0026 | D1 | `memory_entry` | Curated memory entries (with `memory_entry_fts`). |
| `memory_revision` | 0026 | D1 | `memory_revision` | Historical revisions of curated memory entries. |
| `auth_link` | 0027 | D1 | `auth_link` | Links between Better Auth user IDs and directory users. |
| `invitation` | 0027 | D1 | `invitation` | Pending and accepted organization invitations. |
| `device_code` | 0027 | D1 | `device_code` | RFC 8628 OAuth device authorization codes. |
| `evidence` | 0028 | D1 | `evidence` | Upload evidence slots and status (bytes live in R2). |
| `archive` | 0029 | D1 | `archive` | Daily archive file index and stale marker. |
| `archive_run` | 0029 | D1 | `archive_run` | Archived run summaries surviving live detail purge. |
| `archive_attempt` | 0029 | D1 | `archive_attempt` | Daily archive upload retry backoff state. |
| `archive_member` | 0029 | D1 | `archive_member` | Archive run membership index for stale marks. |
| `retained_evidence` | 0029 | D1 | `retained_evidence` | Retained uploads following archived runs. |

---

## 4. Reserved Migrations Note

- **0029 (`0029_archives_retention.sql`):** Landed on main with ARC2 (daily archive index and retained summary/evidence rows).
- **0030 (`0030_quotas_usage.sql`):** Reserved for CF6 (organization quotas and usage metering).

---

## 5. D1-only migrations (CF3)

Versions from 1001 have no Postgres counterpart. The runner
(`crates/runbook-store-d1/src/migrate.rs`) keys history by version, so
later Postgres-numbered files still apply in order.

| Migration | Adds or changes | Why |
|---|---|---|
| `1001_d1_guard.sql` | `d1_guard` and its trigger | Guarded batches: D1 has no interactive transactions (CF3-2). |
| `1002_execution_token_route.sql` | `execution_token_route` | Execution-token hash → run, so the api Worker routes a bearer to its ExecutionDO (CF2-9). |
| `1003_memory_fts_words.sql` | `memory_entry_fts` over `search_tsv` | 0026 indexed the row's JSON, where escapes split words (CF3-3). |
| `1004_run_search_words_and_admission_notice.sql` | `run_summary_fts` over `search_tsv`; `admission_notice` | The same fix for run search; notices for waiters promoted in D1 (CF3-6). |
| `1005_admission_pool_revision.sql` … `1008_supersede_notice.sql` | pool revisions, `object_purge`, `archive_version`, `supersede_notice` | CF3 fix 3, CF4/CF5. |
| `1009_webhook_log_to_objects.sql` | drops `webhook_delivery`, `webhook_attempt` | The delivery log lives in each subscription's WebhookDO (CF11). |
| `1010_runbook_source_digests.sql` | digest and size columns on `runbook`, `runbook_version`, `draft` | Runbook source in R2 (CF11). |
| `1011_run_release.sql` | `run_release` | A portable import's runs, held in their objects until its D1 commit, to release after it (gate fix D2). |
| `1012_cron_backoff.sql` | `attempts` on `object_purge`, `supersede_notice` | A failed cron item backs off on its own instead of hiding the next ones (gate fix, low). |
| `1013_webhook_subscription_deleted.sql` | `webhook_subscription_deleted` | A deleted subscription's marker, written with its deletion: its WebhookDO retires only on this positive evidence (gate round 2). |
| `1014_run_release_backoff.sql` | `attempts` on `run_release` | A release the cron cannot finish backs off; the rows now commit with an import's control batch, so only a fully committed import is released (gate round 2). |
| `1015_replay_sweep_backoff.sql` | `sweep_after`, `sweep_failures` on `directory_request` | A replay row whose R2 object delete failed backs off (an hour, doubling, up to a week), so later sweeps move past it (CF12b review round 2). |

None of these tables is in the Postgres manifest, so the parity test is
unchanged; none is in the ExecutionDO schema either.
