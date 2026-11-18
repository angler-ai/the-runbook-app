//! LANG9b: what a webhook trigger's call starts. The payload is reduced to
//! the fields its schema declares (others are ignored), checked against the
//! schema, then `when` decides whether to start, `inputs` maps it to the
//! runbook's inputs and `dedupe` names the call for duplicate suppression.
use crate::EngineError;
use runbook_model::{Data, FieldError};
use runbook_spec::{DataSchema, DataType, ir::WebhookTrigger};
use serde_json::{Map, Value};

/// The most a trigger's expressions may cost together.
const BUDGET: u64 = 100_000;

#[derive(Clone, Debug, PartialEq)]
pub enum WebhookOutcome {
    /// Start a run with these inputs; `dedupe` keys duplicate suppression.
    Start {
        inputs: Data,
        dedupe: Option<String>,
    },
    /// `when` was false: nothing starts.
    Skipped,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WebhookError {
    /// The payload doesn't match the declared schema (422).
    InvalidPayload(Vec<FieldError>),
    /// An expression failed to evaluate (422).
    Evaluation(EngineError),
}

/// Keep only what `schema` declares, at every object level.
fn declared(schema: &DataSchema, value: &Value) -> Value {
    match (&schema.kind, value) {
        (DataType::Object { properties }, Value::Object(map)) => Value::Object(
            map.iter()
                .filter_map(|(k, v)| properties.get(k).map(|s| (k.clone(), declared(s, v))))
                .collect::<Map<_, _>>(),
        ),
        (DataType::List { items }, Value::Array(list)) => {
            Value::Array(list.iter().map(|v| declared(items, v)).collect())
        }
        _ => value.clone(),
    }
}

pub fn webhook_start(
    trigger: &WebhookTrigger,
    payload: &Value,
    now_ms: i64,
) -> Result<WebhookOutcome, WebhookError> {
    let payload = match &trigger.payload {
        Some(schema) => {
            let reduced = declared(schema, payload);
            let errors = runbook_spec::validate_data(schema, &reduced);
            if !errors.is_empty() {
                return Err(WebhookError::InvalidPayload(
                    errors
                        .into_iter()
                        .map(|d| FieldError {
                            path: if d.path.is_empty() {
                                "payload".into()
                            } else {
                                format!("payload{}", d.path.replace('/', "."))
                            },
                            code: d.code,
                            message: d.message.unwrap_or_default(),
                        })
                        .collect(),
                ));
            }
            Some(reduced)
        }
        None => None,
    };
    let evaluation = |e: EngineError| WebhookError::Evaluation(e);
    let env = runbook_spec::trigger_env(trigger.payload.as_ref(), "")
        .map_err(|_| evaluation(EngineError::new("trigger.invalid_definition")))?;
    let bindings =
        crate::data::trigger_bindings(&env, payload.as_ref(), now_ms).map_err(evaluation)?;
    let mut session =
        runbook_expr::EvalSession::new(BUDGET).map_err(|e| evaluation(EngineError::new(e.code)))?;
    // Reading a field the call left out is "missing" rather than an error:
    // `when` is then false, `dedupe` unset, and an input keeps its default.
    let mut render = |source: &str| -> Result<Option<Value>, WebhookError> {
        let template = runbook_expr::check_template(source, &env)
            .map_err(|e| evaluation(EngineError::new(e.code)))?;
        match runbook_expr::render_template(&template, &bindings, &mut session) {
            Ok(v) => Ok(Some(v.value).filter(|v| !v.is_null())),
            Err(e) => {
                let error = crate::data::trigger_eval_error(e);
                if error.code() == "expression.missing_value" {
                    Ok(None)
                } else {
                    Err(evaluation(error))
                }
            }
        }
    };
    if let Some(when) = &trigger.when
        && render(when)? != Some(Value::Bool(true))
    {
        return Ok(WebhookOutcome::Skipped);
    }
    let dedupe = match &trigger.dedupe {
        Some(source) => render(source)?.map(|value| match value {
            Value::String(text) => text,
            other => other.to_string(),
        }),
        None => None,
    };
    let mut inputs = Data::new();
    for (name, value) in &trigger.inputs {
        let value = match value.as_str() {
            Some(source) if source.contains("${{") => render(source)?,
            _ => Some(value.clone()),
        };
        if let Some(value) = value {
            inputs.insert(name.clone(), value);
        }
    }
    Ok(WebhookOutcome::Start { inputs, dedupe })
}
