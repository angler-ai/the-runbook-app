//! Borrowed ingress validation and immutable storage. No caller code executes here.
use std::{collections::BTreeMap, sync::Arc};

use crate::{ExprDiagnostic, FieldDecl, TypeDef, TypeEnv, TypeId};
use serde::{Deserialize, Serialize};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 262_144;
const MAX_LIST: usize = 100_000;
const MAX_STRING: usize = 65_536;
const MAX_BYTES: usize = 8_388_608;
const MAX_PATH: usize = 1_024;
const MAX_ID: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventTime {
    pub unix_seconds: i64,
    pub nanos: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RecordIdentity {
    pub execution_id: String,
    pub step_run_id: Option<String>,
    pub iteration_path: Vec<IterationIdentity>,
    pub revision: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IterationIdentity {
    pub node_id: String,
    pub instance_id: String,
    pub ordinal: u32,
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Origin {
    pub path: String,
    pub record: RecordIdentity,
}
#[derive(Debug, Clone, PartialEq)]
pub enum InputValue {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(String),
    Timestamp(EventTime),
    List(Vec<InputNode>),
    Record(BTreeMap<String, InputNode>),
}
#[derive(Debug, Clone, PartialEq)]
pub struct InputNode {
    pub value: InputValue,
    pub origin: Origin,
}

/// An accepted snapshot, independent of subsequent caller mutations. Cloning a
/// handle shares all collections and strings, including the list origin index.
#[allow(dead_code)] // Reserved sealed runtime data; consumed by Tasks 7/9.
#[derive(Debug, Clone)]
pub struct Bindings {
    pub(crate) schema: Arc<TypeEnv>,
    pub(crate) roots: Arc<BTreeMap<String, Arc<BoundNode>>>,
    pub(crate) root_origin: Arc<Origin>,
    pub(crate) event_time: EventTime,
    pub(crate) list_origins: Arc<BTreeMap<usize, Arc<BoundNode>>>,
    pub(crate) cel_list_origins: Arc<BTreeMap<usize, Arc<BoundNode>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct BoundNode {
    pub(crate) ty: TypeId,
    pub(crate) cel: cel_core::Value,
    pub(crate) value: BoundValue,
    pub(crate) origin: Arc<Origin>,
}
#[allow(dead_code)] // Reserved sealed runtime variants; consumed by Tasks 7/9.
#[derive(Debug, Clone)]
pub(crate) enum BoundValue {
    Null,
    Bool(bool),
    Int(i64),
    Double(f64),
    String(Arc<str>),
    Timestamp(EventTime),
    List(Arc<Vec<Arc<BoundNode>>>),
    Record {
        ty: TypeId,
        fields: Arc<BTreeMap<String, Arc<BoundNode>>>,
    },
}

/// Missing stays distinct from explicit null. Path construction is deferred to
/// the metered runtime; the enclosing snapshot supplies the absence revision.
#[allow(dead_code)] // Reserved sealed lookup result; consumed by Tasks 7/9.
#[derive(Debug)]
pub(crate) enum Slot<'a> {
    Present(&'a Arc<BoundNode>),
    Missing {
        declaration: &'a FieldDecl,
        enclosing: &'a Origin,
        name: &'a str,
    },
}

// These are consumed by the sealed evaluation adapter in Tasks 7/9.
#[allow(dead_code)]
impl Bindings {
    pub fn new(
        env: &TypeEnv,
        roots: &BTreeMap<String, InputNode>,
        root_origin: &Origin,
        event_time: EventTime,
    ) -> Result<Self, ExprDiagnostic> {
        let mut counts = Counts::default();
        validate_shape(roots, &mut counts)?;
        validate(env, roots, root_origin, event_time, &mut counts)?;
        Ok(convert(env, roots, root_origin, event_time))
    }

    pub(crate) fn root(&self, name: &str) -> Option<Slot<'_>> {
        let (name, declaration) = self.schema.binding_roots().get_key_value(name)?;
        Some(match self.roots.get(name) {
            Some(node) => Slot::Present(node),
            None => Slot::Missing {
                declaration,
                enclosing: &self.root_origin,
                name,
            },
        })
    }

    pub(crate) fn field<'a>(&'a self, node: &'a BoundNode, name: &str) -> Option<Slot<'a>> {
        let BoundValue::Record { ty, fields } = &node.value else {
            return None;
        };
        let TypeDef::Record(declarations) = self.schema.type_definition(*ty)? else {
            return None;
        };
        let (name, declaration) = declarations.get_key_value(name)?;
        Some(match fields.get(name) {
            Some(node) => Slot::Present(node),
            None => Slot::Missing {
                declaration,
                enclosing: &node.origin,
                name,
            },
        })
    }

    pub(crate) fn cel_list_node(&self, value: &cel_core::Value) -> Option<&Arc<BoundNode>> {
        let cel_core::Value::List(list) = value else {
            return None;
        };
        self.cel_list_origins.get(&(list.as_ptr() as usize))
    }

    pub(crate) fn list_origin(&self, list: &Arc<Vec<Arc<BoundNode>>>) -> Option<&Arc<BoundNode>> {
        self.list_origins.get(&(Arc::as_ptr(list) as usize))
    }
}

// Snapshot identity must preserve observable output, including signed zero.
// This is deliberately scalar-only: never traverse an unvalidated subtree.
fn same_snapshot_scalar(left: &InputValue, right: &InputValue) -> bool {
    match (left, right) {
        (InputValue::Null, InputValue::Null) => true,
        (InputValue::Bool(a), InputValue::Bool(b)) => a == b,
        (InputValue::Int(a), InputValue::Int(b)) => a == b,
        (InputValue::Double(a), InputValue::Double(b)) => a.to_bits() == b.to_bits(),
        (InputValue::String(a), InputValue::String(b)) => a == b,
        (InputValue::Timestamp(a), InputValue::Timestamp(b)) => a == b,
        _ => false,
    }
}

fn error(message: &'static str) -> ExprDiagnostic {
    ExprDiagnostic {
        code: "expression.binding_error",
        span: 0..0,
        message: message.into(),
    }
}
fn limit(message: &'static str) -> ExprDiagnostic {
    ExprDiagnostic {
        code: "expression.binding_limit",
        span: 0..0,
        message: message.into(),
    }
}

pub(crate) fn validate_time(time: EventTime) -> Result<(), ExprDiagnostic> {
    if !(-62_135_596_800..=253_402_300_799).contains(&time.unix_seconds)
        || time.nanos >= 1_000_000_000
    {
        return Err(error(
            "Timestamp is outside UTC years 0001 through 9999 or has invalid nanoseconds",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Counts {
    nodes: usize,
    bytes: usize,
}
impl Counts {
    fn bytes(&mut self, bytes: usize) -> Result<(), ExprDiagnostic> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| limit("Bindings exceed 8,388,608 string and metadata bytes"))?;
        Ok(())
    }
    fn node(&mut self, depth: usize) -> Result<(), ExprDiagnostic> {
        if depth > MAX_DEPTH {
            return Err(limit("Bindings exceed depth 32"));
        }
        if self.nodes == MAX_NODES {
            return Err(limit("Bindings exceed 262,144 value nodes"));
        }
        self.nodes += 1;
        Ok(())
    }
}

// The shape envelope precedes schema matching, so malformed typed trees are
// still rejected for depth/width without recursing into or owning their tail.
fn validate_shape(
    roots: &BTreeMap<String, InputNode>,
    counts: &mut Counts,
) -> Result<(), ExprDiagnostic> {
    enum Children<'a> {
        Record(std::collections::btree_map::Values<'a, String, InputNode>),
        List(std::slice::Iter<'a, InputNode>),
    }
    if roots.len() > 256 {
        return Err(limit("Bindings exceed 256 roots"));
    }
    let mut stack = vec![(Children::Record(roots.values()), 0)];
    while let Some((children, depth)) = stack.last_mut() {
        let child = match children {
            Children::Record(iter) => iter.next(),
            Children::List(iter) => iter.next(),
        };
        let Some(node) = child else {
            stack.pop();
            continue;
        };
        let depth = *depth;
        #[cfg(test)]
        VISITS.with(|n| n.set(n.get() + 1));
        counts.node(depth)?;
        match &node.value {
            InputValue::List(items) => {
                if items.len() > MAX_LIST {
                    return Err(limit("Bound list exceeds 100,000 elements"));
                }
                stack.push((Children::List(items.iter()), depth + 1));
            }
            InputValue::Record(fields) => {
                if fields.len() > 256 {
                    return Err(limit("Bound record exceeds 256 fields"));
                }
                stack.push((Children::Record(fields.values()), depth + 1));
            }
            InputValue::String(value) if value.len() > MAX_STRING => {
                return Err(limit("Bound string exceeds 65,536 bytes"));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
thread_local! {
    static VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static COPIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn identity_string(value: &str, counts: &mut Counts) -> Result<(), ExprDiagnostic> {
    if value.is_empty() || value.len() > MAX_ID {
        return Err(limit("Runtime identities must contain 1 through 128 bytes"));
    }
    if value.chars().any(char::is_control) {
        return Err(error(
            "Runtime identities cannot contain control characters",
        ));
    }
    counts.bytes(value.len())
}
fn metadata(origin: &Origin, counts: &mut Counts) -> Result<(), ExprDiagnostic> {
    if origin.path.len() > MAX_PATH {
        return Err(limit("Canonical binding path exceeds 1,024 bytes"));
    }
    counts.bytes(origin.path.len())?;
    let id = &origin.record;
    if id.revision == 0 {
        return Err(error("Record revisions must be positive"));
    }
    identity_string(&id.execution_id, counts)?;
    if let Some(step) = &id.step_run_id {
        identity_string(step, counts)?;
    }
    // This gate precedes scanning or copying the vector, even if every string is empty.
    if id.iteration_path.len() > MAX_DEPTH {
        return Err(limit("Iteration identity exceeds depth 32"));
    }
    for (index, iteration) in id.iteration_path.iter().enumerate() {
        identity_string(&iteration.node_id, counts)?;
        identity_string(&iteration.instance_id, counts)?;
        if id.iteration_path[..index].iter().any(|prior| {
            prior.node_id == iteration.node_id && prior.instance_id == iteration.instance_id
        }) {
            return Err(error("Iteration identity repeats an instance"));
        }
    }
    Ok(())
}

fn child_identity(child: &InputNode, parent: &Origin) -> Result<(), ExprDiagnostic> {
    let id = &child.origin.record;
    let parent = &parent.record;
    if id.execution_id != parent.execution_id {
        return Err(error("Child execution identity differs from its container"));
    }
    if id.step_run_id == parent.step_run_id {
        if id.iteration_path != parent.iteration_path {
            return Err(error("A record subtree changes iteration identity"));
        }
    } else if !matches!(child.value, InputValue::Record(_))
        || id.step_run_id.is_none()
        || !id.iteration_path.starts_with(&parent.iteration_path)
    {
        return Err(error(
            "Only a nested step record may introduce a step or iteration identity",
        ));
    }
    Ok(())
}

// Compare the canonical suffix without allocating paths from unvalidated data.
fn field_path(path: &str, parent: &str, name: &str) -> bool {
    if parent.is_empty() {
        return path == name;
    }
    path.len() == parent.len() + 1 + name.len()
        && path.starts_with(parent)
        && path.as_bytes()[parent.len()] == b'.'
        && path.ends_with(name)
}
// Scalar lexical aliases may point at a canonical item outside their local
// root name. Validate that external path directly, without interpreting a name
// as provenance or using scalar equality to discover its origin.
fn canonical_item_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut cursor = 0;
    loop {
        let start = cursor;
        if !bytes
            .get(cursor)
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return false;
        }
        cursor += 1;
        while bytes
            .get(cursor)
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        {
            cursor += 1;
        }
        if crate::schema::validate_name(&path[start..cursor]).is_err() {
            return false;
        }
        let mut ends_in_index = false;
        while bytes.get(cursor) == Some(&b'[') {
            cursor += 1;
            let start = cursor;
            while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
                cursor += 1;
            }
            if start == cursor
                || cursor - start > 6
                || bytes.get(cursor) != Some(&b']')
                || (cursor - start > 1 && bytes[start] == b'0')
            {
                return false;
            }
            let Ok(index) = path[start..cursor].parse::<usize>() else {
                return false;
            };
            if index >= MAX_LIST {
                return false;
            }
            cursor += 1;
            ends_in_index = true;
        }
        if cursor == bytes.len() {
            return ends_in_index;
        }
        if bytes.get(cursor) != Some(&b'.') {
            return false;
        }
        cursor += 1;
    }
}
fn index_path(path: &str, parent: &str, index: usize) -> bool {
    let mut digits = [0_u8; 20];
    let mut cursor = digits.len();
    let mut remaining = index;
    loop {
        cursor -= 1;
        digits[cursor] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    let suffix = &digits[cursor..];
    let bytes = path.as_bytes();
    bytes.len() == parent.len() + suffix.len() + 2
        && bytes.starts_with(parent.as_bytes())
        && bytes.get(parent.len()) == Some(&b'[')
        && bytes.ends_with(b"]")
        && &bytes[parent.len() + 1..bytes.len() - 1] == suffix
}

/// Each frame stores an iterator, never a copied vector of pending children.
/// Consequently a rejected wide list never visits its unvalidated remainder.
enum Frame<'a> {
    Record {
        children: std::collections::btree_map::Iter<'a, String, InputNode>,
        declarations: &'a BTreeMap<String, FieldDecl>,
        parent: &'a Origin,
        depth: usize,
    },
    List {
        children: std::iter::Enumerate<std::slice::Iter<'a, InputNode>>,
        ty: TypeId,
        parent: &'a Origin,
        depth: usize,
    },
}

fn validate(
    env: &TypeEnv,
    roots: &BTreeMap<String, InputNode>,
    root_origin: &Origin,
    time: EventTime,
    counts: &mut Counts,
) -> Result<(), ExprDiagnostic> {
    validate_time(time)?;
    metadata(root_origin, counts)?;
    if !root_origin.path.is_empty() {
        return Err(error("Root-container origin must have an empty path"));
    }
    validate_fields(roots, env.binding_roots())?;
    for name in roots.keys() {
        counts.bytes(name.len())?;
    }
    let mut stack = vec![Frame::Record {
        children: roots.iter(),
        declarations: env.binding_roots(),
        parent: root_origin,
        depth: 0,
    }];
    // A step snapshot may occur at multiple canonical paths, but its iteration
    // identity must agree everywhere. Leaf revisions intentionally may differ.
    let mut steps: BTreeMap<&str, &[IterationIdentity]> = BTreeMap::new();
    if let Some(step) = root_origin.record.step_run_id.as_deref() {
        steps.insert(step, &root_origin.record.iteration_path);
    }
    let mut origins = BTreeMap::new();
    while let Some(frame) = stack.last_mut() {
        let (node, ty, nullable, depth, parent, canonical) = match frame {
            Frame::Record {
                children,
                declarations,
                parent,
                depth,
            } => {
                let Some((name, node)) = children.next() else {
                    stack.pop();
                    continue;
                };
                let decl = &declarations[name];
                (
                    node,
                    decl.ty,
                    decl.nullable,
                    *depth,
                    *parent,
                    field_path(&node.origin.path, &parent.path, name),
                )
            }
            Frame::List {
                children,
                ty,
                parent,
                depth,
            } => {
                let Some((index, node)) = children.next() else {
                    stack.pop();
                    continue;
                };
                (
                    node,
                    *ty,
                    false,
                    *depth,
                    *parent,
                    index_path(&node.origin.path, &parent.path, index),
                )
            }
        };
        metadata(&node.origin, counts)?;
        let scalar_alias = depth == 0
            && !matches!(node.value, InputValue::List(_) | InputValue::Record(_))
            && canonical_item_path(&node.origin.path);
        if !canonical && !scalar_alias {
            return Err(error(
                "Child origin is not its canonical field or index path",
            ));
        }
        // The root-container identity describes missing roots. Present roots
        // may belong to a different step snapshot in the same execution.
        if depth == 0 {
            if node.origin.record.execution_id != root_origin.record.execution_id {
                return Err(error("Root execution identity differs from its container"));
            }
        } else {
            child_identity(node, parent)?;
        }
        let identity = &node.origin.record;
        let key = (
            node.origin.path.as_str(),
            identity.execution_id.as_str(),
            identity.step_run_id.as_deref(),
            identity.iteration_path.as_slice(),
        );
        if let Some((revision, prior_value)) = origins.insert(key, (identity.revision, &node.value))
        {
            // Repeated origins can arise only from scalar aliases. Comparing
            // collections here would traverse caller subtrees before validation.
            if revision != identity.revision
                || matches!(node.value, InputValue::List(_) | InputValue::Record(_))
                || !same_snapshot_scalar(prior_value, &node.value)
            {
                return Err(error(
                    "Canonical aliases disagree about their snapshot or value",
                ));
            }
        }
        if let Some(step) = &node.origin.record.step_run_id {
            let iterations = node.origin.record.iteration_path.as_slice();
            if steps
                .get(step.as_str())
                .is_some_and(|prior| *prior != iterations)
            {
                return Err(error("A step-run identity occurs in different iterations"));
            }
            steps.entry(step).or_insert(iterations);
        }
        let ty_def = env.type_definition(ty).expect("validated schema type");
        match (&node.value, ty_def) {
            (InputValue::Null, _) if nullable => {}
            (InputValue::Bool(_), TypeDef::Bool) | (InputValue::Int(_), TypeDef::Int) => {}
            (InputValue::Double(value), TypeDef::Double) if value.is_finite() => {}
            (InputValue::String(value), TypeDef::String) => {
                if value.len() > MAX_STRING {
                    return Err(limit("Bound string exceeds 65,536 bytes"));
                }
                counts.bytes(value.len())?;
            }
            (InputValue::Timestamp(value), TypeDef::Timestamp) => validate_time(*value)?,
            (InputValue::List(children), TypeDef::List(child_ty)) => {
                if children.len() > MAX_LIST {
                    return Err(limit("Bound list exceeds 100,000 elements"));
                }
                stack.push(Frame::List {
                    children: children.iter().enumerate(),
                    ty: *child_ty,
                    parent: &node.origin,
                    depth: depth + 1,
                });
            }
            (InputValue::Record(children), TypeDef::Record(declarations)) => {
                validate_fields(children, declarations)?;
                for name in children.keys() {
                    counts.bytes(name.len())?;
                }
                stack.push(Frame::Record {
                    children: children.iter(),
                    declarations,
                    parent: &node.origin,
                    depth: depth + 1,
                });
            }
            _ => {
                return Err(error(
                    "Bound value does not match its declared type or nullability",
                ));
            }
        }
    }
    Ok(())
}

fn validate_fields(
    children: &BTreeMap<String, InputNode>,
    declarations: &BTreeMap<String, FieldDecl>,
) -> Result<(), ExprDiagnostic> {
    // Cardinality gates avoid scanning a caller's unbounded unknown-key map.
    if children.len() > declarations.len() {
        return Err(error("Binding contains undeclared fields"));
    }
    for name in children.keys() {
        if name.len() > MAX_ID || !declarations.contains_key(name) {
            return Err(error("Binding contains an undeclared field"));
        }
    }
    for (name, decl) in declarations {
        if decl.required && !children.contains_key(name) {
            return Err(error("Binding is missing a required field"));
        }
    }
    Ok(())
}

/// Iterative postorder conversion runs only after the entire borrowed input has
/// passed. The work stack and every partially built tree have accepted bounds.
fn convert(
    env: &TypeEnv,
    roots: &BTreeMap<String, InputNode>,
    root_origin: &Origin,
    time: EventTime,
) -> Bindings {
    enum Work<'a> {
        Enter(&'a InputNode, TypeId),
        Finish(&'a InputNode, TypeId, usize),
    }
    let mut work = Vec::new();
    let mut values: Vec<Arc<BoundNode>> = Vec::new();
    let mut list_origins = BTreeMap::new();
    let mut cel_list_origins = BTreeMap::new();
    for (name, node) in roots.iter().rev() {
        work.push(Work::Enter(node, env.binding_roots()[name].ty));
    }
    while let Some(task) = work.pop() {
        let (node, ty, value) = match task {
            Work::Enter(node, ty) => {
                let value = match &node.value {
                    InputValue::Null => BoundValue::Null,
                    InputValue::Bool(v) => BoundValue::Bool(*v),
                    InputValue::Int(v) => BoundValue::Int(*v),
                    InputValue::Double(v) => BoundValue::Double(*v),
                    InputValue::String(v) => BoundValue::String(Arc::from(v.as_str())),
                    InputValue::Timestamp(v) => BoundValue::Timestamp(*v),
                    InputValue::List(children) => {
                        let TypeDef::List(child_ty) =
                            env.type_definition(ty).expect("validated schema")
                        else {
                            unreachable!()
                        };
                        work.push(Work::Finish(node, ty, children.len()));
                        for child in children.iter().rev() {
                            work.push(Work::Enter(child, *child_ty));
                        }
                        continue;
                    }
                    InputValue::Record(children) => {
                        let TypeDef::Record(declarations) =
                            env.type_definition(ty).expect("validated schema")
                        else {
                            unreachable!()
                        };
                        work.push(Work::Finish(node, ty, children.len()));
                        for (name, child) in children.iter().rev() {
                            work.push(Work::Enter(child, declarations[name].ty));
                        }
                        continue;
                    }
                };
                (node, ty, value)
            }
            Work::Finish(node, ty, count) => {
                let children = values.split_off(values.len() - count);
                let value = match &node.value {
                    InputValue::List(_) => BoundValue::List(Arc::new(children)),
                    InputValue::Record(fields) => BoundValue::Record {
                        ty,
                        fields: Arc::new(fields.keys().cloned().zip(children).collect()),
                    },
                    _ => unreachable!(),
                };
                (node, ty, value)
            }
        };
        #[cfg(test)]
        COPIES.with(|n| n.set(n.get() + 1));
        let origin = Arc::new(node.origin.clone());
        let cel = crate::runtime::prepare_value(&value);
        let bound = Arc::new(BoundNode {
            ty,
            value,
            origin,
            cel,
        });
        if let cel_core::Value::List(list) = &bound.cel {
            cel_list_origins.insert(list.as_ptr() as usize, bound.clone());
        }
        if let BoundValue::List(list) = &bound.value {
            list_origins.insert(Arc::as_ptr(list) as usize, bound.clone());
        }
        values.push(bound);
    }
    Bindings {
        schema: Arc::new(env.clone()),
        roots: Arc::new(roots.keys().cloned().zip(values).collect()),
        root_origin: Arc::new(root_origin.clone()),
        event_time: time,
        list_origins: Arc::new(list_origins),
        cel_list_origins: Arc::new(cel_list_origins),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SchemaDef;

    fn origin(path: &str) -> Origin {
        Origin {
            path: path.into(),
            record: RecordIdentity {
                execution_id: "e".into(),
                step_run_id: None,
                iteration_path: vec![],
                revision: 1,
            },
        }
    }
    fn node(path: &str, value: InputValue) -> InputNode {
        InputNode {
            value,
            origin: origin(path),
        }
    }
    fn decl(ty: u32, required: bool, nullable: bool) -> FieldDecl {
        FieldDecl {
            ty: TypeId(ty),
            required,
            nullable,
        }
    }
    fn time() -> EventTime {
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        }
    }
    fn reset() {
        VISITS.with(|n| n.set(0));
        COPIES.with(|n| n.set(0));
    }

    #[test]
    fn overflowing_wide_input_never_visits_or_copies_the_remainder() {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![TypeDef::Int, TypeDef::List(TypeId(0))],
            roots: BTreeMap::from([("x".into(), decl(1, true, false))]),
        })
        .unwrap();
        let roots = BTreeMap::from([(
            "x".into(),
            node(
                "x",
                InputValue::List(
                    (0..100_001)
                        .map(|i| node(&format!("x[{i}]"), InputValue::Int(i)))
                        .collect(),
                ),
            ),
        )]);
        let InputValue::List(items) = &roots["x"].value else {
            unreachable!()
        };
        let remainder_address = &items[100_000] as *const InputNode;
        reset();
        assert!(Bindings::new(&env, &roots, &origin(""), time()).is_err());
        VISITS.with(|n| assert_eq!(n.get(), 1));
        COPIES.with(|n| assert_eq!(n.get(), 0));
        // Still caller owned, including the last node and its original allocation.
        let InputValue::List(items) = &roots["x"].value else {
            unreachable!()
        };
        assert_eq!(items.len(), 100_001);
        assert_eq!(&items[100_000] as *const InputNode, remainder_address);
        assert_eq!(items[100_000].value, InputValue::Int(100_000));
    }

    #[test]
    fn metadata_rejection_copies_no_prefix_of_an_otherwise_valid_list() {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![TypeDef::Int, TypeDef::List(TypeId(0))],
            roots: BTreeMap::from([("x".into(), decl(1, true, false))]),
        })
        .unwrap();
        let mut children = vec![
            node("x[0]", InputValue::Int(0)),
            node("x[1]", InputValue::Int(1)),
        ];
        children[1].origin.record.execution_id = "e".repeat(129);
        let roots = BTreeMap::from([("x".into(), node("x", InputValue::List(children)))]);
        reset();
        assert!(Bindings::new(&env, &roots, &origin(""), time()).is_err());
        VISITS.with(|n| assert_eq!(n.get(), 3));
        COPIES.with(|n| assert_eq!(n.get(), 0));
    }

    #[test]
    fn projections_and_cloned_handles_preserve_storage_and_empty_list_identity() {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![
                TypeDef::Int,
                TypeDef::List(TypeId(0)),
                TypeDef::Record(BTreeMap::from([
                    ("a".into(), decl(1, true, false)),
                    ("b".into(), decl(1, true, false)),
                ])),
            ],
            roots: BTreeMap::from([("x".into(), decl(2, true, false))]),
        })
        .unwrap();
        let roots = BTreeMap::from([(
            "x".into(),
            node(
                "x",
                InputValue::Record(BTreeMap::from([
                    ("a".into(), node("x.a", InputValue::List(vec![]))),
                    ("b".into(), node("x.b", InputValue::List(vec![]))),
                ])),
            ),
        )]);
        let bindings = Bindings::new(&env, &roots, &origin(""), time()).unwrap();
        let cloned = bindings.clone();
        assert!(Arc::ptr_eq(&bindings.roots, &cloned.roots));
        assert!(Arc::ptr_eq(&bindings.list_origins, &cloned.list_origins));
        let Slot::Present(record) = bindings.root("x").unwrap() else {
            unreachable!()
        };
        let BoundValue::Record { fields, .. } = &record.value else {
            unreachable!()
        };
        let BoundValue::Record {
            fields: projected_fields,
            ..
        } = record.value.clone()
        else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(fields, &projected_fields));
        let Slot::Present(a) = bindings.field(record, "a").unwrap() else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(a, &fields["a"]));
        let BoundValue::List(list) = &a.value else {
            unreachable!()
        };
        let BoundValue::List(projected_list) = a.value.clone() else {
            unreachable!()
        };
        assert!(Arc::ptr_eq(list, &projected_list));
        assert!(Arc::ptr_eq(bindings.list_origin(list).unwrap(), a));
        let BoundValue::List(other) = &fields["b"].value else {
            unreachable!()
        };
        assert!(!Arc::ptr_eq(list, other));
        assert_eq!(bindings.list_origins.len(), 2);
        assert_eq!(bindings.list_origin(other).unwrap().origin.path, "x.b");
    }

    #[test]
    fn missing_slots_keep_container_revision_and_null_remains_present() {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![
                TypeDef::Int,
                TypeDef::Record(BTreeMap::from([("field".into(), decl(0, false, false))])),
            ],
            roots: BTreeMap::from([
                ("missing".into(), decl(0, false, false)),
                ("nullable_value".into(), decl(0, false, true)),
                ("record".into(), decl(1, false, false)),
            ]),
        })
        .unwrap();
        let mut root_origin = origin("");
        root_origin.record.revision = 7;
        let mut record = node("record", InputValue::Record(BTreeMap::new()));
        record.origin.record.revision = 5;
        let roots = BTreeMap::from([
            (
                "nullable_value".into(),
                node("nullable_value", InputValue::Null),
            ),
            ("record".into(), record),
        ]);
        let original = Bindings::new(&env, &roots, &root_origin, time()).unwrap();
        match original.root("missing").unwrap() {
            Slot::Missing {
                declaration,
                enclosing,
                name,
            } => {
                assert_eq!(declaration.ty, TypeId(0));
                assert_eq!(enclosing.record.revision, 7);
                assert_eq!(name, "missing");
            }
            _ => panic!("missing root fabricated a value"),
        }
        let Slot::Present(null) = original.root("nullable_value").unwrap() else {
            panic!("null turned into missing")
        };
        assert!(matches!(null.value, BoundValue::Null));
        let Slot::Present(record) = original.root("record").unwrap() else {
            unreachable!()
        };
        let Slot::Missing {
            enclosing, name, ..
        } = original.field(record, "field").unwrap()
        else {
            panic!("missing field fabricated a value")
        };
        assert_eq!(enclosing.record.revision, 5);
        assert_eq!(name, "field");
        root_origin.record.revision = 8;
        let amended = Bindings::new(&env, &roots, &root_origin, time()).unwrap();
        let Slot::Missing { enclosing, .. } = original.root("missing").unwrap() else {
            unreachable!()
        };
        assert_eq!(enclosing.record.revision, 7);
        let Slot::Missing { enclosing, .. } = amended.root("missing").unwrap() else {
            unreachable!()
        };
        assert_eq!(enclosing.record.revision, 8);
        let empty = Bindings::new(&env, &BTreeMap::new(), &root_origin, time()).unwrap();
        assert!(matches!(empty.root("missing"), Some(Slot::Missing { .. })));
    }
}

#[cfg(test)]
mod cel_storage_tests {
    use super::*;
    use crate::{SchemaDef, runtime::PreparedRecord};
    #[test]
    fn cel_projection_shares_prepared_storage_and_canonical_owner() {
        let origin = |path: &str| Origin {
            path: path.into(),
            record: RecordIdentity {
                execution_id: "e".into(),
                step_run_id: None,
                iteration_path: vec![],
                revision: 1,
            },
        };
        let decl = |id| FieldDecl {
            ty: TypeId(id),
            nullable: false,
            required: true,
        };
        let env = TypeEnv::new(&SchemaDef {
            types: vec![
                TypeDef::Int,
                TypeDef::List(TypeId(0)),
                TypeDef::Record(BTreeMap::from([("items".into(), decl(1))])),
            ],
            roots: BTreeMap::from([("record".into(), decl(2))]),
        })
        .unwrap();
        let roots = BTreeMap::from([(
            "record".into(),
            InputNode {
                origin: origin("record"),
                value: InputValue::Record(BTreeMap::from([(
                    "items".into(),
                    InputNode {
                        origin: origin("record.items"),
                        value: InputValue::List(vec![]),
                    },
                )])),
            },
        )]);
        let bindings = Bindings::new(
            &env,
            &roots,
            &origin(""),
            EventTime {
                unix_seconds: 0,
                nanos: 0,
            },
        )
        .unwrap();
        let cel_core::Value::Message(record) = &bindings.roots["record"].cel else {
            panic!()
        };
        let record = record.as_any().downcast_ref::<PreparedRecord>().unwrap();
        let cel_core::Value::List(list) = &record.fields["items"].cel else {
            panic!()
        };
        let copy = list.clone();
        assert!(Arc::ptr_eq(list, &copy));
        let key = list.as_ptr() as usize;
        assert_eq!(bindings.cel_list_origins[&key].origin.path, "record.items");
        assert!(Arc::ptr_eq(
            &bindings.cel_list_origins[&key],
            &record.fields["items"]
        ));
    }
}
