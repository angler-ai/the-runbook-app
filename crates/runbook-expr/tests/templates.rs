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
fn fixture(
    types: Vec<TypeDef>,
    roots: Vec<(&str, FieldDecl, Option<InputValue>)>,
) -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types,
        roots: roots
            .iter()
            .map(|(name, field, _)| ((*name).into(), field.clone()))
            .collect(),
    })
    .unwrap();
    let values = roots
        .into_iter()
        .filter_map(|(name, _, value)| value.map(|value| (name.into(), node(name, value))))
        .collect();
    let bindings = Bindings::new(
        &env,
        &values,
        &origin(""),
        EventTime {
            unix_seconds: 123,
            nanos: 456_000_000,
        },
    )
    .unwrap();
    (env, bindings)
}
fn empty() -> (TypeEnv, Bindings) {
    fixture(vec![], vec![])
}
fn render(
    source: &str,
    env: &TypeEnv,
    bindings: &Bindings,
    session: &mut EvalSession,
) -> Result<EvaluatedValue, EvalError> {
    render_template(&check_template(source, env).unwrap(), bindings, session)
}
fn fresh(source: &str, env: &TypeEnv, bindings: &Bindings) -> EvaluatedValue {
    render(
        source,
        env,
        bindings,
        &mut EvalSession::new(100_000).unwrap(),
    )
    .unwrap()
}
fn reads(paths: &[(&str, ReadKind)]) -> Vec<ReadRef> {
    paths
        .iter()
        .map(|(path, kind)| ReadRef {
            origin: origin(*path),
            kind: *kind,
        })
        .collect()
}

#[test]
fn exact_expression_preserves_types_and_collection_provenance() {
    let regions = InputValue::List(vec![node(
        "inputs.regions[0]",
        InputValue::String("us".into()),
    )]);
    let inputs = InputValue::Record(BTreeMap::from([(
        "regions".into(),
        node("inputs.regions", regions),
    )]));
    let (env, bindings) = fixture(
        vec![
            TypeDef::String,
            TypeDef::List(TypeId(0)),
            TypeDef::Record(BTreeMap::from([("regions".into(), field(1))])),
        ],
        vec![("inputs", field(2), Some(inputs))],
    );
    for (source, expected) in [
        ("${{ inputs.regions }}", json!(["us"])),
        ("${{inputs}}", json!({"regions": ["us"]})),
        ("${{true}}", json!(true)),
        ("${{42}}", json!(42)),
        ("${{1.25}}", json!(1.25)),
        ("${{null}}", json!(null)),
        ("${{'text'}}", json!("text")),
    ] {
        assert_eq!(fresh(source, &env, &bindings).value, expected);
    }
    assert_eq!(
        fresh("${{inputs.regions}}", &env, &bindings).stats.reads,
        reads(&[
            ("inputs.regions", ReadKind::Membership),
            ("inputs.regions[0]", ReadKind::Value)
        ])
    );
    for source in [
        " ${{inputs.regions}}",
        "${{inputs.regions}} ",
        "${{inputs}}\n",
    ] {
        let mut session = EvalSession::new(100_000).unwrap();
        assert_eq!(
            render(source, &env, &bindings, &mut session),
            Err(EvalError::TypeError)
        );
        assert!(session.stats().reads.is_empty());
        assert_eq!(
            render("ok", &env, &bindings, &mut session).unwrap().value,
            json!("ok")
        );
    }
}

#[test]
fn text_scalar_formatting_and_literals_are_exact_and_nonrecursive() {
    let (env, bindings) = empty();
    for (source, expected) in [
        ("", ""),
        (" plain\t\n\\é ", " plain\t\n\\é "),
        (
            "${{null}}/${{true}}/${{false}}/${{-42}}/${{1.25}}/${{-0.0}}",
            "null/true/false/-42/1.25/-0.0",
        ),
        ("t=${{now()}}", "t=1970-01-01T00:02:03.456Z"),
        (
            "${{timestamp('2000-01-01T01:00:00.000000001+01:00')}}!",
            "2000-01-01T00:00:00.000000001Z!",
        ),
        ("[${{'}}'}}]", "[}}]"),
        ("[${{true // }} ignored\n}}]", "[true]"),
        ("[${{'\\n\\t\\\"\\\\é'}}]", "[\n\t\"\\é]"),
        ("[${{'${{not_recursive}}'}}]", "[${{not_recursive}}]"),
    ] {
        assert_eq!(
            fresh(source, &env, &bindings).value,
            json!(expected),
            "{source}"
        );
    }
}

#[test]
fn text_pays_one_node_and_quotes_and_each_encoded_byte_once() {
    let (env, bindings) = empty();
    // JSON string content a + true + quote + newline + é + 7 = 12 encoded bytes;
    // two quotes + one node + two literal-expression dispatches = 17 units.
    let source = "a${{true}}\"\né${{7}}";
    let mut session = EvalSession::new(17).unwrap();
    let result = render(source, &env, &bindings, &mut session).unwrap();
    assert_eq!(result.value, json!("atrue\"\né7"));
    assert_eq!(serde_json::to_vec(&result.value).unwrap().len(), 14);
    assert_eq!(result.stats.cost, 17);
    let mut short = EvalSession::new(16).unwrap();
    assert_eq!(
        render(source, &env, &bindings, &mut short),
        Err(EvalError::CostExceeded)
    );
    let before = short.stats();
    assert_eq!(
        render("", &env, &bindings, &mut short),
        Err(EvalError::CostExceeded)
    );
    assert_eq!(short.stats(), before);
}

#[test]
fn bound_null_record_consumes_value_but_literal_null_has_no_source() {
    let (env, bindings) = fixture(
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
    for source in ["[${{x}}]", "[${{true ? x : null}}]"] {
        let result = fresh(source, &env, &bindings);
        assert_eq!(result.value, json!("[null]"));
        assert_eq!(result.stats.reads, reads(&[("x", ReadKind::Value)]));
    }
    let result = fresh("[${{false ? x : null}}]", &env, &bindings);
    assert_eq!(result.value, json!("[null]"));
    assert!(result.stats.reads.is_empty());
}

#[test]
fn literal_and_empty_templates_check_schema_and_terminal_state_first() {
    let (env, bindings) = empty();
    let (other, other_bindings) = fixture(
        vec![TypeDef::Int],
        vec![("x", field(0), Some(InputValue::Int(1)))],
    );
    for source in ["", "literal", "${{true}}"] {
        let checked = check_template(source, &env).unwrap();
        let mut session = EvalSession::new(100).unwrap();
        assert_eq!(
            render_template(&checked, &other_bindings, &mut session),
            Err(EvalError::SchemaMismatch)
        );
        assert_eq!(session.stats().cost, 0);
        session.cancel();
        assert_eq!(
            render_template(&checked, &bindings, &mut session),
            Err(EvalError::Cancelled)
        );
        assert_eq!(
            render_template(
                &check_template(source, &other).unwrap(),
                &bindings,
                &mut session
            ),
            Err(EvalError::Cancelled)
        );
        assert_eq!(session.stats().cost, 0);
    }
    assert_eq!(fresh("", &env, &bindings).stats.cost, 3);
}

#[test]
fn segment_fuel_is_shared_and_failure_stops_before_later_lookup() {
    let (env, bindings) = fixture(
        vec![TypeDef::Int],
        vec![
            ("a", field(0), Some(InputValue::Int(7))),
            ("b", field(0), Some(InputValue::Int(8))),
            ("c", field(0), Some(InputValue::Int(9))),
        ],
    );
    for source in ["${{a}}!", "${{b}}!"] {
        assert!(render(source, &env, &bindings, &mut EvalSession::new(12).unwrap()).is_ok());
    }
    let mut prefix = EvalSession::new(12).unwrap();
    let mut full = EvalSession::new(12).unwrap();
    assert_eq!(
        render("${{a}}${{b}}", &env, &bindings, &mut prefix),
        Err(EvalError::CostExceeded)
    );
    assert_eq!(
        render("${{a}}${{b}}${{c}}", &env, &bindings, &mut full),
        Err(EvalError::CostExceeded)
    );
    assert_eq!(full.stats(), prefix.stats());
    // Three string-overhead units, a's five evaluation/read units and one
    // output byte, then b's dispatch/lookup: its three-unit read fails.
    assert_eq!(full.stats().reads, reads(&[("a", ReadKind::Value)]));
    assert_eq!(full.stats().cost, 11);
    let before = full.stats();
    assert_eq!(
        render("${{c}}", &env, &bindings, &mut full),
        Err(EvalError::CostExceeded)
    );
    assert_eq!(full.stats(), before);
}

#[test]
fn text_output_limits_charge_escaping_and_share_across_calls() {
    let (env, bindings) = empty();
    for (source, accepted) in [
        ("a".repeat(65_534), true),
        ("a".repeat(65_535), false),
        ("\"".repeat(32_767), true),
        ("\"".repeat(32_768), false),
        ("\0".repeat(10_922), true),
        ("\0".repeat(10_923), false),
        ("é".repeat(32_767), true),
        ("é".repeat(32_768), false),
    ] {
        let mut session = EvalSession::new(100_000).unwrap();
        let result = render(&source, &env, &bindings, &mut session);
        if accepted {
            let result = result.unwrap();
            assert_eq!(
                result.stats.cost,
                serde_json::to_vec(&result.value).unwrap().len() as u64 + 1
            );
        } else {
            assert_eq!(result, Err(EvalError::OutputLimit));
            let before = session.stats();
            assert_eq!(
                render("", &env, &bindings, &mut session),
                Err(EvalError::OutputLimit)
            );
            assert_eq!(session.stats(), before);
        }
    }
    let mut session = EvalSession::new(100_000).unwrap();
    for _ in 0..2 {
        render(&"a".repeat(32_766), &env, &bindings, &mut session).unwrap();
    }
    assert_eq!(session.stats().cost, 65_538);
    assert_eq!(
        render("", &env, &bindings, &mut session),
        Err(EvalError::OutputLimit)
    );
}

#[test]
fn late_output_and_type_errors_preserve_prefix_stats_and_stop_later_work() {
    let (env, bindings) = fixture(
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
    for (prefix, full, error) in [
        ("ok${{a}}", "ok${{a}}${{c}}", EvalError::OutputLimit),
        ("ok${{b}}", "ok${{b}}${{c}}", EvalError::TypeError),
    ] {
        let mut left = EvalSession::new(100_000).unwrap();
        let mut right = EvalSession::new(100_000).unwrap();
        assert_eq!(
            render(prefix, &env, &bindings, &mut left),
            Err(error.clone())
        );
        assert_eq!(
            render(full, &env, &bindings, &mut right),
            Err(error.clone())
        );
        assert_eq!(left.stats(), right.stats());
        assert_eq!(
            right.stats().cost,
            if error == EvalError::OutputLimit {
                32_778
            } else {
                7
            }
        );
        assert!(
            right
                .stats()
                .reads
                .iter()
                .all(|read| read.origin.path == "a")
        );
        if error == EvalError::TypeError {
            assert_eq!(
                render("${{c}}", &env, &bindings, &mut right).unwrap().value,
                json!("later")
            );
        }
    }
}

#[test]
fn missing_segment_stops_later_work_without_poisoning_session() {
    let (env, bindings) = fixture(
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
    let mut session = EvalSession::new(100_000).unwrap();
    assert_eq!(
        render("${{a}}${{b}}${{c}}", &env, &bindings, &mut session),
        Err(EvalError::MissingValue { path: "b".into() })
    );
    assert_eq!(
        session.stats().reads,
        reads(&[("a", ReadKind::Value), ("b", ReadKind::Absence)])
    );
    let before = session.stats().cost;
    assert_eq!(
        render("${{c}}", &env, &bindings, &mut session)
            .unwrap()
            .value,
        json!(9)
    );
    assert!(session.stats().cost > before);
}

#[test]
fn read_limit_is_shared_between_segments_and_later_calls() {
    let (env, bindings) = fixture(
        vec![TypeDef::Int, TypeDef::List(TypeId(0))],
        vec![(
            "x",
            field(1),
            Some(InputValue::List(
                (0..4097)
                    .map(|i| node(format!("x[{i}]"), InputValue::Int(0)))
                    .collect(),
            )),
        )],
    );
    let mut session = EvalSession::new(100_000).unwrap();
    // 4095 distinct leaf reads plus the list Membership reaches 4096.
    for start in (0..4095).step_by(63) {
        let source = (start..(start + 63).min(4095))
            .map(|i| format!("${{{{x[{i}]}}}} "))
            .collect::<String>();
        render(&source, &env, &bindings, &mut session).unwrap();
    }
    assert_eq!(session.stats().reads.len(), 4096);
    assert_eq!(
        render(
            "${{x[0]}}${{x[4095]}}${{x[4096]}}",
            &env,
            &bindings,
            &mut session
        ),
        Err(EvalError::ReadLimit)
    );
    assert_eq!(session.stats().reads.len(), 4096);
    let before = session.stats();
    assert_eq!(
        render("", &env, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    assert_eq!(session.stats(), before);
}

#[test]
fn successful_segments_deduplicate_reads_and_cancelled_reuse_does_no_work() {
    let (env, bindings) = fixture(
        vec![TypeDef::String],
        vec![(
            "x",
            field(0),
            Some(InputValue::String("${{not_code}}".into())),
        )],
    );
    let mut session = EvalSession::new(100_000).unwrap();
    let first = render("${{x}}/${{x}}", &env, &bindings, &mut session).unwrap();
    assert_eq!(first.value, json!("${{not_code}}/${{not_code}}"));
    assert_eq!(first.stats.reads, reads(&[("x", ReadKind::Value)]));
    let second = render("${{x}}", &env, &bindings, &mut session).unwrap();
    assert_eq!(second.value, json!("${{not_code}}"));
    assert_eq!(second.stats.reads, first.stats.reads);
    assert!(second.stats.cost > first.stats.cost);
    session.cancel();
    for source in ["", "literal", "${{x}}/${{x}}", "${{x}}"] {
        assert_eq!(
            render(source, &env, &bindings, &mut session),
            Err(EvalError::Cancelled)
        );
        assert_eq!(session.stats(), second.stats);
    }
}

#[test]
fn empty_text_counts_one_node_and_node_limit_stops_before_lookup() {
    let (env, bindings) = fixture(
        vec![TypeDef::Int],
        vec![("x", field(0), Some(InputValue::Int(1)))],
    );
    let checked = check_template("", &env).unwrap();
    let mut session = EvalSession::new(100_000).unwrap();
    for _ in 0..16_384 {
        assert_eq!(
            render_template(&checked, &bindings, &mut session)
                .unwrap()
                .value,
            json!("")
        );
    }
    assert_eq!(session.stats().cost, 49_152);
    let before = session.stats();
    assert_eq!(
        render("${{x}}!", &env, &bindings, &mut session),
        Err(EvalError::OutputLimit)
    );
    assert_eq!(session.stats(), before);
    assert_eq!(
        render("${{x}}", &env, &bindings, &mut session),
        Err(EvalError::OutputLimit)
    );
    assert_eq!(session.stats(), before);
}
