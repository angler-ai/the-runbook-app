//! The control shard's runner ready index. The execution projection in its
//! Durable Object is authoritative. Replayed outbox work can never replace
//! a newer projection or restore a purged execution.
use crate::{D1, prepare, store_error};
use runbook_core::{ExecutionProjection, StoreError, catalog::TenantScope};
use runbook_model::ExecutionId;

pub async fn sync(
    db: &impl D1,
    scope: &TenantScope,
    projection: &ExecutionProjection,
) -> Result<(), StoreError> {
    if !projection.steps.iter().any(|step| step.runner.is_some()) {
        return Ok(());
    }
    let id = projection.execution_id.as_str();
    let sequence = i64::try_from(projection.sequence)
        .map_err(|_| StoreError::Backend("d1.runner_ready_sequence"))?;
    let can_replace = "NOT EXISTS (SELECT 1 FROM runner_ready_heads WHERE execution_id = ?1
                      AND (sequence IS NULL OR sequence >= ?2))";
    let mut statements = vec![
        prepare(format!(
            "DELETE FROM runner_ready_steps WHERE execution_id = ?1 AND {can_replace}"
        ))
        .bind(id)
        .bind(sequence),
    ];
    for step in &projection.steps {
        let Some(runner) = &step.runner else { continue };
        let Some(ready_at) = runner.indexed_at(step.status) else {
            continue;
        };
        let decision = runner
            .decision
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| StoreError::Backend("d1.encode_failed"))?;
        statements.push(prepare(format!(
            "INSERT INTO runner_ready_steps
                (execution_id, step_run_id, org_id, project_id, agent_id, ready_at_ms, max_cost_micro_usd, decision)
             SELECT ?1, ?3, ?4, ?5, ?6, ?7, ?8, ?9 WHERE {can_replace}"
        )).bind(id).bind(sequence).bind(step.step_run_id.as_str()).bind(scope.org_id.as_str())
            .bind(scope.project_id.as_str()).bind(&runner.agent_id).bind(ready_at)
            .bind(runner.max_cost_usd.map(|usd| (usd * 1.0e6).round() as i64))
            .bind(decision.as_deref()));
    }
    statements.push(
        prepare(
            "INSERT INTO runner_ready_heads (execution_id, sequence) VALUES (?, ?)
         ON CONFLICT (execution_id) DO UPDATE SET sequence = excluded.sequence
         WHERE runner_ready_heads.sequence < excluded.sequence",
        )
        .bind(id)
        .bind(sequence),
    );
    db.batch(statements).await.map(|_| ()).map_err(store_error)
}

pub async fn forget(db: &impl D1, execution: &ExecutionId) -> Result<(), StoreError> {
    db.batch(vec![
        prepare(
            "INSERT INTO runner_ready_heads (execution_id, sequence) VALUES (?, NULL)
                 ON CONFLICT (execution_id) DO UPDATE SET sequence = NULL",
        )
        .bind(execution.as_str()),
        prepare("DELETE FROM runner_ready_steps WHERE execution_id = ?").bind(execution.as_str()),
    ])
    .await
    .map(|_| ())
    .map_err(store_error)
}
