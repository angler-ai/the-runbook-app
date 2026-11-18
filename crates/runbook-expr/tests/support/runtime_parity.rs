//! Public evaluation evidence with hand-specified values and ordered read owners.
//! Costs are measured differential evidence, except explicitly derived bounds.
use runbook_expr::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn origin(path: &str) -> Origin {
    Origin {
        path: path.into(),
        record: RecordIdentity {
            execution_id: "e".into(),
            step_run_id: None,
            iteration_path: vec![],
            revision: 7,
        },
    }
}
fn node(path: &str, value: InputValue) -> InputNode {
    InputNode {
        value,
        origin: origin(path),
    }
}
fn decl(ty: u32, nullable: bool, required: bool) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        nullable,
        required,
    }
}
fn read(path: &str, kind: ReadKind) -> ReadRef {
    ReadRef {
        origin: origin(path),
        kind,
    }
}
fn reads(entries: &[(&str, ReadKind)]) -> Vec<ReadRef> {
    entries
        .iter()
        .map(|(path, kind)| read(path, *kind))
        .collect()
}
fn bindings(env: &TypeEnv, roots: BTreeMap<String, InputNode>, owner: Origin) -> Bindings {
    Bindings::new(
        env,
        &roots,
        &owner,
        EventTime {
            unix_seconds: -1,
            nanos: 1,
        },
    )
    .unwrap()
}
fn fixture(types: Vec<TypeDef>, ty: u32, value: InputValue) -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types,
        roots: BTreeMap::from([("x".into(), decl(ty, false, true))]),
    })
    .unwrap();
    let bound = bindings(
        &env,
        BTreeMap::from([("x".into(), node("x", value))]),
        origin(""),
    );
    (env, bound)
}
fn list_fixture(len: usize) -> (TypeEnv, Bindings) {
    assert!(len <= 100_000);
    fixture(
        vec![TypeDef::Int, TypeDef::List(TypeId(0))],
        1,
        InputValue::List(
            (0..len)
                .map(|i| node(&format!("x[{i}]"), InputValue::Int(i as i64 + 1)))
                .collect(),
        ),
    )
}
fn encoded_error(error: &EvalError) -> Value {
    // Exhaustive structured encoding: no Debug/Display dependency in evidence.
    match error {
        EvalError::CostExceeded => json!({"kind": "CostExceeded"}),
        EvalError::Cancelled => json!({"kind": "Cancelled"}),
        EvalError::ReadLimit => json!({"kind": "ReadLimit"}),
        EvalError::OutputLimit => json!({"kind": "OutputLimit"}),
        EvalError::SchemaMismatch => json!({"kind": "SchemaMismatch"}),
        EvalError::MissingValue { path } => json!({"kind": "MissingValue", "path": path}),
        EvalError::TypeError => json!({"kind": "TypeError"}),
        EvalError::IndexOutOfBounds => json!({"kind": "IndexOutOfBounds"}),
    }
}
struct Check<'a> {
    name: &'a str,
    source: &'a str,
    expected: Result<Value, EvalError>,
    reads: Vec<ReadRef>,
    loops: u64,
}
fn segment(
    rows: &mut Vec<Value>,
    env: &TypeEnv,
    bound: &Bindings,
    session: &mut EvalSession,
    check: Check<'_>,
) {
    let expr = check_expression(check.source, env).unwrap();
    let result = evaluate_in(&expr, bound, session);
    let stats = session.stats();
    if let Ok(value) = &result {
        assert_eq!(
            value.stats, stats,
            "{} returned cumulative stats",
            check.name
        );
    }
    let actual = result.map(|value| value.value);
    assert_eq!(actual, check.expected, "{} value/error", check.name);
    assert_eq!(
        stats.reads, check.reads,
        "{} complete ordered reads",
        check.name
    );
    assert_eq!(
        stats.loop_entries, check.loops,
        "{} loop entries",
        check.name
    );
    let outcome = match actual {
        Ok(value) => json!({"value": value, "stats": stats}),
        Err(error) => json!({"error": encoded_error(&error), "stats": stats}),
    };
    rows.push(json!({"name": format!("runtime/{}", check.name), "outcome": outcome}));
}
fn fresh(rows: &mut Vec<Value>, env: &TypeEnv, bound: &Bindings, check: Check<'_>) {
    // Both public entry points must agree, including exact integer JSON numbers.
    let expr = check_expression(check.source, env).unwrap();
    let convenience = evaluate(&expr, bound, 100_000);
    assert_eq!(
        convenience
            .as_ref()
            .map(|v| v.value.clone())
            .map_err(Clone::clone),
        check.expected,
        "{} convenience value/error",
        check.name
    );
    let mut session = EvalSession::new(100_000).unwrap();
    segment(rows, env, bound, &mut session, check);
    if let Ok(value) = convenience {
        assert_eq!(value.stats, session.stats());
    }
}

pub fn append(rows: &mut Vec<Value>) {
    use ReadKind::{Absence as A, Membership as M, Value as V};
    let (env, bound) = list_fixture(3);
    for (name, source, value, expected_reads, loops) in [
        (
            "exists-first",
            "x.exists(i, i == 1)",
            json!(true),
            reads(&[("x", M), ("x[0]", V)]),
            2,
        ),
        (
            "exists-middle",
            "x.exists(i, i == 2)",
            json!(true),
            reads(&[("x", M), ("x[0]", V), ("x[1]", V)]),
            3,
        ),
        (
            "exists-no-match",
            "x.exists(i, i == 9)",
            json!(false),
            reads(&[("x", M), ("x[0]", V), ("x[1]", V), ("x[2]", V)]),
            3,
        ),
        (
            "scalar-alias-unused",
            "x.exists(i, true)",
            json!(true),
            reads(&[("x", M)]),
            2,
        ),
        (
            "direct-index",
            "x[1]",
            json!(2),
            reads(&[("x", M), ("x[1]", V)]),
            0,
        ),
        (
            "whole-list",
            "true ? x : x",
            json!([1, 2, 3]),
            reads(&[("x", M), ("x[0]", V), ("x[1]", V), ("x[2]", V)]),
            0,
        ),
    ] {
        fresh(
            rows,
            &env,
            &bound,
            Check {
                name,
                source,
                expected: Ok(value),
                reads: expected_reads,
                loops,
            },
        );
    }
    let mut session = EvalSession::new(100_000).unwrap();
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "index-out-of-range",
            source: "x[99]",
            expected: Err(EvalError::IndexOutOfBounds),
            reads: reads(&[("x", M)]),
            loops: 0,
        },
    );
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "index-error-recovery",
            source: "x[0]",
            expected: Ok(json!(1)),
            reads: reads(&[("x", M), ("x[0]", V)]),
            loops: 0,
        },
    );
    let (empty_env, empty_bound) = list_fixture(0);
    fresh(
        rows,
        &empty_env,
        &empty_bound,
        Check {
            name: "exists-empty",
            source: "x.exists(i, true)",
            expected: Ok(json!(false)),
            reads: reads(&[("x", M)]),
            loops: 0,
        },
    );

    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("code".into(), decl(0, false, false))])),
        ],
        roots: BTreeMap::from([
            ("x".into(), decl(0, true, true)),
            ("r".into(), decl(1, true, true)),
            ("missing".into(), decl(0, false, false)),
            ("present".into(), decl(1, false, true)),
        ]),
    })
    .unwrap();
    let bound = bindings(
        &env,
        BTreeMap::from([
            ("x".into(), node("x", InputValue::Null)),
            ("r".into(), node("r", InputValue::Null)),
            (
                "present".into(),
                node(
                    "present",
                    InputValue::Record(BTreeMap::from([(
                        "code".into(),
                        node("present.code", InputValue::Int(200)),
                    )])),
                ),
            ),
        ]),
        origin(""),
    );
    for (name, source, expected, expected_reads) in [
        ("null-leaf", "x", Ok(json!(null)), reads(&[("x", V)])),
        (
            "null-comparison",
            "x == null",
            Ok(json!(true)),
            reads(&[("x", V)]),
        ),
        (
            "missing-root",
            "missing",
            Err(EvalError::MissingValue {
                path: "missing".into(),
            }),
            reads(&[("missing", A)]),
        ),
        (
            "nullable-record",
            "true ? r : null",
            Ok(json!(null)),
            reads(&[("r", V)]),
        ),
        (
            "nullable-record-presence",
            "r == null",
            Ok(json!(true)),
            reads(&[("r", V)]),
        ),
        (
            "present-record-presence",
            "present != null",
            Ok(json!(true)),
            reads(&[("present", V)]),
        ),
        (
            "nullable-record-child",
            "(true ? r : null).code",
            Err(EvalError::MissingValue {
                path: "r.code".into(),
            }),
            reads(&[("r.code", A)]),
        ),
        (
            "literal-null-child",
            "(false ? r : null).code",
            Err(EvalError::TypeError),
            vec![],
        ),
        ("short-or", "true || missing == 1", Ok(json!(true)), vec![]),
        (
            "short-and",
            "false && missing == 1",
            Ok(json!(false)),
            vec![],
        ),
        (
            "short-ternary",
            "true ? present.code : missing",
            Ok(json!(200)),
            reads(&[("present.code", V)]),
        ),
        (
            "whole-record",
            "present",
            Ok(json!({"code": 200})),
            reads(&[("present", M), ("present.code", V)]),
        ),
    ] {
        fresh(
            rows,
            &env,
            &bound,
            Check {
                name,
                source,
                expected,
                reads: expected_reads,
                loops: 0,
            },
        );
    }
    amendment_rows(rows);
    scalar_rows(rows);
    limit_rows(rows);
    terminal_rows(rows);
}

fn amendment_rows(rows: &mut Vec<Value>) {
    use ReadKind::{Absence as A, Value as V};
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("code".into(), decl(0, false, false))])),
        ],
        roots: BTreeMap::from([
            ("x".into(), decl(0, false, false)),
            ("r".into(), decl(1, false, false)),
        ]),
    })
    .unwrap();
    let mut session = EvalSession::new(100_000).unwrap();
    let mut root_reads = Vec::new();
    for (name, revision) in [("root-amendment-7", 7), ("root-amendment-8", 8)] {
        let mut owner = origin("");
        owner.record.revision = revision;
        owner.record.step_run_id = Some("root-snapshot".into());
        let bound = bindings(&env, BTreeMap::new(), owner);
        let mut expected = read("x", A);
        expected.origin.record.revision = revision;
        expected.origin.record.step_run_id = Some("root-snapshot".into());
        root_reads.push(expected);
        segment(
            rows,
            &env,
            &bound,
            &mut session,
            Check {
                name,
                source: "x",
                expected: Err(EvalError::MissingValue { path: "x".into() }),
                reads: root_reads.clone(),
                loops: 0,
            },
        );
    }
    let mut session = EvalSession::new(100_000).unwrap();
    let mut field_reads = Vec::new();
    let mut first_stats = None;
    // Each tuple changes precisely one part of the full identity after amendment.
    for (name, value, revision, step, instance, ordinal) in [
        ("field-amendment-7", 200, 7, "health-a", "batch-a", 0),
        ("field-amendment-8", 201, 8, "health-a", "batch-a", 0),
        ("iteration-ordinal", 202, 8, "health-a", "batch-a", 1),
        ("iteration-instance", 203, 8, "health-a", "batch-b", 1),
        ("iteration-step-run", 204, 8, "health-b", "batch-b", 1),
    ] {
        let identity = RecordIdentity {
            execution_id: "e".into(),
            step_run_id: Some(step.into()),
            revision,
            iteration_path: vec![
                IterationIdentity {
                    node_id: "outer".into(),
                    instance_id: "outer-run".into(),
                    ordinal: 2,
                },
                IterationIdentity {
                    node_id: "foreach".into(),
                    instance_id: instance.into(),
                    ordinal,
                },
            ],
        };
        let leaf = InputNode {
            value: InputValue::Int(value),
            origin: Origin {
                path: "r.code".into(),
                record: identity.clone(),
            },
        };
        let container = InputNode {
            value: InputValue::Record(BTreeMap::from([("code".into(), leaf)])),
            origin: Origin {
                path: "r".into(),
                record: identity.clone(),
            },
        };
        let bound = bindings(&env, BTreeMap::from([("r".into(), container)]), origin(""));
        field_reads.push(ReadRef {
            origin: Origin {
                path: "r.code".into(),
                record: identity,
            },
            kind: V,
        });
        segment(
            rows,
            &env,
            &bound,
            &mut session,
            Check {
                name,
                source: "r.code",
                expected: Ok(json!(value)),
                reads: field_reads.clone(),
                loops: 0,
            },
        );
        if first_stats.is_none() {
            first_stats = Some(session.stats());
        }
    }
    assert_eq!(
        first_stats.unwrap().reads,
        field_reads[..1],
        "amendments must not mutate returned traces"
    );
}

fn scalar_rows(rows: &mut Vec<Value>) {
    let (env, bound) = fixture(vec![TypeDef::Int], 0, InputValue::Int(1));
    for (name, source, expected) in [
        ("now", "now()", json!("1969-12-31T23:59:59.000000001Z")),
        (
            "timestamp-min",
            "timestamp('0001-01-01T00:00:00Z')",
            json!("0001-01-01T00:00:00Z"),
        ),
        (
            "timestamp-max",
            "timestamp('9999-12-31T23:59:59.999999999Z')",
            json!("9999-12-31T23:59:59.999999999Z"),
        ),
        (
            "timestamp-leap-offset",
            "timestamp('2000-02-29T01:30:00.120000000+01:30')",
            json!("2000-02-29T00:00:00.12Z"),
        ),
    ] {
        fresh(
            rows,
            &env,
            &bound,
            Check {
                name,
                source,
                expected: Ok(expected),
                reads: vec![],
                loops: 0,
            },
        );
    }
    for (name, number) in [("int-min", i64::MIN), ("int-max", i64::MAX)] {
        let (env, bound) = fixture(vec![TypeDef::Int], 0, InputValue::Int(number));
        fresh(
            rows,
            &env,
            &bound,
            Check {
                name,
                source: "x",
                expected: Ok(json!(number)),
                reads: vec![read("x", ReadKind::Value)],
                loops: 0,
            },
        );
    }
    let (env, bound) = fixture(vec![TypeDef::Double], 0, InputValue::Double(-0.0));
    fresh(
        rows,
        &env,
        &bound,
        Check {
            name: "negative-zero",
            source: "x",
            expected: Ok(json!(-0.0)),
            reads: vec![read("x", ReadKind::Value)],
            loops: 0,
        },
    );
    let result = &rows.last().unwrap()["outcome"]["value"];
    assert_eq!(result.as_f64().unwrap().to_bits(), (-0.0f64).to_bits());
    assert_eq!(serde_json::to_string(result).unwrap(), "-0.0");
}

fn limit_rows(rows: &mut Vec<Value>) {
    use ReadKind::{Membership as M, Value as V};
    let (env, bound) = list_fixture(100_000);
    let mut session = EvalSession::new(100_000).unwrap();
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "large-output-early-reject",
            source: "x",
            expected: Err(EvalError::OutputLimit),
            reads: vec![],
            loops: 0,
        },
    );
    assert!(
        session.stats().cost < 10,
        "must reject before scanning 100k children"
    );
    let stopped = session.stats();
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "large-output-sticky",
            source: "true",
            expected: Err(EvalError::OutputLimit),
            reads: vec![],
            loops: 0,
        },
    );
    assert_eq!(session.stats(), stopped);
    for (name, text, accepted) in [
        ("output-exact-cap", "a".repeat(65_534), true),
        ("output-over-cap", "a".repeat(65_535), false),
        ("escaped-output-exact", "\"".repeat(32_767), true),
        ("escaped-output-over", "\"".repeat(32_768), false),
        ("control-output-over", "\0".repeat(10_923), false),
        ("utf8-output-exact", "é".repeat(32_767), true),
    ] {
        let (env, bound) = fixture(vec![TypeDef::String], 0, InputValue::String(text.clone()));
        let mut session = EvalSession::new(100_000).unwrap();
        segment(
            rows,
            &env,
            &bound,
            &mut session,
            Check {
                name,
                source: "x",
                expected: if accepted {
                    Ok(json!(text))
                } else {
                    Err(EvalError::OutputLimit)
                },
                reads: vec![read("x", V)],
                loops: 0,
            },
        );
        if accepted {
            assert_eq!(
                session.stats().cost,
                65_542,
                "65536 encoded bytes + root dispatch/lookup/read 5 + output node 1"
            );
        }
    }
    let (env, bound) = fixture(
        vec![TypeDef::String],
        0,
        InputValue::String("a".repeat(32_766)),
    );
    let mut session = EvalSession::new(100_000).unwrap();
    for name in ["cumulative-cap-first", "cumulative-cap-exact"] {
        segment(
            rows,
            &env,
            &bound,
            &mut session,
            Check {
                name,
                source: "x",
                expected: Ok(json!("a".repeat(32_766))),
                reads: vec![read("x", V)],
                loops: 0,
            },
        );
    }
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "cumulative-cap-over",
            source: "true",
            expected: Err(EvalError::OutputLimit),
            reads: vec![read("x", V)],
            loops: 0,
        },
    );
    let stopped = session.stats();
    session.cancel();
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "cumulative-cap-sticky",
            source: "false",
            expected: Err(EvalError::OutputLimit),
            reads: vec![read("x", V)],
            loops: 0,
        },
    );
    assert_eq!(session.stats(), stopped);

    let (env, bound) = list_fixture(4096);
    let expected_reads: Vec<_> = std::iter::once(read("x", M))
        .chain((0..4095).map(|i| read(&format!("x[{i}]"), V)))
        .collect();
    let mut session = EvalSession::new(100_000).unwrap();
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "read-limit",
            source: "x",
            expected: Err(EvalError::ReadLimit),
            reads: expected_reads.clone(),
            loops: 0,
        },
    );
    let stopped = session.stats();
    segment(
        rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "read-limit-sticky",
            source: "true",
            expected: Err(EvalError::ReadLimit),
            reads: expected_reads,
            loops: 0,
        },
    );
    assert_eq!(session.stats(), stopped);
}

fn terminal_rows(rows: &mut Vec<Value>) {
    let (env, bound) = fixture(vec![TypeDef::Int], 0, InputValue::Int(1));
    for (name, source, next_name) in [
        ("fuel-or", "x == 1 || true", "fuel-or-sticky"),
        ("fuel-and", "x == 1 && false", "fuel-and-sticky"),
        (
            "fuel-ternary",
            "x == 1 ? true : false",
            "fuel-ternary-sticky",
        ),
    ] {
        // Outer dispatch + compare + x dispatch/lookup = 4; the x read
        // precharges 1 emission + 1 path byte + 1 execution-id byte = 3.
        // The next literal dispatch fails. No boolean form may suppress it.
        let mut session = EvalSession::new(7).unwrap();
        segment(
            rows,
            &env,
            &bound,
            &mut session,
            Check {
                name,
                source,
                expected: Err(EvalError::CostExceeded),
                reads: vec![read("x", ReadKind::Value)],
                loops: 0,
            },
        );
        assert_eq!(session.stats().cost, 7);
        let stopped = session.stats();
        session.cancel();
        segment(
            rows,
            &env,
            &bound,
            &mut session,
            Check {
                name: next_name,
                source: "true",
                expected: Err(EvalError::CostExceeded),
                reads: vec![read("x", ReadKind::Value)],
                loops: 0,
            },
        );
        assert_eq!(session.stats(), stopped);
    }
    for (name, source, prefix, next_name) in [
        (
            "cancel-or",
            "x == 1 || true",
            "cancel-or-prefix",
            "cancel-or-sticky",
        ),
        (
            "cancel-and",
            "x == 1 && false",
            "cancel-and-prefix",
            "cancel-and-sticky",
        ),
        (
            "cancel-ternary",
            "x == 1 ? true : false",
            "cancel-ternary-prefix",
            "cancel-ternary-sticky",
        ),
    ] {
        let mut session = EvalSession::new(100_000).unwrap();
        segment(
            rows,
            &env,
            &bound,
            &mut session,
            Check {
                name: prefix,
                source: "x",
                expected: Ok(json!(1)),
                reads: vec![read("x", ReadKind::Value)],
                loops: 0,
            },
        );
        let stopped = session.stats();
        session.cancel();
        for (row_name, expression) in [(name, source), (next_name, "true")] {
            segment(
                rows,
                &env,
                &bound,
                &mut session,
                Check {
                    name: row_name,
                    source: expression,
                    expected: Err(EvalError::Cancelled),
                    reads: vec![read("x", ReadKind::Value)],
                    loops: 0,
                },
            );
            assert_eq!(session.stats(), stopped);
        }
    }
}
