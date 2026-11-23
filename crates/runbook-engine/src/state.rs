use crate::EngineError;
use runbook_model::{
    ActorRef, ChecklistTick, Data, ExecutionId, ExecutionStatus, MemoryId, MemoryReadRef,
    StepRunId, StepStatus, Verdict,
};
use runbook_spec::CompiledRunbook;
use std::rc::Rc;

/// Shared immutable definition; reducer clones never copy or recompile a snapshot.
#[derive(Clone, Debug)]
struct Definition(Rc<CompiledRunbook>);
impl PartialEq for Definition {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || self.0.snapshot_bytes() == other.0.snapshot_bytes()
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct StepState {
    pub step_id: String,
    pub node_id: String,
    pub scope: Vec<crate::IterationRef>,
    pub foreach: Option<crate::ForeachSnapshot>,
    pub deadline_at_ms: Option<i64>,
    pub wait: Option<crate::WaitState>,
    /// LANG4: a call step's link to its child execution.
    pub call: Option<crate::CallState>,
    pub approval: Option<crate::ApprovalState>,
    pub loop_state: Option<crate::LoopState>,
    pub satisfied: bool,
    pub exhausted: bool,
    /// Pending declarations have no materialized run ID yet.
    pub id: Option<StepRunId>,
    pub status: StepStatus,
    pub verdict: Option<Verdict>,
    pub outputs: Data,
    pub summary: Option<String>,
    pub checklist: Vec<ChecklistTick>,
    pub evidence: Vec<String>,
    /// UP1: finalized uploads attached by this step's reports, in order.
    pub files: Vec<runbook_model::EvidenceFileRef>,
    pub logs: Vec<String>,
    pub branch: Option<crate::BranchChoice>,
    pub group_finally: Option<crate::GroupFinallyState>,
    pub group_finally_done: bool,
    pub agent_notified_attempt: Option<u32>,
    pub catch_source: Option<StepRunId>,
    pub skip_reason: Option<crate::SkipReason>,
    pub attempt: u32,
    pub(crate) contributors: Rc<crate::approval::Contributors>,
    pub attempt_history: Vec<Rc<crate::AttemptRecord>>,
    pub recovery_pending: bool,
    pub retry_timer: Option<crate::RetryTimer>,
    pub human_continuation: bool,
    pub handoff: Option<crate::ResolvedHandoff>,
    /// LANG7: a strict task's assignee as resolved for its latest attempt.
    pub strict_assignee: Option<crate::ResolvedAssignee>,
    /// LANG7: the person who took over this agent task's current claim.
    pub taken_over: Option<crate::TakenOver>,
    pub routing_error: Option<String>,
    pub blocked_notification_queued: bool,
    /// LANG9: when this step last became open (ready, claimed or waiting),
    /// the base its reminders and escalation count from; how many
    /// reminders were sent since; and its follow-up escalation, if any.
    pub opened_at_ms: Option<i64>,
    pub(crate) due_after_ms: Option<u64>,
    pub overdue_since_ms: Option<i64>,
    pub reminders_sent: u32,
    pub followup_escalation: Option<crate::EscalationSnapshot>,
    pub revision: u64,
    pub actor: Option<ActorRef>,
    pub finished_at_ms: Option<i64>,
    pub reads: Vec<runbook_expr::ReadRef>,
    /// Amendment annotations (B9a): the record as first completed, the
    /// override history, and whether an evaluated input has since changed.
    pub original: Option<crate::OriginalStepRecord>,
    pub amendments: Vec<runbook_model::Amendment>,
    pub amended: bool,
    pub stale: bool,
    pub memory_read: Vec<MemoryReadRef>,
    pub memory_proposed: Vec<MemoryId>,
    /// MEM6: live entries this step run saved, in save order.
    pub memory_saved: Vec<MemoryId>,
    /// MEM6: entries this step run edited, in edit order.
    pub memory_edited: Vec<MemoryId>,
    /// R0-4: the runner agent's work on this step.
    pub runner: crate::RunnerStepState,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionState {
    definition: Definition,
    pub id: ExecutionId,
    /// Engine semantics recorded by `ExecutionStarted` (F95).
    pub engine_version: u32,
    pub sequence: u64,
    pub at_ms: i64,
    pub status: ExecutionStatus,
    pub phase: crate::ExecutionPhase,
    pub cleanup: Option<crate::CleanupState>,
    pub limits: crate::ExecutionLimits,
    pub materialized_steps: u64,
    pub work_reason: crate::WorkReason,
    pub execution_deadline_at_ms: Option<i64>,
    pub pause_reasons: std::collections::BTreeSet<crate::PauseReason>,
    pub terminal_reason: Option<String>,
    pub pending_terminal_cause: Option<crate::TerminalCause>,
    /// LANG3: the run that replaced this one, from its cancellation.
    pub superseded: Option<Supersession>,
    /// LANG4: the call step that started this run, when it is a child.
    pub parent: Option<crate::ParentLink>,
    pub signals: std::collections::BTreeMap<String, Rc<crate::SignalDelivery>>,
    pub runtime_errors: Vec<crate::RuntimeFault>,
    pub steps: Vec<StepState>,
    pub inputs: Data,
    pub fields: Data,
    pub vars: Data,
    pub project_vars: Data,
    pub title: String,
    pub tags: Vec<String>,
    pub url: String,
    pub created_by: String,
    pub started_at_ms: Option<i64>,
    pub data_revision: u64,
    pub revisions: std::collections::BTreeMap<String, runbook_expr::RecordIdentity>,
    pub initialization_reads: Vec<runbook_expr::ReadRef>,
    pub output_reads: Vec<runbook_expr::ReadRef>,
    pub output_revision: u64,
    pub outputs: Data,
    pub result: serde_json::Value,
}
impl ExecutionState {
    pub fn new(id: ExecutionId, definition: Rc<CompiledRunbook>) -> Self {
        let mut nodes = vec![];
        crate::tree::collect_local(&definition.ir().steps, &mut nodes);
        crate::tree::collect_local(&definition.ir().finally, &mut nodes);
        let steps = nodes
            .iter()
            .map(|node| StepState::pending(node, node.id.clone(), vec![]))
            .collect();
        Self {
            definition: Definition(definition),
            id,
            engine_version: crate::ENGINE_VERSION,
            sequence: 0,
            at_ms: 0,
            status: ExecutionStatus::Queued,
            pending_terminal_cause: None,
            superseded: None,
            parent: None,
            execution_deadline_at_ms: None,
            phase: Default::default(),
            cleanup: None,
            limits: Default::default(),
            materialized_steps: 0,
            work_reason: Default::default(),
            pause_reasons: Default::default(),
            terminal_reason: None,
            runtime_errors: vec![],
            signals: Default::default(),
            steps,
            inputs: Data::new(),
            fields: Data::new(),
            vars: Data::new(),
            project_vars: Data::new(),
            title: String::new(),
            tags: vec![],
            url: String::new(),
            created_by: String::new(),
            started_at_ms: None,
            data_revision: 0,
            revisions: Default::default(),
            initialization_reads: vec![],
            output_reads: vec![],
            output_revision: 0,
            outputs: Data::new(),
            result: serde_json::Value::Null,
        }
    }
    /// Fresh queued state sharing only the sealed definition and execution ID.
    pub(crate) fn fresh(&self) -> Self {
        Self::new(self.id.clone(), self.definition.0.clone())
    }
    pub fn node(&self, id: &str) -> Option<&runbook_spec::ir::Node> {
        let id = self
            .steps
            .iter()
            .find(|s| s.step_id == id)
            .map_or(id, |s| s.node_id.as_str());
        crate::tree::find(&self.compiled().ir().steps, id)
            .or_else(|| crate::tree::find(&self.compiled().ir().finally, id))
    }
    pub fn compiled(&self) -> &CompiledRunbook {
        &self.definition.0
    }
    pub(crate) fn step_index(&self, id: &StepRunId) -> Result<usize, EngineError> {
        self.steps
            .iter()
            .position(|step| step.id.as_ref() == Some(id))
            .ok_or(EngineError::invariant(
                "event references an absent step run",
            ))
    }
}

impl StepState {
    pub(crate) fn pending(
        node: &runbook_spec::ir::Node,
        step_id: String,
        scope: Vec<crate::IterationRef>,
    ) -> Self {
        Self {
            step_id,
            node_id: node.id.clone(),
            scope,
            foreach: None,
            loop_state: None,
            approval: None,
            wait: None,
            call: None,
            deadline_at_ms: None,
            satisfied: false,
            exhausted: false,
            id: None,
            status: StepStatus::Pending,
            verdict: None,
            outputs: Data::new(),
            summary: None,
            checklist: match &node.kind {
                runbook_spec::ir::NodeKind::Task(t) => t
                    .checklist
                    .iter()
                    .map(|title| ChecklistTick {
                        title: title.clone(),
                        completed: false,
                    })
                    .collect(),
                _ => vec![],
            },
            evidence: vec![],
            files: vec![],
            logs: vec![],
            branch: None,
            catch_source: None,
            group_finally_done: false,
            agent_notified_attempt: None,
            group_finally: None,
            skip_reason: None,
            attempt: 1,
            contributors: Default::default(),
            attempt_history: vec![],
            recovery_pending: false,
            retry_timer: None,
            human_continuation: false,
            handoff: None,
            strict_assignee: None,
            taken_over: None,
            routing_error: None,
            blocked_notification_queued: false,
            opened_at_ms: None,
            due_after_ms: node.due_after.map(|due| due.millis),
            overdue_since_ms: None,
            reminders_sent: 0,
            followup_escalation: None,
            revision: 0,
            actor: None,
            finished_at_ms: None,
            reads: vec![],
            runner: crate::RunnerStepState::default(),
            original: None,
            amendments: vec![],
            amended: false,
            stale: false,
            memory_read: vec![],
            memory_proposed: vec![],
            memory_saved: vec![],
            memory_edited: vec![],
        }
    }
}

/// LANG4: the call step a child execution belongs to. `depth` counts calls
/// from the top-level run (its child is 1).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ParentLink {
    pub execution_id: runbook_model::ExecutionId,
    pub step_run_id: runbook_model::StepRunId,
    pub step_id: String,
    pub depth: u32,
}
/// LANG3: who replaced a run cancelled by `on_conflict: cancel_previous`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Supersession {
    pub execution_id: runbook_model::ExecutionId,
    /// The principal who started the replacing run.
    pub started_by: String,
}
pub(crate) fn supersession(
    superseded_by: &Option<runbook_model::ExecutionId>,
    started_by: &Option<String>,
) -> Option<Supersession> {
    superseded_by.as_ref().map(|id| Supersession {
        execution_id: id.clone(),
        started_by: started_by.clone().unwrap_or_default(),
    })
}
