use runbook_expr::{FieldDecl, SchemaDef, TemplateMode, TypeDef, TypeEnv, TypeId, check_template};
use std::collections::BTreeMap;

fn env() -> TypeEnv {
    let field = |id| FieldDecl {
        ty: TypeId(id),
        nullable: false,
        required: true,
    };
    TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::String,
            TypeDef::List(TypeId(0)),
            TypeDef::Record(BTreeMap::from([("regions".into(), field(1))])),
        ],
        roots: BTreeMap::from([("inputs".into(), field(2))]),
    })
    .unwrap()
}

#[test]
fn only_an_exact_single_expression_is_whole_value() {
    let env = env();
    for source in ["${{ inputs.regions }}", "${{\ttrue\n}}"] {
        assert_eq!(
            check_template(source, &env).unwrap().mode(),
            TemplateMode::WholeValue,
            "{source:?}"
        );
    }
    for source in [
        "",
        "literal",
        "}}",
        "${",
        " ${{ inputs.regions }}",
        "${{ inputs.regions }}\n",
        "prefix ${{ inputs.regions }} suffix",
        "${{true}}${{false}}",
    ] {
        assert_eq!(
            check_template(source, &env).unwrap().mode(),
            TemplateMode::Text,
            "{source:?}"
        );
    }
}

#[test]
fn quotes_escapes_and_line_comments_hide_delimiters() {
    for source in [
        "${{ '}}' }}",
        "${{ \"}}\" }}",
        "${{ '${{ true }}' }}",
        r#"${{ '\'}}' }}"#,
        r#"${{ "\"}}" }}"#,
        r#"${{ '\\' }}"#,
        r#"${{ '\x7d\u007d\U0000007d\175' }}"#,
        "${{ true // }} ${{ ' \"\n }}",
        "${{ true // }}\r ${{\n }}",
        "${{ '//'}}",
        "${{ 'é}}🦀' }}",
    ] {
        assert_eq!(
            check_template(source, &env()).unwrap().mode(),
            TemplateMode::WholeValue,
            "{source:?}"
        );
    }
}

#[test]
fn malformed_boundaries_have_exact_utf8_byte_spans() {
    for (source, span) in [
        ("é ${{true", 3..10),
        ("é ${{ true // }}", 3..17),
        ("é ${{ '}}", 3..10),
        ("é ${{ true ${{false}} }}", 12..15),
        ("${{}}", 3..3),
    ] {
        let error = check_template(source, &env()).unwrap_err();
        assert_eq!(
            error.code, "expression.syntax_error",
            "{source:?}: {error:?}"
        );
        assert_eq!(error.span, span, "{source:?}");
        assert!(source.is_char_boundary(error.span.start));
        assert!(source.is_char_boundary(error.span.end));
    }
}

#[test]
fn compiler_diagnostics_are_translated_without_trimming() {
    let env = env();
    // Compiler spans are the expression's UTF-8 offsets, plus the literal and opener.
    for expression in [
        "  inputs.missing  ",
        " true && ",
        " 'é' > 1 ",
        " 'bad\\q' ",
        " [1] ",
        "  ",
    ] {
        let source = format!("🦀é ${{{{{expression}}}}} suffix");
        let original = runbook_expr::check_expression(expression, &env).unwrap_err();
        let translated = check_template(&source, &env).unwrap_err();
        assert_eq!(translated.code, original.code);
        assert_eq!(translated.message, original.message);
        assert_eq!(
            translated.span,
            (original.span.start + 10)..(original.span.end + 10)
        );
        assert!(source.is_char_boundary(translated.span.start));
        assert!(source.is_char_boundary(translated.span.end));
    }
}

#[test]
fn literal_source_byte_limit_is_inclusive() {
    assert_eq!(
        check_template(&"a".repeat(65_536), &env()).unwrap().mode(),
        TemplateMode::Text
    );
    for source in ["a".repeat(65_537), "é".repeat(32_769)] {
        let error = check_template(&source, &env()).unwrap_err();
        assert_eq!(error.code, "expression.compile_limit");
        assert_eq!(error.span, 0..source.len());
    }
}

#[test]
fn expression_count_limit_is_inclusive() {
    let source = "x${{true}}".repeat(64) + "x";
    assert_eq!(
        check_template(&source, &env()).unwrap().mode(),
        TemplateMode::Text
    );
    let error = check_template(&"${{true}}".repeat(65), &env()).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(error.span, 576..579);
}

#[test]
fn expression_byte_limit_counts_untrimmed_source() {
    let expression = " ".repeat(4092) + "true";
    assert_eq!(
        check_template(&format!("${{{{{expression}}}}}"), &env())
            .unwrap()
            .mode(),
        TemplateMode::WholeValue
    );
    let error = check_template(&format!("é${{{{ {expression}}}}}"), &env()).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(error.span, 5..4102);
}

#[test]
fn checked_compiler_rejects_unsupported_forms_and_wrong_declared_types() {
    for expression in ["[1]", "1 + 2", "r'abc'", "'''abc'''", "b'abc'"] {
        let error = check_template(&format!("${{{{{expression}}}}}"), &env()).unwrap_err();
        assert_eq!(
            error.code, "expression.unsupported",
            "{expression}: {error:?}"
        );
    }
    let error = check_template("${{ inputs.regions > 1 }}", &env()).unwrap_err();
    assert_eq!(error.code, "expression.type_error");
}

#[test]
fn expression_limit_diagnostic_never_splits_utf8() {
    let source = "${{".to_owned() + &" ".repeat(4095) + "'é'}}";
    let error = check_template(&source, &env()).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(error.span, 3..4101);
    assert!(source.is_char_boundary(error.span.start));
    assert!(source.is_char_boundary(error.span.end));
}
