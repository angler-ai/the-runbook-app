use runbook_spec::{CompileContext, Diagnostic, compile, parse, validate_policy};
use serde_json::{Value, json};
fn context() -> CompileContext {
    CompileContext {
        users: [(
            "sam".into(),
            "usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        )]
        .into(),
        teams: [(
            "ops".into(),
            "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        )]
        .into(),
        ..Default::default()
    }
}
fn source(steps: Value, finally: Value, owners: Value) -> runbook_spec::Spec {
    parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test","owners":owners},"spec":{"vars":{"items":[1]},"steps":steps,"finally":finally}}).to_string()).unwrap()
}
fn check(steps: Value) -> Vec<Diagnostic> {
    let c = compile(&source(steps, json!([]), json!([])), &context()).unwrap();
    validate_policy(&c)
}
fn gate(id: &str, auto: bool, agent: bool) -> Value {
    let mut v = json!({"id":id,"approval":{"prompt":"Go?","options":[{"id":"rollback","auto":true}],"approvers":{"users":["sam"]},"allow_agents":agent}});
    if auto {
        v["approval"]["timeout"] = json!("1m");
        v["approval"]["on_timeout"] = json!({"decide":"rollback"});
    }
    v
}
fn task(id: &str, opt_in: bool) -> Value {
    json!({"id":id,"title":"Task","destructive":true,"allow_auto_decision":opt_in})
}
fn codes(d: &[Diagnostic]) -> Vec<&str> {
    d.iter().map(|e| e.code.as_str()).collect()
}
#[test]
fn auto_option_requires_task_opt_in() {
    let d = check(json!([gate("gate",true,false),{"id":"group","steps":[task("rollback",false)]}]));
    assert_eq!(codes(&d), ["spec.auto_decision_destructive"]);
    assert_eq!(d[0].path, "/spec/steps/0/approval/on_timeout/decide");
    assert!(
        check(json!([gate("gate",true,false),{"id":"group","steps":[task("rollback",true)]}]))
            .is_empty()
    );
}
#[test]
fn agent_gate_cannot_reach_destructive_work() {
    let d = check(json!([gate("gate",false,true),{"id":"group","steps":[task("rollback",true)]}]));
    assert_eq!(codes(&d), ["spec.agent_approval_destructive"]);
    assert_eq!(d[0].path, "/spec/steps/0/approval/allow_agents");
}
#[test]
fn automatic_option_must_be_declared() {
    for options in [
        json!(["keep"]),
        json!(["rollback"]),
        json!([{"id":"rollback","auto":false}]),
    ] {
        let mut g = gate("gate", true, false);
        g["approval"]["options"] = options;
        assert_eq!(codes(&check(json!([g]))), ["spec.auto_option_required"]);
    }
}
#[test]
fn explicit_timeout_settings_require_timeout() {
    for (key, value) in [
        ("on_timeout", json!("escalate")),
        ("on_timeout", json!({"decide":"rollback"})),
        ("escalate_to", json!([])),
        ("escalate_to", json!(["team:ops"])),
        ("escalation_timeout", json!("1m")),
    ] {
        let mut g = gate("gate", false, false);
        g["approval"][key] = value;
        let errors = compile(&source(json!([g]), json!([]), json!([])), &context()).unwrap_err();
        assert_eq!(errors[0].code, "spec.on_timeout_requires_timeout");
        assert_eq!(errors[0].path, format!("/spec/steps/0/approval/{key}"));
    }
    assert!(check(json!([gate("gate", false, false)])).is_empty());
}

#[test]
fn automatic_policy_activation_is_explicit() {
    assert!(
        check(json!([
            gate("gate", false, false),
            task("destructive", false)
        ]))
        .is_empty()
    );
    let mut g = gate("gate", true, false);
    g["if"] = json!("${{ false }}");
    let mut t = task("destructive", false);
    t["if"] = json!("${{ false }}");
    assert_eq!(
        codes(&check(json!([g, t]))),
        ["spec.auto_decision_destructive"]
    );
}
#[test]
fn approval_recipients_and_escalation_fallback() {
    let mut g = gate("gate", false, false);
    g["approval"]["approvers"] = json!({});
    assert_eq!(codes(&check(json!([g]))), ["spec.approvers_required"]);
    let mut g = gate("gate", false, false);
    g["approval"]["timeout"] = json!("1m");
    assert_eq!(
        codes(&check(json!([g.clone()]))),
        ["spec.escalation_recipients_required"]
    );
    let c = compile(
        &source(json!([g.clone()]), json!([]), json!(["sam"])),
        &context(),
    )
    .unwrap();
    assert!(validate_policy(&c).is_empty());
    g["approval"]["escalate_to"] = json!([]);
    let c = compile(
        &source(json!([g.clone()]), json!([]), json!(["sam"])),
        &context(),
    )
    .unwrap();
    assert_eq!(
        codes(&validate_policy(&c)),
        ["spec.escalation_recipients_required"]
    );
    g["approval"]["escalate_to"] = json!(["team:ops"]);
    g["approval"]["min"] = json!(5);
    assert!(check(json!([g])).is_empty()); // Team count is not voter count.
}
#[test]
fn policy_follows_nested_continuations() {
    for steps in [
        json!([{"id":"group","steps":[gate("gate",true,false)]},task("later",false)]),
        json!([
            gate("gate", true, false),
            gate("human", false, false),
            task("later", false)
        ]),
        json!([gate("gate",true,false),{"id":"switch","switch":{"cases":[{"when":"${{ false }}","steps":[task("later",false)]}]}}]),
    ] {
        assert_eq!(codes(&check(steps)), ["spec.auto_decision_destructive"]);
    }
    let switch = json!({"id":"switch","switch":{"cases":[{"when":"${{ true }}","steps":[gate("gate",true,false)]},{"when":"${{ false }}","steps":[task("other_case",false)]}]}});
    assert!(check(json!([task("earlier", false), switch.clone()])).is_empty());
    assert_eq!(
        codes(&check(json!([switch, task("later", false)]))),
        ["spec.auto_decision_destructive"]
    );
}
#[test]
fn policy_follows_failure_and_cleanup() {
    let normal = json!({"id":"work","title":"Work"});
    let group = |work: Value| json!({"id":"group","steps":[gate("gate",true,false),work],"catch":[task("caught",false)]});
    assert_eq!(
        codes(&check(json!([group(normal.clone())]))),
        ["spec.auto_decision_destructive"]
    );
    let mut continuing = normal.clone();
    continuing["on_failure"] = json!("continue");
    assert!(check(json!([group(continuing)])).is_empty());
    let nested = json!({"id":"outer","steps":[{"id":"inner","steps":[normal],"catch":[gate("gate",true,false),{"id":"catch_work","title":"Catch work"}]}],"catch":[task("outer_catch",false)]});
    assert_eq!(
        codes(&check(json!([nested]))),
        ["spec.auto_decision_destructive"]
    );
    let c = compile(
        &source(
            json!([gate("gate", true, false)]),
            json!([task("top_cleanup", false)]),
            json!([]),
        ),
        &context(),
    )
    .unwrap();
    assert_eq!(
        codes(&validate_policy(&c)),
        ["spec.auto_decision_destructive"]
    );
}
#[test]
fn policy_respects_end_and_parallel_boundaries() {
    let end = json!({"id":"end","end":{"status":"succeeded"}});
    assert!(
        check(json!([
            gate("gate", true, false),
            end.clone(),
            task("later", false)
        ]))
        .is_empty()
    );
    let mut guarded = end.clone();
    guarded["if"] = json!("${{ true }}");
    assert_eq!(
        codes(&check(json!([
            gate("gate", true, false),
            guarded,
            task("later", false)
        ]))),
        ["spec.auto_decision_destructive"]
    );
    assert!(check(json!([gate("gate",true,false),end.clone(),{"id":"unentered","steps":[],"finally":[task("unentered_cleanup",false)]}])).is_empty());
    let branch = json!({"id":"group","steps":[gate("gate",true,false),end.clone()],"finally":[{"id":"cleanup","title":"Cleanup"}]});
    assert!(check(json!([branch.clone(), task("later", false)])).is_empty());
    let c = compile(
        &source(
            json!([branch]),
            json!([task("top_cleanup", false)]),
            json!([]),
        ),
        &context(),
    )
    .unwrap();
    assert_eq!(
        codes(&validate_policy(&c)),
        ["spec.auto_decision_destructive"]
    );
    let p = json!({"id":"parallel","parallel":[gate("gate",true,false),task("independent",false)],"fail_fast":true});
    assert!(check(json!([p.clone()])).is_empty());
    assert_eq!(
        codes(&check(json!([p, task("after_join", false)]))),
        ["spec.auto_decision_destructive"]
    );
    let p = json!({"id":"parallel","parallel":[gate("gate",true,false),end.clone()]});
    assert!(check(json!([p, task("unreachable", false)])).is_empty());
    let in_cleanup = json!({"id":"group","steps":[end],"finally":[gate("gate",true,false)]});
    assert!(check(json!([in_cleanup, task("unreachable", false)])).is_empty());
}
#[test]
fn policy_revisits_bounded_iteration_bodies() {
    // 50 is the largest loop bound publication admits (PRD §17, R52).
    for (maximum, bad) in [(1, false), (2, true), (50, true)] {
        let n = json!({"id":"repeat","loop":{"until":"${{ true }}","max_iterations":maximum,"steps":[task("earlier",false),gate("gate",true,false)]}});
        assert_eq!(
            codes(&check(json!([n]))),
            if bad {
                vec!["spec.auto_decision_destructive"]
            } else {
                vec![]
            }
        );
        let n = json!({"id":"each","foreach":{"as":"inner_item","items":"${{ vars.items }}","max_items":maximum,"steps":[task("earlier",false),gate("gate",true,false)]}});
        assert_eq!(
            codes(&check(json!([n]))),
            if bad {
                vec!["spec.auto_decision_destructive"]
            } else {
                vec![]
            }
        );
    }
    let loop_end = json!({"id":"repeat","loop":{"until":"${{ false }}","max_iterations":2,"steps":[task("earlier",false),gate("gate",true,false),{"id":"end","end":{"status":"succeeded"}}]}});
    assert!(check(json!([loop_end])).is_empty());
}

#[test]
fn handoff_requires_task_for_controls_in_nested_contexts() {
    let mut controls = vec![
        json!({"id":"group","steps":[]}),
        json!({"id":"parallel","parallel":[]}),
        json!({"id":"each","foreach":{"as":"inner_item","items":"${{ vars.items }}","max_items":1,"steps":[]}}),
        json!({"id":"repeat","loop":{"until":"${{ true }}","max_iterations":1,"steps":[]}}),
        json!({"id":"switch","switch":{"cases":[]}}),
        json!({"id":"wait","wait":{"for":"1s"}}),
        json!({"id":"set","set":{"vars":{"items":[]}}}),
        json!({"id":"end","end":{"status":"succeeded"}}),
        gate("gate", false, false),
    ];
    for control in &mut controls {
        control["on_failure"] = json!("handoff");
        let contexts = [
            (json!([control]), json!([]), "/spec/steps/0"),
            (
                json!([{"id":"outer","steps":[control]}]),
                json!([]),
                "/spec/steps/0/steps/0",
            ),
            (
                json!([{"id":"outer","steps":[],"catch":[control]}]),
                json!([]),
                "/spec/steps/0/catch/0",
            ),
            (
                json!([{"id":"outer","steps":[],"finally":[control]}]),
                json!([]),
                "/spec/steps/0/finally/0",
            ),
            (
                json!([{"id":"outer","parallel":[control]}]),
                json!([]),
                "/spec/steps/0/parallel/0",
            ),
            (
                json!([{"id":"outer","foreach":{"items":"${{ vars.items }}","max_items":1,"steps":[control]}}]),
                json!([]),
                "/spec/steps/0/foreach/steps/0",
            ),
            (
                json!([{"id":"outer","loop":{"until":"${{ true }}","max_iterations":1,"steps":[control]}}]),
                json!([]),
                "/spec/steps/0/loop/steps/0",
            ),
            (
                json!([{"id":"outer","switch":{"cases":[{"when":"${{ true }}","steps":[control]}]}}]),
                json!([]),
                "/spec/steps/0/switch/cases/0/steps/0",
            ),
            (
                json!([{"id":"outer","switch":{"cases":[],"default":{"steps":[control]}}}]),
                json!([]),
                "/spec/steps/0/switch/default/steps/0",
            ),
            (json!([]), json!([control]), "/spec/finally/0"),
        ];
        for (steps, finally, path) in contexts {
            let compiled = compile(&source(steps, finally, json!([])), &context()).unwrap();
            let all = validate_policy(&compiled);
            let diagnostics: Vec<_> = all
                .into_iter()
                .filter(|d| d.code != "spec.end_in_finally")
                .collect();
            assert_eq!(
                codes(&diagnostics),
                ["spec.handoff_requires_task"],
                "{path}: {control}"
            );
            assert_eq!(diagnostics[0].path, format!("{path}/on_failure"));
        }
    }
}

#[test]
fn task_handoff_and_task_only_defaults_pass_publication_policy() {
    use runbook_spec::ir::FailurePolicy;
    let spec = parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{
        "defaults":{"on_failure":"handoff"},
        "steps":[{"id":"explicit","title":"Task","on_failure":"handoff"},{"id":"group","steps":[{"id":"inherited","title":"Task"}]},{"id":"continuing","steps":[],"on_failure":"continue"}]
    }}).to_string()).unwrap();
    let compiled = compile(&spec, &context()).unwrap();
    assert!(validate_policy(&compiled).is_empty());
    assert_eq!(compiled.ir().steps[0].on_failure, FailurePolicy::Handoff);
    assert_eq!(compiled.ir().steps[1].on_failure, FailurePolicy::Fail);
    let runbook_spec::ir::NodeKind::Group(group) = &compiled.ir().steps[1].kind else {
        panic!("expected group")
    };
    assert_eq!(group.children[0].on_failure, FailurePolicy::Handoff);
    assert_eq!(compiled.ir().steps[2].on_failure, FailurePolicy::Continue);
}

#[test]
fn unchanged_flagship_compiles_and_passes_publication_policy() {
    let mut ctx = context();
    ctx.skills = [
        ("./skills/check-error-rates".into(), [1; 32]),
        ("./skills/rollback-service".into(), [2; 32]),
    ]
    .into();
    ctx.teams = [
        (
            "oncall".into(),
            "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        ),
        (
            "eng-leads".into(),
            "team_01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap(),
        ),
    ]
    .into();
    let spec = parse(include_str!("fixtures/post-deploy.yaml")).unwrap();
    let compiled = compile(&spec, &ctx).unwrap();
    assert!(validate_policy(&compiled).is_empty());
}

#[test]
fn agent_approval_requires_min_one() {
    let mut g = gate("gate", false, true);
    g["approval"]["min"] = json!(2);
    assert_eq!(
        codes(&check(json!([g]))),
        ["spec.agent_approval_requires_min_one"]
    );
    assert!(check(json!([gate("gate", false, true)])).is_empty());
}

#[test]
fn timeout_requires_task() {
    for mut node in [
        json!({"id":"group","steps":[]}),
        json!({"id":"parallel","parallel":[]}),
        json!({"id":"each","foreach":{"items":"${{ vars.items }}","max_items":2,"steps":[]}}),
        json!({"id":"repeat","loop":{"until":"${{ true }}","max_iterations":1,"steps":[]}}),
        json!({"id":"switch","switch":{"cases":[]}}),
        json!({"id":"wait","wait":{"for":"1s"}}),
        gate("gate", false, false),
        json!({"id":"set","set":{"vars":{"items":"${{ vars.items }}"}}}),
        json!({"id":"end","end":{"status":"succeeded"}}),
    ] {
        node["timeout"] = json!("1m");
        let d = check(json!([node]));
        assert_eq!(codes(&d), ["spec.timeout_requires_task"]);
        assert_eq!(d[0].path, "/spec/steps/0/timeout");
    }
    assert!(check(json!([{"id":"work","title":"Work","timeout":"1m"}])).is_empty());
}

#[test]
fn end_in_finally_is_rejected_at_publish() {
    let c = compile(
        &source(
            json!([]),
            json!([{"id":"wrap","steps":[{"id":"stop","end":{"status":"succeeded"}}]}]),
            json!([]),
        ),
        &context(),
    )
    .unwrap();
    let d = validate_policy(&c);
    assert_eq!(codes(&d), ["spec.end_in_finally"]);
    assert_eq!(d[0].path, "/spec/finally/0/steps/0/end");
    assert!(check(json!([{"id":"wrap","steps":[{"id":"work","title":"Work"}],"catch":[{"id":"stop","end":{"status":"succeeded"}}]}])).is_empty());
}
