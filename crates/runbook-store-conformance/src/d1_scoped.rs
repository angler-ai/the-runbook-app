//! The D1-scoped subset of the shared cases (CF3, plan 2026-11-12 §2.6):
//! every case whose store is one cross-execution trait D1 holds alone (the
//! directory, credentials, webhooks, memory, auth links and device codes),
//! with no execution store behind it. `runbook-store-d1` runs exactly these
//! natively over SQLite and inside workerd over Miniflare's D1.
//!
//! A case is D1-scoped when it has one generic parameter and that
//! parameter's whole bound is one of [`D1_TIERS`]. [`D1_CASES`] lists them,
//! a test re-derives the list from the sources (so a new case at one of
//! those tiers fails until it is listed), and every harness calls
//! [`assert_d1_registered`] with what it runs (so a harness that skips one
//! fails). Cases that need an execution store too (the catalog, queue and
//! archive tiers) run over the composed Cloudflare store; on workerd that is
//! CF5.

/// `(parameter, bound)` pairs whose cases a D1 store alone must pass.
pub const D1_TIERS: &[(&str, &str)] = &[
    ("F", "DirectoryFactory"),
    ("F", "CredentialFactory"),
    ("W", "WebhookStore"),
    ("M", "MemoryStore"),
    ("D", "DirectoryStore + AuthLinkStore"),
    ("C", "DeviceCodeStore"),
    // NOTIFY1: preferences and the email outbox.
    ("F", "NotifyFactory"),
];

/// Every shared D1-scoped case, as `module::function`.
pub const D1_CASES: &[&str] = &[
    "auth_links::invitation_links_once_and_applies_role",
    "auth_links::links_refuse_uninvited_and_stale",
    "credentials::pat_and_service_authorization_is_current",
    "credentials::credential_usage_write_is_throttled",
    "credentials::secret_format_and_hash_are_pinned",
    "device_codes::poll_consumes_an_approval_once",
    "device_codes::denial_expiry_and_collisions",
    "directory::directory_snapshot_is_org_scoped",
    "directory::directory_revision_fences_mutations",
    "directory::directory_rejects_cross_org_membership",
    "directory::directory_slug_uniqueness_is_org_scoped",
    "directory::directory_deactivation_and_removal_are_visible",
    "directory::directory_fixture_has_distinct_principals",
    "directory::verified_email_key_reads_are_unique_and_atomic",
    "directory::invalid_batches_and_duplicate_ids_do_not_leak_indexes",
    "directory::shared_identity_deactivation_is_org_local",
    "directory::org_role_permission_matrix",
    "directory::last_active_owner_is_protected",
    "directory::org_roles_are_org_local",
    "eligibility::team_membership_does_not_grant_project_access",
    "eligibility::approval_requires_current_target_match",
    "eligibility::unknown_static_reference_fails_publication",
    "eligibility::handoff_missing_route_notifies_admins_without_broadening",
    "eligibility::two_users_not_two_tokens_satisfy_quorum",
    "eligibility::unresolved_handoff_without_admin_still_records_blocked_route",
    "memory_entries::stale_revision_is_refused",
    "memory_entries::foreign_and_expired_entries_are_hidden",
    "memory_entries::secrets_are_stored_redacted",
    "memory_entries::entry_quota_is_per_runbook",
    "memory_entries::patches_follow_transitions_and_keep_history",
    "memory_entries::history_is_bounded_lockable_and_restorable",
    "memory_entries::people_keep_their_revision_and_the_store_holds_locks",
    "memory_entries::search_filters_and_pages",
    "notify::preferences_salt_and_token",
    "notify::outbox_records_once_and_claims_are_fenced",
    "notify::digest_rows_are_claimed_by_age_and_marked_sent",
    "webhooks::limit_is_per_project_and_tenants_are_isolated",
    "webhooks::fan_out_is_idempotent_and_claims_are_fenced",
    "webhooks::replay_rows_and_delete_cascade",
    "webhooks::stale_saves_conflict",
    "webhooks::delivery_log_purge_is_bounded_and_keeps_verdicts",
];

/// Panic unless `registered` is exactly [`D1_CASES`] (any order, no
/// duplicates), naming what is missing or unknown.
pub fn assert_d1_registered(harness: &str, registered: &[&str]) {
    let missing: Vec<_> = D1_CASES
        .iter()
        .filter(|case| !registered.contains(case))
        .collect();
    let unknown: Vec<_> = registered
        .iter()
        .filter(|case| !D1_CASES.contains(case))
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let duplicated: Vec<_> = registered.iter().filter(|c| !seen.insert(**c)).collect();
    assert!(
        missing.is_empty() && unknown.is_empty() && duplicated.is_empty(),
        "{harness}: D1-scoped cases not registered: {missing:?}; \
         unknown: {unknown:?}; registered twice: {duplicated:?}"
    );
}

/// `pub async fn` cases with one generic parameter whose whole bound is one
/// of `tiers`, in one module's source.
pub fn cases_bound(module: &str, source: &str, tiers: &[(&str, &str)]) -> Vec<String> {
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
        let clauses: Vec<String> = crate::execution_scoped::top_level_items(&signature)
            .into_iter()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .collect();
        if let [clause] = clauses.as_slice()
            && let Some((param, bound)) = clause.split_once(':')
            && tiers.contains(&(param.trim(), bound.trim()))
        {
            found.push(format!("{module}::{name}"));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_matches_every_case_at_a_d1_tier() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut derived = vec![];
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let module = path.file_stem().unwrap().to_str().unwrap().to_owned();
            if path.extension().is_none_or(|e| e != "rs")
                || module == "d1_scoped"
                || module == "execution_scoped"
            {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            derived.extend(cases_bound(&module, &source, D1_TIERS));
        }
        let derived: Vec<&str> = derived.iter().map(String::as_str).collect();
        assert_d1_registered("runbook-store-conformance sources", &derived);
    }

    #[test]
    fn a_missing_or_unknown_case_fails_the_harness() {
        let mut short: Vec<&str> = D1_CASES.to_vec();
        short.pop();
        assert!(std::panic::catch_unwind(|| assert_d1_registered("t", &short)).is_err());
        let mut extra: Vec<&str> = D1_CASES.to_vec();
        extra.push("directory::not_a_case");
        assert!(std::panic::catch_unwind(|| assert_d1_registered("t", &extra)).is_err());
        assert_d1_registered("t", D1_CASES);
    }

    #[test]
    fn the_scanner_takes_one_parameter_with_a_whole_tier_bound() {
        let source = "pub async fn a<W: WebhookStore>(w: &W) {}\n\
                      pub async fn b<D: DirectoryStore + AuthLinkStore>(d: &D) {}\n\
                      pub async fn c<S, D>(s: &S, d: &D) where D: DirectoryStore {}\n\
                      pub async fn d<F: CatalogFactory>(f: &F) {}\n\
                      pub async fn e<W: WebhookStore + Clone>(w: &W) {}\n";
        assert_eq!(cases_bound("m", source, D1_TIERS), ["m::a", "m::b"]);
    }
}
