#[allow(dead_code)]
#[path = "support/flagship.rs"]
mod flagship;

#[path = "support/flagship_runtime.rs"]
mod flagship_runtime;

use std::collections::BTreeSet;

#[test]
fn every_flagship_state_and_site_executes_with_verified_provenance() {
    // A missing execution, duplicate name, or empty result must not make a future
    // native/Wasm comparison pass simply because both corpora omitted the case.
    let sites = [
        "/spec/execution/concurrency/key",
        "/spec/execution/title",
        "/spec/outputs/result",
        "/spec/steps/0/parallel/0/instructions",
        "/spec/steps/1/foreach/items",
        "/spec/steps/1/foreach/steps/0/title",
        "/spec/steps/1/foreach/steps/0/with/region",
        "/spec/steps/2/if",
        "/spec/steps/2/loop/until",
        "/spec/steps/3/switch/cases/0/when",
        "/spec/steps/3/switch/cases/0/steps/0/approval/prompt",
        "/spec/steps/3/switch/cases/0/steps/1/if",
        "/spec/steps/3/switch/cases/0/steps/1/title",
        "/spec/steps/3/switch/cases/0/steps/2/set/vars/result",
        "/companion/report",
        "/companion/direct-index",
        "/companion/whole-output-record",
        "/companion/distinct-iterations",
        "/companion/current-loop",
        "/companion/last-loop",
    ];
    let mut expected = BTreeSet::new();
    for scenario in [
        "healthy",
        "unhealthy",
        "rollback",
        "keep",
        "null-verdict",
        "missing-output",
        "distinct-iterations",
    ] {
        for site in sites {
            expected.insert(format!("flagship/{scenario}{site}"));
        }
    }
    expected.insert("flagship/null-verdict/companion/null-value".into());
    let rows = flagship_runtime::runtime_rows();
    let actual: BTreeSet<_> = rows
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(actual, expected, "all known runtime rows must be present");
    assert_eq!(rows.len(), actual.len(), "row names must be unique");
    for row in rows {
        assert!(!row["source"].as_str().unwrap().is_empty());
        assert!(!row["scope"].as_str().unwrap().is_empty());
        let outcome = &row["outcome"];
        assert!(outcome.get("value").is_some() ^ outcome.get("error").is_some());
        assert!(outcome["stats"]["cost"].as_u64().unwrap() > 0);
        assert!(!outcome["stats"]["reads"].as_array().unwrap().is_empty());
    }
}
