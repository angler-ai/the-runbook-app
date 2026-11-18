use cel_core::{parser::parse_bounded, CompileLimitKind, CompileLimits};

#[test]
fn source_bound_precedes_lexing() {
    let limits = CompileLimits::runbook_v1();
    assert_eq!(
        parse_bounded(&" ".repeat(4097), &limits).unwrap_err().kind,
        CompileLimitKind::SourceBytes
    );
    assert!(parse_bounded(&format!("{}true", " ".repeat(4092)), &limits)
        .unwrap()
        .is_ok());
    assert!(!parse_bounded("(", &limits).unwrap().errors.is_empty());
}

#[test]
fn token_bound_precedes_parser_work() {
    let limits = CompileLimits::runbook_v1();
    assert_eq!(
        parse_bounded(&"!".repeat(513), &limits).unwrap_err().kind,
        CompileLimitKind::Tokens
    );
    assert!(parse_bounded(&"true ".repeat(512), &limits).is_ok());
    assert!(parse_bounded("true // !? ' ", &limits).unwrap().is_ok());
}

#[test]
fn bounded_parser_rejects_before_stack_growth() {
    let limits = CompileLimits::runbook_v1();
    for source in [
        format!("{}true", "!".repeat(300)),
        format!("{}true{}", "(".repeat(100), ")".repeat(100)),
        format!("{}true", "true?true:".repeat(125)),
    ] {
        assert_eq!(
            parse_bounded(&source, &limits).unwrap_err().kind,
            CompileLimitKind::ParseDepth
        );
    }
    assert_eq!(
        parse_bounded(&format!("a{}", ".a".repeat(65)), &limits)
            .unwrap_err()
            .kind,
        CompileLimitKind::TreeDepth
    );
    assert!(parse_bounded(&format!("a{}", ".a".repeat(64)), &limits)
        .unwrap()
        .is_ok());
}

#[test]
fn bounded_parser_small_stack_child() {
    if std::env::var_os("CEL_BOUNDED_STACK_CHILD").is_none() {
        return;
    }
    std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            bounded_parser_rejects_before_stack_growth();
            bounded_parser_limits_allocations();
            bounded_macros_expand_only_one_variable_exists();
            let limits = CompileLimits::runbook_v1();
            assert_eq!(
                parse_bounded(&format!("{}true", "!".repeat(2000)), &limits)
                    .unwrap_err()
                    .kind,
                CompileLimitKind::Tokens
            );
            // Combine stack consumers, including syntax errors and partial-tree drop.
            for nesting in [0, 1, 10, 20, 21, 22, 23, 24, 25] {
                for unary in [0, 1, 32, 64, 128, 230, 255] {
                    for chain in [0, 31, 62, 64, 65] {
                        let body =
                            format!("{}xs.exists(x, a{})", "!".repeat(unary), ".a".repeat(chain));
                        for (open, close) in [
                            ("(", ")"),
                            ("[", "]"),
                            ("{0:", "}"),
                            ("T{a:", "}"),
                            ("f(", ")"),
                            ("a[", "]"),
                        ] {
                            let source = format!(
                                "{}{}{}",
                                open.repeat(nesting),
                                body,
                                close.repeat(nesting)
                            );
                            drop(parse_bounded(&source, &limits));
                            let malformed = format!("{}{}", open.repeat(nesting), body);
                            drop(parse_bounded(&malformed, &limits));
                        }
                    }
                }
            }
            let grouped = format!(
                "{}xs.exists(x, a{}){}",
                "(".repeat(20),
                ".a".repeat(62),
                ")".repeat(20)
            );
            assert!(parse_bounded(&grouped, &limits).unwrap().is_ok());
            for source in [
                format!("{}xs.exists(x, a{})", "!".repeat(230), ".a".repeat(62)),
                format!("{}true{}", "T{a:".repeat(60), "}".repeat(60)),
                format!("{}true{}", "{0:".repeat(80), "}".repeat(80)),
                format!("{}0{}", "a[".repeat(80), "]".repeat(80)),
                format!("{}true{}", "f(".repeat(100), ")".repeat(100)),
                format!("{}0{}", "[".repeat(100), "]".repeat(100)),
            ] {
                assert!(parse_bounded(&source, &limits).is_err());
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn bounded_parser_small_stack_subprocess() {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "bounded_parser_small_stack_child", "--nocapture"])
        .env("CEL_BOUNDED_STACK_CHILD", "1")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "small-stack child terminated abnormally: {status}"
    );
}

#[test]
fn bounded_macros_expand_only_one_variable_exists() {
    let limits = CompileLimits::runbook_v1();
    let parsed = parse_bounded("xs.exists(x, x > 0)", &limits).unwrap();
    // The bounded macro is alpha-equivalent to upstream, with an internal name
    // the Runbook source profile cannot spell. Preserve all other AST details.
    let mut expected = cel_core::parse("xs.exists(x, x > 0)").ast;
    let cel_core::Expr::Comprehension(data) = &mut expected.as_mut().unwrap().node else {
        panic!()
    };
    assert_eq!(data.accu_var, "__result__");
    data.accu_var = "@result".into();
    let cel_core::Expr::Unary {
        expr: condition, ..
    } = &mut data.loop_condition.node
    else {
        panic!()
    };
    let cel_core::Expr::Binary {
        right: accumulated, ..
    } = &mut data.loop_step.node
    else {
        panic!()
    };
    for generated in [
        condition.as_mut(),
        accumulated.as_mut(),
        data.result.as_mut(),
    ] {
        assert!(matches!(&generated.node, cel_core::Expr::Ident(name) if name == "__result__"));
        generated.node = cel_core::Expr::Ident("@result".into());
    }
    assert_eq!(parsed.ast, expected);
    assert!(matches!(
        parsed.ast.unwrap().node,
        cel_core::Expr::Comprehension(_)
    ));
    assert!(parsed.macro_calls.is_empty());
    for source in [
        "xs.exists(x, x > 0) || xs.exists(y, y < 0)",
        "xs.exists(x, ys.exists(y, y > 0))",
    ] {
        assert_eq!(
            parse_bounded(source, &limits).unwrap_err().kind,
            CompileLimitKind::MacroCount
        );
    }
    for source in ["xs.all(x, true)", "xs.exists(i, x, true)", "has(x.a)"] {
        assert!(matches!(
            parse_bounded(source, &limits).unwrap().ast.unwrap().node,
            cel_core::Expr::Call { .. }
        ));
    }
}

#[test]
fn bounded_diagnostics_do_not_format_large_rejected_tokens() {
    let source = format!("a.\"{}\"", "\u{1}".repeat(1500));
    let result = parse_bounded(&source, &CompileLimits::runbook_v1()).unwrap();
    assert!(!result.errors.is_empty());
    assert!(result.errors.iter().map(|e| e.message.len()).sum::<usize>() <= 4096);
}

#[test]
fn bounded_parser_limits_allocations() {
    let limits = CompileLimits::runbook_v1();
    let source = |count| format!("xs.exists(x, f({}))", vec!["0"; count].join(","));
    assert!(parse_bounded(&source(249), &limits).unwrap().is_ok());
    assert!(parse_bounded(&source(250), &limits).unwrap().is_ok());
    // The macro's extra two predicate edges are checked before expansion.
    assert!(
        parse_bounded(&format!("xs.exists(x, a{})", ".a".repeat(62)), &limits)
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        parse_bounded(&format!("xs.exists(x, a{})", ".a".repeat(63)), &limits)
            .unwrap_err()
            .kind,
        CompileLimitKind::TreeDepth
    );
}

#[test]
fn bounded_lexical_errors_preserve_spans_and_upstream_meaning() {
    let limits = CompileLimits::runbook_v1();
    for source in [
        "'unterminated",
        "'\\q'",
        "'\\uD800'",
        "'a\nb'",
        "💣",
        "a.💣",
        "\"\\xGG\"",
        "b'\\q'",
    ] {
        let parsed = parse_bounded(source, &limits).unwrap();
        assert!(!parsed.errors.is_empty(), "{source:?}");
        for error in parsed.errors {
            assert!(source.get(error.span).is_some());
        }
    }
    for source in [
        "'💣'",
        "'\\u263a'",
        "\"// {} ()\"",
        "true // ' unterminated",
        "-9223372036854775808",
        "a[0].b",
        "true ? false : true",
    ] {
        let bounded = parse_bounded(source, &limits).unwrap();
        let ordinary = cel_core::parse(source);
        assert!(bounded.errors.is_empty(), "{source:?}");
        assert_eq!(bounded.ast, ordinary.ast, "{source:?}");
    }
}
#[test]
fn public_bounded_lexer_checks_source_before_tokenization() {
    let source = " ".repeat(4097);
    let error = cel_core::parser::lex_bounded(&source).unwrap_err();
    assert_eq!(error.kind, cel_core::CompileLimitKind::SourceBytes);
    assert!(cel_core::parser::lex_bounded("true").unwrap().is_ok());
}

#[test]
fn bounded_exists_preserves_user_accumulator_named_roots() {
    use cel_core::{Ast, Env, EvaluationBudget, MapActivation, Value};
    let source = "items.exists(i, __result__)";
    let parsed = parse_bounded(source, &CompileLimits::runbook_v1()).unwrap();
    assert!(parsed.errors.is_empty());
    let program = Env::new()
        .program(&Ast::new_unchecked(parsed.ast.unwrap(), source))
        .unwrap();
    let mut activation = MapActivation::new();
    activation.insert("items", Value::List(vec![Value::Int(1)].into()));
    activation.insert("__result__", Value::Bool(true));
    assert_eq!(
        program
            .eval_with_context(&activation, &EvaluationBudget::new(100))
            .result
            .unwrap(),
        Value::Bool(true)
    );
}
