//! Exact boundaries for every compile-time envelope and the runtime meter.
//!
//! Each limit is exercised at the value it admits and one past it, and the
//! rejection must carry its specific code (and, where it identifies which
//! limit fired, its span), not merely be an error.
use runbook_expr::{
    Bindings, EvalError, EvalSession, EventTime, FieldDecl, InputNode, InputValue, Origin,
    RecordIdentity, ResultType, SchemaDef, TypeDef, TypeEnv, TypeId, check_expression, evaluate,
    evaluate_in,
};
use serde_json::json;
use std::collections::BTreeMap;

const MAX_FUEL: u64 = 100_000;

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
fn decl(ty: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        required: true,
        nullable: false,
    }
}
fn empty() -> TypeEnv {
    TypeEnv::new(&SchemaDef {
        types: vec![],
        roots: BTreeMap::new(),
    })
    .unwrap()
}
fn code(source: &str, env: &TypeEnv) -> Result<ResultType, &'static str> {
    check_expression(source, env)
        .map(|e| e.result_type())
        .map_err(|e| e.code)
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

/// `xs: list<int>` and `s: string` roots with `n` list items and an
/// `len`-byte string.
fn list_and_string(n: usize, len: usize) -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Int, TypeDef::List(TypeId(0)), TypeDef::String],
        roots: BTreeMap::from([("xs".into(), decl(1)), ("s".into(), decl(2))]),
    })
    .unwrap();
    let items = (0..n)
        .map(|i| node(format!("xs[{i}]"), InputValue::Int(i as i64)))
        .collect();
    let bindings = Bindings::new(
        &env,
        &BTreeMap::from([
            ("xs".into(), node("xs", InputValue::List(items))),
            ("s".into(), node("s", InputValue::String("a".repeat(len)))),
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

// SOURCE_BYTES = 4096 (cel-core CompileLimits::runbook_v1), counted in UTF-8.
#[test]
fn source_byte_limit_admits_4096_and_rejects_4097() {
    let env = empty();
    let at = " ".repeat(4092) + "true";
    assert_eq!(at.len(), 4096);
    assert_eq!(code(&at, &env), Ok(ResultType::Bool));
    // Multibyte content counts bytes, not characters.
    let literal = format!("'{}'", "é".repeat(2047));
    assert_eq!(literal.len(), 4096);
    assert_eq!(code(&literal, &env), Ok(ResultType::String));
    for over in [
        " ".repeat(4093) + "true",
        format!("'{}a'", "é".repeat(2047)),
    ] {
        assert_eq!(over.len(), 4097);
        let error = check_expression(&over, &env).unwrap_err();
        assert_eq!(error.code, "expression.compile_limit");
        assert_eq!(error.span, 0..4097);
    }
}

// TOKENS = 512. The over-limit source has fewer nodes than the accepted one,
// so only the token count can reject it; the span names the 513th token.
#[test]
fn token_limit_admits_512_and_rejects_513() {
    let env = empty();
    // `!` + 128 balanced leaves = 256 nodes, 510 tokens; one parenthesized
    // leaf adds two tokens and no node.
    let at = format!("!{}", balanced(128)).replacen("true", "(true)", 1);
    assert_eq!(code(&at, &env), Ok(ResultType::Bool));
    // 255 nodes, 509 + 4 tokens.
    let over = balanced(128)
        .replacen("true", "(true)", 1)
        .replacen(" true", " (true)", 1);
    assert_eq!(over.matches("(true)").count(), 2);
    let error = check_expression(&over, &env).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(error.span, over.len() - 1..over.len());
    // Removing one token from the same tree is accepted.
    let under = balanced(128).replacen("true", "(true)", 1);
    assert_eq!(code(&under, &env), Ok(ResultType::Bool));
}

// Checker identifiers are at most 128 bytes (compile.rs), the same bound
// as schema identifiers (limits::IDENTIFIER_BYTES), for roots and fields.
#[test]
fn identifier_limit_admits_128_bytes_for_roots_and_fields() {
    let root = "r".repeat(128);
    let field = "f".repeat(128);
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([(field.clone(), decl(0))])),
        ],
        roots: BTreeMap::from([(root.clone(), decl(1))]),
    })
    .unwrap();
    assert_eq!(code(&format!("{root}.{field}"), &env), Ok(ResultType::Int));
    // One byte more: rejected as a compile limit at the identifier itself,
    // not as an undeclared identifier/field type error.
    let source = format!("{root}.{field}f");
    let error = check_expression(&source, &env).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(error.span, 129..258);
    let source = format!("{root}r.{field}");
    let error = check_expression(&source, &env).unwrap_err();
    assert_eq!(error.code, "expression.compile_limit");
    assert_eq!(error.span, 0..129);
    // The schema cannot declare the longer name either.
    let rejected = TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Int],
        roots: BTreeMap::from([("r".repeat(129), decl(0))]),
    })
    .unwrap_err();
    assert_eq!(rejected.code, "expression.schema_limit");
}

// PARSE_DEPTH = 256 recursive-descent frames. Parentheses add frames but no
// AST nodes or tree depth, so this is the only limit nested grouping meets:
// with the current grammar 24 levels fit and 25 do not.
#[test]
fn nested_parentheses_hit_the_parser_frame_limit_at_25_levels() {
    let env = empty();
    let nest = |n: usize, leaf: &str| format!("{}{leaf}{}", "(".repeat(n), ")".repeat(n));
    for leaf in ["true", "1", "'x'"] {
        assert!(code(&nest(24, leaf), &env).is_ok(), "{leaf}");
        for n in [25, 26, 200, 2000] {
            let source = nest(n, leaf);
            let error = check_expression(&source, &env).unwrap_err();
            assert_eq!(error.code, "expression.compile_limit", "{n} {leaf}");
            assert!(error.span.start >= 24 && error.span.end <= source.len());
        }
    }
    // Each `!` costs one more frame, so the frame budget is exact: inside 24
    // parentheses six negations fit and a seventh does not (inside 23, 16
    // and 17), far below the 32-level tree depth that `!` also counts.
    let negated = |parens: usize, nots: usize| {
        format!(
            "{}{}true{}",
            "(".repeat(parens),
            "!".repeat(nots),
            ")".repeat(parens)
        )
    };
    for (parens, admitted) in [(24, 6), (23, 16)] {
        assert_eq!(code(&negated(parens, admitted), &env), Ok(ResultType::Bool));
        let source = negated(parens, admitted + 1);
        let error = check_expression(&source, &env).unwrap_err();
        assert_eq!(error.code, "expression.compile_limit", "{parens}");
        // Reported at the operand the parser could not enter.
        assert_eq!(error.span, parens + admitted + 1..parens + admitted + 5);
    }
    // Unbalanced variants are syntax, not limit, failures when shallow.
    for source in ["((true)", "(true))", "(", ")", "((((((((true"] {
        assert_eq!(
            code(source, &env),
            Err("expression.syntax_error"),
            "{source}"
        );
    }
}

// profile.rs: a timestamp() argument is a string literal of at most 64 bytes.
#[test]
fn timestamp_literal_argument_admits_64_bytes() {
    let env = empty();
    let valid = |len: usize| {
        let literal = format!("2000-01-01T00:00:00.{}Z", "0".repeat(len - 21));
        assert_eq!(literal.len(), len);
        format!("timestamp('{literal}')")
    };
    assert_eq!(code(&valid(64), &env), Ok(ResultType::Timestamp));
    assert_eq!(code(&valid(65), &env), Err("expression.unsupported"));
    // At the bound an invalid literal reaches calendar validation.
    let invalid = |len: usize| format!("timestamp('{}')", "a".repeat(len));
    assert_eq!(code(&invalid(64), &env), Err("expression.type_error"));
    assert_eq!(code(&invalid(65), &env), Err("expression.unsupported"));
}

// EvalSession::new: fuel above 100,000 is refused with a specific code, and
// the convenience entry point never evaluates under an oversized budget.
#[test]
fn evaluation_fuel_hard_cap_is_100_000() {
    assert!(EvalSession::new(MAX_FUEL).is_ok());
    for fuel in [MAX_FUEL + 1, u64::MAX] {
        let error = EvalSession::new(fuel).unwrap_err();
        assert_eq!(error.code, "expression.evaluation_limit");
        assert_eq!(error.span, 0..0);
    }
    let (env, bindings) = list_and_string(0, 0);
    let literal = check_expression("true", &env).unwrap();
    assert_eq!(
        evaluate(&literal, &bindings, MAX_FUEL).unwrap().value,
        json!(true)
    );
    assert_eq!(
        evaluate(&literal, &bindings, MAX_FUEL + 1),
        Err(EvalError::CostExceeded)
    );
}

// The meter is exact: an expression whose reported cost is C succeeds with
// fuel C and fails with C - 1, never succeeding beyond its budget.
#[test]
fn reported_cost_is_exactly_the_fuel_needed() {
    let (env, bindings) = list_and_string(1000, 300);
    for source in [
        "true",
        "xs.exists(i, i < 0)",
        "xs.exists(i, i == 999)",
        "xs.exists(i, false)",
        "xs[999] == 999",
        "s == s",
        "s < 'b'",
        "true ? s : 'x'",
        "now() > timestamp('2000-01-01T00:00:00Z')",
        "xs",
    ] {
        let expression = check_expression(source, &env).unwrap();
        let full = evaluate(&expression, &bindings, MAX_FUEL).unwrap();
        let cost = full.stats.cost;
        assert!(cost > 0 && cost <= MAX_FUEL, "{source}: {cost}");
        let exact = evaluate(&expression, &bindings, cost).unwrap();
        assert_eq!(exact, full, "{source}");
        let mut session = EvalSession::new(cost - 1).unwrap();
        assert_eq!(
            evaluate_in(&expression, &bindings, &mut session),
            Err(EvalError::CostExceeded),
            "{source}"
        );
        assert!(session.stats().cost < cost, "{source}");
    }
}

// Iteration charges fuel per element: over the largest bindable list
// (100,000 items) a predicate that never reads its item still stops at the
// hard cap rather than scanning the whole list.
#[test]
fn exists_over_the_largest_list_stops_at_the_fuel_cap() {
    let (env, bindings) = list_and_string(100_000, 0);
    let expression = check_expression("xs.exists(i, false)", &env).unwrap();
    let mut session = EvalSession::new(MAX_FUEL).unwrap();
    assert_eq!(
        evaluate_in(&expression, &bindings, &mut session),
        Err(EvalError::CostExceeded)
    );
    let stats = session.stats();
    assert_eq!(stats.cost, MAX_FUEL);
    // Each iteration costs exactly 8 fuel, one of which is the visit charge
    // that is paid before the item is bound.
    assert_eq!(stats.loop_entries, 12_499);
    let per_item = |n: usize| {
        let (env, bindings) = list_and_string(n, 0);
        let expression = check_expression("xs.exists(i, false)", &env).unwrap();
        let result = evaluate(&expression, &bindings, MAX_FUEL).unwrap();
        assert_eq!(result.stats.loop_entries, n as u64);
        result.stats.cost
    };
    assert_eq!(per_item(11) - per_item(10), 8);
    assert_eq!(per_item(5000) - per_item(4000), 8 * 1000);
    // A predicate that reads its item stops at the read limit first.
    let reading = check_expression("xs.exists(i, i < 0)", &env).unwrap();
    let mut session = EvalSession::new(MAX_FUEL).unwrap();
    assert_eq!(
        evaluate_in(&reading, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    assert_eq!(session.stats().reads.len(), 4096);
}

// reads.rs MAX_READS = 4096: one membership read of the list plus one value
// read per visited item. 4095 items fit; the 4096th item is refused.
#[test]
fn comprehension_read_limit_admits_4096_reads() {
    let expression = |env: &TypeEnv| check_expression("xs.exists(i, i < 0)", env).unwrap();
    let (env, bindings) = list_and_string(4095, 0);
    let result = evaluate(&expression(&env), &bindings, MAX_FUEL).unwrap();
    assert_eq!(result.value, json!(false));
    assert_eq!(result.stats.reads.len(), 4096);
    assert_eq!(result.stats.loop_entries, 4095);
    let (env, bindings) = list_and_string(4096, 0);
    let mut session = EvalSession::new(MAX_FUEL).unwrap();
    assert_eq!(
        evaluate_in(&expression(&env), &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
    assert_eq!(session.stats().reads.len(), 4096);
    // The limit is sticky: a later trivial evaluation cannot succeed.
    let literal = check_expression("true", &env).unwrap();
    assert_eq!(
        evaluate_in(&literal, &bindings, &mut session),
        Err(EvalError::ReadLimit)
    );
}

// String comparison precharges both operands' bytes, so the longest string
// comparable within the hard cap is exact: cost(s < s) = 14 + 2 * len.
#[test]
fn string_comparison_cost_is_linear_and_capped() {
    let cost = |len: usize| {
        let (env, bindings) = list_and_string(0, len);
        let expression = check_expression("s < s", &env).unwrap();
        let mut session = EvalSession::new(MAX_FUEL).unwrap();
        let result = evaluate_in(&expression, &bindings, &mut session);
        (result, session.stats().cost)
    };
    let (base, base_cost) = cost(0);
    assert_eq!(base.unwrap().value, json!(false));
    assert_eq!(cost(10).1, base_cost + 20);
    let at = (MAX_FUEL - base_cost) as usize / 2;
    let (result, spent) = cost(at);
    assert_eq!(result.unwrap().value, json!(false));
    assert_eq!(spent, MAX_FUEL);
    let (result, spent) = cost(at + 1);
    assert_eq!(result, Err(EvalError::CostExceeded));
    assert!(spent < MAX_FUEL);
    // The maximum bindable string is refused before any byte comparison.
    let (result, spent) = cost(65_536);
    assert_eq!(result, Err(EvalError::CostExceeded));
    assert!(spent < 100);
}
