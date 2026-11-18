//! Store-trait parity on PostgreSQL (store hardening): the shared parity
//! cases, plus the credential conformance cases that previously ran only on
//! the memory adapter. Each case owns one test database.
mod support;
use runbook_store_conformance::{
    CredentialFactory, parity,
    portable::{p, q},
};
use runbook_store_postgres::PostgresCredentials;
use support::factory::{PostgresFactory, with_factory};

impl CredentialFactory for PostgresFactory {
    type Credentials = PostgresCredentials;
    async fn create_credentials(
        &self,
    ) -> Result<Self::Credentials, runbook_core::credentials::CredentialError> {
        Ok(PostgresCredentials::new(self.config.clone()))
    }
}

macro_rules! parity_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| {
                let ps = f.fresh_store(p()).await;
                let qs = f.fresh_store(q()).await;
                parity::$name(&ps, &qs, &f.fresh_directory()).await;
                Ok(())
            })
            .await;
        }
    )*};
}
parity_cases!(
    claim_store_is_fenced_and_scoped,
    start_store_reads_and_cancel_are_scoped,
    org_routing_reads_are_org_scoped,
    blob_delete_and_listing_are_scoped,
    foreign_writes_and_counts_are_refused,
);

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn execution_credential_reads_are_scoped() {
    with_factory(async |f| {
        let ps = f.fresh_store(p()).await;
        let qs = f.fresh_store(q()).await;
        let credentials = PostgresCredentials::new(f.config.clone());
        parity::execution_credential_reads_are_scoped(&ps, &qs, &f.fresh_directory(), &credentials)
            .await;
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn catalog_publication_reads_are_scoped() {
    with_factory(async |f| {
        let ps = f.fresh_store(p()).await;
        let qs = f.fresh_store(q()).await;
        parity::catalog_publication_reads_are_scoped(&ps, &qs).await;
        Ok(())
    })
    .await;
}

macro_rules! credential_cases {
    ($($name:ident),* $(,)?) => {$(
        #[tokio::test]
        #[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
        async fn $name() {
            with_factory(async |f| {
                runbook_store_conformance::credentials::$name(f).await;
                Ok(())
            })
            .await;
        }
    )*};
}
credential_cases!(
    pat_and_service_authorization_is_current,
    credential_usage_write_is_throttled,
    secret_format_and_hash_are_pinned,
);
