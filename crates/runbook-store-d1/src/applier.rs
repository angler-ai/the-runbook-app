//! The derived-row applier (CF3, D56): one ExecutionDO derived row into D1,
//! idempotent by (execution, sequence). It does what the memory and
//! Postgres adapters do inline at commit:
//! - memory proposals (D40), each idempotent by (execution, proposal
//!   sequence) on its own, so a later summary never hides an earlier row's
//!   proposals;
//! - the run summary with its search columns, and the run's inbox
//!   (assignments), only for a sequence newer than the stored one (an old
//!   or duplicate row writes nothing);
//! - the step-run index the step routes resolve through;
//! - marking the run's daily archive stale when the run is already in a
//!   file (ARC1's `mark_run_stale`);
//! - releasing the run's admission when the row says so, promoting
//!   waiters (`admission.rs`).
//!
//! A run's summary, inbox and index rows have one writer, the run's own
//! ExecutionDO, which applies its rows in sequence order, so the plan may
//! read them first. Only what other runs share is guarded: the owner row,
//! the memory quota (a conditional insert) and the admission pool.
use crate::{
    admission::{Promotion, claim_owner},
    catalog::{D1Catalog, err, label},
    memory::scope_key,
    sql::{Columns, D1, D1Error, Statement, json, prepare, uint},
};
use runbook_core::{
    StoreError,
    catalog::{Assignment, AssignmentKind, DerivedUpdate, RunSummary},
    memory_entries::{
        MemoryAction, MemoryProposal, MemoryRevisionRecord, actor_label, run_action, run_quota,
        text_words,
    },
};
use runbook_model::{MemorySource, MemoryStatus, StepRunId};
use std::collections::BTreeSet;

/// What applying one row did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// The summary and inbox were written (false for an old or duplicate
    /// sequence, or a deleted run).
    pub summary: bool,
    /// Waiters the release promoted; the host tells their objects.
    pub promoted: Vec<Promotion>,
}

fn kind_label(kind: AssignmentKind) -> &'static str {
    match kind {
        AssignmentKind::Work => "work",
        AssignmentKind::Approval => "approval",
        AssignmentKind::Handoff => "handoff",
    }
}
pub(crate) fn recipient_key(recipient: &runbook_model::RecipientRef) -> String {
    match recipient {
        runbook_model::RecipientRef::User(user) => format!("user:{user}"),
        runbook_model::RecipientRef::Team(team) => format!("team:{team}"),
    }
}
/// The words run search matches: the title and the tags (Postgres indexes
/// `title || tags` with the `simple` configuration).
pub(crate) fn summary_words(summary: &RunSummary) -> String {
    text_words(&format!("{} {}", summary.title, summary.tags.join(" "))).join(" ")
}

/// The summary row with its search columns; `created_at_ms` is the one the
/// start stamped (later summaries keep it).
pub(crate) fn summary_statement(summary: &RunSummary) -> Result<Statement, StoreError> {
    Ok(prepare(
        "INSERT INTO run_summary (execution_id, runbook_id, sequence, status, completed_at_ms, \
         body, title, tags, fields, created_by, created_at_ms, search_tsv) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (execution_id) DO UPDATE SET \
         runbook_id = excluded.runbook_id, sequence = excluded.sequence, status = excluded.status, \
         completed_at_ms = excluded.completed_at_ms, body = excluded.body, title = excluded.title, \
         tags = excluded.tags, fields = excluded.fields, created_by = excluded.created_by, \
         created_at_ms = excluded.created_at_ms, search_tsv = excluded.search_tsv \
         WHERE run_summary.sequence < excluded.sequence",
    )
    .bind(summary.execution_id.as_str())
    .bind(summary.runbook_id.as_str())
    .bind(uint(summary.sequence).map_err(err)?)
    .bind(label(&summary.status)?)
    .bind(summary.completed_at_ms)
    .bind(json(summary).map_err(err)?)
    .bind(summary.title.as_str())
    .bind(json(&summary.tags).map_err(err)?)
    .bind(serde_json::to_string(&summary.fields).map_err(|_| StoreError::Backend("d1.encode"))?)
    .bind(summary.created_by.as_str())
    .bind(summary.created_at_ms)
    .bind(summary_words(summary)))
}

pub(crate) fn assignment_statement(
    execution: &str,
    ordinal: usize,
    row: &Assignment,
) -> Result<Statement, StoreError> {
    let recipients: Vec<String> = row.recipients.iter().map(recipient_key).collect();
    Ok(prepare(
        "INSERT INTO assignment (execution_id, ordinal, step_run_id, body, kind, updated_at_ms, \
         recipients, voted_by) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(execution)
    .bind(
        i64::try_from(ordinal)
            .map_err(|_| StoreError::InvalidBatch("catalog.too_many_assignments"))?,
    )
    .bind(row.step_run_id.as_str())
    .bind(json(row).map_err(err)?)
    .bind(kind_label(row.kind))
    .bind(row.updated_at_ms)
    .bind(json(&recipients).map_err(err)?)
    .bind(json(&row.voted_by).map_err(err)?))
}

/// Gate fix D4: `insert` (an `assignment_statement`) only while D1's
/// summary of `execution` is older than `sequence`.
fn fresh_assignment(insert: Statement, execution: &str, sequence: crate::Value) -> Statement {
    let mut statement = insert;
    statement.sql = statement
        .sql
        .replacen(
            ") VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            ") SELECT ?, ?, ?, ?, ?, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM run_summary \
         WHERE execution_id = ? AND sequence >= ?)",
            1,
        )
        .into();
    statement.bind(execution).bind(sequence)
}

/// D40 on D1: insert a run's memory write (a proposal, or a MEM6 live save)
/// only if no write of this execution at this or a later sequence is
/// stored, its ID is free and its scope is under the quota of its status;
/// its first revision only if the entry went in. A write over the quota
/// (two runs racing the last slot: the ExecutionDO checked the quota before
/// committing, the catalog applies later) cannot fail the commit that
/// recorded it, which has already happened, so it leaves an archived drop
/// marker instead: the same ID, `dropped: memory.limit_reached`, and a
/// `drop` revision. Reviewers find it by the ID the run's document lists.
fn proposal_statements(
    scope: &runbook_core::catalog::TenantScope,
    proposal: &MemoryProposal,
) -> Result<Vec<Statement>, StoreError> {
    let entry = &proposal.entry;
    let MemorySource::StepRun {
        execution_id,
        actor,
        ..
    } = &entry.source
    else {
        return Err(StoreError::InvalidBatch("memory.invalid_entry"));
    };
    if &entry.scope != scope
        || entry.status == MemoryStatus::Archived
        || execution_id != &proposal.execution_id
    {
        return Err(StoreError::InvalidBatch("memory.invalid_entry"));
    }
    let written = run_action(entry);
    let mut entry = entry.clone();
    entry.revision = 1;
    entry.last_writer = Some(actor_label(actor));
    let mut marker = entry.clone();
    marker.status = MemoryStatus::Archived;
    marker.dropped = Some("memory.limit_reached".into());
    let actor = actor_label(actor);
    let sequence = uint(proposal.sequence).map_err(err)?;
    let words = text_words(&format!("{} {}", entry.title, entry.body.as_ref())).join(" ");
    let limit = uint(run_quota(entry.status) as u64).map_err(err)?;
    let counted = label(&entry.status)?;
    const INSERT: &str = "INSERT INTO memory_entry (memory_id, org_id, project_id, scope_key, \
        status, step_id, tags, revision, expires_at_ms, created_at_ms, updated_at_ms, \
        source_execution_id, proposal_sequence, body, search_tsv) \
        SELECT ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ? \
        WHERE NOT EXISTS (SELECT 1 FROM memory_entry WHERE source_execution_id = ? \
        AND proposal_sequence >= ?) \
        AND NOT EXISTS (SELECT 1 FROM memory_entry WHERE memory_id = ?)";
    let mut out = vec![];
    for (stored, action, sql) in [
        (
            &entry,
            written,
            format!(
                "{INSERT} AND (SELECT count(*) FROM memory_entry WHERE org_id = ? \
                 AND project_id = ? AND scope_key = ? AND status = ?) < ?"
            ),
        ),
        (&marker, MemoryAction::Drop, INSERT.to_owned()),
    ] {
        let status = label(&stored.status)?;
        let mut insert = prepare(sql)
            .bind(entry.id.as_str())
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str())
            .bind(scope_key(&entry.memory_scope))
            .bind(status.clone())
            .bind(entry.step_id.clone())
            .bind(json(&entry.tags).map_err(err)?)
            .bind(entry.expires_at_ms)
            .bind(entry.created_at_ms)
            .bind(entry.updated_at_ms)
            .bind(proposal.execution_id.as_str())
            .bind(sequence.clone())
            .bind(json(stored).map_err(err)?)
            .bind(words.clone())
            .bind(proposal.execution_id.as_str())
            .bind(sequence.clone())
            .bind(entry.id.as_str());
        if action != MemoryAction::Drop {
            insert = insert
                .bind(scope.org_id.as_str())
                .bind(scope.project_id.as_str())
                .bind(scope_key(&entry.memory_scope))
                .bind(counted.clone())
                .bind(limit.clone());
        }
        let revision = MemoryRevisionRecord {
            action,
            actor: actor.clone(),
            at_ms: entry.updated_at_ms,
            entry: stored.clone(),
        };
        out.push(insert);
        out.push(
            prepare(
                "INSERT INTO memory_revision (memory_id, revision, org_id, project_id, action, \
                 at_ms, body) SELECT ?, 1, ?, ?, ?, ?, ? \
                 WHERE EXISTS (SELECT 1 FROM memory_entry WHERE memory_id = ? \
                 AND source_execution_id = ? AND proposal_sequence = ? AND status = ?) \
                 AND NOT EXISTS (SELECT 1 FROM memory_revision WHERE memory_id = ? AND revision = 1)",
            )
            .bind(entry.id.as_str())
            .bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str())
            .bind(label(&action)?)
            .bind(revision.at_ms)
            .bind(json(&revision).map_err(err)?)
            .bind(entry.id.as_str())
            .bind(proposal.execution_id.as_str())
            .bind(sequence.clone())
            .bind(status)
            .bind(entry.id.as_str()),
        );
    }
    Ok(out)
}

impl<D: D1> D1Catalog<D> {
    /// Apply one derived row of this tenant's execution. `step_runs` are
    /// the run's step-run IDs to index (new ones are added; an ID stays
    /// with the run that first claimed it).
    /// A deleted run's last derived row: only its memory proposals apply
    /// (idempotent by execution and proposal sequence, as always).
    async fn apply_proposals_only(&self, update: &DerivedUpdate) -> Result<(), StoreError> {
        let mut statements = Vec::new();
        for proposal in &update.memory {
            statements.extend(proposal_statements(&self.scope, proposal)?);
        }
        if statements.is_empty() {
            return Ok(());
        }
        self.db.batch(statements).await.map(|_| ()).map_err(err)
    }

    pub async fn apply_derived(
        &self,
        update: &DerivedUpdate,
        step_runs: &[StepRunId],
    ) -> Result<Applied, StoreError> {
        let id = &update.summary.execution_id;
        let key = id.as_str();
        let mut steps = BTreeSet::new();
        if update
            .assignments
            .iter()
            .any(|row| &row.execution_id != id || !steps.insert(&row.step_run_id))
        {
            return Err(StoreError::InvalidBatch("catalog.invalid_assignments"));
        }
        let reads = self
            .db
            .batch(vec![
                prepare("SELECT 1 AS dead FROM tombstone WHERE execution_id = ?").bind(key),
                prepare("SELECT org_id, project_id FROM execution_owner WHERE execution_id = ?")
                    .bind(key),
                prepare("SELECT sequence, created_at_ms FROM run_summary WHERE execution_id = ?")
                    .bind(key),
            ])
            .await
            .map_err(err)?;
        if !reads[0].rows.is_empty() {
            // A deleted run: its rows are gone for good, but its memory
            // proposals are the project's and stay (gate fix, as native).
            self.apply_proposals_only(update).await?;
            return Ok(Applied::default());
        }
        if let Some(owner) = reads[1].rows.first()
            && (owner.text(0).map_err(err)? != self.org()
                || owner.text(1).map_err(err)? != self.project())
        {
            return Err(StoreError::InvalidBatch("catalog.execution_scope_conflict"));
        }
        let prior = reads[2]
            .rows
            .first()
            .map(|row| {
                Ok::<_, StoreError>((row.unsigned(0).map_err(err)?, row.integer(1).map_err(err)?))
            })
            .transpose()?;
        let fresh = prior.is_none_or(|(sequence, _)| sequence < update.summary.sequence);
        let mut statements = vec![
            // A retention deletion racing this apply wins.
            crate::sql::guard(
                "recorded",
                "NOT EXISTS (SELECT 1 FROM tombstone WHERE execution_id = ?)",
            )
            .bind(key),
        ];
        statements.extend(claim_owner(&self.scope, id));
        for proposal in &update.memory {
            statements.extend(proposal_statements(&self.scope, proposal)?);
        }
        if !step_runs.is_empty() {
            let ids: Vec<&str> = step_runs.iter().map(StepRunId::as_str).collect();
            statements.push(
                prepare(
                    "INSERT INTO step_run_index (step_run_id, execution_id, org_id, project_id) \
                     SELECT value, ?, ?, ? FROM json_each(?) WHERE true \
                     ON CONFLICT (step_run_id) DO NOTHING",
                )
                .bind(key)
                .bind(self.org())
                .bind(self.project())
                .bind(json(&ids).map_err(err)?),
            );
        }
        if fresh {
            let mut summary = update.summary.clone();
            if let Some((_, created)) = prior {
                // Only the start stamps the creation time.
                summary.created_at_ms = created;
            }
            // Gate fix D4: another drain may have applied a newer row since
            // the read above; the inbox follows only a summary still older
            // than this one (the summary upsert comes last in the batch).
            let sequence = uint(update.summary.sequence).map_err(err)?;
            let older = "NOT EXISTS (SELECT 1 FROM run_summary WHERE execution_id = ? \
                         AND sequence >= ?)";
            statements.push(
                prepare(format!(
                    "DELETE FROM assignment WHERE execution_id = ? AND {older}"
                ))
                .bind(key)
                .bind(key)
                .bind(sequence.clone()),
            );
            for (ordinal, row) in update.assignments.iter().enumerate() {
                statements.push(fresh_assignment(
                    assignment_statement(key, ordinal, row)?,
                    key,
                    sequence.clone(),
                ));
            }
            // ARC2 on D1 (CF5): a commit past the revision the run's file
            // lists makes the file stale. The row reaches D1 through the
            // outbox, so the mark is late by at most the D56 bound, never
            // lost; a lagging row the file already covers marks nothing.
            statements.push(
                prepare(
                    "UPDATE archive SET stale = 1, stale_marks = stale_marks + 1 \
                     WHERE (org_id, project_id, runbook_id, day) IN (SELECT m.org_id, \
                     m.project_id, m.runbook_id, m.day FROM archive_member m \
                     WHERE m.execution_id = ? AND NOT EXISTS (SELECT 1 FROM archive_run r \
                       WHERE r.execution_id = m.execution_id \
                       AND json_extract(r.body, '$.revision') >= ?))",
                )
                .bind(key)
                .bind(crate::sql::uint(update.summary.sequence).map_err(err)?),
            );
            statements.push(summary_statement(&summary)?);
        }
        match self.db.batch(statements).await {
            Ok(_) => {}
            Err(D1Error::Guard(code)) if code == "recorded" => {
                self.apply_proposals_only(update).await?;
                return Ok(Applied::default());
            }
            Err(error) => return Err(err(error)),
        }
        let promoted = if update.release_concurrency {
            self.release(id).await?
        } else {
            vec![]
        };
        Ok(Applied {
            summary: fresh,
            promoted,
        })
    }
}
