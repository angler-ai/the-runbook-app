use runbook_model::RecipientRef;
use runbook_spec::{
    CompileContext, compile, parse, resolve_approval_escalation, resolve_escalation, restore,
};
use serde_json::json;
#[test]
fn owners_are_default_escalation_target() {
    let owners = vec![RecipientRef::Team("team:oncall".into())];
    assert_eq!(resolve_escalation(&[], &owners).unwrap(), owners);
    let explicit = vec![RecipientRef::Team("team:incident".into())];
    assert_eq!(resolve_escalation(&explicit, &owners).unwrap(), explicit);
    assert_eq!(
        resolve_escalation(&[], &[]).unwrap_err().code,
        "approval.no_escalation_target"
    );
}
#[test]
fn compiled_escalation_uses_pins_and_preserves_explicit_empty() {
    let context = CompileContext {
        users: [(
            "alice".into(),
            "usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        )]
        .into(),
        teams: [(
            "oncall".into(),
            "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
        )]
        .into(),
        ..Default::default()
    };
    let user = RecipientRef::User("usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap());
    let team = RecipientRef::Team("team_01ARZ3NDEKTSV4RRFFQ69G5FAV".into());
    for (explicit, owners, expected) in [
        (
            None,
            json!(["user:alice", "team:oncall"]),
            Some(vec![user, team.clone()]),
        ),
        (
            Some(json!(["team:oncall"])),
            json!(["user:alice"]),
            Some(vec![team]),
        ),
        (Some(json!([])), json!(["team:oncall"]), None),
        (None, json!([]), None),
    ] {
        let mut approval =
            json!({"prompt":"Go?","options":["go"],"approvers":{"users":["alice"]},"timeout":"1m"});
        if let Some(explicit) = explicit {
            approval["escalate_to"] = explicit;
        }
        let source = json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test","owners":owners},"spec":{"steps":[{"id":"gate","approval":approval}]}});
        let compiled = compile(&parse(&source.to_string()).unwrap(), &context).unwrap();
        let restored = restore(compiled.snapshot_bytes()).unwrap();
        let result = resolve_approval_escalation(&compiled, &compiled.ir().steps[0]);
        assert_eq!(
            result,
            resolve_approval_escalation(&restored, &restored.ir().steps[0])
        );
        match expected {
            Some(expected) => assert_eq!(result.unwrap(), expected),
            None => assert_eq!(result.unwrap_err().code, "approval.no_escalation_target"),
        }
    }
}
