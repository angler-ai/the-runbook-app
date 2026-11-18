//! Native behaviour of the browser compiler's JSON operations.
mod support;
use runbook_spec::authoring::{CompileDirectory, compile_context};
use serde_json::{Value, json};
use support::*;

fn server_validate(source: &str, directory: &Value) -> Value {
    let directory: CompileDirectory = serde_json::from_value(directory.clone()).unwrap();
    let context = compile_context(&directory, source).unwrap();
    json!({"ok": runbook_api::authoring::validate(source, &context)})
}

#[test]
fn validate_is_the_v1_validate_body() {
    let source = flagship();
    let directory = flagship_directory();
    let out = call("validate", &json!({"source": source, "context": directory}));
    assert_eq!(out, json!({"ok": {"diagnostics": [], "step_budget": 20}}));
    assert_eq!(out, server_validate(&source, &directory));
    // No context is the server's project-less default: the flagship's teams
    // and skills are then unknown.
    let bare = call("validate", &json!({"source": source}));
    assert_eq!(bare, server_validate(&source, &json!({})));
    assert_eq!(bare["ok"]["step_budget"], Value::Null);
    assert!(!bare["ok"]["diagnostics"].as_array().unwrap().is_empty());
}

#[test]
fn validate_reports_one_based_spans() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: [unclosed\n";
    let out = call("validate", &json!({"source": source}));
    let d = &out["ok"]["diagnostics"][0];
    assert!(d["line"].as_u64().is_some_and(|l| l >= 1), "{out}");
    assert!(d["column"].as_u64().is_some_and(|c| c >= 1), "{out}");
    assert_eq!(out, server_validate(source, &json!({})));
}

#[test]
fn describe_is_the_version_form() {
    let source = flagship();
    let out = call("describe", &json!({"source": source}));
    let compiled = runbook_spec::normalize(&runbook_spec::parse(&source).unwrap()).unwrap();
    assert_eq!(
        out,
        json!({"ok": {
            "runbook": {
                "id": "post-deploy",
                "title": "Post-deploy verification",
                "inputs": runbook_spec::schema_json(&compiled.inputs),
                "fields": runbook_spec::schema_json(&compiled.fields),
            },
            "diagnostics": [],
        }})
    );
    assert_eq!(
        out["ok"]["runbook"]["inputs"]["required"],
        json!(["service", "version"])
    );
}

#[test]
fn outline_and_describe_return_diagnostics_when_the_source_does_not_normalize() {
    for (op, key) in [("outline", "outline"), ("describe", "runbook")] {
        let out = call(op, &json!({"source": "kind: Runbook\n"}));
        assert_eq!(out["ok"][key], Value::Null, "{out}");
        assert!(!out["ok"]["diagnostics"].as_array().unwrap().is_empty());
    }
}

#[test]
fn abi_errors_are_envelopes_not_traps() {
    let code = |op: &[u8], input: &[u8]| -> Value {
        let out: Value = serde_json::from_slice(&runbook_web_wasm::call(op, input)).unwrap();
        assert!(out.get("ok").is_none(), "{out}");
        assert!(out["error"]["message"].is_string(), "{out}");
        out["error"]["code"].clone()
    };
    assert_eq!(code(b"compile", b"{}"), "abi.unknown_operation");
    assert_eq!(code(&[0xff], b"{}"), "abi.unknown_operation");
    assert_eq!(code(b"validate", b"{"), "abi.invalid_input");
    assert_eq!(code(b"validate", b"{}"), "abi.invalid_input");
    assert_eq!(
        code(b"outline", br#"{"source":"","extra":1}"#),
        "abi.invalid_input"
    );
    assert_eq!(
        code(
            b"validate",
            br#"{"source":"","context":{"skills":[{"name":"x","hash":"00"}]}}"#
        ),
        "abi.invalid_input"
    );
    let big = json!({"source": " ".repeat(runbook_spec::authoring::MAX_SOURCE_BYTES + 1)});
    for op in ["validate", "outline", "describe"] {
        assert_eq!(
            code(op.as_bytes(), &serde_json::to_vec(&big).unwrap()),
            "spec.source_limit"
        );
    }
    let bad_vars = json!({"source": flagship(), "context": {"variables": {"x": {"type": "nope"}}}});
    assert_eq!(
        code(b"validate", &serde_json::to_vec(&bad_vars).unwrap()),
        "vars.invalid_stored_declarations"
    );
}

/// U29: an input over MAX_INPUT_BYTES is refused before it is parsed (the
/// host refuses it before copying; this is the module's own guard), so a
/// huge request never grows linear memory by its parsed size.
#[test]
fn oversized_input_is_refused_before_parsing() {
    let limit = runbook_web_wasm::MAX_INPUT_BYTES;
    assert!(
        limit >= 6 * runbook_spec::authoring::MAX_SOURCE_BYTES,
        "a 1 MiB source escapes to at most 6 MiB of JSON"
    );
    // Not even JSON: parsed, this would be abi.invalid_input.
    for op in ["validate", "outline", "describe"] {
        let out: Value = serde_json::from_slice(&runbook_web_wasm::call(
            op.as_bytes(),
            &vec![b' '; limit + 1],
        ))
        .unwrap();
        assert_eq!(out["error"]["code"], "abi.input_limit", "{op}");
    }
    // At the limit, the input is parsed as before.
    let out: Value =
        serde_json::from_slice(&runbook_web_wasm::call(b"outline", &vec![b' '; limit])).unwrap();
    assert_eq!(out["error"]["code"], "abi.invalid_input");
}
