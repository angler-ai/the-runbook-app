# Portable project archive (D3)

One project exports as a portable archive and imports into either backend
(PRD section 14, "Export and portability"). The format is independent of
the database: the memory adapters and PostgreSQL produce byte-identical
archives for the same content. Code: `runbook_core::portable` (format,
validation, directory slice, memory adapters),
`runbook_store_postgres::{export_project, import_project}` and the
`runbook-server export|import` admin commands.

## Layout

```
manifest.json
organization.ndjson          1 row: OrganizationRecord
project.ndjson               1 row: ProjectRecord
users.ndjson                 UserRecord (id, name, active, org role)
teams.ndjson                 TeamRecord
project_members.ndjson       {user_id, role}
team_members.ndjson          {team_id, user_id}
project_variables.ndjson     0 or 1 VariablesRecord {revision, variables}
runbooks.ndjson              {record, draft_revision, draft_source}
runbook_versions.ndjson      {runbook_id, version: VersionRecord}
drafts.ndjson                keyed DraftRevision rows
skill_versions.ndjson        SkillVersion, in recording order
admission_pools.ndjson       {key, limit, active, queue: [execution IDs, FIFO]}
executions.ndjson            run unit (below), events removed
execution_events.ndjson      {execution_id, event}, by run then sequence
tombstones.ndjson            Tombstone of each deleted run
webhook_subscriptions.ndjson subscription settings without secrets
blobs.ndjson                 {key: BlobKey, sha256, size}
memory_entries.ndjson        {entry: current MemoryEntryRecord, proposal_sequence}
memory_revisions.ndjson      MemoryRevisionRecord, by entry ID then revision
blobs/<sha256>               blob bytes, one file per distinct content
```

Every table file is present, even when empty, in this order. Each NDJSON
line is the compact serde JSON of one row, ending in `\n`.

`manifest.json` (pretty JSON):

```json
{
  "format": "runbook-project-archive",
  "schema_version": 2,
  "org_id": "org_…",
  "project_id": "prj_…",
  "created_at_ms": 1800000000000,
  "files": [
    {"path": "organization.ndjson", "records": 1, "bytes": 61, "sha256": "…"},
    {"path": "blobs/3f…", "bytes": 1024, "sha256": "3f…"}
  ]
}
```

### Schema versions

Version 2 (D42) adds the two memory tables after `blobs.ndjson`: every
memory entry of the project (proposed, active and archived) with every
revision, and, for an agent proposal, the execution sequence that recorded
it, so a redelivered D40 derived row stays a no-op after import. Version 1
archives (the first 17 tables, no memory) still decode and import; any
other version is `archive.unsupported_schema_version`. Exports always
write the current version, so a version 1 archive re-exports as version 2
with empty memory tables and otherwise identical table files.
`crates/runbook-store-conformance/tests/fixtures/archive-v1` is an archive
the version 1 encoder wrote; both adapters must keep importing it.

Memory rows are checked like the others: each revision carries the
archive's scope, revisions are numbered 1..n with a fixed identity
(scope, source, creation time) and a first revision that created or
proposed the entry, the entries line equals the last revision
(`archive.invalid_memory`), revisions without an entry are
`archive.orphan_memory_revisions`, a runbook entry must name an exported
runbook, and memory IDs are unique and must not exist on the target.
Bodies are stored as written: they were redacted when saved.

`created_at_ms` is the host clock when the archive was written; it is the
only field not derived from the project's content. Files list the tables in
layout order, then blob files sorted by digest; blob entries have no
`records`.

### Runs as units

A run line reuses the B2.4 run slice (`retention::ExecutionSlice`): scope,
the stored execution (head, projection, timers, timer history, outbox,
idempotency, claims, derived outbox), actor replay records, its catalog
slice (summary, assignments, admission), work leases and execution blob
keys. The archive adds the start record, the start replay rows (with their
creation time) and claim log lines. The slice's events move to
`execution_events.ndjson`, one per line. Open runs export too (the B2.4
retention export still refuses them); the daily archive is the per-day
form of the same run unit.

## Determinism

Rows are sorted canonically before encoding (IDs, keys, digests); order that
carries meaning is kept: skill recording order (latest wins), assignment
order and pool queues. Equal content and `created_at_ms` always encode to
equal bytes. Decoding rejects an archive whose rows are not in canonical
order, so a decoded archive always re-encodes to itself.

## Trust model

An archive is untrusted input. Its digests prove only that the files match
the manifest: anyone can edit a file and recompute its digest. Tenant
isolation therefore never relies on the digests. Every import (memory and
PostgreSQL, through `decode` or with a `ProjectData` handed in directly)
runs `portable::validate` before it touches a store, and the adapters then
refuse anything that would reach outside the imported project:

- every scoped row carries the archive's scope, and every ID embedded in a
  run unit names that run: the run summary, each assignment, the admission
  request and row, work leases, actor replay records, start replay
  outcomes, timers, outbox and idempotency rows, claims, derived updates,
  step projections and execution blob keys (`archive.foreign_id`);
- an execution ID (run or tombstone) that already exists anywhere on the
  target, in a run, a tombstone or a webhook event, is `import.id_conflict`;
  so is any other existing ID (runbook, version, step run, team, user,
  subscription, blob), because PostgreSQL writes every import row with a
  plain `INSERT`, never an upsert;
- keyed rows are unique within the archive (`archive.duplicate_row`), and
  pools agree with their admissions: each queued admission's key matches
  its pool and `active` equals the pool's admitted runs
  (`archive.invalid_pool`);
- the memory import refuses an archive whose scope differs from the store
  handle's bound project (`import.scope_mismatch`).

## Integrity on import

Before any target is touched, decoding checks: the manifest's format and
schema version; the exact file set (no missing or unlisted files, blob
names equal to their digests); every file's size and SHA-256; NDJSON record
counts; one organization and one project matching the manifest scope; the
scope on every scoped row (run slices, work leases, tombstones); event
sequences contiguous from 1 to each run's revision with no orphan events;
references (versions to runbooks, memberships to exported users and
teams, queued IDs and execution blobs to runs, blob index to blob files);
canonical order; then everything `validate` checks (above). Truncated or
tampered files fail as `archive.size_mismatch`
or `archive.digest_mismatch` naming the file.

Digests give integrity, not authenticity: someone who edits a file and its
manifest entry consistently produces a different, valid archive. Signing is
deferred (PRD "signing" is a later plan).

## What is and is not exported

Exported: the project and its organization record; the directory slice
(project members, plus organization owners and admins who may act in every
project; the teams that the exported versions, runs or blobs mention by
ID, such as approvers, escalation targets and compiled snapshots; and those
teams' memberships among the exported users); project variables; runbooks with their draft, every
version (published and preview) and the compiled snapshots they reference;
keyed drafts; skill versions and their files; concurrency pools; every run
as a unit; tombstones; every blob of the project (shared snapshots and
skill files by content, execution blobs by run); webhook subscription
settings; runbook memory (D42).

Never exported:

- **Secrets and hashes.** No PAT or service-token row (organization-level
  and hash-only), no execution credential family or token row (their
  hashes, and the live agent sessions they belong to, stay on the source),
  no webhook signing secret or previous secret (D36). Token metadata travels
  only as the run's audit events (`ExecutionCredentialsIssued` names the
  family ID, generation and issuer). After import, agents obtain new
  execution credentials through the normal issuance path; claims are kept.
- **Verified-email bindings.** Sign-in identity facts belong to the
  instance; users bind again by signing in.
- **Webhook delivery state.** Recorded events, deliveries, attempts, webhook
  replay rows and the project event outbox are operational logs with a
  90-day purge.
- **Legacy admission idempotency** (`start_idempotency`, the B2 `admit`
  API). Start replays supersede it and are exported.
- **Users outside the slice.** A run's history may name a user who is not
  exported (for example a team member outside the project who was notified
  of an approval); those IDs are references only, as they were on the
  source.

## Import rules

- **Empty target only.** Import refuses a project that holds any row:
  runbooks, versions, drafts, skills, pools, admissions, runs, work leases,
  start records or replays, credentials, step index, tombstones, blobs,
  variables, webhook subscriptions, events, deliveries or replays, memory entries, or
  project outbox rows (`import.target_not_empty`). There is no `--force`.
- **IDs preserved.** Every ID keeps its value. An ID already taken by
  another project (runbook, run, step run, subscription, project in another
  organization) fails as `import.id_conflict` or `import.project_conflict`.
- **New organization by default.** An archive whose organization already
  exists on the target is refused (`import.org_exists`). The organization is
  created with its archived ID, title, users (with their archived roles),
  teams, project, project roles and team memberships. A user ID that
  already has an identity anywhere on the instance is refused
  (`import.user_exists`): an archive never links itself to someone else's
  identity.
- **Existing organization, by explicit opt-in** (`ImportOptions {
  into_existing_org: true }`, `runbook-server import --into-existing-org`).
  Nothing that exists changes: the organization keeps its title; a user who
  is already in the organization keeps their role, identity and team
  memberships; a user ID known elsewhere on the instance but not in this
  organization is refused (`import.user_exists`); new users are created as
  members, never owners or admins; only teams that do not exist are
  created, and only they receive members; project roles are added for
  users without one and never changed. An existing project record must
  match exactly (`import.project_mismatch`) and be empty. All directory
  rows are plain inserts under the organization's locked revision row.
- **All or nothing.** PostgreSQL imports in one transaction (rolled back on
  any failure). The memory adapters stage a copy of each store and install
  all of them only after every stage succeeded.
- **Stop the server while importing.** The PostgreSQL import holds an
  advisory lock on the project ID and, when the project row already
  exists, locks it `FOR UPDATE` before checking that the project is empty,
  so writers that reference the project by foreign key (memberships,
  variables) wait for the import. Catalog and run tables do not reference
  the project row, so a live server could still write into the project
  between the emptiness check and the commit. Import into a stopped server
  (or one no client can reach).
- **Queue order.** Queued admissions take fresh queue positions in the
  archived FIFO order; admitted and released admissions keep their times.
- **Webhooks.** An imported subscription keeps its settings (including
  `enabled`) but stores no secret (`UNSET_SECRET`). Deliveries fail closed
  as `secret_unavailable` without counting against its health until an
  admin rotates the secret; rotation of an unset secret starts no overlap
  window.
- **Clocks.** Imported rows keep their stored times. Timers already due on
  the target's clock fire on the next drain.

## Commands

```
runbook-server export --org <org_id> --project <prj_id|slug> --out <dir>
runbook-server import --in <dir|day.zip> [--org <org_id>] [--into-existing-org]
```

Both are local administrative commands on the server binary, not HTTP
routes, and use the server's `DATABASE_URL` (and `RUNBOOK_ORIGIN`, as
every command does). Both apply pending migrations first, as `assign-owner`
does. `export` writes into an absent or empty directory
(`export.out_not_empty` otherwise): files go to a temporary sibling folder
that is renamed into place only when complete, so a failed export leaves
no partial archive. `import --in` reads regular files at the top level and
in `blobs/` only, never follows links (the entry must not be a link and
the opened handle must be that same file), and refuses a file over 1 GiB
or an archive over 4 GiB (`archive.too_large`). `import` reads and verifies the
archive before opening the database; `--org`, when given, must equal the
archive's organization (`import.org_mismatch`). Success prints one summary
line (IDs and counts) on stdout and exits 0; failure prints one error code
(with the archive file name for archive errors) on stderr and exits 1.
Neither prints secrets or connection details. The archive is a directory;
no ZIP crate is in the lockfile, so ZIP packaging is left to the operator
(or a later plan).

## Daily archives (D53, ARC1–ARC3)

A daily archive is one runbook's runs that finished on one UTC day, as a
ZIP (`GET /v1/runbooks/{rb}/archives/{date}`):

```
manifest.json                  format runbook-daily-archive, schema 1: org,
                               project, runbook, day and date, versions used,
                               run count, and per run its folder, revision,
                               summary, files (size, SHA-256) and evidence
<exe>/events.ndjson            the run's events, one per line
<exe>/execution.json           the run unit above, without events, claim
                               logs and start replays
<exe>/logs/<step_run>.ndjson   claim log lines, one JSON string per line
<exe>/report.md                the execution document as Markdown
project/…                      this format (manifest and tables) for the
                               context: directory slice, the runbook and its
                               draft, the versions the runs used and their
                               snapshots, their pools; no runs
```

Bytes are deterministic (fixed timestamps and compression). Evidence objects
are listed, not included; range downloads (`format=zip&from&to`) and
`include_evidence=true` bundle the day files with the objects still held.
`runbook-server import --in <day.zip>` (or
`archives::import::to_project_data`) checks every file against the
manifest, rebuilds each run unit from its folder and imports the result as
this format's `ProjectData`, with the same validation and target rules
(an empty project, a new organization unless `--into-existing-org`).
