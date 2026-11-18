# Contributing

## Licensing and the contributor agreement

The Runbook is owned by Angler AI, Inc. and licensed in two parts
([LICENSING.md](LICENSING.md)): AGPL-3.0-only for the server and apps,
Apache-2.0 for the language, API definitions and agent integrations.
Before a first pull request is merged, its author signs Angler AI's
contributor license agreement, which lets Angler AI license contributions
under both and relicense them later. The pull request's check links to it.

The full local check set is `scripts/check.sh` (see its header for what each
step runs and needs). This file holds the rules a change must follow that
the code alone does not show.

## Migrations: expand/contract

An update can be undone by running the previous release's code again
(`deploy/update.sh` restarts the previous image; `cloudflare/scripts/update.mjs`
runs `wrangler rollback`). The schema is **not** rolled back: the database
stays as the new release migrated it. That is only safe if every migration
keeps the previous release's code working. So:

1. **Expand freely.** A migration may add tables, columns (nullable, or
   `NOT NULL` with a `DEFAULT`), indexes, and triggers, and backfill data.
   The previous release ignores what it does not know.
2. **Contract in a later release.** Removing or reshaping something the code
   used takes two releases:
   - release R ships code that no longer reads or writes the old shape (it
     may still write both shapes while reading the new one);
   - a later release ships the migration that drops, renames or tightens it,
     annotated with R:

   ```sql
   -- contract(unused-since 0.3.0): 0.3.0 stopped reading run.legacy_note
   ALTER TABLE runbook.run DROP COLUMN legacy_note;
   ```

   The annotation sits on the comment line(s) right above the statement
   and names a release older than the one being built (the workspace
   version in `Cargo.toml`).

`scripts/check-migrations.mjs` (step "Migration safety" of `scripts/check.sh`)
enforces this for new files under
`crates/runbook-store-postgres/migrations/`, `cloudflare/migrations/d1/`
and the Durable Object schemas (`cloudflare/migrations/do/`, `orgdo/`,
`webhookdo/`), and the sign-in service's (`auth/drizzle/pg/`,
`auth/drizzle/sqlite/`). It lexes the SQL the way each database does
(Postgres nests block comments and has dollar quoting; SQLite does neither
and has backtick names), splits statements on every `;` outside quotes,
dollar quoting and SQLite trigger bodies, and allows only what it can prove
additive: creating tables (not TEMP, not `IF NOT EXISTS`, which may name an
existing one), indexes, views, schemas, sequences and types; `ADD COLUMN`
without inline constraints (nullable, or `NOT NULL` with a `DEFAULT`);
`INSERT`/`SELECT` calling only plain functions (`lower`, `coalesce`,
`now`...). Everything else is flagged:

- **contract** (needs `contract(unused-since X.Y.Z)`): every `DROP` (each
  name of a list), `RENAME`, every other `ALTER` (tables, indexes, schemas,
  types, sequences, views...), `CREATE OR REPLACE`, `REVOKE`, `DELETE`,
  `TRUNCATE`, `INSERT OR REPLACE` / `REPLACE INTO`, `UPDATE ... = NULL`,
  dynamic SQL (`DO`, `EXECUTE`, `CALL`), a call to any other function, and
  a new constraint or unique index on an existing table.
- **unprovable** (needs a contract annotation, or `-- expand-only: <why the
  previous release keeps working>`, which a reviewer then checks): creating
  a function, `WITH`, an `UPDATE` backfill, an upsert, a new enum value (see
  "New values"), any trigger on an existing table, DDL named inside a
  string, and any statement the check does not know. `expand-only` never
  covers a statement whose text (strings and bodies too) names `DROP`,
  `DELETE`, `TRUNCATE`, `ALTER` or `RENAME`.

Tables (and views and indexes) created earlier in the same file are new,
matched by kind and by qualified name with its quoting and case
(`scratch.run` is not `runbook.run`, `"Run"` is not `run`), so changing or
dropping them is free (a SQLite table rebuild's new table). A dropped index
or trigger counts as replaced only when the file creates it again as the
same kind on the same table with the same uniqueness and columns (index) or
timing and event (trigger) as in the earlier migration that defined it.
Every trigger body is read, a new table's too: a body that changes the
schema, calls a non-plain function or writes a table the file did not
create is flagged.

The check guards against honest mistakes and cheap tricks; review and CI,
not this script, stand between the database and a determined author. It
does not flag statements that are safe but slow: `VACUUM FULL`, `REINDEX`
or a plain `CREATE INDEX` on a large table lock it while the update runs
(prefer `CREATE INDEX CONCURRENTLY` in Postgres).

**One annotation grammar**, at the start of the line, exactly:
`-- contract(unused-since X.Y.Z): <reason>` and `-- expand-only: <reason>`.
The check rejects anything that looks like one but is not (another case,
indentation, missing spaces or reason); every parser of the rollback floor
(the check, `cloudflare/scripts/update.mjs`, runbook-server's `build.rs`)
reads only this form, and `scripts/tests/fixtures/floor-annotations.sql`
keeps them in agreement.
Once releases are tagged, `unused-since` must name one that exists. The
files that existed when the rule arrived are listed in the script's
`BASELINE` and are not checked; never add a file to it.

**The rollback floor**, per store (`postgres`, `d1`, `execution_do`,
`org_do`, `webhook_do`). The newest `unused-since` in a store's migrations is
its rollback floor: code older than it is not safe on the new schema, so an install
running it can only update with `--restore-backup` (deploy/update.sh,
`npm run update`), never by restarting its old code. Raise the floor by
hand for what migrations do not show (a new value written for the first
time) with `[workspace.metadata.runbook] rollback-floor = "X.Y.Z"` in the
workspace `Cargo.toml` (it applies to every store). `runbook-server
--version`, the image label `ai.angler.runbook.rollback-floor` (postgres)
and the bundle's `RELEASE.json` (every store) report it. On Cloudflare a
Durable Object floor cannot be met by restoring D1: an install below it can
only update with `--accept-no-rollback`.

Also:

- **Never edit an applied migration.** Each runner stores a file's SHA-256;
  an edited file is refused (`postgres.migration_history_mismatch`,
  `d1.migration_history_mismatch`, `do.migration_history_mismatch`), and
  the D1 and DO files are pinned by hash in their tests. Add the next
  numbered file instead.
- **Each edition reports its schema** in `/v1/meta` (`schema`), and the
  native server refuses a database a newer release migrated
  (`postgres.schema_newer_than_code`) unless `RUNBOOK_ALLOW_NEWER_SCHEMA=true`
  (the rollback path). On Workers, the D1 and Durable Object runners accept
  versions they do not know when every file they know is applied unchanged,
  because `wrangler rollback` cannot set a variable.

## New values

A store reads a closed set of values for a status, a kind or a role and
refuses one it does not know (for example the D1 directory's roles,
`crates/runbook-store-d1/src/directory.rs`, and admission's kinds,
`admission.rs`). After a rollback the previous release reads rows the newer
one wrote, including during its health check, so:

- a new value is **read-tolerant one release before it is written**: release
  R reads (or safely ignores) it, release R+1 may write it;
- if a value must be written in the release that introduces it, set
  `rollback-floor` to that release (above): older code is then refused a
  rollback onto it;
- a migration adding an enum value (`ALTER TYPE ... ADD VALUE`) is
  `expand-only` only when the previous release already reads it.

Every release's notes repeat this as a checklist item.

## Internal contract with the sign-in service

The api (Worker or native server) and the sign-in service call each other
(`POST /internal/session`, `POST /internal/can-register`). During an update
or a rollback they run adjacent releases for a moment, so these shapes only
grow: fields may be added, none removed, renamed or retyped.
`api/internal/` records each version, and tests on both sides read every
file there (see its README).

## Releases

One version number covers every binary and both editions: `[workspace.package]
version` in `Cargo.toml`. `runbook-server --version` (with the schema and
rollback floor), `runbook --version` (and `runbook version`, which also asks
the server) and `/v1/meta` report it.

To release: run `node scripts/release/release.mjs sync-versions X.Y.Z`
(then `cargo update -w`), rename `## Unreleased` in `docs/release-notes.md`
(operator notes, newest first) to `## X.Y.Z`, merge to main, and push the tag
`vX.Y.Z`; the release notes include that section.
`.github/workflows/release.yml` then runs two jobs. `build` (read-only)
refuses a tag that does not match `Cargo.toml` or whose commit is not on
`main`, builds the image for amd64 and arm64 into an OCI archive, the CLI and
server binaries, the Cloudflare bundle (its `RELEASE.json` names the image
digest and the rollback floors), the notes (schema, rollback floor, new
migrations per store, contract migrations, the new-values checklist,
changes) and `SHA256SUMS`, and pushes nothing. `publish` pushes the image
to `ghcr.io/angler-ai/runbook:X.Y.Z` by digest, attests the image and
every asset (`actions/attest-build-provenance`), creates the GitHub
Release, and only then moves `X.Y` and `latest` (when newest). The update
scripts verify those attestations (`gh attestation verify`), which GitHub
offers for public repositories: the repository goes public before the
first public release. They pin the signer workflow
(`angler-ai/the-runbook-app/.github/workflows/release.yml`) and the source ref
(`refs/tags/vX.Y.Z`), so another workflow, or `release.yml` run for a tag on
another commit, cannot vouch for a release. Turn on in the repository's
settings: a tag ruleset for `v*` (only maintainers create release tags;
none is moved or deleted) and a `publish` environment that only `v*` tags
may use, with required reviewers. `publish` holds the write tokens: on a
persistent self-hosted runner it shares a machine with `build`, so give it
an ephemeral or separate runner, or GitHub's runners once the repository is
public. While it is private the workflow runs on the default
self-hosted runner (it needs docker with buildx, skopeo, gh and git). Every
step is a script you can run locally: `node scripts/release/release.mjs
sync-versions|check-tag|image-tags|floor|cloudflare-bundle|notes|oci-digest|checksums`
and `scripts/release/binaries.sh`. The workflow refuses a release whose
package versions disagree or whose exact release-notes section is missing.

`deploy/update.sh` and `cloudflare/scripts/update.mjs` install these releases
(or build a tag from a checkout with `--source git` / `--source checkout`).
