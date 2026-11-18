use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../api/examples")
        .join(format!("{name}.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn validator(name: &str) -> jsonschema::Validator {
    let api = runbook_contract::openapi();
    assert!(
        api["components"]["schemas"].get(name).is_some(),
        "missing {name} contract"
    );
    jsonschema::validator_for(
        &json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$ref":format!("#/components/schemas/{name}"),"components":api["components"]}),
    )
    .unwrap()
}

// Catches a missing memory contract, a character-count-only body guard, or untyped status/source.
#[test]
fn memory_body_limit_and_status_are_typed() {
    let schema = validator("MemoryEntryView");
    let entry = fixture("memory-entry");
    let proposal = fixture("memory-proposal");
    for (name, body, accepted) in [
        ("4096 ASCII bytes", "a".repeat(4096), true),
        ("4096 multibyte bytes", "é".repeat(2048), true),
        (
            "4097 multibyte bytes",
            format!("{}a", "é".repeat(2048)),
            false,
        ),
        ("4096 four-byte UTF8 bytes", "🦀".repeat(1024), true),
        ("4100 four-byte UTF8 bytes", "🦀".repeat(1025), false),
        ("4097 ASCII bytes", "a".repeat(4097), false),
    ] {
        let mut raw = entry.clone();
        raw["body_markdown"] = json!(body);
        let parsed = serde_json::from_value::<runbook_model::MemoryEntryView>(raw.clone());
        assert_eq!(parsed.is_ok(), accepted, "{name}");
        let request = json!({"title":"Baseline", "body_markdown":body, "tags":[], "step_id":null});
        let saved = serde_json::from_value::<runbook_model::SaveMemoryRequest>(request);
        assert_eq!(saved.is_ok(), accepted, "save {name}");
        if !accepted {
            assert!(
                parsed.unwrap_err().to_string().contains("data.max_length"),
                "{name}"
            );
            assert!(
                saved.unwrap_err().to_string().contains("data.max_length"),
                "save {name}"
            );
        }
    }
    for raw in [&entry, &proposal] {
        let typed: runbook_model::MemoryEntryView = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(serde_json::to_value(typed).unwrap(), *raw);
    }
    assert!(schema.is_valid(&entry));
    assert!(schema.is_valid(&proposal));
    let api = runbook_contract::openapi();
    assert_eq!(
        api["components"]["schemas"]["MemoryBody"]["x-max-utf8-bytes"],
        4096
    );
    let oversized = "é".repeat(2049);
    let error = runbook_model::MemoryBody::try_from(oversized.clone()).unwrap_err();
    assert_eq!(error.code, "data.max_length");
    assert_eq!(error.path, "body_markdown");
    // A generic schema validates characters; the Rust type enforces the extra byte constraint.
    let mut schema_only = entry.clone();
    schema_only["body_markdown"] = json!(oversized);
    assert!(schema.is_valid(&schema_only));
    assert!(serde_json::from_value::<runbook_model::MemoryEntryView>(schema_only).is_err());
    for (field, value) in [
        ("id", json!("mem_bad")),
        ("expires_at", json!("tomorrow")),
        ("revision", json!(4294967296_u64)),
    ] {
        let mut invalid = entry.clone();
        invalid[field] = value;
        assert!(
            serde_json::from_value::<runbook_model::MemoryEntryView>(invalid).is_err(),
            "{field}"
        );
    }
    for field in ["status", "source", "scope"] {
        let mut request = json!({"title":"Baseline", "body_markdown":"Data", "tags":[]});
        request[field] = entry[field].clone();
        assert!(
            serde_json::from_value::<runbook_model::SaveMemoryRequest>(request.clone()).is_err(),
            "server-owned {field}"
        );
        assert!(!validator("SaveMemoryRequest").is_valid(&request));
    }
    assert_eq!(proposal["source"]["kind"], "step_run");
    assert_eq!(
        proposal["source"]["execution_id"],
        "exe_01JABCDEFGHJKMNPQRSTVWXYZ0"
    );
    assert_eq!(
        proposal["source"]["step_run_id"],
        "sr_01JABCDEFGHJKMNPQRSTVWXYZ1"
    );
    for status in ["proposed", "active", "archived"] {
        let mut raw = entry.clone();
        raw["status"] = json!(status);
        assert!(schema.is_valid(&raw), "valid status {status}");
    }
    for status in ["accepted", "rejected", "pending", "", "ACTIVE"] {
        let mut raw = entry.clone();
        raw["status"] = json!(status);
        assert!(!schema.is_valid(&raw), "invalid status {status}");
        assert!(serde_json::from_value::<runbook_model::MemoryEntryView>(raw).is_err());
    }
}

// Catches accidental unconditional scope requirements that exclude execution credentials,
// or granting those credentials review or archive capabilities (MEM6: they save and edit).
#[test]
fn memory_routes_declare_scopes() {
    let api = runbook_contract::openapi();
    for request in [
        json!({"action":"accept"}),
        json!({"action":"reject"}),
        json!({"action":"archive"}),
        json!({"action":"edit","entry":{"title":"Baseline","body_markdown":"Updated data","tags":[],"step_id":null}}),
    ] {
        assert!(validator("MemoryPatchRequest").is_valid(&request));
        let patch: runbook_model::MemoryPatchRequest =
            serde_json::from_value(request.clone()).unwrap();
        assert_eq!(serde_json::to_value(patch).unwrap(), request);
    }
    for path in [
        "/v1/runbooks/{rb}/memory",
        "/v1/projects/{p}/memory",
        "/v1/memory/{mem}",
    ] {
        let methods = if path == "/v1/memory/{mem}" {
            ["get", "patch"]
        } else {
            ["get", "post"]
        };
        for method in methods {
            let op = &api["paths"][path][method];
            assert_eq!(op["x-implementation"], "implemented", "{method} {path}");
            let alternatives = op["x-authorization-alternatives"].as_array().unwrap();
            let scope = if method == "get" {
                "runbooks:read"
            } else {
                "runbooks:write"
            };
            assert_eq!(alternatives[0]["scopes"], json!([scope]));
            assert!(
                op.get("x-required-scopes").is_none(),
                "alternatives are OR, not AND"
            );
            let execution = &alternatives[1];
            assert_eq!(execution["credential"], "execution_access");
            assert_eq!(execution["resource"], "own_runbook_or_own_project");
            // MEM6: an execution credential saves live (or proposes when
            // asked) and only edits; reviewing and archiving stay with people.
            assert_eq!(
                execution["status"],
                if method == "post" {
                    "active_or_proposed"
                } else {
                    "active"
                }
            );
            if method == "post" {
                assert_eq!(execution["source"], "own_current_step_run");
            }
            if method == "patch" {
                assert_eq!(execution["actions"], json!(["edit"]));
                assert_eq!(
                    op["requestBody"]["content"]["application/json"]["schema"]["$ref"],
                    "#/components/schemas/MemoryPatchRequest"
                );
            }
        }
    }
    // MEM6: the revision history is for people's credentials only.
    let history = &api["paths"]["/v1/memory/{mem}/revisions"]["get"];
    assert_eq!(history["x-implementation"], "implemented");
    let alternatives = history["x-authorization-alternatives"].as_array().unwrap();
    assert_eq!(
        alternatives,
        &[json!({"credential":"scoped_principal","scopes":["runbooks:read"]})]
    );
    assert_eq!(
        history["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/MemoryRevisionPage"
    );
    // A save may ask to propose; the flag defaults to a live save.
    assert!(validator("SaveMemoryRequest").is_valid(
        &json!({"title":"T","body_markdown":"B","tags":[],"step_id":null,"propose":true})
    ));
}

// Catches document loss of provenance/amendments, missing memory audit references,
// or a waiting approval incorrectly rendered as already decided.
#[test]
fn execution_document_example_validates() {
    let schema = validator("ExecutionDocumentView");
    let raw = fixture("execution-document");
    assert!(schema.is_valid(&raw));
    let document: runbook_model::ExecutionDocumentView =
        serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(document).unwrap(), raw);
    assert_eq!(raw["header"]["status"], "waiting");
    assert_eq!(raw["header"]["result"], Value::Null);
    let sections = raw["sections"].as_array().unwrap();
    let paths: Vec<_> = sections
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "smoke.api_health",
            "smoke.login_flow",
            "regions[0].error_rate",
            "regions[1].error_rate",
            "regions[2].error_rate",
            "stabilize[0].recheck",
            "stabilize[1].recheck",
            "stabilize[2].recheck",
            "decide.signoff"
        ]
    );
    assert_eq!(sections[2]["memory_read"][0]["revision"], 2);
    assert_eq!(sections[0]["amended"], true);
    assert_eq!(sections[0]["amendments"].as_array().unwrap().len(), 1);
    assert_eq!(sections[8]["stale"], true);
    assert_eq!(sections[8]["approval"]["status"], "waiting");
    assert_eq!(sections[8]["approval"]["decisions"], json!([]));
    for (section, step) in sections
        .iter()
        .zip(raw["header"]["steps"].as_array().unwrap())
    {
        for key in [
            "path",
            "status",
            "verdict",
            "actor",
            "summary",
            "outputs",
            "amended",
            "amendments",
            "stale",
        ] {
            assert_eq!(section[key], step[key], "section/header {key}");
        }
        assert_eq!(section["step_run_id"], step["id"]);
    }
    // Separate policy variant: the gate is decided and the execution proceeds toward cleanup.
    // Its auto:true keep option is an explicit policy change from the primary fixture.
    let mut policy = raw.clone();
    policy["header"]["status"] = json!("running");
    policy["header"]["steps"][8]["status"] = json!("succeeded");
    policy["header"]["steps"][8]["actor"] = json!({"kind":"system"});
    policy["header"]["steps"][8]["outputs"] = json!({"decision":"keep","decided_by":"system"});
    policy["sections"][8]["status"] = json!("succeeded");
    policy["sections"][8]["actor"] = json!({"kind":"system"});
    policy["sections"][8]["outputs"] = json!({"decision":"keep","decided_by":"system"});
    policy["sections"][8]["approval"]["status"] = json!("succeeded");
    policy["sections"][8]["approval"]["options"][0]["auto"] = json!(true);
    policy["sections"][8]["approval"]["decisions"] =
        json!([{"by":"system","policy":"on_timeout","option":"keep"}]);
    assert_eq!(
        policy["sections"][8]["approval"]["decisions"][0]["policy"],
        "on_timeout"
    );
    assert_eq!(
        policy["sections"][8]["approval"]["decisions"][0]["by"],
        "system"
    );
    assert_eq!(policy["sections"][8]["outputs"]["decided_by"], "system");
    assert_eq!(
        policy["header"]["steps"][8]["outputs"]["decided_by"],
        "system"
    );
    assert!(schema.is_valid(&policy));
    let document: runbook_model::ExecutionDocumentView =
        serde_json::from_value(policy.clone()).unwrap();
    assert_eq!(serde_json::to_value(document).unwrap(), policy);
    let api = runbook_contract::openapi();
    assert_eq!(
        api["paths"]["/v1/executions/{exe}/report"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/ExecutionDocumentView"
    );
}
