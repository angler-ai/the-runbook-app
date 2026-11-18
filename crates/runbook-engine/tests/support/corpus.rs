//! Shared native/Wasm execution traces; assertions run in both targets.
use crate::support::*;
use serde_json::{Value, json};
pub fn rows() -> Value {
    let mut failure = succeeded("a");
    let Input::Report { status, .. } = &mut failure else {
        unreachable!()
    };
    *status = runbook_model::ReportStatus::Failed;
    let mut high = succeeded("repeat#1.check");
    let Input::Report {
        outputs, verdict, ..
    } = &mut high
    else {
        unreachable!()
    };
    outputs.insert("rate".into(), json!(0.1));
    *verdict = Some(runbook_model::Verdict::Fail);
    let mut low = succeeded("repeat#2.check");
    let Input::Report {
        outputs, verdict, ..
    } = &mut low
    else {
        unreachable!()
    };
    outputs.insert("rate".into(), json!(0.0));
    *verdict = Some(runbook_model::Verdict::Pass);
    let cases = [
        (
            "wait_duration",
            "spec: {steps: [{id: gate, wait: {for: 5m}}, {id: a, title: A}]}",
            vec![Input::Tick { at_ms: 300_000 }, succeeded("a")],
        ),
        (
            "wait_absolute",
            "spec: {steps: [{id: gate, wait: {until: '1970-01-01T00:00:00.000001Z'}}]}",
            vec![Input::Tick { at_ms: 1 }],
        ),
        (
            "wait_timeout",
            "spec: {steps: [{id: gate, wait: {signal: deploy, timeout: 5m}}]}",
            vec![Input::Tick { at_ms: 300_000 }],
        ),
        (
            "retry_success",
            "spec: {steps: [{id: a, title: A, retry: {max_attempts: 2, backoff: 1m}}]}",
            vec![
                failure.clone(),
                Input::Tick { at_ms: 60_000 },
                succeeded("a"),
            ],
        ),
        (
            "handoff_unresolved",
            "spec: {steps: [{id: a, title: A, on_failure: handoff}]}",
            vec![failure.clone()],
        ),
        (
            "group_catch",
            "spec: {steps: [{id: group, steps: [{id: a, title: A}], catch: [{id: recover, title: Recover}]}], outputs: {result: '${{ steps.a.status }}'}}",
            vec![failure.clone(), succeeded("recover")],
        ),
        (
            "continued_failure",
            "spec: {steps: [{id: a, title: A, on_failure: continue}, {id: b, title: B}]}",
            vec![failure.clone(), succeeded("b")],
        ),
        (
            "loop_recovery",
            "spec: {steps: [{id: repeat, loop: {max_iterations: 2, until: '${{ steps.check.outputs.rate <= 0.01 }}', steps: [{id: check, title: Check, check: true, outputs: {rate: {type: number, required: true}}}]}}], outputs: {result: '${{ steps.repeat.satisfied }}', finding: '${{ steps.repeat.verdict }}'}}",
            vec![high, low],
        ),
        (
            "loop_delay",
            "spec: {steps: [{id: repeat, loop: {max_iterations: 2, delay: 5m, until: '${{ iteration == 2 }}', steps: [{id: a, title: A}]}}]}",
            vec![
                succeeded("repeat#1.a"),
                Input::Tick { at_ms: 300_000 },
                succeeded("repeat#2.a"),
            ],
        ),
        (
            "loop_exhaustion",
            "spec: {steps: [{id: repeat, loop: {max_iterations: 2, until: '${{ false }}', steps: []}}], outputs: {result: '${{ steps.repeat.exhausted }}'}}",
            vec![],
        ),
        (
            "foreach",
            "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, west, eu]}}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 3, concurrency: 2, steps: [{id: a, title: A}]}}]}",
            vec![
                succeeded("each[west].a"),
                succeeded("each[east].a"),
                succeeded("each[eu].a"),
            ],
        ),
        (
            "foreach_duplicate",
            "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, east]}}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 2, steps: [{id: a, title: A}]}}]}",
            vec![],
        ),
        (
            "foreach_scope",
            "spec: {inputs: {items: {type: list, items: {type: string}, default: [east, west]}}, vars: {seen: none}, steps: [{id: each, foreach: {items: '${{ inputs.items }}', key: '${{ item }}', max_items: 2, steps: [{id: save, set: {vars: {seen: '${{ item }}'}}}]}}], outputs: {result: '${{ vars.seen }}', last_status: '${{ steps.save.status }}'}}",
            vec![],
        ),
        (
            "parallel",
            "spec: {steps: [{id: both, parallel: [{id: a, title: A}, {id: b, title: B}]}]}",
            vec![succeeded("b"), succeeded("a")],
        ),
        (
            "fail_fast",
            "spec: {steps: [{id: both, fail_fast: true, parallel: [{id: a, title: A}, {id: b, title: B}]}]}",
            vec![failure.clone()],
        ),
        (
            "sequential",
            "spec: {steps: [{id: a, title: A}]}",
            vec![succeeded("a")],
        ),
        (
            "atomic",
            "spec: {fields: {severity: {type: string, default: minor}}, vars: {result: healthy}, steps: [{id: change, set: {fields: {severity: major}, vars: {result: '${{ fields.severity }}'}}}], outputs: {result: '${{ vars.result }}'}}",
            vec![],
        ),
        (
            "initializer",
            "spec: {vars: {b: '${{ vars.a }}', a: seed}, steps: [], outputs: {result: '${{ vars.b }}'}}",
            vec![],
        ),
        (
            "failed_assignment",
            "spec: {inputs: {missing: {type: string}}, vars: {result: before}, steps: [{id: change, set: {vars: {result: '${{ inputs.missing }}'}}}]}",
            vec![],
        ),
        (
            "typed",
            "spec: {inputs: {n: {type: integer, default: 1.0}, d: {type: number, default: 1}}, vars: {v: true}, steps: [], outputs: {integer: '${{ inputs.n }}', double: '${{ inputs.d }}', result: '${{ vars.v }}'}}",
            vec![],
        ),
        (
            "nullable",
            "spec: {steps: [{id: a, title: A}], outputs: {result: '${{ steps.a.verdict }}'}}",
            vec![succeeded("a")],
        ),
        (
            "guard",
            "spec: {steps: [{id: skipped, title: Skip, if: '${{ false }}'}, {id: a, title: A}]}",
            vec![succeeded("a")],
        ),
        (
            "switch",
            "spec: {inputs: {first: {type: boolean, default: true}, missing: {type: boolean}}, steps: [{id: choose, switch: {cases: [{when: '${{ inputs.first }}', steps: [{id: a, title: A}]}, {when: '${{ inputs.missing }}', steps: [{id: never, title: Never}]}]}}]}",
            vec![succeeded("a")],
        ),
    ];
    let mut rows = vec![];
    for (name, spec, inputs) in cases {
        let trace = run_case(spec, &inputs);
        assert_eq!(trace.state, trace.replay());
        if name == "failed_assignment" {
            assert_eq!(trace.error_codes, ["expression.missing_value"]);
            assert_eq!(trace.count("DataChanged"), 0);
        } else if name == "foreach_duplicate" {
            assert_eq!(trace.error_codes, ["foreach.duplicate_key"]);
            assert_eq!(trace.count("ExecutionFaulted"), 1);
            assert_eq!(trace.count("StepReady"), 0);
        } else {
            assert!(trace.error_codes.is_empty());
            assert_eq!(
                trace.state.status,
                if name == "handoff_unresolved" {
                    runbook_model::ExecutionStatus::Running
                } else if matches!(name, "fail_fast" | "wait_timeout") {
                    runbook_model::ExecutionStatus::Failed
                } else {
                    runbook_model::ExecutionStatus::Succeeded
                }
            );
        }
        match name {
            "retry_success" => {
                assert_eq!(trace.count("RetryScheduled"), 1);
                assert_eq!(trace.step("a").attempt_history.len(), 2);
            }
            "handoff_unresolved" => {
                assert_eq!(trace.count("StepHandedOff"), 1);
                assert_eq!(
                    trace.step("a").routing_error.as_deref(),
                    Some("handoff.no_recipient")
                );
            }
            "group_catch" => {
                assert_eq!(trace.state.result, json!("failed"));
                assert_eq!(trace.count("CatchActivated"), 1);
            }
            "continued_failure" => {
                assert_eq!(trace.step("a").status, runbook_model::StepStatus::Failed)
            }
            "loop_recovery" => {
                assert_eq!(trace.state.result, json!(true));
                assert_eq!(trace.state.outputs["finding"], json!("fail"));
            }
            "loop_delay" => assert_eq!(trace.count("TimerFired"), 1),
            "loop_exhaustion" => {
                assert_eq!(trace.state.result, json!(true));
                assert_eq!(
                    trace.step("repeat").verdict,
                    Some(runbook_model::Verdict::Fail)
                );
            }
            "foreach_scope" => {
                assert_eq!(trace.state.result, json!("west"));
                assert_eq!(trace.state.outputs["last_status"], json!("succeeded"));
                assert!(
                    trace
                        .state
                        .output_reads
                        .iter()
                        .any(|r| r.origin.path == "steps.save.status"
                            && r.origin.record.iteration_path[0].ordinal == 1)
                );
            }
            "fail_fast" => {
                assert_eq!(trace.count("SiblingsCancelled"), 1);
                assert_eq!(trace.step("b").status, runbook_model::StepStatus::Cancelled);
            }
            "guard" => assert_eq!(trace.count("StepSkipped"), 1),
            "switch" => {
                assert_eq!(trace.count("BranchSelected"), 1);
                assert!(trace.step("never").id.is_none());
            }
            "atomic" => assert_eq!(trace.state.result, json!("minor")),
            "initializer" => assert_eq!(trace.state.result, json!("seed")),
            "typed" => {
                assert_eq!(trace.state.result, json!(true));
                assert_eq!(trace.state.outputs["double"].to_string(), "1.0");
            }
            _ => (),
        }
        rows.push(json!({"name":name,"events":trace.events,"commands":trace.commands,"errors":trace.error_codes,"result":trace.state.result,"outputs":trace.state.outputs,"status":trace.state.status}));
    }
    let mut context = runbook_spec::CompileContext::default();
    context.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test, owners: [user:alice]}\nspec: {steps: [{id: a, title: A, on_failure: handoff}]}",
        &[failure, succeeded("a")],
        context,
    );
    assert_eq!(trace.count("StepHandedOff"), 1);
    assert_eq!(trace.count("StepBlockedNotificationQueued"), 1);
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.state, trace.replay());
    rows.push(json!({"name":"handoff_resolved","events":trace.events,"commands":trace.commands,"errors":trace.error_codes,"result":trace.state.result,"outputs":trace.state.outputs,"status":trace.state.status}));
    for (name, choices) in [
        ("approval_quorum", ["keep", "keep"]),
        ("approval_split", ["keep", "rollback"]),
    ] {
        let mut context = runbook_spec::CompileContext::default();
        context.teams.insert(
            "oncall".into(),
            "team_00000000000000000000000001".parse().unwrap(),
        );
        let mut trace = run_document(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [keep, rollback], approvers: {teams: [oncall]}, min: 2}}]}",
            &[],
            context,
        );
        for (index, option) in choices.iter().enumerate() {
            let vote = runbook_engine::ResolvedVote {
                at_ms: 10 + index as i64,
                base_sequence: trace.state.sequence,
                step_run_id: trace.step("gate").id.clone().unwrap(),
                actor: runbook_model::ActorRef::Human {
                    user_id: format!("usr_{:026}", index + 1).parse().unwrap(),
                },
                credential_ref: format!("pat_{index}"),
                option: (*option).into(),
                comment: None,
                eligibility: runbook_engine::VoteEligibility {
                    directory_revision: index as u64 + 1,
                    eligible: true,
                    matched_recipients: vec![runbook_model::RecipientRef::Team(
                        "team_00000000000000000000000001".into(),
                    )],
                },
            };
            for event in runbook_engine::apply_vote(&trace.state, &vote).unwrap() {
                trace.append(event, vote.actor.clone(), 10 + index as i64);
            }
            trace.settle();
        }
        assert_eq!(trace.state, trace.replay());
        assert_eq!(
            trace.count("ApprovalDecided"),
            usize::from(name == "approval_quorum")
        );
        assert!(trace.error_codes.is_empty());
        rows.push(json!({"name":name,"events":trace.events,"commands":trace.commands,"outputs":trace.state.outputs,"status":trace.state.status}));
    }
    let mut trace = run_case(
        "spec: {steps: [{id: a, wait: {signal: deploy}}, {id: b, wait: {signal: deploy}}]}",
        &[],
    );
    send_signal(&mut trace, "deploy", "first", 1);
    send_signal(&mut trace, "deploy", "first", 2);
    send_signal(&mut trace, "deploy", "second", 3);
    assert_eq!(trace.count("SignalReceived"), 2);
    assert!(trace.error_codes.is_empty());
    assert_eq!(
        trace.state.status,
        runbook_model::ExecutionStatus::Succeeded
    );
    assert_eq!(trace.state, trace.replay());
    rows.push(json!({"name":"signal_once","events":trace.events,"commands":trace.commands,"status":trace.state.status}));
    for name in [
        "approval_expiry",
        "approval_late_vote",
        "execution_deadline",
    ] {
        let mut directory = runbook_spec::CompileContext::default();
        directory.teams.insert(
            "oncall".into(),
            "team_00000000000000000000000001".parse().unwrap(),
        );
        directory.teams.insert(
            "leads".into(),
            "team_00000000000000000000000002".parse().unwrap(),
        );
        let mut trace = run_document(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test, owners: [team:leads]}\nspec: {execution: {timeout: 2h}, steps: [{id: gate, approval: {prompt: Proceed?, options: [rollback], approvers: {teams: [oncall]}, timeout: 30m, escalation_timeout: 30m}}, {id: rollback, title: Rollback}]}",
            &[Input::Tick { at_ms: 3_600_000 }],
            directory,
        );
        if name == "approval_late_vote" {
            let vote = runbook_engine::ResolvedVote {
                at_ms: 5_400_000,
                base_sequence: trace.state.sequence,
                step_run_id: trace.step("gate").id.clone().unwrap(),
                actor: runbook_model::ActorRef::Human {
                    user_id: "usr_00000000000000000000000003".parse().unwrap(),
                },
                credential_ref: "pat_carol".into(),
                option: "rollback".into(),
                comment: Some("late review".into()),
                eligibility: runbook_engine::VoteEligibility {
                    eligible: true,
                    directory_revision: 2,
                    matched_recipients: vec![runbook_model::RecipientRef::Team(
                        "team_00000000000000000000000002".into(),
                    )],
                },
            };
            for event in runbook_engine::apply_vote(&trace.state, &vote).unwrap() {
                trace.append(event, vote.actor.clone(), vote.at_ms);
            }
            trace.settle();
            assert_eq!(trace.ready, ["rollback"]);
        } else if name == "execution_deadline" {
            for command in runbook_engine::advance_time(&trace.state, 7_200_000).unwrap() {
                trace.commit_command(command, 7_200_000);
            }
            trace.settle();
            assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Failed);
        } else {
            assert_eq!(trace.state.status, runbook_model::ExecutionStatus::Paused);
        }
        assert!(trace.error_codes.is_empty());
        assert_eq!(trace.state, trace.replay());
        rows.push(json!({"name":name,"events":trace.events,"commands":trace.commands,"status":trace.state.status,"pauses":trace.state.pause_reasons,"reason":trace.state.terminal_reason}));
    }
    let mut directory = runbook_spec::CompileContext::default();
    directory.users.insert(
        "alice".into(),
        "usr_00000000000000000000000001".parse().unwrap(),
    );
    let trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Roll back?, options: [{id: rollback, auto: true}], approvers: {users: [alice]}, timeout: 30m, on_timeout: {decide: rollback}}}, {id: rollback, title: Rollback, destructive: true, allow_auto_decision: true}]}",
        &[Input::Tick { at_ms: 1_800_000 }],
        directory,
    );
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert!(
        trace
            .event_names()
            .windows(2)
            .any(|events| events == ["ApprovalExpired", "DecisionApplied"])
    );
    assert_eq!(trace.step("gate").outputs["decided_by"], "system");
    assert_eq!(trace.state, trace.replay());
    rows.push(json!({"name":"automatic_policy","events":trace.events,"commands":trace.commands,"gate_outputs":trace.step("gate").outputs,"status":trace.state.status}));
    for (name, source, inputs) in [
        (
            "cleanup_after_timeout",
            "spec: {execution: {timeout: 2h}, steps: [{id: work, title: Work}], finally: [{id: notify, title: Notify}]}",
            vec![Input::Tick { at_ms: 7_200_000 }, succeeded("notify")],
        ),
        (
            "nested_finally",
            "spec: {steps: [{id: group, steps: [{id: work, title: Work}], finally: [{id: clean, title: Clean}]}], finally: [{id: notify, title: Notify}]}",
            vec![succeeded("work"), succeeded("clean"), succeeded("notify")],
        ),
        (
            "end_runs_cleanup",
            "spec: {steps: [{id: stop, end: {status: cancelled, message: Stopped}}, {id: never, title: Never}], finally: [{id: notify, title: Notify}]}",
            vec![succeeded("notify")],
        ),
    ] {
        let trace = run_case(source, &inputs);
        assert!(trace.error_codes.is_empty(), "{:?}", trace.error_codes);
        assert_eq!(trace.state, trace.replay());
        rows.push(json!({"name":name,"events":trace.events,"commands":trace.commands,"status":trace.state.status,"count":trace.state.materialized_steps,"reason":trace.state.terminal_reason}));
    }
    let trace = run_case(
        "spec: {inputs: {region: {type: string, default: east}}, steps: [{id: work, title: Work, instructions: {default: shared, human: 'Inspect ${{ inputs.region }}', agent: 'Check ${{ inputs.region }}'}, checklist: ['Visit ${{ inputs.region }}'], with: {region: '${{ inputs.region }}', window: 10m, retry: false}}]}",
        &[],
    );
    let id = trace.step("work").id.as_ref().unwrap();
    rows.push(json!({"name":"audience_presentation","human":runbook_engine::render_step(&trace.state,id,runbook_model::ActorKind::Human).unwrap(),"agent":runbook_engine::render_step(&trace.state,id,runbook_model::ActorKind::Agent).unwrap()}));
    let mut trace = run_case(
        "spec: {steps: [{id: work, title: Work, checklist: [One, Two]}]}",
        &[],
    );
    let event = runbook_engine::update_checklist(
        &trace.state,
        trace.step("work").id.as_ref().unwrap(),
        &[true, false],
    )
    .unwrap();
    trace.append(event, runbook_model::ActorRef::System, 1);
    assert_eq!(trace.state, trace.replay());
    rows.push(json!({"name":"checklist_update","events":trace.events,"checklist":trace.step("work").checklist}));
    let trace = run_case(
        "spec: {steps: [], outputs: {result: rolled_back}, report: 'Result: ${{ outputs.result }}'}",
        &[],
    );
    let report = runbook_engine::render_report(&trace.state)
        .unwrap()
        .unwrap();
    assert_eq!(report.markdown, "Result: rolled_back");
    rows.push(json!({"name":"report_template","report":report,"events":trace.events}));
    let trace = run_case(
        "spec: {agents: {params: {repo: acme/api}}, steps: [{id: work, title: Work, actor: agent}]}",
        &[],
    );
    let mut projection = trace.initial.clone();
    let intents = trace
        .events
        .iter()
        .filter_map(|event| {
            projection = runbook_engine::apply(&projection, event).unwrap();
            runbook_engine::notification_for(event, &projection)
        })
        .filter(|intent| intent.kind == "agent.work_available")
        .collect::<Vec<_>>();
    assert_eq!(intents.len(), 1);
    rows.push(json!({"name":"notification_intents","intents":intents}));
    for (name, recovered) in [
        ("flagship_manual_rollback", false),
        ("flagship_recovered_loop", true),
    ] {
        let trace = super::support::flagship::run(recovered);
        assert_eq!(trace.state, trace.replay());
        rows.push(json!({"name":name,"trace":super::support::flagship::serialized(&trace)}));
    }
    let mut context = runbook_spec::CompileContext::default();
    context.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let mut trace = run_document(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec: {steps: [{id: gate, approval: {prompt: Proceed?, options: [keep], approvers: {teams: [oncall]}, min: 1, allow_agents: true}}]}",
        &[],
        context,
    );
    let fact = runbook_engine::AgentDecision {
        at_ms: 10,
        base_sequence: trace.state.sequence,
        step_run_id: trace.step("gate").id.clone().unwrap(),
        option: "keep".into(),
        comment: Some("checked".into()),
        principal: "agt_ops".into(),
        agent_name: "ops".into(),
        model: Some("model-a".into()),
        session_id: "session-1".into(),
    };
    for event in runbook_engine::apply_agent_decision(&trace.state, &fact).unwrap() {
        let restored = serde_json::from_value(serde_json::to_value(event).unwrap()).unwrap();
        trace.append(restored, fact.actor(), fact.at_ms);
    }
    trace.settle();
    assert!(trace.error_codes.is_empty());
    assert_eq!(trace.count("ApprovalDecided"), 0);
    assert_eq!(
        trace.step("gate").approval.as_ref().unwrap().distinct_votes,
        0
    );
    assert_eq!(trace.state, trace.replay());
    rows.push(json!({"name":"agent_decision","events":trace.events,"commands":trace.commands,"status":trace.state.status,"outputs":trace.step("gate").outputs}));
    Value::Array(rows)
}
