//! UI-2: the outline carries enough detail for the graph's node cards and
//! side panel. The fixture is the complex review runbook the graph gallery
//! draws (`scripts/dev/review-runbooks/release-train.yaml`).
mod support;
use serde_json::{Value, json};
use support::*;

fn release_train() -> String {
    std::fs::read_to_string(repo().join("scripts/dev/review-runbooks/release-train.yaml")).unwrap()
}

fn outline() -> Value {
    let out = call("outline", &json!({"source": release_train()}));
    assert_eq!(out["ok"]["diagnostics"], json!([]), "{out}");
    out["ok"]["outline"].clone()
}

/// Find a node by id anywhere in the tree.
fn find<'a>(nodes: &'a Value, id: &str) -> Option<&'a Value> {
    for n in nodes.as_array()? {
        if n["id"] == id {
            return Some(n);
        }
        if let Some(found) = n.get("children").and_then(|c| find(c, id)) {
            return Some(found);
        }
        for b in n
            .get("branches")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(found) = find(&b["children"], id) {
                return Some(found);
            }
        }
    }
    None
}

fn node(id: &str) -> Value {
    let o = outline();
    find(&o["steps"], id)
        .or_else(|| find(&o["finally"], id))
        .unwrap_or_else(|| panic!("no node {id}"))
        .clone()
}

#[test]
fn the_web_release_train_fixture_is_the_compiler_output() {
    assert_fixture(
        "web/src/ui/graph/fixtures/release-train.outline.json",
        &outline(),
    );
}

#[test]
fn the_outline_carries_the_description() {
    assert_eq!(
        outline()["description"],
        "Ships a release across regions with canaries, agent checks, a risk-based approval path, a customer-facing go signal and cleanup."
    );
}

#[test]
fn tasks_carry_instructions_outputs_timeout_and_failure_policy() {
    let migrations = node("migrations");
    assert_eq!(
        migrations["instructions"],
        "Dry-run pending migrations against a replica and check every one is additive."
    );
    assert_eq!(
        migrations["outputs"],
        json!([{"name": "pending", "type": "integer", "required": true}])
    );
    // `defaults: { timeout: 20m, on_failure: handoff }` apply to tasks.
    assert_eq!(migrations["timeout"], "20m");
    assert_eq!(migrations["onFailure"], "handoff");
    assert_eq!(migrations["actor"], "agent");
}

#[test]
fn human_tasks_carry_assignee_and_checklist() {
    let status = node("status_page");
    assert_eq!(status["actor"], "human");
    assert_eq!(status["assignee"], "team:oncall");
    assert_eq!(
        status["checklist"],
        json!(["Maintenance closed", "Release notes linked"])
    );
}

#[test]
fn approvals_carry_prompt_options_approvers_and_timeouts() {
    let cab = node("cab_review");
    assert_eq!(
        cab["approval"],
        json!({
            "prompt": "Approve the high-risk release ${{ inputs.version }}?",
            "options": [{"id": "ship", "title": "ship"}, {"id": "hold", "title": "hold"}],
            "approvers": ["team:oncall"],
            "min": 1,
            "timeout": "4h",
            "onTimeout": "escalate",
            "escalateTo": ["team:customer-success"],
            "escalationTimeout": "1h"
        })
    );
}

#[test]
fn waits_name_their_signal_duration_or_time() {
    assert_eq!(
        node("customer_go")["wait"],
        json!({"kind": "signal", "signal": "release_email_sent", "timeout": "1d"})
    );
    let every = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/every-kind.yaml"),
    )
    .unwrap();
    let out = call("outline", &json!({"source": every}));
    let steps = &out["ok"]["outline"]["steps"];
    assert_eq!(
        find(steps, "pause").unwrap()["wait"],
        json!({"kind": "for", "duration": "1h30m"})
    );
    assert_eq!(
        find(steps, "until").unwrap()["wait"],
        json!({"kind": "until", "until": "2030-01-01T00:00:00Z"})
    );
}

#[test]
fn parallels_carry_their_join() {
    assert_eq!(node("canary")["join"], json!({"kind": "any"}));
    assert_eq!(node("announce")["join"], json!({"kind": "all"}));
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: j, title: J}\nspec:\n  steps:\n    - id: fan\n      join: {at_least: 2}\n      parallel: [{id: a, title: A}, {id: b, title: B}, {id: c, title: C}]\n";
    let out = call("outline", &json!({"source": source}));
    assert_eq!(
        out["ok"]["outline"]["steps"][0]["join"],
        json!({"kind": "at_least", "count": 2})
    );
}

#[test]
fn foreach_and_loop_carry_their_expressions() {
    assert_eq!(
        node("rollout")["foreach"],
        json!({"items": "${{ inputs.regions }}", "as": "region", "concurrency": 1, "maxItems": 6})
    );
    assert_eq!(
        node("settle")["loop"],
        json!({
            "until": "${{ steps.region_health.outputs.error_rate <= 0.01 }}",
            "maxIterations": 4,
            "delay": "20s"
        })
    );
}

/// UI-3 review: the run page names a keyed foreach's iterations by their
/// key (digits included) and an unkeyed one's by its item, so the outline
/// says which it is. `key` is omitted when iterations are keyed by index.
#[test]
fn a_foreach_carries_its_key_expression() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: k, title: K}\nspec:\n  steps:\n    - id: each\n      foreach:\n        items: \"${{ [1, 2] }}\"\n        as: n\n        key: \"${{ n }}\"\n        max_items: 5\n        steps: [{id: a, title: A}]\n";
    let out = call("outline", &json!({"source": source}));
    assert_eq!(out["ok"]["diagnostics"], json!([]), "{out}");
    assert_eq!(
        out["ok"]["outline"]["steps"][0]["foreach"]["key"],
        json!("${{ n }}")
    );
    assert!(node("rollout")["foreach"].get("key").is_none());
}

#[test]
fn switch_branches_are_labelled_by_their_condition() {
    let gate = node("risk_gate");
    let branches = gate["branches"].as_array().unwrap();
    let summary: Vec<(Value, Value, Value)> = branches
        .iter()
        .map(|b| (b["kind"].clone(), b["label"].clone(), b["when"].clone()))
        .collect();
    assert_eq!(
        summary,
        [
            (
                json!("case"),
                json!("inputs.risk == 'high'"),
                json!("${{ inputs.risk == 'high' }}")
            ),
            (
                json!("case"),
                json!("inputs.risk == 'medium'"),
                json!("${{ inputs.risk == 'medium' }}")
            ),
            (json!("default"), json!("default"), Value::Null),
        ]
    );
    // A step's `if` is its guard.
    assert_eq!(
        node("hold_if_rejected")["guard"],
        "${{ steps.cab_review.outputs.decision == 'hold' }}"
    );
}

#[test]
fn group_branches_are_kinded_catch_and_finally() {
    let preflight = node("preflight");
    let kinds: Vec<&Value> = preflight["branches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| &b["kind"])
        .collect();
    assert_eq!(kinds, [&json!("catch")]);
}

#[test]
fn calls_set_and_end_carry_their_targets() {
    assert_eq!(
        node("verify")["call"],
        json!({"runbook": "post-deploy-checks", "with": ["regions", "service", "version"]})
    );
    assert_eq!(
        node("shipped")["set"],
        json!({"fields": ["outcome"], "vars": ["decision"]})
    );
    assert_eq!(
        node("stop_after_preflight")["end"],
        json!({"status": "failed", "message": "Preflight failed"})
    );
    // A pinned version is carried as written.
    let pinned = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: p, title: P}\nspec:\n  steps:\n    - id: child\n      call: {runbook: other, version: 3}\n";
    let out = call("outline", &json!({"source": pinned}));
    assert_eq!(
        out["ok"]["outline"]["steps"][0]["call"],
        json!({"runbook": "other", "version": 3})
    );
}

#[test]
fn the_hint_stays_for_compatibility() {
    assert_eq!(node("canary")["hint"], "Run in parallel until one succeeds");
    assert_eq!(node("verify")["hint"], "Run post-deploy-checks");
}
