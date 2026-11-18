//! Actual private-pass native/Wasm corpus. Expectations are asserted here before
//! exporting results, so parity alone cannot bless identical wrong outcomes.
use super::*;
use serde_json::json;
fn compile(body: Value) -> Result<CheckedScopes> {
    let s=crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"corpus","title":"Corpus"},"spec":body}).to_string()).unwrap();
    check(
        &crate::normalize(&s).unwrap(),
        &s.source_spans,
        &crate::parse_declarations(&json!({})).unwrap(),
    )
}
fn kind(t: ResultType) -> &'static str {
    match t {
        ResultType::String => "String",
        ResultType::Bool => "Bool",
        ResultType::Int => "Int",
        ResultType::Double => "Double",
        ResultType::Timestamp => "Timestamp",
        ResultType::List(_) => "List",
        ResultType::Record { .. } => "Record",
        ResultType::Nullable(_) => "Nullable",
        ResultType::Null => "Null",
    }
}
fn fingerprint(env: &TypeEnv) -> String {
    env.fingerprint()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub(crate) fn corpus() -> Value {
    let s = crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap();
    let c = check(
        &crate::normalize(&s).unwrap(),
        &s.source_spans,
        &crate::parse_declarations(&json!({})).unwrap(),
    )
    .unwrap();
    let kinds: Vec<_> = c
        .sites
        .iter()
        .flat_map(|s| {
            s.template
                .expressions()
                .map(|e| kind(e.expression.result_type()))
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "String", "String", "String", "String", "List", "String", "String", "Bool", "Bool",
            "Bool", "String", "String", "Bool", "String", "String"
        ]
    );
    assert_eq!(c.sites.len(), 14);
    assert_eq!(c.environments.len(), 3);
    let inventory:Vec<_>=c.sites.iter().map(|s|{
        let env=&c.environments[c.scopes[s.scope].environment].handle;
        let expressions:Vec<_>=s.template.expressions().map(|e|{
            assert_eq!(e.expression.descriptor().schema_fingerprint,env.fingerprint());
            json!({"type":kind(e.expression.result_type()),"static":e.expression.static_dependencies().iter().map(|d|json!([d.root,d.member])).collect::<Vec<_>>()})
        }).collect();
        json!({"path":s.path,"scope":s.scope,"fingerprint":fingerprint(env),"expressions":expressions})
    }).collect();
    let static_refs: Vec<_> = inventory
        .iter()
        .flat_map(|s| {
            s["expressions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["static"].clone())
        })
        .collect();
    assert_eq!(
        static_refs,
        json!([
            [["inputs", "service"]],
            [["inputs", "version"]],
            [["inputs", "service"]],
            [["inputs", "service"]],
            [["inputs", "regions"]],
            [["region", null]],
            [["region", null]],
            [["steps", "regions"]],
            [["steps", "recheck"]],
            [
                ["steps", "smoke"],
                ["steps", "regions"],
                ["steps", "stabilize"]
            ],
            [["inputs", "version"]],
            [["inputs", "service"]],
            [["steps", "signoff"]],
            [["steps", "signoff"]],
            [["vars", "result"]]
        ])
        .as_array()
        .unwrap()
        .as_slice()
    );
    assert_eq!(
        c.sites.iter().map(|s| s.scope).collect::<Vec<_>>(),
        [0, 0, 0, 0, 1, 1, 0, 2, 0, 0, 0, 0, 0, 0]
    );
    assert_eq!(
        c.sites[3]
            .template
            .expressions()
            .next()
            .unwrap()
            .expression
            .result_type(),
        ResultType::List(Box::new(ResultType::String))
    );
    let mut rows = vec![
        json!({"name":"flagship","inventory":inventory,"types":c.resources.type_slots,"fields":c.resources.field_slots}),
    ];
    for (name, number, kind, valid) in [
        ("int_decimal", "1.0", "integer", true),
        ("int_max", "9223372036854775807", "integer", true),
        ("int_overflow", "9223372036854775808", "integer", false),
        ("number_integer", "1", "number", true),
        ("number_underflow", "1e-400", "number", true),
        ("number_overflow", "1.8e308", "number", false),
    ] {
        let value: Value = serde_json::from_str(number).unwrap();
        let result = compile(
            json!({"fields":{"n":{"type":kind}},"steps":[{"id":"assign","set":{"fields":{"n":value}}}]}),
        );
        assert_eq!(result.is_ok(), valid, "{name}");
        rows.push(json!({"name":name,"outcome":result.map(|_|"ok".to_string()).unwrap_or_else(|e|e.code)}));
    }
    let vars = compile(json!({"vars":{"b":"${{ vars.a }}","a":"seed"},"steps":[]})).unwrap();
    assert_eq!(vars.initialization_order, ["a", "b"]);
    rows.push(json!({"name":"forward_vars","order":vars.initialization_order,"fingerprint":fingerprint(&vars.environments[0].handle)}));
    for (name, body, code, path) in [
        (
            "cycle",
            json!({"vars":{"a":"${{ vars.b }}","b":"${{ vars.a }}"},"steps":[]}),
            "spec.variable_cycle",
            "/spec/vars/a",
        ),
        (
            "unknown_var",
            json!({"vars":{"a":"${{ vars.missing }}"},"steps":[]}),
            "spec.unknown_variable",
            "/spec/vars/a",
        ),
        (
            "outside_alias",
            json!({"steps":[{"id":"a","title":"${{ region }}"}]}),
            "expression.type_error",
            "/spec/steps/0/title",
        ),
        (
            "outside_iteration",
            json!({"steps":[{"id":"a","title":"${{ iteration }}"}]}),
            "expression.type_error",
            "/spec/steps/0/title",
        ),
        (
            "outside_outputs",
            json!({"steps":[{"id":"a","title":"${{ outputs.result }}"}]}),
            "expression.type_error",
            "/spec/steps/0/title",
        ),
    ] {
        let e = compile(body).unwrap_err();
        assert_eq!(e.code, code);
        assert_eq!(e.path, path);
        rows.push(json!({"name":name,"code":e.code,"path":e.path}));
    }
    let combined = compile(combined_fixture(64, 5, 14)).unwrap();
    assert_eq!(combined.discovery.segments, 1024);
    assert_eq!(combined.environments.len(), 65);
    rows.push(json!({"name":"combined_boundary","environments":combined.environments.len(),"segments":combined.discovery.segments,"type_slots":combined.resources.type_slots,"field_slots":combined.resources.field_slots,"source_bytes":combined.discovery.bytes}));
    drop(combined);
    for (name, accepted, rejected, width, want_types, want_fields, path) in [
        (
            "field_boundary",
            78,
            79,
            5,
            53_088,
            259_748,
            "/spec/steps/78/foreach",
        ),
        (
            "type_boundary",
            96,
            97,
            1,
            65_184,
            151_704,
            "/spec/steps/96/foreach",
        ),
    ] {
        let c = compile(combined_fixture(accepted, width, 0)).unwrap();
        assert_eq!(c.resources.type_slots, want_types);
        assert_eq!(c.resources.field_slots, want_fields);
        rows.push(json!({"name":name,"accepted_scopes":c.environments.len(),"type_slots":c.resources.type_slots,"field_slots":c.resources.field_slots}));
        drop(c);
        let e = compile(combined_fixture(rejected, width, 0)).unwrap_err();
        assert_eq!(e.code, "spec.compile_limit");
        assert_eq!(e.path, path);
        rows.push(json!({"name":format!("{name}_rejected"),"code":e.code,"path":e.path}));
    }
    for root in ["vars", "outputs"] {
        let mut body = json!({"steps":[{"id":"a","title":"A"}]});
        body[root] = json!({"xs":["${{ steps.a.verdict }}"]});
        if root == "outputs" {
            body["report"] = json!("${{ outputs.xs[0] }}");
        }
        let error = compile(body).unwrap_err();
        assert_eq!(error.code, "spec.uninferable_type");
        assert_eq!(error.path, format!("/spec/{root}/xs"));
        rows.push(
            json!({"name":format!("nullable_array_{root}"),"code":error.code,"path":error.path}),
        );
    }
    let list = json!({"type":"list","items":{"type":"object",
        "properties":{"a":{"type":"string"}}}});
    for target in ["fields", "vars"] {
        let mut body = json!({"inputs":{"xs":list},"fields":{"xs":list},
            "vars":{"xs":"${{ inputs.xs }}","item":{"a":"valid"}},
            "steps":[{"id":"s","set":{}}]});
        body["steps"][0]["set"][target] = json!({"xs":[{}, {"a":"valid"}, "${{ vars.item }}"]});
        compile(body).unwrap();
        rows.push(json!({"name":format!("optional_record_list_{target}"),"outcome":"ok"}));
    }
    compile(
        json!({"outputs":{"xs":[{"value":"${{ steps.a.verdict }}"}]},
        "report":"Value: ${{ outputs.xs[0].value }}",
        "steps":[{"id":"a","title":"A"}]}),
    )
    .unwrap();
    rows.push(json!({"name":"nullable_member_in_record_list","outcome":"ok"}));
    assert_eq!(rows.len(), 23);
    Value::Array(rows)
}
pub(super) fn combined_fixture(scopes: usize, width: usize, title_segments: usize) -> Value {
    let mut inputs = serde_json::Map::new();
    inputs.insert(
        "names".into(),
        json!({"type":"list","items":{"type":"string"}}),
    );
    for i in 0..220 {
        let inner: serde_json::Map<_, _> = (0..width)
            .map(|j| (format!("f{i}_{j}"), json!({"type":"integer"})))
            .collect();
        inputs.insert(format!("deep{i}"),json!({"type":"object","properties":{"items":{"type":"list","items":{"type":"object","properties":inner}}}}));
    }
    // A balanced 128-leaf conjunction has 255 AST nodes, within the unchanged
    // 256-node/32-depth cap. 896 such segments exercise the AST multiplier as
    // well as the 1,024-segment and many-environment allowances.
    let mut dense = "true".to_string();
    for _ in 0..7 {
        dense = format!("({dense}&&{dense})");
    }
    let title = format!("${{{{ {dense} }}}}").repeat(title_segments);
    let nodes:Vec<_>=(0..scopes).map(|i|json!({"id":format!("n{i}"),"title":title,"foreach":{"items":"${{ inputs.names }}","as":format!("a{i}"),"key":format!("${{{{ a{i} }}}}"),"max_items":1,"steps":[]}})).collect();
    json!({"inputs":inputs,"steps":nodes})
}
