use runbook_model::{
    CreateRunbookRequest, DraftView, Problem, ProjectMemberRequest, PublishedVersionView,
    SaveDraftRequest, ValidateRequest, ValidateResponse,
};
use serde_json::json;

const RB: &str = "rb_01JABCDEFGHJKMNPQRSTVWXYZ0";
const RBV: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXYZ0";
const SOURCE: &str = "# keep this comment\napiVersion: runbook/v1\nkind: Runbook\n";

fn validator(name: &str) -> jsonschema::Validator {
    let api = runbook_contract::openapi();
    assert!(
        api["components"]["schemas"][name].is_object(),
        "missing {name}"
    );
    jsonschema::validator_for(
        &json!({"$ref":format!("#/components/schemas/{name}"),"components":api["components"]}),
    )
    .unwrap()
}

// Missing Editor would prevent an intended authoring principal from being represented.
#[test]
fn editor_membership_is_representable() {
    let raw = json!({"user_id":"usr_01JABCDEFGHJKMNPQRSTVWXYZ0","role":"editor"});
    let member: ProjectMemberRequest = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(member).unwrap(), raw);
    assert!(validator("ProjectMemberRequest").is_valid(&raw));
}

// A denial hint must survive Rust and schema boundaries without changing older errors.
#[test]
fn publish_denial_round_trips_actionable_hint() {
    let mut raw = json!({"type":"about:blank","title":"Forbidden","status":403,"detail":"Ask a human to publish the saved draft.","instance":"/v1/runbooks/example/versions","code":"auth.scope_missing","errors":[]});
    let old: Problem = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(old).unwrap(), raw);
    raw["next_action"] = json!("ask_human_to_publish");
    let denied: Problem = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(denied).unwrap(), raw);
    assert!(validator("Problem").is_valid(&raw));
    raw["next_action"] = json!("invented_action");
    assert!(serde_json::from_value::<Problem>(raw.clone()).is_err());
    assert!(!validator("Problem").is_valid(&raw));
}

// A client must be able to edit exact source and track a positive revision.
#[test]
fn authoring_schemas_preserve_source_and_validate_revision_identity_and_budget() {
    fn request<T: serde::de::DeserializeOwned + serde::Serialize>(name: &str) {
        let schema = validator(name);
        let raw = json!({"source":SOURCE});
        assert!(schema.is_valid(&raw), "{name}");
        let value: T = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(value).unwrap(), raw, "{name}");
        for invalid in [json!({}), json!({"source":42})] {
            assert!(!schema.is_valid(&invalid), "{name}");
            assert!(serde_json::from_value::<T>(invalid).is_err(), "{name}");
        }
    }
    request::<CreateRunbookRequest>("CreateRunbookRequest");
    request::<SaveDraftRequest>("SaveDraftRequest");
    request::<ValidateRequest>("ValidateRequest");
    let schema = validator("DraftView");
    let draft = json!({"runbook_id":RB,"source":SOURCE,"revision":1});
    assert!(schema.is_valid(&draft));
    let view: DraftView = serde_json::from_value(draft.clone()).unwrap();
    assert_eq!(view.source, SOURCE);
    assert_eq!(serde_json::to_value(view).unwrap(), draft);
    let mut maximum = draft.clone();
    maximum["revision"] = json!(u64::MAX);
    assert!(schema.is_valid(&maximum));
    assert!(serde_json::from_value::<DraftView>(maximum).is_ok());
    for field in ["runbook_id", "source", "revision"] {
        let mut missing = draft.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!schema.is_valid(&missing));
        assert!(serde_json::from_value::<DraftView>(missing).is_err());
    }
    for revision in [
        json!(0),
        json!(-1),
        json!(1.5),
        json!("1"),
        serde_json::from_str("18446744073709551616").unwrap(),
    ] {
        let mut invalid = draft.clone();
        invalid["revision"] = revision;
        assert!(!schema.is_valid(&invalid), "{invalid}");
        assert!(serde_json::from_value::<DraftView>(invalid).is_err());
    }
    for id in [
        "rb_",
        "rbv_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "rb_81JABCDEFGHJKMNPQRSTVWXYZ0",
    ] {
        let mut invalid = draft.clone();
        invalid["runbook_id"] = json!(id);
        assert!(!schema.is_valid(&invalid));
        assert!(serde_json::from_value::<DraftView>(invalid).is_err());
    }
    let publication = validator("PublishedVersionView");
    let version = json!({"runbook_id":RB,"runbook_version_id":RBV,"runbook_version":1});
    assert!(publication.is_valid(&version));
    let published: PublishedVersionView = serde_json::from_value(version.clone()).unwrap();
    assert_eq!(serde_json::to_value(published).unwrap(), version);
    for number in [json!(null), json!(0), json!(4294967296_u64)] {
        let mut invalid = version.clone();
        invalid["runbook_version"] = number;
        assert!(!publication.is_valid(&invalid));
        assert!(serde_json::from_value::<PublishedVersionView>(invalid).is_err());
    }
    for field in ["runbook_id", "runbook_version_id", "runbook_version"] {
        let mut missing = version.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(!publication.is_valid(&missing));
        assert!(serde_json::from_value::<PublishedVersionView>(missing).is_err());
    }
    let validation = validator("ValidateResponse");
    for raw in [
        json!({"diagnostics":[],"step_budget":1}),
        json!({"diagnostics":[{"path":"spec.steps","code":"spec.invalid","message":"Invalid steps."}],"step_budget":null}),
    ] {
        assert!(validation.is_valid(&raw));
        let response: ValidateResponse = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(response).unwrap(), raw);
    }
    for invalid in [
        json!({"diagnostics":[],"step_budget":-1}),
        json!({"diagnostics":[]}),
    ] {
        assert!(!validation.is_valid(&invalid));
        assert!(serde_json::from_value::<ValidateResponse>(invalid).is_err());
    }
}

// Dropping a route or mislabeling publish as write would mislead generated clients.
#[test]
fn authoring_operations_declare_scopes_typed_payloads_and_implemented_status() {
    let api = runbook_contract::openapi();
    for (path, method, scope, request, response, status) in [
        (
            "/v1/projects/{p}/runbooks",
            "post",
            "runbooks:write",
            Some("CreateRunbookRequest"),
            "DraftWriteView",
            "201",
        ),
        (
            "/v1/runbooks/{rb}/draft",
            "get",
            "runbooks:read",
            None,
            "DraftView",
            "200",
        ),
        (
            "/v1/runbooks/{rb}/draft",
            "patch",
            "runbooks:write",
            Some("SaveDraftRequest"),
            "DraftWriteView",
            "200",
        ),
        (
            "/v1/runbooks/{rb}/versions",
            "post",
            "runbooks:publish",
            None,
            "PublishedVersionView",
            "201",
        ),
        (
            "/v1/validate",
            "post",
            "runbooks:write",
            Some("ValidateRequest"),
            "ValidateResponse",
            "200",
        ),
    ] {
        let operation = &api["paths"][path][method];
        assert_eq!(
            operation["x-implementation"], "implemented",
            "{method} {path}"
        );
        assert_eq!(operation["x-required-scopes"], json!([scope]));
        assert_eq!(
            operation["responses"][status]["content"]["application/json"]["schema"]["$ref"],
            format!("#/components/schemas/{response}")
        );
        assert_eq!(
            operation["responses"]["403"]["x-error-codes"],
            json!(["auth.scope_missing"])
        );
        assert_eq!(
            operation["responses"]["403"]["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem"
        );
        if let Some(request) = request {
            assert_eq!(
                operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
                format!("#/components/schemas/{request}")
            );
        }
        if method == "post" && path != "/v1/validate" {
            assert!(
                operation["parameters"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["name"] == "Idempotency-Key" && p["required"] == true)
            );
        }
    }
}

// Without publish preconditions a concurrent unseen edit could become published.
#[test]
fn draft_save_and_publish_require_reviewed_revision_preconditions() {
    let api = runbook_contract::openapi();
    assert_eq!(
        api["paths"]["/v1/runbooks/{rb}/draft"]["get"]["responses"]["200"]["headers"]["ETag"]["schema"]
            ["type"],
        "string"
    );
    for (path, method) in [
        ("/v1/runbooks/{rb}/draft", "patch"),
        ("/v1/runbooks/{rb}/versions", "post"),
    ] {
        let operation = &api["paths"][path][method];
        let precondition = operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "If-Match")
            .unwrap();
        assert_eq!(precondition["in"], "header");
        assert_eq!(precondition["required"], true);
        let header = jsonschema::validator_for(&precondition["schema"]).unwrap();
        assert!(header.is_valid(&json!("\"draft-7\"")));
        for invalid in ["*", "", "W/\"draft-7\"", "\"draft-6\", \"draft-7\""] {
            assert!(!header.is_valid(&json!(invalid)), "{invalid}");
        }
        assert!(
            precondition["description"]
                .as_str()
                .unwrap()
                .contains("expected draft revision")
        );
        for status in ["412", "428"] {
            assert!(
                operation["responses"][status]["description"]
                    .as_str()
                    .unwrap()
                    .contains("No mutation")
            );
        }
        assert_eq!(
            operation["responses"]["409"]["x-error-codes"],
            json!(["runbook.git_managed"])
        );
    }
}

#[test]
fn validate_request_project_is_optional() {
    let without: runbook_model::ValidateRequest =
        serde_json::from_value(serde_json::json!({"source": "x"})).unwrap();
    assert_eq!(without.project, None);
    assert_eq!(
        serde_json::to_value(&without).unwrap(),
        serde_json::json!({"source": "x"})
    );
    let with: runbook_model::ValidateRequest =
        serde_json::from_value(serde_json::json!({"source": "x", "project": "payments"})).unwrap();
    assert_eq!(with.project.as_deref(), Some("payments"));
}

#[test]
fn claim_request_actor_declaration_is_optional() {
    let plain: runbook_model::ClaimRequest =
        serde_json::from_value(serde_json::json!({"session_id": "s1"})).unwrap();
    assert_eq!(plain.actor, None);
    assert_eq!(
        serde_json::to_value(&plain).unwrap(),
        serde_json::json!({"session_id": "s1"})
    );
    let agent: runbook_model::ClaimRequest = serde_json::from_value(serde_json::json!({
        "session_id": "s1",
        "actor": {"kind": "agent", "agent_name": "deploy-bot", "model": "m-1"}
    }))
    .unwrap();
    let actor = agent.actor.unwrap();
    assert_eq!(actor.kind, runbook_model::ActorKind::Agent);
    assert_eq!(actor.agent_name.as_deref(), Some("deploy-bot"));
    assert!(
        serde_json::from_value::<runbook_model::ClaimRequest>(serde_json::json!({
            "session_id": "s1", "actor": {"kind": "robot"}
        }))
        .is_err()
    );
}

/// UI-1: the runbook list declares its search parameters, and its rows
/// gain description, tags, draft_ahead, last_run and active_runs without
/// breaking a client that reads (or a server that wrote) the old shape.
#[test]
fn runbook_list_declares_search_and_summary_fields() {
    let api = runbook_contract::openapi();
    let names: Vec<&str> = api["paths"]["/v1/projects/{p}/runbooks"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    for name in ["limit", "cursor", "q", "tag"] {
        assert!(names.contains(&name), "{name}: {names:?}");
    }
    let row = validator("RunbookCatalogView");
    let old = json!({"runbook_id":"rb_00000000000000000000000001","slug":"deploy","origin":"ui","archived":false,"title":"Deploy"});
    assert!(row.is_valid(&old));
    let view: runbook_model::RunbookCatalogView = serde_json::from_value(old.clone()).unwrap();
    assert!(view.tags.is_empty() && !view.draft_ahead && view.last_run.is_none());
    // UI-1 review: absent run fields mean "withheld" (no executions:read),
    // not "no runs"; an absent truncation flag means a whole description.
    assert_eq!(view.active_runs, None);
    assert!(!view.description_truncated);
    let written = serde_json::to_value(&view).unwrap();
    for absent in ["last_run", "active_runs", "description_truncated"] {
        assert!(written.get(absent).is_none(), "{absent}: {written}");
    }
    let new = json!({"runbook_id":"rb_00000000000000000000000001","slug":"deploy","origin":"ui","archived":false,"title":"Deploy","latest_version":2,"description":"Ships it","tags":["prod"],"draft_ahead":true,"last_run":{"id":"exe_00000000000000000000000001","status":"running","created_at_ms":1,"completed_at_ms":null},"active_runs":1});
    assert!(row.is_valid(&new));
    let view: runbook_model::RunbookCatalogView = serde_json::from_value(new.clone()).unwrap();
    assert_eq!(serde_json::to_value(view).unwrap(), new);
    let mut bad = new.clone();
    bad["last_run"]["status"] = json!("sleeping");
    assert!(!row.is_valid(&bad));
    // A cut description says so.
    let mut cut = new.clone();
    cut["description_truncated"] = json!(true);
    assert!(row.is_valid(&cut));
    let view: runbook_model::RunbookCatalogView = serde_json::from_value(cut.clone()).unwrap();
    assert!(view.description_truncated);
    assert_eq!(serde_json::to_value(view).unwrap(), cut);
    // The contract says when the run fields are withheld.
    let api = runbook_contract::openapi();
    let schema = &api["components"]["schemas"]["RunbookCatalogView"]["properties"];
    for field in ["last_run", "active_runs"] {
        let text = schema[field].to_string();
        assert!(text.contains("executions:read"), "{field}: {text}");
    }
    let tag = api["paths"]["/v1/projects/{p}/runbooks"]["get"]["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "tag")
        .unwrap()
        .to_string();
    assert!(tag.contains("\\\\,"), "{tag}");
}
