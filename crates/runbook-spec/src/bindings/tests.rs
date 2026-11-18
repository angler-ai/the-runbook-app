use super::*;
use serde_json::{Value, json};
fn ir(spec: Value) -> NormalizedRunbook {
    crate::normalize(&crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":spec}).to_string()).unwrap()).unwrap()
}
fn user() -> UserId {
    "usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
}
fn team() -> TeamId {
    "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap()
}
fn catalog() -> Bindings {
    Bindings {
        skills: [
            ("./skills/check-error-rates".into(), [1; 32]),
            ("./skills/rollback-service".into(), [2; 32]),
        ]
        .into(),
        users: [("sam".into(), user())].into(),
        teams: [
            ("oncall".into(), team()),
            ("eng-leads".into(), team()),
            ("sam".into(), team()),
        ]
        .into(),
    }
}
fn select_from(ir: &NormalizedRunbook, c: &Bindings) -> Result<Bindings, Diagnostic> {
    select(
        ir,
        &Catalog {
            skills: &c.skills,
            users: &c.users,
            teams: &c.teams,
        },
    )
}
fn flagship() -> NormalizedRunbook {
    crate::normalize(&crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap())
        .unwrap()
}
#[test]
fn referenced_bindings_pin_flagship() {
    let runbook = flagship();
    let bindings = select_from(&runbook, &catalog()).unwrap();
    assert_eq!(
        bindings.skills,
        [
            ("error_rates".into(), [1; 32]),
            ("rollback".into(), [2; 32])
        ]
        .into()
    );
    assert_eq!(
        bindings
            .teams
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["eng-leads", "oncall"]
    );
    assert!(bindings.users.is_empty());
    assert_eq!(crate::estimate_step_runs(&runbook).unwrap(), 20);
}
#[test]
fn referenced_bindings_ignore_unused_catalog() {
    let runbook = flagship();
    let mut c = catalog();
    let before = select_from(&runbook, &c).unwrap();
    c.skills.insert("unused".into(), [9; 32]);
    c.users.insert("unused".into(), user());
    c.teams.insert("unused".into(), team());
    assert_eq!(select_from(&runbook, &c).unwrap(), before);
}
#[test]
fn referenced_bindings_keep_identity_namespaces() {
    let mut runbook = ir(
        json!({"steps":[{"id":"task","title":"Task","assignee":"user:sam"},
        {"id":"gate","approval":{"prompt":"Go?","options":["yes"],"approvers":{"users":["sam"],"teams":["sam"]},"timeout":"1m","escalate_to":["team:sam","user:sam"]}}]}),
    );
    runbook.metadata.owners = vec![Recipient::User("sam".into()), Recipient::Team("sam".into())];
    let bindings = select_from(&runbook, &catalog()).unwrap();
    assert_eq!(bindings.users, [("sam".into(), user())].into());
    assert_eq!(bindings.teams, [("sam".into(), team())].into());
}
#[test]
fn referenced_bindings_skip_computed_assignee() {
    let runbook =
        ir(json!({"steps":[{"id":"task","title":"Task","assignee":"${{ inputs.owner }}"}]}));
    assert_eq!(
        select_from(&runbook, &Bindings::default()).unwrap(),
        Bindings::default()
    );
}
#[test]
fn referenced_bindings_are_owned() {
    let runbook = flagship();
    let mut c = catalog();
    let bindings = select_from(&runbook, &c).unwrap();
    c.skills.clear();
    c.teams.clear();
    c.users.clear();
    assert_eq!(bindings.skills["rollback"], [2; 32]);
    assert_eq!(bindings.teams["oncall"], team());
}
#[test]
fn referenced_bindings_missing_reference_is_source_local() {
    for (step, code, suffix) in [
        (
            json!({"id":"task","title":"Task","skill":"missing"}),
            "spec.unresolved_skill",
            "/skill",
        ),
        (
            json!({"id":"task","title":"Task","assignee":"sam"}),
            "spec.unresolved_user",
            "/assignee",
        ),
        (
            json!({"id":"gate","approval":{"prompt":"Go?","options":["yes"],"approvers":{"users":["sam"]}}}),
            "spec.unresolved_user",
            "/approval/approvers/users/0",
        ),
        (
            json!({"id":"gate","approval":{"prompt":"Go?","options":["yes"],"approvers":{"teams":["sam"]}}}),
            "spec.unresolved_team",
            "/approval/approvers/teams/0",
        ),
        (
            json!({"id":"gate","approval":{"prompt":"Go?","options":["yes"],"timeout":"1m","escalate_to":["team:sam"]}}),
            "spec.unresolved_team",
            "/approval/escalate_to/0",
        ),
    ] {
        let error = select_from(&ir(json!({"steps":[step]})), &Bindings::default()).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.path, format!("/spec/steps/0{suffix}"));
    }
    let mut runbook = ir(json!({"steps":[]}));
    runbook.metadata.owners = vec![Recipient::Team("sam".into())];
    let error = select_from(&runbook, &Bindings::default()).unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.path.as_str()),
        ("spec.unresolved_team", "/metadata/owners/0")
    );
}
#[test]
fn referenced_bindings_resolve_all_control_bodies() {
    let task =
        |id: &str| json!({"id":id,"title":id,"skill":format!("skill://{id}"),"assignee":"sam"});
    let runbook = ir(json!({"steps":[
        {"id":"group","steps":[task("child")],"catch":[task("catch")],"finally":[task("cleanup")]},
        {"id":"parallel","parallel":[task("parallel_child")]},
        {"id":"each","foreach":{"items":"${{ [1] }}","max_items":1,"steps":[task("each_child")]}},
        {"id":"repeat","loop":{"until":"${{ true }}","max_iterations":1,"steps":[task("loop_child")]}},
        {"id":"switch","switch":{"cases":[{"when":"${{ true }}","steps":[task("case_child")]}],"default":{"steps":[task("default_child")]}}}
    ],"finally":[task("final_child")]}));
    let names = [
        "child",
        "catch",
        "cleanup",
        "parallel_child",
        "each_child",
        "loop_child",
        "case_child",
        "default_child",
        "final_child",
    ];
    let mut c = catalog();
    for (i, name) in names.iter().enumerate() {
        c.skills.insert(format!("skill://{name}"), [i as u8; 32]);
    }
    let selected = select_from(&runbook, &c).unwrap();
    assert_eq!(selected.skills.len(), names.len());
    for (i, name) in names.iter().enumerate() {
        assert_eq!(selected.skills[&format!("skill://{name}")], [i as u8; 32]);
    }
    assert_eq!(selected.users, [("sam".into(), user())].into());
}

#[test]
fn referenced_bindings_resolve_alias_target_once() {
    let runbook = ir(
        json!({"skills":{"first":"second","second":"./real","unused":"./missing"},"steps":[{"id":"task","title":"Task","skill":"first"}]}),
    );
    let mut c = Bindings::default();
    c.skills.insert("./real".into(), [9; 32]);
    assert_eq!(
        select_from(&runbook, &c).unwrap_err().code,
        "spec.unresolved_skill"
    );
    c.skills.insert("second".into(), [3; 32]);
    assert_eq!(
        select_from(&runbook, &c).unwrap().skills,
        [("first".into(), [3; 32])].into()
    );
    let undeclared = ir(json!({"steps":[{"id":"task","title":"Task","skill":"second"}]}));
    assert_eq!(
        select_from(&undeclared, &c).unwrap_err().code,
        "spec.unresolved_skill"
    );
}

#[test]
fn direct_skill_uri_cannot_be_redirected_by_alias() {
    let runbook = ir(
        json!({"skills":{"skill://deploy":"./other"},"steps":[{"id":"task","title":"Task","skill":"skill://deploy"}]}),
    );
    let mut c = Bindings::default();
    c.skills.insert("skill://deploy".into(), [1; 32]);
    c.skills.insert("./other".into(), [2; 32]);
    assert_eq!(
        select_from(&runbook, &c).unwrap().skills["skill://deploy"],
        [1; 32]
    );
    c.skills.remove("skill://deploy");
    assert_eq!(
        select_from(&runbook, &c).unwrap_err().code,
        "spec.unresolved_skill"
    );
}
