//! ARC1–ARC3 on this edition (CF5): the archive index on D1, files and
//! retained uploads in R2 (`archive_index.rs`), live runs in their objects
//! (reached through the object protocol from the api Worker), and
//! retention's deletion through `RunStore` (D1 commit point, then the
//! object's purge).
//!
//! - [`CfArchives`] serves the archive routes over the composed store.
//! - [`CfArchiveHost`] is the daily Cron pass's host (`cron::archive_pass`):
//!   every tenant with runs or files, each with the retention policy the
//!   Worker's vars set (D54 defaults otherwise), and reports rendered as
//!   the report route renders them.
use crate::cloudflare::{CfBackends, CfExecution, CfHost};
use runbook_api::{backends::Backends, executions::ExecutionBackend};
use runbook_core::{
    StoreError,
    archives::job::{ReportSource, RetentionPolicy},
    catalog::TenantScope,
};
use runbook_model::{ExecutionId, OrganizationId, ProjectId, RunbookId};
use runbook_store_d1::{
    D1, D1Directory,
    sql::{Columns, prepare},
};

/// D54's self-hosted defaults: live detail 90 days; files and summaries
/// kept. `RUNBOOK_RETENTION_LIVE_DAYS`, `_ARCHIVE_DAYS` and
/// `_SUMMARY_DAYS` (Worker vars, `off` for none) override them, checked
/// like the native configuration.
pub fn retention_policy(
    var: impl Fn(&str) -> Option<String>,
) -> Result<RetentionPolicy, &'static str> {
    let days = |name: &str, default: Option<u32>| -> Result<Option<u32>, &'static str> {
        match var(name)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
        {
            None => Ok(default),
            Some(v) if v == "off" => Ok(None),
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|d| *d > 0)
                .map(Some)
                .ok_or("config.invalid_retention_days"),
        }
    };
    let policy = RetentionPolicy {
        live_days: days("RUNBOOK_RETENTION_LIVE_DAYS", Some(90))?,
        archive_days: days("RUNBOOK_RETENTION_ARCHIVE_DAYS", None)?,
        summary_days: days("RUNBOOK_RETENTION_SUMMARY_DAYS", None)?,
    };
    policy.check()?;
    Ok(policy)
}

/// The archive routes' backend.
pub struct CfArchives<H: CfHost> {
    backends: CfBackends<H>,
}
impl<H: CfHost> Clone for CfArchives<H> {
    fn clone(&self) -> Self {
        Self {
            backends: self.backends.clone(),
        }
    }
}
impl<H: CfHost> CfArchives<H> {
    pub fn new(backends: CfBackends<H>) -> Self {
        Self { backends }
    }
}
impl<H: CfHost> runbook_api::archives::ArchiveBackend for CfArchives<H> {
    type Store = CfExecution<H>;
    async fn runbook_project(
        &self,
        org: &OrganizationId,
        runbook_id: &RunbookId,
    ) -> Result<Option<ProjectId>, StoreError> {
        runbook_store_d1::D1Catalog::new(
            self.backends.for_org(org),
            TenantScope {
                org_id: org.clone(),
                project_id: runbook_api::backends::zero_project(),
            },
        )
        .runbook_project(org, runbook_id)
        .await
    }
    async fn archived_run_project(
        &self,
        org: &OrganizationId,
        execution_id: &ExecutionId,
    ) -> Result<Option<ProjectId>, StoreError> {
        self.backends
            .for_org(org)
            .all(
                prepare("SELECT project_id FROM archive_run WHERE execution_id = ? AND org_id = ?")
                    .bind(execution_id.as_str())
                    .bind(org.as_str()),
            )
            .await
            .map_err(|_| StoreError::Backend("d1.archive_run"))?
            .first()
            .map(|row| row.parse(0).map_err(|_| StoreError::Backend("d1.decode")))
            .transpose()
    }
    fn store(&self, scope: &TenantScope) -> Self::Store {
        self.backends.execution(scope.clone())
    }
    fn policy(&self, _project: &ProjectId) -> RetentionPolicy {
        self.backends.host().retention()
    }
}

/// Each run's `report.md`, rendered as the report route renders it.
pub struct CfReports<H: CfHost>(pub CfBackends<H>);
impl<H: CfHost> ReportSource for CfReports<H> {
    async fn report_markdown(
        &self,
        scope: &TenantScope,
        execution_id: &ExecutionId,
    ) -> Result<String, StoreError> {
        self.0
            .executions()
            .report_markdown(scope, execution_id)
            .await
            .map_err(|e| match e.status {
                404 => StoreError::NotFound,
                _ => StoreError::Backend("archive.report_unavailable"),
            })
    }
}

/// The daily archive and retention pass's host (CF4-7, CF5).
pub struct CfArchiveHost<H: CfHost> {
    backends: CfBackends<H>,
    directory: D1Directory<H::D1>,
    reports: CfReports<H>,
}
impl<H: CfHost> CfArchiveHost<H> {
    pub fn new(backends: CfBackends<H>) -> Self {
        Self {
            directory: D1Directory::new(backends.control()),
            reports: CfReports(backends.clone()),
            backends,
        }
    }
}
impl<H: CfHost> crate::cron::ArchiveHost for CfArchiveHost<H> {
    type Store = CfExecution<H>;
    type Directory = D1Directory<H::D1>;
    type Reports = CfReports<H>;
    /// Every tenant with a live run or anything archived, on every shard
    /// (each shard's own organizations only, CF11).
    async fn tenants(&self) -> Result<Vec<TenantScope>, StoreError> {
        let shards = self.backends.host().shards();
        let mut out = Vec::new();
        for (binding, db) in shards.data_shards() {
            let rows = db
                .all(prepare(
                    "SELECT org_id, project_id FROM execution_owner \
                     UNION SELECT org_id, project_id FROM archive \
                     UNION SELECT org_id, project_id FROM archive_run \
                     UNION SELECT org_id, project_id FROM archive_version \
                     ORDER BY org_id, project_id",
                ))
                .await
                .map_err(|_| StoreError::Backend("d1.tenants"))?;
            for row in &rows {
                let scope = TenantScope {
                    org_id: row.parse(0).map_err(|_| StoreError::Backend("d1.decode"))?,
                    project_id: row.parse(1).map_err(|_| StoreError::Backend("d1.decode"))?,
                };
                if shards.owns(&binding, &scope.org_id) {
                    out.push(scope);
                }
            }
        }
        out.sort_by(|a, b| {
            (a.org_id.as_str(), a.project_id.as_str())
                .cmp(&(b.org_id.as_str(), b.project_id.as_str()))
        });
        Ok(out)
    }
    fn store(&self, scope: &TenantScope) -> Self::Store {
        self.backends.execution(scope.clone())
    }
    fn directory(&self) -> &Self::Directory {
        &self.directory
    }
    fn reports(&self) -> &Self::Reports {
        &self.reports
    }
    fn policy(&self, _scope: &TenantScope) -> RetentionPolicy {
        self.backends.host().retention()
    }
    fn now_ms(&self) -> i64 {
        self.backends.host().now_ms() + self.backends.host().retention_clock_offset_ms()
    }
}

/// LANG9b: every non-archived runbook's latest published version, on
/// every shard (each shard's own organizations only), for the schedule
/// pass.
pub async fn published_versions<H: CfHost>(
    backends: &CfBackends<H>,
) -> Result<Vec<runbook_core::triggers::PublishedVersion>, StoreError> {
    let shards = backends.host().shards();
    let mut out = Vec::new();
    let decode = |_| StoreError::Backend("d1.decode");
    for (binding, db) in shards.data_shards() {
        let rows = db
            .all(prepare(
                "SELECT v.org_id, v.project_id, v.runbook_id, v.version_id, v.number, \
                        v.snapshot_digest \
                 FROM runbook_version v JOIN runbook r ON r.runbook_id = v.runbook_id \
                 WHERE r.archived = 0 AND v.number = ( \
                     SELECT max(w.number) FROM runbook_version w \
                     WHERE w.runbook_id = v.runbook_id) \
                 ORDER BY v.org_id, v.project_id, v.runbook_id",
            ))
            .await
            .map_err(|_| StoreError::Backend("d1.published_versions"))?;
        for row in &rows {
            let scope = TenantScope {
                org_id: row.parse(0).map_err(decode)?,
                project_id: row.parse(1).map_err(decode)?,
            };
            if !shards.owns(&binding, &scope.org_id) {
                continue;
            }
            out.push(runbook_core::triggers::PublishedVersion {
                scope,
                runbook_id: row.parse(2).map_err(decode)?,
                version_id: row.parse(3).map_err(decode)?,
                number: u32::try_from(row.integer(4).map_err(decode)?)
                    .ok()
                    .and_then(std::num::NonZeroU32::new)
                    .ok_or(StoreError::Backend("d1.invalid_version_number"))?,
                snapshot_digest: row.digest(5).map_err(decode)?,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::retention_policy;

    #[test]
    fn the_retention_policy_reads_d54_defaults_and_vars() {
        let none = |_: &str| None;
        let policy = retention_policy(none).unwrap();
        assert_eq!(
            (policy.live_days, policy.archive_days, policy.summary_days),
            (Some(90), None, None)
        );
        let set = |name: &str| match name {
            "RUNBOOK_RETENTION_LIVE_DAYS" => Some("30".to_owned()),
            "RUNBOOK_RETENTION_ARCHIVE_DAYS" => Some("365".to_owned()),
            "RUNBOOK_RETENTION_SUMMARY_DAYS" => Some("400".to_owned()),
            _ => None,
        };
        let policy = retention_policy(set).unwrap();
        assert_eq!(
            (policy.live_days, policy.archive_days, policy.summary_days),
            (Some(30), Some(365), Some(400))
        );
        let zero = |name: &str| (name == "RUNBOOK_RETENTION_LIVE_DAYS").then(|| "0".to_owned());
        assert!(retention_policy(zero).is_err());
        // Files may not expire before the live copies they back up.
        let early =
            |name: &str| (name == "RUNBOOK_RETENTION_ARCHIVE_DAYS").then(|| "30".to_owned());
        assert!(retention_policy(early).is_err());
    }
}
