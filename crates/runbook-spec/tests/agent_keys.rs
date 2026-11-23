//! R0-3: runner agent keys (`agent.name`, `model`, `max_cost_usd`,
//! `max_tokens`, `network`, step `connections`, `spec.defaults.agent`),
//! the retired `agent.runner`, ceiling checks against the agent directory
//! and the destructive-reach rules (runner design §6.3, §6.5).
use runbook_model::{
    AgentCeiling, AgentCeilingCredential, AgentCeilingHost, AgentCeilingTool, AgentDirectoryEntry,
    AgentStatus, Delivery, RunnerLabel,
};
use runbook_spec::{
    CompileContext, Diagnostic, compile,
    ir::{AgentTarget, NodeKind, Task},
    parse, validate_policy,
};
use serde_json::{Value, json};

fn entry(name: &str, id: &str, tool_label: RunnerLabel) -> AgentDirectoryEntry {
    AgentDirectoryEntry {
        agent_id: id.into(),
        name: name.into(),
        project_id: "prj_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        group_id: "rg_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        status: AgentStatus::Active,
        version_id: format!("{id}_v1"),
        ceiling: AgentCeiling {
            profiles: vec!["fast".into(), "deep".into()],
            profile_models: Default::default(),
            max_cost_usd: 1.0,
            max_tokens: 200_000,
            timeout_s: 900,
            credentials: vec![
                AgentCeilingCredential {
                    id: "cred_dd".into(),
                    name: "datadog-readonly".into(),
                    delivery: vec![Delivery::Proxy],
                    label: RunnerLabel::Read,
                },
                AgentCeilingCredential {
                    id: "cred_gh".into(),
                    name: "github-write".into(),
                    delivery: vec![Delivery::Proxy],
                    label: RunnerLabel::Write,
                },
            ],
            tools: vec![AgentCeilingTool {
                server_id: "mcp_1".into(),
                name: "query-metrics".into(),
                label: tool_label,
            }],
            hosts: vec![AgentCeilingHost {
                host: "api.datadoghq.com".into(),
                label: RunnerLabel::Read,
            }],
        },
    }
}

fn context(entries: &[AgentDirectoryEntry]) -> CompileContext {
    CompileContext {
        teams: [(
            "ops".into(),
            "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        )]
        .into(),
        agents: Some(
            entries
                .iter()
                .map(|e| (e.name.clone(), e.clone()))
                .collect(),
        ),
        ..Default::default()
    }
}
fn readonly() -> CompileContext {
    context(&[entry("deploy-checker", "agt_1", RunnerLabel::Read)])
}

fn document(spec: Value) -> runbook_spec::Spec {
    parse(
        &json!({"apiVersion":"therunbook.app/v1","kind":"Runbook",
            "metadata":{"id":"test","title":"Test"},"spec":spec})
        .to_string(),
    )
    .unwrap()
}
fn steps(steps: Value) -> runbook_spec::Spec {
    document(json!({"steps": steps}))
}
fn errors(spec: &runbook_spec::Spec, ctx: &CompileContext) -> Vec<Diagnostic> {
    match compile(spec, ctx) {
        Ok(compiled) => validate_policy(&compiled),
        Err(e) => e,
    }
}
fn codes(d: &[Diagnostic]) -> Vec<&str> {
    d.iter().map(|e| e.code.as_str()).collect()
}
fn task(spec: &runbook_spec::Spec, ctx: &CompileContext, index: usize) -> Task {
    let compiled = compile(spec, ctx).unwrap_or_else(|e| panic!("{e:?}"));
    match &compiled.ir().steps[index].kind {
        NodeKind::Task(task) => (**task).clone(),
        other => panic!("{other:?}"),
    }
}
fn gate(id: &str) -> Value {
    json!({"id":id,"approval":{"prompt":"Go?","options":["go","stop"],"approvers":{"teams":["ops"]}}})
}

#[test]
fn agent_name_and_limits_parse() {
    let spec = steps(json!([{
        "id":"check","title":"Check","actor":"agent","connections":["datadog-readonly"],
        "agent":{"name":"deploy-checker","model":"deep","max_cost_usd":0.5,"max_tokens":1000,
                 "network":["api.datadoghq.com"],"hints":{"tools":["query-metrics"]}}
    }]));
    let t = task(&spec, &readonly(), 0);
    assert_eq!(
        t.agent_target,
        Some(AgentTarget::Runner {
            agent_id: "agt_1".into(),
            name: "deploy-checker".into(),
            group_id: "rg_01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        })
    );
    let limits = t.agent_limits.clone().unwrap();
    assert_eq!(limits.model.as_deref(), Some("deep"));
    assert_eq!(limits.max_cost_usd, Some(0.5));
    assert_eq!(limits.max_tokens, Some(1000));
    assert_eq!(limits.network, Some(vec!["api.datadoghq.com".to_owned()]));
    assert_eq!(t.connections, Some(vec!["cred_dd".to_owned()]));
    assert!(t.agent.tools.is_some(), "hints are kept");

    // `spec.defaults.agent` names the agent of every agent step.
    let spec = document(json!({
        "defaults":{"agent":{"name":"deploy-checker"}},
        "steps":[{"id":"a","title":"A","actor":"agent"},{"id":"h","title":"H","actor":"human"}]
    }));
    assert!(matches!(
        task(&spec, &readonly(), 0).agent_target,
        Some(AgentTarget::Runner { .. })
    ));
    assert_eq!(task(&spec, &readonly(), 1).agent_target, None);

    // A step without a name stays open to external agents, and its stored
    // definition is unchanged by the new keys.
    let spec = steps(json!([{"id":"a","title":"A","actor":"agent"}]));
    let t = task(&spec, &readonly(), 0);
    assert_eq!(t.agent_target, None);
    assert!(t.connections.is_none());

    // Limits are positive numbers; names are strings.
    for agent in [
        json!({"name":"deploy-checker","max_cost_usd":0}),
        json!({"name":"deploy-checker","max_cost_usd":-1}),
        json!({"name":"deploy-checker","max_tokens":0}),
        json!({"name":"deploy-checker","max_tokens":1.5}),
        json!({"name":7}),
        json!({"name":"deploy-checker","network":"api.datadoghq.com"}),
    ] {
        let spec = steps(json!([{"id":"a","title":"A","actor":"agent","agent":agent}]));
        assert!(!errors(&spec, &readonly()).is_empty(), "{agent}");
    }
    // Naming an agent on a person's step is an error.
    let spec =
        steps(json!([{"id":"a","title":"A","actor":"human","agent":{"name":"deploy-checker"}}]));
    assert_eq!(
        codes(&errors(&spec, &readonly())),
        ["spec.agent_requires_agent_actor"]
    );
    // An unknown agent fails publication.
    let spec = steps(json!([{"id":"a","title":"A","actor":"agent","agent":{"name":"ghost"}}]));
    let d = errors(&spec, &readonly());
    assert_eq!(codes(&d), ["spec.agent_unknown"]);
    assert_eq!(d[0].path, "/spec/steps/0/agent/name");
}

#[test]
fn agent_runner_key_is_retired_with_hint() {
    let spec = steps(json!([{"id":"a","title":"A","actor":"agent","agent":{"runner":"hosted"}}]));
    let d = errors(&spec, &readonly());
    assert_eq!(codes(&d), ["spec.retired_key"]);
    assert_eq!(d[0].path, "/spec/steps/0/agent/runner");
    assert!(
        d[0].message.as_deref().unwrap().contains("agent.name"),
        "{d:?}"
    );
}

#[test]
fn step_limits_above_ceiling_fail_publish() {
    for (agent, connections, what) in [
        (
            json!({"name":"deploy-checker","max_cost_usd":2}),
            json!([]),
            "max_cost_usd",
        ),
        (
            json!({"name":"deploy-checker","max_tokens":300000}),
            json!([]),
            "max_tokens",
        ),
        (
            json!({"name":"deploy-checker","network":["evil.example"]}),
            json!([]),
            "network",
        ),
        (
            json!({"name":"deploy-checker"}),
            json!(["prod-admin"]),
            "connections",
        ),
    ] {
        let spec = steps(
            json!([{"id":"a","title":"A","actor":"agent","agent":agent,"connections":connections}]),
        );
        let d = errors(&spec, &readonly());
        assert_eq!(codes(&d), ["spec.agent_outside_ceiling"], "{what}: {d:?}");
        assert!(d[0].path.contains(what), "{what}: {}", d[0].path);
    }
    // At the ceiling is fine.
    let spec = steps(json!([{"id":"a","title":"A","actor":"agent",
        "agent":{"name":"deploy-checker","max_cost_usd":1,"max_tokens":200000}}]));
    assert!(errors(&spec, &readonly()).is_empty());
}

#[test]
fn profile_outside_allowed_list_fails() {
    let spec = steps(
        json!([{"id":"a","title":"A","actor":"agent","agent":{"name":"deploy-checker","model":"huge"}}]),
    );
    let d = errors(&spec, &readonly());
    assert_eq!(codes(&d), ["spec.agent_outside_ceiling"]);
    assert_eq!(d[0].path, "/spec/steps/0/agent/model");
}

#[test]
fn destructive_reach_requires_destructive_flag() {
    let ctx = context(&[entry("fixer", "agt_2", RunnerLabel::Destructive)]);
    let spec = steps(json!([{"id":"a","title":"A","actor":"agent","agent":{"name":"fixer"}}]));
    let d = errors(&spec, &ctx);
    assert_eq!(codes(&d), ["spec.destructive_reach"]);
    let message = d[0].message.as_deref().unwrap();
    assert!(message.contains("query-metrics"), "{message}");
    // Write reach is not destructive.
    let spec =
        steps(json!([{"id":"a","title":"A","actor":"agent","agent":{"name":"deploy-checker"}}]));
    assert!(errors(&spec, &readonly()).is_empty());
    // Marked destructive, after a person's approval: publishes.
    let spec = steps(json!([gate("gate"),
        {"id":"a","title":"A","actor":"agent","destructive":true,"agent":{"name":"fixer"}}]));
    assert!(errors(&spec, &ctx).is_empty(), "{:?}", errors(&spec, &ctx));
}

#[test]
fn destructive_step_needs_dominating_human_approval() {
    let ctx = context(&[entry("fixer", "agt_2", RunnerLabel::Destructive)]);
    let fix = json!({"id":"fix","title":"Fix","actor":"agent","destructive":true,"agent":{"name":"fixer"}});
    // No approval at all.
    let d = errors(&steps(json!([fix.clone()])), &ctx);
    assert_eq!(codes(&d), ["spec.destructive_needs_approval"]);
    // An approval on one branch of a switch before the step.
    let switch = json!({"id":"route","switch":{
        "cases":[{"when":"${{ true }}","steps":[gate("gate")]}],
        "default":{"steps":[{"id":"skip","title":"Skip"}]}}});
    let d = errors(&steps(json!([switch, fix.clone()])), &ctx);
    assert_eq!(codes(&d), ["spec.destructive_needs_approval"]);
    // An approval before the switch dominates the step after it.
    let switch = json!({"id":"route","switch":{
        "cases":[{"when":"${{ true }}","steps":[{"id":"x","title":"X"}]}]}});
    assert!(errors(&steps(json!([gate("gate"), switch, fix.clone()])), &ctx).is_empty());
    // ...and a step nested in a group after it.
    assert!(
        errors(
            &steps(json!([gate("gate"), {"id":"g","steps":[fix.clone()]}])),
            &ctx
        )
        .is_empty()
    );
    // A guarded approval may be skipped.
    let mut guarded = gate("gate");
    guarded["if"] = json!("${{ false }}");
    let d = errors(&steps(json!([guarded, fix.clone()])), &ctx);
    assert_eq!(codes(&d), ["spec.destructive_needs_approval"]);
    // An approval in a parallel sibling branch does not dominate.
    let d = errors(
        &steps(json!([{"id":"p","parallel":[gate("gate"), fix.clone()]}])),
        &ctx,
    );
    assert_eq!(codes(&d), ["spec.destructive_needs_approval"]);
    // Agents may not decide it.
    let mut agents = gate("gate");
    agents["approval"]["allow_agents"] = json!(true);
    agents["approval"]["min"] = json!(1);
    let d = errors(&steps(json!([agents, fix.clone()])), &ctx);
    assert!(
        codes(&d).contains(&"spec.destructive_needs_approval"),
        "{d:?}"
    );
    // Top-level finally runs even before the approval.
    let spec = document(json!({"steps":[gate("gate")],"finally":[fix.clone()]}));
    assert_eq!(
        codes(&errors(&spec, &ctx)),
        ["spec.destructive_needs_approval"]
    );
    // External (non-runner) destructive steps keep today's rules.
    let spec = steps(json!([{"id":"fix","title":"Fix","actor":"agent","destructive":true}]));
    assert!(errors(&spec, &ctx).is_empty());
}

#[test]
fn resolution_survives_the_stored_snapshot() {
    let spec = document(json!({
        "defaults":{"agent":{"name":"deploy-checker"}},
        "steps":[{"id":"a","title":"A","actor":"agent","connections":["datadog-readonly"],
            "agent":{"model":"deep","max_cost_usd":0.5,"network":["api.datadoghq.com"]}}]
    }));
    let compiled = compile(&spec, &readonly()).unwrap();
    let restored = runbook_spec::restore(compiled.snapshot_bytes()).unwrap();
    assert_eq!(restored.ir(), compiled.ir());
    assert_eq!(restored.hash(), compiled.hash());
    let NodeKind::Task(task) = &restored.ir().steps[0].kind else {
        panic!()
    };
    assert!(matches!(
        task.agent_target,
        Some(AgentTarget::Runner { .. })
    ));
    assert_eq!(task.connections, Some(vec!["cred_dd".to_owned()]));
}

#[test]
fn offline_compile_leaves_names_unchecked() {
    // Without a directory (offline validation, the browser editor) the
    // name is not checked; publish always has one.
    let spec =
        steps(json!([{"id":"a","title":"A","actor":"agent","agent":{"name":"ghost","model":"x"}}]));
    let compiled = compile(&spec, &CompileContext::default()).unwrap();
    let NodeKind::Task(task) = &compiled.ir().steps[0].kind else {
        panic!()
    };
    assert_eq!(
        task.agent_target,
        Some(AgentTarget::Named {
            name: "ghost".into()
        })
    );
    // ...and such a result cannot be stored.
    assert!(runbook_spec::restore(compiled.snapshot_bytes()).is_err());
}

#[test]
fn external_agent_name_is_reserved() {
    // `external` keeps a step open to external agents even if the
    // directory somehow holds an agent of that name.
    let ctx = context(&[
        entry("external", "agt_x", RunnerLabel::Read),
        entry("deploy-checker", "agt_1", RunnerLabel::Read),
    ]);
    let spec = document(json!({
        "defaults":{"agent":{"name":"deploy-checker"}},
        "steps":[{"id":"a","title":"A","actor":"agent","agent":{"name":"external"}}]
    }));
    assert_eq!(
        task(&spec, &ctx, 0).agent_target,
        Some(AgentTarget::External)
    );
    // Runner-only keys need a runner agent.
    let spec = steps(
        json!([{"id":"a","title":"A","actor":"agent","agent":{"name":"external","model":"fast"}}]),
    );
    assert_eq!(codes(&errors(&spec, &ctx)), ["spec.agent_outside_ceiling"]);
    // The project's default agent applies to unnamed agent steps only.
    let mut ctx = readonly();
    ctx.default_agent = Some("deploy-checker".into());
    let spec = steps(json!([
        {"id":"a","title":"A","actor":"agent"},
        {"id":"b","title":"B","actor":"agent","agent":{"name":"external"}}
    ]));
    assert!(matches!(
        task(&spec, &ctx, 0).agent_target,
        Some(AgentTarget::Runner { .. })
    ));
    assert_eq!(
        task(&spec, &ctx, 1).agent_target,
        Some(AgentTarget::External)
    );
}

#[test]
fn empty_connections_narrow_destructive_credential_reach() {
    let mut agent = entry("deploy-checker", "agt_1", RunnerLabel::Read);
    agent.ceiling.credentials[0].label = RunnerLabel::Destructive;
    let ctx = context(&[agent]);
    let mut step = json!({"id":"a","title":"A","actor":"agent","agent":{"name":"deploy-checker"}});
    assert_eq!(
        codes(&errors(&steps(json!([step.clone()])), &ctx)),
        ["spec.destructive_reach"]
    );
    step["connections"] = json!([]);
    let compiled = compile(&steps(json!([step])), &ctx).unwrap();
    let restored = runbook_spec::restore(compiled.snapshot_bytes()).unwrap();
    let NodeKind::Task(task) = &restored.ir().steps[0].kind else {
        panic!()
    };
    assert_eq!(
        serde_json::to_value(task).unwrap()["connections"],
        json!([])
    );
}

#[test]
fn old_published_empty_connections_load_as_omitted() {
    // The previous compiler wrote this snapshot from source with connections: [].
    let restored =
        runbook_spec::restore(include_bytes!("fixtures/legacy-connections.snapshot")).unwrap();
    let NodeKind::Task(task) = &restored.ir().steps[0].kind else {
        panic!()
    };
    assert!(task.connections.is_none());
}

#[test]
fn publication_requires_explicit_destructive_approval_options_or_a_decision_guard() {
    let ctx = context(&[entry("fixer", "agt_2", RunnerLabel::Destructive)]);
    let mut fix = json!({"id":"fix","title":"Fix","actor":"agent","destructive":true,"agent":{"name":"fixer"}});
    let gate = gate("signoff");
    let diagnostics = |gate, fix| {
        let compiled = compile(&steps(json!([gate, fix])), &ctx).unwrap();
        runbook_spec::publication_approval_diagnostics(compiled.ir()).0
    };
    let errors = diagnostics(gate.clone(), fix.clone());
    assert_eq!(codes(&errors), ["spec.destructive_approval_options"]);
    let message = errors[0].message.as_deref().unwrap();
    assert!(message.contains("signoff") && message.contains("approves: [...]"));
    fix["if"] = json!("${{ true || steps.signoff.outputs.decision == 'go' }}");
    assert_eq!(
        codes(&diagnostics(gate.clone(), fix.clone())),
        ["spec.destructive_approval_options"]
    );
    fix["if"] = json!("${{ steps.signoff.outputs.decision == 'go' && true }}");
    assert!(diagnostics(gate, fix).is_empty());
}

#[test]
fn publication_warns_that_an_approval_before_a_loop_covers_every_iteration() {
    let ctx = context(&[entry("fixer", "agt_2", RunnerLabel::Destructive)]);
    let mut gate = gate("signoff");
    gate["approval"]["approves"] = json!(["go"]);
    let fix = json!({"id":"fix","title":"Fix","actor":"agent","destructive":true,"agent":{"name":"fixer"}});
    let looped =
        json!({"id":"repeat","loop":{"until":"${{ false }}","max_iterations":2,"steps":[fix]}});
    let compiled = compile(&steps(json!([gate, looped])), &ctx).unwrap();
    let (errors, warnings) = runbook_spec::publication_approval_diagnostics(compiled.ir());
    assert!(errors.is_empty());
    assert_eq!(codes(&warnings), ["spec.destructive_approval_loop"]);
}

#[test]
fn approval_agent_target_is_explicit_and_ceiling_checked() {
    let mut review = gate("review");
    review["approval"]["agent"] = json!({"name":"deploy-checker","after":"15m","model":"deep","max_cost_usd":0.5,"max_tokens":1000});
    assert_eq!(
        codes(&errors(&steps(json!([review])), &readonly())),
        ["spec.agent_requires_allow_agents"]
    );
    review["approval"]["allow_agents"] = json!(true);
    let spec = steps(json!([review]));
    let compiled = compile(&spec, &readonly()).unwrap();
    let compiled = runbook_spec::restore(compiled.snapshot_bytes()).unwrap();
    let NodeKind::Approval(approval) = &compiled.ir().steps[0].kind else {
        panic!("expected approval");
    };
    assert_eq!(approval.agent_after.unwrap().millis, 900_000);
    assert_eq!(
        serde_json::to_value(approval.agent_target.as_ref()).unwrap(),
        json!({"runner":{"agent_id":"agt_1","name":"deploy-checker","group_id":"rg_01ARZ3NDEKTSV4RRFFQ69G5FAV"}})
    );
    assert_eq!(
        serde_json::to_value(approval.agent_limits.as_ref()).unwrap(),
        json!({"model":"deep","max_cost_usd":0.5,"max_tokens":1000})
    );
    let destructive = steps(json!([review, {"id":"deploy","title":"Deploy","destructive":true}]));
    assert!(codes(&errors(&destructive, &readonly())).contains(&"spec.agent_approval_destructive"));
    review["approval"]["agent"]["max_tokens"] = json!(200001);
    assert_eq!(
        codes(&errors(&steps(json!([review])), &readonly())),
        ["spec.agent_outside_ceiling"]
    );
}
