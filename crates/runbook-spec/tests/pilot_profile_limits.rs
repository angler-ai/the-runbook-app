//! F85: the pilot profile's runtime limits account for every PRD §17
//! per-step size guard (outputs 64 KB, logs 5 MB per step): each row says
//! whether it is enforced or deferred, so the profile never silently omits
//! a required guard.
fn profile() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/pilot-profile.md"),
    )
    .unwrap()
}

fn section<'a>(doc: &'a str, heading: &str) -> &'a str {
    let start = doc
        .find(heading)
        .unwrap_or_else(|| panic!("missing {heading}"));
    let rest = &doc[start + heading.len()..];
    &rest[..rest.find("\n## ").unwrap_or(rest.len())]
}

#[test]
fn runtime_limits_list_the_prd_17_step_size_guards_as_enforced_or_deferred() {
    let doc = profile();
    let limits = section(&doc, "## Runtime limits");
    for (guard, size) in [("outputs", "64 KB"), ("log", "5 MB")] {
        let row = limits
            .lines()
            .find(|l| l.starts_with('|') && l.to_lowercase().contains(guard) && l.contains(size))
            .unwrap_or_else(|| panic!("no runtime-limits row for step {guard} ({size})"));
        let row = row.to_lowercase();
        assert!(
            row.contains("deferred") || row.contains("enforced"),
            "the {guard} row must say enforced or deferred: {row}"
        );
    }
}
