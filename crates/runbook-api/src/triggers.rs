//! LANG9b: the schedule pass. Each host runs it about once a minute: for
//! every runbook's latest published version, each schedule whose latest
//! tick falls inside its window starts one run. The start's idempotency key
//! is (runbook, schedule, tick), so overlapping passes, several replicas or
//! a retried pass start each tick once, without a leader.
//!
//! [`due_schedules`] finds the ticks to start; the host starts them
//! ([`start_due`] natively; on Cloudflare each in its run's own object).
use crate::backends::Backends;
use crate::executions::{ExecutionBackend, StartIdentity, StartInvocation};
use runbook_core::{StoreError, catalog::TenantScope, directory_operations::DirectoryOperations};
use runbook_model::{Data, ExecutionId, RunbookId, StartRequest, VersionSelector};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::num::NonZeroU32;

/// How late a pass may still start a tick when the schedule sets no
/// `catch_up`: a few passes' worth, for a slow or restarted host.
pub const DEFAULT_WINDOW_MS: i64 = 5 * 60_000;

/// One tick to start: everything but the new run's ID and project values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DueStart {
    pub org_id: runbook_model::OrganizationId,
    pub project_id: runbook_model::ProjectId,
    pub runbook_id: RunbookId,
    pub number: NonZeroU32,
    pub schedule: String,
    pub tick_ms: i64,
    pub inputs: Data,
    pub key: String,
}
impl DueStart {
    pub fn scope(&self) -> TenantScope {
        TenantScope {
            org_id: self.org_id.clone(),
            project_id: self.project_id.clone(),
        }
    }
    pub fn identity(&self) -> StartIdentity {
        StartIdentity {
            request_hash: Sha256::digest(self.key.as_bytes()).into(),
            key: self.key.clone(),
            principal: format!("schedule:{}", self.schedule),
        }
    }
    /// The start of this tick as run `execution_id`.
    pub fn invocation(
        &self,
        execution_id: ExecutionId,
        origin: &str,
        project_vars: Data,
    ) -> StartInvocation {
        StartInvocation {
            url: format!(
                "{}/v1/executions/{execution_id}",
                origin.trim_end_matches('/')
            ),
            execution_id,
            request: StartRequest {
                runbook: self.runbook_id.to_string(),
                version: Some(VersionSelector::Number(self.number)),
                inputs: self.inputs.clone(),
                fields: Data::new(),
                title: None,
                tags: vec![],
            },
            identity: self.identity(),
            project_vars,
        }
    }
}

/// What a host sends to the place a due start runs (on Cloudflare, the new
/// run's own object).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduledStart {
    pub start: DueStart,
    pub origin: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SchedulePass {
    /// Published versions with at least one schedule.
    pub scheduled: usize,
    /// Runs started this pass.
    pub started: usize,
    /// Failures, by runbook and code. A failed start is retried next pass
    /// while its tick is inside the window.
    pub failed: Vec<(String, &'static str)>,
}

/// The ticks due at `now_ms` that haven't started yet.
pub async fn due_schedules<B: Backends>(
    backends: &B,
    now_ms: i64,
    pass: &mut SchedulePass,
) -> Result<Vec<DueStart>, StoreError> {
    let executions = backends.executions();
    let mut due = vec![];
    for version in backends.published_versions().await? {
        let store = backends.execution(version.scope.clone());
        let triggers =
            match runbook_core::triggers::triggers_of(&store, version.snapshot_digest).await {
                Ok(triggers) => triggers,
                Err(_) => {
                    pass.failed
                        .push((version.runbook_id.to_string(), "trigger.snapshot_invalid"));
                    continue;
                }
            };
        if triggers.schedule.is_empty() {
            continue;
        }
        pass.scheduled += 1;
        for schedule in &triggers.schedule {
            let (Some(cron), Some(tz)) = (
                runbook_spec::cron::Cron::parse(&schedule.cron),
                runbook_spec::cron::time_zone(&schedule.timezone),
            ) else {
                continue;
            };
            let window = schedule.catch_up.map_or(DEFAULT_WINDOW_MS, |d| {
                i64::try_from(d.millis)
                    .unwrap_or(i64::MAX)
                    .max(DEFAULT_WINDOW_MS)
            });
            let Some(tick_ms) = cron.last_tick(tz, now_ms, window) else {
                continue;
            };
            let start = DueStart {
                org_id: version.scope.org_id.clone(),
                project_id: version.scope.project_id.clone(),
                runbook_id: version.runbook_id.clone(),
                number: version.number,
                schedule: schedule.id.clone(),
                tick_ms,
                inputs: schedule.inputs.clone().into_iter().collect(),
                key: runbook_core::triggers::start_key(
                    "schedule",
                    &version.runbook_id,
                    &schedule.id,
                    &tick_ms.to_string(),
                ),
            };
            match executions
                .find_replay(&version.scope, &start.identity())
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => due.push(start),
                Err(error) => pass
                    .failed
                    .push((version.runbook_id.to_string(), error.code)),
            }
        }
    }
    Ok(due)
}

/// The project values a due start runs with.
pub async fn project_vars<B: Backends>(backends: &B, start: &DueStart) -> Result<Data, StoreError> {
    backends
        .directory()
        .project_variables(&start.org_id, &start.project_id)
        .await
        .map(|v| v.variables.values)
        .map_err(|_| StoreError::Backend("storage.unavailable"))
}

/// One pass where a start can run anywhere (the native server). `mint`
/// gives each new run's ID; `origin` is the public URL runs link back to.
pub async fn schedule_pass<B: Backends>(
    backends: &B,
    origin: &str,
    now_ms: i64,
    mut mint: impl FnMut() -> Option<String>,
) -> Result<SchedulePass, StoreError> {
    let mut pass = SchedulePass::default();
    let executions = backends.executions();
    for start in due_schedules(backends, now_ms, &mut pass).await? {
        let Some(execution_id) = mint().and_then(|id| format!("exe_{id}").parse().ok()) else {
            return Err(StoreError::Backend("server.entropy"));
        };
        let vars = match project_vars(backends, &start).await {
            Ok(vars) => vars,
            Err(_) => {
                pass.failed
                    .push((start.runbook_id.to_string(), "storage.unavailable"));
                continue;
            }
        };
        match executions
            .start(&start.scope(), start.invocation(execution_id, origin, vars))
            .await
        {
            Ok(_) => pass.started += 1,
            Err(error) => pass.failed.push((start.runbook_id.to_string(), error.code)),
        }
    }
    Ok(pass)
}
