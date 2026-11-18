use std::collections::BTreeMap;

use runbook_spec::expressions::{FieldDecl, SchemaDef, TypeDef, TypeEnv, TypeId, check_expression};

fn field(ty: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        required: true,
        nullable: false,
    }
}

#[test]
fn bad_declared_type_is_rejected() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::String,
            TypeDef::Record(BTreeMap::from([("rate".into(), field(0))])),
        ],
        roots: BTreeMap::from([("inputs".into(), field(1))]),
    })
    .unwrap();
    assert_eq!(
        check_expression("inputs.rate > 0.01", &env)
            .unwrap_err()
            .code,
        "expression.type_error"
    );
}

#[test]
fn evaluation_stops_at_budget() {
    use runbook_spec::expressions::{
        Bindings, EvalError, EventTime, InputNode, InputValue, Origin, RecordIdentity, evaluate,
    };
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Int, TypeDef::List(TypeId(0))],
        roots: BTreeMap::from([("items".into(), field(1))]),
    })
    .unwrap();
    let record = RecordIdentity {
        execution_id: "exe_budget".into(),
        step_run_id: None,
        iteration_path: vec![],
        revision: 1,
    };
    let origin = |path: String| Origin {
        path,
        record: record.clone(),
    };
    let roots = BTreeMap::from([(
        "items".into(),
        InputNode {
            origin: origin("items".into()),
            value: InputValue::List(
                (0..100_000)
                    .map(|value| InputNode {
                        origin: origin(format!("items[{value}]")),
                        value: InputValue::Int(value),
                    })
                    .collect(),
            ),
        },
    )]);
    let bindings = Bindings::new(
        &env,
        &roots,
        &origin(String::new()),
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        },
    )
    .unwrap();
    let expression = check_expression("items.exists(i, i > 999999)", &env).unwrap();
    assert!(matches!(
        evaluate(&expression, &bindings, 100),
        Err(EvalError::CostExceeded)
    ));
}

#[test]
fn reads_and_time_are_replayable() {
    use runbook_spec::expressions::{
        Bindings, EventTime, InputNode, InputValue, IterationIdentity, Origin, ReadKind, ReadRef,
        RecordIdentity, evaluate,
    };
    use serde_json::json;
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("code".into(), field(0))])),
            TypeDef::Record(BTreeMap::from([("outputs".into(), field(1))])),
            TypeDef::Record(BTreeMap::from([("health".into(), field(2))])),
        ],
        roots: BTreeMap::from([("steps".into(), field(3))]),
    })
    .unwrap();
    let container = RecordIdentity {
        execution_id: "exe_replay".into(),
        step_run_id: None,
        iteration_path: vec![],
        revision: 1,
    };
    let step = RecordIdentity {
        execution_id: "exe_replay".into(),
        step_run_id: Some("sr_health_second".into()),
        iteration_path: vec![IterationIdentity {
            node_id: "stabilize".into(),
            instance_id: "sr_stabilize".into(),
            ordinal: 2,
        }],
        revision: 7,
    };
    let expected_code_origin = Origin {
        path: "steps.health.outputs.code".into(),
        record: step.clone(),
    };
    let outputs = InputNode {
        origin: Origin {
            path: "steps.health.outputs".into(),
            record: step.clone(),
        },
        value: InputValue::Record(BTreeMap::from([(
            "code".into(),
            InputNode {
                origin: expected_code_origin.clone(),
                value: InputValue::Int(503),
            },
        )])),
    };
    let health = InputNode {
        origin: Origin {
            path: "steps.health".into(),
            record: step,
        },
        value: InputValue::Record(BTreeMap::from([("outputs".into(), outputs)])),
    };
    let roots = BTreeMap::from([(
        "steps".into(),
        InputNode {
            origin: Origin {
                path: "steps".into(),
                record: container.clone(),
            },
            value: InputValue::Record(BTreeMap::from([("health".into(), health)])),
        },
    )]);
    let bindings = Bindings::new(
        &env,
        &roots,
        &Origin {
            path: String::new(),
            record: container,
        },
        EventTime {
            unix_seconds: 42,
            nanos: 0,
        },
    )
    .unwrap();
    let expression = check_expression("steps.health.outputs.code == 503", &env).unwrap();
    let evaluated = evaluate(&expression, &bindings, 100).unwrap();
    assert_eq!(evaluated.value, json!(true));
    assert_eq!(
        evaluated.stats.reads,
        vec![ReadRef {
            kind: ReadKind::Value,
            origin: expected_code_origin
        }]
    );
    let now = check_expression("now()", &env).unwrap();
    let first = evaluate(&now, &bindings, 100).unwrap();
    let second = evaluate(&now, &bindings, 100).unwrap();
    assert_eq!(first.value, json!("1970-01-01T00:00:42Z"));
    assert_eq!(first.value, second.value);
    assert_eq!(first.stats, second.stats);
}

#[test]
fn yaml_diagnostic_keeps_scalar_anchor_and_source_path() {
    use runbook_spec::{SourceSpan, expressions::check_expression_at};
    let env = TypeEnv::new(&SchemaDef {
        types: vec![],
        roots: BTreeMap::new(),
    })
    .unwrap();
    let source = "'é' == 'é' &&\n missing";
    let standalone = check_expression(source, &env).unwrap_err();
    assert_eq!((standalone.line, standalone.column), (2, 2));
    let error = check_expression_at(
        source,
        &env,
        "/spec/steps/0/if",
        SourceSpan { line: 7, column: 9 },
    )
    .unwrap_err();
    assert_eq!(error.code, standalone.code);
    assert_eq!(error.path, "/spec/steps/0/if");
    // Decoded CEL offsets cannot be added to a quoted/escaped YAML scalar.
    assert_eq!((error.line, error.column), (7, 9));
}
