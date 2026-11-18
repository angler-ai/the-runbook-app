//! Read-only presentation from the pinned compiler and a concrete step instance.
use crate::{EngineError, ExecutionState};
use runbook_expr::ReadRef;
use runbook_model::{ActorKind, StepRunId};
use runbook_spec::ir::NodeKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenderedStep {
    pub instructions: String,
    pub checklist: Vec<String>,
    /// Untrusted parameter data, never appended to instructions or executed.
    #[serde(with = "runbook_spec::stored_value")]
    pub skill_parameters: Value,
    #[serde(with = "runbook_spec::stored_value")]
    pub output_schema: Value,
    pub reads: Vec<ReadRef>,
}

/// Authorization and publication of the read audit belong to the runtime.
pub fn render_step(
    state: &ExecutionState,
    id: &StepRunId,
    audience: ActorKind,
) -> Result<RenderedStep, EngineError> {
    let step = &state.steps[state.step_index(id)?];
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("rendered node is absent"))?;
    let NodeKind::Task(task) = &node.kind else {
        return Err(EngineError::new("step.not_task"));
    };
    let mut reads = vec![];
    let mut text = |suffix: &str| {
        crate::data::evaluate_scoped_path(
            state,
            &step.scope,
            &format!("{}/{suffix}", node.source_path),
            &mut reads,
        )?
        .as_str()
        .map(str::to_owned)
        .ok_or(EngineError::new("expression.type_error"))
    };
    let instructions = runbook_spec::render::instruction_site(task.instructions.as_ref(), audience)
        .map(&mut text)
        .transpose()?
        .unwrap_or_default();
    let checklist = (0..task.checklist.len())
        .map(|index| text(&format!("checklist/{index}")))
        .collect::<Result<Vec<_>, _>>()?;
    let skill_parameters = task
        .parameters
        .keys()
        .map(|key| {
            let escaped = key.replace('~', "~0").replace('/', "~1");
            let value = crate::data::evaluate_scoped_path(
                state,
                &step.scope,
                &format!("{}/with/{escaped}", node.source_path),
                &mut reads,
            )?;
            Ok((key.clone(), value))
        })
        .collect::<Result<serde_json::Map<_, _>, EngineError>>()?;
    Ok(RenderedStep {
        instructions,
        checklist,
        skill_parameters: Value::Object(skill_parameters),
        output_schema: runbook_spec::schema_json(&task.outputs),
        reads,
    })
}

/// F62: the step's instructions (for `audience`, the same site as
/// [`render_step`]) and checklist as the author wrote them, and the run data
/// their placeholders resolve to, kept apart. Pure: it reads only the state.
pub fn step_template(
    state: &ExecutionState,
    id: &StepRunId,
    audience: ActorKind,
) -> Result<runbook_model::StepTemplate, EngineError> {
    let step = &state.steps[state.step_index(id)?];
    let node = state
        .node(&step.step_id)
        .ok_or(EngineError::invariant("rendered node is absent"))?;
    let NodeKind::Task(task) = &node.kind else {
        return Err(EngineError::new("step.not_task"));
    };
    let mut values = runbook_model::Data::new();
    let mut part = |suffix: &str| {
        crate::data::template_parts(
            state,
            &step.scope,
            &format!("{}/{suffix}", node.source_path),
            &mut values,
        )
    };
    let instructions = runbook_spec::render::instruction_site(task.instructions.as_ref(), audience)
        .map(&mut part)
        .transpose()?
        .unwrap_or_default();
    let checklist = (0..task.checklist.len())
        .map(|index| part(&format!("checklist/{index}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(runbook_model::StepTemplate {
        instructions,
        checklist,
        values,
    })
}

/// The display title of one step instance: its `title` template rendered
/// in the step's own scope (iteration bindings such as a foreach alias),
/// or `None` when the node has no title. Every surface that names a step
/// instance (agent work notices, the resume brief, the execution document,
/// inbox assignments) uses this, so they agree. Pure and deterministic: it
/// reads only the state and its pinned checked templates. A template that
/// fails to evaluate falls back to the raw title.
pub fn step_title(state: &ExecutionState, step: &crate::StepState) -> Option<String> {
    rendered_title(state, step, &mut vec![])
}

/// `step_title`, appending the template's reads to `reads` only when it
/// rendered (the agent work notice records them).
pub(crate) fn rendered_title(
    state: &ExecutionState,
    step: &crate::StepState,
    reads: &mut Vec<ReadRef>,
) -> Option<String> {
    let node = state.node(&step.step_id)?;
    let raw = node.title.as_deref()?;
    Some(rendered_text(
        state,
        step,
        &node.source_path,
        "title",
        raw,
        reads,
    ))
}

/// The approval prompt of one approval step instance, rendered like
/// `step_title` (raw prompt on evaluation failure); `None` for other nodes.
pub fn approval_prompt(state: &ExecutionState, step: &crate::StepState) -> Option<String> {
    let node = state.node(&step.step_id)?;
    let NodeKind::Approval(approval) = &node.kind else {
        return None;
    };
    Some(rendered_text(
        state,
        step,
        &node.source_path,
        "approval/prompt",
        &approval.prompt,
        &mut vec![],
    ))
}

fn rendered_text(
    state: &ExecutionState,
    step: &crate::StepState,
    source_path: &str,
    suffix: &str,
    raw: &str,
    reads: &mut Vec<ReadRef>,
) -> String {
    let mut own = vec![];
    match crate::data::evaluate_scoped_path(
        state,
        &step.scope,
        &format!("{source_path}/{suffix}"),
        &mut own,
    ) {
        Ok(Value::String(text)) => {
            crate::data::append_reads(reads, own);
            text
        }
        _ => raw.to_owned(),
    }
}

/// Rendered Markdown is a read result, not a separate persisted report model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedMarkdown {
    pub markdown: String,
    pub reads: Vec<ReadRef>,
}
pub fn render_report(state: &ExecutionState) -> Result<Option<RenderedMarkdown>, EngineError> {
    if state.compiled().ir().report.is_none() {
        return Ok(None);
    }
    if !matches!(
        state.status,
        runbook_model::ExecutionStatus::Succeeded
            | runbook_model::ExecutionStatus::Failed
            | runbook_model::ExecutionStatus::Cancelled
    ) {
        return Err(EngineError::new("execution.not_complete"));
    }
    let mut reads = vec![];
    let value = crate::data::evaluate_path(state, "/spec/report", &mut reads)?;
    let markdown = value
        .as_str()
        .ok_or(EngineError::new("expression.type_error"))?
        .to_owned();
    Ok(Some(RenderedMarkdown { markdown, reads }))
}
