//! The fixtures the runner's Wasm tests use (`redact_fixtures.json`, the
//! cases of runbook-core's `tests/redact.rs`) hold natively too, at every
//! split point, through this crate's handle API.
use runbook_redact_wasm::{drop_redactor, finish, new_redactor, push};

#[test]
fn fixtures_match_the_core_redactor() {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("redact_fixtures.json")).expect("fixtures parse");
    let cases = doc["cases"].as_array().expect("cases");
    assert!(cases.len() >= 30);
    for case in cases {
        let input = case["input"].as_str().expect("input");
        let expected = case["expected"].as_str().expect("expected");
        for split in (0..=input.len()).filter(|i| input.is_char_boundary(*i)) {
            let h = new_redactor();
            let mut actual = push(h, &input.as_bytes()[..split]).expect("live handle");
            actual.push_str(&push(h, &input.as_bytes()[split..]).expect("live handle"));
            actual.push_str(&finish(h).expect("live handle"));
            drop_redactor(h);
            assert_eq!(actual, expected, "split={split} input={input:?}");
        }
    }
}

#[test]
fn handles_are_independent_and_reused() {
    let a = new_redactor();
    let b = new_redactor();
    assert_ne!(a, b);
    let mut out_a = push(a, b"password=").expect("live");
    let mut out_b = push(b, b"plain text ").expect("live");
    out_a.push_str(&push(a, b"hunter2 next").expect("live"));
    out_b.push_str(&push(b, b"more").expect("live"));
    out_a.push_str(&finish(a).expect("live"));
    out_b.push_str(&finish(b).expect("live"));
    assert_eq!(out_a, "password=[REDACTED:password] next");
    assert_eq!(out_b, "plain text more");
    drop_redactor(a);
    assert_eq!(push(a, b"x"), None);
    assert_eq!(new_redactor(), a);
}
