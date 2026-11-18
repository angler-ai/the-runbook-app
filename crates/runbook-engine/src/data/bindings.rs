//! Bounded, type-directed JSON ingress for expression snapshots.
use std::collections::BTreeMap;

use runbook_expr::{
    Bindings, EventTime, FieldDecl, InputNode, InputValue, Origin, RecordIdentity, TypeDef, TypeEnv,
};
use serde_json::Value;

use crate::EngineError;

fn invalid() -> EngineError {
    EngineError::new("expression.binding_error")
}
fn limit() -> EngineError {
    EngineError::new("expression.binding_limit")
}

#[derive(Default)]
struct Counts {
    nodes: usize,
    bytes: usize,
}
impl Counts {
    fn bytes(&mut self, bytes: usize) -> Result<(), EngineError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|n| *n <= 8_388_608)
            .ok_or_else(limit)?;
        Ok(())
    }

    fn metadata(&mut self, path: &str, id: &RecordIdentity) -> Result<(), EngineError> {
        if path.len() > 1_024 || id.iteration_path.len() > 32 {
            return Err(limit());
        }
        self.bytes(path.len())?;
        if id.revision == 0 {
            return Err(invalid());
        }
        self.identity(&id.execution_id)?;
        if let Some(step) = &id.step_run_id {
            self.identity(step)?;
        }
        for item in &id.iteration_path {
            self.identity(&item.node_id)?;
            self.identity(&item.instance_id)?;
        }
        Ok(())
    }

    fn identity(&mut self, id: &str) -> Result<(), EngineError> {
        if id.is_empty() || id.len() > 128 {
            return Err(limit());
        }
        if id.chars().any(char::is_control) {
            return Err(invalid());
        }
        self.bytes(id.len())
    }
}

fn fields<'a>(
    len: usize,
    keys: impl Iterator<Item = &'a String>,
    declarations: &BTreeMap<String, FieldDecl>,
    contains: impl Fn(&str) -> bool,
) -> Result<(), EngineError> {
    if len > declarations.len() {
        return Err(invalid());
    }
    for key in keys {
        if key.len() > 128 || !declarations.contains_key(key) {
            return Err(invalid());
        }
    }
    if declarations
        .iter()
        .any(|(name, decl)| decl.required && !contains(name))
    {
        return Err(invalid());
    }
    Ok(())
}

struct Ingress<'a> {
    env: &'a TypeEnv,
    revisions: &'a BTreeMap<String, RecordIdentity>,
    overrides: &'a BTreeMap<String, RecordIdentity>,
    aliases: &'a BTreeMap<String, Origin>,
    counts: Counts,
    copy: bool,
}
impl Ingress<'_> {
    // The first traversal validates all JSON and bounds without copying any
    // caller strings, identities, or collections. The second builds only that
    // bounded tree; Bindings::new remains the authority for provenance rules.
    fn node(
        &mut self,
        value: &Value,
        decl: &FieldDecl,
        path: &str,
        inherited: &RecordIdentity,
        depth: usize,
    ) -> Result<Option<InputNode>, EngineError> {
        if depth > 32 || self.counts.nodes == 262_144 {
            return Err(limit());
        }
        self.counts.nodes += 1;
        let alias = (depth == 0).then(|| self.aliases.get(path)).flatten();
        let path = alias.map_or(path, |origin| origin.path.as_str());
        let identity = alias.map(|origin| &origin.record).unwrap_or_else(|| {
            self.overrides
                .get(path)
                .or_else(|| self.revisions.get(path))
                .unwrap_or(inherited)
        });
        self.counts.metadata(path, identity)?;
        let env = self.env;
        let ty = env.type_definition(decl.ty).ok_or_else(invalid)?;
        let converted = if value.is_null() && decl.nullable {
            Some(InputValue::Null)
        } else {
            match (value, ty) {
                (Value::Bool(v), TypeDef::Bool) => Some(InputValue::Bool(*v)),
                (Value::Number(_), TypeDef::Int) => Some(InputValue::Int(
                    runbook_spec::schema::integer_value(value).ok_or_else(invalid)?,
                )),
                (Value::Number(v), TypeDef::Double) => {
                    let v = v.as_f64().filter(|v| v.is_finite()).ok_or_else(invalid)?;
                    Some(InputValue::Double(v))
                }
                (Value::String(v), TypeDef::String) => {
                    if v.len() > 65_536 {
                        return Err(limit());
                    }
                    self.counts.bytes(v.len())?;
                    self.copy.then(|| InputValue::String(v.clone()))
                }
                (Value::String(v), TypeDef::Timestamp) => {
                    if v.len() > 65_536 {
                        return Err(limit());
                    }
                    self.counts.bytes(v.len())?;
                    let v = v
                        .parse::<runbook_model::Timestamp>()
                        .map_err(|_| invalid())?;
                    Some(InputValue::Timestamp(EventTime {
                        unix_seconds: v.timestamp(),
                        nanos: v.timestamp_subsec_nanos(),
                    }))
                }
                (Value::Array(items), TypeDef::List(child)) => {
                    if items.len() > 100_000 {
                        return Err(limit());
                    }
                    let child = FieldDecl {
                        ty: *child,
                        required: true,
                        nullable: false,
                    };
                    let mut result = Vec::new();
                    for (index, item) in items.iter().enumerate() {
                        let suffix = format!("[{index}]");
                        if path.len() + suffix.len() > 1_024 {
                            return Err(limit());
                        }
                        if let Some(node) = self.node(
                            item,
                            &child,
                            &format!("{path}{suffix}"),
                            identity,
                            depth + 1,
                        )? {
                            result.push(node);
                        }
                    }
                    self.copy.then_some(InputValue::List(result))
                }
                (Value::Object(items), TypeDef::Record(declarations)) => {
                    fields(items.len(), items.keys(), declarations, |name| {
                        items.contains_key(name)
                    })?;
                    let mut result = BTreeMap::new();
                    for (name, item) in items {
                        self.counts.bytes(name.len())?;
                        if path.len() + 1 + name.len() > 1_024 {
                            return Err(limit());
                        }
                        if let Some(node) = self.node(
                            item,
                            &declarations[name],
                            &format!("{path}.{name}"),
                            identity,
                            depth + 1,
                        )? {
                            result.insert(name.clone(), node);
                        }
                    }
                    self.copy.then_some(InputValue::Record(result))
                }
                _ => return Err(invalid()),
            }
        };
        Ok(if self.copy {
            Some(InputNode {
                value: converted
                    .ok_or(EngineError::invariant("binding copy was not materialized"))?,
                origin: Origin {
                    path: path.to_owned(),
                    record: identity.clone(),
                },
            })
        } else {
            None
        })
    }
}

#[cfg(test)]
pub(super) fn build(
    env: &TypeEnv,
    roots: &BTreeMap<String, Value>,
    revisions: &BTreeMap<String, RecordIdentity>,
    root_origin: &Origin,
    at: EventTime,
) -> Result<Bindings, EngineError> {
    build_with_overrides(env, roots, revisions, &BTreeMap::new(), root_origin, at)
}

#[cfg(test)]
pub(super) fn build_with_overrides(
    env: &TypeEnv,
    roots: &BTreeMap<String, Value>,
    revisions: &BTreeMap<String, RecordIdentity>,
    overrides: &BTreeMap<String, RecordIdentity>,
    root_origin: &Origin,
    at: EventTime,
) -> Result<Bindings, EngineError> {
    build_scoped(
        env,
        roots,
        revisions,
        overrides,
        &BTreeMap::new(),
        root_origin,
        at,
    )
}

pub(super) fn build_scoped(
    env: &TypeEnv,
    roots: &BTreeMap<String, Value>,
    revisions: &BTreeMap<String, RecordIdentity>,
    overrides: &BTreeMap<String, RecordIdentity>,
    aliases: &BTreeMap<String, Origin>,
    root_origin: &Origin,
    at: EventTime,
) -> Result<Bindings, EngineError> {
    fields(roots.len(), roots.keys(), env.binding_roots(), |name| {
        roots.contains_key(name)
    })?;
    let mut ingress = Ingress {
        env,
        revisions,
        overrides,
        aliases,
        counts: Counts::default(),
        copy: false,
    };
    let mut converted = BTreeMap::new();
    for copy in [false, true] {
        ingress.copy = copy;
        ingress.counts = Counts::default();
        ingress
            .counts
            .metadata(&root_origin.path, &root_origin.record)?;
        for (name, value) in roots {
            ingress.counts.bytes(name.len())?;
            if let Some(node) = ingress.node(
                value,
                &env.binding_roots()[name],
                name,
                &root_origin.record,
                0,
            )? {
                converted.insert(name.clone(), node);
            }
        }
    }
    Bindings::new(env, &converted, root_origin, at).map_err(|error| EngineError::new(error.code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use runbook_expr::{SchemaDef, TypeId};
    use serde_json::json;

    fn decl(ty: u32, required: bool, nullable: bool) -> FieldDecl {
        FieldDecl {
            ty: TypeId(ty),
            required,
            nullable,
        }
    }
    fn identity(revision: u64) -> RecordIdentity {
        RecordIdentity {
            execution_id: "execution".into(),
            step_run_id: None,
            iteration_path: vec![],
            revision,
        }
    }
    fn origin() -> Origin {
        Origin {
            path: String::new(),
            record: identity(1),
        }
    }
    fn time() -> EventTime {
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        }
    }
    fn env(types: Vec<TypeDef>, roots: BTreeMap<String, FieldDecl>) -> TypeEnv {
        TypeEnv::new(&SchemaDef { types, roots }).unwrap()
    }

    #[test]
    fn declared_numbers_use_the_declared_representation() {
        let env = env(
            vec![TypeDef::Int, TypeDef::Double],
            BTreeMap::from([
                ("integer".into(), decl(0, true, false)),
                ("double".into(), decl(1, true, false)),
            ]),
        );
        let roots = BTreeMap::from([("integer".into(), json!(1.0)), ("double".into(), json!(1))]);
        assert!(build(&env, &roots, &BTreeMap::new(), &origin(), time()).is_ok());
        let revisions = BTreeMap::new();
        let mut ingress = Ingress {
            env: &env,
            revisions: &revisions,
            overrides: &BTreeMap::new(),
            aliases: &BTreeMap::new(),
            counts: Counts::default(),
            copy: true,
        };
        let int = ingress
            .node(
                &json!(1.0),
                &decl(0, true, false),
                "integer",
                &identity(1),
                0,
            )
            .unwrap()
            .unwrap();
        let double = ingress
            .node(&json!(1), &decl(1, true, false), "double", &identity(1), 0)
            .unwrap()
            .unwrap();
        assert_eq!(int.value, InputValue::Int(1));
        assert_eq!(double.value, InputValue::Double(1.0));
        assert!(
            ingress
                .node(
                    &json!(1.5),
                    &decl(0, true, false),
                    "integer",
                    &identity(1),
                    0
                )
                .is_err()
        );
        assert!(
            ingress
                .node(
                    &json!("1"),
                    &decl(0, true, false),
                    "integer",
                    &identity(1),
                    0
                )
                .is_err()
        );
    }

    #[test]
    fn optional_absence_and_nullable_null_are_distinct() {
        let env = env(
            vec![TypeDef::Int],
            BTreeMap::from([
                ("optional".into(), decl(0, false, false)),
                ("nullable".into(), decl(0, true, true)),
            ]),
        );
        let roots = BTreeMap::from([("nullable".into(), Value::Null)]);
        assert!(build(&env, &roots, &BTreeMap::new(), &origin(), time()).is_ok());
        let mut explicit_null = roots;
        explicit_null.insert("optional".into(), Value::Null);
        assert!(build(&env, &explicit_null, &BTreeMap::new(), &origin(), time()).is_err());
        assert!(build(&env, &BTreeMap::new(), &BTreeMap::new(), &origin(), time()).is_err());
    }

    #[test]
    fn nearest_provenance_override_is_inherited_by_descendants() {
        let env = env(
            vec![
                TypeDef::Int,
                TypeDef::List(TypeId(0)),
                TypeDef::Record(BTreeMap::from([("items".into(), decl(1, true, false))])),
            ],
            BTreeMap::from([("record".into(), decl(2, true, false))]),
        );
        let revisions = BTreeMap::from([
            ("record".into(), identity(2)),
            ("record.items[1]".into(), identity(3)),
        ]);
        let value = json!({"items": [1, 2]});
        let mut ingress = Ingress {
            env: &env,
            revisions: &revisions,
            overrides: &BTreeMap::new(),
            aliases: &BTreeMap::new(),
            counts: Counts::default(),
            copy: true,
        };
        let record = ingress
            .node(&value, &decl(2, true, false), "record", &identity(1), 0)
            .unwrap()
            .unwrap();
        assert_eq!(record.origin.record.revision, 2);
        let InputValue::Record(fields) = record.value else {
            panic!()
        };
        assert_eq!(fields["items"].origin.record.revision, 2);
        let InputValue::List(items) = &fields["items"].value else {
            panic!()
        };
        assert_eq!(items[0].origin.path, "record.items[0]");
        assert_eq!(items[0].origin.record.revision, 2);
        assert_eq!(items[1].origin.record.revision, 3);
        assert!(
            build(
                &env,
                &BTreeMap::from([("record".into(), value)]),
                &revisions,
                &origin(),
                time()
            )
            .is_ok()
        );
    }

    #[test]
    fn selected_overrides_precede_baseline_revisions_at_each_path() {
        let env = env(
            vec![TypeDef::Int, TypeDef::List(TypeId(0))],
            BTreeMap::from([("items".into(), decl(1, true, false))]),
        );
        let revisions = BTreeMap::from([
            ("items".into(), identity(2)),
            ("items[1]".into(), identity(3)),
            ("items[2]".into(), identity(4)),
        ]);
        let overrides = BTreeMap::from([
            ("items".into(), identity(5)),
            ("items[1]".into(), identity(6)),
        ]);
        let value = json!([1, 2, 3]);
        let mut ingress = Ingress {
            env: &env,
            revisions: &revisions,
            overrides: &overrides,
            aliases: &BTreeMap::new(),
            counts: Counts::default(),
            copy: true,
        };
        let node = ingress
            .node(&value, &decl(1, true, false), "items", &identity(1), 0)
            .unwrap()
            .unwrap();
        assert_eq!(node.origin.record.revision, 5);
        let InputValue::List(items) = node.value else {
            panic!()
        };
        assert_eq!(items[0].origin.record.revision, 5);
        assert_eq!(items[1].origin.record.revision, 6);
        assert_eq!(items[2].origin.record.revision, 4);
        assert!(
            build_with_overrides(
                &env,
                &BTreeMap::from([("items".into(), value)]),
                &revisions,
                &overrides,
                &origin(),
                time(),
            )
            .is_ok()
        );
    }

    #[test]
    fn unused_oversized_override_is_ignored_and_selected_invalid_identity_fails_preflight() {
        let env = env(
            vec![TypeDef::Int],
            BTreeMap::from([("value".into(), decl(0, true, false))]),
        );
        let roots = BTreeMap::from([("value".into(), json!(1))]);
        let mut oversized = identity(2);
        oversized.execution_id = "x".repeat(8_388_609);
        let mut overrides = BTreeMap::from([("unused".into(), oversized)]);
        let revisions = BTreeMap::from([("value".into(), identity(0))]);
        overrides.insert("value".into(), identity(3));
        assert!(
            build_with_overrides(&env, &roots, &revisions, &overrides, &origin(), time(),).is_ok()
        );

        overrides.insert("value".into(), identity(0));
        let mut ingress = Ingress {
            env: &env,
            revisions: &revisions,
            overrides: &overrides,
            aliases: &BTreeMap::new(),
            counts: Counts::default(),
            copy: false,
        };
        assert!(
            ingress
                .node(
                    &roots["value"],
                    &decl(0, true, false),
                    "value",
                    &identity(1),
                    0
                )
                .is_err()
        );
        assert!(!ingress.copy);
        assert_eq!(ingress.counts.nodes, 1);
        assert!(
            build_with_overrides(&env, &roots, &revisions, &overrides, &origin(), time(),).is_err()
        );
    }

    #[test]
    fn scalar_root_alias_uses_selected_canonical_origin() {
        let env = env(
            vec![TypeDef::String],
            BTreeMap::from([("region".into(), decl(0, true, false))]),
        );
        let roots = BTreeMap::from([("region".into(), json!("west"))]);
        let selected = Origin {
            path: "foreach_items[0]".into(),
            record: RecordIdentity {
                step_run_id: Some("foreach-run".into()),
                ..identity(2)
            },
        };
        let revisions = BTreeMap::from([("foreach_items[0]".into(), identity(0))]);
        let overrides = BTreeMap::from([("foreach_items[0]".into(), identity(0))]);
        let mut aliases = BTreeMap::from([("region".into(), selected.clone())]);
        aliases.insert(
            "unused".into(),
            Origin {
                path: "x".repeat(1_025),
                record: identity(0),
            },
        );
        assert!(
            build_scoped(
                &env,
                &roots,
                &revisions,
                &overrides,
                &aliases,
                &origin(),
                time(),
            )
            .is_ok()
        );
        let mut ingress = Ingress {
            env: &env,
            revisions: &revisions,
            overrides: &overrides,
            aliases: &aliases,
            counts: Counts::default(),
            copy: true,
        };
        let node = ingress
            .node(
                &roots["region"],
                &decl(0, true, false),
                "region",
                &identity(1),
                0,
            )
            .unwrap()
            .unwrap();
        assert_eq!(node.origin, selected);

        aliases.get_mut("region").unwrap().record.revision = 0;
        assert!(
            build_scoped(
                &env,
                &roots,
                &revisions,
                &overrides,
                &aliases,
                &origin(),
                time(),
            )
            .is_err()
        );
        aliases.get_mut("region").unwrap().record.revision = 2;
        aliases.get_mut("region").unwrap().path = "x".repeat(1_025);
        assert!(
            build_scoped(
                &env,
                &roots,
                &revisions,
                &overrides,
                &aliases,
                &origin(),
                time(),
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_and_deep_json_are_rejected_in_borrowed_preflight() {
        let env = env(
            vec![TypeDef::Int],
            BTreeMap::from([("value".into(), decl(0, true, false))]),
        );
        let mut deep = json!(0);
        for _ in 0..100 {
            deep = json!([deep]);
        }
        let roots = BTreeMap::from([("unknown".into(), deep)]);
        assert!(build(&env, &roots, &BTreeMap::new(), &origin(), time()).is_err());
        // An unknown subtree is neither inspected nor converted; a known root
        // with an incompatible container also fails before traversing its tail.
        let value = &roots["unknown"];
        let revisions = BTreeMap::new();
        let mut ingress = Ingress {
            env: &env,
            revisions: &revisions,
            overrides: &BTreeMap::new(),
            aliases: &BTreeMap::new(),
            counts: Counts::default(),
            copy: false,
        };
        assert!(
            ingress
                .node(value, &decl(0, true, false), "value", &identity(1), 0)
                .is_err()
        );
        assert_eq!(ingress.counts.nodes, 1);
        let mut ingress = Ingress {
            env: &env,
            revisions: &revisions,
            overrides: &BTreeMap::new(),
            aliases: &BTreeMap::new(),
            counts: Counts::default(),
            copy: false,
        };
        assert!(
            ingress
                .node(value, &decl(0, true, false), "value", &identity(1), 33)
                .is_err()
        );
        assert_eq!(ingress.counts.nodes, 0);
    }
}
