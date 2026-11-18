//! The execution-scoped subset of the shared cases (CF2, plan 2026-11-12
//! §2.6): every case one execution's own store can serve without a catalog.
//! On Cloudflare that store is one ExecutionDO (D56), so `runbook-store-do`
//! runs exactly these, natively over SQLite and inside workerd.
//!
//! A case is execution-scoped when its factory bound is one of
//! [`EXECUTION_TIERS`]. [`EXECUTION_SCOPED`] lists them by name, and a test
//! here re-derives the set from this crate's sources, so a new case at one
//! of those tiers fails until it is listed. Every harness then calls
//! [`assert_registered`] with the names it runs, so a harness that skips a
//! listed case fails too. The full cross-store suite on workerd is CF5.

/// The factory tiers whose cases an ExecutionDO store must pass.
pub const EXECUTION_TIERS: &[&str] = &["StoreFactory", "SessionFactory", "WorkQueueFactory"];

/// Every shared execution-scoped case, as `module::function`.
pub const EXECUTION_SCOPED: &[&str] = &[
    "execution::atomic_batch_or_nothing",
    "execution::stale_commit_has_no_side_effects",
    "execution::one_claim_generation_winner",
    "execution::idempotent_response_is_payload_bound",
    "execution::replay_equals_projection",
    "clock::store_clock_controls_deadline_equality",
    "clock::session_time_and_snapshot_are_fenced",
    "clock::session_commits_at_its_begin_time",
    "queue::due_item_has_one_current_worker",
    "queue::stale_worker_cannot_ack_successor",
    "queue::delivery_leases_are_scoped_and_fenced",
    "queue::timer_replacements_require_new_generation",
];

/// The blob tier (CF4): cases a `BlobStore` alone must pass, such as an
/// object-store adapter (R2) with no execution store behind it.
pub const BLOB_TIERS: &[&str] = &["BlobFactory"];

/// Every shared blob-tier case, as `module::function`.
pub const BLOB_CASES: &[&str] = &[
    "blob::blob_roundtrip_preserves_chunk_boundaries",
    "blob::shared_blobs_are_separate_and_immutable",
    "uploads::upload_blobs_support_head_and_ranges",
    "uploads::upload_ranges_cross_chunk_boundaries",
];

/// Panic unless `registered` is exactly [`EXECUTION_SCOPED`] (any order,
/// no duplicates), naming what is missing or unknown.
pub fn assert_registered(harness: &str, registered: &[&str]) {
    assert_list(harness, EXECUTION_SCOPED, registered);
}

/// Panic unless `registered` is exactly [`BLOB_CASES`].
pub fn assert_blob_registered(harness: &str, registered: &[&str]) {
    assert_list(harness, BLOB_CASES, registered);
}

fn assert_list(harness: &str, list: &[&str], registered: &[&str]) {
    let missing: Vec<_> = list
        .iter()
        .filter(|case| !registered.contains(case))
        .collect();
    let unknown: Vec<_> = registered
        .iter()
        .filter(|case| !list.contains(case))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let duplicated: Vec<_> = registered.iter().filter(|c| !seen.insert(**c)).collect();
    assert!(
        missing.is_empty() && unknown.is_empty() && duplicated.is_empty(),
        "{harness}: execution-scoped cases not registered: {missing:?}; \
         unknown: {unknown:?}; registered twice: {duplicated:?}"
    );
}

/// `pub async fn` cases whose factory `F` is bound by exactly one tier,
/// in one module's source. Each signature is read whole, from `pub async
/// fn` to its body, so wrapped generics and `where` clauses count. A tier
/// with any extra bound (`StoreFactory<Store: UploadStore>`, `+ Clone`,
/// `where F::Store: …`) is not execution-scoped: those traits are
/// cross-execution (D1, OrgDO).
pub fn cases_at_tiers(module: &str, source: &str) -> Vec<String> {
    cases_at(module, source, EXECUTION_TIERS)
}

/// [`cases_at_tiers`] for any list of tiers.
pub fn cases_at(module: &str, source: &str, tiers: &[&str]) -> Vec<String> {
    let mut found = vec![];
    let mut rest = source;
    while let Some(start) = rest.find("pub async fn ") {
        let after = &rest[start + "pub async fn ".len()..];
        let end = after.find('{').unwrap_or(after.len());
        let signature: String = after[..end]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        rest = &after[end..];
        let name: String = signature
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() || signature.contains("F::") {
            continue;
        }
        let mut bounds = vec![];
        for clause in top_level_items(&signature) {
            if let Some(bound) = clause.trim().strip_prefix("F:") {
                bounds.push(bound.trim().to_owned());
            }
        }
        if let [bound] = bounds.as_slice()
            && tiers.contains(&bound.as_str())
        {
            found.push(format!("{module}::{name}"));
        }
    }
    found
}

/// The generic parameters and `where` predicates of one signature, split
/// at commas that are not nested inside `<>` or `()`.
pub(crate) fn top_level_items(signature: &str) -> Vec<String> {
    let mut items = vec![];
    // Generics: between the first `<` and its matching `>`, before `(`.
    let open = signature.find('(').unwrap_or(signature.len());
    if let Some(lt) = signature[..open].find('<') {
        items.extend(split_top(&signature[lt + 1..], '>'));
    }
    if let Some(at) = signature.find(" where ") {
        items.extend(split_top(&signature[at + " where ".len()..], '{'));
    }
    items
}

fn split_top(text: &str, close: char) -> Vec<String> {
    let mut items = vec![];
    let mut depth = 0usize;
    let mut current = String::new();
    for c in text.chars() {
        match c {
            '<' | '(' => depth += 1,
            '>' | ')' if depth > 0 => depth -= 1,
            _ if c == close && depth == 0 => break,
            ',' if depth == 0 => {
                items.push(std::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    items.push(current);
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<(String, String)> {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut out = vec![];
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let module = path.file_stem().unwrap().to_str().unwrap().to_owned();
            if path.extension().is_none_or(|e| e != "rs") || module == "execution_scoped" {
                continue;
            }
            out.push((module, std::fs::read_to_string(&path).unwrap()));
        }
        out
    }

    #[test]
    fn the_blob_list_matches_every_case_at_the_blob_tier() {
        let derived: Vec<String> = sources()
            .iter()
            .flat_map(|(module, source)| cases_at(module, source, BLOB_TIERS))
            .collect();
        let derived: Vec<&str> = derived.iter().map(String::as_str).collect();
        assert_blob_registered("runbook-store-conformance sources", &derived);
    }

    #[test]
    fn the_list_matches_every_case_at_an_execution_tier() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut derived = vec![];
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let module = path.file_stem().unwrap().to_str().unwrap().to_owned();
            if path.extension().is_none_or(|e| e != "rs") || module == "execution_scoped" {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            derived.extend(cases_at_tiers(&module, &source));
        }
        let derived: Vec<&str> = derived.iter().map(String::as_str).collect();
        assert_registered("runbook-store-conformance sources", &derived);
    }

    #[test]
    fn a_missing_or_unknown_case_fails_the_harness() {
        let mut short: Vec<&str> = EXECUTION_SCOPED.to_vec();
        short.pop();
        assert!(std::panic::catch_unwind(|| assert_registered("t", &short)).is_err());
        let mut extra: Vec<&str> = EXECUTION_SCOPED.to_vec();
        extra.push("execution::not_a_case");
        assert!(std::panic::catch_unwind(|| assert_registered("t", &extra)).is_err());
        let mut twice: Vec<&str> = EXECUTION_SCOPED.to_vec();
        twice.push(EXECUTION_SCOPED[0]);
        assert!(std::panic::catch_unwind(|| assert_registered("t", &twice)).is_err());
        assert_registered("t", EXECUTION_SCOPED);
    }

    #[test]
    fn the_scanner_reads_where_clauses_and_wrapped_generics() {
        let source = "pub async fn a<F>(f: &F) -> Result<(), StoreError>\n\
                      where\n    F: SessionFactory,\n{\n}\n\
                      pub async fn b<\n    F: WorkQueueFactory,\n>(\n    f: &F,\n) -> Result<(), StoreError> {\n}\n\
                      pub async fn c<F>(f: &F) -> Result<(), StoreError>\n\
                      where\n    F: StoreFactory,\n    F::Store: UploadStore,\n{\n}\n\
                      pub async fn d<F: StoreFactory>(f: &F) -> Result<(), StoreError>\n\
                      where\n    F::Store: UploadStore,\n{\n}\n\
                      pub async fn e<F: StoreFactory + Clone>(f: &F) {}\n\
                      pub async fn g<S, D>(store: &S, dir: &D)\nwhere\n    S: StoreFactory,\n{\n}\n";
        assert_eq!(cases_at_tiers("m", source), ["m::a", "m::b"]);
    }

    #[test]
    fn the_scanner_reads_tier_bounds_only() {
        let source = "pub async fn a<F: StoreFactory>(f: &F) {}\n\
                      pub async fn b<F: CatalogFactory>(f: &F) {}\n\
                      async fn c<F: SessionFactory>(f: &F) {}\n\
                      pub async fn d<F: WorkQueueFactory>(\n    f: &F,\n) {}\n\
                      pub async fn e<F: StoreFactory<Store: UploadStore>>(\n    f: &F,\n) {}\n";
        assert_eq!(cases_at_tiers("m", source), ["m::a", "m::d"]);
    }
}
