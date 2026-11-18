//! Independent expected metadata, executed identically by native and actual Wasm.
use runbook_expr::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const ROW_NAMES: &[&str] = &[
    "metadata/literal",
    "metadata/whole",
    "metadata/utf8-spans",
    "metadata/chain-dedup",
    "metadata/index-root",
    "metadata/exists-shadow",
    "metadata/conditional-range",
    "metadata/all-branches",
    "metadata/callees",
    "metadata/unknown",
    "metadata/type-error",
    "metadata/source-boundary",
    "metadata/source-over",
    "metadata/depth-boundary",
    "metadata/depth-over",
    "metadata/parser-frames",
    "metadata/segments-boundary",
    "metadata/segments-over",
    "metadata/template-over",
    "metadata/nested-macro",
];

fn field(id: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(id),
        required: true,
        nullable: false,
    }
}
fn env() -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::List(TypeId(0)),
            TypeDef::Record(BTreeMap::from([("value".into(), field(0))])),
            TypeDef::Record(BTreeMap::from([
                ("a".into(), field(2)),
                ("items".into(), field(1)),
                ("position".into(), field(0)),
            ])),
            TypeDef::Record(BTreeMap::from([
                ("items".into(), field(1)),
                ("limit".into(), field(0)),
            ])),
        ],
        roots: BTreeMap::from([
            ("vars".into(), field(3)),
            ("inputs".into(), field(4)),
            ("items".into(), field(1)),
            ("__result__".into(), field(0)),
        ]),
    })
    .unwrap()
}
fn deps(pairs: &[(&str, Option<&str>)]) -> Vec<StaticDependency> {
    pairs
        .iter()
        .map(|(root, member)| StaticDependency {
            root: (*root).into(),
            member: member.map(str::to_owned),
        })
        .collect()
}
fn encode(summary: &TemplateSyntaxSummary) -> Value {
    json!({
        "mode": format!("{:?}", summary.mode),
        "expressions": summary.expressions.iter().map(|s| json!({
            "span": [s.source_span.start, s.source_span.end],
            "dependencies": s.dependencies.iter().map(|d| json!({"root": d.root, "member":d.member})).collect::<Vec<_>>()
        })).collect::<Vec<_>>()
    })
}

pub fn rows() -> Vec<Value> {
    let mut rows = Vec::new();
    let env = env();
    for (name, source, interiors, expected, mode, checks) in [
        (
            "literal",
            "é literal",
            vec![],
            vec![],
            TemplateMode::Text,
            true,
        ),
        (
            "whole",
            "${{ vars.a.value }}",
            vec![" vars.a.value "],
            vec![deps(&[("vars", Some("a"))])],
            TemplateMode::WholeValue,
            true,
        ),
        (
            "utf8-spans",
            "é ${{ vars.position }} / ${{ '}}' }}",
            vec![" vars.position ", " '}}' "],
            vec![deps(&[("vars", Some("position"))]), vec![]],
            TemplateMode::Text,
            true,
        ),
        (
            "chain-dedup",
            "${{ vars.a.value == vars.a.value }}",
            vec![" vars.a.value == vars.a.value "],
            vec![deps(&[("vars", Some("a"))])],
            TemplateMode::WholeValue,
            true,
        ),
        (
            "index-root",
            "${{ items[vars.position] }}",
            vec![" items[vars.position] "],
            vec![deps(&[("items", None), ("vars", Some("position"))])],
            TemplateMode::WholeValue,
            true,
        ),
        (
            "exists-shadow",
            "${{ inputs.items.exists(vars, vars > __result__) && vars.position > 0 }}",
            vec![" inputs.items.exists(vars, vars > __result__) && vars.position > 0 "],
            vec![deps(&[
                ("inputs", Some("items")),
                ("__result__", None),
                ("vars", Some("position")),
            ])],
            TemplateMode::WholeValue,
            true,
        ),
        (
            "conditional-range",
            "${{ (true ? inputs.items : vars.items).exists(inputs, inputs > vars.position) }}",
            vec![" (true ? inputs.items : vars.items).exists(inputs, inputs > vars.position) "],
            vec![deps(&[
                ("inputs", Some("items")),
                ("vars", Some("items")),
                ("vars", Some("position")),
            ])],
            TemplateMode::WholeValue,
            true,
        ),
        (
            "all-branches",
            "${{ true ? vars.a.value : inputs.limit }}",
            vec![" true ? vars.a.value : inputs.limit "],
            vec![deps(&[("vars", Some("a")), ("inputs", Some("limit"))])],
            TemplateMode::WholeValue,
            true,
        ),
        (
            "callees",
            "${{ now() > timestamp('2026-01-01T00:00:00Z') }}",
            vec![" now() > timestamp('2026-01-01T00:00:00Z') "],
            vec![vec![]],
            TemplateMode::WholeValue,
            true,
        ),
        (
            "unknown",
            "${{ unknown.value }}",
            vec![" unknown.value "],
            vec![deps(&[("unknown", Some("value"))])],
            TemplateMode::WholeValue,
            false,
        ),
        (
            "type-error",
            "${{ vars.position > 'wrong' }}",
            vec![" vars.position > 'wrong' "],
            vec![deps(&[("vars", Some("position"))])],
            TemplateMode::WholeValue,
            false,
        ),
    ] {
        let summary = summarize_template_syntax(source).unwrap();
        assert_eq!(summary.mode, mode, "{name}");
        assert_eq!(summary.expressions.len(), interiors.len(), "{name}");
        let mut cursor = 0;
        for ((actual, interior), expected) in
            summary.expressions.iter().zip(interiors).zip(&expected)
        {
            let start = cursor + source[cursor..].find(interior).unwrap();
            let end = start + interior.len();
            assert_eq!(actual.source_span, start..end, "{name}");
            assert_eq!(&actual.dependencies, expected, "{name}");
            cursor = end;
        }
        if checks {
            let checked = check_template(source, &env).unwrap();
            let expressions: Vec<_> = checked.expressions().collect();
            assert_eq!(expressions.len(), summary.expressions.len());
            for (actual, expected) in expressions.iter().zip(&summary.expressions) {
                assert_eq!(actual.source_span, expected.source_span);
                assert_eq!(
                    actual.expression.static_dependencies(),
                    expected.dependencies
                );
                assert_eq!(
                    actual.expression.descriptor().source,
                    source[expected.source_span.clone()]
                );
                assert_eq!(
                    restore_expression(&actual.expression.descriptor(), &env)
                        .unwrap()
                        .static_dependencies(),
                    expected.dependencies
                );
            }
        } else {
            assert_eq!(
                check_template(source, &env).unwrap_err().code,
                "expression.type_error"
            );
        }
        rows.push(json!({"name":format!("metadata/{name}"), "outcome": encode(&summary)}));
    }
    for (name, source, expected_error, count) in [
        (
            "source-boundary",
            format!("${{{{{}true}}}}", " ".repeat(4092)),
            None,
            1,
        ),
        (
            "source-over",
            format!("${{{{{}true}}}}", " ".repeat(4093)),
            Some("expression.compile_limit"),
            0,
        ),
        (
            "depth-boundary",
            format!("${{{{{}true}}}}", "!".repeat(32)),
            None,
            1,
        ),
        (
            "depth-over",
            format!("${{{{{}true}}}}", "!".repeat(33)),
            Some("expression.compile_limit"),
            0,
        ),
        (
            "parser-frames",
            format!("${{{{{}true{}}}}}", "(".repeat(256), ")".repeat(256)),
            Some("expression.compile_limit"),
            0,
        ),
        ("segments-boundary", "${{true}}".repeat(64), None, 64),
        (
            "segments-over",
            "${{true}}".repeat(65),
            Some("expression.compile_limit"),
            0,
        ),
        (
            "template-over",
            "x".repeat(65_537),
            Some("expression.compile_limit"),
            0,
        ),
        (
            "nested-macro",
            "${{ xs.exists(x, xs.exists(y, true)) }}".into(),
            Some("expression.unsupported"),
            0,
        ),
    ] {
        let result = summarize_template_syntax(&source);
        let outcome = match expected_error {
            Some(code) => {
                let error = result.unwrap_err();
                assert_eq!(error.code, code, "{name}");
                assert_eq!(error, check_template(&source, &env).unwrap_err(), "{name}");
                json!({"diagnostic":{"code":error.code,"span":[error.span.start,error.span.end],"message":error.message}})
            }
            None => {
                let summary = result.unwrap();
                assert_eq!(summary.expressions.len(), count);
                assert!(
                    summary
                        .expressions
                        .iter()
                        .all(|expr| expr.dependencies.is_empty())
                );
                encode(&summary)
            }
        };
        rows.push(json!({"name":format!("metadata/{name}"), "outcome":outcome}));
    }
    assert_eq!(rows.len(), ROW_NAMES.len());
    for (row, name) in rows.iter().zip(ROW_NAMES) {
        assert_eq!(row["name"], *name);
    }
    rows
}
