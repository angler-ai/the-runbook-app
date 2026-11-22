//! R0-4: the runner's ready listing (`runbook.runner_ready_steps`): one row
//! per ready step aimed at a runner agent, kept in the execution commit so
//! it never disagrees with the run's projection.
use crate::{
    PostgresRunners,
    rows::{db, int},
};
use runbook_core::{ExecutionProjection, StoreError, catalog::TenantScope, runners::RunnerError};
use runbook_model::*;

pub(crate) async fn sync(
    c: &tokio_postgres::Client,
    scope: &TenantScope,
    execution: &ExecutionId,
    projection: &ExecutionProjection,
) -> Result<(), StoreError> {
    c.execute(
        "DELETE FROM runbook.runner_ready_steps WHERE execution_id = $1",
        &[&execution.as_str()],
    )
    .await
    .map_err(db)?;
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
            .map_err(|_| StoreError::Backend("postgres.encode_failed"))?;
        c.execute(
            "INSERT INTO runbook.runner_ready_steps (step_run_id, execution_id, org_id, project_id, \
             agent_id, ready_at_ms, max_cost_micro_usd, decision) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            &[
                &step.step_run_id.as_str(),
                &execution.as_str(),
                &scope.org_id.as_str(),
                &scope.project_id.as_str(),
                &runner.agent_id,
                &ready_at,
                &runner
                    .max_cost_usd
                    .map(|usd| (usd * 1.0e6).round() as i64),
                &decision,
            ],
        )
        .await
        .map_err(db)?;
    }
    Ok(())
}

impl PostgresRunners {
    /// Ready steps of the install's agents in one group, oldest first.
    pub(crate) async fn ready_steps_query(
        &self,
        install: &RunnerInstallId,
        group: &RunnerGroupId,
        project: Option<&ProjectId>,
        after: Option<(i64, &StepRunId)>,
        limit: u64,
        decision_since: Option<i64>,
    ) -> Result<Vec<ReadyStepView>, RunnerError> {
        let conn = self.conn().await?;
        let rows = conn
            .client()
            .query(
                "SELECT r.step_run_id, r.execution_id, r.project_id, r.agent_id, r.ready_at_ms, r.max_cost_micro_usd, r.decision \
                 FROM runbook.runner_ready_steps r \
                 JOIN runbook.runner_agents a ON a.org_id = r.org_id AND a.agent_id = r.agent_id \
                 WHERE a.install_id = $1 AND a.group_id = $2 AND a.project_id = r.project_id \
                 AND ($3::text IS NULL OR r.project_id = $3) \
                 AND (r.decision IS NULL OR (r.ready_at_ms <= extract(epoch from statement_timestamp()) * 1000
                    AND EXISTS (SELECT 1 FROM runbook.runner_runners worker WHERE worker.install_id = $1
                        AND worker.group_id = $2 AND worker.decision_v1 AND worker.last_heartbeat_at_ms > $7::bigint))) \
                 AND ($4::bigint IS NULL OR (r.ready_at_ms, r.step_run_id) > ($4, $5::text)) \
                 AND EXISTS (SELECT 1 FROM runbook.runner_group_projects gp \
                 WHERE gp.group_id = a.group_id AND gp.project_id = r.project_id) \
                 ORDER BY r.ready_at_ms, r.step_run_id LIMIT $6",
                &[
                    &install.as_str(),
                    &group.as_str(),
                    &project.map(ProjectId::as_str),
                    &after.map(|(at, _)| at),
                    &after.map(|(_, id)| id.as_str()),
                    &int(limit).map_err(RunnerError::Store)?,
                    &decision_since,
                ],
            )
            .await
            .map_err(|e| RunnerError::Store(db(e)))?;
        let decode = |_| RunnerError::Store(StoreError::Backend("postgres.decode_failed"));
        rows.iter()
            .map(|row| {
                let decision = row
                    .get::<_, Option<&str>>(6)
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|_| {
                        RunnerError::Store(StoreError::Backend("postgres.decode_failed"))
                    })?;
                Ok(ReadyStepView {
                    kind: if decision.is_some() {
                        RunnerWorkKind::Decision
                    } else {
                        RunnerWorkKind::Step
                    },
                    decision,
                    step_run_id: row.get::<_, &str>(0).parse().map_err(decode)?,
                    execution_id: row.get::<_, &str>(1).parse().map_err(decode)?,
                    project_id: row.get::<_, &str>(2).parse().map_err(decode)?,
                    agent_id: row.get(3),
                    ready_at: row.get(4),
                    max_cost_usd: row
                        .get::<_, Option<i64>>(5)
                        .map(|micros| micros as f64 / 1.0e6),
                })
            })
            .collect()
    }
}
