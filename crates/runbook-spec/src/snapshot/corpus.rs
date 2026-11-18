//! Storage conformance rows shared by native tests and the existing Wasm probe.
use super::*;
use serde_json::{Value, json};

fn limits() -> Limits {
    Limits {
        bytes: 8_000_000,
        values: 400_000,
        depth: 1024,
    }
}
fn normalized(steps: Value) -> NormalizedRunbook {
    crate::normalize(&crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":{"steps":steps}}).to_string()).unwrap()).unwrap()
}
fn metrics(value: &Value) -> (usize, usize) {
    let mut pending = vec![(value, 0)];
    let mut count = 0;
    let mut maximum = 0;
    while let Some((v, depth)) = pending.pop() {
        count += 1;
        maximum = maximum.max(depth);
        match v {
            Value::Array(a) => pending.extend(a.iter().map(|v| (v, depth + 1))),
            Value::Object(o) => pending.extend(o.values().map(|v| (v, depth + 1))),
            _ => (),
        }
    }
    (count, maximum)
}
pub(crate) fn rows() -> Vec<Value> {
    let flagship = crate::normalize(
        &crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap(),
    )
    .unwrap();
    let mut opaque = json!({"$serde_json::private::Number":"1"});
    for _ in 0..56 {
        opaque = json!([opaque]);
    }
    let mut node = json!({"id":"leaf","title":"Leaf","agent":{"tools":opaque}});
    for i in 0..32 {
        node = json!({"id":format!("g{i}"),"steps":[node]});
    }
    let nested = normalized(json!([node]));
    let mut opaque = json!(0);
    for _ in 0..123 {
        opaque = json!([opaque]);
    }
    let deep_source = normalized(json!([{"id":"leaf","title":"Leaf","agent":{"tools":opaque}}]));
    assert_eq!(metrics(&deep_source.source_json).1, 128);
    let mut numbers = normalized(
        json!([{"id":"leaf","title":"Leaf","agent":{"tools":null,"commands":{"$serde_json::private::Number":"1"}}}]),
    );
    for text in [
        "9223372036854775808",
        "12345678901234567890123456789",
        "1.0",
        "1e-400",
        "-0.0",
        "1e+400",
    ] {
        numbers
            .variables
            .insert(text.into(), Value::Number(text.parse().unwrap()));
    }
    let mut results = vec![];
    for (name, ir) in [
        ("snapshot_flagship", flagship),
        ("snapshot_nested", nested),
        ("snapshot_source_depth128", deep_source),
        ("snapshot_exact_numbers", numbers),
    ] {
        let bindings = Bindings {
            skills: [("sample".into(), [17; 32])].into(),
            users: [(
                "sam".into(),
                "usr_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            )]
            .into(),
            teams: [(
                "oncall".into(),
                "team_01ARZ3NDEKTSV4RRFFQ69G5FAV".parse().unwrap(),
            )]
            .into(),
        };
        let project = crate::parse_declarations(&json!({})).unwrap();
        let bytes = encode(
            SnapshotRef {
                ir: &ir,
                bindings: &bindings,
                project_variables: &project,
                hash: [7; 32],
                worst_case_step_runs: 42,
            },
            limits(),
        )
        .unwrap();
        let decoded = decode(&bytes, limits()).unwrap();
        assert_eq!(decoded.ir, ir);
        assert_eq!(decoded.bindings, bindings);
        assert_eq!(decoded.project_variables, project);
        assert_eq!(decoded.hash, [7; 32]);
        assert_eq!(decoded.worst_case_step_runs, 42);
        let tree = wire::decode(&bytes, limits()).unwrap();
        let (values, depth) = metrics(tree.get());
        results.push(json!({"name":name,"bytes":bytes.len(),"values":values,"depth":depth}));
    }
    // Adversarial but structurally valid DTO: reach the typed recursion guard,
    // not just the early error path for an invalid root array. Public semantic
    // restoration must reject this source depth; this helper only proves safety.
    let ir = normalized(json!([]));
    let bindings = Bindings::default();
    let project = crate::parse_declarations(&json!({})).unwrap();
    let bytes = encode(
        SnapshotRef {
            ir: &ir,
            bindings: &bindings,
            project_variables: &project,
            hash: [0; 32],
            worst_case_step_runs: 0,
        },
        limits(),
    )
    .unwrap();
    let mut value = wire::decode(&bytes, limits()).unwrap().into_value();
    let mut stored = json!("Null");
    for _ in 0..255 {
        stored = json!({"Array":[stored]});
    }
    value["ir"]["source_json"] = stored;
    assert_eq!(metrics(&value).1, 512);
    let bytes = wire::encode(&value, limits()).unwrap();
    let decoded = decode(&bytes, limits()).unwrap();
    assert_eq!(metrics(&decoded.ir.source_json).1, 255);
    let mut leaf = &mut value["ir"]["source_json"];
    for _ in 0..255 {
        leaf = &mut leaf["Array"][0];
    }
    *leaf = json!("Future");
    let bytes = wire::encode(&value, limits()).unwrap();
    assert!(matches!(decode(&bytes, limits()), Err(Error::Invalid)));
    results.push(
        json!({"name":"snapshot_typed_depth512","decoded_depth":255,"malformed_leaf":"invalid"}),
    );
    // Exercise recursive typed error cleanup at the safety boundary in Wasm too.
    let mut value = json!(null);
    for _ in 0..512 {
        value = json!([value]);
    }
    let bytes = wire::encode(&value, limits()).unwrap();
    assert!(matches!(decode(&bytes, limits()), Err(Error::Invalid)));
    value = json!([value]);
    let bytes = wire::encode(&value, limits()).unwrap();
    assert!(matches!(decode(&bytes, limits()), Err(Error::Limit)));
    results.push(json!({"name":"snapshot_depth_guard","at512":"invalid","at513":"limit"}));
    results
}
