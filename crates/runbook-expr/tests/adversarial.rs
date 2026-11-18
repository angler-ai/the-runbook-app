//! Adversarial literals, operators, escapes and template delimiters: each is
//! rejected with its specific diagnostic or evaluates exactly, never panics.
use runbook_expr::{
    Bindings, EvalSession, EventTime, Origin, RecordIdentity, ResultType, SchemaDef, TypeEnv,
    check_expression, check_template, evaluate, render_template,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn empty() -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![],
        roots: BTreeMap::new(),
    })
    .unwrap();
    let origin = Origin {
        path: String::new(),
        record: RecordIdentity {
            execution_id: "e".into(),
            step_run_id: None,
            iteration_path: vec![],
            revision: 1,
        },
    };
    let bindings = Bindings::new(
        &env,
        &BTreeMap::new(),
        &origin,
        EventTime {
            unix_seconds: 0,
            nanos: 0,
        },
    )
    .unwrap();
    (env, bindings)
}

fn value(source: &str) -> Value {
    let (env, bindings) = empty();
    let expression = check_expression(source, &env).unwrap_or_else(|e| panic!("{source}: {e:?}"));
    evaluate(&expression, &bindings, 100_000).unwrap().value
}

fn rejects(source: &str, code: &str, span: std::ops::Range<usize>) {
    let (env, _) = empty();
    let error = check_expression(source, &env).unwrap_err();
    assert_eq!((error.code, error.span), (code, span), "{source:?}");
}

#[test]
fn integer_literals_are_exact_at_i64_bounds_and_rejected_beyond() {
    assert_eq!(value("9223372036854775807"), json!(i64::MAX));
    assert_eq!(value("-9223372036854775808"), json!(i64::MIN));
    assert_eq!(value("0x7fffffffffffffff"), json!(i64::MAX));
    assert_eq!(value("-0x7fffffffffffffff"), json!(-i64::MAX));
    assert_eq!(
        value("-9223372036854775808 < 9223372036854775807"),
        json!(true)
    );
    // i64::MAX + 1 and i64::MIN - 1 do not wrap or saturate.
    rejects("9223372036854775808", "expression.syntax_error", 0..19);
    rejects("-9223372036854775809", "expression.syntax_error", 1..20);
    rejects("0x8000000000000000", "expression.syntax_error", 0..18);
    rejects("-0x8000000000000000", "expression.syntax_error", 1..19);
    rejects(&"9".repeat(4000), "expression.syntax_error", 0..4000);
    // Negation applies only to a literal, never to an operand.
    rejects("--1", "expression.unsupported", 0..3);
    rejects("-(-1)", "expression.unsupported", 0..4);
    rejects("-true", "expression.unsupported", 0..5);
    // Unsigned literals are outside the profile, even when they fit.
    rejects("18446744073709551615u", "expression.unsupported", 0..21);
    rejects("18446744073709551616u", "expression.syntax_error", 0..21);
}

#[test]
fn arithmetic_overflow_and_division_by_zero_are_unreachable() {
    // Every arithmetic operator is a rejected token, so neither overflow
    // nor division by zero can reach evaluation.
    for (source, at) in [
        ("9223372036854775807 + 1", 20),
        ("9223372036854775807 * 2", 20),
        ("1 / 0", 2),
        ("1 % 0", 2),
        ("-9223372036854775808 / -1", 21),
        ("-9223372036854775808 % -1", 21),
        ("1.0 / 0.0", 4),
    ] {
        rejects(source, "expression.unsupported", at..at + 1);
    }
    // `-` after an operand is binary subtraction, rejected as a node.
    rejects("-9223372036854775808 - 1", "expression.unsupported", 0..24);
    rejects("1 - 1", "expression.unsupported", 0..5);
}

// The parser folds the i64::MIN literal, so negating it reaches the typed
// negate_int64 overload. It must never wrap, panic or return a value.
#[test]
fn negating_the_minimum_integer_never_produces_a_value() {
    let (env, bindings) = empty();
    for source in [
        "-(-9223372036854775808)",
        "--9223372036854775808",
        "-(-9223372036854775808) == 0",
    ] {
        let Ok(expression) = check_expression(source, &env) else {
            continue;
        };
        let result = evaluate(&expression, &bindings, 100_000);
        assert!(result.is_err(), "{source}: {result:?}");
    }
}

#[test]
fn float_literals_must_be_finite() {
    assert_eq!(value("1.7976931348623157e308"), json!(f64::MAX));
    assert_eq!(value("-1.7976931348623157e308"), json!(f64::MIN));
    assert_eq!(value("5e-324"), json!(5e-324));
    assert_eq!(value("1e-400"), json!(0.0));
    rejects("1e309", "expression.unsupported", 0..5);
    rejects("-1e309", "expression.unsupported", 1..6);
    rejects(
        &format!("1{}.0", "0".repeat(400)),
        "expression.unsupported",
        0..403,
    );
}

#[test]
fn string_escapes_decode_exactly_or_fail_as_syntax() {
    for (source, expected) in [
        (r"'\U0010FFFF'", "\u{10FFFF}"),
        (r"'é'", "é"),
        (r"'\x41'", "A"),
        (r"'\101'", "A"),
        (r"'\a\b\f\n\r\t\v'", "\u{7}\u{8}\u{c}\n\r\t\u{b}"),
        (r#"'\'\"\\\`\?'"#, "'\"\\`?"),
        ("'\u{0}'", "\u{0}"),
        ("'🦀é'", "🦀é"),
    ] {
        assert_eq!(value(source), json!(expected), "{source}");
    }
    for source in [
        r"'\xZZ'",
        r"'\x4'",
        r"'\ud800'",
        r"'\udfff'",
        r"'\U00110000'",
        r"'\UFFFFFFFF'",
        r"'\777'",
        r"'\8'",
        r"'\q'",
        r"'\'",
        "'unterminated",
        "\"unterminated",
        "'line\nbreak'",
    ] {
        rejects(source, "expression.syntax_error", 0..1);
    }
    // Byte, raw and triple-quoted literals are outside the profile.
    rejects(r"b'\xff'", "expression.unsupported", 0..7);
    rejects(r"r'\q'", "expression.unsupported", 0..5);
    rejects("'''a'''", "expression.unsupported", 0..7);
}

#[test]
fn non_ascii_outside_literals_is_a_syntax_error_on_a_char_boundary() {
    let (env, _) = empty();
    for source in [
        "é",
        "true && é",
        "\u{feff}true",
        "true\u{a0}",
        "tru\u{301}e",
        "😀 == 😀",
        "\u{202e}true",
    ] {
        let error = check_expression(source, &env).unwrap_err();
        assert_eq!(error.code, "expression.syntax_error", "{source:?}");
        assert!(error.span.end <= source.len());
        assert!(source.is_char_boundary(error.span.start), "{source:?}");
        assert!(source.is_char_boundary(error.span.end), "{source:?}");
    }
}

#[test]
fn long_strings_stay_within_the_source_and_fuel_bounds() {
    let (env, bindings) = empty();
    let long = format!("'{}'", "x".repeat(4094));
    let result = evaluate(&check_expression(&long, &env).unwrap(), &bindings, 100_000).unwrap();
    assert_eq!(result.value.as_str().unwrap().len(), 4094);
    // Comparing two literals charges both, so 2 KB operands still fit.
    let pair = format!("'{0}' == '{0}'", "y".repeat(2040));
    let expression = check_expression(&pair, &env).unwrap();
    let cost = evaluate(&expression, &bindings, 100_000)
        .unwrap()
        .stats
        .cost;
    assert!(cost > 4 * 2040, "{cost}");
    let mut session = EvalSession::new(4 * 2040).unwrap();
    assert!(matches!(
        runbook_expr::evaluate_in(&expression, &bindings, &mut session),
        Err(runbook_expr::EvalError::CostExceeded)
    ));
}

#[test]
fn template_delimiters_never_interpolate_recursively() {
    let (env, bindings) = empty();
    for (source, expected) in [
        // Delimiters inside a CEL string are data, not a second expression.
        ("${{ '${{ true }}' }}", json!("${{ true }}")),
        ("a${{ '}}' }}b", json!("a}}b")),
        ("${{ '${{' }}${{ '}}' }}", json!("${{}}")),
        ("$${{ 1 }}$", json!("$1$")),
        ("}} ${ {{ }}", json!("}} ${ {{ }}")),
        ("${{true}}}}", json!("true}}")),
        ("${{ -9223372036854775808 }}", json!(i64::MIN)),
    ] {
        let template = check_template(source, &env).unwrap_or_else(|e| panic!("{source}: {e:?}"));
        let mut session = EvalSession::new(100_000).unwrap();
        assert_eq!(
            render_template(&template, &bindings, &mut session)
                .unwrap()
                .value,
            expected,
            "{source:?}"
        );
    }
}

#[test]
fn malformed_templates_fail_with_exact_codes_and_spans() {
    let (env, _) = empty();
    for (source, code, span) in [
        ("${{", "expression.syntax_error", 0..3),
        ("${{ ${{ }} }}", "expression.syntax_error", 4..7),
        (
            "${{ true }} ${{ ${{ }} }} }}",
            "expression.syntax_error",
            16..19,
        ),
        (
            "${{ ${{ ${{ ${{ true }} }} }} }}",
            "expression.syntax_error",
            4..7,
        ),
        ("${{ true }", "expression.syntax_error", 0..10),
        ("${{ true } }", "expression.syntax_error", 0..12),
        ("${{ 'unterminated }}", "expression.syntax_error", 0..20),
        ("${{ }}", "expression.syntax_error", 3..3),
        ("${{ ((true) }}", "expression.syntax_error", 11..11),
        ("${{ 1 / 0 }}", "expression.unsupported", 6..7),
        ("${{ '\\ud800' }}", "expression.syntax_error", 4..5),
    ] {
        let error = check_template(source, &env).unwrap_err();
        assert_eq!((error.code, error.span.clone()), (code, span), "{source:?}");
        assert!(source.is_char_boundary(error.span.start));
        assert!(source.is_char_boundary(error.span.end));
    }
    // Opening delimiters repeated to the template source limit stay bounded.
    let source = "${{".repeat(21_845);
    let error = check_template(&source, &env).unwrap_err();
    assert_eq!((error.code, error.span), ("expression.syntax_error", 3..6));
    let source = format!("${{{{{}", " ".repeat(65_533));
    assert_eq!(source.len(), 65_536);
    let error = check_template(&source, &env).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(error.span, 3..4100);
}

#[test]
fn whole_value_result_types_survive_extreme_literals() {
    let (env, _) = empty();
    for (source, ty) in [
        ("${{ 9223372036854775807 }}", ResultType::Int),
        ("${{ 1.7976931348623157e308 }}", ResultType::Double),
        ("${{ '\\U0010FFFF' }}", ResultType::String),
        (
            "${{ timestamp('0001-01-01T00:00:00Z') }}",
            ResultType::Timestamp,
        ),
    ] {
        let template = check_template(source, &env).unwrap();
        let expression = template.expressions().next().unwrap().expression;
        assert_eq!(expression.result_type(), ty, "{source}");
    }
}
