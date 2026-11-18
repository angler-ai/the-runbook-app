use runbook_expr::{
    FieldDecl, ResultType, SchemaDef, TypeDef, TypeEnv, TypeId, check_expression,
    restore_expression,
};
use std::collections::BTreeMap;

fn field(id: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(id),
        nullable: false,
        required: true,
    }
}
fn schema(rate: TypeDef) -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types: vec![
            rate,
            TypeDef::Record(BTreeMap::from([("rate".into(), field(0))])),
            TypeDef::List(TypeId(0)),
            TypeDef::Record(BTreeMap::new()),
            TypeDef::Record(BTreeMap::from([("outputs".into(), field(3))])),
            TypeDef::Record(BTreeMap::from([("health".into(), field(4))])),
        ],
        roots: BTreeMap::from([
            ("inputs".into(), field(1)),
            ("xs".into(), field(2)),
            ("steps".into(), field(5)),
        ]),
    })
    .unwrap()
}
fn empty() -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types: vec![],
        roots: BTreeMap::new(),
    })
    .unwrap()
}
fn rejects(source: &str, env: &TypeEnv, code: &str) {
    let error = check_expression(source, env).unwrap_err();
    assert_eq!(error.code, code, "{source}: {error:?}");
    assert!(
        error.span.start <= error.span.end && error.span.end <= source.len(),
        "{error:?}"
    );
    assert!(source.is_char_boundary(error.span.start) && source.is_char_boundary(error.span.end));
    assert!(error.message.len() <= 4096);
}

#[test]
fn profile_keeps_real_typechecking() {
    rejects(
        "inputs.rate > 0.01",
        &schema(TypeDef::String),
        "expression.type_error",
    );
    rejects(
        "steps.health.outputs.missing",
        &schema(TypeDef::Double),
        "expression.type_error",
    );
    assert_eq!(
        check_expression("inputs.rate > 0.01", &schema(TypeDef::Double))
            .unwrap()
            .result_type(),
        ResultType::Bool
    );
}

#[test]
fn profile_rejects_unapproved_work() {
    let env = schema(TypeDef::Double);
    for source in [
        "xs == xs",
        "inputs == inputs",
        "[1]",
        "{'a': 1}",
        "'abc'[0]",
        "1 + 1",
        "2 - 1",
        "1 * 2",
        "1 / 2",
        "1 % 2",
        "b'abc'",
        "1u",
        "inputs.?rate",
        "xs[?0]",
        "xs.all(x, true)",
        "xs.filter(x, true)",
        "xs.map(x, x)",
        "has(inputs.rate)",
        "xs.exists(x, true) || xs.exists(x, true)",
        "xs.exists(x, xs.exists(y, true))",
        "xs.exists(i, x, true)",
        "custom()",
        "-inputs.rate",
        "timestamp(inputs.rate)",
        "duration('1s')",
        "r'abc'",
        "'''abc'''",
        "`inputs`.rate",
        ".inputs.rate",
        "1 in xs",
        "true ? 1 : 'bad'",
    ] {
        rejects(source, &env, "expression.unsupported");
    }
    for source in [
        "-1",
        "-1.5",
        "-9223372036854775808",
        "!false",
        "true && false || true",
        "xs.exists(x, x > 0.0)",
        "true ? xs : xs",
        "true ? inputs : inputs",
        "null == null",
        "now()",
        "timestamp('2000-01-01T00:00:00Z')",
    ] {
        assert!(check_expression(source, &env).is_ok(), "{source}");
    }
}

#[test]
fn literal_result_types_do_not_depend_on_schema_arena() {
    let env = empty();
    for (source, ty) in [
        ("null", ResultType::Null),
        ("true", ResultType::Bool),
        ("1", ResultType::Int),
        ("1.5", ResultType::Double),
        ("'hello'", ResultType::String),
    ] {
        assert_eq!(check_expression(source, &env).unwrap().result_type(), ty);
    }
    let env = schema(TypeDef::Double);
    assert_eq!(
        check_expression("inputs", &env).unwrap().result_type(),
        ResultType::Record {
            type_id: TypeId(1),
            schema_fingerprint: env.fingerprint()
        }
    );
}

#[test]
fn descriptor_is_bound_to_schema_and_profile() {
    let env = schema(TypeDef::Double);
    let expression = check_expression("inputs.rate > 0.01", &env).unwrap();
    let descriptor = expression.descriptor();
    assert_eq!(
        restore_expression(&descriptor, &env).unwrap().descriptor(),
        descriptor
    );
    let mut changed = descriptor.clone();
    changed.profile.push('x');
    assert!(restore_expression(&changed, &env).is_err());
    changed = descriptor.clone();
    changed.cost_model.push('x');
    assert!(restore_expression(&changed, &env).is_err());
    changed = descriptor.clone();
    changed.schema_fingerprint[0] ^= 1;
    assert!(restore_expression(&changed, &env).is_err());
    changed = descriptor;
    changed.source = "inputs.rate > 'bad'".into();
    assert_eq!(
        restore_expression(&changed, &env).unwrap_err().code,
        "expression.type_error"
    );
}

#[test]
fn timestamps_are_literal_bounded_and_in_the_utc_calendar_range() {
    let env = empty();
    for source in [
        "timestamp('0001-01-01T00:00:00Z')",
        "timestamp('9999-12-31T23:59:59.999999999Z')",
        "timestamp('2000-01-01T01:00:00+01:00')",
    ] {
        assert_eq!(
            check_expression(source, &env).unwrap().result_type(),
            ResultType::Timestamp
        );
    }
    for source in [
        "timestamp('0000-01-01T00:00:00Z')",
        "timestamp('0001-01-01T00:00:00+01:00')",
        "timestamp('9999-12-31T23:59:59-01:00')",
        "timestamp('2016-12-31T23:59:60Z')",
        "timestamp('bad')",
    ] {
        assert!(check_expression(source, &env).is_err(), "{source}");
    }
    rejects(
        &format!("timestamp('{}')", "x".repeat(65)),
        &env,
        "expression.unsupported",
    );
}

#[test]
fn nullable_scalars_and_records_have_sound_result_types() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::String, TypeDef::Record(BTreeMap::new())],
        roots: BTreeMap::from([
            (
                "verdict".into(),
                FieldDecl {
                    nullable: true,
                    ..field(0)
                },
            ),
            (
                "record".into(),
                FieldDecl {
                    nullable: true,
                    ..field(1)
                },
            ),
            ("other".into(), field(1)),
        ]),
    })
    .unwrap();
    assert_eq!(
        check_expression("verdict", &env).unwrap().result_type(),
        ResultType::Nullable(Box::new(ResultType::String))
    );
    assert_eq!(
        check_expression("verdict == null", &env)
            .unwrap()
            .result_type(),
        ResultType::Bool
    );
    let nullable_record = ResultType::Nullable(Box::new(ResultType::Record {
        type_id: TypeId(1),
        schema_fingerprint: env.fingerprint(),
    }));
    for source in ["record", "true ? record : other"] {
        assert_eq!(
            check_expression(source, &env).unwrap().result_type(),
            nullable_record
        );
    }
    for source in [
        "record == null",
        "record != null",
        "null == record",
        "null != record",
    ] {
        assert_eq!(
            check_expression(source, &env).unwrap().result_type(),
            ResultType::Bool
        );
    }
    rejects("record == other", &env, "expression.unsupported");
}

#[test]
fn literal_null_ternaries_never_claim_a_nonnullable_result() {
    let env = schema(TypeDef::Double);
    let record = ResultType::Record {
        type_id: TypeId(1),
        schema_fingerprint: env.fingerprint(),
    };
    let cases = [("inputs", record), ("now()", ResultType::Timestamp)];
    for (value, ty) in cases {
        for source in [
            format!("true ? null : {value}"),
            format!("false ? {value} : null"),
            format!("true ? (false ? {value} : null) : {value}"),
        ] {
            assert_eq!(
                check_expression(&source, &env).unwrap().result_type(),
                ResultType::Nullable(Box::new(ty.clone())),
                "{source}"
            );
        }
    }
    // These infer Dyn upstream; metadata must not manufacture a checked type.
    for value in ["xs", "1", "'a'", "true", "1.5"] {
        for source in [
            format!("true ? null : {value}"),
            format!("false ? {value} : null"),
        ] {
            rejects(&source, &env, "expression.unsupported");
        }
    }
}

#[test]
fn nullable_string_index_is_still_an_unsupported_operation() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::String],
        roots: BTreeMap::from([(
            "s".into(),
            FieldDecl {
                nullable: true,
                ..field(0)
            },
        )]),
    })
    .unwrap();
    rejects("s[0]", &env, "expression.unsupported");
}

#[test]
fn exists_accumulator_cannot_capture_source_identifiers() {
    let env = schema(TypeDef::Double);
    rejects("xs.exists(i, __result__)", &env, "expression.type_error");
    assert_eq!(
        check_expression("xs.exists(__result__, __result__ > 0.0)", &env)
            .unwrap()
            .result_type(),
        ResultType::Bool
    );
}

#[test]
fn exists_preserves_declared_root_and_lexical_shadowing() {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Double, TypeDef::List(TypeId(0)), TypeDef::Bool],
        roots: BTreeMap::from([("xs".into(), field(1)), ("__result__".into(), field(2))]),
    })
    .unwrap();
    for source in [
        "xs.exists(i, __result__)",
        "xs.exists(__result__, __result__ > 0.0)",
    ] {
        assert_eq!(
            check_expression(source, &env).unwrap().result_type(),
            ResultType::Bool
        );
    }
    rejects("@result", &env, "expression.syntax_error");
    rejects("xs.exists(i, `@result`)", &env, "expression.unsupported");
}
