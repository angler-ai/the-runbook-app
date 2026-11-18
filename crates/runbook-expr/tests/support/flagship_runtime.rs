//! Reusable public-API runtime evidence for native and later Wasm execution.
//! Expected values come from the data-only fixture; expected reads below are
//! independently specified, never reconstructed from observed runtime stats.
use super::flagship;

use flagship::{
    COMPANION_REPORT, ExpectedResult, Scenario, ScenarioFixture, Scope, flagship_cases,
    scenario_fixture, scope_schema,
};
use runbook_expr::*;
use serde_json::{Value, json};

fn scenario_name(scenario: Scenario) -> &'static str {
    match scenario {
        Scenario::Healthy => "healthy",
        Scenario::Unhealthy => "unhealthy",
        Scenario::Rollback => "rollback",
        Scenario::Keep => "keep",
        Scenario::NullVerdict => "null-verdict",
        Scenario::MissingOutput => "missing-output",
        Scenario::DistinctIterations => "distinct-iterations",
    }
}
fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Global => "global",
        Scope::Foreach => "foreach",
        Scope::Loop => "loop",
        Scope::Report => "report",
    }
}
fn healthy_rates(scenario: Scenario) -> bool {
    matches!(scenario, Scenario::Healthy | Scenario::NullVerdict)
}

// Trace owners are specified from the scenario contract, independently of the
// input tree converter. Comparing ReadRef checks every field and list position.
fn root_read(path: &str, kind: ReadKind) -> ReadRef {
    ReadRef {
        origin: Origin {
            path: path.into(),
            record: RecordIdentity {
                execution_id: "exe_flagship".into(),
                step_run_id: None,
                iteration_path: vec![],
                revision: 1,
            },
        },
        kind,
    }
}
fn step_read(path: &str, step: &str, kind: ReadKind) -> ReadRef {
    let mut read = root_read(path, kind);
    read.origin.record.step_run_id = Some(format!("sr_{step}"));
    read.origin.record.revision = 7;
    read
}
fn iteration_read(path: &str, control: &str, child: &str, index: u32, kind: ReadKind) -> ReadRef {
    let mut read = step_read(path, &format!("{child}_{index}"), kind);
    read.origin.record.iteration_path = vec![IterationIdentity {
        node_id: control.into(),
        instance_id: format!("control_{control}"),
        ordinal: index,
    }];
    read.origin.record.revision = 7 + u64::from(index);
    read
}
fn regional_read(index: u32, suffix: &str, kind: ReadKind) -> ReadRef {
    iteration_read(
        &format!("steps.regions.items[{index}].error_rate.{suffix}"),
        "regions",
        "error_rate",
        index,
        kind,
    )
}
fn loop_read(index: u32, path: &str) -> ReadRef {
    iteration_read(path, "stabilize", "recheck", index, ReadKind::Value)
}
fn site_reads(scenario: Scenario, pointer: &str) -> Vec<ReadRef> {
    use ReadKind::{Absence as A, Membership as M, Value as V};
    match pointer {
        "/spec/execution/title" => vec![
            root_read("inputs.service", V),
            root_read("inputs.version", V),
        ],
        "/spec/execution/concurrency/key"
        | "/spec/steps/0/parallel/0/instructions"
        | "/spec/steps/3/switch/cases/0/steps/1/title" => vec![root_read("inputs.service", V)],
        "/spec/steps/3/switch/cases/0/steps/0/approval/prompt" => {
            vec![root_read("inputs.version", V)]
        }
        "/spec/steps/1/foreach/items" => vec![
            root_read("inputs.regions", M),
            root_read("inputs.regions[0]", V),
            root_read("inputs.regions[1]", V),
            root_read("inputs.regions[2]", V),
        ],
        "/spec/steps/1/foreach/steps/0/title" | "/spec/steps/1/foreach/steps/0/with/region" => {
            vec![root_read("inputs.regions[1]", V)]
        }
        "/spec/steps/2/if" => {
            let mut reads = vec![step_read("steps.regions.items", "regions", M)];
            if matches!(scenario, Scenario::MissingOutput) {
                reads.push(regional_read(0, "outputs.rate", A));
                // Ordinary missing values are not terminal resource failures.
                // CEL's exists continues its error accumulator, observes the
                // next true predicate, then stops at the third loop condition.
                // The wrapper returns the saved first MissingValue afterward.
                reads.push(regional_read(1, "outputs.rate", V));
            } else {
                reads.push(regional_read(0, "outputs.rate", V));
                reads.push(regional_read(1, "outputs.rate", V));
                if healthy_rates(scenario) {
                    reads.push(regional_read(2, "outputs.rate", V));
                }
            }
            reads
        }
        "/spec/steps/2/loop/until" => vec![loop_read(0, "steps.recheck.outputs.rate")],
        "/spec/steps/3/switch/cases/0/when" => {
            let mut reads = vec![step_read("steps.smoke.verdict", "smoke", V)];
            if healthy_rates(scenario) {
                reads.push(step_read("steps.regions.verdict", "regions", V));
                reads.push(step_read("steps.stabilize.verdict", "stabilize", V));
            }
            reads
        }
        "/spec/steps/3/switch/cases/0/steps/1/if"
        | "/spec/steps/3/switch/cases/0/steps/2/set/vars/result" => vec![step_read(
            "steps.signoff.outputs.decision",
            "signoff",
            if matches!(
                scenario,
                Scenario::Rollback | Scenario::Keep | Scenario::DistinctIterations
            ) {
                V
            } else {
                A
            },
        )],
        "/spec/outputs/result" => vec![root_read("vars.result", V)],
        "/companion/report" => vec![root_read("outputs.result", V)],
        _ => panic!("missing hand-derived reads for {pointer}"),
    }
}
fn expected_result(expected: &ExpectedResult) -> Result<Value, EvalError> {
    match expected {
        ExpectedResult::Value(value) => Ok(value.clone()),
        ExpectedResult::MissingValue { path } => {
            Err(EvalError::MissingValue { path: path.clone() })
        }
    }
}
fn encoded_error(error: EvalError) -> Value {
    match error {
        EvalError::CostExceeded => json!({"kind": "CostExceeded"}),
        EvalError::Cancelled => json!({"kind": "Cancelled"}),
        EvalError::ReadLimit => json!({"kind": "ReadLimit"}),
        EvalError::OutputLimit => json!({"kind": "OutputLimit"}),
        EvalError::SchemaMismatch => json!({"kind": "SchemaMismatch"}),
        EvalError::MissingValue { path } => json!({"kind": "MissingValue", "path": path}),
        EvalError::TypeError => json!({"kind": "TypeError"}),
        EvalError::IndexOutOfBounds => json!({"kind": "IndexOutOfBounds"}),
    }
}

struct Check<'a> {
    scenario: Scenario,
    scope: Scope,
    pointer: &'a str,
    source: &'a str,
    template: bool,
    expected: Result<Value, EvalError>,
    reads: Vec<ReadRef>,
    loops: u64,
}
fn execute(check: Check<'_>, session: &mut EvalSession) -> Value {
    let fixture = scenario_fixture(check.scenario, check.scope);
    let env = TypeEnv::new(&scope_schema(check.scope)).unwrap();
    let bound = bindings(&env, &fixture);
    let name = format!(
        "flagship/{}{}",
        scenario_name(check.scenario),
        check.pointer
    );
    let result = if check.template {
        render_template(
            &check_template(check.source, &env).unwrap(),
            &bound,
            session,
        )
    } else {
        evaluate_in(
            &check_expression(check.source, &env).unwrap(),
            &bound,
            session,
        )
    };
    let stats = session.stats();
    if let Ok(value) = &result {
        assert_eq!(value.stats, stats, "{name}: returned cumulative stats");
    }
    let actual = result.map(|value| value.value);
    assert_eq!(actual, check.expected, "{name}: value/error");
    assert_eq!(
        stats.reads, check.reads,
        "{name}: full ordered origins/read kinds"
    );
    assert_eq!(stats.loop_entries, check.loops, "{name}: loop entries");
    let outcome = match actual {
        Ok(value) => json!({"value": value, "stats": stats}),
        Err(error) => json!({"error": encoded_error(error), "stats": stats}),
    };
    json!({
        "name": name, "source": check.source, "scope": scope_name(check.scope),
        "api": if check.template { "render_template" } else { "evaluate_in" },
        "event_time": fixture.event_time, "outcome": outcome,
    })
}
fn bindings(env: &TypeEnv, fixture: &ScenarioFixture) -> Bindings {
    Bindings::new(
        env,
        &fixture.roots,
        &fixture.root_origin,
        fixture.event_time,
    )
    .unwrap()
}
fn fresh(check: Check<'_>) -> Value {
    execute(check, &mut EvalSession::new(100_000).unwrap())
}

pub fn runtime_rows() -> Vec<Value> {
    // Keep runtime evidence tied to the original containing values, including the
    // two expression segments in the title. Compile tests pin all source bytes.
    let cases = flagship_cases();
    assert_eq!(cases.len(), 14);
    assert_eq!(
        cases
            .iter()
            .map(|case| case.expressions.len())
            .sum::<usize>(),
        15
    );
    let mut rows = Vec::new();
    for scenario in Scenario::ALL {
        for case in &cases {
            let fixture = scenario_fixture(scenario, case.scope);
            assert_eq!(case.schema, scope_schema(case.scope));
            rows.push(fresh(Check {
                scenario,
                scope: case.scope,
                pointer: &case.pointer,
                source: &case.source,
                template: true,
                expected: expected_result(&fixture.expected[&case.pointer]),
                reads: site_reads(scenario, &case.pointer),
                // exists visits all three items for no match, or two predicate
                // items plus the accumulator termination entry for a match.
                loops: if case.pointer == "/spec/steps/2/if" {
                    3
                } else {
                    0
                },
            }));
        }
        companions(&mut rows, scenario);
    }
    rows
}

fn companions(rows: &mut Vec<Value>, scenario: Scenario) {
    use ReadKind::{Membership as M, Value as V};
    let report = scenario_fixture(scenario, Scope::Report);
    rows.push(fresh(Check {
        scenario,
        scope: Scope::Report,
        pointer: "/companion/report",
        source: COMPANION_REPORT,
        template: true,
        expected: expected_result(&report.expected["/companion/report"]),
        reads: site_reads(scenario, "/companion/report"),
        loops: 0,
    }));
    let second_rate = if healthy_rates(scenario) { 0.002 } else { 0.04 };
    rows.push(fresh(Check {
        scenario,
        scope: Scope::Global,
        pointer: "/companion/direct-index",
        source: "steps.regions.items[1].error_rate.outputs.rate",
        template: false,
        expected: Ok(json!(second_rate)),
        reads: vec![
            step_read("steps.regions.items", "regions", M),
            regional_read(1, "outputs.rate", V),
        ],
        loops: 0,
    }));
    rows.push(fresh(Check {
        scenario,
        scope: Scope::Global,
        pointer: "/companion/whole-output-record",
        source: "${{ steps.regions.items[1].error_rate.outputs }}",
        template: true,
        expected: Ok(json!({"rate": second_rate})),
        reads: vec![
            step_read("steps.regions.items", "regions", M),
            regional_read(1, "outputs", M),
            regional_read(1, "outputs.rate", V),
        ],
        loops: 0,
    }));
    // Both comparisons are true, so && consumes both iterations. The ordered
    // collection Membership deduplicates; distinct iteration owners must not.
    rows.push(fresh(Check {
        scenario, scope: Scope::Global, pointer: "/companion/distinct-iterations",
        source: "steps.stabilize.iterations[0].recheck.outputs.rate >= 0.005 && steps.stabilize.iterations[1].recheck.outputs.rate == 0.005",
        template: false, expected: Ok(json!(true)),
        reads: vec![
            step_read("steps.stabilize.iterations", "stabilize", M),
            loop_read(0, "steps.stabilize.iterations[0].recheck.outputs.rate"),
            loop_read(1, "steps.stabilize.iterations[1].recheck.outputs.rate"),
        ], loops: 0,
    }));
    // The same canonical path is evaluated in separately compiled lexical
    // scopes through ONE session. Its full identity, rather than its path,
    // distinguishes current iteration 0 from last iteration 1.
    let mut session = EvalSession::new(100_000).unwrap();
    rows.push(execute(
        Check {
            scenario,
            scope: Scope::Loop,
            pointer: "/companion/current-loop",
            source: "steps.recheck.outputs.rate",
            template: false,
            expected: Ok(json!(if healthy_rates(scenario) { 0.005 } else { 0.04 })),
            reads: vec![loop_read(0, "steps.recheck.outputs.rate")],
            loops: 0,
        },
        &mut session,
    ));
    rows.push(execute(
        Check {
            scenario,
            scope: Scope::Global,
            pointer: "/companion/last-loop",
            source: "steps.recheck.outputs.rate",
            template: false,
            expected: Ok(json!(0.005)),
            reads: vec![
                loop_read(0, "steps.recheck.outputs.rate"),
                loop_read(1, "steps.recheck.outputs.rate"),
            ],
            loops: 0,
        },
        &mut session,
    ));
    if matches!(scenario, Scenario::NullVerdict) {
        rows.push(fresh(Check {
            scenario,
            scope: Scope::Global,
            pointer: "/companion/null-value",
            source: "steps.smoke.verdict",
            template: false,
            expected: Ok(Value::Null),
            reads: vec![step_read("steps.smoke.verdict", "smoke", V)],
            loops: 0,
        }));
    }
}
