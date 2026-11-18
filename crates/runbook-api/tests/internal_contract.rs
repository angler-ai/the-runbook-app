//! UPD1 review #8: the internal contract with the sign-in service holds
//! across one release in both directions (api/internal/README.md): this
//! side parses every recorded session answer (and one with a field it does
//! not know), and answers every recorded can-register request with
//! `{allowed: bool}`.
#[path = "support/fake_backends.rs"]
mod fake_backends;
use fake_backends::{request, router, run};
use serde_json::Value;
use std::path::Path;

fn fixtures(prefix: &str, suffix: &str) -> Vec<(String, Value)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../api/internal");
    let mut out: Vec<(String, Value)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix) && n.ends_with(suffix))
        .map(|n| {
            let v = serde_json::from_str(&std::fs::read_to_string(dir.join(&n)).unwrap()).unwrap();
            (n, v)
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    assert!(!out.is_empty(), "no {prefix}*{suffix} fixtures");
    out
}

#[test]
fn every_recorded_session_answer_parses() {
    for (name, body) in fixtures("auth-session.v", ".json") {
        let session =
            runbook_api::session::parse_session(&body).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(
            Some(session.email.as_str()),
            body["email"].as_str(),
            "{name}"
        );
        let mut grown = body.clone();
        grown["a_field_from_a_newer_release"] = Value::from(1);
        assert!(
            runbook_api::session::parse_session(&grown).is_some(),
            "{name} plus a field"
        );
    }
}

#[test]
fn every_recorded_can_register_request_gets_the_recorded_answer_shape() {
    let answers = fixtures("can-register.v", ".response.json");
    for (name, body) in fixtures("can-register.v", ".request.json") {
        let r =
            run(router(true).can_register(request("POST", "/internal/can-register", None, body)));
        assert_eq!(r.status, 200, "{name}: {:?}", r.body);
        for (answer, shape) in &answers {
            for (key, value) in shape.as_object().unwrap() {
                let ours = &r.body[key];
                assert_eq!(
                    std::mem::discriminant(ours),
                    std::mem::discriminant(value),
                    "{name} answered {key} unlike {answer}"
                );
            }
        }
    }
}
