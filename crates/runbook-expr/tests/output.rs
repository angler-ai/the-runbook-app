use runbook_expr::*;
use serde_json::json;
use std::collections::BTreeMap;

fn origin(path: impl Into<String>) -> Origin {
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
fn node(path: impl Into<String>, value: InputValue) -> InputNode {
    InputNode {
        origin: origin(path),
        value,
    }
}
fn field(ty: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        nullable: false,
        required: true,
    }
}
fn fixture(types: Vec<TypeDef>, ty: u32, value: InputValue) -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types,
        roots: BTreeMap::from([("x".into(), field(ty))]),
    })
    .unwrap();
    let bindings = Bindings::new(
        &env,
        &BTreeMap::from([("x".into(), node("x", value))]),
        &origin(""),
        EventTime {
            unix_seconds: 123,
            nanos: 456_000_000,
        },
    )
    .unwrap();
    (env, bindings)
}
fn run(source: &str, env: &TypeEnv, bindings: &Bindings) -> EvaluatedValue {
    evaluate(&check_expression(source, env).unwrap(), bindings, 100_000).unwrap()
}

#[test]
fn output_conversion_is_precharged() {
    let (env, bindings) = fixture(vec![TypeDef::Int], 0, InputValue::Int(42));
    for (source, expected, cost) in [
        ("true", json!(true), 6),
        ("false", json!(false), 7),
        ("42", json!(42), 4),
        ("null", json!(null), 6),
        ("1.25", json!(1.25), 6),
    ] {
        let expr = check_expression(source, &env).unwrap();
        let result = evaluate(&expr, &bindings, cost).unwrap();
        assert_eq!(result.value, expected, "{source}");
        assert_eq!(result.stats.cost, cost, "{source}");
        assert!(result.stats.reads.is_empty());
        assert_eq!(
            evaluate(&expr, &bindings, cost - 1),
            Err(EvalError::CostExceeded)
        );
    }
    assert_eq!(run("x", &env, &bindings).value, json!(42));
    assert_eq!(
        run("now()", &env, &bindings).value,
        json!("1970-01-01T00:02:03.456Z")
    );
    assert_eq!(run("now()", &env, &bindings), run("now()", &env, &bindings));
    assert_eq!(
        run("timestamp('0001-01-01T00:00:00Z')", &env, &bindings).value,
        json!("0001-01-01T00:00:00Z")
    );
    assert_eq!(
        run(
            "timestamp('9999-12-31T23:59:59.999999999Z')",
            &env,
            &bindings
        )
        .value,
        json!("9999-12-31T23:59:59.999999999Z")
    );
    assert_eq!(
        evaluate(&check_expression("true", &env).unwrap(), &bindings, 100_001),
        Err(EvalError::CostExceeded)
    );
}

#[test]
fn whole_collections_record_returned_memberships_and_leaves() {
    let (env, bindings) = fixture(
        vec![
            TypeDef::Int,
            TypeDef::List(TypeId(0)),
            TypeDef::Record(BTreeMap::from([
                ("a".into(), field(1)),
                ("b".into(), field(0)),
            ])),
        ],
        2,
        InputValue::Record(BTreeMap::from([
            (
                "a".into(),
                node(
                    "x.a",
                    InputValue::List(vec![
                        node("x.a[0]", InputValue::Int(1)),
                        node("x.a[1]", InputValue::Int(2)),
                    ]),
                ),
            ),
            ("b".into(), node("x.b", InputValue::Int(3))),
        ])),
    );
    let result = run("true ? x : x", &env, &bindings);
    assert_eq!(result.value, json!({"a": [1, 2], "b": 3}));
    assert_eq!(
        result.stats.reads,
        [
            ("x", ReadKind::Membership),
            ("x.a", ReadKind::Membership),
            ("x.a[0]", ReadKind::Value),
            ("x.a[1]", ReadKind::Value),
            ("x.b", ReadKind::Value)
        ]
        .into_iter()
        .map(|(path, kind)| ReadRef {
            origin: origin(path),
            kind
        })
        .collect::<Vec<_>>()
    );
}

#[test]
fn encoded_string_bound_counts_quotes_controls_and_utf8() {
    for (text, accepted) in [
        ("a".repeat(65_534), true),
        ("a".repeat(65_535), false),
        ("\"".repeat(32_767), true),
        ("\"".repeat(32_768), false),
        ("\0".repeat(10_922), true),
        ("\0".repeat(10_923), false),
        ("é".repeat(32_767), true),
        ("é".repeat(32_768), false),
    ] {
        let (env, bindings) = fixture(vec![TypeDef::String], 0, InputValue::String(text.clone()));
        let expr = check_expression("x", &env).unwrap();
        let result = evaluate(&expr, &bindings, 100_000);
        if accepted {
            let result = result.unwrap();
            assert_eq!(result.value, json!(text));
            assert_eq!(
                result.stats.cost,
                serde_json::to_vec(&result.value).unwrap().len() as u64 + 6
            );
        } else {
            assert_eq!(result, Err(EvalError::OutputLimit));
        }
    }
}

#[test]
fn output_limits_and_cancellation_are_sticky_across_session_reuse() {
    let (env, bindings) = fixture(
        vec![TypeDef::String],
        0,
        InputValue::String("a".repeat(32_766)),
    );
    let expr = check_expression("x", &env).unwrap();
    let mut session = EvalSession::new(100_000).unwrap();
    let first = evaluate_in(&expr, &bindings, &mut session).unwrap();
    let second = evaluate_in(&expr, &bindings, &mut session).unwrap();
    assert_eq!(first.stats.cost, 32_774);
    assert_eq!(second.stats.cost, 65_545);
    assert_eq!(first.stats.reads, second.stats.reads);
    assert_eq!(
        evaluate_in(&expr, &bindings, &mut session),
        Err(EvalError::OutputLimit)
    );
    let stopped = session.stats();
    session.cancel();
    assert_eq!(
        evaluate_in(&expr, &bindings, &mut session),
        Err(EvalError::OutputLimit)
    );
    assert_eq!(session.stats(), stopped);
    let mut cancelled = EvalSession::new(100_000).unwrap();
    cancelled.cancel();
    assert_eq!(
        evaluate_in(&expr, &bindings, &mut cancelled),
        Err(EvalError::Cancelled)
    );
    assert_eq!(cancelled.stats().cost, 0);
}

#[test]
fn oversized_collection_rejects_before_visiting_children() {
    let (env, bindings) = fixture(
        vec![TypeDef::Int, TypeDef::List(TypeId(0))],
        1,
        InputValue::List(
            (0..100_000)
                .map(|i| node(format!("x[{i}]"), InputValue::Int(i)))
                .collect(),
        ),
    );
    let mut session = EvalSession::new(100_000).unwrap();
    assert_eq!(
        evaluate_in(
            &check_expression("x", &env).unwrap(),
            &bindings,
            &mut session
        ),
        Err(EvalError::OutputLimit)
    );
    assert!(session.stats().cost < 10);
    assert!(session.stats().reads.is_empty());
}

#[test]
fn cumulative_node_limit_counts_empty_containers() {
    let (env, bindings) = fixture(
        vec![TypeDef::Record(BTreeMap::new())],
        0,
        InputValue::Record(BTreeMap::new()),
    );
    let expr = check_expression("x", &env).unwrap();
    let mut session = EvalSession::new(100_000).unwrap();
    for _ in 0..16_384 {
        assert_eq!(
            evaluate_in(&expr, &bindings, &mut session).unwrap().value,
            json!({})
        );
    }
    assert_eq!(
        session.stats().reads,
        vec![ReadRef {
            origin: origin("x"),
            kind: ReadKind::Membership
        }]
    );
    assert_eq!(
        evaluate_in(&expr, &bindings, &mut session),
        Err(EvalError::OutputLimit)
    );
}

#[test]
fn schema_mismatch_precedes_dispatch_and_negative_zero_survives() {
    let (env, bindings) = fixture(vec![TypeDef::Double], 0, InputValue::Double(-0.0));
    let value = run("x", &env, &bindings).value;
    assert_eq!(serde_json::to_string(&value).unwrap(), "-0.0");
    let (other, _) = fixture(vec![TypeDef::Int], 0, InputValue::Int(1));
    let mut session = EvalSession::new(100_000).unwrap();
    assert_eq!(
        evaluate_in(
            &check_expression("true", &other).unwrap(),
            &bindings,
            &mut session
        ),
        Err(EvalError::SchemaMismatch)
    );
    assert_eq!(session.stats().cost, 0);
}

#[test]
fn nullable_record_result_preserves_presence_and_literal_null_has_no_read() {
    let mut declaration = field(0);
    declaration.nullable = true;
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Record(BTreeMap::new())],
        roots: BTreeMap::from([("x".into(), declaration)]),
    })
    .unwrap();
    let bindings = Bindings::new(
        &env,
        &BTreeMap::from([("x".into(), node("x", InputValue::Null))]),
        &origin(""),
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        },
    )
    .unwrap();
    let result = run("true ? x : null", &env, &bindings);
    assert_eq!(result.value, json!(null));
    assert_eq!(
        result.stats.reads,
        vec![ReadRef {
            origin: origin("x"),
            kind: ReadKind::Value
        }]
    );
    assert!(
        run("false ? x : null", &env, &bindings)
            .stats
            .reads
            .is_empty()
    );
}

#[test]
fn output_scalar_formats_match_json_for_numeric_extremes_and_all_escapes() {
    for number in [i64::MIN, i64::MAX] {
        let (env, bindings) = fixture(vec![TypeDef::Int], 0, InputValue::Int(number));
        let result = run("x", &env, &bindings);
        assert_eq!(result.value, json!(number));
        assert_eq!(
            result.stats.cost,
            serde_json::to_string(&number).unwrap().len() as u64 + 6
        );
    }
    for number in [
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        -0.0,
        0.0,
        1e20,
    ] {
        let (env, bindings) = fixture(vec![TypeDef::Double], 0, InputValue::Double(number));
        let result = run("x", &env, &bindings);
        assert_eq!(result.value.as_f64().unwrap().to_bits(), number.to_bits());
        assert_eq!(
            result.stats.cost,
            serde_json::to_string(&number).unwrap().len() as u64 + 6
        );
    }
    let text: String = (0..=31).map(char::from).chain("\\\"é🦀/".chars()).collect();
    let (env, bindings) = fixture(vec![TypeDef::String], 0, InputValue::String(text.clone()));
    let result = run("x", &env, &bindings);
    assert_eq!(result.value, json!(text));
    assert_eq!(result.stats.cost, 185 + 6); // 27*6 + 5*2 control bytes, 11 suffix bytes, two quotes.
    let (env, bindings) = fixture(
        vec![TypeDef::Timestamp],
        0,
        InputValue::Timestamp(EventTime {
            unix_seconds: -1,
            nanos: 1,
        }),
    );
    assert_eq!(
        run("x", &env, &bindings).value,
        json!("1969-12-31T23:59:59.000000001Z")
    );
}

#[test]
fn output_read_limit_is_sticky_and_never_returns_partial_value() {
    let (env, bindings) = fixture(
        vec![TypeDef::Int, TypeDef::List(TypeId(0))],
        1,
        InputValue::List(
            (0..4096)
                .map(|i| node(format!("x[{i}]"), InputValue::Int(0)))
                .collect(),
        ),
    );
    let expr = check_expression("x", &env).unwrap();
    let mut session = EvalSession::new(100_000).unwrap();
    assert_eq!(
        evaluate_in(&expr, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    let stopped = session.stats();
    assert_eq!(stopped.reads.len(), 4096);
    assert_eq!(
        stopped.reads.last().unwrap(),
        &ReadRef {
            origin: origin("x[4094]"),
            kind: ReadKind::Value
        }
    );
    let literal = check_expression("true", &env).unwrap();
    assert_eq!(
        evaluate_in(&literal, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    assert_eq!(session.stats(), stopped);
}

#[test]
fn output_fuel_failure_precharges_raw_scan_and_remains_terminal() {
    let (env, bindings) = fixture(
        vec![TypeDef::String],
        0,
        InputValue::String("a".repeat(5000)),
    );
    let expr = check_expression("x", &env).unwrap();
    let mut session = EvalSession::new(100).unwrap();
    assert_eq!(
        evaluate_in(&expr, &bindings, &mut session),
        Err(EvalError::CostExceeded)
    );
    let stopped = session.stats();
    assert_eq!(stopped.cost, 8); // dispatch/lookup/read = 5; node + quotes = 3; raw scan fails.
    assert_eq!(
        evaluate_in(&expr, &bindings, &mut session),
        Err(EvalError::CostExceeded)
    );
    assert_eq!(session.stats(), stopped);
}

#[test]
fn public_missing_and_index_errors_stay_structured_and_nonterminal() {
    let (env, bindings) = fixture(
        vec![TypeDef::Int, TypeDef::List(TypeId(0))],
        1,
        InputValue::List(vec![]),
    );
    let mut session = EvalSession::new(100_000).unwrap();
    assert_eq!(
        evaluate_in(
            &check_expression("x[0]", &env).unwrap(),
            &bindings,
            &mut session
        ),
        Err(EvalError::IndexOutOfBounds)
    );
    assert_eq!(
        evaluate_in(
            &check_expression("true", &env).unwrap(),
            &bindings,
            &mut session
        )
        .unwrap()
        .value,
        json!(true)
    );
    let mut optional = field(0);
    optional.required = false;
    let (env, bindings) = fixture(
        vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("a".into(), optional)])),
        ],
        1,
        InputValue::Record(BTreeMap::new()),
    );
    let mut session = EvalSession::new(100_000).unwrap();
    assert_eq!(
        evaluate_in(
            &check_expression("x.a", &env).unwrap(),
            &bindings,
            &mut session
        ),
        Err(EvalError::MissingValue { path: "x.a".into() })
    );
    let result = evaluate_in(
        &check_expression("x", &env).unwrap(),
        &bindings,
        &mut session,
    )
    .unwrap();
    assert_eq!(result.value, json!({}));
    assert_eq!(
        result.stats.reads,
        vec![
            ReadRef {
                origin: origin("x.a"),
                kind: ReadKind::Absence
            },
            ReadRef {
                origin: origin("x"),
                kind: ReadKind::Membership
            }
        ]
    );
}
