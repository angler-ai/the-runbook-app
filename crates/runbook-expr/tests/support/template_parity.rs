//! Shared native/Wasm corpus rows. Every row is checked against a hand-derived
//! outcome, full ordered provenance, zero loop entries, and exact session cost.
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
fn field(ty: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        nullable: false,
        required: true,
    }
}
fn time() -> EventTime {
    EventTime {
        unix_seconds: 123,
        nanos: 456_000_000,
    }
}
fn fixture(
    types: Vec<TypeDef>,
    roots: Vec<(&str, FieldDecl, Option<InputValue>)>,
) -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types,
        roots: roots
            .iter()
            .map(|(name, decl, _)| ((*name).into(), decl.clone()))
            .collect(),
    })
    .unwrap();
    let roots = roots
        .into_iter()
        .filter_map(|(name, _, value)| value.map(|value| (name.into(), node(name, value))))
        .collect();
    let bound = Bindings::new(&env, &roots, &origin(""), time()).unwrap();
    (env, bound)
}
fn reads(entries: &[(&str, ReadKind)]) -> Vec<ReadRef> {
    entries
        .iter()
        .map(|(path, kind)| ReadRef {
            origin: origin(path),
            kind: *kind,
        })
        .collect()
}
fn encoded_error(error: &EvalError) -> Value {
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
    cost: u64,
}
fn segment(
    rows: &mut Vec<Value>,
    env: &TypeEnv,
    bound: &Bindings,
    session: &mut EvalSession,
    check: Check<'_>,
) {
    let checked = check_template(check.source, env).unwrap();
    let result = render_template(&checked, bound, session);
    let stats = session.stats();
    if let Ok(result) = &result {
        assert_eq!(result.stats, stats, "{} cumulative stats", check.name);
    }
    let actual = result.map(|result| result.value);
    assert_eq!(actual, check.expected, "{} value/error", check.name);
    assert_eq!(
        stats.reads, check.reads,
        "{} complete ordered origins/kinds",
        check.name
    );
    assert_eq!(stats.cost, check.cost, "{} exact cost", check.name);
    assert_eq!(stats.loop_entries, 0, "{} loop entries", check.name);
    let outcome = match actual {
        Ok(value) => json!({"value": value, "stats": stats}),
        Err(error) => json!({"error": encoded_error(&error), "stats": stats}),
    };
    rows.push(json!({"name": format!("template/{}", check.name), "outcome": outcome}));
}
fn fresh(rows: &mut Vec<Value>, env: &TypeEnv, bound: &Bindings, check: Check<'_>) {
    segment(
        rows,
        env,
        bound,
        &mut EvalSession::new(100_000).unwrap(),
        check,
    );
}

pub fn rows() -> Vec<Value> {
    use ReadKind::{Absence as A, Membership as M, Value as V};
    let mut rows = Vec::new();
    let (env, bound) = fixture(
        vec![
            TypeDef::String,
            TypeDef::List(TypeId(0)),
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("a".into(), field(2))])),
        ],
        vec![
            (
                "x",
                field(1),
                Some(InputValue::List(vec![node(
                    "x[0]",
                    InputValue::String("us".into()),
                )])),
            ),
            (
                "r",
                field(3),
                Some(InputValue::Record(BTreeMap::from([(
                    "a".into(),
                    node("r.a", InputValue::Int(7)),
                )]))),
            ),
        ],
    );
    for (name, source, value, owners, cost) in [
        (
            "whole-list",
            "${{x}}",
            json!(["us"]),
            reads(&[("x", M), ("x[0]", V)]),
            19,
        ),
        (
            "whole-record",
            "${{r}}",
            json!({"a":7}),
            reads(&[("r", M), ("r.a", V)]),
            19,
        ),
    ] {
        assert_eq!(
            check_template(source, &env).unwrap().mode(),
            TemplateMode::WholeValue
        );
        fresh(
            &mut rows,
            &env,
            &bound,
            Check {
                name,
                source,
                expected: Ok(value),
                reads: owners,
                cost,
            },
        );
    }
    for (name, source) in [
        ("text-list-error", " ${{x}}"),
        ("text-record-error", " ${{r}}"),
    ] {
        assert_eq!(
            check_template(source, &env).unwrap().mode(),
            TemplateMode::Text
        );
        fresh(
            &mut rows,
            &env,
            &bound,
            Check {
                name,
                source,
                expected: Err(EvalError::TypeError),
                reads: vec![],
                cost: 6,
            },
        );
    }
    let (empty, empty_bound) = fixture(vec![], vec![]);
    for (name, source, expected, cost) in [
        // 29 content bytes + 3 overhead + 6 literal dispatches + 2 unary negatives.
        (
            "scalars",
            "${{null}}/${{true}}/${{false}}/${{-42}}/${{1.25}}/${{-0.0}}",
            "null/true/false/-42/1.25/-0.0",
            40,
        ),
        // 12 encoded content bytes + quotes/node (3) + two dispatches.
        (
            "utf8-escaped-cost",
            "a${{true}}\"\né${{7}}",
            "atrue\"\né7",
            17,
        ),
        ("quoted-delimiter", "[${{'}}'}}]", "[}}]", 10),
        // Six decoded UTF-8 literal bytes, seven encoded content bytes, brackets,
        // one dispatch and the three-unit text output overhead.
        ("escaped-delimiter", "[${{\"a\\\"}}é\"}}]", "[a\"}}é]", 19),
        (
            "commented-delimiter",
            "[${{true // }} ignored\n}}]",
            "[true]",
            10,
        ),
        (
            "supplied-time",
            "t=${{now()}}",
            "t=1970-01-01T00:02:03.456Z",
            30,
        ),
    ] {
        fresh(
            &mut rows,
            &empty,
            &empty_bound,
            Check {
                name,
                source,
                expected: Ok(json!(expected)),
                reads: vec![],
                cost,
            },
        );
    }
    let (env, bound) = fixture(
        vec![TypeDef::String],
        vec![(
            "x",
            field(0),
            Some(InputValue::String("${{not_code}}".into())),
        )],
    );
    fresh(
        &mut rows,
        &env,
        &bound,
        Check {
            name: "nonrecursive",
            source: "${{x}}/${{x}}",
            expected: Ok(json!("${{not_code}}/${{not_code}}")),
            reads: reads(&[("x", V)]),
            cost: 37,
        },
    );
    let (env, bound) = fixture(
        vec![TypeDef::Record(BTreeMap::new())],
        vec![(
            "x",
            FieldDecl {
                nullable: true,
                ..field(0)
            },
            Some(InputValue::Null),
        )],
    );
    for (name, source, owners, cost) in [
        ("bound-null", "[${{x}}]", reads(&[("x", V)]), 14),
        ("literal-null", "[${{false ? x : null}}]", vec![], 12),
    ] {
        fresh(
            &mut rows,
            &env,
            &bound,
            Check {
                name,
                source,
                expected: Ok(json!("[null]")),
                reads: owners,
                cost,
            },
        );
    }
    let (env, bound) = fixture(
        vec![TypeDef::Int],
        vec![
            ("a", field(0), Some(InputValue::Int(7))),
            ("b", field(0), Some(InputValue::Int(8))),
            ("c", field(0), Some(InputValue::Int(9))),
        ],
    );
    for (name, source, value, path) in [
        ("fuel-single-a", "${{a}}!", "7!", "a"),
        ("fuel-single-b", "${{b}}!", "8!", "b"),
    ] {
        segment(
            &mut rows,
            &env,
            &bound,
            &mut EvalSession::new(12).unwrap(),
            Check {
                name,
                source,
                expected: Ok(json!(value)),
                reads: reads(&[(path, V)]),
                cost: 10,
            },
        );
    }
    let mut prefix = EvalSession::new(12).unwrap();
    let mut full = EvalSession::new(12).unwrap();
    for (name, source, session) in [
        ("fuel-prefix", "${{a}}${{b}}", &mut prefix),
        ("fuel-third-unread", "${{a}}${{b}}${{c}}", &mut full),
    ] {
        // 3 overhead + a dispatch/lookup/read/output (6) + b dispatch/lookup (2).
        // The 3-unit b read cannot be charged; c is never dispatched.
        segment(
            &mut rows,
            &env,
            &bound,
            session,
            Check {
                name,
                source,
                expected: Err(EvalError::CostExceeded),
                reads: reads(&[("a", V)]),
                cost: 11,
            },
        );
    }
    assert_eq!(prefix.stats(), full.stats());
    segment(
        &mut rows,
        &env,
        &bound,
        &mut full,
        Check {
            name: "fuel-sticky",
            source: "${{c}}",
            expected: Err(EvalError::CostExceeded),
            reads: reads(&[("a", V)]),
            cost: 11,
        },
    );

    let (env, bound) = fixture(
        vec![TypeDef::String, TypeDef::List(TypeId(0))],
        vec![
            ("a", field(0), Some(InputValue::String("\"".repeat(32_768)))),
            (
                "b",
                field(1),
                Some(InputValue::List(vec![node(
                    "b[0]",
                    InputValue::String("unread".into()),
                )])),
            ),
            ("c", field(0), Some(InputValue::String("later".into()))),
        ],
    );
    for (name_prefix, name_full, source_prefix, source_full, error, cost, owners) in [
        (
            "late-output-prefix",
            "late-output-third-unread",
            "ok${{a}}",
            "ok${{a}}${{c}}",
            EvalError::OutputLimit,
            32_778,
            reads(&[("a", V)]),
        ),
        (
            "late-type-prefix",
            "late-type-third-unread",
            "ok${{b}}",
            "ok${{b}}${{c}}",
            EvalError::TypeError,
            7,
            vec![],
        ),
    ] {
        let mut prefix = EvalSession::new(100_000).unwrap();
        let mut full = EvalSession::new(100_000).unwrap();
        for (name, source, session) in [
            (name_prefix, source_prefix, &mut prefix),
            (name_full, source_full, &mut full),
        ] {
            segment(
                &mut rows,
                &env,
                &bound,
                session,
                Check {
                    name,
                    source,
                    expected: Err(error.clone()),
                    reads: owners.clone(),
                    cost,
                },
            );
        }
        assert_eq!(prefix.stats(), full.stats());
        if error == EvalError::TypeError {
            // Ordinary type failure permits a later valid call: 2 dispatch/lookup,
            // 3 read units, 8 output units for JSON "later" plus its node.
            segment(
                &mut rows,
                &env,
                &bound,
                &mut full,
                Check {
                    name: "late-type-recovery",
                    source: "${{c}}",
                    expected: Ok(json!("later")),
                    reads: reads(&[("c", V)]),
                    cost: 20,
                },
            );
        }
    }
    let (env, bound) = fixture(
        vec![TypeDef::Int],
        vec![
            ("a", field(0), Some(InputValue::Int(7))),
            (
                "b",
                FieldDecl {
                    required: false,
                    ..field(0)
                },
                None,
            ),
            ("c", field(0), Some(InputValue::Int(9))),
        ],
    );
    let mut prefix = EvalSession::new(100_000).unwrap();
    let mut full = EvalSession::new(100_000).unwrap();
    for (name, source, session) in [
        ("late-missing-prefix", "${{a}}${{b}}", &mut prefix),
        ("late-missing-third-unread", "${{a}}${{b}}${{c}}", &mut full),
    ] {
        // 9 through a, then b dispatch/lookup/read (5), diagnostic path copy (1).
        segment(
            &mut rows,
            &env,
            &bound,
            session,
            Check {
                name,
                source,
                expected: Err(EvalError::MissingValue { path: "b".into() }),
                reads: reads(&[("a", V), ("b", A)]),
                cost: 15,
            },
        );
    }
    assert_eq!(prefix.stats(), full.stats());
    segment(
        &mut rows,
        &env,
        &bound,
        &mut full,
        Check {
            name: "late-missing-recovery",
            source: "${{c}}",
            expected: Ok(json!(9)),
            reads: reads(&[("a", V), ("b", A), ("c", V)]),
            cost: 22,
        },
    );

    for (name, length, expected, cost) in [
        (
            "encoded-boundary",
            32_767,
            Ok(json!("\"".repeat(32_767))),
            65_537,
        ),
        // 3 overhead + raw 32768 bytes; escape-expansion capacity fails before charge.
        (
            "encoded-overflow",
            32_768,
            Err(EvalError::OutputLimit),
            32_771,
        ),
    ] {
        fresh(
            &mut rows,
            &empty,
            &empty_bound,
            Check {
                name,
                source: &"\"".repeat(length),
                expected,
                reads: vec![],
                cost,
            },
        );
    }
    let mut session = EvalSession::new(100_000).unwrap();
    for (name, cost) in [
        ("output-reuse-first", 32_769),
        ("output-reuse-boundary", 65_538),
    ] {
        segment(
            &mut rows,
            &empty,
            &empty_bound,
            &mut session,
            Check {
                name,
                source: &"a".repeat(32_766),
                expected: Ok(json!("a".repeat(32_766))),
                reads: vec![],
                cost,
            },
        );
    }
    segment(
        &mut rows,
        &empty,
        &empty_bound,
        &mut session,
        Check {
            name: "output-reuse-overflow",
            source: "",
            expected: Err(EvalError::OutputLimit),
            reads: vec![],
            cost: 65_538,
        },
    );

    let mut session = EvalSession::new(100_000).unwrap();
    segment(
        &mut rows,
        &env,
        &bound,
        &mut session,
        Check {
            name: "cancel-first",
            source: "${{a}}!",
            expected: Ok(json!("7!")),
            reads: reads(&[("a", V)]),
            cost: 10,
        },
    );
    session.cancel();
    for (name, source) in [("cancel-reuse", "${{c}}"), ("cancel-empty", "")] {
        segment(
            &mut rows,
            &env,
            &bound,
            &mut session,
            Check {
                name,
                source,
                expected: Err(EvalError::Cancelled),
                reads: reads(&[("a", V)]),
                cost: 10,
            },
        );
    }
    for (name, source) in [("schema-empty", ""), ("schema-literal", "literal")] {
        segment(
            &mut rows,
            &empty,
            &bound,
            &mut EvalSession::new(0).unwrap(),
            Check {
                name,
                source,
                expected: Err(EvalError::SchemaMismatch),
                reads: vec![],
                cost: 0,
            },
        );
    }
    ingress_rows(&mut rows);
    rows
}

fn ingress_env(types: Vec<TypeDef>, root_ty: u32) -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types,
        roots: BTreeMap::from([("x".into(), field(root_ty))]),
    })
    .unwrap()
}
fn ingress_row(
    rows: &mut Vec<Value>,
    name: &str,
    result: Result<Bindings, ExprDiagnostic>,
    expected_message: Option<&str>,
) {
    let outcome = match (result, expected_message) {
        (Ok(_), None) => json!({"accepted": true}),
        (Err(error), Some(message)) => {
            assert_eq!(
                error.code, "expression.binding_limit",
                "{name} diagnostic code"
            );
            assert_eq!(error.span, 0..0, "{name} diagnostic span");
            assert_eq!(error.message, message, "{name} correct reachable gate");
            json!({"diagnostic": {"code": error.code, "span": {"start": error.span.start, "end": error.span.end}, "message": error.message}})
        }
        (actual, expected) => panic!(
            "{name}: accepted={}, expected acceptance={}",
            actual.is_ok(),
            expected.is_none()
        ),
    };
    rows.push(json!({"name": format!("ingress/{name}"), "outcome": outcome}));
}
fn ingress_rows(rows: &mut Vec<Value>) {
    // Invalid caller input need not match a constructible depth-100000 schema.
    // The shape gate runs before schema matching/copying; the accepted schema
    // remains closed Int. Never recursively clone or drop this caller tree,
    // including if the expected error changes: dismantle BEFORE asserting it.
    let env = ingress_env(vec![TypeDef::Int], 0);
    let mut deep = node("x", InputValue::Int(0));
    for _ in 0..100_000 {
        deep = node("x", InputValue::List(vec![deep]));
    }
    let roots = BTreeMap::from([("x".into(), deep)]);
    let result = Bindings::new(&env, &roots, &origin(""), time());
    let mut pending: Vec<_> = roots.into_values().collect();
    let mut dismantled = 0;
    while let Some(node) = pending.pop() {
        dismantled += 1;
        match node.value {
            InputValue::List(children) => pending.extend(children),
            InputValue::Record(children) => pending.extend(children.into_values()),
            _ => {}
        }
    }
    assert_eq!(dismantled, 100_001);
    ingress_row(
        rows,
        "deep-borrowed-rejection",
        result,
        Some("Bindings exceed depth 32"),
    );

    let env = ingress_env(vec![TypeDef::Int, TypeDef::List(TypeId(0))], 1);
    {
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
        let result = Bindings::new(&env, &roots, &origin(""), time());
        // The caller still owns the untouched tail after rejection.
        let InputValue::List(items) = &roots["x"].value else {
            unreachable!()
        };
        assert_eq!(items.len(), 100_001);
        assert_eq!(items[100_000].value, InputValue::Int(100_000));
        ingress_row(
            rows,
            "list-width-overflow",
            result,
            Some("Bound list exceeds 100,000 elements"),
        );
    }
    let env = ingress_env(vec![TypeDef::String, TypeDef::List(TypeId(0))], 1);
    {
        // 128 * 65536 = aggregate ceiling. Root name/container/list metadata
        // cost 4 bytes; indexed paths plus one-byte execution IDs cost 786.
        // Shorten the last string by 790, keeping every string below its cap.
        let mut roots = BTreeMap::from([(
            "x".into(),
            node(
                "x",
                InputValue::List(
                    (0..128)
                        .map(|i| {
                            node(
                                &format!("x[{i}]"),
                                InputValue::String("s".repeat(if i == 127 {
                                    65_536 - 790
                                } else {
                                    65_536
                                })),
                            )
                        })
                        .collect(),
                ),
            ),
        )]);
        ingress_row(
            rows,
            "aggregate-byte-boundary",
            Bindings::new(&env, &roots, &origin(""), time()),
            None,
        );
        let InputValue::List(items) = &mut roots.get_mut("x").unwrap().value else {
            unreachable!()
        };
        let InputValue::String(last) = &mut items[127].value else {
            unreachable!()
        };
        last.push('s');
        ingress_row(
            rows,
            "aggregate-byte-overflow",
            Bindings::new(&env, &roots, &origin(""), time()),
            Some("Bindings exceed 8,388,608 string and metadata bytes"),
        );
    }
    let env = ingress_env(
        vec![
            TypeDef::Int,
            TypeDef::List(TypeId(0)),
            TypeDef::List(TypeId(1)),
        ],
        2,
    );
    {
        // Finite depth 2, widths 3/87380, and short metadata: the node gate is
        // reachable independently of width/depth/aggregate bytes. 1+3+3*87380
        // is exactly 262144. Drop each accepted binding before the next call.
        let mut roots = BTreeMap::from([(
            "x".into(),
            node(
                "x",
                InputValue::List(
                    (0..3)
                        .map(|i| {
                            node(
                                &format!("x[{i}]"),
                                InputValue::List(
                                    (0..87_380)
                                        .map(|j| node(&format!("x[{i}][{j}]"), InputValue::Int(0)))
                                        .collect(),
                                ),
                            )
                        })
                        .collect(),
                ),
            ),
        )]);
        ingress_row(
            rows,
            "node-boundary",
            Bindings::new(&env, &roots, &origin(""), time()),
            None,
        );
        let InputValue::List(groups) = &mut roots.get_mut("x").unwrap().value else {
            unreachable!()
        };
        let InputValue::List(items) = &mut groups[2].value else {
            unreachable!()
        };
        items.push(node("x[2][87380]", InputValue::Int(0)));
        ingress_row(
            rows,
            "node-overflow",
            Bindings::new(&env, &roots, &origin(""), time()),
            Some("Bindings exceed 262,144 value nodes"),
        );
    }
}
