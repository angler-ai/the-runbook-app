//! Native bridge to B5's atomic start store. HTTP replay uses no second table.
use runbook_api::executions::{
    ExecutionApiError as Error, ExecutionBackend, RerunInvocation, StartIdentity, StartInvocation,
};
use runbook_core::{
    ExecutionStore, StoreError, StoredExecution,
    authoring::{AuthoringFactory, AuthoringStore},
    blob::{BlobKey, BlobStore},
    catalog::{Admission, AdmissionState, CatalogStore, TenantScope},
    directory::DirectoryStore,
    start::{
        StartCommand, StartError, StartOutcome, StartRecord, StartStore, commit_start_with_policy,
        prepare_start_commit,
    },
};
use runbook_model::*;
use runbook_store_postgres::{
    DatabaseConfig, PostgresAuthoring, PostgresDirectory, PostgresExecutionStore,
};
use std::num::{NonZeroU32, NonZeroU64};
use std::sync::Arc;

#[derive(Clone)]
pub struct PostgresExecutionBackend {
    pub config: DatabaseConfig,
    pub objects: Option<Arc<runbook_store_objects::ObjectStorage>>,
}

impl PostgresExecutionBackend {
    pub fn new(config: DatabaseConfig) -> Self {
        Self {
            config,
            objects: None,
        }
    }

    pub fn with_objects(
        mut self,
        objects: Option<Arc<runbook_store_objects::ObjectStorage>>,
    ) -> Self {
        self.objects = objects;
        self
    }

    fn store(&self, scope: TenantScope) -> PostgresExecutionStore {
        PostgresExecutionStore::new(self.config.clone(), scope).with_objects(self.objects.clone())
    }
}
use runbook_api::executions::store_error;
fn start_error(e: StartError) -> Error {
    match e {
        StartError::Store(e) => store_error(e),
        StartError::VersionNotFound => Error::new(404, "runbook.version_not_found"),
        // LANG3 `cancel_previous` preparation (nothing written).
        StartError::ConcurrencyTooManyHolders => {
            Error::new(409, runbook_core::concurrency::TOO_MANY_HOLDERS_CODE)
        }
        StartError::ConcurrencyContended => {
            Error::new(409, runbook_core::concurrency::CONTENDED_CODE)
        }
        StartError::ConcurrencyHolderUnavailable(holder) => Error {
            status: 409,
            code: runbook_core::concurrency::HOLDER_UNAVAILABLE_CODE,
            diagnostics: vec![Diagnostic {
                path: holder.to_string(),
                code: runbook_core::concurrency::HOLDER_UNAVAILABLE_CODE.into(),
                message: format!(
                    "Run {holder} holds or waits for this concurrency key and could not be cancelled now. Try again, or cancel it first."
                ),
                line: None,
                column: None,
            }],
        },
        StartError::Diagnostics(diagnostics) => Error {
            status: 422,
            code: "start.invalid",
            diagnostics,
        },
        // U10: the same 422 code and `{path, code, message}` errors as rerun.
        StartError::InvalidData(errors) => Error {
            status: 422,
            code: StartError::INVALID_DATA_CODE,
            diagnostics: errors
                .into_iter()
                .map(|e| Diagnostic {
                    path: e.path,
                    code: e.code,
                    message: e.message,
                    line: None,
                    column: None,
                })
                .collect(),
        },
    }
}
impl ExecutionBackend for PostgresExecutionBackend {
    async fn report(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<ExecutionDocumentView, Error> {
        Ok(self.document(scope, execution).await?.1)
    }
    async fn report_markdown(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<String, Error> {
        let (state, document) = self.document(scope, execution).await?;
        // Name people the way the page does; the export is read by people.
        let names = self.names(scope).await?;
        Ok(runbook_core::execution_document::render_markdown_named(
            &state, &document, &names,
        ))
    }
    async fn report_junit(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<String, Error> {
        let (state, document) = self.document(scope, execution).await?;
        let names = self.names(scope).await?;
        Ok(runbook_core::report_junit::render_junit_named(
            &state, &document, &names,
        ))
    }
    async fn rerun(
        &self,
        scope: &TenantScope,
        invocation: RerunInvocation,
    ) -> Result<ExecutionView, Error> {
        let store = self.store(scope.clone());
        let original = store
            .start_record(&invocation.original)
            .await
            .map_err(store_error)?
            .ok_or_else(|| Error::new(404, "resource.not_found"))?;
        // Omitted version reruns the original's exact pinned version,
        // including a preview (which keeps a null version number).
        let selector = invocation
            .version
            .unwrap_or_else(|| VersionSelector::Id(original.version_id.clone()));
        let resolved = store
            .resolve_version(&original.runbook_id, Some(selector))
            .await
            .map_err(|e| match e {
                StoreError::NotFound => Error::new(404, "runbook.version_not_found"),
                other => store_error(other),
            })?;
        let target = store
            .version_record(&original.runbook_id, &resolved.runbook_version_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| Error::new(404, "runbook.version_not_found"))?;
        let bytes = store
            .get(&BlobKey::Shared {
                hash: target.snapshot_digest,
            })
            .await
            .map_err(store_error)?;
        let compiled = runbook_spec::restore(&bytes)
            .map_err(|_| Error::new(503, "execution.snapshot_invalid"))?;
        // The original's resolved inputs and *initial* fields, validated
        // against the target's schemas; nothing is created on rejection.
        let initial = runbook_engine::InitialData {
            context: original.context.clone(),
            vars: Data::new(),
            reads: vec![],
        };
        let context = runbook_core::rerun::rerun_context(
            &initial,
            &compiled,
            runbook_core::rerun::RerunInvocation {
                url: invocation.url.clone(),
                created_by: invocation.identity.principal.clone(),
                project_vars: invocation.project_vars,
            },
        )
        .map_err(|rejected| Error {
            status: runbook_core::rerun::RerunRejected::STATUS,
            code: runbook_core::rerun::RerunRejected::CODE,
            diagnostics: rejected
                .errors
                .into_iter()
                .map(|e| Diagnostic {
                    path: e.path,
                    code: e.code,
                    message: e.message,
                    line: None,
                    column: None,
                })
                .collect(),
        })?;
        let command = StartCommand {
            execution_id: invocation.execution_id,
            runbook_id: original.runbook_id.clone(),
            version: Some(VersionSelector::Id(resolved.runbook_version_id)),
            inputs: context.inputs,
            fields: context.fields,
            project_vars: context.project_vars,
            // The original's resolved title and requested tags carry over; the
            // target version supplies its own spec tags (R28).
            title: original.context.title.clone(),
            tags: original.request_tags.clone(),
            url: invocation.url,
            created_by: invocation.identity.principal.clone(),
            // A rerun runs under today's cap, not the original's.
            limits: runbook_engine::ExecutionLimits {
                max_step_runs: self.config.step_cap(),
                ..context.limits
            },
            idempotency_key: invocation.identity.key,
            principal: invocation.identity.principal,
            request_hash: invocation.identity.request_hash,
        };
        let mut commit = prepare_start_commit(&store, &store, &command)
            .await
            .map_err(start_error)?;
        commit.record.rerun_of = Some(invocation.original);
        let directory = PostgresDirectory::new(self.config.clone());
        let outcome = commit_start_with_policy(&store, &directory, scope, commit)
            .await
            .map_err(start_error)?;
        initial_outcome(&store, scope, outcome).await
    }
    async fn search(
        &self,
        scope: &TenantScope,
        query: runbook_api::search::SearchQuery,
        origin: &str,
    ) -> Result<serde_json::Value, Error> {
        crate::run_search::search(self, scope, query, origin).await
    }

    async fn execution_project(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Option<ProjectId>, Error> {
        // B5a's explicit org-scoped key read; never scan all projects/executions.
        let store = self.store(scope.clone());
        store
            .execution_project(&scope.org_id, execution)
            .await
            .map_err(store_error)
    }
    async fn summary_retained(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<bool, Error> {
        use runbook_core::archives::ArchiveIndex;
        let store = self.store(scope.clone());
        Ok(store
            .archived_run(scope, execution)
            .await
            .map_err(store_error)?
            .is_some())
    }
    async fn tombstone_project(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Option<ProjectId>, Error> {
        let store = self.store(scope.clone());
        store
            .tombstone_project(&scope.org_id, execution)
            .await
            .map_err(store_error)
    }
    async fn find_replay(
        &self,
        scope: &TenantScope,
        identity: &StartIdentity,
    ) -> Result<Option<ExecutionView>, Error> {
        let store = self.store(scope.clone());
        let Some(replay) = store
            .find_replay(&identity.key)
            .await
            .map_err(store_error)?
        else {
            return Ok(None);
        };
        if replay.principal != identity.principal || replay.request_hash != identity.request_hash {
            return Err(Error::new(409, "request.idempotency_key_reused"));
        }
        Ok(Some(initial_outcome(&store, scope, replay.outcome).await?))
    }
    async fn published_triggers(
        &self,
        scope: &TenantScope,
        runbook: &str,
    ) -> Result<
        Option<(
            runbook_core::triggers::PublishedVersion,
            std::rc::Rc<runbook_spec::ir::Triggers>,
        )>,
        Error,
    > {
        let rb = if let Ok(id) = runbook.parse::<RunbookId>() {
            id
        } else {
            let factory = PostgresAuthoring::new(self.config.clone());
            match factory
                .scoped(scope.clone())
                .catalog_runbooks()
                .await
                .map_err(store_error)?
                .into_iter()
                .find(|r| r.slug == runbook)
            {
                Some(found) => found.runbook_id,
                None => return Ok(None),
            }
        };
        runbook_core::triggers::published_triggers(&self.store(scope.clone()), scope, &rb)
            .await
            .map_err(store_error)
    }
    async fn start(
        &self,
        scope: &TenantScope,
        invocation: StartInvocation,
    ) -> Result<ExecutionView, Error> {
        let factory = PostgresAuthoring::new(self.config.clone());
        let authoring = factory.scoped(scope.clone());
        let rb = if let Ok(id) = invocation.request.runbook.parse::<RunbookId>() {
            id
        } else {
            authoring
                .catalog_runbooks()
                .await
                .map_err(store_error)?
                .into_iter()
                .find(|r| r.slug == invocation.request.runbook)
                .map(|r| r.runbook_id)
                .ok_or_else(|| Error::new(404, "resource.not_found"))?
        };
        let store = self.store(scope.clone());
        let command = StartCommand {
            execution_id: invocation.execution_id,
            runbook_id: rb,
            version: invocation.request.version,
            inputs: invocation.request.inputs,
            fields: invocation.request.fields,
            title: invocation.request.title,
            tags: invocation.request.tags,
            project_vars: invocation.project_vars,
            url: invocation.url,
            created_by: invocation.identity.principal.clone(),
            limits: runbook_engine::ExecutionLimits {
                max_step_runs: self.config.step_cap(),
                ..Default::default()
            },
            idempotency_key: invocation.identity.key,
            principal: invocation.identity.principal,
            request_hash: invocation.identity.request_hash,
        };
        let commit = prepare_start_commit(&store, &store, &command)
            .await
            .map_err(start_error)?;
        let directory = PostgresDirectory::new(self.config.clone());
        let outcome = commit_start_with_policy(&store, &directory, scope, commit)
            .await
            .map_err(start_error)?;
        initial_outcome(&store, scope, outcome).await
    }
    async fn read(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<ExecutionView, Error> {
        let store = self.store(scope.clone());
        let run = store.load(execution).await.map_err(store_error)?;
        let record = store
            .start_record(execution)
            .await
            .map_err(store_error)?
            .ok_or_else(|| Error::new(404, "resource.not_found"))?;
        let admission = effective_admission(
            &store,
            execution,
            store.admission(execution).await.map_err(store_error)?,
        )
        .await?;
        let state = restore_state(&store, &record, &run).await?;
        let mut view = current_view(&record, &run, &state, admission)?;
        fill_identity(&store, scope, execution, &record, &mut view).await;
        view.notifications_not_delivered = self.undelivered(scope, execution).await?;
        Ok(view)
    }
}
/// B1 identity fields live outside the engine projection: slug from the
/// catalog row, creation facts from the run summary.
async fn fill_identity(
    store: &PostgresExecutionStore,
    scope: &TenantScope,
    execution: &ExecutionId,
    record: &StartRecord,
    view: &mut ExecutionView,
) {
    view.project_id = Some(scope.project_id.clone());
    if let Ok(Some(runbook)) = store.read_runbook(&record.runbook_id).await {
        view.runbook_slug = Some(runbook.slug);
    }
    if let Ok(Some(summary)) = store.get_run(execution).await {
        view.created_at_ms = Some(summary.created_at_ms);
        view.created_by = Some(summary.created_by);
    }
}
async fn initial_outcome(
    store: &PostgresExecutionStore,
    scope: &TenantScope,
    outcome: StartOutcome,
) -> Result<ExecutionView, Error> {
    let record = store
        .start_record(&outcome.execution_id)
        .await
        .map_err(store_error)?
        .ok_or_else(|| Error::new(503, "start.record_missing"))?;
    // Reconstruct only the immutable initial response; later activation/edits
    // must never turn a POST retry into a current-state GET.
    let mut view = initial_view(
        &outcome.execution_id,
        &record,
        effective_admission(store, &outcome.execution_id, outcome.admission).await?,
    );
    if let Ok(Some(runbook)) = store.read_runbook(&record.runbook_id).await {
        view.runbook_slug = Some(runbook.slug);
    }
    view.project_id = Some(scope.project_id.clone());
    if let Ok(Some(summary)) = store.get_run(&outcome.execution_id).await {
        view.created_at_ms = Some(summary.created_at_ms);
    }
    Ok(view)
}
// Unpooled runs are admitted at their immutable start-transaction timestamp.
async fn effective_admission(
    store: &PostgresExecutionStore,
    id: &ExecutionId,
    admission: Option<Admission>,
) -> Result<Option<Admission>, Error> {
    if admission.is_some() {
        return Ok(admission);
    }
    let at = unpooled_time(
        async {
            store
                .get_run(id)
                .await
                .map_err(store_error)?
                .map(|s| s.created_at_ms)
                .ok_or_else(|| Error::new(404, "resource.not_found"))
        },
        async {
            let run = store.load(id).await.map_err(store_error)?;
            Ok(run
                .events
                .iter()
                .find(|e| matches!(e.event, runbook_engine::Event::ExecutionStarted { .. }))
                .map(|e| e.at_ms))
        },
    )
    .await?;
    Ok(Some(Admission {
        execution_id: id.clone(),
        state: AdmissionState::Admitted,
        admitted_at_ms: Some(at),
    }))
}
async fn unpooled_time(
    summary: impl std::future::Future<Output = Result<i64, Error>>,
    event: impl std::future::Future<Output = Result<Option<i64>, Error>>,
) -> Result<i64, Error> {
    // Read the fallback first: if activation races this read, the following
    // event snapshot either observes it or the fallback still predates it.
    let summary = summary.await?;
    Ok(event.await?.unwrap_or(summary))
}
fn initial_view(
    id: &ExecutionId,
    record: &StartRecord,
    admission: Option<Admission>,
) -> ExecutionView {
    ExecutionView {
        overdue_steps: vec![],
        schema_revision: NonZeroU32::MIN,
        id: id.clone(),
        url: record.context.url.clone(),
        web_url: execution_web_url(&record.context.url, id),
        runbook_id: Some(record.runbook_id.clone()),
        runbook_slug: None,
        project_id: None,
        created_at_ms: None,
        created_by: Some(record.context.created_by.clone()),
        started_at_ms: None,
        completed_at_ms: None,
        runbook_version_id: record.version_id.clone(),
        runbook_version: record.version.map(NonZeroU32::get),
        title: record.context.title.clone().unwrap_or_default(),
        status: ExecutionStatus::Queued,
        phase: ExecutionPhase::Ordinary,
        admission: admission.map(admission_view),
        execution_deadline_at_ms: None,
        result: None,
        pending_terminal_status: None,
        terminal_reason: None,
        inputs: record.context.inputs.clone(),
        fields: record.context.fields.clone(),
        tags: record.context.tags.clone(),
        steps: vec![],
        amended: false,
        amendments: vec![],
        stale_step_ids: vec![],
        rerun_of: record.rerun_of.clone(),
        resume_brief: None,
        notifications_not_delivered: vec![],
        superseded_by: None,
        // LANG4: a child names its parent from its start, before it runs.
        parent: record
            .context
            .parent
            .as_ref()
            .map(runbook_core::execution_document::parent_view),
        progress: None,
        cost: None,
    }
}
fn admission_view(admission: Admission) -> ExecutionAdmissionView {
    ExecutionAdmissionView {
        state: match admission.state {
            AdmissionState::Queued => ExecutionAdmissionState::Queued,
            AdmissionState::Admitted => ExecutionAdmissionState::Admitted,
            AdmissionState::Released => ExecutionAdmissionState::Released,
        },
        admitted_at_ms: admission.admitted_at_ms,
    }
}
use runbook_api::executions::restore_state;
fn current_view(
    record: &StartRecord,
    run: &StoredExecution,
    state: &runbook_engine::ExecutionState,
    admission: Option<Admission>,
) -> Result<ExecutionView, Error> {
    let mut view = initial_view(&state.id, record, admission);
    if state.sequence == 0 {
        return Ok(view);
    }
    view.status = runbook_engine::reported_status(state);
    view.phase = if matches!(
        state.status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    ) {
        ExecutionPhase::Completed
    } else {
        match state.phase {
            runbook_engine::ExecutionPhase::Ordinary => ExecutionPhase::Ordinary,
            runbook_engine::ExecutionPhase::Cleanup => ExecutionPhase::Cleanup,
        }
    };
    view.title = state.title.clone();
    view.inputs = state.inputs.clone();
    view.fields = state.fields.clone();
    view.tags = state.tags.clone();
    view.result = (!state.result.is_null()).then(|| state.result.clone());
    view.started_at_ms = state.started_at_ms;
    view.completed_at_ms = run.projection.completed_at_ms;
    view.execution_deadline_at_ms = state.execution_deadline_at_ms;
    view.pending_terminal_status = state.pending_terminal_cause.map(|c| match c {
        runbook_engine::TerminalCause::Succeeded => ExecutionStatus::Succeeded,
        runbook_engine::TerminalCause::Failed => ExecutionStatus::Failed,
        runbook_engine::TerminalCause::Cancelled => ExecutionStatus::Cancelled,
    });
    view.terminal_reason = matches!(
        view.status,
        ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
    .then(|| state.terminal_reason.clone())
    .flatten();
    // Runbook order, the same as the document's sections (walkthrough #2).
    for step in runbook_core::execution_document::runbook_order(state) {
        let Some(id) = &step.id else {
            continue;
        };
        let claim = run
            .claims
            .get(id)
            .filter(|_| step.status == StepStatus::Claimed)
            .map(|c| {
                NonZeroU64::new(c.generation)
                    .map(|generation| ClaimRef {
                        id: c.claim_id.clone(),
                        generation,
                    })
                    .ok_or_else(|| Error::new(503, "execution.claim_invalid"))
            })
            .transpose()?;
        view.steps.push(StepView {
            schema_revision: NonZeroU32::MIN,
            id: id.clone(),
            kind: state.node(&step.step_id).map(|node| {
                match &node.kind {
                    runbook_spec::ir::NodeKind::Task(_) => "task",
                    runbook_spec::ir::NodeKind::Approval(_) => "approval",
                    runbook_spec::ir::NodeKind::Group(_) => "group",
                    runbook_spec::ir::NodeKind::Parallel { .. } => "parallel",
                    runbook_spec::ir::NodeKind::Foreach(_) => "foreach",
                    runbook_spec::ir::NodeKind::Loop(_) => "loop",
                    runbook_spec::ir::NodeKind::Switch(_) => "switch",
                    runbook_spec::ir::NodeKind::Wait(_) => "wait",
                    runbook_spec::ir::NodeKind::Set(_) => "set",
                    runbook_spec::ir::NodeKind::End(_) => "end",
                    runbook_spec::ir::NodeKind::Call(_) => "call",
                }
                .to_owned()
            }),
            completed_at_ms: step.finished_at_ms,
            path: runbook_core::execution_document::path_of(step),
            status: step.status,
            verdict: step.verdict,
            outputs: step.outputs.clone(),
            summary: step.summary.clone(),
            actor: step.actor.clone(),
            claim,
            attempt: NonZeroU32::new(step.attempt)
                .ok_or_else(|| Error::new(503, "execution.attempt_invalid"))?,
            satisfied: step.satisfied,
            exhausted: step.exhausted,
            checklist: step.checklist.clone(),
            // Set below from state with the run-level amendment facts.
            amended: false,
            amendments: vec![],
            stale: false,
            eligible_recipients: step
                .handoff
                .as_ref()
                .map(|h| h.eligible_recipients.clone())
                .unwrap_or_default(),
            // Set below from state, with the other state facts (LANG4).
            call: None,
            wait: None,
            actor_kind: None,
            assignees: vec![],
            branch: None,
            iterations: None,
            skip_reason: None,
            followup: None,
            runner: None,
            cost: None,
        });
    }
    runbook_core::execution_document::apply_amendment_facts(state, &mut view);
    Ok(view)
}

impl PostgresExecutionBackend {
    /// C1: human notices that reached no webhook subscriber.
    async fn undelivered(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<Vec<UndeliveredNotification>, Error> {
        runbook_core::webhooks::undelivered_for(
            &runbook_store_postgres::PostgresWebhooks::new(self.config.clone()),
            scope,
            execution,
        )
        .await
        .map_err(store_error)
    }
    /// Directory names of the org's users, for the exports people read.
    async fn names(
        &self,
        scope: &TenantScope,
    ) -> Result<std::collections::BTreeMap<UserId, String>, Error> {
        let snapshot = runbook_store_postgres::PostgresDirectory::new(self.config.clone())
            .snapshot(&scope.org_id)
            .await
            .map_err(|_| Error::new(503, "storage.unavailable"))?;
        Ok(snapshot
            .users
            .iter()
            .map(|(id, user)| (id.clone(), user.name.clone()))
            .collect())
    }
    /// One consistent revision backs the document and its Markdown export.
    async fn document(
        &self,
        scope: &TenantScope,
        execution: &ExecutionId,
    ) -> Result<(runbook_engine::ExecutionState, ExecutionDocumentView), Error> {
        let store = self.store(scope.clone());
        let run = store.load(execution).await.map_err(store_error)?;
        let record = store
            .start_record(execution)
            .await
            .map_err(store_error)?
            .ok_or_else(|| Error::new(404, "resource.not_found"))?;
        let admission = effective_admission(
            &store,
            execution,
            store.admission(execution).await.map_err(store_error)?,
        )
        .await?;
        let state = restore_state(&store, &record, &run).await?;
        let mut header = current_view(&record, &run, &state, admission)?;
        fill_identity(&store, scope, execution, &record, &mut header).await;
        header.notifications_not_delivered = self.undelivered(scope, execution).await?;
        let mut document =
            runbook_core::execution_document::build_document(&state, header, |step| {
                format!("/v1/step-runs/{step}/logs")
            });
        runbook_core::execution_document::mark_dropped_proposals(
            &store,
            scope,
            &mut document.sections,
        )
        .await
        .map_err(store_error)?;
        Ok((state, document))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unpooled_timestamp_survives_activation_between_reads() {
        use std::cell::Cell;
        let activated = Cell::new(false);
        let at = unpooled_time(async { Ok(if activated.get() { 0 } else { 123 }) }, async {
            // Activation commits immediately after this event snapshot.
            let event = activated.get().then_some(123);
            activated.set(true);
            Ok(event)
        })
        .await
        .unwrap();
        assert_eq!(at, 123);
    }
}
