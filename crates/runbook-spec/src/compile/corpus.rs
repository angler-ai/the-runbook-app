//! Public API conformance, reused by the existing native/Wasm executable.
use crate::{CompileContext, compile, parse, restore, validate_policy};
use serde_json::{Value, json};
fn hash(c: &crate::CompiledRunbook) -> String {
    c.hash().iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn rows() -> Vec<Value> {
    let mut rows = vec![];
    let ctx = CompileContext {
        skills: [
            ("./skills/check-error-rates".into(), [1; 32]),
            ("./skills/rollback-service".into(), [2; 32]),
        ]
        .into(),
        teams: [
            (
                "oncall".into(),
                "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            ),
            (
                "eng-leads".into(),
                "team_01ARZ3NDEKTSV4RRFFQ69G5FAW".parse().unwrap(),
            ),
        ]
        .into(),
        ..Default::default()
    };
    let original = compile(
        &parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap(),
        &ctx,
    )
    .unwrap();
    let restored = restore(original.snapshot_bytes()).unwrap();
    assert!(validate_policy(&restored).is_empty());
    assert_eq!(original.hash(), restored.hash());
    assert_eq!(original.ir(), restored.ir());
    assert_eq!(original.skill_hashes(), restored.skill_hashes());
    assert_eq!(restored.worst_case_step_runs(), 20);
    assert_eq!(restored.checked_sites().count(), 14);
    assert_eq!(
        restored
            .checked_sites()
            .map(|s| s.template.expressions().count())
            .sum::<usize>(),
        15
    );
    rows.push(json!({"name":"public_flagship","hash":hash(&restored),"budget":20,"sites":14,"segments":15}));
    let lim = crate::wire::Limits {
        bytes: 64 * 1024 * 1024,
        values: 2_000_000,
        depth: 512,
    };
    let tree = crate::wire::decode(original.snapshot_bytes(), lim).unwrap();
    for (name, field, value, code) in [
        (
            "public_hash_tamper",
            "hash",
            json!(vec![0; 32]),
            "spec.snapshot_hash_mismatch",
        ),
        (
            "public_budget_tamper",
            "worst_case_step_runs",
            json!(21),
            "spec.snapshot_budget_mismatch",
        ),
    ] {
        let mut changed = tree.get().clone();
        changed[field] = value;
        let bytes = crate::wire::encode(&changed, lim).unwrap();
        let errors = restore(&bytes).unwrap_err();
        assert_eq!(errors[0].code, code);
        rows.push(json!({"name":name,"code":errors[0].code}));
    }
    let mut changed = tree.get().clone();
    changed["ir"]["source_yaml"] = json!("not YAML: [");
    changed["ir"]["source_json"] = json!("Null");
    let bytes = crate::wire::encode(&changed, lim).unwrap();
    let restored = restore(&bytes).unwrap();
    assert_eq!(restored.hash(), original.hash());
    assert_eq!(restored.ir().steps, original.ir().steps);
    rows.push(json!({"name":"public_ignores_source","hash":hash(&restored)}));
    for (name, body, expected) in [
        (
            "public_type_error",
            json!({"steps":[{"id":"task","title":"Task","if":"${{ 1 }}"}]}),
            "expression.type_error",
        ),
        (
            "public_missing_pin",
            json!({"steps":[{"id":"task","title":"Task","skill":"skill://absent"}]}),
            "spec.unresolved_skill",
        ),
    ] {
        let spec=parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":body}).to_string()).unwrap();
        let errors = compile(&spec, &CompileContext::default()).unwrap_err();
        assert_eq!(errors[0].code, expected);
        rows.push(json!({"name":name,"code":errors[0].code,"path":errors[0].path,"line":errors[0].line,"column":errors[0].column}));
    }
    for (name, auto, agent, timeout, declared, expected) in [
        (
            "policy_auto_path",
            true,
            false,
            true,
            true,
            "spec.auto_decision_destructive",
        ),
        (
            "policy_agent_path",
            false,
            true,
            true,
            true,
            "spec.agent_approval_destructive",
        ),
        ("policy_dormant_option", false, false, true, true, ""),
        (
            "policy_missing_timeout",
            true,
            false,
            false,
            true,
            "spec.on_timeout_requires_timeout",
        ),
        (
            "policy_missing_option",
            true,
            false,
            true,
            false,
            "spec.auto_option_required",
        ),
    ] {
        let mut approval = json!({"prompt":"Go?","options":[{"id":"rollback","auto":declared}],"approvers":{"teams":["oncall"]},"allow_agents":agent});
        if auto {
            approval["on_timeout"] = json!({"decide":"rollback"});
        }
        if timeout {
            approval["timeout"] = json!("1m");
        }
        let spec = parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test","owners":["team:oncall"]},"spec":{"steps":[{"id":"gate","approval":approval},{"id":"destructive","title":"Work","destructive":true}]}}).to_string()).unwrap();
        let errors = match compile(&spec, &ctx) {
            Ok(c) => {
                let before = validate_policy(&c);
                assert_eq!(
                    before,
                    validate_policy(&restore(c.snapshot_bytes()).unwrap())
                );
                before
            }
            Err(errors) => errors,
        };
        assert_eq!(errors.first().map_or("", |d| d.code.as_str()), expected);
        rows.push(json!({"name":name,"errors":errors}));
    }
    rows
}
