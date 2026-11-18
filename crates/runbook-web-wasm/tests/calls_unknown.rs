//! UI-2: the browser has no catalog of published runbooks, so `validate`
//! treats every call target as unknown and never reports a call diagnostic
//! the server might not (a call to a published runbook used to show
//! `spec.call_unresolved`). Every other check still runs.
mod support;
use serde_json::{Value, json};
use support::*;

const CALL_CODES: [&str; 8] = [
    "spec.call_unresolved",
    "spec.call_cycle",
    "spec.call_depth_exceeded",
    "spec.call_unknown_input",
    "spec.call_missing_input",
    "spec.call_input_type",
    "spec.call_unknown_output",
    "spec.call_output_type",
];

fn codes(out: &Value) -> Vec<String> {
    out["ok"]["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("{out}"))
        .iter()
        .map(|d| d["code"].as_str().unwrap().to_owned())
        .collect()
}

fn runbook(steps: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: caller, title: Caller}}\nspec:\n  inputs:\n    service: {{ type: string, required: true }}\n  steps:\n{steps}"
    )
}

#[test]
fn a_call_to_an_unknown_runbook_is_not_an_error() {
    let source = runbook(
        "    - id: child\n      call:\n        runbook: post-deploy-checks\n        with: { service: api, version: \"${{ inputs.service }}\", regions: [a, b] }\n        outputs:\n          verdict: { type: string, required: true }\n          count: { type: integer }\n    - id: after\n      title: \"Use ${{ steps.child.outputs.verdict }}\"\n",
    );
    let out = call("validate", &json!({"source": source}));
    assert_eq!(codes(&out), Vec::<String>::new(), "{out}");
    // The budget is still computed.
    assert!(out["ok"]["step_budget"].is_u64(), "{out}");
}

#[test]
fn pinned_and_repeated_calls_are_not_errors() {
    let source = runbook(
        "    - id: one\n      call: { runbook: other, version: 3, with: { a: 1 } }\n    - id: two\n      call: { runbook: other, version: 3, with: { b: x } }\n    - id: three\n      call: { runbook: other }\n",
    );
    let out = call("validate", &json!({"source": source}));
    assert_eq!(codes(&out), Vec::<String>::new(), "{out}");
}

#[test]
fn the_release_train_validates_without_a_catalog() {
    let source =
        std::fs::read_to_string(repo().join("scripts/dev/review-runbooks/release-train.yaml"))
            .unwrap();
    let directory = json!({
        "teams": [
            {"id": "team_01ARZ3NDEKTSV4RRFFQ69G5FAV", "slug": "oncall"},
            {"id": "team_01ARZ3NDEKTSV4RRFFQ69G5FAW", "slug": "customer-success"}
        ]
    });
    let out = call("validate", &json!({"source": source, "context": directory}));
    let found = codes(&out);
    assert!(
        found.iter().all(|c| !CALL_CODES.contains(&c.as_str())),
        "{out}"
    );
}

#[test]
fn other_errors_beside_a_call_are_still_reported() {
    // An unknown reference elsewhere: the compiler must get past the call
    // (call errors used to stop compilation before the scope checks).
    let source = runbook(
        "    - id: child\n      call: { runbook: nowhere, with: { x: 1 } }\n    - id: after\n      title: \"Use ${{ inputs.nope }}\"\n",
    );
    let out = call("validate", &json!({"source": source}));
    let found = codes(&out);
    assert!(!found.is_empty(), "{out}");
    assert!(
        found.iter().all(|c| !CALL_CODES.contains(&c.as_str())),
        "{out}"
    );
}

#[test]
fn a_call_to_itself_is_still_a_cycle() {
    // Certain without a catalog: the server reports it too.
    let source = runbook("    - id: again\n      call: { runbook: caller }\n");
    let out = call("validate", &json!({"source": source}));
    assert_eq!(codes(&out), ["spec.call_cycle"], "{out}");
}
