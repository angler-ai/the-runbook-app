//! Joint admission tests: storage probes alone do not certify a compilable tree.
use crate::*;
use serde_json::{Value, json};
fn source(steps: Value) -> Value {
    json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"steps":steps}})
}
fn roundtrip(value: Value) -> (usize, usize) {
    roundtrip_with_project(value, &parse_declarations(&json!({})).unwrap())
}
fn roundtrip_with_project(value: Value, project: &DataSchema) -> (usize, usize) {
    let spec = parse(&value.to_string()).unwrap();
    let ir = normalize(&spec).unwrap();
    scopes::check(&ir, &spec.source_spans, project).unwrap();
    let pins = bindings::Bindings::default();
    let limits = wire::Limits {
        bytes: 64 * 1024 * 1024,
        values: 2_000_000,
        depth: 512,
    };
    let encoded = snapshot::encode(
        snapshot::SnapshotRef {
            ir: &ir,
            bindings: &pins,
            project_variables: project,
            hash: [0; 32],
            worst_case_step_runs: estimate_step_runs(&ir).unwrap(),
        },
        limits,
    )
    .unwrap();
    let decoded = snapshot::decode(&encoded, limits).unwrap();
    assert_eq!(decoded.ir, ir);
    scopes::check(&decoded.ir, &spec.source_spans, &decoded.project_variables).unwrap();
    let context = CompileContext {
        project_variables: project.clone(),
        ..Default::default()
    };
    let compiled = compile(&spec, &context).unwrap();
    let restored = restore(compiled.snapshot_bytes()).unwrap();
    assert_eq!(restored.ir(), compiled.ir());
    assert_eq!(restored.hash(), compiled.hash());
    (spec.raw_source.len(), encoded.len())
}
#[test]
fn admitted_source_and_scope_boundaries_roundtrip_storage() {
    let mut data = json!({"type":"string"});
    for _ in 0..15 {
        data = json!({"type":"list","items":data});
    }
    let mut v = source(json!([]));
    v["spec"]["inputs"] = json!({"deep":data});
    roundtrip(v);
    let tasks: Vec<_> = (0..256)
        .map(|i| json!({"id":format!("t{i}"),"title":"Task"}))
        .collect();
    roundtrip(source(json!(tasks)));
    let mut opaque = json!(0);
    for _ in 0..56 {
        opaque = json!([opaque]);
    }
    let mut node = json!({"id":"leaf","title":"Leaf","agent":{"tools":opaque}});
    for i in 0..32 {
        node = json!({"id":format!("g{i}"),"steps":[node]});
    }
    roundtrip(source(json!([node])));
    let mut opaque = json!(0);
    for _ in 0..123 {
        opaque = json!([opaque]);
    }
    roundtrip(source(
        json!([{"id":"leaf","title":"Leaf","agent":{"tools":opaque}}]),
    ));
    let mut v = source(json!([]));
    v["x-padding"] = json!("");
    let padding = normalize::MAX_SOURCE_BYTES - v.to_string().len();
    v["x-padding"] = json!("x".repeat(padding));
    let (source_bytes, wire_bytes) = roundtrip(v);
    assert_eq!(source_bytes, normalize::MAX_SOURCE_BYTES);
    println!("source_byte_boundary: source={source_bytes}, wire={wire_bytes}");
}

#[test]
fn admitted_source_value_boundary_roundtrips_storage() {
    let mut v = source(json!([{"id":"leaf","title":"Leaf","agent":{"tools":[]}}]));
    fn count(v: &Value) -> usize {
        1 + match v {
            Value::Array(a) => a.iter().map(count).sum(),
            Value::Object(o) => o.values().map(count).sum(),
            _ => 0,
        }
    }
    let count = normalize::MAX_SOURCE_VALUES - count(&v);
    v["spec"]["steps"][0]["agent"]["tools"] = Value::Array(vec![json!(""); count]);
    let (source_bytes, wire_bytes) = roundtrip(v);
    println!("source_value_boundary: source={source_bytes}, wire={wire_bytes}");
    let mut v = source(json!([{ "id":"leaf", "title":"Leaf", "agent":{"tools":[]} }]));
    v["spec"]["steps"][0]["agent"]["tools"] = Value::Array(vec![json!(""); count]);
    let declaration =
        json!({"values":{"type":"list","items":{"type":"string"},"default":vec!["";262_144]}});
    assert!(declaration.to_string().len() < 1_048_576);
    let project = parse_declarations(&declaration).unwrap();
    let (_, combined_bytes) = roundtrip_with_project(v, &project);
    assert!(combined_bytes > 32 * 1024 * 1024);
    println!("combined_source_and_project: wire={combined_bytes}");
}
