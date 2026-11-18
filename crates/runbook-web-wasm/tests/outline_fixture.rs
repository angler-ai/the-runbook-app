//! The hand-written outline fixture the web components are built on
//! (`web/src/ui/outline/fixtures/post-deploy.outline.json`) is exactly what the
//! compiler emits for the flagship. The compiler is the truth.
mod support;
use serde_json::{Value, json};
use support::*;

#[test]
fn flagship_outline_matches_the_web_fixture() {
    // The outline needs no directory: it is the same with and without one.
    let bare = call("outline", &json!({"source": flagship()}));
    let with = call(
        "outline",
        &json!({"source": flagship(), "context": flagship_directory()}),
    );
    assert_eq!(bare, with);
    assert_eq!(bare["ok"]["diagnostics"], json!([]));
    assert_fixture(
        "web/src/ui/outline/fixtures/post-deploy.outline.json",
        &bare["ok"]["outline"],
    );
}

#[test]
fn the_web_flagship_mirror_is_byte_identical() {
    // web/src/wasm/runbook.test.ts runs the built module on this copy (Vite
    // serves only web/), so it must stay the conformance flagship.
    let mirror =
        std::fs::read_to_string(repo().join("web/src/wasm/fixtures/post-deploy.yaml.txt")).unwrap();
    assert_eq!(mirror, flagship());
}

#[test]
fn every_outline_kind_is_reachable() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/every-kind.yaml"),
    )
    .unwrap();
    let context = json!({"users": [{"id": "usr_01ARZ3NDEKTSV4RRFFQ69G5FA1", "name": "Alice"}]});
    let valid = call("validate", &json!({"source": source, "context": context}));
    assert_eq!(valid["ok"]["diagnostics"], json!([]), "{valid}");
    let out = call("outline", &json!({"source": source}));
    fn kinds(nodes: &Value, out: &mut std::collections::BTreeSet<String>) {
        for n in nodes.as_array().unwrap() {
            out.insert(n["kind"].as_str().unwrap().to_owned());
            if let Some(children) = n.get("children") {
                kinds(children, out);
            }
            for b in n
                .get("branches")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                kinds(&b["children"], out);
            }
        }
    }
    let mut seen = Default::default();
    kinds(&out["ok"]["outline"]["steps"], &mut seen);
    let flagship = call("outline", &json!({"source": flagship()}));
    kinds(&flagship["ok"]["outline"]["steps"], &mut seen);
    assert_eq!(
        seen.into_iter().collect::<Vec<_>>(),
        [
            "approval", "end", "foreach", "group", "loop", "parallel", "set", "switch", "task",
            "wait"
        ]
    );
    let prepare = &out["ok"]["outline"]["steps"][0];
    assert_eq!(prepare["branches"][0]["label"], "catch");
    assert_eq!(prepare["branches"][1]["label"], "finally");
    assert_eq!(prepare["children"][1]["hint"], "Wait 1h30m");
    assert_eq!(out["ok"]["outline"]["steps"][3]["actor"], "any");
    assert_eq!(
        out["ok"]["outline"]["steps"][5]["hint"],
        "Repeat up to 1 time"
    );
}

#[test]
fn parallel_hints_name_their_join() {
    let source = |join: &str| {
        format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: j, title: J}}\nspec:\n  steps:\n    - id: fan\n      join: {join}\n      parallel: [{{id: a, title: A}}, {{id: b, title: B}}]\n"
        )
    };
    for (join, hint) in [
        ("all", "Run in parallel"),
        ("any", "Run in parallel until one succeeds"),
        ("{at_least: 2}", "Run in parallel until 2 succeed"),
    ] {
        let out = call("outline", &json!({"source": source(join)}));
        assert_eq!(
            out["ok"]["outline"]["steps"][0]["hint"],
            json!(hint),
            "{out}"
        );
    }
}

/// LANG7: a runbook with only IDs validates, and the outline shows the
/// titles derived from them, as the server's compiled definition does.
#[test]
fn a_lean_runbook_outlines_with_derived_titles() {
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lean.yaml"),
    )
    .unwrap();
    let valid = call("validate", &json!({"source": source}));
    assert_eq!(valid["ok"]["diagnostics"], json!([]), "{valid}");
    let out = call("outline", &json!({"source": source}));
    let outline = &out["ok"]["outline"];
    assert_eq!(outline["title"], "Check health", "{out}");
    let titles: Vec<&str> = outline["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Ping api", "Confirm"], "{out}");
    let server = runbook_spec::compile(
        &runbook_spec::parse(&source).unwrap(),
        &runbook_spec::CompileContext::default(),
    )
    .unwrap();
    assert_eq!(server.ir().metadata.title, "Check health");
    assert_eq!(server.ir().steps[0].title.as_deref(), Some("Ping api"));
}
