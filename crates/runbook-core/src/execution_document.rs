//! B8d: the execution document. One view backs the live JSON document, the
//! Markdown export and the terminal report, so they can never disagree.
//!
//! - Sections follow runbook order ([`runbook_order`]: iterations right after
//!   their container) with the step path (including iteration indexes), the actor, verdict, outputs, evidence and approval decisions.
//! - Approval decisions keep their provenance: human signatures carry the
//!   directory revision they were authorized against; automatic and agent
//!   decisions are recorded as system policy, never as a human signature.
//! - Titles and approval prompts are rendered per step instance by the
//!   engine (`step_title`, `approval_prompt`), as agent work notices are.
//! - Untrusted text (titles from data, summaries, comments, evidence) is
//!   escaped in Markdown; outputs are rendered as JSON in fenced blocks.
//! - A custom `spec.report` renders only once the run is terminal (D34);
//!   until then the document carries the default pending header.
//! - Log bodies are referenced by URL, not embedded.
use runbook_engine::{ExecutionState, StepState, render_report};
use runbook_model::{
    ActorRef, ApprovalOption, ApprovalView, DecisionRecord, ExecutionDocumentView, ExecutionStatus,
    ExecutionView, PagedLogRef, StepRunId, StepSection, UserId,
};
use runbook_spec::ir::{Node, NodeKind};
use std::collections::{BTreeMap, HashMap};
use std::num::{NonZeroU32, NonZeroU64};

const SCHEMA_REVISION: NonZeroU32 = NonZeroU32::MIN;

/// Build the document from a consistent execution revision. `header` comes
/// from the same read; `log_href` names each step's paged log resource.
pub fn build_document(
    state: &ExecutionState,
    mut header: ExecutionView,
    log_href: impl Fn(&StepRunId) -> String,
) -> ExecutionDocumentView {
    apply_amendment_facts(state, &mut header);
    let sections = runbook_order(state)
        .into_iter()
        .filter_map(|step| {
            let id = step.id.clone()?;
            Some(section(state, step, id, &log_href))
        })
        .collect();
    ExecutionDocumentView {
        header,
        sections,
        trace_project: None,
    }
}

/// The step records in runbook order: the order the steps are written in
/// the runbook (`spec.steps`, then `spec.finally`), with each iteration of
/// a `foreach` or `loop` right after its container, in iteration order.
///
/// The engine keeps records in the order it creates them, and it admits
/// iteration bodies after every step it declares up front, so its order
/// put iterations at the end. The document, the Markdown export and the
/// run view all list steps in this order, so they cannot disagree.
pub fn runbook_order(state: &ExecutionState) -> Vec<&StepState> {
    let ir = state.compiled().ir();
    let mut rank: HashMap<&str, usize> = HashMap::new();
    preorder(&ir.steps, &mut rank);
    preorder(&ir.finally, &mut rank);
    let node_of: HashMap<&str, &str> = state
        .steps
        .iter()
        .map(|step| (step.step_id.as_str(), step.node_id.as_str()))
        .collect();
    let rank_of = |node_id: &str| rank.get(node_id).copied().unwrap_or(usize::MAX);
    // Per enclosing iteration: (container's rank, iteration index); then
    // the step's own rank. A container's key is a prefix of its
    // iterations' keys, so it sorts first; a later sibling's rank is above
    // every rank inside the container, so it sorts after them all.
    let key = |step: &StepState| -> Vec<usize> {
        let mut key = Vec::with_capacity(step.scope.len() * 2 + 1);
        for iteration in &step.scope {
            let container = node_of.get(iteration.parent.as_str()).copied();
            key.push(container.map_or(usize::MAX, rank_of));
            key.push(iteration.index as usize);
        }
        key.push(rank_of(&step.node_id));
        key
    };
    let mut ordered: Vec<&StepState> = state.steps.iter().collect();
    // Stable: records with equal keys (none expected) keep engine order.
    ordered.sort_by_cached_key(|step| key(step));
    ordered
}

/// Pre-order ranks of every declared node (a node before its bodies).
fn preorder<'a>(nodes: &'a [Node], rank: &mut HashMap<&'a str, usize>) {
    for node in nodes {
        let next = rank.len();
        rank.entry(node.id.as_str()).or_insert(next);
        for body in bodies(node) {
            preorder(body, rank);
        }
    }
}

/// A node's child bodies, in the order the runbook writes them.
fn bodies(node: &Node) -> Vec<&[Node]> {
    match &node.kind {
        NodeKind::Group(group) => vec![&group.children, &group.catch, &group.finally],
        NodeKind::Switch(switch) => switch
            .cases
            .iter()
            .map(|case| case.children.as_slice())
            .chain(std::iter::once(switch.default.as_slice()))
            .collect(),
        NodeKind::Parallel { children, .. } => vec![children],
        NodeKind::Foreach(foreach) => vec![&foreach.children],
        NodeKind::Loop(looped) => vec![&looped.children],
        _ => vec![],
    }
}

/// LANG3: who replaced the run, when the replacement is what ended it. A
/// holder that was already failing or finishing its cleanup when the
/// cancellation arrived ends `failed` or `succeeded`; it was not cancelled,
/// so it carries no "replaced by" line (LANG3 re-check, finding 1).
pub fn superseded_by(state: &ExecutionState) -> Option<runbook_model::SupersededBy> {
    use runbook_model::ExecutionStatus as X;
    if matches!(state.status, X::Succeeded | X::Failed) {
        return None;
    }
    state
        .superseded
        .as_ref()
        .map(|s| runbook_model::SupersededBy {
            execution_id: s.execution_id.clone(),
            started_by: s.started_by.clone(),
        })
}

/// B9a amendment facts on a run view, always from state, never from the
/// caller's view: the run-level `amended`/`amendments`/`stale_step_ids`
/// and each listed step's markers. `GET /v1/executions/{exe}` and the
/// document header share this, so they cannot disagree.
pub fn apply_amendment_facts(state: &ExecutionState, view: &mut ExecutionView) {
    // LANG3: who replaced the run, also a state fact the views share.
    view.superseded_by = superseded_by(state);
    // LANG4: the parent/child links, both ways. A child that has not run
    // yet keeps the link its view took from its start record.
    if let Some(parent) = parent_of(state) {
        view.parent = Some(parent);
    }
    view.overdue_steps = if matches!(
        state.status,
        ExecutionStatus::Running | ExecutionStatus::Paused
    ) {
        runbook_order(state)
            .into_iter()
            .filter_map(|step| {
                use runbook_model::StepStatus as S;
                let due_at_ms = step.overdue_since_ms?;
                if matches!(
                    step.status,
                    S::Succeeded | S::Failed | S::Skipped | S::Cancelled | S::TimedOut | S::Expired
                ) {
                    return None;
                }
                Some(runbook_model::OverdueStepView {
                    path: step.step_id.clone(),
                    title: runbook_engine::step_title(state, step)
                        .unwrap_or_else(|| step.node_id.clone()),
                    due_at_ms,
                    step_run_id: step.id.clone(),
                })
            })
            .collect()
    } else {
        vec![]
    };
    view.amended = state.steps.iter().any(|step| step.amended);
    view.amendments = state
        .steps
        .iter()
        .flat_map(|step| step.amendments.iter().cloned())
        .collect();
    view.stale_step_ids = state
        .steps
        .iter()
        .filter(|step| step.stale)
        .filter_map(|step| step.id.clone())
        .collect();
    for listed in &mut view.steps {
        if let Some(step) = state
            .steps
            .iter()
            .find(|step| step.id.as_ref() == Some(&listed.id))
        {
            listed.amended = step.amended;
            listed.amendments = step.amendments.clone();
            listed.stale = step.stale;
            listed.call = call_of(step);
            listed.wait = wait_of(step);
            listed.actor_kind = task_actor_kind(state, step);
            listed.assignees = task_assignees(state, step);
            listed.branch = branch_of(step);
            listed.iterations = iterations_of(step);
            listed.skip_reason = skip_reason_of(step);
            listed.followup = followup_of(step);
            listed.runner = runner_of(state, step);
            listed.cost = step.runner.cost.map(runbook_engine::RecordedCost::to_cost);
        }
    }
    // R0-6: the run's cost is its steps' (every step, listed or not).
    view.cost = state
        .steps
        .iter()
        .filter_map(|step| step.runner.cost.as_ref())
        .fold(None, |total: Option<runbook_engine::RecordedCost>, cost| {
            let mut total = total.unwrap_or_default();
            total.add(cost);
            Some(total)
        })
        .map(runbook_engine::RecordedCost::to_cost);
    view.progress = Some(progress_of(state));
}

/// UI-2: how many of the run's top-level steps (`spec.steps`) finished.
/// Each is counted from its own record, so a container counts once it
/// finishes, not as its children do.
pub fn progress_of(state: &ExecutionState) -> runbook_model::ExecutionProgressView {
    let top = &state.compiled().ir().steps;
    let done = top
        .iter()
        .filter(|node| {
            state.steps.iter().any(|step| {
                step.scope.is_empty()
                    && step.node_id == node.id
                    && matches!(
                        step.status,
                        runbook_model::StepStatus::Succeeded
                            | runbook_model::StepStatus::Failed
                            | runbook_model::StepStatus::Skipped
                            | runbook_model::StepStatus::Cancelled
                            | runbook_model::StepStatus::TimedOut
                            | runbook_model::StepStatus::Expired
                    )
            })
        })
        .count();
    runbook_model::ExecutionProgressView {
        done: u32::try_from(done).unwrap_or(u32::MAX),
        total: u32::try_from(top.len()).unwrap_or(u32::MAX),
    }
}

/// The runner agent a task or approval targets, as resolved at publish.
pub fn runner_of(
    state: &ExecutionState,
    step: &StepState,
) -> Option<runbook_model::StepRunnerView> {
    let (target, limits) = match &state.node(&step.step_id)?.kind {
        NodeKind::Task(task) => (&task.agent_target, &task.agent_limits),
        NodeKind::Approval(approval) => (&approval.agent_target, &approval.agent_limits),
        _ => return None,
    };
    let Some(runbook_spec::ir::AgentTarget::Runner { agent_id, name, .. }) = target else {
        return None;
    };
    let runner = &step.runner;
    let approval = step.approval.as_ref();
    Some(runbook_model::StepRunnerView {
        agent_id: agent_id.clone(),
        agent_name: name.clone(),
        trace_id: runner.trace_id.clone(),
        model: limits.as_ref().and_then(|l| l.model.clone()),
        agent_version_id: runner.agent_version_id.clone(),
        waiting_reason: runner.waiting_reason.clone(),
        waiting_detail: runner.waiting_detail.clone(),
        releases: runner
            .releases
            .iter()
            .map(|r| runbook_model::StepCredentialRelease {
                credential_id: r.credential_id.clone(),
                credential_version: r.credential_version,
                delivery: r.delivery.clone(),
                runner_id: r.runner_id.clone(),
                at: r.at_ms,
                revoked: r.revoked.clone(),
            })
            .collect(),
        released_never_reported: runner.released_never_reported,
        decision_ready_at_ms: approval
            .and_then(|a| a.requested.as_ref())
            .and_then(|r| r.request.agent_ready_at_ms),
        decision_lease: approval.and_then(|a| a.decision_lease.clone()),
        agent_notes: approval.map(|a| a.agent_notes.clone()).unwrap_or_default(),
    })
}

/// UI-2: a task's declared actor; `None` for other steps.
pub fn task_actor_kind(
    state: &ExecutionState,
    step: &StepState,
) -> Option<runbook_model::StepActorKind> {
    let NodeKind::Task(task) = &state.node(&step.step_id)?.kind else {
        return None;
    };
    Some(match task.actor {
        runbook_spec::ir::Actor::Human => runbook_model::StepActorKind::Human,
        runbook_spec::ir::Actor::Agent => runbook_model::StepActorKind::Agent,
        runbook_spec::ir::Actor::Any => runbook_model::StepActorKind::Any,
    })
}

/// UI-2: a task's resolved assignee (empty for other steps).
pub fn task_assignees(
    state: &ExecutionState,
    step: &StepState,
) -> Vec<runbook_model::RecipientRef> {
    match state.node(&step.step_id).map(|node| &node.kind) {
        Some(NodeKind::Task(task)) => assignees_of(state, task),
        _ => vec![],
    }
}

/// A task's `assignee` resolved through the compiled directory pins: the
/// recipients its inbox work goes to (B8f). Unresolved or absent: none.
pub fn assignees_of(
    state: &ExecutionState,
    task: &runbook_spec::ir::Task,
) -> Vec<runbook_model::RecipientRef> {
    use runbook_model::RecipientRef;
    use runbook_spec::ir::{Assignee, Recipient};
    match &task.assignee {
        Some(Assignee::Recipient(Recipient::User(slug))) => state
            .compiled()
            .users()
            .get(slug)
            .map(|user| vec![RecipientRef::User(user.clone())])
            .unwrap_or_default(),
        Some(Assignee::Recipient(Recipient::Team(slug))) => state
            .compiled()
            .teams()
            .get(slug)
            .map(|team| vec![RecipientRef::Team(team.to_string())])
            .unwrap_or_default(),
        _ => vec![],
    }
}

/// UI-2: a waiting `wait` step's signal or timer; `None` otherwise.
pub fn wait_of(step: &StepState) -> Option<runbook_model::StepWaitView> {
    if step.status != runbook_model::StepStatus::Waiting {
        return None;
    }
    let wait = step.wait.as_ref()?;
    Some(match &wait.target {
        runbook_engine::WaitTarget::Time(at_ms) => runbook_model::StepWaitView {
            signal: None,
            until_ms: Some(*at_ms),
        },
        runbook_engine::WaitTarget::Signal(name) => runbook_model::StepWaitView {
            signal: Some(name.clone()),
            until_ms: wait.timeout_at_ms,
        },
    })
}

/// UI-3: the branch a `switch` took; `None` before it chose and for
/// other steps.
pub fn branch_of(step: &StepState) -> Option<runbook_model::StepBranchView> {
    step.branch.map(|choice| match choice {
        runbook_engine::BranchChoice::Case(index) => runbook_model::StepBranchView::Case { index },
        runbook_engine::BranchChoice::Default => runbook_model::StepBranchView::Default,
    })
}

/// UI-3: a started `foreach` (with its item count) or `loop`'s iterations.
pub fn iterations_of(step: &StepState) -> Option<runbook_model::StepIterationsView> {
    if let Some(snapshot) = &step.foreach {
        return Some(runbook_model::StepIterationsView {
            started: snapshot.admitted,
            total: Some(u32::try_from(snapshot.keys.len()).unwrap_or(u32::MAX)),
            labels: item_labels(&snapshot.items),
        });
    }
    step.loop_state
        .as_ref()
        .map(|record| runbook_model::StepIterationsView {
            started: record.admitted,
            total: None,
            labels: vec![],
        })
}

/// A foreach's items as short text, when every one is a scalar.
fn item_labels(items: &serde_json::Value) -> Vec<String> {
    const MAX: usize = 80;
    let Some(items) = items.as_array() else {
        return vec![];
    };
    let mut labels = Vec::with_capacity(items.len());
    for item in items {
        let text = match item {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            _ => return vec![],
        };
        labels.push(if text.chars().count() > MAX {
            let cut: String = text.chars().take(MAX - 1).collect();
            format!("{cut}…")
        } else {
            text
        });
    }
    labels
}

/// UI-3: why the engine skipped a step.
pub fn skip_reason_of(step: &StepState) -> Option<runbook_model::StepSkipReason> {
    if step.status != runbook_model::StepStatus::Skipped {
        return None;
    }
    step.skip_reason.map(|reason| match reason {
        runbook_engine::SkipReason::Condition => runbook_model::StepSkipReason::Condition,
    })
}

/// LANG9: reminders already sent and a follow-up escalation for a step's
/// current attempt. `None` until either follow-up has happened.
pub fn followup_of(step: &StepState) -> Option<runbook_model::StepFollowupView> {
    let escalation = step.followup_escalation.as_ref();
    if step.reminders_sent == 0 && escalation.is_none() {
        return None;
    }
    Some(runbook_model::StepFollowupView {
        reminders_sent: step.reminders_sent,
        escalated: escalation.is_some(),
        escalated_to: escalation
            .map(|snapshot| snapshot.recipients.clone())
            .unwrap_or_default(),
    })
}

/// LANG4: the `call` step that started this run, when it is a child.
pub fn parent_of(state: &ExecutionState) -> Option<runbook_model::ExecutionParentView> {
    state.parent.as_ref().map(parent_view)
}

/// LANG4: a parent link as the views show it.
pub fn parent_view(link: &runbook_engine::ParentLink) -> runbook_model::ExecutionParentView {
    runbook_model::ExecutionParentView {
        execution_id: link.execution_id.clone(),
        step_run_id: link.step_run_id.clone(),
        step: link.step_id.clone(),
        depth: link.depth,
    }
}

/// LANG4: a `call` step's child.
pub fn call_of(step: &StepState) -> Option<runbook_model::StepCallView> {
    step.call.as_ref().map(|c| runbook_model::StepCallView {
        child_execution_id: c.child_execution_id.clone(),
        // An outcome from the child proves it started, even when its start
        // record had not reached the parent yet.
        started: c.started
            || (c.completed
                && matches!(
                    c.error.as_deref(),
                    None | Some(
                        "call.child_failed" | "call.child_cancelled" | "call.outputs_invalid"
                    )
                )),
        error: c.error.clone(),
    })
}

fn section(
    state: &ExecutionState,
    step: &StepState,
    id: StepRunId,
    log_href: &impl Fn(&StepRunId) -> String,
) -> StepSection {
    StepSection {
        path: path_of(step),
        title: runbook_engine::step_title(state, step).unwrap_or_else(|| step.step_id.clone()),
        status: step.status,
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
        verdict: step.verdict,
        actor: step.actor.clone(),
        summary: step.summary.clone(),
        outputs: step.outputs.clone(),
        evidence: step.evidence.clone(),
        approval: approval_view(state, step, &id),
        memory_read: memory_read(step),
        memory_proposed: step
            .memory_proposed
            .iter()
            .map(|id| runbook_model::MemoryProposedRef {
                memory_id: id.clone(),
                dropped: None,
            })
            .collect(),
        memory_saved: step
            .memory_saved
            .iter()
            .map(|id| runbook_model::MemoryProposedRef {
                memory_id: id.clone(),
                dropped: None,
            })
            .collect(),
        memory_edited: step
            .memory_edited
            .iter()
            .map(|id| runbook_model::MemoryProposedRef {
                memory_id: id.clone(),
                dropped: None,
            })
            .collect(),
        amended: step.amended,
        amendments: step.amendments.clone(),
        stale: step.stale,
        call: call_of(step),
        assignee: assignee_of(state, step),
        task_actor: match state.node(&step.step_id).map(|node| &node.kind) {
            Some(NodeKind::Task(task)) => Some(match task.actor {
                runbook_spec::ir::Actor::Human => runbook_model::TaskActor::Human,
                runbook_spec::ir::Actor::Agent => runbook_model::TaskActor::Agent,
                runbook_spec::ir::Actor::Any => runbook_model::TaskActor::Any,
            }),
            _ => None,
        },
        labels: state
            .node(&step.step_id)
            .map(|node| node.labels.clone())
            .unwrap_or_default(),
        failure_type: matches!(
            step.status,
            runbook_model::StepStatus::Failed
                | runbook_model::StepStatus::TimedOut
                | runbook_model::StepStatus::Expired
        )
        .then(|| runbook_engine::failure_type(state, step).ok())
        .flatten()
        .map(|kind| kind.as_str().to_owned()),
        taken_over: step
            .taken_over
            .as_ref()
            .map(|taken| runbook_model::TakenOverView {
                by: taken.by.clone(),
                at_ms: taken.at_ms,
                previous_actor_kind: taken.previous_actor_kind,
            }),
        followup: followup_of(step),
        logs: PagedLogRef {
            href: log_href(&id),
            next_cursor: None,
        },
        step_run_id: id,
    }
}

/// LANG7: a task's assignee: as written, or an expression's resolution for
/// the current attempt (strict resolution or handoff), with its mode.
fn assignee_of(
    state: &ExecutionState,
    step: &StepState,
) -> Option<runbook_model::StepAssigneeView> {
    use runbook_spec::ir::{Assignee, Recipient};
    let NodeKind::Task(task) = &state.node(&step.step_id)?.kind else {
        return None;
    };
    let text = |r: &Recipient| match r {
        Recipient::User(name) => format!("user:{name}"),
        Recipient::Team(name) => format!("team:{name}"),
    };
    let to = match task.assignee.as_ref()? {
        Assignee::Recipient(recipient) => Some(text(recipient)),
        Assignee::Expression(_) => step
            .strict_assignee
            .as_ref()
            .and_then(|r| r.request.assignee.as_ref())
            .or_else(|| {
                step.handoff
                    .as_ref()
                    .and_then(|h| h.request.assignee.as_ref())
            })
            .map(text),
    };
    let strict = task.assignment.is_strict();
    let resolved = step
        .strict_assignee
        .as_ref()
        .filter(|r| strict && r.request.attempt == step.attempt);
    Some(runbook_model::StepAssigneeView {
        to,
        strict,
        recipients: resolved
            .map(|r| r.eligible_recipients.clone())
            .unwrap_or_default(),
        routing_error: step
            .strict_assignee
            .as_ref()
            .filter(|_| strict && step.handoff.is_none())
            .and(step.routing_error.clone()),
    })
}

/// `step`, `parent[2].step` or `loop#2.step`: the engine already records an
/// iteration's full display path as its step ID (outermost first).
pub fn path_of(step: &StepState) -> String {
    step.step_id.clone()
}

fn approval_view(state: &ExecutionState, step: &StepState, id: &StepRunId) -> Option<ApprovalView> {
    let approval = step.approval.as_ref()?;
    let NodeKind::Approval(definition) = &state.node(&step.step_id)?.kind else {
        return None;
    };
    let excluded = runbook_engine::excluded_completers(state, step);
    let requested = approval
        .requested
        .as_ref()
        .map(|r| r.recipients.as_slice())
        .unwrap_or_default();
    let escalated = approval
        .escalation_notice_recipients
        .as_deref()
        .unwrap_or_default();
    let all_excluded = !(requested.is_empty() && escalated.is_empty())
        && requested.iter().chain(escalated).all(|recipient| {
            matches!(recipient, runbook_model::RecipientRef::User(id) if excluded.contains(id))
        });
    Some(ApprovalView {
        excluded_completers: excluded.into_iter().collect(),
        blocked_reason: if !matches!(
            step.status,
            runbook_model::StepStatus::Waiting | runbook_model::StepStatus::Expired
        ) {
            None
        } else if runbook_engine::independent_review_missing_report(state, step) {
            Some("approval.review_missing_report".into())
        } else if all_excluded {
            Some("approval.no_independent_approver".into())
        } else if runbook_engine::independent_review_unknown_service(state, step) {
            Some("approval.unknown_service_contributor".into())
        } else {
            approval
                .automatic_withheld
                .as_ref()
                .filter(|reason| reason.as_str() == "approval.independent_review_required")
                .cloned()
        },
        schema_revision: SCHEMA_REVISION,
        step_run_id: id.clone(),
        status: step.status,
        prompt: runbook_engine::approval_prompt(state, step)
            .unwrap_or_else(|| definition.prompt.clone()),
        options: definition
            .options
            .iter()
            .map(|option| ApprovalOption {
                id: option.id.clone(),
                title: option.title.clone(),
                auto: option.auto,
            })
            .collect(),
        eligible_recipients: approval
            .requested
            .as_ref()
            .map(|requested| requested.recipients.clone())
            .unwrap_or_default(),
        escalation_recipients: approval
            .escalation
            .as_ref()
            .map(|escalation| escalation.recipients.clone())
            .unwrap_or_default(),
        escalated: approval.escalation.is_some(),
        quorum: NonZeroU32::new(definition.min).unwrap_or(NonZeroU32::MIN),
        decisions: decisions_of(approval),
        decision_lease: approval.decision_lease.clone(),
        agent_notes: approval.agent_notes.clone(),
    })
}

/// Approval decisions with provenance: human signatures carry the directory
/// revision they were authorized against; automatic decisions are system
/// policy and agent decisions carry agent provenance, never a human
/// signature (D26).
pub(crate) fn decisions_of(approval: &runbook_engine::ApprovalState) -> Vec<DecisionRecord> {
    let mut decisions: Vec<DecisionRecord> = approval
        .votes
        .iter()
        .filter_map(|signature| {
            let ActorRef::Human { user_id } = &signature.vote.actor else {
                return None;
            };
            Some(DecisionRecord::Human {
                user_id: user_id.clone(),
                membership_revision: NonZeroU64::new(signature.vote.eligibility.directory_revision)
                    .unwrap_or(NonZeroU64::MIN),
                option: signature.vote.option.clone(),
                comment: signature.vote.comment.clone(),
            })
        })
        .collect();
    if let Some(automatic) = &approval.automatic_decision {
        decisions.push(DecisionRecord::System {
            policy: "automatic".into(),
            option: automatic.option.clone(),
        });
    }
    if let Some(agent) = &approval.agent_decision {
        decisions.push(agent_record(agent));
    }
    decisions
}

/// An agent decision with its provenance (F94).
pub(crate) fn agent_record(agent: &runbook_engine::AgentDecision) -> DecisionRecord {
    DecisionRecord::Agent {
        principal: agent
            .principal
            .starts_with("runner:")
            .then(|| agent.principal.clone()),
        evidence: agent.evidence.clone(),
        evidence_source: (agent.principal.starts_with("runner:") && !agent.evidence.is_empty())
            .then_some(runbook_model::AgentEvidenceSource::RunnerReported),
        agent_name: agent.agent_name.clone(),
        model: agent.model.clone(),
        session_id: agent.session_id.clone(),
        option: agent.option.clone(),
        comment: agent.comment.clone(),
    }
}

fn terminal(status: ExecutionStatus) -> bool {
    matches!(
        status,
        ExecutionStatus::Succeeded | ExecutionStatus::Failed | ExecutionStatus::Cancelled
    )
}

/// The custom `spec.report` Markdown, only once the run is terminal (D34).
pub(crate) fn terminal_report(state: &ExecutionState) -> Option<String> {
    if !terminal(state.status) {
        return None;
    }
    render_report(state)
        .ok()
        .flatten()
        .map(|report| report.markdown)
}

/// The Markdown export of the same document. The custom report renders only
/// for terminal runs (D34); otherwise the default pending header is shown.
pub fn render_markdown(state: &ExecutionState, document: &ExecutionDocumentView) -> String {
    render_markdown_named(state, document, &BTreeMap::new())
}

/// [`render_markdown`] with people named the way the page names them: a user
/// in `names` by name, anyone else by id. Names are escaped like any data.
pub fn render_markdown_named(
    state: &ExecutionState,
    document: &ExecutionDocumentView,
    names: &BTreeMap<UserId, String>,
) -> String {
    let header = &document.header;
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", escape(&header.title)));
    out.push_str(&format!(
        "Status: **{}**\n\n",
        escape(&serde_label(&header.status))
    ));
    if let Some(superseded) = &header.superseded_by {
        out.push_str(&format!(
            "Cancelled: replaced by {} started by {}\n\n",
            escape(superseded.execution_id.as_str()),
            principal_label(&superseded.started_by, names)
        ));
    }
    if let Some(parent) = &header.parent {
        out.push_str(&format!(
            "Called by: {} (step {})\n\n",
            escape(parent.execution_id.as_str()),
            escape(&parent.step)
        ));
    }
    for notice in &header.notifications_not_delivered {
        out.push_str(&format!(
            "> **Notification not delivered:** {} ({})\n\n",
            escape(&notice.event_type),
            escape(&notice.reason)
        ));
    }
    if terminal(state.status) {
        if let Ok(Some(report)) = render_report(state) {
            out.push_str("## Report\n\n");
            out.push_str(&report.markdown);
            out.push_str("\n\n");
        }
    } else {
        out.push_str("_This run is in progress. The final report appears when it completes._\n\n");
    }
    for section in &document.sections {
        out.push_str(&format!(
            "## {} ({})\n\n",
            escape(&section.title),
            escape(&section.path)
        ));
        out.push_str(&format!("- Status: {}\n", serde_label(&section.status)));
        for (label, writes) in [
            ("Memory proposed", &section.memory_proposed),
            ("Memory saved", &section.memory_saved),
            ("Memory edited", &section.memory_edited),
        ] {
            for write in writes {
                out.push_str(&format!(
                    "- {label}: {}{}\n",
                    write.memory_id,
                    write
                        .dropped
                        .as_deref()
                        .map(|code| format!(" (dropped: {})", escape(code)))
                        .unwrap_or_default()
                ));
            }
        }
        if let Some(verdict) = &section.verdict {
            out.push_str(&format!("- Verdict: {}\n", serde_label(verdict)));
        }
        if let Some(actor) = &section.actor {
            out.push_str(&format!("- Actor: {}\n", actor_label(actor, names)));
        }
        if let Some(followup) = &section.followup {
            if followup.reminders_sent == 1 {
                out.push_str("- Reminded: 1 time\n");
            } else if followup.reminders_sent > 1 {
                out.push_str(&format!("- Reminded: {} times\n", followup.reminders_sent));
            }
            if followup.escalated {
                if followup.escalated_to.is_empty() {
                    out.push_str("- Escalated\n");
                } else {
                    out.push_str(&format!(
                        "- Escalated to: {}\n",
                        followup
                            .escalated_to
                            .iter()
                            .map(|recipient| recipient_label(recipient, names))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
        if let Some(taken) = &section.taken_over {
            out.push_str(&format!(
                "- Taken over by {} (from {})\n",
                actor_label(&taken.by, names),
                serde_label(&taken.previous_actor_kind)
            ));
        }
        if let Some(assignee) = section.assignee.as_ref().filter(|a| a.strict) {
            out.push_str(&format!(
                "- Assignee (strict): {}{}\n",
                escape(assignee.to.as_deref().unwrap_or("unresolved")),
                assignee
                    .routing_error
                    .as_deref()
                    .map(|code| format!(" ({})", escape(code)))
                    .unwrap_or_default()
            ));
        }
        if let Some(call) = &section.call {
            out.push_str(&format!(
                "- Child run: {}{}\n",
                escape(call.child_execution_id.as_str()),
                call.error
                    .as_deref()
                    .map(|code| format!(" ({})", escape(code)))
                    .unwrap_or_default()
            ));
        }
        out.push('\n');
        if let Some(summary) = &section.summary {
            out.push_str(&format!("> {}\n\n", escape(summary).replace('\n', "\n> ")));
        }
        if !section.outputs.is_empty() {
            let json = serde_json::to_string_pretty(&section.outputs).unwrap_or_default();
            out.push_str(&format!(
                "```json\n{}\n```\n\n",
                json.replace("```", "`\u{200b}``")
            ));
        }
        if let Some(approval) = &section.approval {
            out.push_str(&format!("**Approval:** {}\n\n", escape(&approval.prompt)));
            for decision in &approval.decisions {
                let line = match decision {
                    DecisionRecord::Human {
                        user_id,
                        option,
                        comment,
                        ..
                    } => format!(
                        "- {} chose {}{}",
                        person(user_id, names),
                        escape(option),
                        comment
                            .as_deref()
                            .map(|c| format!(": {}", escape(c)))
                            .unwrap_or_default()
                    ),
                    DecisionRecord::System { policy, option } => format!(
                        "- System policy ({}) chose {}",
                        escape(policy),
                        escape(option)
                    ),
                    DecisionRecord::Agent {
                        agent_name,
                        model,
                        option,
                        comment,
                        evidence,
                        evidence_source,
                        ..
                    } => format!(
                        "- Agent {}{} chose {}{}{}",
                        escape(agent_name),
                        model
                            .as_deref()
                            .map(|m| format!(" ({})", escape(m)))
                            .unwrap_or_default(),
                        escape(option),
                        comment
                            .as_deref()
                            .map(|c| format!(": {}", escape(c)))
                            .unwrap_or_default(),
                        if evidence.is_empty() {
                            String::new()
                        } else {
                            format!(
                                " ({}evidence: {})",
                                if evidence_source.is_some() {
                                    "runner-reported "
                                } else {
                                    ""
                                },
                                evidence
                                    .iter()
                                    .map(|e| escape(e))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        }
                    ),
                };
                out.push_str(&line);
                out.push('\n');
            }
            // R1-D2: a runner agent reviewing now, and its abstentions.
            if let Some(lease) = &approval.decision_lease {
                out.push_str(&format!(
                    "- Agent {} is reviewing; people can still decide\n",
                    escape(&lease.agent_name)
                ));
            }
            for note in &approval.agent_notes {
                out.push_str(&format!(
                    "- Agent {}{} abstained: {}\n",
                    escape(&note.agent_name),
                    note.model
                        .as_deref()
                        .map(|m| format!(" ({})", escape(m)))
                        .unwrap_or_default(),
                    escape(&note.reason)
                ));
            }
            out.push('\n');
        }
        // UP1: uploads are listed by `evd_` ID with their verified type.
        let files = state
            .steps
            .iter()
            .find(|s| s.id.as_ref() == Some(&section.step_run_id))
            .map_or(&[][..], |s| &s.files[..]);
        for evidence in &section.evidence {
            match files.iter().find(|f| f.id.as_str() == evidence) {
                Some(file) => out.push_str(&format!(
                    "- Evidence: {} ({})\n",
                    escape(evidence),
                    escape(&file.content_type)
                )),
                None => out.push_str(&format!("- Evidence: {}\n", escape(evidence))),
            }
        }
        out.push_str(&format!("[Logs]({})\n\n", section.logs.href));
    }
    out
}

/// An escaped actor label: a named person by name, otherwise `human <id>`.
fn actor_label(actor: &ActorRef, names: &BTreeMap<UserId, String>) -> String {
    match actor {
        ActorRef::Human { user_id } => match names.get(user_id) {
            Some(name) => escape(name),
            None => escape(&format!("human {user_id}")),
        },
        ActorRef::Agent { name, model, .. } => escape(&match model {
            Some(model) => format!("agent {name} ({model})"),
            None => format!("agent {name}"),
        }),
        ActorRef::System => "system".into(),
    }
}

/// An escaped notification recipient: a named person or `team:<reference>`.
fn recipient_label(
    recipient: &runbook_model::RecipientRef,
    names: &BTreeMap<UserId, String>,
) -> String {
    match recipient {
        runbook_model::RecipientRef::User(user_id) => person(user_id, names),
        runbook_model::RecipientRef::Team(reference) => escape(&format!("team:{reference}")),
    }
}

/// An escaped principal (`user:…`, `service:…`): a person's name when known.
fn principal_label(principal: &str, names: &BTreeMap<UserId, String>) -> String {
    match principal
        .strip_prefix("user:")
        .and_then(|id| id.parse::<UserId>().ok())
    {
        Some(user_id) => person(&user_id, names),
        None => escape(principal),
    }
}

/// An escaped person: their name when known, otherwise their id.
fn person(user_id: &UserId, names: &BTreeMap<UserId, String>) -> String {
    escape(names.get(user_id).map_or(user_id.as_str(), String::as_str))
}

fn serde_label<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        _ => String::new(),
    }
}

/// Escape untrusted text for inline Markdown: no markup, links, HTML or
/// headings can be injected through data.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '>' | '~' => {
                out.push('\\');
                out.push(c);
            }
            '<' => out.push_str("&lt;"),
            '&' => out.push_str("&amp;"),
            '\r' => {}
            c if c.is_control() && c != '\n' => {}
            c => out.push(c),
        }
    }
    out
}

/// Mark the proposals and live saves (MEM6) the catalog dropped (an
/// archived marker naming the reason, D1's race with a full quota) so the
/// document says so. Reads the catalog only for sections that wrote memory.
pub async fn mark_dropped_proposals<M: crate::memory_entries::MemoryStore>(
    store: &M,
    scope: &crate::catalog::TenantScope,
    sections: &mut [runbook_model::StepSection],
) -> Result<(), crate::StoreError> {
    for section in sections {
        for proposal in section
            .memory_proposed
            .iter_mut()
            .chain(section.memory_saved.iter_mut())
        {
            proposal.dropped = store
                .get_memory(scope, &proposal.memory_id)
                .await?
                .and_then(|entry| entry.dropped);
        }
    }
    Ok(())
}

/// D41: each (entry, revision) a step's reads returned, once, in first-read
/// order. A later edit adds a revision; it never rewrites a recorded one.
fn memory_read(step: &StepState) -> Vec<runbook_model::MemoryReadRef> {
    let mut seen = Vec::new();
    for read in &step.memory_read {
        if !seen.contains(read) {
            seen.push(read.clone());
        }
    }
    seen
}

#[cfg(test)]
mod superseded_tests {
    use super::superseded_by;
    use runbook_engine::{ExecutionState, Supersession};
    use runbook_model::ExecutionStatus as X;
    use std::rc::Rc;

    fn state(status: X) -> ExecutionState {
        let spec = runbook_spec::parse(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: t, title: T}\nspec:\n  steps:\n    - {id: a, title: A}\n",
        )
        .unwrap();
        let compiled = runbook_spec::compile(&spec, &Default::default()).unwrap();
        let mut state = ExecutionState::new(
            "exe_00000000000000000000000001".parse().unwrap(),
            Rc::new(compiled),
        );
        state.status = status;
        state.superseded = Some(Supersession {
            execution_id: "exe_00000000000000000000000002".parse().unwrap(),
            started_by: "user:usr_00000000000000000000000001".into(),
        });
        state
    }

    #[test]
    fn only_runs_the_replacement_ended_carry_the_trail() {
        assert!(superseded_by(&state(X::Cancelled)).is_some());
        assert!(
            superseded_by(&state(X::Running)).is_some(),
            "still cleaning up"
        );
        assert_eq!(superseded_by(&state(X::Failed)), None);
        assert_eq!(superseded_by(&state(X::Succeeded)), None);
    }
}
