//! Incremental output accounting and depth-bounded, borrowed JSON traversal.
use crate::bindings::{BoundNode, BoundValue};
use crate::{EvalError, EvalSession, ReadKind};
use cel_core::Value;
use serde_json::{Map, Value as Json};
use std::{collections::btree_map, fmt::Write as _, sync::Arc};

const MAX_BYTES: usize = 65_536;
const MAX_NODES: usize = 16_384;
const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, Default)]
pub(crate) struct OutputBudget {
    bytes: usize,
    nodes: usize,
}

fn check_capacity(
    session: &mut EvalSession,
    nodes: usize,
    bytes: usize,
    depth: usize,
) -> Result<(), EvalError> {
    if let Some(error) = session.terminal_error() {
        return Err(error);
    }
    if depth > MAX_DEPTH
        || nodes > MAX_NODES.saturating_sub(session.output.nodes)
        || bytes > MAX_BYTES.saturating_sub(session.output.bytes)
    {
        session.terminal = Some(EvalError::OutputLimit);
        return Err(EvalError::OutputLimit);
    }
    Ok(())
}

/// Charges before allocation/copy. Failed charges leave counters and fuel unchanged.
/// Text rendering uses one node and two quotes once, then content-only charges.
pub(crate) fn charge_output(
    session: &mut EvalSession,
    nodes: usize,
    bytes: usize,
    depth: usize,
) -> Result<(), EvalError> {
    check_capacity(session, nodes, bytes, depth)?;
    if !session.budget.charge((nodes + bytes) as u64) {
        return Err(session.terminal_error().expect("failed output charge"));
    }
    session.output.nodes += nodes;
    session.output.bytes += bytes;
    Ok(())
}

/// Account raw UTF-8 before scanning; only JSON escape expansion is charged after.
/// The caller pays surrounding quotes separately and copies only after success.
pub(crate) fn charge_json_string_content(
    session: &mut EvalSession,
    text: &str,
) -> Result<(), EvalError> {
    charge_output(session, 0, text.len(), 0)?;
    let extra = text
        .bytes()
        .map(|byte| match byte {
            b'"' | b'\\' | b'\n' | b'\r' | b'\t' | 8 | 12 => 1,
            0..=31 => 5,
            _ => 0,
        })
        .sum();
    charge_output(session, 0, extra, 0)
}

/// All accepted numeric/timestamp representations fit in this stack buffer.
pub(crate) struct FixedText {
    bytes: [u8; 64],
    length: usize,
}
impl FixedText {
    fn new() -> Self {
        Self {
            bytes: [0; 64],
            length: 0,
        }
    }
    fn append(&mut self, value: &[u8]) -> Result<(), ()> {
        let end = self
            .length
            .checked_add(value.len())
            .filter(|end| *end <= self.bytes.len())
            .ok_or(())?;
        self.bytes[self.length..end].copy_from_slice(value);
        self.length = end;
        Ok(())
    }
    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.length]).expect("scalar formatter writes UTF-8")
    }
}
impl std::io::Write for FixedText {
    fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
        self.append(value)
            .map_err(|()| std::io::Error::from(std::io::ErrorKind::WriteZero))?;
        Ok(value.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl std::fmt::Write for FixedText {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.append(value.as_bytes()).map_err(|()| std::fmt::Error)
    }
}
pub(crate) enum ScalarText<'a> {
    Borrowed(&'a str),
    Fixed(FixedText),
}
impl ScalarText<'_> {
    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(value) => value,
            Self::Fixed(value) => value.as_str(),
        }
    }
}

/// Only scalar formatting: never generic CEL Display or collection serialization.
pub(crate) fn scalar_text(value: &Value) -> Result<ScalarText<'_>, EvalError> {
    let mut text = FixedText::new();
    match value {
        Value::String(value) => return Ok(ScalarText::Borrowed(value)),
        Value::Null => return Ok(ScalarText::Borrowed("null")),
        Value::Bool(true) => return Ok(ScalarText::Borrowed("true")),
        Value::Bool(false) => return Ok(ScalarText::Borrowed("false")),
        Value::Int(value) => {
            serde_json::to_writer(&mut text, value).map_err(|_| EvalError::TypeError)?
        }
        Value::Double(value) if value.is_finite() => {
            serde_json::to_writer(&mut text, value).map_err(|_| EvalError::TypeError)?
        }
        Value::Timestamp(value)
            if value.is_valid() && (0..1_000_000_000).contains(&value.nanos) =>
        {
            let date = value
                .to_datetime_utc()
                .ok_or(EvalError::TypeError)?
                .date_naive();
            let seconds = value.seconds.rem_euclid(86_400);
            write!(
                text,
                "{date}T{:02}:{:02}:{:02}",
                seconds / 3600,
                seconds / 60 % 60,
                seconds % 60
            )
            .map_err(|_| EvalError::TypeError)?;
            if value.nanos != 0 {
                let mut fraction = value.nanos;
                let mut width = 9;
                while fraction % 10 == 0 {
                    fraction /= 10;
                    width -= 1;
                }
                write!(text, ".{fraction:0width$}").map_err(|_| EvalError::TypeError)?;
            }
            text.write_char('Z').map_err(|_| EvalError::TypeError)?;
        }
        _ => return Err(EvalError::TypeError),
    }
    Ok(ScalarText::Fixed(text))
}

// Each frame owns only completed output and an iterator into immutable bindings.
// No pending vector proportional to input width and no recursive conversion.
enum Frame<'a> {
    Array {
        children: std::slice::Iter<'a, Arc<BoundNode>>,
        values: Vec<Json>,
    },
    Object {
        children: btree_map::Iter<'a, String, Arc<BoundNode>>,
        values: Map<String, Json>,
        key: Option<String>,
    },
}
impl<'a> Frame<'a> {
    fn next(&mut self, session: &mut EvalSession) -> Result<Option<&'a BoundNode>, EvalError> {
        match self {
            Self::Array { children, .. } => Ok(children.next().map(AsRef::as_ref)),
            Self::Object { children, key, .. } => {
                let Some((name, node)) = children.next() else {
                    return Ok(None);
                };
                charge_output(session, 0, 2, 0)?;
                charge_json_string_content(session, name)?;
                *key = Some(name.clone());
                Ok(Some(node))
            }
        }
    }
    fn insert(&mut self, value: Json) {
        match self {
            Self::Array { values, .. } => values.push(value),
            Self::Object { values, key, .. } => {
                values.insert(key.take().expect("prepared object key"), value);
            }
        }
    }
    fn finish(self) -> Json {
        match self {
            Self::Array { values, .. } => Json::Array(values),
            Self::Object { values, .. } => Json::Object(values),
        }
    }
}

pub(crate) fn materialize(
    value: &Value,
    source: Option<&BoundNode>,
    session: &mut EvalSession,
) -> Result<Json, EvalError> {
    if let Some(error) = session.terminal_error() {
        return Err(error);
    }
    let mut stack = Vec::<Frame<'_>>::new();
    let mut current = (value, source);
    loop {
        let (value, source) = current;
        let depth = stack.len();
        let mut completed = match value {
            Value::List(_) | Value::Message(_) => {
                let source = source.ok_or(EvalError::TypeError)?;
                let (count, record) = match &source.value {
                    BoundValue::List(items) => (items.len(), false),
                    BoundValue::Record { fields, .. } => (fields.len(), true),
                    _ => return Err(EvalError::TypeError),
                };
                // One child needs at least one node/byte; reject wide input in O(1).
                // This is a lower-bound check, not a reservation or double charge.
                let punctuation = 2 + count.saturating_sub(1) + if record { count } else { 0 };
                check_capacity(session, 1 + count, punctuation + count, depth)?;
                charge_output(session, 1, punctuation, depth)?;
                session
                    .reads
                    .record(&source.origin, ReadKind::Membership, &session.budget)?;
                let mut frame = match &source.value {
                    BoundValue::List(items) => Frame::Array {
                        children: items.iter(),
                        values: Vec::new(),
                    },
                    BoundValue::Record { fields, .. } => Frame::Object {
                        children: fields.iter(),
                        values: Map::new(),
                        key: None,
                    },
                    _ => unreachable!(),
                };
                if let Some(node) = frame.next(session)? {
                    stack.push(frame);
                    current = (&node.cel, Some(node));
                    continue;
                }
                frame.finish()
            }
            _ => {
                let text = scalar_text(value)?;
                let string = matches!(value, Value::String(_) | Value::Timestamp(_));
                if string {
                    charge_output(session, 1, 2, depth)?;
                    charge_json_string_content(session, text.as_str())?;
                } else {
                    charge_output(session, 1, text.as_str().len(), depth)?;
                }
                if let Some(source) = source {
                    session
                        .reads
                        .record(&source.origin, ReadKind::Value, &session.budget)?;
                }
                match value {
                    Value::Null => Json::Null,
                    Value::Bool(value) => Json::Bool(*value),
                    Value::Int(value) => Json::Number((*value).into()),
                    Value::Double(value) => Json::Number(
                        serde_json::Number::from_f64(*value).ok_or(EvalError::TypeError)?,
                    ),
                    Value::String(_) | Value::Timestamp(_) => {
                        Json::String(text.as_str().to_owned())
                    }
                    _ => return Err(EvalError::TypeError),
                }
            }
        };
        loop {
            let Some(frame) = stack.last_mut() else {
                return Ok(completed);
            };
            frame.insert(completed);
            if let Some(node) = frame.next(session)? {
                current = (&node.cel, Some(node));
                break;
            }
            completed = stack.pop().expect("active output frame").finish();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Origin, RecordIdentity, TypeId};

    fn nested_list(depth: usize) -> Arc<BoundNode> {
        let origin = Arc::new(Origin {
            path: "x".into(),
            record: RecordIdentity {
                execution_id: "e".into(),
                step_run_id: None,
                iteration_path: vec![],
                revision: 1,
            },
        });
        let mut node = Arc::new(BoundNode {
            ty: TypeId(0),
            cel: Value::Null,
            value: BoundValue::Null,
            origin: origin.clone(),
        });
        for _ in 0..depth {
            let value = BoundValue::List(Arc::new(vec![node]));
            let cel = crate::runtime::prepare_value(&value);
            node = Arc::new(BoundNode {
                ty: TypeId(0),
                cel,
                value,
                origin: origin.clone(),
            });
        }
        node
    }

    // Public schemas stop at depth 16. Exercise the output guard through private
    // sealed fixtures rather than weakening the ingress schema to reach depth 33.
    #[test]
    fn output_depth_32_succeeds_and_33_stops_before_leaf_allocation() {
        let node = nested_list(32);
        let mut session = EvalSession::new(100_000).unwrap();
        let mut result = materialize(&node.cel, Some(&node), &mut session).unwrap();
        for _ in 0..32 {
            let Json::Array(mut children) = result else {
                panic!("missing nested array")
            };
            result = children.pop().unwrap();
        }
        assert_eq!(result, Json::Null);
        assert_eq!(session.output.nodes, 33);
        assert_eq!(session.output.bytes, 68);
        let node = nested_list(33);
        let mut session = EvalSession::new(100_000).unwrap();
        assert_eq!(
            materialize(&node.cel, Some(&node), &mut session),
            Err(EvalError::OutputLimit)
        );
        assert_eq!(session.output.nodes, 33);
        assert_eq!(session.output.bytes, 66);
    }

    #[test]
    fn output_obeys_existing_terminal_before_inspecting_raw_value() {
        let mut session = EvalSession::new(100).unwrap();
        session.cancel();
        assert_eq!(
            materialize(&Value::Double(f64::INFINITY), None, &mut session),
            Err(EvalError::Cancelled)
        );
        assert_eq!(session.output.nodes, 0);
        assert_eq!(session.output.bytes, 0);
    }

    #[test]
    fn output_nonfinite_and_invalid_time_do_not_allocate() {
        for value in [
            Value::Double(f64::INFINITY),
            Value::Double(f64::NEG_INFINITY),
            Value::Double(f64::NAN),
            Value::Timestamp(cel_core::Timestamp::new(0, -1)),
            Value::Timestamp(cel_core::Timestamp::new(253_402_300_800, 0)),
        ] {
            let mut session = EvalSession::new(100).unwrap();
            assert_eq!(
                materialize(&value, None, &mut session),
                Err(EvalError::TypeError)
            );
            assert_eq!(session.output.nodes, 0);
            assert_eq!(session.output.bytes, 0);
        }
    }

    #[test]
    fn output_failed_precharge_leaves_counters_and_used_fuel_unchanged() {
        let mut session = EvalSession::new(3).unwrap();
        assert_eq!(
            materialize(&Value::String(Arc::from("abc")), None, &mut session),
            Err(EvalError::CostExceeded)
        );
        assert_eq!(session.stats().cost, 3);
        assert_eq!(session.output.nodes, 1);
        assert_eq!(session.output.bytes, 2);
        let mut session = EvalSession::new(100_000).unwrap();
        let values = Arc::new((0..100_000).map(|_| nested_list(0)).collect());
        let value = BoundValue::List(values);
        let origin = nested_list(0).origin.clone();
        let node = BoundNode {
            ty: TypeId(0),
            cel: crate::runtime::prepare_value(&value),
            value,
            origin,
        };
        assert_eq!(
            materialize(&node.cel, Some(&node), &mut session),
            Err(EvalError::OutputLimit)
        );
        assert_eq!(session.output.nodes, 0, "no output nodes visited/allocated");
        assert_eq!(session.output.bytes, 0);
        assert_eq!(session.stats().cost, 0);
        assert!(session.stats().reads.is_empty());
    }
}
