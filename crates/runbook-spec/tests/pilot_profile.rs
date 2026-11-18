//! D2: `docs/pilot-profile.md` stays in sync with
//! `conformance/spec/pilot-capabilities.json`.
//!
//! The profile's "Supported keys", "Rejected keys" and "Preserved keys"
//! tables must list exactly the capability file's rules of that class (path
//! and example value, as a multiset), with the supported owner and the
//! rejection code the classifier reports. Regenerate the tables from the
//! capability file when it changes; this test names every difference.
use runbook_spec::classify;
use serde_json::Value;
use std::collections::BTreeMap;

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// (path, canonical JSON value) -> (third column, count)
type Rows = BTreeMap<(String, String), (String, usize)>;

fn cell(text: &str) -> String {
    let text = text.trim();
    text.strip_prefix('`')
        .and_then(|t| t.strip_suffix('`'))
        .unwrap_or(text)
        .to_owned()
}

/// Rows of the first table under `### {heading}`.
fn table(doc: &str, heading: &str) -> Rows {
    let mut lines = doc
        .lines()
        .skip_while(|l| l.trim() != format!("### {heading}"));
    assert!(lines.next().is_some(), "missing section '### {heading}'");
    let mut rows = Rows::new();
    let mut in_table = false;
    for line in lines {
        let line = line.trim();
        if line.starts_with("#") {
            break;
        }
        if !line.starts_with('|') {
            if in_table {
                break;
            }
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split(" | ").collect();
        if !in_table {
            in_table = true; // header row
            continue;
        }
        if cells.iter().all(|c| c.trim().chars().all(|ch| ch == '-')) {
            continue; // separator row
        }
        assert!(cells.len() >= 2, "short row under {heading}: {line}");
        let path = cell(cells[0]);
        let value: Value = serde_json::from_str(&cell(cells[1]))
            .unwrap_or_else(|e| panic!("{heading}: value of {path} is not JSON ({e}): {line}"));
        let third = cells.get(2).map(|c| cell(c)).unwrap_or_default();
        let entry = rows
            .entry((path, value.to_string()))
            .or_insert((third.clone(), 0));
        assert_eq!(entry.0, third, "{heading}: conflicting rows for {line}");
        entry.1 += 1;
    }
    assert!(in_table, "no table under '### {heading}'");
    rows
}

fn expected(class: &str) -> Rows {
    let text =
        std::fs::read_to_string(root().join("conformance/spec/pilot-capabilities.json")).unwrap();
    let manifest: Value = serde_json::from_str(&text).unwrap();
    let mut rows = Rows::new();
    for rule in manifest["rules"].as_array().unwrap() {
        if rule["class"] != class {
            continue;
        }
        let path = rule["path"].as_str().unwrap().to_owned();
        let third = match class {
            "supported" => rule["owner"].as_str().unwrap().to_owned(),
            "rejected" => classify(&path, &rule["value"])
                .code()
                .expect("a rejected rule has a code")
                .to_owned(),
            _ => String::new(),
        };
        let entry = rows
            .entry((path, rule["value"].to_string()))
            .or_insert((third, 0));
        entry.1 += 1;
    }
    rows
}

fn compare(heading: &str, class: &str, doc: &str) {
    let documented = table(doc, heading);
    let expected = expected(class);
    let missing: Vec<_> = expected
        .iter()
        .filter(|(k, v)| documented.get(*k) != Some(v))
        .collect();
    let extra: Vec<_> = documented
        .iter()
        .filter(|(k, v)| expected.get(*k) != Some(v))
        .collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "docs/pilot-profile.md '{heading}' differs from pilot-capabilities.json ({class}).\n\
         missing or different: {missing:#?}\nnot in the capability file: {extra:#?}"
    );
}

fn profile() -> String {
    std::fs::read_to_string(root().join("docs/pilot-profile.md"))
        .expect("docs/pilot-profile.md exists")
}

#[test]
fn supported_keys_table_matches_the_capability_file() {
    compare("Supported keys", "supported", &profile());
}

#[test]
fn rejected_keys_table_matches_the_capability_file() {
    compare("Rejected keys", "rejected", &profile());
}

#[test]
fn preserved_keys_table_matches_the_capability_file() {
    compare("Preserved keys", "preserved", &profile());
}

#[test]
fn profile_states_the_pilot_non_claims() {
    let doc = profile();
    for phrase in [
        "not full v1",
        "no production security certification",
        "no public Docker release",
        "no performance targets",
        "Postgres",
        "Cloudflare",
    ] {
        assert!(
            doc.contains(phrase),
            "pilot-profile.md must state: {phrase}"
        );
    }
}
