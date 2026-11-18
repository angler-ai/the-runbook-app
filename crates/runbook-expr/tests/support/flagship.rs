//! Test-only, data-only flagship corpus. These explicit declarations are not A5
//! schema lowering, and expected results are not evidence of execution.
use runbook_expr::{FieldDecl, SchemaDef, TypeDef, TypeId};
use std::collections::BTreeMap;
use yaml_rust2::{Yaml, YamlLoader};

pub const FLAGSHIP: &str = include_str!("../../../runbook-spec/tests/fixtures/post-deploy.yaml");
pub const COMPANION_REPORT: &str = "Result: ${{ outputs.result }}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Global,
    Foreach,
    Loop,
    Report,
}

#[derive(Debug, Clone)]
pub struct FlagshipCase {
    pub pointer: String,
    pub source: String,
    pub expressions: Vec<String>,
    pub scope: Scope,
    pub schema: SchemaDef,
}

pub fn flagship_sources() -> BTreeMap<String, String> {
    fn walk(value: &Yaml, pointer: &str, found: &mut BTreeMap<String, String>) {
        match value {
            Yaml::String(source) if source.contains("${{") => {
                assert!(found.insert(pointer.into(), source.clone()).is_none());
            }
            Yaml::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    walk(value, &format!("{pointer}/{index}"), found);
                }
            }
            Yaml::Hash(fields) => {
                for (key, value) in fields {
                    let key = key
                        .as_str()
                        .expect("string YAML key")
                        .replace('~', "~0")
                        .replace('/', "~1");
                    walk(value, &format!("{pointer}/{key}"), found);
                }
            }
            _ => {}
        }
    }
    let documents = YamlLoader::load_from_str(FLAGSHIP).expect("trusted flagship YAML");
    assert_eq!(documents.len(), 1);
    let mut found = BTreeMap::new();
    walk(&documents[0], "", &mut found);
    found
}

pub fn flagship_cases() -> Vec<FlagshipCase> {
    flagship_sources()
        .into_iter()
        .map(|(pointer, source)| {
            let scope = match pointer.as_str() {
                "/spec/steps/1/foreach/steps/0/title"
                | "/spec/steps/1/foreach/steps/0/with/region" => Scope::Foreach,
                "/spec/steps/2/loop/until" => Scope::Loop,
                _ => Scope::Global,
            };
            // Inventory only: this trusted fixture contains no delimiter within CEL
            // strings/comments. check_template remains the authoritative scanner.
            // Retain all whitespace; these are the exact bytes checked separately.
            let expressions = source
                .split("${{")
                .skip(1)
                .map(|part| {
                    part.split_once("}}")
                        .expect("closed fixture interpolation")
                        .0
                        .to_owned()
                })
                .collect();
            FlagshipCase {
                pointer,
                source,
                expressions,
                scope,
                schema: scope_schema(scope),
            }
        })
        .collect()
}

fn field(ty: TypeId) -> FieldDecl {
    FieldDecl {
        ty,
        nullable: false,
        required: true,
    }
}
fn optional(ty: TypeId) -> FieldDecl {
    FieldDecl {
        required: false,
        ..field(ty)
    }
}
fn nullable(ty: TypeId) -> FieldDecl {
    FieldDecl {
        nullable: true,
        ..field(ty)
    }
}
fn add(types: &mut Vec<TypeDef>, ty: TypeDef) -> TypeId {
    let id = TypeId(types.len().try_into().unwrap());
    types.push(ty);
    id
}
fn record(types: &mut Vec<TypeDef>, fields: &[(&str, FieldDecl)]) -> TypeId {
    add(
        types,
        TypeDef::Record(
            fields
                .iter()
                .map(|(name, field)| ((*name).into(), field.clone()))
                .collect(),
        ),
    )
}

// Primitive IDs stay private to this test-only schema builder.
const STRING: TypeId = TypeId(0);
const INT: TypeId = TypeId(1);
const DOUBLE: TypeId = TypeId(2);
const BOOL: TypeId = TypeId(3);
const TIMESTAMP: TypeId = TypeId(4);

fn step(types: &mut Vec<TypeDef>, outputs: TypeId, extras: &[(&str, FieldDecl)]) -> TypeId {
    let mut fields = vec![
        ("status", field(STRING)),
        ("verdict", nullable(STRING)),
        ("outputs", field(outputs)),
        ("actor", nullable(STRING)),
        ("started_at", nullable(TIMESTAMP)),
        ("finished_at", nullable(TIMESTAMP)),
        ("attempts", field(INT)),
        ("amended", field(BOOL)),
        ("stale", field(BOOL)),
    ];
    fields.extend_from_slice(extras);
    record(types, &fields)
}

pub fn scope_schema(scope: Scope) -> SchemaDef {
    let mut types = vec![
        TypeDef::String,
        TypeDef::Int,
        TypeDef::Double,
        TypeDef::Bool,
        TypeDef::Timestamp,
    ];
    let strings = add(&mut types, TypeDef::List(STRING));
    let empty_outputs = record(&mut types, &[]);
    let status_output = record(&mut types, &[("status_code", optional(INT))]);
    let rate_output = record(&mut types, &[("rate", optional(DOUBLE))]);
    let approval_output = record(
        &mut types,
        &[
            ("decision", optional(STRING)),
            ("comment", optional(STRING)),
            ("decided_by", optional(STRING)),
        ],
    );
    let ordinary = step(&mut types, empty_outputs, &[]);
    let api_health = step(&mut types, status_output, &[]);
    let rate = step(&mut types, rate_output, &[]);
    let signoff = step(&mut types, approval_output, &[]);
    let region_item = record(&mut types, &[("error_rate", field(rate))]);
    let region_items = add(&mut types, TypeDef::List(region_item));
    let regions = step(&mut types, empty_outputs, &[("items", field(region_items))]);
    let loop_item = record(&mut types, &[("recheck", field(rate))]);
    let iterations = add(&mut types, TypeDef::List(loop_item));
    let stabilize = step(
        &mut types,
        empty_outputs,
        &[
            ("iterations", field(iterations)),
            ("satisfied", field(BOOL)),
            ("exhausted", field(BOOL)),
        ],
    );
    let steps = record(
        &mut types,
        &[
            ("smoke", field(ordinary)),
            ("api_health", field(api_health)),
            ("login_flow", field(ordinary)),
            ("regions", field(regions)),
            ("error_rate", field(rate)),
            ("stabilize", field(stabilize)),
            ("recheck", field(rate)),
            ("decide", field(ordinary)),
            ("signoff", field(signoff)),
            ("rollback", field(ordinary)),
            ("mark_major", field(ordinary)),
            ("mark_healthy", field(ordinary)),
            ("notify", field(ordinary)),
        ],
    );
    let inputs = record(
        &mut types,
        &[
            ("service", field(STRING)),
            ("version", field(STRING)),
            ("regions", field(strings)),
        ],
    );
    let fields = record(&mut types, &[("severity", field(STRING))]);
    let vars = record(&mut types, &[("result", field(STRING))]);
    // No project variables are declared by this flagship. The empty record is
    // deliberately closed, rather than granting arbitrary project fields.
    let project = record(&mut types, &[("vars", field(empty_outputs))]);
    let execution = record(
        &mut types,
        &[
            ("id", field(STRING)),
            ("title", field(STRING)),
            ("url", field(STRING)),
            ("created_by", field(STRING)),
            ("tags", field(strings)),
            ("started_at", field(TIMESTAMP)),
        ],
    );
    let mut roots: BTreeMap<_, _> = [
        ("inputs", field(inputs)),
        ("fields", field(fields)),
        ("vars", field(vars)),
        ("steps", field(steps)),
        ("project", field(project)),
        ("execution", field(execution)),
    ]
    .into_iter()
    .map(|(name, field)| (name.into(), field))
    .collect();
    match scope {
        Scope::Global => {}
        Scope::Foreach => {
            roots.insert("region".into(), field(STRING));
            roots.insert("index".into(), field(INT));
        }
        Scope::Loop => {
            roots.insert("iteration".into(), field(INT));
        }
        Scope::Report => {
            roots.insert("outputs".into(), field(vars));
        }
    }
    SchemaDef { types, roots }
}

use runbook_expr::{EventTime, InputNode, InputValue, IterationIdentity, Origin, RecordIdentity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Debug, Clone, Copy)]
pub enum Scenario {
    Healthy,
    Unhealthy,
    Rollback,
    Keep,
    NullVerdict,
    MissingOutput,
    DistinctIterations,
}
impl Scenario {
    pub const ALL: [Self; 7] = [
        Self::Healthy,
        Self::Unhealthy,
        Self::Rollback,
        Self::Keep,
        Self::NullVerdict,
        Self::MissingOutput,
        Self::DistinctIterations,
    ];
}
pub struct ScenarioFixture {
    pub roots: BTreeMap<String, InputNode>,
    pub root_origin: Origin,
    pub event_time: EventTime,
    pub expected: BTreeMap<String, ExpectedResult>,
}

/// Hand-derived expectations for later public-API execution, not observed reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExpectedResult {
    Value(Value),
    MissingValue { path: String },
}

pub fn scenario_fixture(scenario: Scenario, scope: Scope) -> ScenarioFixture {
    let healthy = matches!(scenario, Scenario::Healthy | Scenario::NullVerdict);
    let null_verdict = matches!(scenario, Scenario::NullVerdict);
    let decision = match scenario {
        Scenario::Rollback => Some("rollback"),
        Scenario::Keep | Scenario::DistinctIterations => Some("keep"),
        _ => None,
    };
    let result = match scenario {
        Scenario::Healthy => "healthy",
        Scenario::Rollback => "rolled_back",
        Scenario::Keep | Scenario::DistinctIterations => "kept",
        _ => "undetermined",
    };
    let verdict = if null_verdict {
        Value::Null
    } else {
        json!(if healthy { "pass" } else { "fail" })
    };
    let step_value = |outputs: Value| {
        json!({
            "status": if null_verdict { "skipped" } else { "succeeded" },
            "verdict": verdict, "outputs": outputs, "actor": null,
            "started_at": null, "finished_at": null, "attempts": 1,
            "amended": false, "stale": false,
        })
    };
    let regional_rates = if healthy {
        [0.001, 0.002, 0.003]
    } else {
        [0.001, 0.04, 0.003]
    };
    let region_checks: Vec<_> = regional_rates
        .into_iter()
        .enumerate()
        .map(|(index, rate)| {
            step_value(
                if index == 0 && matches!(scenario, Scenario::MissingOutput) {
                    json!({})
                } else {
                    json!({"rate": rate})
                },
            )
        })
        .collect();
    let first_rate = if healthy { 0.005 } else { 0.04 };
    let rechecks = [
        step_value(json!({"rate": first_rate})),
        step_value(json!({"rate": 0.005})),
    ];
    let mut regions = step_value(json!({}));
    regions["items"] = Value::Array(
        region_checks
            .iter()
            .map(|check| json!({"error_rate": check}))
            .collect(),
    );
    let mut stabilize = step_value(json!({}));
    stabilize["iterations"] = json!([{"recheck": rechecks[0]}, {"recheck": rechecks[1]}]);
    stabilize["satisfied"] = json!(!null_verdict);
    stabilize["exhausted"] = json!(false);
    let mut steps = serde_json::Map::new();
    for name in [
        "smoke",
        "login_flow",
        "decide",
        "rollback",
        "mark_major",
        "mark_healthy",
        "notify",
    ] {
        steps.insert(name.into(), step_value(json!({})));
    }
    steps.insert("api_health".into(), step_value(json!({"status_code": 200})));
    steps.insert("regions".into(), regions);
    steps.insert(
        "error_rate".into(),
        region_checks[if scope == Scope::Foreach { 1 } else { 2 }].clone(),
    );
    steps.insert("stabilize".into(), stabilize);
    steps.insert(
        "recheck".into(),
        rechecks[usize::from(scope != Scope::Loop)].clone(),
    );
    steps.insert("signoff".into(), step_value(decision.map_or_else(|| json!({}), |choice| json!({"decision": choice, "comment": "Reviewed checks", "decided_by": "user_oncall"}))));
    let mut values = json!({
        "inputs": {"service": "api", "version": "v1.2.3", "regions": ["us-west", "us-east", "eu-west"]},
        "fields": {"severity": if healthy { "none" } else { "major" }},
        "vars": {"result": result}, "project": {"vars": {}}, "steps": steps,
        "execution": {"id": "exe_flagship", "title": "Verify api v1.2.3", "url": "https://example.test/executions/exe_flagship", "created_by": "user_oncall", "tags": ["deploy", "production"], "started_at": 1794240000_i64},
    });
    match scope {
        Scope::Foreach => {
            values["region"] = json!("us-east");
            values["index"] = json!(1);
        }
        Scope::Loop => {
            values["iteration"] = json!(1);
        }
        Scope::Report => {
            values["outputs"] = json!({"result": result});
        }
        Scope::Global => {}
    }
    let root_origin = Origin {
        path: String::new(),
        record: RecordIdentity {
            execution_id: "exe_flagship".into(),
            step_run_id: None,
            iteration_path: vec![],
            revision: 1,
        },
    };
    let schema = scope_schema(scope);
    let roots = values
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| {
            let path = if name == "region" {
                "inputs.regions[1]"
            } else {
                name
            };
            (
                name.clone(),
                input_node(
                    value,
                    schema.roots[name].ty,
                    &schema,
                    path,
                    &root_origin.record,
                    scope,
                ),
            )
        })
        .collect();
    let missing_decision = || ExpectedResult::MissingValue {
        path: "steps.signoff.outputs.decision".into(),
    };
    let mut expected: BTreeMap<String, ExpectedResult> = [
        ("/spec/execution/title", json!("Verify api v1.2.3")),
        ("/spec/execution/concurrency/key", json!("api")),
        (
            "/spec/steps/0/parallel/0/instructions",
            json!("Call /healthz on api and record the status code."),
        ),
        (
            "/spec/steps/1/foreach/items",
            json!(["us-west", "us-east", "eu-west"]),
        ),
        (
            "/spec/steps/1/foreach/steps/0/title",
            json!("Error rate in us-east"),
        ),
        (
            "/spec/steps/1/foreach/steps/0/with/region",
            json!("us-east"),
        ),
        ("/spec/steps/2/if", json!(!healthy)),
        (
            "/spec/steps/2/loop/until",
            json!(scope != Scope::Loop || healthy),
        ),
        ("/spec/steps/3/switch/cases/0/when", json!(!healthy)),
        (
            "/spec/steps/3/switch/cases/0/steps/0/approval/prompt",
            json!("Checks failed for v1.2.3. Choose an action."),
        ),
        (
            "/spec/steps/3/switch/cases/0/steps/1/title",
            json!("Roll back api"),
        ),
        ("/spec/outputs/result", json!(result)),
    ]
    .into_iter()
    .map(|(pointer, value)| (pointer.into(), ExpectedResult::Value(value)))
    .collect();
    expected.insert(
        "/spec/steps/3/switch/cases/0/steps/1/if".into(),
        decision.map_or_else(missing_decision, |choice| {
            ExpectedResult::Value(json!(choice == "rollback"))
        }),
    );
    expected.insert(
        "/spec/steps/3/switch/cases/0/steps/2/set/vars/result".into(),
        decision.map_or_else(missing_decision, |choice| {
            ExpectedResult::Value(json!(if choice == "rollback" {
                "rolled_back"
            } else {
                "kept"
            }))
        }),
    );
    if matches!(scenario, Scenario::MissingOutput) {
        expected.insert(
            "/spec/steps/2/if".into(),
            ExpectedResult::MissingValue {
                path: "steps.regions.items[0].error_rate.outputs.rate".into(),
            },
        );
    }
    // Only expectations executable in this snapshot's lexical scope belong here.
    expected.retain(|pointer, _| {
        let site_scope = match pointer.as_str() {
            "/spec/steps/1/foreach/steps/0/title" | "/spec/steps/1/foreach/steps/0/with/region" => {
                Scope::Foreach
            }
            "/spec/steps/2/loop/until" => Scope::Loop,
            _ => Scope::Global,
        };
        site_scope == scope
    });
    if scope == Scope::Report {
        expected.insert(
            "/companion/report".into(),
            ExpectedResult::Value(json!(format!("Result: {result}"))),
        );
    }
    ScenarioFixture {
        roots,
        root_origin,
        event_time: EventTime {
            unix_seconds: 1794240000,
            nanos: 123_000_000,
        },
        expected,
    }
}

// Bounded, trusted test data conversion by the explicitly declared type. Runtime
// numbers are not guessed from their JSON spelling (rate stays Double).
fn input_node(
    value: &Value,
    ty: TypeId,
    schema: &SchemaDef,
    path: &str,
    parent: &RecordIdentity,
    scope: Scope,
) -> InputNode {
    let mut identity = parent.clone();
    if let Some(name) = path
        .strip_prefix("steps.")
        .filter(|name| !name.contains(['.', '[']))
    {
        identity.step_run_id = Some(format!("sr_{name}"));
        identity.revision = 7;
    }
    for (node_id, child, count) in [("regions", "error_rate", 3), ("stabilize", "recheck", 2)] {
        for index in 0..count {
            let collection = if node_id == "regions" {
                "items"
            } else {
                "iterations"
            };
            let selected = if child == "error_rate" {
                if scope == Scope::Foreach { 1 } else { 2 }
            } else {
                usize::from(scope != Scope::Loop)
            };
            if path == format!("steps.{node_id}.{collection}[{index}].{child}")
                || (path == format!("steps.{child}") && index == selected)
            {
                identity.step_run_id = Some(format!("sr_{child}_{index}"));
                identity.iteration_path = vec![IterationIdentity {
                    node_id: node_id.into(),
                    instance_id: format!("control_{node_id}"),
                    ordinal: index as u32,
                }];
                identity.revision = 7 + index as u64;
            }
        }
    }
    let input = if value.is_null() {
        InputValue::Null
    } else {
        match &schema.types[ty.0 as usize] {
            TypeDef::Bool => InputValue::Bool(value.as_bool().unwrap()),
            TypeDef::Int => InputValue::Int(value.as_i64().unwrap()),
            TypeDef::Double => InputValue::Double(value.as_f64().unwrap()),
            TypeDef::String => InputValue::String(value.as_str().unwrap().into()),
            TypeDef::Timestamp => InputValue::Timestamp(EventTime {
                unix_seconds: value.as_i64().unwrap(),
                nanos: 0,
            }),
            TypeDef::List(item) => InputValue::List(
                value
                    .as_array()
                    .unwrap()
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        input_node(
                            value,
                            *item,
                            schema,
                            &format!("{path}[{index}]"),
                            &identity,
                            scope,
                        )
                    })
                    .collect(),
            ),
            TypeDef::Record(fields) => InputValue::Record(
                value
                    .as_object()
                    .unwrap()
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            input_node(
                                value,
                                fields[name].ty,
                                schema,
                                &format!("{path}.{name}"),
                                &identity,
                                scope,
                            ),
                        )
                    })
                    .collect(),
            ),
        }
    };
    InputNode {
        value: input,
        origin: Origin {
            path: path.into(),
            record: identity,
        },
    }
}
