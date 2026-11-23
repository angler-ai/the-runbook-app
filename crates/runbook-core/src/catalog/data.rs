//! Pure staged operations. Adapters apply these to a cloned database and publish
//! the clone only after the entire transaction succeeds; never publish partial writes.
use super::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone)]
pub(crate) struct RunbookState {
    scope: TenantScope,
    record: RunbookRecord,
    draft_source: String,
    draft_revision: u64,
    versions: Vec<VersionRecord>,
}
#[derive(Clone, Default)]
pub(crate) struct CatalogData {
    drafts: BTreeMap<(TenantScope, String), DraftRevision>,
    runbooks: BTreeMap<runbook_model::RunbookId, RunbookState>,
    skills: Vec<(TenantScope, SkillVersion)>,
    owners: BTreeMap<ExecutionId, TenantScope>,
    runs: BTreeMap<ExecutionId, RunSummary>,
    assignments: BTreeMap<ExecutionId, Vec<Assignment>>,
    admissions: BTreeMap<ExecutionId, (AdmitRequest, Admission)>,
    idempotency: BTreeMap<(TenantScope, String), AdmitRequest>,
    pools: BTreeMap<(TenantScope, String), Pool>,
}
#[derive(Clone)]
struct Pool {
    limit: u32,
    active: u32,
    queue: VecDeque<ExecutionId>,
}
impl CatalogData {
    pub(crate) fn owned_record_counts(&self, id: &ExecutionId) -> BTreeMap<String, usize> {
        [
            ("catalog_owner", usize::from(self.owners.contains_key(id))),
            ("run_summary", usize::from(self.runs.contains_key(id))),
            ("assignment", self.assignments.get(id).map_or(0, Vec::len)),
            ("admission", usize::from(self.admissions.contains_key(id))),
            (
                "start_idempotency",
                self.idempotency
                    .values()
                    .filter(|r| &r.execution_id == id)
                    .count(),
            ),
            (
                "queue_entry",
                self.pools
                    .values()
                    .map(|p| p.queue.iter().filter(|entry| *entry == id).count())
                    .sum(),
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v))
        .collect()
    }

    pub(crate) fn create_runbook(
        &mut self,
        scope: &TenantScope,
        record: RunbookRecord,
        source: String,
    ) -> Result<runbook_model::DraftView, StoreError> {
        if runbook_model::parse_runbook_ref(&record.slug).is_err() || record.slug.contains('@') {
            return Err(StoreError::InvalidBatch("catalog.invalid_slug"));
        }
        if self.runbooks.contains_key(&record.runbook_id) {
            return Err(StoreError::InvalidBatch("catalog.runbook_exists"));
        }
        if self
            .runbooks
            .values()
            .any(|r| &r.scope == scope && r.record.slug == record.slug)
        {
            return Err(StoreError::InvalidBatch("catalog.runbook_slug_conflict"));
        }
        let view = crate::publish::draft_view(record.runbook_id.clone(), source.clone(), 1);
        self.runbooks.insert(
            record.runbook_id.clone(),
            RunbookState {
                scope: scope.clone(),
                record,
                draft_source: source,
                draft_revision: 1,
                versions: vec![],
            },
        );
        Ok(view)
    }
    fn runbook(&self, scope: &TenantScope, id: &runbook_model::RunbookId) -> Option<&RunbookState> {
        self.runbooks.get(id).filter(|r| &r.scope == scope)
    }
    fn runbook_mut(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
    ) -> Option<&mut RunbookState> {
        self.runbooks.get_mut(id).filter(|r| &r.scope == scope)
    }
    pub(crate) fn read_runbook(
        &self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
    ) -> Option<RunbookRecord> {
        self.runbook(scope, id).map(|r| r.record.clone())
    }
    pub(crate) fn get_draft(
        &self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
    ) -> Option<runbook_model::DraftView> {
        self.runbook(scope, id).map(|r| {
            crate::publish::draft_view(id.clone(), r.draft_source.clone(), r.draft_revision)
        })
    }
    pub(crate) fn save_draft(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
        expected: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        let runbook = self.runbook_mut(scope, id).ok_or(StoreError::NotFound)?;
        if runbook.record.origin == RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.git_managed"));
        }
        if runbook.draft_revision != expected {
            return Ok(CommitResult::Conflict);
        }
        let next = expected
            .checked_add(1)
            .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?;
        runbook.draft_source = source;
        runbook.draft_revision = next;
        Ok(CommitResult::Committed(next))
    }
    fn stored_version(
        prepared: crate::publish::PreparedPublication,
        number: Option<std::num::NonZeroU32>,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> VersionRecord {
        VersionRecord {
            version_id: prepared.version_id,
            number,
            branch,
            commit_sha,
            semantic_hash: prepared.semantic_hash,
            snapshot_digest: prepared.snapshot_digest,
            snapshot_size: prepared.snapshot_size,
            source: prepared.source,
            pins: prepared.pins,
        }
    }
    pub(crate) fn publish_revision(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
        expected: u64,
        prepared: crate::publish::PreparedPublication,
    ) -> Result<runbook_model::PublishedVersionView, StoreError> {
        let runbook = self.runbook_mut(scope, id).ok_or(StoreError::NotFound)?;
        if runbook.record.origin == RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.git_managed"));
        }
        if runbook.draft_revision != expected || runbook.draft_source != prepared.source {
            return Err(StoreError::InvalidBatch("catalog.draft_conflict"));
        }
        let latest = runbook
            .versions
            .iter()
            .filter(|v| v.number.is_some())
            .max_by_key(|v| v.number);
        if let Some(latest) = latest
            && latest.semantic_hash == prepared.semantic_hash
        {
            return Ok(runbook_model::PublishedVersionView {
                warnings: prepared.warnings.clone(),
                runbook_id: id.clone(),
                runbook_version_id: latest.version_id.clone(),
                runbook_version: latest.number.expect("published number"),
            });
        }
        let next = latest.map_or(1, |v| v.number.expect("published number").get() + 1);
        let number = std::num::NonZeroU32::new(next)
            .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
        let view = runbook_model::PublishedVersionView {
            warnings: prepared.warnings.clone(),
            runbook_id: id.clone(),
            runbook_version_id: prepared.version_id.clone(),
            runbook_version: number,
        };
        runbook
            .versions
            .push(Self::stored_version(prepared, Some(number), None, None));
        Ok(view)
    }
    pub(crate) fn publish_git(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
        prepared: crate::publish::PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<runbook_model::PublishedVersionView, StoreError> {
        let runbook = self.runbook_mut(scope, id).ok_or(StoreError::NotFound)?;
        runbook.record.origin = RunbookOrigin::Git;
        runbook.record.archived = false;
        if runbook.draft_source != prepared.source {
            runbook.draft_revision = runbook
                .draft_revision
                .checked_add(1)
                .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?;
            runbook.draft_source = prepared.source.clone();
        }
        let latest = runbook
            .versions
            .iter()
            .filter(|v| v.number.is_some())
            .max_by_key(|v| v.number);
        if let Some(latest) = latest
            && latest.semantic_hash == prepared.semantic_hash
        {
            return Ok(runbook_model::PublishedVersionView {
                warnings: prepared.warnings.clone(),
                runbook_id: id.clone(),
                runbook_version_id: latest.version_id.clone(),
                runbook_version: latest.number.expect("published number"),
            });
        }
        let next = latest.map_or(1, |v| v.number.expect("published number").get() + 1);
        let number = std::num::NonZeroU32::new(next)
            .ok_or(StoreError::InvalidBatch("catalog.version_overflow"))?;
        let view = runbook_model::PublishedVersionView {
            warnings: prepared.warnings.clone(),
            runbook_id: id.clone(),
            runbook_version_id: prepared.version_id.clone(),
            runbook_version: number,
        };
        runbook.versions.push(Self::stored_version(
            prepared,
            Some(number),
            branch,
            commit_sha,
        ));
        Ok(view)
    }
    pub(crate) fn archive_git(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
    ) -> Result<(), StoreError> {
        let runbook = self.runbook_mut(scope, id).ok_or(StoreError::NotFound)?;
        if runbook.record.origin != RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.not_git_managed"));
        }
        runbook.record.archived = true;
        Ok(())
    }
    pub(crate) fn detach_git(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
    ) -> Result<(), StoreError> {
        let runbook = self.runbook_mut(scope, id).ok_or(StoreError::NotFound)?;
        if runbook.record.origin != RunbookOrigin::Git {
            return Err(StoreError::InvalidBatch("runbook.not_git_managed"));
        }
        runbook.record.origin = RunbookOrigin::Ui;
        Ok(())
    }
    pub(crate) fn create_preview(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
        prepared: crate::publish::PreparedPublication,
        branch: Option<String>,
        commit_sha: Option<String>,
    ) -> Result<runbook_model::VersionView, StoreError> {
        let runbook = self.runbook_mut(scope, id).ok_or(StoreError::NotFound)?;
        if let Some(existing) = runbook
            .versions
            .iter()
            .find(|v| v.version_id == prepared.version_id)
        {
            return Ok(existing.view(id));
        }
        let record = Self::stored_version(prepared, None, branch, commit_sha);
        let view = record.view(id);
        runbook.versions.push(record);
        Ok(view)
    }
    pub(crate) fn resolve_version(
        &self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
        selector: Option<runbook_model::VersionSelector>,
    ) -> Result<runbook_model::VersionView, StoreError> {
        let runbook = self.runbook(scope, id).ok_or(StoreError::NotFound)?;
        let found = match selector {
            None => runbook
                .versions
                .iter()
                .filter(|v| v.number.is_some())
                .max_by_key(|v| v.number),
            Some(runbook_model::VersionSelector::Number(number)) => {
                runbook.versions.iter().find(|v| v.number == Some(number))
            }
            Some(runbook_model::VersionSelector::Id(version_id)) => {
                runbook.versions.iter().find(|v| v.version_id == version_id)
            }
        };
        found.map(|v| v.view(id)).ok_or(StoreError::NotFound)
    }
    pub(crate) fn version_record(
        &self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
        version_id: &runbook_model::VersionId,
    ) -> Option<VersionRecord> {
        self.runbook(scope, id).and_then(|runbook| {
            runbook
                .versions
                .iter()
                .find(|v| &v.version_id == version_id)
                .cloned()
        })
    }
    pub(crate) fn list_versions(
        &self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
    ) -> Result<Vec<runbook_model::PublishedVersionView>, StoreError> {
        let runbook = self.runbook(scope, id).ok_or(StoreError::NotFound)?;
        let mut published: Vec<_> = runbook
            .versions
            .iter()
            .filter_map(|v| {
                v.number.map(|number| runbook_model::PublishedVersionView {
                    warnings: Vec::new(),
                    runbook_id: id.clone(),
                    runbook_version_id: v.version_id.clone(),
                    runbook_version: number,
                })
            })
            .collect();
        published.sort_by_key(|v| v.runbook_version);
        Ok(published)
    }
    pub(crate) fn list_previews(
        &self,
        scope: &TenantScope,
        id: &runbook_model::RunbookId,
    ) -> Result<Vec<runbook_model::VersionView>, StoreError> {
        let runbook = self.runbook(scope, id).ok_or(StoreError::NotFound)?;
        let mut previews: Vec<_> = runbook
            .versions
            .iter()
            .filter(|v| v.number.is_none())
            .map(|v| v.view(id))
            .collect();
        previews.sort_by(|a, b| a.runbook_version_id.cmp(&b.runbook_version_id));
        Ok(previews)
    }

    pub(crate) fn record_skill_version(
        &mut self,
        scope: &TenantScope,
        version: SkillVersion,
    ) -> SkillVersion {
        if let Some((_, existing)) = self
            .skills
            .iter()
            .find(|(owner, v)| owner == scope && v.name == version.name && v.hash == version.hash)
        {
            return existing.clone();
        }
        self.skills.push((scope.clone(), version.clone()));
        version
    }
    pub(crate) fn skill_version(
        &self,
        scope: &TenantScope,
        hash: &[u8; 32],
    ) -> Option<SkillVersion> {
        self.skills
            .iter()
            .find(|(owner, v)| owner == scope && &v.hash == hash)
            .map(|(_, v)| v.clone())
    }
    pub(crate) fn latest_skills(&self, scope: &TenantScope) -> Vec<SkillVersion> {
        let mut latest: Vec<SkillVersion> = Vec::new();
        for (owner, version) in &self.skills {
            if owner != scope {
                continue;
            }
            match latest.iter_mut().find(|v| v.name == version.name) {
                Some(slot) => *slot = version.clone(),
                None => latest.push(version.clone()),
            }
        }
        latest.sort_by(|a, b| a.name.cmp(&b.name));
        latest
    }
    pub(crate) fn versions_pinning_skill(
        &self,
        scope: &TenantScope,
        hash: &[u8; 32],
    ) -> Vec<(runbook_model::RunbookId, VersionRecord)> {
        let mut pinned = Vec::new();
        for (id, runbook) in &self.runbooks {
            if &runbook.scope != scope {
                continue;
            }
            for version in &runbook.versions {
                if version.pins.skills.values().any(|pinned| pinned == hash) {
                    pinned.push((id.clone(), version.clone()));
                }
            }
        }
        pinned.sort_by(|a, b| (&a.0, &a.1.version_id).cmp(&(&b.0, &b.1.version_id)));
        pinned
    }

    /// Remove one queued waiter; admitted runs are untouched.
    pub(crate) fn cancel_queued(
        &mut self,
        scope: &TenantScope,
        id: &runbook_model::ExecutionId,
    ) -> bool {
        let Some((request, row)) = self.admissions.get(id) else {
            return false;
        };
        if row.state != AdmissionState::Queued || self.owners.get(id) != Some(scope) {
            return false;
        }
        let pool_key = (scope.clone(), request.key.clone());
        if let Some(pool) = self.pools.get_mut(&pool_key) {
            pool.queue.retain(|queued| queued != id);
        }
        self.admissions.remove(id);
        true
    }
    pub(crate) fn search_runs(
        &self,
        scope: &TenantScope,
        filters: crate::search::RunFilters,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<crate::search::RunPage, StoreError> {
        if limit == 0 {
            return Err(StoreError::InvalidBatch("query.invalid_limit"));
        }
        let scoped: Vec<RunSummary> = self
            .runs
            .values()
            .filter(|run| self.owners.get(&run.execution_id) == Some(scope))
            .cloned()
            .collect();
        let indexed: std::collections::BTreeSet<&String> = scoped
            .iter()
            .filter(|run| {
                filters
                    .runbook_id
                    .as_ref()
                    .is_none_or(|id| id == &run.runbook_id)
            })
            .flat_map(|run| run.fields.keys())
            .collect();
        if filters
            .fields
            .iter()
            .any(|f| !indexed.contains(&f.field().to_owned()))
        {
            return Err(StoreError::InvalidBatch("query.field_not_indexed"));
        }
        let after = cursor
            .map(|c| crate::search::cursor_from(scope, &filters, &c))
            .transpose()?;
        let mut runs: Vec<RunSummary> = scoped
            .into_iter()
            .filter(|run| crate::search::matches(run, &filters))
            .filter(|run| {
                after
                    .as_ref()
                    .is_none_or(|c| crate::search::is_after_cursor(run, c))
            })
            .collect();
        crate::search::sort_runs(&mut runs);
        let next_cursor = (runs.len() > limit)
            .then(|| crate::search::cursor_for(scope, &filters, &runs[limit - 1]));
        runs.truncate(limit);
        Ok(crate::search::RunPage { runs, next_cursor })
    }

    pub(crate) fn export_run(
        &self,
        scope: &TenantScope,
        id: &ExecutionId,
    ) -> Result<RunCatalogSlice, StoreError> {
        if self.owners.get(id).is_some_and(|owner| owner != scope) {
            return Err(StoreError::NotFound);
        }
        Ok(RunCatalogSlice {
            summary: self.runs.get(id).cloned(),
            assignments: self.assignments.get(id).cloned().unwrap_or_default(),
            admission: self.admissions.get(id).cloned(),
        })
    }

    pub(crate) fn delete_run(
        &mut self,
        scope: &TenantScope,
        id: &ExecutionId,
    ) -> Result<(), StoreError> {
        self.export_run(scope, id)?;
        if self
            .admissions
            .get(id)
            .is_some_and(|(_, row)| row.state != AdmissionState::Released)
        {
            return Err(StoreError::InvalidBatch("catalog.admission_not_released"));
        }
        if self
            .pools
            .iter()
            .any(|((owner, _), pool)| owner == scope && pool.queue.contains(id))
        {
            return Err(StoreError::InvalidBatch(
                "catalog.released_admission_queued",
            ));
        }
        self.runs.remove(id);
        self.assignments.remove(id);
        self.admissions.remove(id);
        self.idempotency
            .retain(|(owner, _), request| owner != scope || &request.execution_id != id);
        self.owners.remove(id);
        Ok(())
    }

    fn check_owner(&self, scope: &TenantScope, id: &ExecutionId) -> Result<(), StoreError> {
        if self.owners.get(id).is_some_and(|owner| owner != scope) {
            return Err(StoreError::InvalidBatch("catalog.execution_scope_conflict"));
        }
        Ok(())
    }
    pub(crate) fn claim_execution(
        &mut self,
        scope: &TenantScope,
        id: &ExecutionId,
    ) -> Result<(), StoreError> {
        self.check_owner(scope, id)?;
        self.owners.insert(id.clone(), scope.clone());
        Ok(())
    }
    pub(crate) fn read_draft(&self, scope: &TenantScope, key: &str) -> Option<DraftRevision> {
        self.drafts.get(&(scope.clone(), key.into())).cloned()
    }
    pub(crate) fn replace_draft(
        &mut self,
        scope: &TenantScope,
        key: &str,
        expected: u64,
        source: String,
    ) -> Result<CommitResult, StoreError> {
        if key.is_empty() {
            return Err(StoreError::InvalidBatch("catalog.empty_draft_key"));
        }
        let current = self
            .read_draft(scope, key)
            .map_or(0, |draft| draft.revision);
        if expected != current {
            return Ok(CommitResult::Conflict);
        }
        let revision = current
            .checked_add(1)
            .ok_or(StoreError::InvalidBatch("catalog.revision_overflow"))?;
        self.drafts.insert(
            (scope.clone(), key.into()),
            DraftRevision {
                key: key.into(),
                revision,
                source,
            },
        );
        Ok(CommitResult::Committed(revision))
    }
    pub(crate) fn list_runs(&self, scope: &TenantScope) -> Vec<RunSummary> {
        self.runs
            .values()
            .filter(|run| self.owners.get(&run.execution_id) == Some(scope))
            .cloned()
            .collect()
    }
    pub(crate) fn get_run(&self, scope: &TenantScope, id: &ExecutionId) -> Option<RunSummary> {
        (self.owners.get(id) == Some(scope))
            .then(|| self.runs.get(id).cloned())
            .flatten()
    }
    pub(crate) fn list_assignments(&self, scope: &TenantScope) -> Vec<Assignment> {
        self.assignments
            .iter()
            .filter(|(id, _)| self.owners.get(*id) == Some(scope))
            .flat_map(|(_, rows)| rows.clone())
            .collect()
    }
    pub(crate) fn admission(&self, scope: &TenantScope, id: &ExecutionId) -> Option<Admission> {
        if self.owners.get(id) != Some(scope) {
            return None;
        }
        self.admissions.get(id).map(|(_, row)| row.clone())
    }
    pub(crate) fn admit(
        &mut self,
        scope: &TenantScope,
        request: AdmitRequest,
        now: i64,
    ) -> Result<Admission, StoreError> {
        self.check_owner(scope, &request.execution_id)?;
        if request.key.is_empty() || request.idempotency_key.is_empty() || request.limit == 0 {
            return Err(StoreError::InvalidBatch("catalog.invalid_admission"));
        }
        let token = (scope.clone(), request.idempotency_key.clone());
        if let Some(prior) = self.idempotency.get(&token) {
            if prior != &request {
                return Err(StoreError::InvalidBatch("catalog.idempotency_conflict"));
            }
            return Ok(self.admissions[&request.execution_id].1.clone());
        }
        let row = self.admit_pooled(scope, request.clone(), now)?;
        self.idempotency.insert(token, request);
        Ok(row)
    }
    /// Pool admission without start-idempotency bookkeeping (start commits
    /// carry their own replay record). D25: the request limit replaces the
    /// pool limit; raising it promotes waiters first, in FIFO order.
    pub(crate) fn admit_pooled(
        &mut self,
        scope: &TenantScope,
        request: AdmitRequest,
        now: i64,
    ) -> Result<Admission, StoreError> {
        self.admit_pooled_at(scope, request, now, false)
    }
    /// Pooled admission; with `first`, a start that has to wait is queued
    /// ahead of every other waiter (LANG3 `cancel_previous`).
    pub(crate) fn admit_pooled_at(
        &mut self,
        scope: &TenantScope,
        request: AdmitRequest,
        now: i64,
        first: bool,
    ) -> Result<Admission, StoreError> {
        self.check_owner(scope, &request.execution_id)?;
        if request.key.is_empty() || request.limit == 0 {
            return Err(StoreError::InvalidBatch("catalog.invalid_admission"));
        }
        if self.admissions.contains_key(&request.execution_id) {
            return Err(StoreError::InvalidBatch("catalog.admission_conflict"));
        }
        let pool_key = (scope.clone(), request.key.clone());
        let (promoted, state) = {
            let pool = self.pools.entry(pool_key).or_insert(Pool {
                limit: request.limit,
                active: 0,
                queue: VecDeque::new(),
            });
            pool.limit = request.limit;
            let mut promoted = Vec::new();
            while crate::concurrency::promotable(pool.active, pool.limit) > 0 {
                let Some(next) = pool.queue.pop_front() else {
                    break;
                };
                pool.active += 1;
                promoted.push(next);
            }
            let state = if pool.active < pool.limit {
                pool.active += 1;
                AdmissionState::Admitted
            } else {
                if first {
                    pool.queue.push_front(request.execution_id.clone());
                } else {
                    pool.queue.push_back(request.execution_id.clone());
                }
                AdmissionState::Queued
            };
            (promoted, state)
        };
        for next in promoted {
            let row = &mut self
                .admissions
                .get_mut(&next)
                .expect("queued admission exists")
                .1;
            row.state = AdmissionState::Admitted;
            row.admitted_at_ms = Some(now);
        }
        let row = Admission {
            execution_id: request.execution_id.clone(),
            state,
            admitted_at_ms: (state == AdmissionState::Admitted).then_some(now),
        };
        self.owners
            .insert(request.execution_id.clone(), scope.clone());
        self.admissions
            .insert(request.execution_id.clone(), (request, row.clone()));
        Ok(row)
    }
    /// Release one admission (idempotent), then promote waiters while the
    /// pool is under its current limit. D25: a lowered limit admits no one
    /// new until active drops below it.
    pub(crate) fn release_admission(&mut self, scope: &TenantScope, id: &ExecutionId, now: i64) {
        let Some((request, row)) = self.admissions.get_mut(id) else {
            return;
        };
        let previous = row.state;
        if previous == AdmissionState::Released {
            return;
        }
        row.state = AdmissionState::Released;
        let pool = self
            .pools
            .get_mut(&(scope.clone(), request.key.clone()))
            .expect("admission pool exists");
        if previous == AdmissionState::Queued {
            pool.queue.retain(|queued| queued != id);
            return;
        }
        pool.active -= 1;
        while pool.active < pool.limit {
            let Some(next) = pool.queue.pop_front() else {
                break;
            };
            let promoted = &mut self
                .admissions
                .get_mut(&next)
                .expect("queued admission exists")
                .1;
            promoted.state = AdmissionState::Admitted;
            promoted.admitted_at_ms = Some(now);
            pool.active += 1;
        }
    }
    /// LANG3: the key's holders in admission order (oldest first, ties by
    /// execution ID), then its waiters in FIFO order.
    pub(crate) fn admission_pool(
        &self,
        scope: &TenantScope,
        key: &str,
    ) -> crate::concurrency::PoolView {
        use crate::concurrency::PoolHolder;
        let mut admitted: Vec<(Option<i64>, &ExecutionId)> = self
            .admissions
            .iter()
            .filter(|(id, (request, row))| {
                request.key == key
                    && row.state == AdmissionState::Admitted
                    && self.owners.get(*id) == Some(scope)
            })
            .map(|(id, (_, row))| (row.admitted_at_ms, id))
            .collect();
        admitted.sort();
        let mut holders: Vec<PoolHolder> = admitted
            .into_iter()
            .map(|(_, id)| PoolHolder {
                execution_id: id.clone(),
                state: AdmissionState::Admitted,
            })
            .collect();
        if let Some(pool) = self.pools.get(&(scope.clone(), key.to_owned())) {
            holders.extend(pool.queue.iter().map(|id| PoolHolder {
                execution_id: id.clone(),
                state: AdmissionState::Queued,
            }));
        }
        crate::concurrency::PoolView { holders }
    }
    pub(crate) fn apply_derived(
        &mut self,
        scope: &TenantScope,
        update: DerivedUpdate,
        now: i64,
    ) -> Result<(), StoreError> {
        let id = &update.summary.execution_id;
        self.check_owner(scope, id)?;
        if self
            .runs
            .get(id)
            .is_some_and(|prior| prior.sequence >= update.summary.sequence)
        {
            return Ok(());
        }
        let mut steps = BTreeSet::new();
        if update
            .assignments
            .iter()
            .any(|row| &row.execution_id != id || !steps.insert(&row.step_run_id))
        {
            return Err(StoreError::InvalidBatch("catalog.invalid_assignments"));
        }
        self.owners.insert(id.clone(), scope.clone());
        self.assignments.insert(id.clone(), update.assignments);
        if update.release_concurrency {
            self.release_admission(scope, id, now);
        }
        let mut summary = update.summary;
        // Only the start stamps the creation time; later summaries keep it.
        if let Some(prior) = self.runs.get(&summary.execution_id) {
            summary.created_at_ms = prior.created_at_ms;
        }
        self.runs.insert(summary.execution_id.clone(), summary);
        Ok(())
    }
}

impl CatalogData {
    /// D3: this project's catalog rows. Skills keep recording order.
    pub(crate) fn portable_rows(&self, scope: &TenantScope) -> crate::portable::StoreRows {
        use crate::portable::{PoolRow, RunbookRow, VersionRow};
        let mut rows = crate::portable::StoreRows::default();
        for state in self.runbooks.values().filter(|r| &r.scope == scope) {
            rows.runbooks.push(RunbookRow {
                record: state.record.clone(),
                draft_revision: state.draft_revision,
                draft_source: state.draft_source.clone(),
            });
            for version in &state.versions {
                rows.versions.push(VersionRow {
                    runbook_id: state.record.runbook_id.clone(),
                    version: version.clone(),
                });
            }
        }
        rows.drafts = self
            .drafts
            .iter()
            .filter(|((owner, _), _)| owner == scope)
            .map(|(_, draft)| draft.clone())
            .collect();
        rows.skills = self
            .skills
            .iter()
            .filter(|(owner, _)| owner == scope)
            .map(|(_, skill)| skill.clone())
            .collect();
        rows.pools = self
            .pools
            .iter()
            .filter(|((owner, _), _)| owner == scope)
            .map(|((_, key), pool)| PoolRow {
                key: key.clone(),
                limit: pool.limit,
                active: pool.active,
                queue: pool.queue.iter().cloned().collect(),
            })
            .collect();
        rows
    }
    /// Whether any catalog row belongs to this project.
    pub(crate) fn portable_has_rows(&self, scope: &TenantScope) -> bool {
        self.runbooks.values().any(|r| &r.scope == scope)
            || self.drafts.keys().any(|(owner, _)| owner == scope)
            || self.skills.iter().any(|(owner, _)| owner == scope)
            || self.pools.keys().any(|(owner, _)| owner == scope)
            || self.owners.values().any(|owner| owner == scope)
            || self.idempotency.keys().any(|(owner, _)| owner == scope)
    }
    /// D3 import into an empty project: IDs are preserved; an ID already
    /// owned by any project is a conflict.
    pub(crate) fn portable_import(
        &mut self,
        scope: &TenantScope,
        rows: &crate::portable::StoreRows,
    ) -> Result<(), crate::portable::PortableError> {
        use crate::portable::PortableError::Conflict;
        for row in &rows.runbooks {
            if self.runbooks.contains_key(&row.record.runbook_id) {
                return Err(Conflict("import.id_conflict"));
            }
            self.runbooks.insert(
                row.record.runbook_id.clone(),
                RunbookState {
                    scope: scope.clone(),
                    record: row.record.clone(),
                    draft_source: row.draft_source.clone(),
                    draft_revision: row.draft_revision,
                    versions: vec![],
                },
            );
        }
        for row in &rows.versions {
            if self.runbooks.values().any(|r| {
                r.versions
                    .iter()
                    .any(|v| v.version_id == row.version.version_id)
            }) {
                return Err(Conflict("import.id_conflict"));
            }
            let state = self
                .runbooks
                .get_mut(&row.runbook_id)
                .filter(|r| &r.scope == scope)
                .ok_or(Conflict("import.id_conflict"))?;
            state.versions.push(row.version.clone());
        }
        for draft in &rows.drafts {
            self.drafts
                .insert((scope.clone(), draft.key.clone()), draft.clone());
        }
        for skill in &rows.skills {
            self.skills.push((scope.clone(), skill.clone()));
        }
        for pool in &rows.pools {
            self.pools.insert(
                (scope.clone(), pool.key.clone()),
                Pool {
                    limit: pool.limit,
                    active: pool.active,
                    queue: pool.queue.iter().cloned().collect(),
                },
            );
        }
        for run in &rows.runs {
            let id = &run.slice.run.projection.execution_id;
            if self.owners.contains_key(id) {
                return Err(Conflict("import.id_conflict"));
            }
            self.owners.insert(id.clone(), scope.clone());
            let catalog = &run.slice.catalog;
            if let Some(summary) = &catalog.summary {
                self.runs.insert(id.clone(), summary.clone());
            }
            if !catalog.assignments.is_empty() {
                self.assignments
                    .insert(id.clone(), catalog.assignments.clone());
            }
            if let Some(admission) = &catalog.admission {
                self.admissions.insert(id.clone(), admission.clone());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(n: u8) -> TenantScope {
        TenantScope {
            org_id: "org_00000000000000000000000000".parse().unwrap(),
            project_id: format!("prj_{n:026}").parse().unwrap(),
        }
    }
    fn request(n: u8) -> AdmitRequest {
        AdmitRequest {
            execution_id: format!("exe_{n:026}").parse().unwrap(),
            key: "pool".into(),
            limit: 1,
            idempotency_key: format!("request-{n}"),
        }
    }
    /// LANG3 re-check, finding 2: `cancel_previous` releases every waiter
    /// before it queues, so `first` is a guard; pin it with a seeded waiter.
    #[test]
    fn a_first_in_line_start_is_promoted_before_an_older_waiter() {
        let mut data = CatalogData::default();
        data.admit(&scope(1), request(1), 7).unwrap();
        assert_eq!(
            data.admit(&scope(1), request(2), 7).unwrap().state,
            AdmissionState::Queued
        );
        let three = data
            .admit_pooled_at(&scope(1), request(3), 7, true)
            .unwrap();
        assert_eq!(three.state, AdmissionState::Queued);
        data.release_admission(&scope(1), &request(1).execution_id, 8);
        let state = |n: u8| {
            data.admission(&scope(1), &request(n).execution_id)
                .unwrap()
                .state
        };
        assert_eq!(state(3), AdmissionState::Admitted);
        assert_eq!(state(2), AdmissionState::Queued);
    }
    fn update(n: u8, seq: u64, release: bool) -> DerivedUpdate {
        DerivedUpdate {
            summary: RunSummary {
                execution_id: request(n).execution_id,
                runbook_id: "rb_00000000000000000000000000".parse().unwrap(),
                sequence: seq,
                status: ExecutionStatus::Succeeded,
                completed_at_ms: Some(7),
                title: String::new(),
                tags: vec![],
                fields: Default::default(),
                created_by: String::new(),
                created_at_ms: 0,
            },
            assignments: vec![],
            release_concurrency: release,
            memory: vec![],
        }
    }
    #[test]
    fn drafts_compare_revision_and_isolate_projects() {
        let mut data = CatalogData::default();
        assert_eq!(
            data.replace_draft(&scope(1), "x", 0, "one".into()),
            Ok(CommitResult::Committed(1))
        );
        assert_eq!(
            data.replace_draft(&scope(1), "x", 0, "stale".into()),
            Ok(CommitResult::Conflict)
        );
        assert_eq!(data.read_draft(&scope(2), "x"), None);
        assert_eq!(data.read_draft(&scope(1), "x").unwrap().source, "one");
    }
    #[test]
    fn fifo_release_is_idempotent_and_stale_updates_do_not_release() {
        let mut data = CatalogData::default();
        for n in 1..=3 {
            assert_eq!(
                data.admit(&scope(1), request(n), 7).unwrap().state,
                if n == 1 {
                    AdmissionState::Admitted
                } else {
                    AdmissionState::Queued
                }
            );
        }
        data.apply_derived(&scope(1), update(1, 2, false), 7)
            .unwrap();
        data.apply_derived(&scope(1), update(1, 1, true), 7)
            .unwrap();
        assert_eq!(
            data.admit(&scope(1), request(2), 7).unwrap().state,
            AdmissionState::Queued
        );
        data.apply_derived(&scope(1), update(1, 3, true), 7)
            .unwrap();
        data.apply_derived(&scope(1), update(1, 4, true), 7)
            .unwrap();
        assert_eq!(
            data.admit(&scope(1), request(2), 7).unwrap().state,
            AdmissionState::Admitted
        );
        assert_eq!(
            data.admit(&scope(1), request(3), 7).unwrap().state,
            AdmissionState::Queued
        );
        assert_eq!(
            data.admit(&scope(1), request(1), 7).unwrap().state,
            AdmissionState::Released
        );
    }
    #[test]
    fn assignment_updates_replace_whole_execution_and_ignore_duplicates() {
        let mut data = CatalogData::default();
        let mut first = update(1, 1, false);
        first.assignments.push(Assignment {
            execution_id: request(1).execution_id,
            step_run_id: "sr_00000000000000000000000000".parse().unwrap(),
            recipients: vec![],
            kind: crate::catalog::AssignmentKind::Work,
            sequence: 1,
            updated_at_ms: 7,
            voted_by: vec![],
            path: String::new(),
            title: String::new(),
        });
        data.apply_derived(&scope(1), first, 7).unwrap();
        assert_eq!(data.list_assignments(&scope(1)).len(), 1);
        data.apply_derived(&scope(1), update(1, 1, false), 7)
            .unwrap();
        assert_eq!(data.list_assignments(&scope(1)).len(), 1);
        data.apply_derived(&scope(1), update(1, 2, false), 7)
            .unwrap();
        assert!(data.list_assignments(&scope(1)).is_empty());
    }
    #[test]
    fn ownership_conflicts_and_scoped_reads_do_not_leak() {
        let mut data = CatalogData::default();
        data.admit(&scope(1), request(1), 7).unwrap();
        data.apply_derived(&scope(1), update(1, 1, false), 7)
            .unwrap();
        assert!(data.admit(&scope(2), request(1), 7).is_err());
        assert!(
            data.apply_derived(&scope(2), update(1, 2, false), 7)
                .is_err()
        );
        assert!(data.list_runs(&scope(2)).is_empty());
        assert!(data.list_assignments(&scope(2)).is_empty());
        assert!(data.get_run(&scope(2), &request(1).execution_id).is_none());
        assert!(
            data.admission(&scope(2), &request(1).execution_id)
                .is_none()
        );
        // D25: a different limit replaces the pool limit instead of failing.
        let mut raised = request(2);
        raised.limit = 2;
        assert_eq!(
            data.admit(&scope(1), raised, 7).unwrap().state,
            AdmissionState::Admitted
        );
        let mut conflicting = request(2);
        conflicting.idempotency_key = request(1).idempotency_key;
        assert!(data.admit(&scope(1), conflicting, 7).is_err());
    }

    #[test]
    fn export_and_delete_are_scoped_and_allow_missing_catalog_rows() {
        let mut data = CatalogData::default();
        let id = request(1).execution_id;
        let empty = RunCatalogSlice {
            summary: None,
            assignments: vec![],
            admission: None,
        };
        assert_eq!(data.export_run(&scope(1), &id).unwrap(), empty);
        data.claim_execution(&scope(1), &id).unwrap();
        assert_eq!(data.export_run(&scope(1), &id).unwrap(), empty);
        assert_eq!(data.export_run(&scope(2), &id), Err(StoreError::NotFound));
        assert_eq!(data.delete_run(&scope(2), &id), Err(StoreError::NotFound));
        assert_eq!(data.owners.get(&id), Some(&scope(1)));
        data.delete_run(&scope(1), &id).unwrap();
        data.delete_run(&scope(1), &id).unwrap();
        assert!(!data.owners.contains_key(&id));
    }

    #[test]
    fn deleting_released_run_preserves_shared_pool_and_siblings() {
        let mut data = CatalogData::default();
        for n in 1..=3 {
            data.admit(&scope(1), request(n), 7).unwrap();
        }
        let id = request(1).execution_id;
        assert!(data.delete_run(&scope(1), &id).is_err());
        assert!(
            data.delete_run(&scope(1), &request(3).execution_id)
                .is_err()
        );
        let mut finished = update(1, 1, true);
        finished.assignments.push(Assignment {
            execution_id: id.clone(),
            step_run_id: "sr_00000000000000000000000000".parse().unwrap(),
            recipients: vec![],
            kind: crate::catalog::AssignmentKind::Work,
            sequence: 1,
            updated_at_ms: 7,
            voted_by: vec![],
            path: String::new(),
            title: String::new(),
        });
        data.apply_derived(&scope(1), finished.clone(), 7).unwrap();
        let exported = data.export_run(&scope(1), &id).unwrap();
        assert_eq!(exported.summary, Some(finished.summary));
        assert_eq!(exported.assignments, finished.assignments);
        assert_eq!(
            exported.admission,
            Some((
                request(1),
                Admission {
                    execution_id: id.clone(),
                    state: AdmissionState::Released,
                    admitted_at_ms: Some(7)
                }
            ))
        );
        let sibling = data
            .export_run(&scope(1), &request(2).execution_id)
            .unwrap();
        data.delete_run(&scope(1), &id).unwrap();
        assert!(data.export_run(&scope(1), &id).unwrap().summary.is_none());
        assert!(!data.assignments.contains_key(&id));
        assert!(!data.admissions.contains_key(&id));
        assert!(
            !data
                .idempotency
                .contains_key(&(scope(1), request(1).idempotency_key))
        );
        assert!(!data.owners.contains_key(&id));
        assert_eq!(
            data.export_run(&scope(1), &request(2).execution_id)
                .unwrap(),
            sibling
        );
        assert_eq!(data.pools[&(scope(1), "pool".into())].active, 1);
        data.apply_derived(&scope(1), update(2, 1, true), 7)
            .unwrap();
        assert_eq!(
            data.admission(&scope(1), &request(3).execution_id)
                .unwrap()
                .state,
            AdmissionState::Admitted
        );
    }

    #[test]
    fn publication_numbers_dedupe_and_previews_match_postgres_semantics() {
        fn prepared(n: u8, hash: u8, source: &str) -> crate::publish::PreparedPublication {
            crate::publish::PreparedPublication {
                version_id: format!("rbv_{n:026}").parse().unwrap(),
                semantic_hash: [hash; 32],
                snapshot_digest: [hash.wrapping_add(100); 32],
                snapshot_size: 3,
                source: source.into(),
                snapshot: vec![1, 2, 3],
                warnings: Vec::new(),
                pins: crate::publish::SelectedPins {
                    skills: Default::default(),
                    users: Default::default(),
                    teams: Default::default(),
                },
            }
        }
        let id: runbook_model::RunbookId = "rb_00000000000000000000000001".parse().unwrap();
        let mut data = CatalogData::default();
        let record = RunbookRecord {
            runbook_id: id.clone(),
            slug: "demo".into(),
            origin: RunbookOrigin::Ui,
            archived: false,
        };
        assert_eq!(
            data.create_runbook(&scope(1), record, "one".into())
                .unwrap()
                .revision
                .get(),
            1
        );
        let one = data
            .publish_revision(&scope(1), &id, 1, prepared(1, 7, "one"))
            .unwrap();
        assert_eq!(one.runbook_version.get(), 1);
        // Semantic duplicate of the latest returns it, candidate discarded.
        assert_eq!(
            data.publish_revision(&scope(1), &id, 1, prepared(2, 7, "one"))
                .unwrap(),
            one
        );
        // Stale revision and changed source both fail without a version.
        assert!(
            data.publish_revision(&scope(1), &id, 9, prepared(3, 8, "one"))
                .is_err()
        );
        data.save_draft(&scope(1), &id, 1, "two".into()).unwrap();
        assert!(
            data.publish_revision(&scope(1), &id, 2, prepared(3, 8, "one"))
                .is_err()
        );
        assert_eq!(
            data.publish_revision(&scope(1), &id, 2, prepared(3, 8, "two"))
                .unwrap()
                .runbook_version
                .get(),
            2
        );
        let preview = data
            .create_preview(
                &scope(1),
                &id,
                prepared(4, 9, "two"),
                Some("b".into()),
                None,
            )
            .unwrap();
        assert_eq!(preview.runbook_version, None);
        assert_eq!(
            data.resolve_version(&scope(1), &id, None)
                .unwrap()
                .runbook_version,
            std::num::NonZeroU32::new(2)
        );
        assert_eq!(
            data.resolve_version(&scope(2), &id, None),
            Err(StoreError::NotFound)
        );
        assert_eq!(data.list_versions(&scope(1), &id).unwrap().len(), 2);
        assert_eq!(data.list_previews(&scope(1), &id).unwrap(), vec![preview]);
    }

    #[test]
    fn deleting_rejects_released_run_still_in_queue_without_mutation() {
        let mut data = CatalogData::default();
        data.admit(&scope(1), request(1), 7).unwrap();
        data.apply_derived(&scope(1), update(1, 1, true), 7)
            .unwrap();
        let id = request(1).execution_id;
        data.pools
            .get_mut(&(scope(1), "pool".into()))
            .unwrap()
            .queue
            .push_back(id.clone());
        let before = data.export_run(&scope(1), &id).unwrap();
        assert!(data.delete_run(&scope(1), &id).is_err());
        assert_eq!(data.export_run(&scope(1), &id).unwrap(), before);
    }
    /// C4: summaries derived after the start carry no start time (the
    /// adapter stamps it once), so later updates keep the stored one.
    #[test]
    fn derived_updates_keep_the_start_time() {
        let mut data = CatalogData::default();
        let mut first = update(1, 1, false);
        first.summary.created_at_ms = 500;
        data.apply_derived(&scope(1), first, 7).unwrap();
        data.apply_derived(&scope(1), update(1, 2, false), 7)
            .unwrap();
        assert_eq!(data.runs[&request(1).execution_id].created_at_ms, 500);
    }
}
