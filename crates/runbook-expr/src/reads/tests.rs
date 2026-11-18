use crate::{runtime::evaluate_raw, *};
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
fn decl(ty: u32, nullable: bool, required: bool) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        nullable,
        required,
    }
}
fn node(path: &str, value: InputValue) -> InputNode {
    InputNode {
        origin: origin(path),
        value,
    }
}
fn read(path: &str, kind: ReadKind) -> ReadRef {
    ReadRef {
        origin: origin(path),
        kind,
    }
}
fn fixture() -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("code".into(), decl(0, false, false))])),
            TypeDef::Record(BTreeMap::from([
                ("outputs".into(), decl(1, true, false)),
                ("verdict".into(), decl(0, true, false)),
            ])),
            TypeDef::Record(BTreeMap::from([("health".into(), decl(2, false, true))])),
            TypeDef::List(TypeId(0)),
        ],
        roots: BTreeMap::from([
            ("steps".into(), decl(3, false, true)),
            ("xs".into(), decl(4, false, true)),
            ("ys".into(), decl(4, false, true)),
            ("empty".into(), decl(4, false, true)),
            ("missing".into(), decl(0, false, false)),
            ("record".into(), decl(1, true, false)),
            ("__result__".into(), decl(0, false, true)),
        ]),
    })
    .unwrap();
    let bindings = Bindings::new(
        &env,
        &BTreeMap::from([
            (
                "steps".into(),
                node(
                    "steps",
                    InputValue::Record(BTreeMap::from([(
                        "health".into(),
                        node(
                            "steps.health",
                            InputValue::Record(BTreeMap::from([
                                (
                                    "outputs".into(),
                                    node(
                                        "steps.health.outputs",
                                        InputValue::Record(BTreeMap::from([(
                                            "code".into(),
                                            node("steps.health.outputs.code", InputValue::Int(200)),
                                        )])),
                                    ),
                                ),
                                (
                                    "verdict".into(),
                                    node("steps.health.verdict", InputValue::Null),
                                ),
                            ])),
                        ),
                    )])),
                ),
            ),
            (
                "xs".into(),
                node(
                    "xs",
                    InputValue::List(vec![
                        node("xs[0]", InputValue::Int(1)),
                        node("xs[1]", InputValue::Int(2)),
                        node("xs[2]", InputValue::Int(3)),
                    ]),
                ),
            ),
            (
                "ys".into(),
                node(
                    "ys",
                    InputValue::List(vec![node("ys[0]", InputValue::Int(1))]),
                ),
            ),
            ("empty".into(), node("empty", InputValue::List(vec![]))),
            ("record".into(), node("record", InputValue::Null)),
            ("__result__".into(), node("__result__", InputValue::Int(9))),
        ]),
        &origin(""),
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        },
    )
    .unwrap();
    (env, bindings)
}
fn run(
    source: &str,
    env: &TypeEnv,
    bindings: &Bindings,
    session: &mut EvalSession,
) -> Result<cel_core::Value, EvalError> {
    evaluate_raw(&check_expression(source, env).unwrap(), bindings, session)
}
#[test]
fn reads_are_actual_and_iteration_specific() {
    let (env, bindings) = fixture();
    let mut session = EvalSession::new(10000).unwrap();
    assert_eq!(
        run("steps.health.outputs.code", &env, &bindings, &mut session),
        Ok(cel_core::Value::Int(200))
    );
    assert_eq!(
        session.stats().reads,
        vec![read("steps.health.outputs.code", ReadKind::Value)]
    );
    run("true || missing == 1", &env, &bindings, &mut session).unwrap();
    run("steps.health.outputs.code", &env, &bindings, &mut session).unwrap();
    assert_eq!(
        session.stats().reads,
        vec![read("steps.health.outputs.code", ReadKind::Value)]
    );
}
#[test]
fn reads_index_and_exists_exact_vectors() {
    let (env, bindings) = fixture();
    for (source, expected) in [
        (
            "xs[1]",
            vec![
                read("xs", ReadKind::Membership),
                read("xs[1]", ReadKind::Value),
            ],
        ),
        (
            "xs.exists(x, x == 2)",
            vec![
                read("xs", ReadKind::Membership),
                read("xs[0]", ReadKind::Value),
                read("xs[1]", ReadKind::Value),
            ],
        ),
        ("xs.exists(x, true)", vec![read("xs", ReadKind::Membership)]),
        (
            "empty.exists(x, true)",
            vec![read("empty", ReadKind::Membership)],
        ),
        (
            "xs[0] == ys[0]",
            vec![
                read("xs", ReadKind::Membership),
                read("xs[0]", ReadKind::Value),
                read("ys", ReadKind::Membership),
                read("ys[0]", ReadKind::Value),
            ],
        ),
        (
            "__result__ == 9 && xs.exists(__result__, __result__ == 1)",
            vec![
                read("__result__", ReadKind::Value),
                read("xs", ReadKind::Membership),
                read("xs[0]", ReadKind::Value),
            ],
        ),
    ] {
        let mut session = EvalSession::new(10000).unwrap();
        run(source, &env, &bindings, &mut session).unwrap();
        assert_eq!(session.stats().reads, expected, "{source}");
    }
    let mut session = EvalSession::new(10000).unwrap();
    assert_eq!(
        run("xs[99]", &env, &bindings, &mut session),
        Err(EvalError::IndexOutOfBounds)
    );
    assert_eq!(
        session.stats().reads,
        vec![read("xs", ReadKind::Membership)]
    );
}
#[test]
fn reads_missing_and_null_keep_their_owners() {
    let (env, bindings) = fixture();
    for (source, expected_path) in [
        ("missing", "missing"),
        ("record.code", "record.code"),
        ("(true ? record : null).code", "record.code"),
    ] {
        let mut session = EvalSession::new(10000).unwrap();
        assert_eq!(
            run(source, &env, &bindings, &mut session),
            Err(EvalError::MissingValue {
                path: expected_path.into()
            })
        );
        assert_eq!(
            session.stats().reads,
            vec![read(expected_path, ReadKind::Absence)]
        );
        assert_eq!(
            run("1", &env, &bindings, &mut session),
            Ok(cel_core::Value::Int(1))
        );
    }
    let mut session = EvalSession::new(10000).unwrap();
    assert_eq!(
        run(
            "(false ? record : null).code",
            &env,
            &bindings,
            &mut session
        ),
        Err(EvalError::TypeError)
    );
    assert_eq!(session.stats().reads, vec![]);
    assert_eq!(
        run(
            "steps.health.verdict == null",
            &env,
            &bindings,
            &mut session
        ),
        Ok(cel_core::Value::Bool(true))
    );
    assert_eq!(
        session.stats().reads,
        vec![read("steps.health.verdict", ReadKind::Value)]
    );
}

#[test]
fn reads_empty_root_snapshot_absence_and_revision_history() {
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
    let mut session = EvalSession::new(10000).unwrap();
    let mut owner = origin("");
    owner.record.step_run_id = Some("root-container".into());
    for revision in [7, 8] {
        owner.record.revision = revision;
        let bindings = Bindings::new(
            &env,
            &BTreeMap::new(),
            &owner,
            EventTime {
                unix_seconds: 0,
                nanos: 0,
            },
        )
        .unwrap();
        for (source, path) in [("x", "x"), ("r.code", "r")] {
            assert_eq!(
                run(source, &env, &bindings, &mut session),
                Err(EvalError::MissingValue { path: path.into() })
            );
        }
        let expected: Vec<_> = (7..=revision)
            .flat_map(|rev| {
                ["x", "r"].map(|path| {
                    let mut r = read(path, ReadKind::Absence);
                    r.origin.record.step_run_id = Some("root-container".into());
                    r.origin.record.revision = rev;
                    r
                })
            })
            .collect();
        assert_eq!(session.stats().reads, expected);
    }
}

#[test]
fn reads_step_run_full_iteration_and_amended_revision_do_not_collapse() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Int],
        roots: BTreeMap::from([("x".into(), decl(0, false, true))]),
    })
    .unwrap();
    let mut session = EvalSession::new(10000).unwrap();
    let mut expected = Vec::new();
    let mut old_trace = None;
    for (step, ordinal, revision) in [("a", 0, 7), ("b", 1, 7), ("b", 1, 8)] {
        let mut item = node("x", InputValue::Int(1));
        item.origin.record.step_run_id = Some(step.into());
        item.origin.record.iteration_path = vec![IterationIdentity {
            node_id: "loop".into(),
            instance_id: "instance".into(),
            ordinal,
        }];
        item.origin.record.revision = revision;
        let mut want = read("x", ReadKind::Value);
        want.origin.record.step_run_id = Some(step.into());
        want.origin.record.iteration_path = vec![IterationIdentity {
            node_id: "loop".into(),
            instance_id: "instance".into(),
            ordinal,
        }];
        want.origin.record.revision = revision;
        expected.push(want);
        let bindings = Bindings::new(
            &env,
            &BTreeMap::from([("x".into(), item)]),
            &origin(""),
            EventTime {
                unix_seconds: 0,
                nanos: 0,
            },
        )
        .unwrap();
        run("x == x", &env, &bindings, &mut session).unwrap();
        assert_eq!(session.stats().reads, expected);
        if old_trace.is_none() {
            old_trace = Some(session.stats());
        }
    }
    assert_eq!(old_trace.unwrap().reads, expected[..1]);
}

#[test]
fn reads_limits_and_precharges_are_sticky_without_appending() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Int, TypeDef::List(TypeId(0))],
        roots: BTreeMap::from([("x".into(), decl(1, false, true))]),
    })
    .unwrap();
    let values = (0..5000)
        .map(|i| node(&format!("x[{i}]"), InputValue::Int(i)))
        .collect();
    let bindings = Bindings::new(
        &env,
        &BTreeMap::from([("x".into(), node("x", InputValue::List(values)))]),
        &origin(""),
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        },
    )
    .unwrap();
    let mut session = EvalSession::new(100000).unwrap();
    assert_eq!(
        run("x.exists(i, i < 0) || true", &env, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    let mut expected = vec![read("x", ReadKind::Membership)];
    expected.extend((0..4095).map(|i| read(&format!("x[{i}]"), ReadKind::Value)));
    assert_eq!(session.stats().reads, expected);
    let before = session.stats();
    assert_eq!(
        run("true", &env, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    assert_eq!(session.stats(), before);
    let (env, bindings) = fixture();
    let mut session = EvalSession::new(2).unwrap(); // root dispatch+lookup succeed; metadata precharge fails atomically.
    assert_eq!(
        run("__result__", &env, &bindings, &mut session),
        Err(EvalError::CostExceeded)
    );
    assert_eq!(
        session.stats(),
        EvalStats {
            cost: 2,
            loop_entries: 0,
            reads: vec![]
        }
    );
}

#[test]
fn reads_scopes_ternaries_and_reused_calls_clear_origins() {
    let (env, bindings) = fixture();
    let mut session = EvalSession::new(10000).unwrap();
    assert_eq!(
        run(
            "xs.exists(i, (i == 1 ? record : null).code == 0)",
            &env,
            &bindings,
            &mut session
        ),
        Err(EvalError::MissingValue {
            path: "record.code".into()
        })
    );
    assert_eq!(
        session.stats().reads,
        vec![
            read("xs", ReadKind::Membership),
            read("xs[0]", ReadKind::Value),
            read("record.code", ReadKind::Absence),
            read("xs[1]", ReadKind::Value),
            read("xs[2]", ReadKind::Value)
        ]
    );
    let mut next = EvalSession::new(10000).unwrap();
    run("(true ? record : null).code", &env, &bindings, &mut next).unwrap_err();
    assert_eq!(
        run("(false ? record : null).code", &env, &bindings, &mut next),
        Err(EvalError::TypeError)
    );
    assert_eq!(
        next.stats().reads,
        vec![read("record.code", ReadKind::Absence)]
    );
    let mut next = EvalSession::new(10000).unwrap();
    assert_eq!(
        run(
            "xs.exists(i, (i == 2 ? null : record).code == 0)",
            &env,
            &bindings,
            &mut next
        ),
        Err(EvalError::MissingValue {
            path: "record.code".into()
        })
    );
    assert_eq!(
        next.stats().reads,
        vec![
            read("xs", ReadKind::Membership),
            read("xs[0]", ReadKind::Value),
            read("record.code", ReadKind::Absence),
            read("xs[1]", ReadKind::Value),
            read("xs[2]", ReadKind::Value)
        ]
    );
}

#[test]
fn reads_missing_diagnostic_copy_is_precharged() {
    let (env, bindings) = fixture();
    // root dispatch+lookup=2, first Absence=1+7(path)+1(identity)=9;
    // the final MissingValue path copy requires a further seven units.
    let mut session = EvalSession::new(11).unwrap();
    assert_eq!(
        run("missing", &env, &bindings, &mut session),
        Err(EvalError::CostExceeded)
    );
    assert_eq!(
        session.stats(),
        EvalStats {
            cost: 11,
            loop_entries: 0,
            reads: vec![read("missing", ReadKind::Absence)]
        }
    );
}

#[test]
fn reads_virtual_absence_paths_obey_exact_trace_boundary() {
    let root = "r".repeat(128);
    let names: Vec<_> = (0..6)
        .map(|i| "a".repeat(if i == 5 { 122 } else { 128 }))
        .collect();
    let edge = "b".repeat(127);
    let over = "b".repeat(128);
    let mut types = vec![
        TypeDef::Int,
        TypeDef::Record(BTreeMap::from([
            (edge.clone(), decl(0, false, false)),
            (over.clone(), decl(0, false, false)),
        ])),
    ];
    let mut path = root.clone();
    let mut paths = vec![path.clone()];
    for name in &names {
        path.push('.');
        path.push_str(name);
        paths.push(path.clone());
    }
    assert_eq!(path.len(), 896);
    let mut value = node(&path, InputValue::Record(BTreeMap::new()));
    let mut ty = 1;
    for i in (0..6).rev() {
        types.push(TypeDef::Record(BTreeMap::from([(
            names[i].clone(),
            decl(ty, false, true),
        )])));
        ty += 1;
        value = node(
            &paths[i],
            InputValue::Record(BTreeMap::from([(names[i].clone(), value)])),
        );
    }
    let env = TypeEnv::new(&SchemaDef {
        types,
        roots: BTreeMap::from([(root.clone(), decl(ty, false, true))]),
    })
    .unwrap();
    let bindings = Bindings::new(
        &env,
        &BTreeMap::from([(root, value)]),
        &origin(""),
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        },
    )
    .unwrap();
    let boundary = format!("{path}.{edge}");
    let mut session = EvalSession::new(10000).unwrap();
    assert_eq!(
        run(&boundary, &env, &bindings, &mut session),
        Err(EvalError::MissingValue {
            path: boundary.clone()
        })
    );
    assert_eq!(
        session.stats().reads,
        vec![read(&boundary, ReadKind::Absence)]
    );
    let mut session = EvalSession::new(10000).unwrap();
    assert_eq!(
        run(&format!("{path}.{over}"), &env, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    assert_eq!(
        session.stats(),
        EvalStats {
            cost: 16,
            loop_entries: 0,
            reads: vec![]
        }
    );
    assert_eq!(
        run("1", &env, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
}

#[test]
fn reads_output_source_retains_nullable_record_without_consuming_it() {
    let (env, bindings) = fixture();
    let mut session = EvalSession::new(10000).unwrap();
    let expr = check_expression("true ? record : null", &env).unwrap();
    let (value, source) =
        crate::runtime::evaluate_raw_with_source(&expr, &bindings, &mut session).unwrap();
    assert_eq!(value, cel_core::Value::Null);
    assert_eq!(source.unwrap().origin.as_ref(), &origin("record"));
    assert_eq!(session.stats().reads, vec![]);
    let expr = check_expression("false ? record : null", &env).unwrap();
    let (value, source) =
        crate::runtime::evaluate_raw_with_source(&expr, &bindings, &mut session).unwrap();
    assert_eq!(value, cel_core::Value::Null);
    assert!(source.is_none());
    assert_eq!(session.stats().reads, vec![]);
}

#[test]
fn reads_record_items_and_derived_empty_ranges_use_canonical_owners() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("code".into(), decl(0, false, false))])),
            TypeDef::List(TypeId(0)),
            TypeDef::Record(BTreeMap::from([
                ("r".into(), decl(1, true, true)),
                ("xs".into(), decl(2, false, true)),
            ])),
            TypeDef::List(TypeId(3)),
        ],
        roots: BTreeMap::from([
            ("rows".into(), decl(4, false, true)),
            ("other".into(), decl(2, false, true)),
        ]),
    })
    .unwrap();
    let rows = (0..2)
        .map(|i| {
            let path = format!("rows[{i}]");
            node(
                &path,
                InputValue::Record(BTreeMap::from([
                    (
                        "r".into(),
                        node(
                            &format!("{path}.r"),
                            if i == 0 {
                                InputValue::Null
                            } else {
                                InputValue::Record(BTreeMap::new())
                            },
                        ),
                    ),
                    (
                        "xs".into(),
                        node(&format!("{path}.xs"), InputValue::List(vec![])),
                    ),
                ])),
            )
        })
        .collect();
    let bindings = Bindings::new(
        &env,
        &BTreeMap::from([
            ("rows".into(), node("rows", InputValue::List(rows))),
            ("other".into(), node("other", InputValue::List(vec![]))),
        ]),
        &origin(""),
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        },
    )
    .unwrap();
    for (source, expected, missing) in [
        (
            "rows[0].r.code",
            vec![
                read("rows", ReadKind::Membership),
                read("rows[0].r.code", ReadKind::Absence),
            ],
            Some("rows[0].r.code"),
        ),
        (
            "rows[1].r.code",
            vec![
                read("rows", ReadKind::Membership),
                read("rows[1].r.code", ReadKind::Absence),
            ],
            Some("rows[1].r.code"),
        ),
        (
            "rows.exists(item, item.r.code == 1)",
            vec![
                read("rows", ReadKind::Membership),
                read("rows[0].r.code", ReadKind::Absence),
                read("rows[1].r.code", ReadKind::Absence),
            ],
            Some("rows[0].r.code"),
        ),
        (
            "rows[0].xs.exists(i, true)",
            vec![
                read("rows", ReadKind::Membership),
                read("rows[0].xs", ReadKind::Membership),
            ],
            None,
        ),
        (
            "(true ? rows[1].xs : other).exists(i, true)",
            vec![
                read("rows", ReadKind::Membership),
                read("rows[1].xs", ReadKind::Membership),
            ],
            None,
        ),
        (
            "(false ? rows[1].xs : other).exists(i, true)",
            vec![read("other", ReadKind::Membership)],
            None,
        ),
    ] {
        let mut session = EvalSession::new(10000).unwrap();
        let result = run(source, &env, &bindings, &mut session);
        if let Some(path) = missing {
            assert_eq!(
                result,
                Err(EvalError::MissingValue { path: path.into() }),
                "{source}"
            );
        } else {
            assert_eq!(result, Ok(cel_core::Value::Bool(false)), "{source}");
        }
        assert_eq!(session.stats().reads, expected, "{source}");
    }
}

#[test]
fn reads_cleanup_drops_child_slots_after_all_fuel_cutoffs() {
    use super::*;
    let (env, bindings) = fixture();
    let expr = check_expression("xs.exists(i, i < 0)", &env).unwrap();
    // Calls through the real prepared runtime prove partial traces survive each
    // cutoff; direct observer inspection verifies only bounded ephemeral state.
    for fuel in 0..100 {
        let mut session = EvalSession::new(fuel).unwrap();
        let result = evaluate_raw(&expr, &bindings, &mut session);
        assert!(session.stats().cost <= fuel);
        if result == Err(EvalError::CostExceeded) {
            let before = session.stats();
            assert_eq!(
                evaluate_raw(&expr, &bindings, &mut session),
                Err(EvalError::CostExceeded)
            );
            assert_eq!(session.stats(), before);
        }
        let budget = EvaluationBudget::new(fuel);
        let mut tracker = ReadTracker::default();
        let observer = ReadObserver::new(&bindings, &budget, &mut tracker);
        let list = &bindings.roots["xs"].cel;
        observer.bind_item(1, "i", list, 0).unwrap();
        if fuel > 2 {
            let _ = observer.resolve(1, 22, "i", &cel_core::Value::Int(1));
        }
        budget.cancel();
        let before = budget.used();
        observer.leave_scope(1);
        assert_eq!(budget.used(), before);
        let state = observer.state.borrow();
        assert!(state.alias.is_none());
        assert!(state.slots.iter().all(Option::is_none));
    }
}

#[test]
fn reads_record_presence_comparisons_consume_only_compared_records() {
    let (env, bindings) = fixture();
    for (source, expected_value, path) in [
        ("record == null", true, "record"),
        ("null != record", false, "record"),
        (
            "steps.health.outputs == null",
            false,
            "steps.health.outputs",
        ),
        ("null != steps.health.outputs", true, "steps.health.outputs"),
        ("(true ? record : null) == null", true, "record"),
    ] {
        let mut session = EvalSession::new(10000).unwrap();
        assert_eq!(
            run(source, &env, &bindings, &mut session),
            Ok(cel_core::Value::Bool(expected_value))
        );
        assert_eq!(
            session.stats().reads,
            vec![read(path, ReadKind::Value)],
            "{source}"
        );
    }
    let mut session = EvalSession::new(10000).unwrap();
    assert_eq!(
        run(
            "(false ? record : null) == null",
            &env,
            &bindings,
            &mut session
        ),
        Ok(cel_core::Value::Bool(true))
    );
    assert_eq!(session.stats().reads, vec![]);
}
