mod support;
use runbook_expr::{
    Bindings, EventTime, InputValue, IterationIdentity, SchemaDef, TypeDef, TypeEnv,
};
use std::collections::BTreeMap;
use support::*;

fn accepts(env: &TypeEnv, roots: &BTreeMap<String, runbook_expr::InputNode>) -> bool {
    Bindings::new(env, roots, &origin(""), time()).is_ok()
}

#[test]
fn bindings_validate_before_copying() {
    let env = env(
        vec![TypeDef::Int, TypeDef::List(runbook_expr::TypeId(0))],
        1,
    );
    let mut values = roots(InputValue::List(
        (0..100_000)
            .map(|i| node(format!("x[{i}]"), InputValue::Int(i)))
            .collect(),
    ));
    assert!(accepts(&env, &values));
    let InputValue::List(items) = &mut values.get_mut("x").unwrap().value else {
        unreachable!()
    };
    items.push(node("x[100000]", InputValue::Int(0)));
    assert!(!accepts(&env, &values));
}

#[test]
fn scalars_are_exact_and_finite() {
    for value in [i64::MIN, i64::MAX] {
        assert!(accepts(
            &env(vec![TypeDef::Int], 0),
            &roots(InputValue::Int(value))
        ));
    }
    for value in [f64::MIN, f64::MAX, f64::from_bits(1), 0.0] {
        assert!(accepts(
            &env(vec![TypeDef::Double], 0),
            &roots(InputValue::Double(value))
        ));
    }
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!accepts(
            &env(vec![TypeDef::Double], 0),
            &roots(InputValue::Double(value))
        ));
    }
    for value in [
        InputValue::String("1".into()),
        InputValue::Double(1.0),
        InputValue::Null,
        InputValue::Bool(true),
    ] {
        assert!(!accepts(&env(vec![TypeDef::Int], 0), &roots(value)));
    }
}

#[test]
fn timestamps_have_rfc3339_range() {
    let env = env(vec![TypeDef::Timestamp], 0);
    for seconds in [-62_135_596_800, 253_402_300_799] {
        let t = EventTime {
            unix_seconds: seconds,
            nanos: 999_999_999,
        };
        assert!(Bindings::new(&env, &roots(InputValue::Timestamp(t)), &origin(""), t).is_ok());
    }
    for t in [
        EventTime {
            unix_seconds: -62_135_596_801,
            nanos: 0,
        },
        EventTime {
            unix_seconds: 253_402_300_800,
            nanos: 0,
        },
        EventTime {
            unix_seconds: 0,
            nanos: 1_000_000_000,
        },
    ] {
        assert!(
            Bindings::new(&env, &roots(InputValue::Timestamp(t)), &origin(""), time()).is_err()
        );
        assert!(
            Bindings::new(&env, &roots(InputValue::Timestamp(time())), &origin(""), t).is_err()
        );
    }
}

#[test]
fn closed_fields_required_nullable_and_optional() {
    let mut optional = decl(0);
    optional.required = false;
    optional.nullable = true;
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("a".into(), optional.clone())])),
        ],
        roots: BTreeMap::from([("x".into(), optional)]),
    })
    .unwrap();
    assert!(accepts(&env, &BTreeMap::new()));
    assert!(accepts(&env, &roots(InputValue::Null)));
    assert!(!accepts(
        &env,
        &BTreeMap::from([("unknown".into(), node("unknown", InputValue::Int(1)))])
    ));
    let required = support::env(vec![TypeDef::Int], 0);
    assert!(!accepts(&required, &BTreeMap::new()));
    let record = support::env(
        vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("a".into(), decl(0))])),
        ],
        1,
    );
    assert!(!accepts(
        &record,
        &roots(InputValue::Record(BTreeMap::new()))
    ));
    assert!(!accepts(
        &record,
        &roots(InputValue::Record(BTreeMap::from([(
            "b".into(),
            node("x.b", InputValue::Int(1))
        )])))
    ));
}

#[test]
fn metadata_and_canonical_paths_are_validated() {
    let env = env(vec![TypeDef::Int], 0);
    for mutate in [0, 1, 2, 3, 4] {
        let mut data = roots(InputValue::Int(1));
        let record = &mut data.get_mut("x").unwrap().origin;
        match mutate {
            0 => record.path = "other".into(),
            1 => record.path = "x".repeat(1025),
            2 => record.record.execution_id = "e".repeat(129),
            3 => record.record.revision = 0,
            _ => {
                record.record.iteration_path = vec![
                    IterationIdentity {
                        node_id: "n".into(),
                        instance_id: "i".into(),
                        ordinal: 0
                    };
                    33
                ]
            }
        }
        assert!(!accepts(&env, &data));
    }
    for root in [origin("x"), {
        let mut o = origin("");
        o.record.revision = 0;
        o
    }] {
        assert!(Bindings::new(&env, &roots(InputValue::Int(0)), &root, time()).is_err());
    }
    let env = support::env(
        vec![TypeDef::Int, TypeDef::List(runbook_expr::TypeId(0))],
        1,
    );
    assert!(!accepts(
        &env,
        &roots(InputValue::List(vec![node("x[1]", InputValue::Int(0))]))
    ));
}

#[test]
fn string_and_aggregate_limits() {
    let env = env(vec![TypeDef::String], 0);
    assert!(accepts(
        &env,
        &roots(InputValue::String("s".repeat(65_536)))
    ));
    assert!(!accepts(
        &env,
        &roots(InputValue::String("s".repeat(65_537)))
    ));
    let env = support::env(
        vec![TypeDef::String, TypeDef::List(runbook_expr::TypeId(0))],
        1,
    );
    assert!(!accepts(
        &env,
        &roots(InputValue::List(
            (0..128)
                .map(|i| node(format!("x[{i}]"), InputValue::String("s".repeat(65_536))))
                .collect()
        ))
    ));
}

#[test]
fn total_node_limit() {
    let env = env(
        vec![
            TypeDef::Int,
            TypeDef::List(runbook_expr::TypeId(0)),
            TypeDef::List(runbook_expr::TypeId(1)),
        ],
        2,
    );
    let mut data = roots(InputValue::List(
        (0..3)
            .map(|i| {
                node(
                    format!("x[{i}]"),
                    InputValue::List(
                        (0..87_380)
                            .map(|j| node(format!("x[{i}][{j}]"), InputValue::Int(0)))
                            .collect(),
                    ),
                )
            })
            .collect(),
    ));
    assert!(accepts(&env, &data));
    let InputValue::List(groups) = &mut data.get_mut("x").unwrap().value else {
        unreachable!()
    };
    let InputValue::List(items) = &mut groups[2].value else {
        unreachable!()
    };
    items.push(node("x[2][87380]", InputValue::Int(0)));
    assert!(!accepts(&env, &data));
}

#[test]
fn deep_borrowed_tree_rejects_on_small_stack() {
    if std::env::var_os("RUNBOOK_BINDINGS_DEEP_CHILD").is_some() {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(|| {
                let mut value = node("x", InputValue::Int(0));
                for _ in 0..100_000 {
                    value = node("x", InputValue::List(vec![value]));
                }
                let data = BTreeMap::from([("x".into(), value)]);
                assert!(!accepts(&env(vec![TypeDef::Int], 0), &data));
                dismantle(data);
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "deep_borrowed_tree_rejects_on_small_stack",
            "--nocapture",
        ])
        .env("RUNBOOK_BINDINGS_DEEP_CHILD", "1")
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn scalar_iteration_aliases_keep_the_canonical_item_origin() {
    let env = env(vec![TypeDef::Int], 0);
    let mut data = roots(InputValue::Int(3));
    data.get_mut("x").unwrap().origin.path = "inputs.regions[2]".into();
    assert!(accepts(&env, &data));
    for path in [
        "inputs.regions[02]",
        "inputs.regions[-1]",
        "inputs..regions[2]",
        "inputs.regions[2].",
        "inputs.regions",
    ] {
        data.get_mut("x").unwrap().origin.path = path.into();
        assert!(!accepts(&env, &data));
    }
}

#[test]
fn duplicate_canonical_aliases_cannot_claim_different_revisions_or_values() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Int],
        roots: BTreeMap::from([("a".into(), decl(0)), ("b".into(), decl(0))]),
    })
    .unwrap();
    let mut data = BTreeMap::from([
        ("a".into(), node("items[0]", InputValue::Int(1))),
        ("b".into(), node("items[0]", InputValue::Int(1))),
    ]);
    assert!(accepts(&env, &data));
    data.get_mut("b").unwrap().origin.record.revision = 2;
    assert!(!accepts(&env, &data));
    data.get_mut("b").unwrap().origin.record.revision = 1;
    data.get_mut("b").unwrap().value = InputValue::Int(2);
    assert!(!accepts(&env, &data));
}

#[test]
fn canonical_double_aliases_require_identical_bits() {
    for (left, right, accepted) in [
        (-0.0_f64, 0.0_f64, false),
        (0.0, -0.0, false),
        (-0.0, -0.0, true),
        (0.0, 0.0, true),
    ] {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![TypeDef::Double],
            roots: BTreeMap::from([("a".into(), decl(0)), ("b".into(), decl(0))]),
        })
        .unwrap();
        let data = BTreeMap::from([
            ("a".into(), node("items[0]", InputValue::Double(left))),
            ("b".into(), node("items[0]", InputValue::Double(right))),
        ]);
        assert_eq!(
            accepts(&env, &data),
            accepted,
            "{:x} / {:x}",
            left.to_bits(),
            right.to_bits()
        );
    }
}

#[test]
fn exact_aggregate_metadata_boundary() {
    let env = env(
        vec![TypeDef::String, TypeDef::List(runbook_expr::TypeId(0))],
        1,
    );
    // Root name, root-container identity, list path+identity, then item paths+identities.
    let overhead = 1 + 1 + 2 + (0..128).map(|i| format!("x[{i}]").len() + 1).sum::<usize>();
    let mut data = roots(InputValue::List(
        (0..128)
            .map(|i| {
                node(
                    format!("x[{i}]"),
                    InputValue::String("s".repeat(if i == 127 {
                        65_536 - overhead
                    } else {
                        65_536
                    })),
                )
            })
            .collect(),
    ));
    assert!(accepts(&env, &data));
    let InputValue::List(items) = &mut data.get_mut("x").unwrap().value else {
        unreachable!()
    };
    let InputValue::String(value) = &mut items[127].value else {
        unreachable!()
    };
    value.push('s');
    assert!(!accepts(&env, &data));
}

#[test]
fn path_identity_and_iteration_boundaries() {
    let env = env(vec![TypeDef::Int], 0);
    let mut container = origin("");
    container.record.execution_id = "e".repeat(128);
    let mut data = roots(InputValue::Int(0));
    data.get_mut("x").unwrap().origin.record.execution_id = container.record.execution_id.clone();
    let path = format!(
        "{}.{}[0]",
        vec!["a".repeat(128); 7].join("."),
        "b".repeat(118)
    );
    assert_eq!(path.len(), 1024);
    data.get_mut("x").unwrap().origin.path = path;
    assert!(Bindings::new(&env, &data, &container, time()).is_ok());
    data.get_mut("x").unwrap().origin.path.insert(0, 'a');
    assert!(Bindings::new(&env, &data, &container, time()).is_err());
    data.get_mut("x").unwrap().origin.path = "x".into();
    data.get_mut("x").unwrap().origin.record.iteration_path = (0..32)
        .map(|i| IterationIdentity {
            node_id: format!("n{i}"),
            instance_id: format!("i{i}"),
            ordinal: i,
        })
        .collect();
    assert!(Bindings::new(&env, &data, &container, time()).is_ok());
    data.get_mut("x")
        .unwrap()
        .origin
        .record
        .iteration_path
        .push(IterationIdentity {
            node_id: "n32".into(),
            instance_id: "i32".into(),
            ordinal: 32,
        });
    assert!(Bindings::new(&env, &data, &container, time()).is_err());
}

#[test]
fn nested_step_records_preserve_identity_but_allow_leaf_revisions() {
    let env = env(
        vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([
                ("a".into(), decl(0)),
                ("b".into(), decl(0)),
            ])),
        ],
        1,
    );
    let mut a = node("x.a", InputValue::Int(1));
    let mut b = node("x.b", InputValue::Int(2));
    a.origin.record.step_run_id = Some("s".into());
    b.origin.record.step_run_id = Some("s".into());
    b.origin.record.revision = 2;
    let mut data = roots(InputValue::Record(BTreeMap::from([
        ("a".into(), a),
        ("b".into(), b),
    ])));
    data.get_mut("x").unwrap().origin.record.step_run_id = Some("s".into());
    assert!(accepts(&env, &data));
    let InputValue::Record(fields) = &mut data.get_mut("x").unwrap().value else {
        unreachable!()
    };
    fields
        .get_mut("b")
        .unwrap()
        .origin
        .record
        .iteration_path
        .push(IterationIdentity {
            node_id: "n".into(),
            instance_id: "i".into(),
            ordinal: 0,
        });
    assert!(!accepts(&env, &data));
}

#[test]
fn nested_records_can_introduce_step_and_iteration_identity() {
    let env = env(
        vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([("a".into(), decl(0))])),
            TypeDef::Record(BTreeMap::from([("child".into(), decl(1))])),
        ],
        2,
    );
    let mut child = node(
        "x.child",
        InputValue::Record(BTreeMap::from([(
            "a".into(),
            node("x.child.a", InputValue::Int(1)),
        )])),
    );
    let mut identity = child.origin.record.clone();
    identity.step_run_id = Some("s".into());
    identity.iteration_path.push(IterationIdentity {
        node_id: "n".into(),
        instance_id: "i".into(),
        ordinal: 0,
    });
    child.origin.record = identity.clone();
    let InputValue::Record(fields) = &mut child.value else {
        unreachable!()
    };
    fields.get_mut("a").unwrap().origin.record = identity;
    let mut data = roots(InputValue::Record(BTreeMap::from([(
        "child".into(),
        child,
    )])));
    assert!(accepts(&env, &data));
    let InputValue::Record(fields) = &mut data.get_mut("x").unwrap().value else {
        unreachable!()
    };
    fields.get_mut("child").unwrap().origin.record.execution_id = "other".into();
    assert!(!accepts(&env, &data));
}

#[test]
fn depth_33_is_rejected_before_type_conversion() {
    let mut value = node("x", InputValue::Int(0));
    for _ in 0..33 {
        value = node("x", InputValue::List(vec![value]));
    }
    let data = BTreeMap::from([("x".into(), value)]);
    let diagnostic =
        Bindings::new(&env(vec![TypeDef::Int], 0), &data, &origin(""), time()).unwrap_err();
    assert_eq!(diagnostic.code, "expression.binding_limit");
    assert!(diagnostic.message.contains("depth 32"));
    dismantle(data);
}

#[test]
fn one_step_run_id_cannot_claim_different_iterations() {
    let env = env(vec![TypeDef::Int], 0);
    let mut container = origin("");
    container.record.step_run_id = Some("s".into());
    let mut data = roots(InputValue::Int(1));
    let node = data.get_mut("x").unwrap();
    node.origin.record.step_run_id = Some("s".into());
    node.origin.record.iteration_path.push(IterationIdentity {
        node_id: "n".into(),
        instance_id: "i".into(),
        ordinal: 0,
    });
    assert!(Bindings::new(&env, &data, &container, time()).is_err());
}

#[test]
fn wide_rejection_leaves_unvisited_fixture_allocation_owned_by_caller() {
    let env = env(
        vec![TypeDef::Int, TypeDef::List(runbook_expr::TypeId(0))],
        1,
    );
    let mut items: Vec<_> = (0..100_000)
        .map(|i| node(format!("x[{i}]"), InputValue::Int(i)))
        .collect();
    let tail = "this caller-owned tail must survive rejection".to_owned();
    let tail_pointer = tail.as_ptr();
    items.push(node("x[100000]", InputValue::String(tail)));
    let data = roots(InputValue::List(items));
    watch_fixture_allocation(tail_pointer);
    assert!(!accepts(&env, &data));
    assert_eq!(fixture_drops(), 0);
    drop(data);
    assert_eq!(fixture_drops(), 1);
}

#[test]
fn scalar_alias_components_follow_schema_identifier_rules() {
    let env = env(vec![TypeDef::Int], 0);
    let mut data = roots(InputValue::Int(3));
    for path in ["true[0]", "items.null[0]", "items.in[0]", "while[0]"] {
        data.get_mut("x").unwrap().origin.path = path.into();
        assert!(!accepts(&env, &data), "reserved alias component: {path}");
    }
    data.get_mut("x").unwrap().origin.path = "items.true_value[0]".into();
    assert!(accepts(&env, &data));
}
