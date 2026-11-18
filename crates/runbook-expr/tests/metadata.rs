use runbook_expr::*;
use std::collections::BTreeMap;

fn empty_env() -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types: vec![],
        roots: BTreeMap::new(),
    })
    .unwrap()
}

#[test]
fn checked_template_expressions_preserve_source_order_and_utf8_spans() {
    let env = empty_env();
    for (source, interiors, mode) in [
        ("${{ true }}", vec![" true "], TemplateMode::WholeValue),
        (
            "é ${{ '}}' }} / ${{ 'é' // }}\n }}",
            vec![" '}}' ", " 'é' // }}\n "],
            TemplateMode::Text,
        ),
        (
            "${{ '\\' }}' }}",
            vec![" '\\' }}' "],
            TemplateMode::WholeValue,
        ),
        ("é literal", vec![], TemplateMode::Text),
    ] {
        let checked = check_template(source, &env).unwrap();
        assert_eq!(checked.mode(), mode);
        let expressions: Vec<_> = checked.expressions().collect();
        assert_eq!(expressions.len(), interiors.len());
        let mut prior_end = 0;
        for (entry, interior) in expressions.iter().zip(interiors) {
            assert!(entry.source_span.start >= prior_end);
            assert_eq!(&source[entry.source_span.clone()], interior);
            assert_eq!(entry.expression.descriptor().source, interior);
            assert_eq!(
                &source[entry.source_span.start - 3..entry.source_span.start],
                "${{"
            );
            assert_eq!(
                &source[entry.source_span.end..entry.source_span.end + 2],
                "}}"
            );
            prior_end = entry.source_span.end;
        }
    }
}

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
            TypeDef::Record(BTreeMap::from([("value".into(), field(0))])),
            TypeDef::List(TypeId(0)),
            TypeDef::Record(BTreeMap::from([
                ("a".into(), field(1)),
                ("items".into(), field(2)),
                ("position".into(), field(0)),
            ])),
            TypeDef::Record(BTreeMap::from([("limit".into(), field(0))])),
        ],
        roots: BTreeMap::from([("vars".into(), field(3)), ("inputs".into(), field(4))]),
    })
    .unwrap()
}
fn deps(values: &[(&str, Option<&str>)]) -> Vec<StaticDependency> {
    values
        .iter()
        .map(|(root, member)| StaticDependency {
            root: (*root).into(),
            member: member.map(str::to_owned),
        })
        .collect()
}

#[test]
fn static_dependencies_are_conservative_root_members() {
    for (source, expected) in [
        ("vars.a.value", deps(&[("vars", Some("a"))])),
        (
            "vars.items[vars.position]",
            deps(&[("vars", Some("items")), ("vars", Some("position"))]),
        ),
        ("vars", deps(&[("vars", None)])),
        (
            "true ? vars.a.value : inputs.limit",
            deps(&[("vars", Some("a")), ("inputs", Some("limit"))]),
        ),
        (
            "(true ? vars.a : vars.a).value",
            deps(&[("vars", Some("a"))]),
        ),
        ("vars.a.value == vars.a.value", deps(&[("vars", Some("a"))])),
        ("'vars.ghost' // inputs.ghost", vec![]),
        ("now()", vec![]),
        ("timestamp('2026-01-01T00:00:00Z')", vec![]),
    ] {
        let expression = check_expression(source, &env()).unwrap();
        assert_eq!(expression.static_dependencies(), expected, "{source}");
        assert_eq!(
            restore_expression(&expression.descriptor(), &env())
                .unwrap()
                .static_dependencies(),
            expected
        );
    }
}

#[test]
fn exists_alias_dependencies_resolve_to_range_without_root_leakage() {
    for (source, expected) in [
        (
            "vars.items.exists(vars, vars > inputs.limit)",
            deps(&[("vars", Some("items")), ("inputs", Some("limit"))]),
        ),
        (
            "(true ? vars.items : vars.items).exists(inputs, inputs > vars.position)",
            deps(&[("vars", Some("items")), ("vars", Some("position"))]),
        ),
        (
            "vars.items.exists(vars, vars > 2) && vars.position > 0",
            deps(&[("vars", Some("items")), ("vars", Some("position"))]),
        ),
    ] {
        let expression = check_expression(source, &env()).unwrap();
        assert_eq!(expression.static_dependencies(), expected, "{source}");
    }
}

#[test]
fn syntax_summary_is_not_typecheck_or_execution() {
    for (source, expected) in [
        ("${{ unknown.value }}", deps(&[("unknown", Some("value"))])),
        (
            "${{ vars.a.value > 'wrong type' }}",
            deps(&[("vars", Some("a"))]),
        ),
        (
            "${{ vars.items.exists(vars, vars > inputs.limit) }}",
            deps(&[("vars", Some("items")), ("inputs", Some("limit"))]),
        ),
    ] {
        let summary = summarize_template_syntax(source).unwrap();
        assert_eq!(summary.mode, TemplateMode::WholeValue);
        assert_eq!(summary.expressions.len(), 1);
        assert_eq!(summary.expressions[0].source_span, 3..source.len() - 2);
        assert_eq!(summary.expressions[0].dependencies, expected);
        if !source.contains("exists") {
            assert!(check_template(source, &env()).is_err());
        }
    }
    let source = "é ${{ '}}' }} / ${{ vars.position // }}\n }}";
    let summary = summarize_template_syntax(source).unwrap();
    assert_eq!(summary.mode, TemplateMode::Text);
    assert_eq!(summary.expressions.len(), 2);
    assert!(summary.expressions[0].dependencies.is_empty());
    assert_eq!(
        summary.expressions[1].dependencies,
        deps(&[("vars", Some("position"))])
    );
    assert!(
        summarize_template_syntax("literal")
            .unwrap()
            .expressions
            .is_empty()
    );
}

fn balanced(leaves: usize) -> String {
    if leaves == 1 {
        "true".into()
    } else {
        format!(
            "({} && {})",
            balanced(leaves / 2),
            balanced(leaves - leaves / 2)
        )
    }
}

#[test]
fn syntax_summary_preserves_adopted_admission_bounds() {
    for (interior, code) in [
        (" ".repeat(4093) + "true", "expression.compile_limit"),
        ("!".repeat(513), "expression.compile_limit"),
        (
            "(".repeat(256) + "true" + &")".repeat(256),
            "expression.compile_limit",
        ),
        ("!".repeat(33) + "true", "expression.compile_limit"),
        (
            "vars.items.exists(a, vars.items.exists(b, true))".into(),
            "expression.unsupported",
        ),
        ("1 + 1".into(), "expression.unsupported"),
        ("unknown.".into(), "expression.syntax_error"),
    ] {
        let source = format!("${{{{{interior}}}}}");
        let actual = summarize_template_syntax(&source).unwrap_err();
        assert_eq!(actual.code, code);
        let checked = check_template(&source, &empty_env()).unwrap_err();
        assert_eq!(actual, checked);
    }
    for source in ["a".repeat(65_537), "${{true}}".repeat(65)] {
        assert_eq!(
            summarize_template_syntax(&source).unwrap_err(),
            check_template(&source, &empty_env()).unwrap_err()
        );
    }
    for source in [
        "a".repeat(65_536),
        "${{true}}".repeat(64),
        format!("${{{{{}true}}}}", " ".repeat(4092)),
        format!("${{{{{}true}}}}", "!".repeat(32)),
    ] {
        assert!(summarize_template_syntax(&source).is_ok());
        assert!(check_template(&source, &empty_env()).is_ok());
    }
    let at_nodes = format!("${{{{!{}}}}}", balanced(128));
    assert!(summarize_template_syntax(&at_nodes).is_ok());
    let over_nodes = format!("${{{{{} && {}}}}}", balanced(64), balanced(65));
    let error = summarize_template_syntax(&over_nodes).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(
        error,
        check_template(&over_nodes, &empty_env()).unwrap_err()
    );
    // Earlier checked errors must retain priority over later scanner errors.
    let error = check_template("${{ unknown }} ${{", &empty_env()).unwrap_err();
    assert_eq!(error.code, "expression.type_error");
    assert_eq!(error.span, 4..11);
}

#[path = "support/metadata_parity.rs"]
mod metadata_parity;

#[test]
fn metadata_parity_rows_have_independent_expected_values() {
    assert_eq!(metadata_parity::rows().len(), 20);
}
