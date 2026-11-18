//! Deterministic fuzzing of the compiler, evaluator and template renderer.
//!
//! A fixed-seed generator produces grammar-directed expressions over a small
//! typed environment, token soup, mutations of valid expressions, deep and
//! unbalanced nesting, and template strings. Every input is compiled (and
//! evaluated or rendered when it compiles) under `catch_unwind`. No input may
//! panic, exceed a per-item wall-clock bound, report a malformed diagnostic,
//! spend more fuel than its session allows, or evaluate nondeterministically.
#![cfg(not(target_arch = "wasm32"))]

use runbook_expr::{
    Bindings, EvalError, EvalSession, EventTime, ExprDiagnostic, FieldDecl, InputNode, InputValue,
    Origin, RecordIdentity, SchemaDef, TypeDef, TypeEnv, TypeId, check_expression, check_template,
    evaluate_in, render_template,
};
use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

const MAX_FUEL: u64 = 100_000;
const PER_ITEM: Duration = Duration::from_secs(2);
const CODES: &[&str] = &[
    "expression.syntax_error",
    "expression.compile_limit",
    "expression.unsupported",
    "expression.type_error",
];

/// SplitMix64: tiny, deterministic, and good enough for input generation.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ty {
    Bool,
    Int,
    Double,
    Str,
    Time,
    Ints,
    Strs,
}
const SCALARS: [Ty; 5] = [Ty::Bool, Ty::Int, Ty::Double, Ty::Str, Ty::Time];

fn origin(path: impl Into<String>) -> Origin {
    Origin {
        path: path.into(),
        record: RecordIdentity {
            execution_id: "exe".into(),
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
fn decl(ty: u32, nullable: bool) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        required: true,
        nullable,
    }
}

fn fixture() -> (TypeEnv, Bindings) {
    let env = TypeEnv::new(&SchemaDef {
        types: vec![
            TypeDef::Bool,
            TypeDef::Int,
            TypeDef::Double,
            TypeDef::String,
            TypeDef::Timestamp,
            TypeDef::List(TypeId(1)),
            TypeDef::List(TypeId(3)),
            TypeDef::Record(BTreeMap::from([
                ("n".into(), decl(1, false)),
                ("s".into(), decl(3, false)),
                ("ns".into(), decl(3, true)),
                ("items".into(), decl(5, false)),
            ])),
        ],
        roots: BTreeMap::from([
            ("b".into(), decl(0, false)),
            ("i".into(), decl(1, false)),
            ("d".into(), decl(2, false)),
            ("s".into(), decl(3, false)),
            ("t".into(), decl(4, false)),
            ("xs".into(), decl(5, false)),
            ("ss".into(), decl(6, false)),
            ("r".into(), decl(7, false)),
            ("ni".into(), decl(1, true)),
        ]),
    })
    .unwrap();
    let ints = |path: &str, n: i64| {
        InputValue::List(
            (0..n)
                .map(|k| node(format!("{path}[{k}]"), InputValue::Int(k * 7919 - 500_000)))
                .collect(),
        )
    };
    let strings = (0..50)
        .map(|k| {
            let text = match k % 5 {
                0 => String::new(),
                1 => "é🦀\u{0}\"\\".repeat(k),
                2 => "z".repeat(2_000),
                3 => format!("item-{k}"),
                _ => "\u{10FFFF}".into(),
            };
            node(format!("ss[{k}]"), InputValue::String(text))
        })
        .collect();
    let roots = BTreeMap::from([
        ("b".into(), node("b", InputValue::Bool(true))),
        ("i".into(), node("i", InputValue::Int(i64::MIN))),
        ("d".into(), node("d", InputValue::Double(-0.0))),
        ("s".into(), node("s", InputValue::String("héllo 🦀".into()))),
        (
            "t".into(),
            node(
                "t",
                InputValue::Timestamp(EventTime {
                    unix_seconds: 253_402_300_799,
                    nanos: 999_999_999,
                }),
            ),
        ),
        ("xs".into(), node("xs", ints("xs", 200))),
        ("ss".into(), node("ss", InputValue::List(strings))),
        (
            "r".into(),
            node(
                "r",
                InputValue::Record(BTreeMap::from([
                    ("n".into(), node("r.n", InputValue::Int(i64::MAX))),
                    ("s".into(), node("r.s", InputValue::String("rec".into()))),
                    ("ns".into(), node("r.ns", InputValue::Null)),
                    ("items".into(), node("r.items", ints("r.items", 300))),
                ])),
            ),
        ),
        ("ni".into(), node("ni", InputValue::Null)),
    ]);
    let bindings = Bindings::new(
        &env,
        &roots,
        &origin(""),
        EventTime {
            unix_seconds: 1_700_000_000,
            nanos: 5,
        },
    )
    .unwrap();
    (env, bindings)
}

/// Grammar-directed expression of (mostly) type `ty`.
fn expr(rng: &mut Rng, ty: Ty, depth: usize, max: usize) -> String {
    let leaf = depth >= max || rng.chance(30);
    let mut text = if leaf {
        leaf_of(rng, ty)
    } else {
        match (ty, rng.below(6)) {
            (_, 0) => format!(
                "{} ? {} : {}",
                expr(rng, Ty::Bool, depth + 1, max),
                expr(rng, ty, depth + 1, max),
                expr(rng, ty, depth + 1, max)
            ),
            (_, 1) => format!("({})", expr(rng, ty, depth + 1, max)),
            (Ty::Bool, 2) => {
                let operand = SCALARS[rng.below(SCALARS.len())];
                let op = rng.pick(&["==", "!=", "<", "<=", ">", ">="]);
                format!(
                    "{} {op} {}",
                    expr(rng, operand, depth + 1, max),
                    expr(rng, operand, depth + 1, max)
                )
            }
            (Ty::Bool, 3) => format!(
                "{} {} {}",
                expr(rng, Ty::Bool, depth + 1, max),
                rng.pick(&["&&", "||"]),
                expr(rng, Ty::Bool, depth + 1, max)
            ),
            (Ty::Bool, 4) => format!("!{}", expr(rng, Ty::Bool, depth + 1, max)),
            (Ty::Bool, _) => {
                let (range, item) = if rng.chance(50) {
                    (rng.pick(&["xs", "r.items"]), Ty::Int)
                } else {
                    ("ss", Ty::Str)
                };
                let predicate = expr(rng, Ty::Bool, depth + 1, max);
                let compare = format!("v {} {}", rng.pick(&["<", ">", "=="]), leaf_of(rng, item));
                let body = if rng.chance(50) {
                    format!("{compare} && {predicate}")
                } else {
                    predicate
                };
                format!("{range}.exists(v, {body})")
            }
            (Ty::Int | Ty::Str, 2 | 3) => {
                let list = if ty == Ty::Int { Ty::Ints } else { Ty::Strs };
                format!("{}[{}]", expr(rng, list, depth + 1, max), rng.below(320))
            }
            _ => leaf_of(rng, ty),
        }
    };
    if rng.chance(3) {
        // A random spurious operator or extra token.
        text.push_str(rng.pick(&[" + 1", " .x", " )", "(", " ,", "[0]", " in xs", "?"]));
    }
    text
}

fn leaf_of(rng: &mut Rng, ty: Ty) -> String {
    match ty {
        Ty::Bool => rng
            .pick(&[
                "true",
                "false",
                "b",
                "ni == null",
                "r.ns == null",
                "null == null",
            ])
            .into(),
        Ty::Int => rng
            .pick(&[
                "0",
                "-1",
                "i",
                "r.n",
                "9223372036854775807",
                "-9223372036854775808",
                "0x7fffffffffffffff",
                "42",
            ])
            .into(),
        Ty::Double => rng
            .pick(&[
                "d",
                "-0.0",
                "1.5",
                "1e308",
                "5e-324",
                "-1.7976931348623157e308",
            ])
            .into(),
        Ty::Str => match rng.below(6) {
            0 => "s".into(),
            1 => "r.s".into(),
            2 => format!("'{}'", "q".repeat(rng.below(3000))),
            3 => rng
                .pick(&[r"'\U0010FFFF'", r"'\x41\101'", "'é🦀'", "''", r#""\"""#])
                .into(),
            _ => format!("ss[{}]", rng.below(60)),
        },
        Ty::Time => rng
            .pick(&[
                "now()",
                "t",
                "timestamp('0001-01-01T00:00:00Z')",
                "timestamp('9999-12-31T23:59:59.999999999Z')",
                "timestamp('2000-02-30T00:00:00Z')",
            ])
            .into(),
        Ty::Ints => rng.pick(&["xs", "r.items", "b ? xs : r.items"]).into(),
        Ty::Strs => "ss".into(),
    }
}

const SOUP: &[&str] = &[
    "true",
    "false",
    "null",
    "b",
    "i",
    "d",
    "s",
    "t",
    "xs",
    "ss",
    "r",
    "ni",
    "v",
    "x",
    "unknown",
    "1",
    "-",
    "0",
    "9223372036854775808",
    "1e999",
    "0x",
    "1u",
    "1.",
    ".5",
    "'a'",
    "\"b\"",
    "'",
    "\"",
    "'''",
    "b'",
    "r'",
    "\\",
    "\\u",
    "+",
    "*",
    "/",
    "%",
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "&&",
    "||",
    "!",
    "?",
    ":",
    ".",
    ",",
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    "${{",
    "}}",
    "in",
    "exists",
    "all",
    "map",
    "filter",
    "exists_one",
    "has",
    "size",
    "now",
    "timestamp",
    "duration",
    "int",
    "string",
    "dyn",
    "type",
    " ",
    "\n",
    "\t",
    "//",
    "é",
    "🦀",
    "\u{0}",
    "\u{202e}",
    "\u{feff}",
    "`",
    "@result",
    "__result__",
    ".?",
    "[?",
];

fn soup(rng: &mut Rng) -> String {
    (0..rng.below(40))
        .map(|_| rng.pick(SOUP).to_owned())
        .collect::<Vec<_>>()
        .join(if rng.chance(50) { " " } else { "" })
}

fn mutate(rng: &mut Rng, source: &str) -> String {
    let mut chars: Vec<char> = source.chars().collect();
    for _ in 0..=rng.below(4) {
        match rng.below(5) {
            0 if !chars.is_empty() => {
                let at = rng.below(chars.len());
                chars.remove(at);
            }
            1 => {
                let at = rng.below(chars.len() + 1);
                let token: Vec<char> = rng.pick(SOUP).chars().collect();
                chars.splice(at..at, token);
            }
            2 if !chars.is_empty() => chars.truncate(rng.below(chars.len())),
            3 if chars.len() > 1 => {
                let start = rng.below(chars.len());
                let end = start + rng.below(chars.len() - start);
                let copy: Vec<char> = chars[start..end].to_vec();
                chars.splice(start..start, copy);
            }
            _ => {
                let open = rng.below(40);
                let close = if rng.chance(70) { open } else { rng.below(40) };
                let prefix = rng.pick(&["(", "!", "-", "["]);
                let suffix = if prefix == "(" { ")" } else { "" };
                let body: String = chars.iter().collect();
                chars = format!("{}{body}{}", prefix.repeat(open), suffix.repeat(close))
                    .chars()
                    .collect();
            }
        }
    }
    chars.into_iter().collect()
}

fn template(rng: &mut Rng, valid: &[String]) -> String {
    let mut text = String::new();
    for _ in 0..rng.below(8) {
        match rng.below(6) {
            0 if !valid.is_empty() => {
                text.push_str("${{ ");
                text.push_str(&valid[rng.below(valid.len())]);
                text.push_str(" }}");
            }
            1 => text.push_str(rng.pick(&["${{", "}}", "${{ ${{ }} }}", "$", "{{", "}"])),
            2 => text.push_str(rng.pick(&["'", "\"", "\\", "//", "\n", "\r"])),
            3 => text.push_str(&soup(rng)),
            _ => text.push_str(rng.pick(&["plain ", "é🦀 ", "${ {{ ", "a}}b", ""])),
        }
    }
    text
}

fn check_diagnostic(source: &str, error: &ExprDiagnostic) -> Result<(), String> {
    if !CODES.contains(&error.code) {
        return Err(format!("unexpected code {}", error.code));
    }
    if error.span.start > error.span.end
        || error.span.end > source.len()
        || !source.is_char_boundary(error.span.start)
        || !source.is_char_boundary(error.span.end)
    {
        return Err(format!("bad span {:?}", error.span));
    }
    if error.message.is_empty() || error.message.len() > 4096 {
        return Err(format!("bad message length {}", error.message.len()));
    }
    Ok(())
}

fn check_session(fuel: u64, session: &EvalSession) -> Result<(), String> {
    let stats = session.stats();
    if stats.cost > fuel {
        return Err(format!("cost {} exceeds fuel {fuel}", stats.cost));
    }
    if stats.reads.len() > 4096 {
        return Err(format!("{} reads", stats.reads.len()));
    }
    Ok(())
}

#[derive(Default)]
struct Tally {
    compiled: usize,
    evaluated: usize,
    out_of_fuel: usize,
}

fn expression_case(
    source: &str,
    fuel: u64,
    env: &TypeEnv,
    bindings: &Bindings,
    tally: &mut Tally,
) -> Result<(), String> {
    let expression = match check_expression(source, env) {
        Ok(expression) => expression,
        Err(error) => return check_diagnostic(source, &error),
    };
    tally.compiled += 1;
    let run = || {
        let mut session = EvalSession::new(fuel).unwrap();
        let result = evaluate_in(&expression, bindings, &mut session);
        (result, session)
    };
    let (first, session) = run();
    check_session(fuel, &session)?;
    match &first {
        Ok(value) if value.stats != session.stats() => return Err("stale stats".into()),
        Ok(_) => tally.evaluated += 1,
        Err(EvalError::CostExceeded) => tally.out_of_fuel += 1,
        Err(EvalError::SchemaMismatch | EvalError::Cancelled) => {
            return Err(format!("impossible error {first:?}"));
        }
        Err(_) => {}
    }
    let (second, again) = run();
    if first != second || session.stats() != again.stats() {
        return Err("nondeterministic evaluation".into());
    }
    Ok(())
}

fn template_case(
    source: &str,
    fuel: u64,
    env: &TypeEnv,
    bindings: &Bindings,
    tally: &mut Tally,
) -> Result<(), String> {
    let checked = match check_template(source, env) {
        Ok(checked) => checked,
        Err(error) => return check_diagnostic(source, &error),
    };
    tally.compiled += 1;
    let mut session = EvalSession::new(fuel).unwrap();
    let result = render_template(&checked, bindings, &mut session);
    check_session(fuel, &session)?;
    if let Ok(value) = &result {
        tally.evaluated += 1;
        if value.stats != session.stats() {
            return Err("stale stats".into());
        }
    }
    Ok(())
}

fn fuel(rng: &mut Rng) -> u64 {
    match rng.below(4) {
        0 => rng.below(64) as u64,
        1 => rng.below(5_000) as u64,
        _ => MAX_FUEL,
    }
}

#[test]
fn random_inputs_never_panic_hang_or_overspend() {
    let (env, bindings) = fixture();
    let mut rng = Rng(0x05EE_DCE1_2026);
    let mut failures = Vec::new();
    let mut valid = Vec::new();
    let mut tallies: BTreeMap<&str, Tally> = BTreeMap::new();
    let started = Instant::now();
    for index in 0..4_000 {
        let kind = match index % 8 {
            0..=3 => "grammar",
            4 | 5 => "soup",
            6 => "mutation",
            _ => "template",
        };
        let source = match kind {
            "grammar" => {
                let ty = [Ty::Bool, Ty::Int, Ty::Double, Ty::Str, Ty::Time, Ty::Ints][rng.below(6)];
                let max = if rng.chance(10) { 40 } else { 6 };
                expr(&mut rng, ty, 0, max)
            }
            "soup" => soup(&mut rng),
            "mutation" if !valid.is_empty() => {
                let base: &String = &valid[rng.below(valid.len())];
                mutate(&mut rng, &base.clone())
            }
            "mutation" => soup(&mut rng),
            _ => template(&mut rng, &valid),
        };
        let fuel = fuel(&mut rng);
        let tally = tallies.entry(kind).or_default();
        let compiled_before = tally.compiled;
        let start = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            if kind == "template" {
                template_case(&source, fuel, &env, &bindings, tally)
            } else {
                expression_case(&source, fuel, &env, &bindings, tally)
            }
        }));
        let elapsed = start.elapsed();
        let failure = match outcome {
            Err(_) => Some("panicked".to_owned()),
            Ok(Err(reason)) => Some(reason),
            Ok(Ok(())) if elapsed > PER_ITEM => Some(format!("took {elapsed:?}")),
            Ok(Ok(())) => None,
        };
        if let Some(reason) = failure {
            failures.push(format!("#{index} {kind} fuel={fuel} {reason}: {source:?}"));
        } else if kind != "template"
            && tallies[kind].compiled > compiled_before
            && valid.len() < 500
        {
            valid.push(source);
        }
    }
    assert!(
        failures.is_empty(),
        "{} failing inputs:\n{}",
        failures.len(),
        failures[..failures.len().min(20)].join("\n")
    );
    for (kind, tally) in &tallies {
        println!(
            "{kind}: compiled {} evaluated {} out_of_fuel {}",
            tally.compiled, tally.evaluated, tally.out_of_fuel
        );
    }
    // The generator must keep reaching the evaluator, the fuel limit and
    // the template renderer, or this test would silently test only parsing.
    let grammar = &tallies["grammar"];
    assert!(grammar.compiled >= 1_000, "{}", grammar.compiled);
    assert!(grammar.evaluated >= 800, "{}", grammar.evaluated);
    assert!(grammar.out_of_fuel >= 150, "{}", grammar.out_of_fuel);
    assert!(tallies["mutation"].compiled >= 20);
    assert!(tallies["template"].evaluated >= 150);
    assert!(started.elapsed() < Duration::from_secs(60));
}
