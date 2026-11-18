//! T0 contract freeze (plan 2026-11-12 §1): the shared cases registered red,
//! run on Postgres. `settle` expects the frozen stub's error while a case is
//! in `runbook_store_conformance::pending::PENDING`; MEM2, UP1, ARC1 and
//! AUTH2 turn them green here.
mod support;
use runbook_core::StoreError;
use runbook_store_conformance::{
    DirectoryFactory, StoreFactory, archives, auth_links, device_codes, execution::projection,
    memory_entries, pending::settle, uploads,
};
use runbook_store_postgres::PostgresCredentials;
use support::factory::with_factory;

macro_rules! pending_case {
    ($test:ident, $case:literal, |$f:ident| $body:expr) => {
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $test() {
            with_factory(async |$f| {
                settle($case, $body);
                Ok::<(), StoreError>(())
            })
            .await;
        }
    };
}

pending_case!(
    memory_stale_revision_is_refused,
    "memory_entries::stale_revision_is_refused",
    |f| memory_entries::stale_revision_is_refused(&f.create(projection()).await?).await
);
pending_case!(
    memory_foreign_and_expired_entries_are_hidden,
    "memory_entries::foreign_and_expired_entries_are_hidden",
    |f| memory_entries::foreign_and_expired_entries_are_hidden(&f.create(projection()).await?)
        .await
);
pending_case!(
    upload_slots_count_limits_and_finalize_once,
    "uploads::slots_count_limits_and_finalize_once",
    |f| uploads::slots_count_limits_and_finalize_once(f).await
);
pending_case!(
    upload_blobs_support_head_and_ranges,
    "uploads::upload_blobs_support_head_and_ranges",
    |f| uploads::upload_blobs_support_head_and_ranges(f).await
);
pending_case!(
    archive_index_round_trip_and_stale_marks,
    "archives::index_round_trip_and_stale_marks",
    |f| archives::index_round_trip_and_stale_marks(&f.create(projection()).await?).await
);
pending_case!(
    archive_blobs_are_tenant_scoped,
    "archives::archive_blobs_are_tenant_scoped",
    |f| archives::archive_blobs_are_tenant_scoped(f).await
);
pending_case!(
    auth_invitation_links_once_and_applies_role,
    "auth_links::invitation_links_once_and_applies_role",
    |f| auth_links::invitation_links_once_and_applies_role(&f.create_directory().await.unwrap())
        .await
);
pending_case!(
    device_poll_consumes_an_approval_once,
    "device_codes::poll_consumes_an_approval_once",
    |f| device_codes::poll_consumes_an_approval_once(&PostgresCredentials::new(f.config.clone()))
        .await
);
pending_case!(
    auth_links_refuse_uninvited_and_stale,
    "auth_links::links_refuse_uninvited_and_stale",
    |f| auth_links::links_refuse_uninvited_and_stale(&f.create_directory().await.unwrap()).await
);
pending_case!(
    device_denial_expiry_and_collisions,
    "device_codes::denial_expiry_and_collisions",
    |f| device_codes::denial_expiry_and_collisions(&PostgresCredentials::new(f.config.clone()))
        .await
);
