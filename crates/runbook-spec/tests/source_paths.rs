use runbook_spec::parse;
use serde_json::json;

const PATH_BUDGET: usize = 8 * 1024 * 1024;

// UTF-8 and JSON-pointer escaping both count toward stored path bytes. Keep
// source text below 1 MiB and values below 262,144 while amplifying prefixes.
fn source_with_path_bytes(extra_bytes: usize) -> String {
    let key = format!("{}~/", "é".repeat(60));
    let mut source = json!({
        "apiVersion":"therunbook.app/v1", "kind":"Runbook",
        "metadata":{"id":"paths","title":"Paths"}, "spec":{"steps":[]},
        "x-wide":{key:vec![0; 60_000]}
    });
    // Existing fixed pointers total 75 bytes; the escaped array pointer is
    // 132 bytes, and its 60,000 item pointers sum to 8,268,890 bytes.
    // Adding one root scalar contributes slash + key length, filling the gap.
    let baseline_bytes = 75 + 132 + 8_268_890;
    let padding_key = format!(
        "x-{}",
        "p".repeat(PATH_BUDGET - baseline_bytes - 3 + extra_bytes)
    );
    source
        .as_object_mut()
        .unwrap()
        .insert(padding_key, json!(null));
    let source = source.to_string();
    assert!(source.len() < 1_048_576);
    source
}

#[test]
fn cumulative_source_paths_accept_exact_byte_budget() {
    let spec = parse(&source_with_path_bytes(0)).expect("exact byte budget is accepted");
    assert_eq!(
        spec.source_spans.keys().map(String::len).sum::<usize>(),
        PATH_BUDGET
    );
    assert_eq!(spec.source_spans.len(), 60_011);
    assert!(spec.source_spans.keys().any(|path| path.len() > 100_000));
    assert!(
        spec.source_spans
            .contains_key(&format!("/x-wide/{}~0~1/59999", "é".repeat(60)))
    );
}

#[test]
fn cumulative_source_paths_reject_one_extra_byte_and_reset_per_parse() {
    let errors = parse(&source_with_path_bytes(1))
        .map(|_| ())
        .expect_err("one byte over budget is rejected");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].code, "spec.source_path_limit");
    assert!(errors[0].line > 0);
    assert!(errors[0].column > 0);
    let spec = parse(&source_with_path_bytes(0)).expect("each parse has its own budget");
    assert_eq!(
        spec.source_spans.keys().map(String::len).sum::<usize>(),
        PATH_BUDGET
    );
}
