#[path = "support/flagship.rs"]
mod flagship;

use flagship::{FLAGSHIP, Scope, flagship_cases, flagship_sources, scope_schema};
use runbook_expr::{ResultType, TemplateMode, TypeEnv, check_expression, check_template};
use std::collections::BTreeMap;

// This pins every containing value, not just occurrences of an opening delimiter.
fn expected_sources() -> BTreeMap<String, String> {
    [
        ("/spec/execution/title", "Verify ${{ inputs.service }} ${{ inputs.version }}"),
        ("/spec/execution/concurrency/key", "${{ inputs.service }}"),
        ("/spec/steps/0/parallel/0/instructions", "Call /healthz on ${{ inputs.service }} and record the status code."),
        ("/spec/steps/1/foreach/items", "${{ inputs.regions }}"),
        ("/spec/steps/1/foreach/steps/0/title", "Error rate in ${{ region }}"),
        ("/spec/steps/1/foreach/steps/0/with/region", "${{ region }}"),
        ("/spec/steps/2/if", "${{ steps.regions.items.exists(i, i.error_rate.outputs.rate > 0.01) }}"),
        ("/spec/steps/2/loop/until", "${{ steps.recheck.outputs.rate <= 0.01 }}"),
        ("/spec/steps/3/switch/cases/0/when", "${{ steps.smoke.verdict == 'fail' || steps.regions.verdict == 'fail' || steps.stabilize.verdict == 'fail' }}"),
        ("/spec/steps/3/switch/cases/0/steps/0/approval/prompt", "Checks failed for ${{ inputs.version }}. Choose an action."),
        ("/spec/steps/3/switch/cases/0/steps/1/if", "${{ steps.signoff.outputs.decision == 'rollback' }}"),
        ("/spec/steps/3/switch/cases/0/steps/1/title", "Roll back ${{ inputs.service }}"),
        ("/spec/steps/3/switch/cases/0/steps/2/set/vars/result", "${{ steps.signoff.outputs.decision == 'rollback' ? 'rolled_back' : 'kept' }}"),
        ("/spec/outputs/result", "${{ vars.result }}"),
    ].into_iter().map(|(p, s)| (p.into(), s.into())).collect()
}

#[test]
fn all_flagship_sources_are_unchanged() {
    // Catch fixture simplification and omitted instructions/with/concurrency.
    assert!(FLAGSHIP.starts_with("apiVersion: therunbook.app/v1\nkind: Runbook\n"));
    assert_eq!(flagship_sources(), expected_sources());
    let cases = flagship_cases();
    assert_eq!(
        cases
            .iter()
            .map(|c| (c.pointer.clone(), c.source.clone()))
            .collect::<BTreeMap<_, _>>(),
        expected_sources()
    );
    assert_eq!(cases.len(), 14);
    assert_eq!(cases.iter().map(|c| c.expressions.len()).sum::<usize>(), 15);
    let title = cases
        .iter()
        .find(|c| c.pointer == "/spec/execution/title")
        .unwrap();
    assert_eq!(title.expressions, [" inputs.service ", " inputs.version "]);
}

#[test]
fn all_flagship_sites_compile_in_their_scopes() {
    // Catch a missing declared field, wrong list element shape, or a leaked alias.
    let cases = flagship_cases();
    assert_eq!(cases.len(), 14);
    for case in cases {
        let env = TypeEnv::new(&case.schema).unwrap();
        let template = check_template(&case.source, &env)
            .unwrap_or_else(|e| panic!("{}: {e:?}", case.pointer));
        let text = matches!(
            case.pointer.as_str(),
            "/spec/execution/title"
                | "/spec/steps/0/parallel/0/instructions"
                | "/spec/steps/1/foreach/steps/0/title"
                | "/spec/steps/3/switch/cases/0/steps/0/approval/prompt"
                | "/spec/steps/3/switch/cases/0/steps/1/title"
        );
        assert_eq!(
            template.mode(),
            if text {
                TemplateMode::Text
            } else {
                TemplateMode::WholeValue
            },
            "{}",
            case.pointer
        );
        for source in &case.expressions {
            let expression = check_expression(source, &env).unwrap();
            assert_eq!(expression.descriptor().source, *source);
            let expected = match case.pointer.as_str() {
                "/spec/steps/1/foreach/items" => ResultType::List(Box::new(ResultType::String)),
                "/spec/steps/2/if"
                | "/spec/steps/2/loop/until"
                | "/spec/steps/3/switch/cases/0/when"
                | "/spec/steps/3/switch/cases/0/steps/1/if" => ResultType::Bool,
                _ => ResultType::String,
            };
            assert_eq!(expression.result_type(), expected, "{}", case.pointer);
        }
        let expected_scope = match case.pointer.as_str() {
            "/spec/steps/1/foreach/steps/0/title" | "/spec/steps/1/foreach/steps/0/with/region" => {
                Scope::Foreach
            }
            "/spec/steps/2/loop/until" => Scope::Loop,
            _ => Scope::Global,
        };
        assert_eq!(case.scope, expected_scope);
    }
}

#[test]
fn companion_report_scope_is_closed() {
    for scope in [Scope::Global, Scope::Foreach, Scope::Loop, Scope::Report] {
        let env = TypeEnv::new(&scope_schema(scope)).unwrap();
        let report = check_template(flagship::COMPANION_REPORT, &env);
        if scope == Scope::Report {
            assert_eq!(report.unwrap().mode(), TemplateMode::Text);
            assert_eq!(
                check_expression("outputs.unknown", &env).unwrap_err().code,
                "expression.type_error"
            );
        } else {
            assert_eq!(report.unwrap_err().code, "expression.type_error");
        }
        for (alias, allowed) in [
            ("region", scope == Scope::Foreach),
            ("index", scope == Scope::Foreach),
            ("iteration", scope == Scope::Loop),
        ] {
            let checked = check_expression(alias, &env);
            if allowed {
                assert!(checked.is_ok(), "{scope:?}: {alias}");
            } else {
                assert_eq!(checked.unwrap_err().code, "expression.type_error");
            }
        }
        for source in [
            "steps.api_health.outputs.missing",
            "steps.login_flow.outputs.rate",
            "steps.regions.items[0].steps.error_rate",
            "steps.unknown.status",
            "project.vars.unknown",
        ] {
            assert_eq!(
                check_expression(source, &env).unwrap_err().code,
                "expression.type_error",
                "{scope:?}: {source}"
            );
        }
    }
}

#[test]
fn companion_step_shapes_preserve_types_and_nullable_metadata() {
    use runbook_expr::{FieldDecl, TypeDef};
    let schema = scope_schema(Scope::Global);
    let env = TypeEnv::new(&schema).unwrap();
    let TypeDef::Record(steps) = &schema.types[schema.roots["steps"].ty.0 as usize] else {
        panic!("steps record")
    };
    assert_eq!(
        steps.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "api_health",
            "decide",
            "error_rate",
            "login_flow",
            "mark_healthy",
            "mark_major",
            "notify",
            "recheck",
            "regions",
            "rollback",
            "signoff",
            "smoke",
            "stabilize"
        ]
    );
    for (name, declaration) in steps {
        let TypeDef::Record(fields) = &schema.types[declaration.ty.0 as usize] else {
            panic!("step record")
        };
        for metadata in ["verdict", "actor", "started_at", "finished_at"] {
            assert!(fields[metadata].nullable, "{name}.{metadata}");
            assert!(fields[metadata].required, "{name}.{metadata}");
        }
        for (metadata, ty) in [
            ("verdict", TypeDef::String),
            ("actor", TypeDef::String),
            ("started_at", TypeDef::Timestamp),
            ("finished_at", TypeDef::Timestamp),
        ] {
            assert_eq!(
                schema.types[fields[metadata].ty.0 as usize], ty,
                "{name}.{metadata}"
            );
        }
        let FieldDecl { ty, .. } = fields["outputs"];
        let TypeDef::Record(outputs) = &schema.types[ty.0 as usize] else {
            panic!("closed outputs")
        };
        for field in outputs.values() {
            assert!(!field.required && !field.nullable);
        }
        assert_eq!(
            check_expression(&format!("steps.{name}.verdict == null"), &env)
                .unwrap()
                .result_type(),
            ResultType::Bool
        );
    }
    for (source, expected) in [
        ("steps.api_health.outputs.status_code", ResultType::Int),
        ("steps.error_rate.outputs.rate", ResultType::Double),
        ("steps.recheck.outputs.rate", ResultType::Double),
        (
            "steps.regions.items[0].error_rate.outputs.rate",
            ResultType::Double,
        ),
        (
            "steps.stabilize.iterations[0].recheck.outputs.rate",
            ResultType::Double,
        ),
        ("steps.stabilize.satisfied", ResultType::Bool),
        ("steps.stabilize.exhausted", ResultType::Bool),
        ("steps.signoff.outputs.comment", ResultType::String),
        ("steps.signoff.outputs.decided_by", ResultType::String),
        ("steps.smoke.attempts", ResultType::Int),
        ("steps.smoke.amended", ResultType::Bool),
        ("steps.smoke.stale", ResultType::Bool),
        ("steps.smoke.status", ResultType::String),
        ("fields.severity", ResultType::String),
        (
            "execution.tags",
            ResultType::List(Box::new(ResultType::String)),
        ),
    ] {
        assert_eq!(
            check_expression(source, &env).unwrap().result_type(),
            expected,
            "{source}"
        );
    }
    for source in [
        "${{ steps.regions.items[0].error_rate }}",
        "${{ steps.stabilize.iterations }}",
        "${{ true ? steps.recheck.outputs : steps.error_rate.outputs }}",
    ] {
        assert_eq!(
            check_template(source, &env).unwrap().mode(),
            TemplateMode::WholeValue
        );
    }
}

#[test]
fn reusable_states_are_validated_data_with_distinct_iteration_records() {
    use flagship::{Scenario, scenario_fixture};
    use runbook_expr::{Bindings, InputValue};
    for scenario in Scenario::ALL {
        for scope in [Scope::Global, Scope::Foreach, Scope::Loop, Scope::Report] {
            let schema = scope_schema(scope);
            let env = TypeEnv::new(&schema).unwrap();
            let fixture = scenario_fixture(scenario, scope);
            assert_eq!(
                fixture.expected.len(),
                match scope {
                    Scope::Global => 11,
                    Scope::Foreach => 2,
                    Scope::Loop | Scope::Report => 1,
                },
                "{scenario:?}/{scope:?}"
            );
            Bindings::new(
                &env,
                &fixture.roots,
                &fixture.root_origin,
                fixture.event_time,
            )
            .unwrap_or_else(|e| panic!("{scenario:?}/{scope:?}: {e:?}"));
            let InputValue::Record(steps) = &fixture.roots["steps"].value else {
                panic!("steps")
            };
            let InputValue::Record(health) = &steps["api_health"].value else {
                panic!("health")
            };
            assert_eq!(
                health["status"].value,
                InputValue::String(
                    if matches!(scenario, Scenario::NullVerdict) {
                        "skipped"
                    } else {
                        "succeeded"
                    }
                    .into()
                )
            );
            let InputValue::Record(stabilize) = &steps["stabilize"].value else {
                panic!("stabilize")
            };
            let InputValue::List(iterations) = &stabilize["iterations"].value else {
                panic!("iterations")
            };
            assert_eq!(iterations.len(), 2);
            let InputValue::Record(first) = &iterations[0].value else {
                panic!("first iteration")
            };
            let InputValue::Record(last) = &iterations[1].value else {
                panic!("last iteration")
            };
            assert_ne!(
                first["recheck"].origin.record.step_run_id,
                last["recheck"].origin.record.step_run_id
            );
            assert_ne!(
                first["recheck"].origin.record.iteration_path,
                last["recheck"].origin.record.iteration_path
            );
            assert_eq!(
                steps["recheck"].origin.record,
                if scope == Scope::Loop {
                    first["recheck"].origin.record.clone()
                } else {
                    last["recheck"].origin.record.clone()
                }
            );
        }
    }
}
