//! Shared fixtures for the browser compiler tests.
#![allow(dead_code)]
use serde_json::{Value, json};
use std::path::PathBuf;

pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
pub fn flagship() -> String {
    std::fs::read_to_string(repo().join("conformance/spec/post-deploy.yaml")).unwrap()
}
/// The directory the flagship publishes against (teams and library skills).
pub fn flagship_directory() -> Value {
    json!({
        "users": [
            {"id": "usr_01ARZ3NDEKTSV4RRFFQ69G5FA1", "name": "Alice"},
            {"id": "usr_01ARZ3NDEKTSV4RRFFQ69G5FA2", "name": "Bob"},
            {"id": "usr_01ARZ3NDEKTSV4RRFFQ69G5FA3", "name": "Carol", "active": false}
        ],
        "teams": [
            {"id": "team_01ARZ3NDEKTSV4RRFFQ69G5FAV", "slug": "oncall"},
            {"id": "team_01ARZ3NDEKTSV4RRFFQ69G5FAW", "slug": "eng-leads"}
        ],
        "skills": [
            {"name": "check-error-rates", "hash": "01".repeat(32)},
            {"name": "rollback-service", "hash": "02".repeat(32), "reference": "./skills/rollback-service"}
        ],
        "variables": {}
    })
}
pub fn call(op: &str, input: &Value) -> Value {
    let out = runbook_web_wasm::call(op.as_bytes(), &serde_json::to_vec(input).unwrap());
    serde_json::from_slice(&out).unwrap()
}
/// Compare `actual` with the JSON fixture at `relative` (from the repo root).
/// With `UPDATE_FIXTURES=1`, write `actual` there instead: the compiler is
/// the truth for the web fixtures.
pub fn assert_fixture(relative: &str, actual: &Value) {
    let path = repo().join(relative);
    if std::env::var_os("UPDATE_FIXTURES").is_some_and(|v| v == "1") {
        let mut text = serde_json::to_string_pretty(actual).unwrap();
        text.push('\n');
        std::fs::write(&path, text).unwrap();
        return;
    }
    let fixture: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        actual,
        &fixture,
        "{relative} is stale (regenerate with UPDATE_FIXTURES=1). Compiler output:\n{}",
        serde_json::to_string_pretty(actual).unwrap()
    );
}
