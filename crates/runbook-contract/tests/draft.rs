use runbook_model::{ExecutionView, RerunRequest, StartRequest, VersionSelector};
use runbook_model::{ReportRequest, ReportStatus, Verdict};
use serde_json::json;

const VERSION: &str = "rbv_01JABCDEFGHJKMNPQRSTVWXYZ0";

// Losing selector validation must reject invalid requests at this boundary.
#[test]
fn version_selector_accepts_number_or_id() {
    for (name, raw, expected) in [
        ("latest", json!({"runbook":"post-deploy"}), None),
        (
            "published",
            json!({"runbook":"post-deploy","version":3}),
            Some(json!(3)),
        ),
        (
            "preview",
            json!({"runbook":"post-deploy","version":VERSION}),
            Some(json!(VERSION)),
        ),
    ] {
        let request: StartRequest = serde_json::from_value(raw).unwrap();
        assert_eq!(
            request.version.map(|v| serde_json::to_value(v).unwrap()),
            expected,
            "{name}"
        );
    }
    for (name, raw) in [
        ("zero", json!(0)),
        ("negative", json!(-1)),
        ("numeric string", json!("3")),
        ("unprefixed", json!("draft")),
        ("empty id", json!("rbv_")),
        ("invalid ulid", json!("rbv_01IABCDEFGHJKMNPQRSTVWXYZ0")),
        ("overflow ulid", json!("rbv_Z1JABCDEFGHJKMNPQRSTVWXYZ0")),
        ("fraction", json!(1.5)),
        ("overflow number", json!(4294967296_u64)),
    ] {
        assert!(
            serde_json::from_value::<StartRequest>(json!({"runbook":"post-deploy","version":raw}))
                .is_err(),
            "{name}"
        );
        assert!(
            serde_json::from_value::<RerunRequest>(json!({"version":raw})).is_err(),
            "rerun {name}"
        );
    }
    let selected: VersionSelector = serde_json::from_value(json!(3)).unwrap();
    assert!(matches!(selected, VersionSelector::Number(n) if n.get() == 3));
    for number in [json!(3), json!(null)] {
        let raw = execution_fixture(number.clone());
        let view: ExecutionView = serde_json::from_value(raw).unwrap();
        let serialized = serde_json::to_value(view).unwrap();
        assert_eq!(serialized["runbook_version"], number);
        assert_eq!(serialized["runbook_version_id"], VERSION);
    }
}

fn execution_fixture(number: serde_json::Value) -> serde_json::Value {
    json!({"schema_revision":1,"id":"exe_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "runbook_version_id":VERSION,"runbook_version":number,
        "status":"running","phase":"ordinary","result":null,"title":"Deploy check",
        "inputs":{},"fields":{},"tags":[],"steps":[],"amended":false,
        "amendments":[],"stale_step_ids":[],"rerun_of":null,"resume_brief":null,"pending_terminal_status":null})
}

#[test]
fn runbook_ref_text_form_parses() {
    for (raw, name, selector) in [
        ("post-deploy", "post-deploy", None),
        ("post-deploy@3", "post-deploy", Some(json!(3))),
        (
            "post-deploy@rbv_01JABCDEFGHJKMNPQRSTVWXYZ0",
            "post-deploy",
            Some(json!(VERSION)),
        ),
    ] {
        let (actual, version) = runbook_model::parse_runbook_ref(raw).unwrap();
        assert_eq!(actual, name);
        assert_eq!(version.map(|v| serde_json::to_value(v).unwrap()), selector);
    }
    for raw in [
        "post-deploy@0",
        "post-deploy@latest",
        "post-deploy@",
        "post-deploy@-1",
        "post-deploy@3@4",
        "post-deploy@+3",
        "post-deploy@4294967296",
    ] {
        assert_eq!(
            runbook_model::parse_runbook_ref(raw).unwrap_err().code,
            "runbook.invalid_version_selector",
            "{raw}"
        );
    }
}

#[test]
fn runbook_ref_rejects_invalid_slug_names() {
    for raw in [
        "Post Deploy@3",
        "a/b@3",
        "bad_name",
        "",
        "@3",
        "-bad",
        "bad-",
        "two--gaps",
        "é",
    ] {
        let error = runbook_model::parse_runbook_ref(raw).unwrap_err();
        assert_eq!(error.code, "runbook.invalid_reference", "{raw:?}");
        assert_eq!(error.path, "runbook");
    }
    for raw in ["post-deploy", "9-checks@3", "a@3"] {
        assert!(runbook_model::parse_runbook_ref(raw).is_ok(), "{raw}");
    }
}

fn validate(schema_name: &str, instance: &serde_json::Value) {
    let api = runbook_contract::openapi();
    let schema = json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$ref":format!("#/components/schemas/{schema_name}"), "components":api["components"]});
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{schema_name}: {errors:?}");
}

fn fixture(name: &str) -> serde_json::Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../api/examples")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn rerun_rejection_has_field_errors() {
    let raw = fixture("rerun-inputs-invalid");
    let problem: runbook_model::Problem = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(problem.status, 422);
    assert_eq!(problem.code, "execution.inputs_invalid");
    assert_eq!(problem.errors[0].path, "inputs.region");
    assert_eq!(problem.errors[0].code, "data.required");
    assert_eq!(problem.errors[1].code, "data.type_error");
    validate("Problem", &raw);
    assert_eq!(serde_json::to_value(problem).unwrap(), raw);
}

/// U10: an invalid start answers with rerun's code and field-error shape,
/// and the start route documents it.
#[test]
fn start_rejection_has_rerun_field_errors() {
    let raw = fixture("start-inputs-invalid");
    let problem: runbook_model::Problem = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(problem.status, 422);
    assert_eq!(problem.code, "execution.inputs_invalid");
    assert_eq!(problem.errors[0].path, "inputs.service");
    assert_eq!(problem.errors[0].code, "data.enum");
    assert_eq!(problem.errors[1].path, "fields.note");
    validate("Problem", &raw);
    assert_eq!(serde_json::to_value(problem).unwrap(), raw);
    let api = runbook_contract::openapi();
    let start = &api["paths"]["/v1/projects/{p}/executions"]["post"];
    assert_eq!(
        start["responses"]["422"]["x-error-codes"],
        // CF12b: start data past its cap; CF13 review round 1: a first row
        // past one object row.
        json!([
            "execution.inputs_invalid",
            "start.invalid",
            "request.value_too_large",
            "execution.row_too_large"
        ])
    );
    assert!(
        start["description"]
            .as_str()
            .unwrap()
            .contains("422 execution.inputs_invalid")
    );
    assert!(
        api["paths"]["/v1/projects/{p}/executions"]["get"]["responses"]["422"]["x-error-codes"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn draft_exposes_pending_and_amended_states() {
    let waiting = fixture("approval-wait");
    let approval: runbook_model::ApprovalView = serde_json::from_value(waiting).unwrap();
    assert_eq!(serde_json::to_value(approval).unwrap()["status"], "waiting");
    let mut paused = execution_fixture(json!(3));
    paused["status"] = json!("paused");
    validate("ExecutionView", &paused);
    let mut cleanup = execution_fixture(json!(3));
    cleanup["phase"] = json!("cleanup");
    cleanup["pending_terminal_status"] = json!("failed");
    validate("ExecutionView", &cleanup);
    for (name, raw, status, phase, result) in [
        ("paused", paused, "paused", "ordinary", json!(null)),
        ("cleanup", cleanup, "running", "cleanup", json!(null)),
        (
            "amended",
            fixture("amended"),
            "succeeded",
            "completed",
            json!("rolled_back"),
        ),
    ] {
        let view: ExecutionView = serde_json::from_value(raw).unwrap();
        let output = serde_json::to_value(view).unwrap();
        assert_eq!(output["status"], status, "{name}");
        assert_eq!(output["phase"], phase, "{name}");
        assert_eq!(output["result"], result, "{name}");
    }
}

#[test]
fn draft_contains_pilot_routes() {
    let api = runbook_contract::openapi();
    assert_eq!(api["openapi"], "3.1.0");
    for (path, method, request) in [
        ("/v1/orgs/{org}/projects", "post", "CreateProjectRequest"),
        ("/v1/orgs/{org}/users", "post", "CreateUserRequest"),
        ("/v1/orgs/{org}/teams", "post", "CreateTeamRequest"),
        (
            "/v1/orgs/{org}/teams/{team}/members",
            "post",
            "TeamMemberRequest",
        ),
        ("/v1/projects/{p}/members", "post", "ProjectMemberRequest"),
        ("/v1/step-runs/{sr}/decisions", "post", "DecisionRequest"),
        (
            "/v1/executions/{exe}/signals/{name}",
            "post",
            "SignalRequest",
        ),
        ("/v1/executions/{exe}", "patch", "ExecutionPatchRequest"),
        ("/v1/step-runs/{sr}/report", "post", "ReportRequest"),
        (
            "/v1/executions/{exe}/tokens/refresh",
            "post",
            "RefreshRequest",
        ),
        ("/v1/step-runs/{sr}/overrides", "post", "OverrideRequest"),
        ("/v1/executions/{exe}/rerun", "post", "RerunRequest"),
    ] {
        let operation = &api["paths"][path][method];
        let implemented = runbook_model::DIRECTORY_ROUTES
            .iter()
            .chain(runbook_model::AUTHORING_ROUTES)
            .chain(runbook_model::EXECUTION_ROUTES)
            .any(|r| r.path == path && r.method.eq_ignore_ascii_case(method));
        assert_eq!(
            operation["x-implementation"],
            if implemented {
                "implemented"
            } else if matches!(
                path,
                "/v1/step-runs/{sr}/decisions"
                    | "/v1/step-runs/{sr}/report"
                    | "/v1/step-runs/{sr}/overrides"
                    | "/v1/executions/{exe}/signals/{name}"
                    | "/v1/executions/{exe}/tokens/refresh"
                    | "/v1/executions/{exe}"
            ) {
                "postgres"
            } else {
                "planned"
            },
            "{method} {path}"
        );
        assert_eq!(
            operation["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            format!("#/components/schemas/{request}")
        );
        assert_eq!(
            operation["responses"]["422"]["content"]["application/problem+json"]["schema"]["$ref"],
            "#/components/schemas/Problem"
        );
    }
}

#[test]
fn all_examples_validate_against_generated_schemas() {
    for (name, schema) in [
        ("check-finding", "StepView"),
        ("approval-wait", "ApprovalView"),
        ("resume-brief", "ResumeBrief"),
        ("amended", "ExecutionView"),
        ("preview-run", "ExecutionView"),
        ("rerun-inputs-invalid", "Problem"),
        ("start-inputs-invalid", "Problem"),
        ("memory-entry", "MemoryEntryView"),
        ("memory-proposal", "MemoryEntryView"),
        ("execution-document", "ExecutionDocumentView"),
        ("my-tasks", "InboxPage"),
    ] {
        validate(schema, &fixture(name));
    }
}

#[test]
fn generated_schema_rejects_invalid_selectors_and_missing_version_identity() {
    let api = runbook_contract::openapi();
    let schema = json!({"$ref":"#/components/schemas/StartRequest","components":api["components"]});
    let validator = jsonschema::validator_for(&schema).unwrap();
    for version in [
        json!(0),
        json!(-1),
        json!("3"),
        json!("draft"),
        json!(1.5),
        json!("rbv_"),
    ] {
        assert!(
            !validator.is_valid(&json!({"runbook":"post-deploy","version":version})),
            "{version}"
        );
    }
    for version in [json!(1), json!(VERSION)] {
        assert!(validator.is_valid(&json!({"runbook":"post-deploy","version":version})));
    }
    let schema =
        json!({"$ref":"#/components/schemas/ExecutionView","components":api["components"]});
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut raw = execution_fixture(json!(null));
    assert!(validator.is_valid(&raw));
    raw.as_object_mut().unwrap().remove("runbook_version_id");
    assert!(!validator.is_valid(&raw));
    let mut missing_number = execution_fixture(json!(null));
    missing_number
        .as_object_mut()
        .unwrap()
        .remove("runbook_version");
    assert!(!validator.is_valid(&missing_number));
    assert!(serde_json::from_value::<ExecutionView>(missing_number).is_err());
    let selector = &api["components"]["schemas"]["VersionSelector"];
    assert_eq!(selector["oneOf"][0]["minimum"], 1);
    assert_eq!(
        selector["oneOf"][1]["pattern"],
        "^rbv_[0-7][0-9A-HJKMNP-TV-Z]{25}$"
    );
}

// A zero published version is neither a preview nor a real publication.
#[test]
fn execution_view_rejects_zero_published_version() {
    let raw = execution_fixture(json!(0));
    assert!(serde_json::from_value::<ExecutionView>(raw.clone()).is_err());
    let api = runbook_contract::openapi();
    let schema =
        json!({"$ref":"#/components/schemas/ExecutionView","components":api["components"]});
    assert!(!jsonschema::validator_for(&schema).unwrap().is_valid(&raw));
}

#[test]
fn execution_schema_rejects_published_number_overflow() {
    let raw = execution_fixture(json!(4294967296_u64));
    assert!(serde_json::from_value::<ExecutionView>(raw.clone()).is_err());
    let api = runbook_contract::openapi();
    let schema =
        json!({"$ref":"#/components/schemas/ExecutionView","components":api["components"]});
    assert!(!jsonschema::validator_for(&schema).unwrap().is_valid(&raw));
}

#[test]
fn exported_draft_matches_current_types() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api/openapi.draft.json");
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(document, runbook_contract::openapi());
}

// Conflating failed findings with protocol failure must break this test.
#[test]
fn bad_news_is_successful_work() {
    let report: ReportRequest = serde_json::from_value(json!({
        "step_run_id":"sr_01JABCDEFGHJKMNPQRSTVWXYZ0",
        "claim":{"id":"clm_01JABCDEFGHJKMNPQRSTVWXYZ0", "generation":1},
        "status":"succeeded", "verdict":"fail"
    }))
    .unwrap();
    assert_eq!(report.status, ReportStatus::Succeeded);
    assert_eq!(report.verdict, Some(Verdict::Fail));
}

#[test]
fn unsigned_wire_fields_reject_overflow_in_schema_and_rust() {
    fn check<T: serde::de::DeserializeOwned + schemars::JsonSchema>(
        original: serde_json::Value,
        field: &str,
        max: serde_json::Value,
        overflow: serde_json::Value,
    ) {
        let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        for (value, accepted) in [
            (json!(0), false),
            (json!(1), true),
            (max, true),
            (overflow, false),
        ] {
            let mut instance = original.clone();
            instance[field] = value;
            assert_eq!(
                serde_json::from_value::<T>(instance.clone()).is_ok(),
                accepted,
                "Rust {field}: {instance}"
            );
            assert_eq!(
                validator.is_valid(&instance),
                accepted,
                "schema {field}: {instance}"
            );
        }
    }
    let u32_max = json!(u32::MAX);
    let u32_overflow = json!(u64::from(u32::MAX) + 1);
    check::<ExecutionView>(
        execution_fixture(json!(3)),
        "schema_revision",
        u32_max.clone(),
        u32_overflow.clone(),
    );
    for field in ["schema_revision", "attempt"] {
        check::<runbook_model::StepView>(
            fixture("check-finding"),
            field,
            u32_max.clone(),
            u32_overflow.clone(),
        );
    }
    for field in ["schema_revision", "quorum"] {
        check::<runbook_model::ApprovalView>(
            fixture("approval-wait"),
            field,
            u32_max.clone(),
            u32_overflow.clone(),
        );
    }
    check::<runbook_model::ResumeBrief>(
        fixture("resume-brief"),
        "schema_revision",
        u32_max,
        u32_overflow,
    );
    let u64_overflow: serde_json::Value = serde_json::from_str("18446744073709551616").unwrap();
    check::<runbook_model::ClaimRef>(
        json!({"id":"clm_01JABCDEFGHJKMNPQRSTVWXYZ0","generation":1}),
        "generation",
        json!(u64::MAX),
        u64_overflow.clone(),
    );
    check::<runbook_model::DecisionRecord>(
        json!({"by":"human","user_id":"usr_01JABCDEFGHJKMNPQRSTVWXYZ0","membership_revision":1,"option":"rollback","comment":null}),
        "membership_revision",
        json!(u64::MAX),
        u64_overflow,
    );
}

#[test]
fn flagship_examples_preserve_domain_result_and_loop_satisfaction() {
    let completed = fixture("post-deploy-completed");
    validate("ExecutionView", &completed);
    let view: runbook_model::ExecutionView = serde_json::from_value(completed.clone()).unwrap();
    assert_eq!(serde_json::to_value(view).unwrap(), completed);
    assert_eq!(completed["status"], "succeeded");
    assert_eq!(completed["phase"], "completed");
    assert_eq!(completed["result"], "rolled_back");
    let recovered = fixture("loop-recovered");
    validate("StepView", &recovered);
    let view: runbook_model::StepView = serde_json::from_value(recovered.clone()).unwrap();
    assert_eq!(serde_json::to_value(view).unwrap(), recovered);
    assert_eq!(recovered["status"], "succeeded");
    assert_eq!(recovered["verdict"], "fail");
    assert_eq!(recovered["satisfied"], true);
    assert_eq!(recovered["exhausted"], false);
}

/// CF12b review round 1 (finding 5): every route that can now answer
/// `422 request.value_too_large` names it: the directory creates (names past
/// 1 KiB), a skill upload (its reference or file count), a rerun (a merged
/// start record past one row), beside the routes CF12b already named.
#[test]
fn every_route_that_caps_a_value_names_the_code() {
    let api = runbook_contract::openapi();
    for (path, method) in [
        ("/v1/orgs/{org}/projects", "post"),
        ("/v1/orgs/{org}/teams", "post"),
        ("/v1/orgs/{org}/users", "post"),
        ("/v1/orgs/{org}/invitations", "post"),
        ("/v1/projects/{p}/skills", "post"),
        ("/v1/projects/{p}/executions", "post"),
        ("/v1/executions/{exe}/rerun", "post"),
        ("/v1/executions/{exe}/tokens", "post"),
        ("/v1/step-runs/{sr}/report", "post"),
        ("/v1/step-runs/{sr}/decisions", "post"),
        ("/v1/step-runs/{sr}/release", "post"),
    ] {
        let codes = &api["paths"][path][method]["responses"]["422"]["x-error-codes"];
        assert!(
            codes
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c == "request.value_too_large")),
            "{method} {path}: {codes}"
        );
    }
}

/// CF13: every route that commits to a run names
/// `422 execution.row_too_large`, the non-retryable refusal of a change one
/// of whose rows the Cloudflare edition could not hold.
#[test]
fn every_route_that_commits_to_a_run_names_row_too_large() {
    let api = runbook_contract::openapi();
    for (path, method) in [
        ("/v1/step-runs/{sr}/claim", "post"),
        ("/v1/step-runs/{sr}/heartbeat", "post"),
        ("/v1/step-runs/{sr}/logs", "post"),
        ("/v1/step-runs/{sr}/report", "post"),
        ("/v1/step-runs/{sr}/checklist", "post"),
        ("/v1/step-runs/{sr}/release", "post"),
        ("/v1/step-runs/{sr}/decisions", "post"),
        ("/v1/step-runs/{sr}/overrides", "post"),
        ("/v1/executions/{exe}", "patch"),
        ("/v1/executions/{exe}/signals/{name}", "post"),
    ] {
        let codes = &api["paths"][path][method]["responses"]["422"]["x-error-codes"];
        assert!(
            codes
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c == "execution.row_too_large")),
            "{method} {path}: {codes}"
        );
    }
}

/// CF13 review round 1, items 2 and 6: the routes that commit a request's
/// data to a run name the run budget's `422 execution.too_large`, and a
/// start or rerun names `execution.row_too_large` (its first rows are
/// measured too).
#[test]
fn routes_name_the_run_budget_and_starts_name_row_too_large() {
    let api = runbook_contract::openapi();
    let codes = |path: &str, method: &str| {
        api["paths"][path][method]["responses"]["422"]["x-error-codes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    for (path, method) in [
        ("/v1/step-runs/{sr}/report", "post"),
        ("/v1/step-runs/{sr}/checklist", "post"),
        ("/v1/step-runs/{sr}/release", "post"),
        ("/v1/step-runs/{sr}/decisions", "post"),
        ("/v1/step-runs/{sr}/overrides", "post"),
        ("/v1/executions/{exe}", "patch"),
        ("/v1/executions/{exe}/signals/{name}", "post"),
    ] {
        assert!(
            codes(path, method)
                .iter()
                .any(|c| c == "execution.too_large"),
            "{method} {path}"
        );
    }
    for path in ["/v1/projects/{p}/executions", "/v1/executions/{exe}/rerun"] {
        assert!(
            codes(path, "post")
                .iter()
                .any(|c| c == "execution.row_too_large"),
            "post {path}: {:?}",
            codes(path, "post")
        );
    }
}
