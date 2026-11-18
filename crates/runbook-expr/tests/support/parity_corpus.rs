//! Shared complete expression corpus: admission, evaluation, rendering and flagship sites.
//! Adoption additionally requires the source audits and independent implementation review.
#[allow(dead_code)]
#[path = "flagship.rs"]
pub mod flagship;
#[path = "flagship_runtime.rs"]
mod flagship_runtime;
#[path = "metadata_parity.rs"]
mod metadata_parity;
#[path = "runtime_parity.rs"]
mod runtime_parity;
#[path = "template_parity.rs"]
mod template_parity;

use runbook_expr::{
    Bindings, EventTime, ExprDiagnostic, FieldDecl, InputNode, InputValue, IterationIdentity,
    Origin, RecordIdentity, ResultType, SchemaDef, TemplateMode, TypeDef, TypeEnv, TypeId,
    check_expression, check_template, restore_expression,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const REQUIRED_NAMES: &[&str] = &[
    "binding/zero-aliases-negative-positive",
    "binding/zero-alias-before-list-negative-positive",
    "binding/zero-alias-after-list-positive-negative",
    "render/whole-list",
    "render/whole-record",
    "render/text-list-error",
    "render/text-record-error",
    "render/scalars",
    "render/utf8-escaped-cost",
    "render/quoted-delimiter",
    "render/escaped-delimiter",
    "render/commented-delimiter",
    "render/nonrecursive",
    "render/bound-null",
    "render/literal-null",
    "render/supplied-time",
    "render/fuel-single-a",
    "render/fuel-single-b",
    "render/fuel-prefix",
    "render/fuel-third-unread",
    "render/fuel-sticky",
    "render/late-output-prefix",
    "render/late-output-third-unread",
    "render/late-type-prefix",
    "render/late-type-third-unread",
    "render/late-type-recovery",
    "render/late-missing-prefix",
    "render/late-missing-third-unread",
    "render/late-missing-recovery",
    "render/encoded-boundary",
    "render/encoded-overflow",
    "render/output-reuse-first",
    "render/output-reuse-boundary",
    "render/output-reuse-overflow",
    "render/cancel-first",
    "render/cancel-reuse",
    "render/cancel-empty",
    "render/schema-empty",
    "render/schema-literal",
    "ingress/deep-borrowed-rejection",
    "ingress/list-width-overflow",
    "ingress/aggregate-byte-boundary",
    "ingress/aggregate-byte-overflow",
    "ingress/node-boundary",
    "ingress/node-overflow",
    "runtime/exists-empty",
    "runtime/exists-first",
    "runtime/exists-middle",
    "runtime/exists-no-match",
    "runtime/scalar-alias-unused",
    "runtime/direct-index",
    "runtime/index-out-of-range",
    "runtime/null-leaf",
    "runtime/missing-root",
    "runtime/nullable-record",
    "runtime/root-amendment-8",
    "runtime/field-amendment-8",
    "runtime/iteration-ordinal",
    "runtime/now",
    "runtime/int-min",
    "runtime/negative-zero",
    "runtime/large-output-early-reject",
    "runtime/escaped-output-over",
    "runtime/output-exact-cap",
    "runtime/cumulative-cap-over",
    "runtime/read-limit",
    "runtime/fuel-or",
    "runtime/fuel-and",
    "runtime/fuel-ternary",
    "runtime/cancel-or",
    "runtime/cancel-and",
    "runtime/cancel-ternary",
    "compile/literal-bool",
    "compile/source-limit",
    "compile/tokens-over",
    "compile/parser-frames",
    "compile/tree-depth-over",
    "compile/tree-nodes-over",
    "compile/macro-nested",
    "compile/type-field",
    "schema/cycle",
    "schema/depth-over",
    "binding/wrong-type",
    "binding/revision-zero",
    "binding/event-nanos",
    "template/utf8-error",
    "template/comment-delimiter",
    "template/escaped-delimiter",
    "template/segments-over",
    "flagship/expression/spec/execution/title/0",
    "flagship/expression/spec/execution/title/1",
    "flagship/expression/spec/execution/concurrency/key/0",
    "flagship/expression/spec/steps/0/parallel/0/instructions/0",
    "flagship/expression/spec/steps/1/foreach/items/0",
    "flagship/expression/spec/steps/1/foreach/steps/0/title/0",
    "flagship/expression/spec/steps/1/foreach/steps/0/with/region/0",
    "flagship/expression/spec/steps/2/if/0",
    "flagship/expression/spec/steps/2/loop/until/0",
    "flagship/expression/spec/steps/3/switch/cases/0/when/0",
    "flagship/expression/spec/steps/3/switch/cases/0/steps/0/approval/prompt/0",
    "flagship/expression/spec/steps/3/switch/cases/0/steps/1/if/0",
    "flagship/expression/spec/steps/3/switch/cases/0/steps/1/title/0",
    "flagship/expression/spec/steps/3/switch/cases/0/steps/2/set/vars/result/0",
    "flagship/expression/spec/outputs/result/0",
];

fn field(id: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(id),
        nullable: false,
        required: true,
    }
}
fn schema(types: Vec<TypeDef>) -> SchemaDef {
    SchemaDef {
        types,
        roots: BTreeMap::new(),
    }
}
fn diagnostic(error: ExprDiagnostic) -> Value {
    assert!(!error.message.is_empty());
    json!({"diagnostic": {"code": error.code,
        "span": {"start": error.span.start, "end": error.span.end},
        "message": error.message}})
}
fn outcome<T>(
    result: Result<T, ExprDiagnostic>,
    expected_error: Option<&str>,
    success: impl FnOnce(T) -> Value,
) -> Value {
    match (result, expected_error) {
        (Ok(value), None) => success(value),
        (Err(error), Some(code)) => {
            assert_eq!(error.code, code, "{error:?}");
            diagnostic(error)
        }
        (Ok(_), Some(code)) => panic!("expected {code}, got success"),
        (Err(error), None) => panic!("unexpected diagnostic: {error:?}"),
    }
}
fn result_type(ty: ResultType) -> Value {
    match ty {
        ResultType::Null => json!("Null"),
        ResultType::Bool => json!("Bool"),
        ResultType::Int => json!("Int"),
        ResultType::Double => json!("Double"),
        ResultType::String => json!("String"),
        ResultType::Timestamp => json!("Timestamp"),
        ResultType::List(inner) => json!({"List": result_type(*inner)}),
        ResultType::Nullable(inner) => json!({"Nullable": result_type(*inner)}),
        ResultType::Record {
            type_id,
            schema_fingerprint,
        } => json!({"Record": {"type_id": type_id.0, "schema_fingerprint": schema_fingerprint}}),
    }
}
fn push(rows: &mut Vec<Value>, name: impl Into<String>, outcome: Value) {
    rows.push(json!({"name": name.into(), "outcome": outcome}));
}
fn expression(rows: &mut Vec<Value>, name: &str, source: &str, env: &TypeEnv, error: Option<&str>) {
    let result = check_expression(source, env);
    if let Err(error) = &result {
        assert!(error.span.start <= error.span.end && error.span.end <= source.len());
        assert!(
            source.is_char_boundary(error.span.start) && source.is_char_boundary(error.span.end)
        );
        assert!(error.message.len() <= 4096);
    }
    let value = outcome(
        result,
        error,
        |checked| json!({"descriptor": checked.descriptor(), "result_type": result_type(checked.result_type())}),
    );
    push(rows, name, value);
}
fn template(rows: &mut Vec<Value>, name: &str, source: &str, env: &TypeEnv, error: Option<&str>) {
    let result = check_template(source, env);
    if let Err(error) = &result {
        assert!(error.span.start <= error.span.end && error.span.end <= source.len());
        assert!(
            source.is_char_boundary(error.span.start) && source.is_char_boundary(error.span.end)
        );
    }
    push(
        rows,
        name,
        outcome(result, error, |checked| {
            json!({"mode": match checked.mode() {
                TemplateMode::WholeValue => "WholeValue", TemplateMode::Text => "Text",
            }})
        }),
    );
}
fn balanced(n: usize) -> String {
    if n == 1 {
        "true".into()
    } else {
        format!("({} && {})", balanced(n / 2), balanced(n - n / 2))
    }
}

/// Stable shared entry point for the native integration test and Wasm example.
pub fn corpus() -> Value {
    let mut rows = Vec::new();
    let mut def = schema(vec![
        TypeDef::Int,
        TypeDef::String,
        TypeDef::List(TypeId(0)),
        TypeDef::Record(BTreeMap::from([("rate".into(), field(1))])),
    ]);
    def.roots = BTreeMap::from([("xs".into(), field(2)), ("inputs".into(), field(3))]);
    let env = TypeEnv::new(&def).unwrap();
    for (name, source) in [
        ("literal-bool", "true"),
        ("literal-null", "null"),
        ("literal-int-min", "-9223372036854775808"),
        ("literal-double", "-1.5"),
        ("literal-utf8", "'é🦀'"),
        ("exists", "xs.exists(x, x > 0)"),
        ("index", "xs[0]"),
        ("list-ternary", "true ? xs : xs"),
        ("record-ternary", "true ? inputs : inputs"),
        ("now", "now()"),
        ("timestamp", "timestamp('2000-01-01T01:00:00+01:00')"),
    ] {
        expression(&mut rows, &format!("compile/{name}"), source, &env, None);
    }
    for (name, source, code) in [
        ("syntax-empty", "", "expression.syntax_error"),
        ("syntax-utf8", "😀", "expression.syntax_error"),
        ("syntax-partial", "true &&", "expression.syntax_error"),
        ("type-field", "inputs.missing", "expression.type_error"),
        (
            "type-overload",
            "inputs.rate > 0.01",
            "expression.type_error",
        ),
        (
            "macro-nested",
            "xs.exists(x, xs.exists(y, true))",
            "expression.unsupported",
        ),
        (
            "macro-two-variable",
            "xs.exists(i, x, true)",
            "expression.unsupported",
        ),
        (
            "macro-repeated",
            "xs.exists(x, true) || xs.exists(x, true)",
            "expression.unsupported",
        ),
        ("macro-map", "xs.map(x, x)", "expression.unsupported"),
        ("macro-has", "has(inputs.rate)", "expression.unsupported"),
        (
            "macro-accumulator-capture",
            "xs.exists(i, __result__)",
            "expression.type_error",
        ),
        ("arithmetic", "1 + 1", "expression.unsupported"),
        ("list-literal", "[1]", "expression.unsupported"),
        ("collection-equality", "xs == xs", "expression.unsupported"),
        ("string-index", "'abc'[0]", "expression.unsupported"),
        ("raw-string", "r'abc'", "expression.unsupported"),
        ("bad-escape", "'bad\\q'", "expression.syntax_error"),
    ] {
        expression(
            &mut rows,
            &format!("compile/{name}"),
            source,
            &env,
            Some(code),
        );
    }
    for (name, source, error) in [
        ("source-limit", " ".repeat(4092) + "true", None),
        (
            "source-over",
            " ".repeat(4093) + "true",
            Some("expression.compile_limit"),
        ),
        (
            "tokens-over",
            "!".repeat(513),
            Some("expression.compile_limit"),
        ),
        (
            "identifier-over",
            "x".repeat(129),
            Some("expression.compile_limit"),
        ),
        (
            "parser-frames",
            "(".repeat(256) + "true" + &")".repeat(256),
            Some("expression.compile_limit"),
        ),
        (
            "parser-ternary",
            "true?true:".repeat(100) + "false",
            Some("expression.compile_limit"),
        ),
        ("tree-depth-limit", "!".repeat(32) + "true", None),
        (
            "tree-depth-over",
            "!".repeat(33) + "true",
            Some("expression.compile_limit"),
        ),
        ("tree-nodes-limit", format!("!{}", balanced(128)), None),
        (
            "tree-nodes-over",
            format!("{} && {}", balanced(64), balanced(65)),
            Some("expression.compile_limit"),
        ),
        (
            "flat-binary",
            vec!["true"; 100].join(" && "),
            Some("expression.compile_limit"),
        ),
        (
            "flat-member",
            "inputs".to_owned() + &".rate".repeat(100),
            Some("expression.compile_limit"),
        ),
    ] {
        expression(&mut rows, &format!("compile/{name}"), &source, &env, error);
    }

    let descriptor = check_expression("xs[0]", &env).unwrap().descriptor();
    for change in ["roundtrip", "profile", "cost", "schema", "source"] {
        let mut changed = descriptor.clone();
        match change {
            "profile" => changed.profile.push('x'),
            "cost" => changed.cost_model.push('x'),
            "schema" => changed.schema_fingerprint[0] ^= 1,
            "source" => changed.source = "inputs.missing".into(),
            _ => {}
        }
        let result = restore_expression(&changed, &env);
        let value = if change == "roundtrip" {
            json!({"descriptor": result.unwrap().descriptor()})
        } else {
            diagnostic(result.unwrap_err())
        };
        push(&mut rows, format!("restore/{change}"), value);
    }
    schema_rows(&mut rows);
    binding_rows(&mut rows);
    rows.extend(metadata_parity::rows());
    signed_zero_alias_rows(&mut rows);
    template_rows(&mut rows, &env);
    flagship_compile_rows(&mut rows);
    runtime_parity::append(&mut rows);
    rows.extend(flagship_runtime::runtime_rows());
    for mut row in template_parity::rows() {
        // Compiler scanner rows already use template/...; rendered outcomes
        // have their own namespace and independently pinned required names.
        if let Some(suffix) = row["name"].as_str().unwrap().strip_prefix("template/") {
            row["name"] = json!(format!("render/{suffix}"));
        }
        rows.push(row);
    }
    let result = Value::Array(rows);
    assert_corpus(&result);
    result
}

fn schema_rows(rows: &mut Vec<Value>) {
    for (name, def, error) in [
        ("empty", schema(vec![]), None),
        (
            "cycle",
            schema(vec![TypeDef::List(TypeId(0))]),
            Some("expression.schema_error"),
        ),
        (
            "invalid-id",
            schema(vec![TypeDef::List(TypeId(1))]),
            Some("expression.schema_error"),
        ),
        ("types-limit", schema(vec![TypeDef::Int; 1024]), None),
        (
            "types-over",
            schema(vec![TypeDef::Int; 1025]),
            Some("expression.schema_limit"),
        ),
    ] {
        push(
            rows,
            format!("schema/{name}"),
            outcome(
                TypeEnv::new(&def),
                error,
                |env| json!({"fingerprint": env.fingerprint()}),
            ),
        );
    }
    for depth in [16, 17] {
        let mut def = schema(vec![TypeDef::Int]);
        for child in 0..depth {
            def.types.push(TypeDef::List(TypeId(child)));
        }
        push(
            rows,
            if depth == 16 {
                "schema/depth-limit"
            } else {
                "schema/depth-over"
            },
            outcome(
                TypeEnv::new(&def),
                (depth == 17).then_some("expression.schema_limit"),
                |env| json!({"fingerprint": env.fingerprint()}),
            ),
        );
    }
    for (name, count, record) in [
        ("roots-limit", 256, false),
        ("roots-over", 257, false),
        ("fields-limit", 256, true),
        ("fields-over", 257, true),
    ] {
        let fields = (0..count).map(|i| (format!("f{i}"), field(0))).collect();
        let mut def = schema(vec![TypeDef::Int]);
        if record {
            def.types.push(TypeDef::Record(fields));
        } else {
            def.roots = fields;
        }
        push(
            rows,
            format!("schema/{name}"),
            outcome(
                TypeEnv::new(&def),
                (count == 257).then_some("expression.schema_limit"),
                |env| json!({"fingerprint": env.fingerprint()}),
            ),
        );
    }
}

fn origin(path: &str) -> Origin {
    Origin {
        path: path.into(),
        record: RecordIdentity {
            execution_id: "exe_parity".into(),
            step_run_id: None,
            iteration_path: vec![],
            revision: 7,
        },
    }
}
// Snapshot equality is bit-exact even though CEL numeric equality is not.
fn signed_zero_alias_rows(rows: &mut Vec<Value>) {
    for layout in ["aliases", "alias-before-list", "alias-after-list"] {
        for (case, left, right, accepted) in [
            ("negative-positive", -0.0_f64, 0.0_f64, false),
            ("positive-negative", 0.0, -0.0, false),
            ("negative-negative", -0.0, -0.0, true),
            ("positive-positive", 0.0, 0.0, true),
        ] {
            let indexed = layout != "aliases";
            let alias = if layout == "alias-after-list" {
                "z"
            } else {
                "a"
            };
            let env = TypeEnv::new(&SchemaDef {
                types: vec![TypeDef::Double, TypeDef::List(TypeId(0))],
                roots: BTreeMap::from([
                    (alias.into(), field(0)),
                    (
                        if indexed { "items" } else { "b" }.into(),
                        field(u32::from(indexed)),
                    ),
                ]),
            })
            .unwrap();
            let scalar = |value| InputNode {
                value: InputValue::Double(value),
                origin: origin("items[0]"),
            };
            let roots = BTreeMap::from([
                (alias.into(), scalar(left)),
                (
                    if indexed { "items" } else { "b" }.into(),
                    if indexed {
                        InputNode {
                            value: InputValue::List(vec![scalar(right)]),
                            origin: origin("items"),
                        }
                    } else {
                        scalar(right)
                    },
                ),
            ]);
            let result = Bindings::new(
                &env,
                &roots,
                &origin(""),
                EventTime {
                    unix_seconds: 0,
                    nanos: 0,
                },
            );
            push(
                rows,
                format!("binding/zero-{layout}-{case}"),
                outcome(
                    result,
                    (!accepted).then_some("expression.binding_error"),
                    |_| json!({"accepted": true}),
                ),
            );
        }
    }
}

fn binding_rows(rows: &mut Vec<Value>) {
    let mut def = schema(vec![TypeDef::Int]);
    def.roots.insert("x".into(), field(0));
    let env = TypeEnv::new(&def).unwrap();
    for name in [
        "valid",
        "wrong-type",
        "required-missing",
        "unknown-root",
        "revision-zero",
        "identity-over",
        "path-over",
        "iteration-over",
        "root-path",
        "event-nanos",
        "event-seconds",
    ] {
        let mut roots = BTreeMap::from([(
            "x".into(),
            InputNode {
                value: InputValue::Int(i64::MAX),
                origin: origin("x"),
            },
        )]);
        let mut root = origin("");
        let mut time = EventTime {
            unix_seconds: 0,
            nanos: 0,
        };
        let node = roots.get_mut("x").unwrap();
        match name {
            "wrong-type" => node.value = InputValue::String("1".into()),
            "revision-zero" => node.origin.record.revision = 0,
            "identity-over" => node.origin.record.execution_id = "e".repeat(129),
            "path-over" => node.origin.path = "x".repeat(1025),
            "iteration-over" => {
                node.origin.record.iteration_path = vec![
                    IterationIdentity {
                        node_id: "n".into(),
                        instance_id: "i".into(),
                        ordinal: 0
                    };
                    33
                ]
            }
            "required-missing" => roots.clear(),
            "unknown-root" => {
                roots.insert(
                    "unknown".into(),
                    InputNode {
                        value: InputValue::Int(1),
                        origin: origin("unknown"),
                    },
                );
            }
            "root-path" => root.path = "x".into(),
            "event-nanos" => time.nanos = 1_000_000_000,
            "event-seconds" => time.unix_seconds = 253_402_300_800,
            _ => {}
        }
        let result = Bindings::new(&env, &roots, &root, time);
        let value = if name == "valid" {
            result.unwrap();
            json!({"accepted": true})
        } else {
            diagnostic(result.unwrap_err())
        };
        push(rows, format!("binding/{name}"), value);
    }
    for (name, ty, value, accepted) in [
        (
            "double-subnormal",
            TypeDef::Double,
            InputValue::Double(f64::from_bits(1)),
            true,
        ),
        (
            "double-nan",
            TypeDef::Double,
            InputValue::Double(f64::NAN),
            false,
        ),
        (
            "double-infinity",
            TypeDef::Double,
            InputValue::Double(f64::INFINITY),
            false,
        ),
        (
            "string-limit",
            TypeDef::String,
            InputValue::String("s".repeat(65_536)),
            true,
        ),
        (
            "string-over",
            TypeDef::String,
            InputValue::String("s".repeat(65_537)),
            false,
        ),
    ] {
        def.types = vec![ty];
        let env = TypeEnv::new(&def).unwrap();
        let roots = BTreeMap::from([(
            "x".into(),
            InputNode {
                value,
                origin: origin("x"),
            },
        )]);
        let result = Bindings::new(
            &env,
            &roots,
            &origin(""),
            EventTime {
                unix_seconds: 0,
                nanos: 0,
            },
        );
        let value = if accepted {
            result.unwrap();
            json!({"accepted": true})
        } else {
            diagnostic(result.unwrap_err())
        };
        push(rows, format!("binding/{name}"), value);
    }
}

fn template_rows(rows: &mut Vec<Value>, env: &TypeEnv) {
    for (name, source, error) in [
        ("whole-list", "${{ xs }}", None),
        ("text-list-check-only", "list: ${{ xs }}", None),
        ("utf8-literal", "é🦀 ${{ 'é}}🦀' }}", None),
        ("comment-delimiter", "${{ true // }} ${{ ' \"\n }}", None),
        ("escaped-delimiter", r#"${{ '\'}}' }}"#, None),
        ("escaped-backslash", r#"${{ '\\' }}"#, None),
        (
            "escaped-unicode",
            r#"${{ '\x7d\u007d\U0000007d\175' }}"#,
            None,
        ),
        ("utf8-error", "é ${{true", Some("expression.syntax_error")),
        (
            "nested-opener",
            "é ${{ true ${{false}} }}",
            Some("expression.syntax_error"),
        ),
        (
            "comment-unclosed",
            "é ${{ true // }}",
            Some("expression.syntax_error"),
        ),
        (
            "type-offset",
            "🦀é ${{ inputs.missing }} suffix",
            Some("expression.type_error"),
        ),
    ] {
        template(rows, &format!("template/{name}"), source, env, error);
    }
    for (name, source, error) in [
        ("segments-limit", "${{true}}".repeat(64), None),
        (
            "segments-over",
            "${{true}}".repeat(65),
            Some("expression.compile_limit"),
        ),
        ("source-limit", "a".repeat(65_536), None),
        (
            "source-over",
            "a".repeat(65_537),
            Some("expression.compile_limit"),
        ),
        (
            "expression-utf8-over",
            "${{".to_owned() + &" ".repeat(4095) + "'é'}}",
            Some("expression.compile_limit"),
        ),
    ] {
        template(rows, &format!("template/{name}"), &source, env, error);
    }
}

/// Compile all 15 expression segments / 14 containing values in actual scopes.
/// Runtime expansion should reuse flagship::scenario_fixture with all scenarios.
pub fn flagship_compile_rows(rows: &mut Vec<Value>) {
    let cases = flagship::flagship_cases();
    assert_eq!(cases.len(), 14);
    assert_eq!(
        cases
            .iter()
            .map(|case| case.expressions.len())
            .sum::<usize>(),
        15
    );
    for case in cases {
        let env = TypeEnv::new(&case.schema).unwrap();
        template(
            rows,
            &format!("flagship/template{}", case.pointer),
            &case.source,
            &env,
            None,
        );
        for (index, source) in case.expressions.iter().enumerate() {
            expression(
                rows,
                &format!("flagship/expression{}/{index}", case.pointer),
                source,
                &env,
                None,
            );
        }
    }
}

pub fn assert_corpus(value: &Value) {
    let rows = value.as_array().expect("corpus array");
    assert!(!rows.is_empty(), "empty parity evidence");
    let names: BTreeSet<_> = rows
        .iter()
        .map(|row| {
            let name = row["name"].as_str().expect("named row");
            assert!(!name.is_empty());
            assert!(row["outcome"].is_object());
            name
        })
        .collect();
    assert_eq!(names.len(), rows.len(), "duplicate corpus row");
    for name in [
        "runtime/exists-first",
        "runtime/direct-index",
        "runtime/null-leaf",
        "runtime/read-limit",
        "runtime/iteration-ordinal",
    ] {
        if let Some(row) = rows.iter().find(|row| row["name"] == name) {
            assert!(
                !row["outcome"]["stats"]["reads"]
                    .as_array()
                    .expect("runtime reads")
                    .is_empty(),
                "empty reads for {name}"
            );
        }
    }
    for name in REQUIRED_NAMES.iter().chain(metadata_parity::ROW_NAMES) {
        assert!(names.contains(name), "missing required row {name}");
    }
    // Every containing value executes under every named scenario. The source
    // inventory is separately pinned to the unchanged PRD fixture by T12a.
    let mut runtime_names = BTreeSet::new();
    for scenario in [
        "healthy",
        "unhealthy",
        "rollback",
        "keep",
        "null-verdict",
        "missing-output",
        "distinct-iterations",
    ] {
        for case in flagship::flagship_cases() {
            runtime_names.insert(format!("flagship/{scenario}{}", case.pointer));
        }
        for companion in [
            "report",
            "direct-index",
            "whole-output-record",
            "distinct-iterations",
            "current-loop",
            "last-loop",
        ] {
            runtime_names.insert(format!("flagship/{scenario}/companion/{companion}"));
        }
    }
    runtime_names.insert("flagship/null-verdict/companion/null-value".into());
    assert_eq!(runtime_names.len(), 141);
    for name in runtime_names {
        let row = rows
            .iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("missing runtime row {name}"));
        assert!(
            !row["outcome"]["stats"]["reads"]
                .as_array()
                .unwrap()
                .is_empty(),
            "empty runtime reads for {name}"
        );
        assert!(row["outcome"]["stats"]["cost"].as_u64().unwrap() > 0);
    }
    assert_eq!(
        rows.len(),
        396,
        "all reviewed corpus slices must be included"
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.starts_with("flagship/expression/"))
            .count(),
        15
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.starts_with("flagship/template/"))
            .count(),
        14
    );
}
