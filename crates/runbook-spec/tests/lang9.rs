//! LANG9: the language holds only what the server enforces, plus hints under
//! `agent.hints`. Hints are passed to the agent and never enforced; the older
//! `agent.tools` and `agent.commands` spellings stay accepted and compile to
//! the same hints.
use runbook_spec::capabilities::{Support, classify};
use runbook_spec::ir::NodeKind;
use runbook_spec::{CompileContext, compile, normalize, parse};
use serde_json::json;

fn compiled(source: &str) -> runbook_spec::CompiledRunbook {
    compile(&parse(source).unwrap(), &CompileContext::default()).unwrap()
}

#[test]
fn hints_compile_to_the_same_agent_hints_as_the_older_spelling() {
    let older = compiled(
        r#"
metadata: {id: hints}
spec:
  steps:
    - id: check
      agent:
        tools: [query-metrics]
        commands: [{command: kubectl get pods}]
"#,
    );
    let hinted = compiled(
        r#"
metadata: {id: hints}
spec:
  steps:
    - id: check
      agent:
        hints:
          tools: [query-metrics]
          commands: [{command: kubectl get pods}]
"#,
    );
    // The compiled form keeps the authored source, so the two documents
    // store differently (as any edit does); what they mean is identical.
    let NodeKind::Task(task) = &hinted.ir().steps[0].kind else {
        panic!("expected a task");
    };
    let NodeKind::Task(older_task) = &older.ir().steps[0].kind else {
        panic!("expected a task");
    };
    assert_eq!(task.agent, older_task.agent);
    assert_eq!(task.agent.tools, Some(json!(["query-metrics"])));
    assert_eq!(
        task.agent.commands,
        Some(json!([{"command": "kubectl get pods"}]))
    );
}

#[test]
fn a_hint_given_both_ways_is_refused_at_the_hint() {
    let spec = parse(
        r#"
metadata: {id: hints}
spec:
  steps:
    - id: check
      agent:
        tools: [a]
        hints: {tools: [b]}
"#,
    )
    .unwrap();
    let errors: Vec<_> = normalize(&spec)
        .expect_err("must not normalize")
        .into_iter()
        .map(|d| (d.code, d.path))
        .collect();
    assert_eq!(
        errors,
        vec![(
            "spec.duplicate_agent_hint".to_owned(),
            "/spec/steps/0/agent/hints/tools".to_owned()
        )]
    );
}

#[test]
fn different_hints_may_mix_spellings() {
    let both = compiled(
        r#"
metadata: {id: hints}
spec:
  steps:
    - id: check
      agent:
        tools: [query-metrics]
        hints: {commands: [{command: ls}]}
"#,
    );
    let NodeKind::Task(task) = &both.ir().steps[0].kind else {
        panic!("expected a task");
    };
    assert_eq!(task.agent.tools, Some(json!(["query-metrics"])));
    assert_eq!(task.agent.commands, Some(json!([{"command": "ls"}])));
}

#[test]
fn hints_are_preserved_advisory_data_never_capabilities() {
    for path in [
        "spec.steps[0].agent.hints.tools[0]",
        "spec.steps[0].agent.hints.commands[0].command",
    ] {
        assert!(
            matches!(classify(path, &json!("data")), Support::Preserved { reason } if !reason.is_empty()),
            "{path}"
        );
    }
    // Anything else under `hints` is an unknown key, not a silent hint.
    assert!(
        !matches!(
            classify("spec.steps[0].agent.hints.magic", &json!(true)),
            Support::Preserved { .. } | Support::Supported { .. }
        ),
        "an unknown hint must be refused"
    );
}

fn first_error(source: &str) -> (String, String) {
    let spec = parse(source).unwrap();
    let d = normalize(&spec)
        .expect_err("must not normalize")
        .into_iter()
        .next()
        .unwrap();
    (d.code, d.path)
}

#[test]
fn labels_compile_onto_any_step_and_change_the_hash_only_when_present() {
    let plain = compiled(
        r#"
metadata: {id: labels}
spec:
  steps:
    - id: connect
      instructions: Connect Shopify.
    - id: wait_for_shop
      wait: {signal: shopify_connected}
"#,
    );
    let labelled = compiled(
        r#"
metadata: {id: labels}
spec:
  steps:
    - id: connect
      instructions: Connect Shopify.
      labels: {audience: customer, surface: settings/sources}
    - id: wait_for_shop
      wait: {signal: shopify_connected}
      labels: {audience: internal}
"#,
    );
    let ir = labelled.ir();
    assert_eq!(ir.steps[0].labels["audience"], "customer");
    assert_eq!(ir.steps[0].labels["surface"], "settings/sources");
    assert_eq!(ir.steps[1].labels["audience"], "internal");
    assert!(plain.ir().steps.iter().all(|n| n.labels.is_empty()));
    assert_ne!(labelled.hash(), plain.hash());
    // A step without labels stores no `labels` key at all.
    let stored = serde_json::to_string(plain.ir()).unwrap();
    assert!(!stored.contains("\"labels\":"), "{stored}");
}

#[test]
fn invalid_labels_are_refused_at_the_offending_entry() {
    let cases = [
        ("{Audience: customer}", "/spec/steps/0/labels/Audience"),
        ("{audience: 1}", "/spec/steps/0/labels/audience"),
        ("{'-bad': x}", "/spec/steps/0/labels/-bad"),
    ];
    for (labels, at) in cases {
        let source = format!(
            "metadata: {{id: labels}}\nspec:\n  steps:\n    - id: s\n      labels: {labels}\n"
        );
        assert_eq!(
            first_error(&source),
            ("spec.invalid_labels".to_owned(), at.to_owned()),
            "{labels}"
        );
    }
    let many: Vec<String> = (0..17).map(|i| format!("k{i}: v")).collect();
    let source = format!(
        "metadata: {{id: labels}}\nspec:\n  steps:\n    - id: s\n      labels: {{{}}}\n",
        many.join(", ")
    );
    assert_eq!(
        first_error(&source),
        (
            "spec.invalid_labels".to_owned(),
            "/spec/steps/0/labels".to_owned()
        )
    );
}

/// A stored version is checked on restore by rebuilding its source from the
/// IR; labels and wait payload schemas must survive that.
#[test]
fn labels_and_wait_payload_schemas_survive_restore() {
    let compiled = compiled(
        r#"
metadata: {id: onboarding}
spec:
  steps:
    - id: connect
      wait:
        signal: shopify_connected
        outputs: {store_id: {type: string, required: true}}
      labels: {audience: customer}
    - id: welcome
      instructions: "Store ${{ steps.connect.outputs.store_id }} is connected."
      labels: {audience: internal, team: cs}
"#,
    );
    let restored = runbook_spec::restore(compiled.snapshot_bytes()).expect("restores");
    assert_eq!(restored.hash(), compiled.hash());
    assert_eq!(restored.ir().steps[0].labels["audience"], "customer");
    assert_eq!(restored.ir().steps[1].labels["team"], "cs");
    let NodeKind::Wait(wait) = &restored.ir().steps[0].kind else {
        panic!("expected a wait");
    };
    assert!(wait.outputs.is_some());
}

#[test]
fn wait_outputs_need_a_signal_wait() {
    assert_eq!(
        first_error(
            "metadata: {id: w}\nspec:\n  steps:\n    - id: pause\n      wait: {for: 5m, outputs: {x: {type: string}}}\n"
        ),
        (
            "spec.wait_outputs_requires_signal".to_owned(),
            "/spec/steps/0/wait/outputs".to_owned()
        )
    );
}

#[test]
fn a_wait_without_outputs_keeps_its_stored_form() {
    let c = compiled(
        "metadata: {id: w}\nspec:\n  steps:\n    - id: gate\n      wait: {signal: deploy}\n",
    );
    let stored = serde_json::to_string(c.ir()).unwrap();
    assert!(!stored.contains("\"outputs\":{\"kind\""), "{stored}");
    let NodeKind::Wait(wait) = &c.ir().steps[0].kind else {
        panic!("expected a wait");
    };
    assert!(wait.outputs.is_none());
}

#[test]
fn retry_on_and_catch_on_compile_and_survive_restore() {
    let compiled = compiled(
        r#"
metadata: {id: typed}
spec:
  defaults:
    retry: {max_attempts: 2, on: [timeout]}
  steps:
    - id: guarded
      steps:
        - {id: work, title: Work, retry: {max_attempts: 3, on: [timeout, failed]}}
      catch:
        - {id: recover, title: Recover}
      catch_on: [timeout]
"#,
    );
    let restored = runbook_spec::restore(compiled.snapshot_bytes()).expect("restores");
    assert_eq!(restored.hash(), compiled.hash());
    let NodeKind::Group(group) = &restored.ir().steps[0].kind else {
        panic!("expected a group");
    };
    assert_eq!(group.catch_on, vec![runbook_spec::ir::FailureType::Timeout]);
    let NodeKind::Task(task) = &group.children[0].kind else {
        panic!("expected a task");
    };
    assert_eq!(
        task.retry.on,
        vec![
            runbook_spec::ir::FailureType::Timeout,
            runbook_spec::ir::FailureType::Failed
        ]
    );
    let NodeKind::Task(recover) = &group.catch[0].kind else {
        panic!("expected a task");
    };
    assert_eq!(
        recover.retry.on,
        vec![runbook_spec::ir::FailureType::Timeout]
    );
}

#[test]
fn typed_failure_lists_are_checked() {
    let at = |source: &str| first_error(&format!("metadata: {{id: t}}\nspec:\n  steps:\n{source}"));
    assert_eq!(
        at("    - {id: g, steps: [{id: w, title: W}], catch_on: [timeout]}\n"),
        (
            "spec.catch_on_without_catch".to_owned(),
            "/spec/steps/0/catch_on".to_owned()
        )
    );
    assert_eq!(
        at("    - {id: w, title: W, retry: {max_attempts: 2, on: [lease_lost]}}\n"),
        (
            "spec.invalid_value".to_owned(),
            "/spec/steps/0/retry/on/0".to_owned()
        )
    );
    assert_eq!(
        at("    - {id: w, title: W, retry: {max_attempts: 2, on: [timeout, timeout]}}\n"),
        (
            "spec.invalid_value".to_owned(),
            "/spec/steps/0/retry/on/1".to_owned()
        )
    );
    assert_eq!(
        at("    - {id: w, title: W, retry: {max_attempts: 2, on: []}}\n"),
        (
            "spec.invalid_value".to_owned(),
            "/spec/steps/0/retry/on".to_owned()
        )
    );
}

#[test]
fn retry_and_groups_without_the_new_keys_store_nothing_new() {
    let c = compiled(
        "metadata: {id: t}\nspec:\n  steps:\n    - {id: g, steps: [{id: w, title: W, retry: {max_attempts: 2}}], catch: [{id: r, title: R}]}\n",
    );
    let stored = serde_json::to_string(c.ir()).unwrap();
    assert!(!stored.contains("\"catch_on\":"), "{stored}");
    assert!(!stored.contains("\"on\":"), "{stored}");
}

fn followup_context() -> CompileContext {
    let mut context = CompileContext::default();
    context.users.insert(
        "carol".into(),
        "usr_00000000000000000000000003".parse().unwrap(),
    );
    context.teams.insert(
        "leads".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    context
}

#[test]
fn follow_up_compiles_inherits_task_defaults_and_survives_restore() {
    let compiled = compile(
        &parse(
            r#"
metadata: {id: followup}
spec:
  defaults: {remind_after: 1d, escalate_after: 3d, escalate_to: [team:leads]}
  steps:
    - {id: inherits, title: Inherits}
    - {id: overrides, title: Overrides, remind_after: 2h, remind_every: 1h}
    - {id: shop, wait: {signal: connected}, remind_after: 1d, escalate_after: 2d, escalate_to: [user:carol]}
    - {id: gate, approval: {prompt: Go?, options: [go]}, remind_after: 4h}
"#,
        )
        .unwrap(),
        &followup_context(),
    )
    .unwrap();
    let steps = &compiled.ir().steps;
    let inherits = steps[0].followup.as_ref().unwrap();
    assert_eq!(inherits.remind_after.unwrap().millis, 86_400_000);
    assert_eq!(inherits.escalate_after.unwrap().millis, 3 * 86_400_000);
    let overrides = steps[1].followup.as_ref().unwrap();
    assert_eq!(overrides.remind_after.unwrap().millis, 7_200_000);
    assert_eq!(overrides.remind_every.unwrap().millis, 3_600_000);
    assert_eq!(
        overrides.escalate_after.unwrap().millis,
        3 * 86_400_000,
        "unset keys still inherit"
    );
    assert!(steps[2].followup.as_ref().unwrap().escalate_after.is_some());
    assert!(
        steps[3].followup.as_ref().unwrap().escalate_after.is_none(),
        "defaults reach tasks only"
    );
    let restored = runbook_spec::restore(compiled.snapshot_bytes()).expect("restores");
    assert_eq!(restored.hash(), compiled.hash());
    assert_eq!(restored.ir().steps, compiled.ir().steps);
}

#[test]
fn follow_up_is_checked() {
    let at = |step: &str| {
        first_error(&format!(
            "metadata: {{id: f}}\nspec:\n  steps:\n    - {step}\n"
        ))
    };
    let invalid = |path: &str| ("spec.followup_invalid".to_owned(), path.to_owned());
    assert_eq!(
        at("{id: w, title: W, remind_every: 1h}"),
        invalid("/spec/steps/0/remind_every")
    );
    assert_eq!(
        at("{id: w, title: W, remind_after: 0s}").1,
        "/spec/steps/0/remind_after"
    );
    assert_eq!(
        at("{id: w, title: W, escalate_after: 1h}"),
        invalid("/spec/steps/0/escalate_after")
    );
    assert_eq!(
        at("{id: w, title: W, escalate_to: [user:carol]}"),
        invalid("/spec/steps/0/escalate_to")
    );
    assert_eq!(
        at("{id: w, title: W, escalate_after: 1h, escalate_to: []}"),
        invalid("/spec/steps/0/escalate_to")
    );
    // Approvals escalate through `approval.escalate_to`, never these keys.
    let (code, path) = at("{id: g, approval: {prompt: Go?, options: [go]}, escalate_after: 1h}");
    assert_eq!(path, "/spec/steps/0/escalate_after", "{code}");
}

#[test]
fn steps_without_follow_up_store_nothing_new() {
    let c = compiled("metadata: {id: f}\nspec:\n  steps:\n    - {id: w, title: W}\n");
    let stored = serde_json::to_string(c.ir()).unwrap();
    assert!(!stored.contains("\"followup\":"), "{stored}");
}

#[test]
fn independent_approval_references_must_dominate_and_be_human_capable() {
    let gate =
        "{id: gate, approval: {prompt: Review?, options: [yes], exclude_completers_of: [work]}}";
    for steps in [
        format!("{gate}, {{id: work, actor: human}}"),
        format!("{{id: work, actor: agent}}, {gate}"),
        format!("{{id: other, actor: human}}, {gate}"),
        format!("{{id: work, actor: human, if: 'false'}}, {gate}"),
        format!("{{id: parallel_work, parallel: [{{id: work, actor: human}}, {gate}]}}"),
    ] {
        let source = format!("metadata: {{id: independent}}\nspec: {{steps: [{steps}]}}");
        let diagnostics = normalize(&parse(&source).unwrap()).unwrap_err();
        assert_eq!(
            diagnostics[0].code, "spec.invalid_excluded_completer",
            "{steps}"
        );
        assert!(diagnostics[0].path.ends_with("/exclude_completers_of/0"));
    }
}

#[test]
fn independent_approval_cannot_decide_on_timeout() {
    let (code, path) = first_error(
        "metadata: {id: independent}\nspec: {steps: [{id: work, actor: human}, {id: gate, approval: {prompt: Review?, options: [{id: keep, auto: true}], timeout: 1m, on_timeout: {decide: keep}, exclude_completers_of: [work]}}]}",
    );
    assert_eq!(code, "spec.independent_review_required");
    assert_eq!(path, "/spec/steps/1/approval/on_timeout");
}
