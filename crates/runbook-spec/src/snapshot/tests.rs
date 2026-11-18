use super::*;
use serde_json::{Value, json};
fn limits() -> Limits {
    Limits {
        bytes: 8_000_000,
        values: 400_000,
        depth: 512,
    }
}
fn empty_project() -> DataSchema {
    crate::parse_declarations(&json!({})).unwrap()
}
fn normalized(body: Value) -> NormalizedRunbook {
    crate::normalize(&crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":body}).to_string()).unwrap()).unwrap()
}
fn roundtrip(ir: &NormalizedRunbook, project: &DataSchema) -> (Vec<u8>, UnvalidatedSnapshot) {
    let bindings = Bindings::default();
    let snapshot = SnapshotRef {
        ir,
        bindings: &bindings,
        project_variables: project,
        hash: [7; 32],
        worst_case_step_runs: 42,
    };
    let bytes = encode(snapshot, limits()).unwrap();
    let decoded = decode(&bytes, limits()).unwrap();
    assert_eq!(&decoded.ir, ir);
    assert_eq!(&decoded.project_variables, project);
    assert_eq!(decoded.bindings, bindings);
    assert_eq!(decoded.hash, [7; 32]);
    assert_eq!(decoded.worst_case_step_runs, 42);
    (bytes, decoded)
}
#[test]
fn typed_snapshot_roundtrips_flagship() {
    let ir = crate::normalize(
        &crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap(),
    )
    .unwrap();
    roundtrip(&ir, &empty_project());
}
#[test]
fn typed_snapshot_preserves_exact_opaque_values() {
    let marker = json!({"$serde_json::private::Number":"1"});
    let mut ir = normalized(
        json!({"agents":{"params":{"opaque":marker}},"steps":[{"id":"task","title":"Task","with":{"payload":marker},"agent":{"tools":null,"commands":marker}}]}),
    );
    ir.source_json["x-marker"] = marker.clone();
    ir.inputs
        .extensions
        .insert("x-marker".into(), marker.clone());
    ir.variables.insert("marker".into(), marker.clone());
    ir.outputs.insert("marker".into(), marker.clone());
    for n in [
        "9223372036854775808",
        "12345678901234567890123456789",
        "1.0",
        "1e-400",
        "-0.0",
        "1e+400",
    ] {
        ir.variables
            .insert(n.into(), Value::Number(n.parse().unwrap()));
    }
    roundtrip(&ir, &empty_project());
    let crate::ir::NodeKind::Task(task) = &mut ir.steps[0].kind else {
        panic!()
    };
    task.agent.tools = None;
    task.outputs.default = crate::DefaultValue::Value(Value::Null);
    roundtrip(&ir, &empty_project());
}
#[test]
fn typed_snapshot_rejects_unknown_fields_and_versions() {
    let ir = normalized(json!({"steps":[]}));
    let (bytes, _) = roundtrip(&ir, &empty_project());
    for (pointer, replacement) in [
        ("/format", json!(2)),
        ("/profile", json!("future")),
        ("/cost_model", json!("future")),
        ("/extra", json!(true)),
        ("/ir/metadata/extra", json!(true)),
        ("/ir/inputs/extra", json!(true)),
        ("/bindings/extra", json!(true)),
        ("/bindings/users/bad", json!("not-an-id")),
    ] {
        let mut value = wire::decode(&bytes, limits()).unwrap().get().clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        value
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), replacement);
        let bad = wire::encode(&value, limits()).unwrap();
        assert!(decode(&bad, limits()).is_err(), "accepted {pointer}");
    }
}
#[test]
fn typed_snapshot_roundtrips_deep_boundaries() {
    let mut opaque = json!(0);
    for _ in 0..56 {
        opaque = json!([opaque]);
    }
    let mut node = json!({"id":"leaf","title":"Leaf","agent":{"tools":opaque}});
    for i in 0..32 {
        node = json!({"id":format!("g{i}"),"steps":[node]});
    }
    let ir = normalized(json!({"steps":[node]}));
    roundtrip(&ir, &empty_project());
    let mut declaration = json!({"type":"string"});
    for _ in 0..64 {
        declaration = json!({"type":"list","items":declaration});
    }
    let standalone = crate::parse_schema(&declaration).unwrap();
    // Shape-only regression: A5c's complete scope has a stricter type depth.
    roundtrip(&normalized(json!({"steps":[]})), &standalone);
}

#[test]
fn typed_snapshot_roundtrips_companion_variants() {
    let ir = normalized(json!({
        "inputs": {
            "text":{"type":"text"},"string":{"type":"string","default":"x"},
            "number":{"type":"number","min":0.1,"max":2.5},"integer":{"type":"integer"},
            "boolean":{"type":"boolean"},"enum":{"type":"enum","options":["a","b"]},
            "list":{"type":"list","items":{"type":"string"}},
            "object":{"type":"object","properties":{"n":{"type":"integer"}}}
        },
        "execution":{"concurrency":{"key":"fixed","limit":1}},
        "steps":[
            {"id":"group","steps":[{"id":"task","title":"Task","actor":"any","assignee":"user:sam","instructions":{"default":"D","human":"H","agent":"A"},"retry":{"max_attempts":2,"backoff":"1s"},"on_failure":"continue","evidence":{"required":["log"],"accepted":["link","log"]}}],"catch":[{"id":"catch","wait":{"for":"1s"}}],"finally":[{"id":"cleanup","wait":{"until":"2026-11-09T00:00:00Z"}}]},
            {"id":"signal","wait":{"signal":"go","timeout":"1h"}},
            {"id":"gate","approval":{"prompt":"Choose","options":[{"id":"rollback","auto":true}],"on_timeout":{"decide":"rollback"},"timeout":"1m","on_expired":"fail","allow_agents":true}},
            {"id":"set","set":{"vars":{"x":1},"fields":{"severity":"low"}}},
            {"id":"end","end":{"status":"cancelled","message":"Done"}}
        ]
    }));
    roundtrip(&ir, &empty_project());
}

#[test]
fn typed_conversion_depth_cannot_be_bypassed_by_caller_limits() {
    let mut value = json!(null);
    for _ in 0..513 {
        value = json!([value]);
    }
    let permissive = Limits {
        depth: 1024,
        ..limits()
    };
    let bytes = wire::encode(&value, permissive).unwrap();
    assert!(matches!(decode(&bytes, permissive), Err(Error::Limit)));
}
