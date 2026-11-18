//! D3 portable project archive on PostgreSQL. Export reads one project in a
//! single REPEATABLE READ snapshot; import writes it in one READ COMMITTED
//! transaction that rolls back on any failure, so no partial import
//! survives. Imported IDs are preserved; the target project must be empty.
use crate::{
    DatabaseConfig, PostgresStore, catalog,
    execution::{READ_COMMITTED, SNAPSHOT_READ},
    publish::{VERSION_COLUMNS, decode_draft_source, origin_label, origin_of, version_record},
    rows::{connection, db, decode, encode, int, label, uint},
};
use runbook_core::{
    StoreError,
    blob::BlobKey,
    catalog::{AdmissionState, DraftRevision, RunbookRecord, TenantScope},
    directory::{DirectoryCommit, DirectoryError, DirectoryMutation},
    directory_operations::VariablesRecord,
    portable::{
        BlobRow, ClaimLog, ImportOptions, PoolRow, PortableError, ProjectData, RunUnit, RunbookRow,
        StartReplayRow, StoreRows, VersionRow, WebhookRow, directory_commit, directory_slice,
        normalize, referenced_teams, validate,
    },
    retention::Tombstone,
    start::StartReplay,
    webhooks::Subscription,
};
use runbook_model::{ExecutionId, OrganizationId, ProjectId};
use std::collections::BTreeMap;
use tokio_postgres::Client;

fn store(error: StoreError) -> PortableError {
    match error {
        StoreError::Backend("postgres.unique_violation") => {
            PortableError::Conflict("import.id_conflict")
        }
        other => PortableError::Store(other),
    }
}
fn sql(error: tokio_postgres::Error) -> PortableError {
    store(db(error))
}
fn parse<T: std::str::FromStr>(text: &str) -> Result<T, PortableError> {
    text.parse()
        .map_err(|_| PortableError::Store(StoreError::Backend("postgres.invalid_stored_id")))
}

async fn open(config: &DatabaseConfig, begin: &str) -> Result<PostgresStore, PortableError> {
    let connection = PostgresStore::connect(config)
        .await
        .map_err(|e| store(connection(e)))?;
    connection
        .client()
        .batch_execute(begin)
        .await
        .map_err(sql)?;
    Ok(connection)
}
async fn finish<T>(
    connection: PostgresStore,
    result: Result<T, PortableError>,
) -> Result<T, PortableError> {
    match result {
        Ok(value) => {
            connection
                .client()
                .batch_execute("COMMIT")
                .await
                .map_err(sql)?;
            Ok(value)
        }
        Err(error) => {
            let _ = connection.client().batch_execute("ROLLBACK").await;
            Err(error)
        }
    }
}

/// Resolve a project by ID or slug inside one organization.
pub async fn resolve_project(
    config: &DatabaseConfig,
    org: &OrganizationId,
    project: &str,
) -> Result<Option<ProjectId>, PortableError> {
    let connection = PostgresStore::connect(config)
        .await
        .map_err(|e| store(connection(e)))?;
    let row = connection
        .client()
        .query_opt(
            "SELECT project_id FROM runbook.project \
             WHERE org_id = $1 AND (project_id = $2 OR slug = $2)",
            &[&org.as_str(), &project],
        )
        .await
        .map_err(sql)?;
    row.map(|row| parse(row.get(0))).transpose()
}

/// Export one project from a single consistent snapshot.
pub async fn export_project(
    config: &DatabaseConfig,
    scope: &TenantScope,
) -> Result<ProjectData, PortableError> {
    let connection = open(config, SNAPSHOT_READ).await?;
    let result = read_project(connection.client(), scope).await;
    finish(connection, result).await
}

async fn read_project(c: &Client, scope: &TenantScope) -> Result<ProjectData, PortableError> {
    let snapshot = crate::directory::snapshot_in(c, &scope.org_id)
        .await
        .map_err(|e| match e {
            DirectoryError::NotFound => PortableError::Store(StoreError::NotFound),
            other => other.into(),
        })?;
    let tenant = [&scope.org_id.as_str(), &scope.project_id.as_str()];
    let tenant = [
        tenant[0] as &(dyn tokio_postgres::types::ToSql + Sync),
        tenant[1],
    ];
    let variables = c
        .query_opt(
            "SELECT body, revision FROM runbook.project_variables \
             WHERE org_id = $1 AND project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
        .map(|row| {
            Ok::<_, PortableError>(VariablesRecord {
                variables: decode(row.get(0))?,
                revision: uint(row.get(1))?,
            })
        })
        .transpose()?;
    let mut rows = StoreRows::default();
    for row in c
        .query(
            "SELECT runbook_id, slug, origin, archived, draft_source, draft_revision \
             FROM runbook.runbook WHERE org_id = $1 AND project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        rows.runbooks.push(RunbookRow {
            record: RunbookRecord {
                runbook_id: parse(row.get(0))?,
                slug: row.get(1),
                origin: origin_of(row.get(2))?,
                archived: row.get(3),
            },
            draft_source: decode_draft_source(row.get(4))?,
            draft_revision: uint(row.get(5))?,
        });
    }
    for row in c
        .query(
            &format!(
                "SELECT {VERSION_COLUMNS}, runbook_id FROM runbook.runbook_version \
                 WHERE org_id = $1 AND project_id = $2"
            ),
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        rows.versions.push(VersionRow {
            runbook_id: parse(row.get(9))?,
            version: version_record(&row)?,
        });
    }
    for row in c
        .query(
            "SELECT key, revision, source FROM runbook.draft \
             WHERE org_id = $1 AND project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        rows.drafts.push(DraftRevision {
            key: row.get(0),
            revision: uint(row.get(1))?,
            source: row.get(2),
        });
    }
    for row in c
        .query(
            &format!(
                "SELECT {} FROM runbook.skill_version \
                 WHERE org_id = $1 AND project_id = $2 ORDER BY seq",
                crate::skills::COLUMNS
            ),
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        rows.skills.push(crate::skills::skill_of(&row)?);
    }
    for row in c
        .query(
            "SELECT key, concurrency_limit, active FROM runbook.admission_pool \
             WHERE org_id = $1 AND project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        let key: String = row.get(0);
        let queue = c
            .query(
                "SELECT execution_id FROM runbook.admission \
                 WHERE org_id = $1 AND project_id = $2 AND key = $3 \
                 AND queue_order IS NOT NULL ORDER BY queue_order",
                &[tenant[0], tenant[1], &key],
            )
            .await
            .map_err(sql)?
            .iter()
            .map(|row| parse(row.get(0)))
            .collect::<Result<_, _>>()?;
        rows.pools.push(PoolRow {
            key,
            limit: u32::try_from(row.get::<_, i32>(1))
                .map_err(|_| StoreError::Backend("postgres.invalid_pool"))?,
            active: u32::try_from(row.get::<_, i32>(2))
                .map_err(|_| StoreError::Backend("postgres.invalid_pool"))?,
            queue,
        });
    }
    let ids: Vec<ExecutionId> = c
        .query(
            "SELECT e.execution_id FROM runbook.execution e \
             JOIN runbook.execution_owner o USING (execution_id) \
             WHERE o.org_id = $1 AND o.project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
        .iter()
        .map(|row| parse(row.get(0)))
        .collect::<Result<_, _>>()?;
    for id in ids {
        rows.runs.push(read_run(c, scope, &id).await?);
    }
    for row in c
        .query(
            "SELECT body FROM runbook.tombstone WHERE org_id = $1 AND project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        rows.tombstones.push(decode(row.get(0))?);
    }
    for row in c
        .query(
            "SELECT key FROM runbook.blob WHERE org_id = $1 AND project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        let key_text: &str = row.get(0);
        let bytes = crate::blob::read_bytes(c, scope, key_text)
            .await?
            .ok_or(StoreError::Backend("postgres.blob_vanished"))?;
        rows.blobs.push(BlobRow {
            key: decode::<BlobKey>(key_text)?,
            bytes,
        });
    }
    rows.memory = crate::memory_entries::export(c, scope).await?;
    let mut webhooks = Vec::new();
    for row in c
        .query(
            "SELECT body FROM runbook.webhook_subscription \
             WHERE org_id = $1 AND project_id = $2",
            &tenant,
        )
        .await
        .map_err(sql)?
    {
        let subscription: Subscription = decode(row.get(0))?;
        webhooks.push(WebhookRow::from_subscription(&subscription));
    }
    let directory = directory_slice(&snapshot, &scope.project_id, &referenced_teams(&rows))?;
    let mut data = ProjectData {
        scope: scope.clone(),
        directory,
        variables,
        rows,
        webhooks,
    };
    normalize(&mut data);
    Ok(data)
}

async fn read_run(
    c: &Client,
    scope: &TenantScope,
    id: &ExecutionId,
) -> Result<RunUnit, PortableError> {
    let slice = crate::retention::read_slice(c, scope, id).await?;
    let start = c
        .query_opt(
            "SELECT body FROM runbook.execution_start WHERE execution_id = $1",
            &[&id.as_str()],
        )
        .await
        .map_err(sql)?
        .map(|row| decode(row.get(0)))
        .transpose()?;
    let mut start_replays = Vec::new();
    for row in c
        .query(
            "SELECT idempotency_key, principal, request_hash, outcome, created_at_ms \
             FROM runbook.start_replay \
             WHERE org_id = $1 AND project_id = $2 AND execution_id = $3",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &id.as_str(),
            ],
        )
        .await
        .map_err(sql)?
    {
        start_replays.push(StartReplayRow {
            replay: StartReplay {
                key: row.get(0),
                principal: row.get(1),
                request_hash: row
                    .get::<_, Vec<u8>>(2)
                    .try_into()
                    .map_err(|_| StoreError::Backend("postgres.invalid_hash"))?,
                outcome: decode(row.get(3))?,
            },
            created_at_ms: row.get(4),
        });
    }
    let mut logs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in c
        .query(
            "SELECT step_run_id, line FROM runbook.claim_log \
             WHERE execution_id = $1 ORDER BY step_run_id, seq",
            &[&id.as_str()],
        )
        .await
        .map_err(sql)?
    {
        logs.entry(row.get(0)).or_default().push(row.get(1));
    }
    let claim_logs = logs
        .into_iter()
        .map(|(step_run_id, lines)| {
            Ok(ClaimLog {
                step_run_id: parse(&step_run_id)?,
                lines,
            })
        })
        .collect::<Result<_, PortableError>>()?;
    Ok(RunUnit {
        slice,
        start,
        start_replays,
        claim_logs,
    })
}

/// Every table that holds project-scoped rows. Any row refuses the import.
const SCOPED_TABLES: &[&str] = &[
    "runbook",
    "runbook_version",
    "draft",
    "skill_version",
    "admission_pool",
    "admission",
    "execution_owner",
    "work_lease",
    "execution_start",
    "start_replay",
    "start_idempotency",
    "credential_family",
    "execution_credential",
    "step_run_index",
    "tombstone",
    "blob",
    "project_variables",
    "webhook_subscription",
    "webhook_event",
    "webhook_delivery",
    "webhook_replay",
    "project_event_outbox",
    "memory_entry",
    "memory_revision",
];

/// Import into an empty project in one transaction.
pub async fn import_project(
    config: &DatabaseConfig,
    data: &ProjectData,
    options: ImportOptions,
) -> Result<(), PortableError> {
    // Untrusted content: validated before the database is opened.
    validate(data)?;
    let connection = open(config, READ_COMMITTED).await?;
    let result = write_project(connection.client(), data, options).await;
    finish(connection, result).await
}

async fn write_project(
    c: &Client,
    data: &ProjectData,
    options: ImportOptions,
) -> Result<(), PortableError> {
    let scope = &data.scope;
    let org = scope.org_id.as_str();
    let project = scope.project_id.as_str();
    // Serialize concurrent imports of one project ID across organizations.
    c.execute(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        &[&project],
    )
    .await
    .map_err(sql)?;
    // F35 (R44): the organization's directory revision row next, before any
    // check. Authoring commits and directory mutations take it first too, so
    // a live one finishes before the emptiness check reads, and the lock
    // order (revision row, then project row) matches GrantProjectRole.
    c.execute(
        "SELECT revision FROM runbook.directory_revision WHERE org_id = $1 FOR UPDATE",
        &[&org],
    )
    .await
    .map_err(sql)?;
    // An existing (empty) project row is locked: writers that reference it
    // by foreign key (memberships, variables) wait until this import ends.
    // Tables without that key are not blocked; stop the server to import.
    if let Some(row) = c
        .query_opt(
            "SELECT org_id FROM runbook.project WHERE project_id = $1 FOR UPDATE",
            &[&project],
        )
        .await
        .map_err(sql)?
        && row.get::<_, &str>(0) != org
    {
        return Err(PortableError::Conflict("import.project_conflict"));
    }
    if !options.into_existing_org {
        let exists: bool = c
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM runbook.organization WHERE org_id = $1)",
                &[&org],
            )
            .await
            .map_err(sql)?
            .get(0);
        if exists {
            return Err(PortableError::Conflict("import.org_exists"));
        }
    }
    for table in SCOPED_TABLES {
        let busy: bool = c
            .query_one(
                &format!(
                    "SELECT EXISTS (SELECT 1 FROM runbook.{table} \
                     WHERE org_id = $1 AND project_id = $2)"
                ),
                &[&org, &project],
            )
            .await
            .map_err(sql)?
            .get(0);
        if busy {
            return Err(PortableError::TargetNotEmpty);
        }
    }
    check_unused_ids(c, data).await?;
    write_directory(c, data, options).await?;
    if let Some(variables) = &data.variables {
        c.execute(
            "INSERT INTO runbook.project_variables (org_id, project_id, body, revision) \
             VALUES ($1, $2, $3, $4)",
            &[
                &org,
                &project,
                &encode(&variables.variables)?,
                &int(variables.revision)?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    let rows = &data.rows;
    for row in &rows.runbooks {
        c.execute(
            "INSERT INTO runbook.runbook \
             (org_id, project_id, runbook_id, slug, origin, archived, draft_source, draft_revision) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &org,
                &project,
                &row.record.runbook_id.as_str(),
                &row.record.slug,
                &origin_label(row.record.origin),
                &row.record.archived,
                &row.draft_source.as_bytes(),
                &int(row.draft_revision)?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    for row in &rows.versions {
        let v = &row.version;
        c.execute(
            &format!(
                "INSERT INTO runbook.runbook_version \
                 (org_id, project_id, runbook_id, {VERSION_COLUMNS}) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"
            ),
            &[
                &org,
                &project,
                &row.runbook_id.as_str(),
                &v.version_id.as_str(),
                &v.number.map(|n| n.get() as i32),
                &v.branch,
                &v.commit_sha,
                &v.semantic_hash.as_slice(),
                &v.snapshot_digest.as_slice(),
                &int(v.snapshot_size)?,
                &v.source,
                &encode(&v.pins)?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    for draft in &rows.drafts {
        c.execute(
            "INSERT INTO runbook.draft (org_id, project_id, key, revision, source) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &org,
                &project,
                &draft.key,
                &int(draft.revision)?,
                &draft.source,
            ],
        )
        .await
        .map_err(sql)?;
    }
    for skill in &rows.skills {
        c.execute(
            &format!(
                "INSERT INTO runbook.skill_version (org_id, project_id, {}) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
                crate::skills::COLUMNS
            ),
            &[
                &org,
                &project,
                &skill.name,
                &skill.description,
                &skill.hash.as_slice(),
                &encode(&skill.manifest)?,
                &encode(&skill.provenance)?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    for pool in &rows.pools {
        c.execute(
            "INSERT INTO runbook.admission_pool \
             (org_id, project_id, key, concurrency_limit, active) VALUES ($1, $2, $3, $4, $5)",
            &[
                &org,
                &project,
                &pool.key,
                &i32::try_from(pool.limit).map_err(|_| StoreError::Backend("import.pool"))?,
                &i32::try_from(pool.active).map_err(|_| StoreError::Backend("import.pool"))?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    let mut queued = BTreeMap::new();
    for unit in &rows.runs {
        write_run(c, scope, unit, &mut queued).await?;
    }
    // Queued admissions take fresh queue positions in archived FIFO order.
    for pool in &rows.pools {
        for id in &pool.queue {
            let (request, admission) = queued.remove(id).ok_or(PortableError::Archive {
                code: "archive.invalid_admission",
                path: None,
            })?;
            insert_admission(c, scope, request, admission, true).await?;
        }
    }
    if !queued.is_empty() {
        return Err(PortableError::Archive {
            code: "archive.invalid_admission",
            path: None,
        });
    }
    for tombstone in &rows.tombstones {
        insert_tombstone(c, tombstone).await?;
    }
    for blob in &rows.blobs {
        if !crate::blob::insert_blob(c, scope, &blob.key, &blob.bytes)
            .await
            .map_err(store)?
        {
            return Err(PortableError::Conflict("import.id_conflict"));
        }
    }
    crate::memory_entries::import(c, &rows.memory)
        .await
        .map_err(|e| match e {
            StoreError::InvalidBatch("memory.duplicate") => {
                PortableError::Conflict("import.id_conflict")
            }
            other => store(other),
        })?;
    for row in &data.webhooks {
        c.execute(
            "INSERT INTO runbook.webhook_subscription \
             (subscription_id, org_id, project_id, created_at_ms, body) \
             VALUES ($1, $2, $3, $4, $5)",
            &[
                &row.id.as_str(),
                &org,
                &project,
                &row.created_at_ms,
                &encode(&row.to_subscription(scope))?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    Ok(())
}

async fn insert_admission(
    c: &Client,
    scope: &TenantScope,
    request: &runbook_core::catalog::AdmitRequest,
    admission: &runbook_core::catalog::Admission,
    queued: bool,
) -> Result<(), PortableError> {
    c.execute(
        "INSERT INTO runbook.admission \
         (execution_id, org_id, project_id, key, state, queue_order, request, admitted_at_ms) \
         VALUES ($1, $2, $3, $4, $5, \
         CASE WHEN $6 THEN nextval('runbook.admission_queue_order') END, $7, $8)",
        &[
            &admission.execution_id.as_str(),
            &scope.org_id.as_str(),
            &scope.project_id.as_str(),
            &request.key,
            &catalog::state_label(admission.state),
            &queued,
            &encode(request)?,
            &admission.admitted_at_ms,
        ],
    )
    .await
    .map_err(sql)?;
    Ok(())
}

async fn insert_tombstone(c: &Client, tombstone: &Tombstone) -> Result<(), PortableError> {
    c.execute(
        "INSERT INTO runbook.tombstone (execution_id, org_id, project_id, body) \
         VALUES ($1, $2, $3, $4)",
        &[
            &tombstone.execution_id.as_str(),
            &tombstone.scope.org_id.as_str(),
            &tombstone.scope.project_id.as_str(),
            &encode(tombstone)?,
        ],
    )
    .await
    .map_err(sql)?;
    Ok(())
}

type Queued<'a> = BTreeMap<
    ExecutionId,
    (
        &'a runbook_core::catalog::AdmitRequest,
        &'a runbook_core::catalog::Admission,
    ),
>;

async fn write_run<'a>(
    c: &Client,
    scope: &TenantScope,
    unit: &'a RunUnit,
    queued: &mut Queued<'a>,
) -> Result<(), PortableError> {
    let slice = &unit.slice;
    let run = &slice.run;
    let p = &run.projection;
    let id = p.execution_id.as_str();
    c.execute(
        "INSERT INTO runbook.execution_owner (execution_id, org_id, project_id) \
         VALUES ($1, $2, $3)",
        &[&id, &scope.org_id.as_str(), &scope.project_id.as_str()],
    )
    .await
    .map_err(sql)?;
    c.execute(
        "INSERT INTO runbook.execution \
         (execution_id, revision, runbook_id, status, completed_at_ms, projection) \
         VALUES ($1, $2, $3, $4, $5, $6)",
        &[
            &id,
            &int(run.revision)?,
            &p.runbook_id.as_str(),
            &label(&p.status)?,
            &p.completed_at_ms,
            &encode(p)?,
        ],
    )
    .await
    .map_err(sql)?;
    let seqs = run
        .events
        .iter()
        .map(|e| int(e.seq))
        .collect::<Result<Vec<_>, _>>()?;
    let bodies = run
        .events
        .iter()
        .map(encode)
        .collect::<Result<Vec<_>, _>>()?;
    c.execute(
        "INSERT INTO runbook.execution_event (execution_id, seq, body) \
         SELECT $1, * FROM unnest($2::bigint[], $3::text[])",
        &[&id, &seqs, &bodies],
    )
    .await
    .map_err(sql)?;
    for step in &p.steps {
        c.execute(
            "INSERT INTO runbook.step_run_index (step_run_id, execution_id, org_id, project_id) \
             VALUES ($1, $2, $3, $4)",
            &[
                &step.step_run_id.as_str(),
                &id,
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
            ],
        )
        .await
        .map_err(sql)?;
    }
    for (table, timers) in [
        ("execution_timer", &run.timers),
        ("execution_timer_history", &run.timer_history),
    ] {
        for timer in timers.values() {
            c.execute(
                &format!(
                    "INSERT INTO runbook.{table} (execution_id, key, body) VALUES ($1, $2, $3)"
                ),
                &[&id, &timer.key, &encode(timer)?],
            )
            .await
            .map_err(sql)?;
        }
    }
    for intent in run.outbox.values() {
        c.execute(
            "INSERT INTO runbook.execution_outbox (execution_id, event_id, body) \
             VALUES ($1, $2, $3)",
            &[&id, &intent.event_id, &encode(intent)?],
        )
        .await
        .map_err(sql)?;
    }
    for record in run.idempotency.values() {
        c.execute(
            "INSERT INTO runbook.execution_idempotency (execution_id, key, body) \
             VALUES ($1, $2, $3)",
            &[&id, &record.key, &encode(record)?],
        )
        .await
        .map_err(sql)?;
    }
    for claim in run.claims.values() {
        c.execute(
            "INSERT INTO runbook.execution_claim \
             (execution_id, step_run_id, generation, body, lease_deadline_at_ms, released) \
             VALUES ($1, $2, $3, $4, $5, $6)",
            &[
                &id,
                &claim.step_run_id.as_str(),
                &int(claim.generation)?,
                &encode(claim)?,
                &claim.lease_deadline_at_ms,
                &claim.released,
            ],
        )
        .await
        .map_err(sql)?;
    }
    for update in run.derived_outbox.values() {
        c.execute(
            "INSERT INTO runbook.execution_derived_outbox (execution_id, sequence, body) \
             VALUES ($1, $2, $3)",
            &[&id, &int(update.summary.sequence)?, &encode(update)?],
        )
        .await
        .map_err(sql)?;
    }
    for record in &slice.actor_replays {
        let (prefix, at) = runbook_core::actor::replay_record_parts(record)?
            .ok_or(StoreError::InvalidBatch("replay.invalid_record"))?;
        c.execute(
            "INSERT INTO runbook.actor_replay (execution_id, prefix, at_ms, body) \
             VALUES ($1, $2, $3, $4)",
            &[&id, &prefix, &at, &encode(record)?],
        )
        .await
        .map_err(sql)?;
    }
    let catalog_slice = &slice.catalog;
    if let Some(summary) = &catalog_slice.summary {
        catalog::write_summary(c, summary, false)
            .await
            .map_err(store)?;
    }
    for (ordinal, row) in catalog_slice.assignments.iter().enumerate() {
        catalog::insert_assignment(c, id, ordinal, row)
            .await
            .map_err(store)?;
    }
    if let Some((request, admission)) = &catalog_slice.admission {
        if admission.state == AdmissionState::Queued {
            queued.insert(p.execution_id.clone(), (request, admission));
        } else {
            insert_admission(c, scope, request, admission, false).await?;
        }
    }
    for work in &slice.work {
        let (kind, key, work_generation) = crate::work_queue::columns(&work.lease.id);
        c.execute(
            "INSERT INTO runbook.work_lease \
             (org_id, project_id, execution_id, kind, work_key, work_generation, \
              generation, deadline_at_ms, acknowledged, body) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &id,
                &kind,
                &key,
                &int(work_generation)?,
                &int(work.lease.generation)?,
                &work.lease.deadline_at_ms,
                &work.acknowledged,
                &encode(&work.lease)?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    if let Some(start) = &unit.start {
        c.execute(
            "INSERT INTO runbook.execution_start (execution_id, org_id, project_id, body) \
             VALUES ($1, $2, $3, $4)",
            &[
                &id,
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &encode(start)?,
            ],
        )
        .await
        .map_err(sql)?;
    }
    for row in &unit.start_replays {
        let replay = &row.replay;
        c.execute(
            "INSERT INTO runbook.start_replay \
             (org_id, project_id, idempotency_key, principal, request_hash, execution_id, \
              outcome, created_at_ms) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &replay.key,
                &replay.principal,
                &replay.request_hash.as_slice(),
                &replay.outcome.execution_id.as_str(),
                &encode(&replay.outcome)?,
                &row.created_at_ms,
            ],
        )
        .await
        .map_err(sql)?;
    }
    for log in &unit.claim_logs {
        for (n, line) in log.lines.iter().enumerate() {
            c.execute(
                "INSERT INTO runbook.claim_log (execution_id, step_run_id, seq, line) \
                 VALUES ($1, $2, $3, $4)",
                &[&id, &log.step_run_id.as_str(), &(n as i64 + 1), line],
            )
            .await
            .map_err(sql)?;
        }
    }
    Ok(())
}

/// Any execution ID in the archive (runs and tombstones) that already
/// exists anywhere on the target is a conflict: an archive never names a
/// run it does not bring. Other IDs (runbooks, versions, step runs, teams,
/// users, subscriptions, blobs) are primary keys written by plain INSERT,
/// so an existing one fails the transaction as a unique violation.
async fn check_unused_ids(c: &Client, data: &ProjectData) -> Result<(), PortableError> {
    let ids: Vec<&str> = data
        .rows
        .runs
        .iter()
        .map(|r| r.slice.run.projection.execution_id.as_str())
        .chain(data.rows.tombstones.iter().map(|t| t.execution_id.as_str()))
        .collect();
    let taken: bool = c
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM runbook.execution_owner WHERE execution_id = ANY($1)) \
             OR EXISTS (SELECT 1 FROM runbook.tombstone WHERE execution_id = ANY($1)) \
             OR EXISTS (SELECT 1 FROM runbook.webhook_event WHERE execution_id = ANY($1))",
            &[&ids],
        )
        .await
        .map_err(sql)?
        .get(0);
    if taken {
        return Err(PortableError::Conflict("import.id_conflict"));
    }
    Ok(())
}

/// The directory part of an import, as plain INSERTs under the
/// organization's locked revision row: nothing existing is updated.
async fn write_directory(
    c: &Client,
    data: &ProjectData,
    options: ImportOptions,
) -> Result<(), PortableError> {
    let org = data.scope.org_id.as_str();
    let locked = c
        .query_opt(
            "SELECT revision FROM runbook.directory_revision WHERE org_id = $1 FOR UPDATE",
            &[&org],
        )
        .await
        .map_err(sql)?;
    let existing = match locked {
        Some(_) => Some(
            crate::directory::snapshot_in(c, &data.scope.org_id)
                .await
                .map_err(PortableError::from)?,
        ),
        None => None,
    };
    let ids: Vec<&str> = data.directory.users.iter().map(|u| u.id.as_str()).collect();
    let known = c
        .query(
            "SELECT user_id FROM runbook.user_identity WHERE user_id = ANY($1)",
            &[&ids],
        )
        .await
        .map_err(sql)?
        .iter()
        .map(|row| parse(row.get(0)))
        .collect::<Result<_, _>>()?;
    let Some(DirectoryCommit { mutations, .. }) =
        directory_commit(&data.directory, existing.as_ref(), &known, options)?
    else {
        return Ok(());
    };
    for mutation in mutations {
        match mutation {
            DirectoryMutation::CreateOrganization { title } => {
                c.execute(
                    "INSERT INTO runbook.organization (org_id, title) VALUES ($1, $2)",
                    &[&org, &title],
                )
                .await
                .map_err(sql)?;
                c.execute(
                    "INSERT INTO runbook.directory_revision (org_id, revision) VALUES ($1, 0)",
                    &[&org],
                )
                .await
                .map_err(sql)?;
            }
            DirectoryMutation::CreateUser(user) => {
                c.execute(
                    "INSERT INTO runbook.user_identity (user_id, name) VALUES ($1, $2)",
                    &[&user.id.as_str(), &user.name],
                )
                .await
                .map_err(sql)?;
                c.execute(
                    "INSERT INTO runbook.org_user (org_id, user_id, name, active, org_role) \
                     VALUES ($1, $2, $3, $4, $5)",
                    &[
                        &org,
                        &user.id.as_str(),
                        &user.name,
                        &user.active,
                        &crate::directory::org_role_label(user.role),
                    ],
                )
                .await
                .map_err(sql)?;
            }
            DirectoryMutation::CreateTeam(team) => {
                c.execute(
                    "INSERT INTO runbook.team (team_id, org_id, slug, title) VALUES ($1, $2, $3, $4)",
                    &[&team.id.as_str(), &org, &team.slug, &team.title],
                )
                .await
                .map_err(sql)?;
            }
            DirectoryMutation::CreateProject(project) => {
                c.execute(
                    "INSERT INTO runbook.project (project_id, org_id, slug, title) \
                     VALUES ($1, $2, $3, $4)",
                    &[&project.id.as_str(), &org, &project.slug, &project.title],
                )
                .await
                .map_err(sql)?;
            }
            DirectoryMutation::GrantProjectRole {
                project_id,
                user_id,
                role,
            } => {
                c.execute(
                    "INSERT INTO runbook.project_member (org_id, project_id, user_id, role) \
                     VALUES ($1, $2, $3, $4)",
                    &[
                        &org,
                        &project_id.as_str(),
                        &user_id.as_str(),
                        &crate::directory::role_label(role),
                    ],
                )
                .await
                .map_err(sql)?;
            }
            DirectoryMutation::AddTeamMember { team_id, user_id } => {
                c.execute(
                    "INSERT INTO runbook.team_member (org_id, team_id, user_id) \
                     VALUES ($1, $2, $3)",
                    &[&org, &team_id.as_str(), &user_id.as_str()],
                )
                .await
                .map_err(sql)?;
            }
            _ => return Err(PortableError::Conflict("import.directory_conflict")),
        }
    }
    c.execute(
        "UPDATE runbook.directory_revision SET revision = revision + 1 WHERE org_id = $1",
        &[&org],
    )
    .await
    .map_err(sql)?;
    Ok(())
}
