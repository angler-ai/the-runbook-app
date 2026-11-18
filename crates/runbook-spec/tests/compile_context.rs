//! The pure compile context (B3, F98): the server and the browser compiler
//! build the same `CompileContext` from the same directory facts.
use runbook_spec::authoring::{
    CompileDirectory, DirectoryTeam, DirectoryUser, LatestSkill, compile_context,
    resolve_skill_reference,
};
use runbook_spec::{CompileContext, parse_declarations};
use serde_json::json;

fn user(n: u32, name: &str, active: bool) -> DirectoryUser {
    DirectoryUser {
        id: format!("usr_{n:026}").parse().unwrap(),
        name: name.into(),
        active,
    }
}
fn team(n: u32, slug: &str) -> DirectoryTeam {
    DirectoryTeam {
        id: format!("team_{n:026}").parse().unwrap(),
        slug: slug.into(),
    }
}
fn skill(name: &str, reference: Option<&str>, byte: u8) -> LatestSkill {
    LatestSkill {
        name: name.into(),
        reference: reference.map(str::to_owned),
        hash: [byte; 32],
    }
}
const SOURCE: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: demo, title: Demo}\nspec:\n  skills:\n    errors: ./skills/check-error-rates\n    deploy: acme/tools/deploy\n    missing: ./skills/nowhere\n  steps:\n    - id: work\n      title: Work\n";

#[test]
fn empty_directory_matches_the_default_context() {
    let built = compile_context(&CompileDirectory::default(), "not yaml: [").unwrap();
    let default = CompileContext::default();
    assert_eq!(built.expression_profile, default.expression_profile);
    assert_eq!(built.skills, default.skills);
    assert_eq!(built.users, default.users);
    assert_eq!(built.teams, default.teams);
    assert_eq!(built.project_variables, default.project_variables);
}

#[test]
fn active_users_by_id_and_unique_name_teams_by_slug_and_id() {
    let directory = CompileDirectory {
        users: vec![
            user(3, "Carol", true),
            user(1, "Alice", true),
            user(2, "Alice", true),
            user(4, "Dave", false),
            user(5, "Bob", true),
        ],
        teams: vec![team(2, "eng-leads"), team(1, "oncall")],
        ..Default::default()
    };
    let ctx = compile_context(&directory, SOURCE).unwrap();
    let users: Vec<(&str, &str)> = ctx
        .users
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        users,
        [
            ("Bob", "usr_00000000000000000000000005"),
            ("Carol", "usr_00000000000000000000000003"),
            (
                "usr_00000000000000000000000001",
                "usr_00000000000000000000000001"
            ),
            (
                "usr_00000000000000000000000002",
                "usr_00000000000000000000000002"
            ),
            (
                "usr_00000000000000000000000003",
                "usr_00000000000000000000000003"
            ),
            (
                "usr_00000000000000000000000005",
                "usr_00000000000000000000000005"
            ),
        ]
    );
    let teams: Vec<(&str, &str)> = ctx
        .teams
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        teams,
        [
            ("eng-leads", "team_00000000000000000000000002"),
            ("oncall", "team_00000000000000000000000001"),
            (
                "team_00000000000000000000000001",
                "team_00000000000000000000000001"
            ),
            (
                "team_00000000000000000000000002",
                "team_00000000000000000000000002"
            ),
        ]
    );
}

#[test]
fn latest_skills_pin_names_and_resolve_source_aliases() {
    let directory = CompileDirectory {
        skills: vec![
            skill("check-error-rates", None, 1),
            skill("deploy-tool", Some("acme/tools/deploy"), 2),
        ],
        ..Default::default()
    };
    let ctx = compile_context(&directory, SOURCE).unwrap();
    assert_eq!(
        ctx.skills.into_iter().collect::<Vec<_>>(),
        [
            ("./skills/check-error-rates".to_owned(), [1; 32]),
            ("acme/tools/deploy".to_owned(), [2; 32]),
            ("skill://check-error-rates".to_owned(), [1; 32]),
            ("skill://deploy-tool".to_owned(), [2; 32]),
        ]
    );
    assert_eq!(
        resolve_skill_reference(&directory.skills, "skill://deploy-tool"),
        Some([2; 32])
    );
    assert_eq!(
        resolve_skill_reference(&directory.skills, "skill://a/deploy-tool"),
        None
    );
}

#[test]
fn variable_declarations_parse_or_fail() {
    let declarations = json!({"region": {"type": "string", "default": "us-east"}});
    let directory = CompileDirectory {
        variables: declarations.as_object().unwrap().clone(),
        ..Default::default()
    };
    let ctx = compile_context(&directory, SOURCE).unwrap();
    assert_eq!(
        ctx.project_variables,
        parse_declarations(&declarations).unwrap()
    );
    let invalid = CompileDirectory {
        variables: json!({"region": {"type": "nope"}})
            .as_object()
            .unwrap()
            .clone(),
        ..Default::default()
    };
    assert!(compile_context(&invalid, SOURCE).is_err());
}

#[test]
fn directory_json_is_the_browser_wire_shape() {
    let directory: CompileDirectory = serde_json::from_value(json!({
        "users": [{"id": "usr_00000000000000000000000001", "name": "Alice"}],
        "teams": [{"id": "team_00000000000000000000000001", "slug": "oncall"}],
        "skills": [{"name": "deploy", "hash": "ab".repeat(32), "reference": null}],
        "variables": {"region": {"type": "string"}}
    }))
    .unwrap();
    assert!(directory.users[0].active, "active defaults to true");
    assert_eq!(directory.skills[0].hash, [0xab; 32]);
    let round: CompileDirectory =
        serde_json::from_value(serde_json::to_value(&directory).unwrap()).unwrap();
    assert_eq!(round, directory);
    for bad in [
        json!({"skills": [{"name": "x", "hash": "ab"}]}),
        json!({"skills": [{"name": "x", "hash": "zz".repeat(32)}]}),
        json!({"users": [{"id": "bob", "name": "Bob"}]}),
        json!({"unknown": 1}),
    ] {
        assert!(serde_json::from_value::<CompileDirectory>(bad).is_err());
    }
}
