//! Native PostgreSQL adapter. Connection details never enter error messages.
mod archives;
mod blob;
pub mod blob_copy;
pub use blob_copy::{BlobCopier, BlobCopyStats};
mod bootstrap;
pub use bootstrap::{BootstrapFault, BootstrapSeed, DoctorReport};
mod credentials;
pub use credentials::{IssueOutcome, PostgresCredentials, TokenIssue};
mod catalog;
mod claims;
mod config;
mod directory;
mod execution;
mod execution_credentials;
mod live_events;
mod me_tasks;
mod memory_entries;
mod notify;
pub use notify::PostgresNotify;
mod migrate;
mod pool;
mod portable;
pub use portable::{export_project, import_project, resolve_project};
mod project_outbox;
mod rate_limits;
pub use rate_limits::{RateLimitError, purge_rate_windows, rate_limit_hit};
mod publish;
mod retention;
mod rows;
mod scopes;
mod uploads;
pub use scopes::{execution_owner, published_versions, tenant_scopes};
mod search;
#[doc(hidden)]
pub use search::RUNBOOK_RUN_STATS_SQL;
mod agent_directory;
mod runner_ready;
mod runners;
pub use runners::PostgresRunners;
mod signing;
pub use signing::{PostgresSigningKeys, StoredSigningKey};
mod skills;
mod start;
mod webhooks;
mod work_queue;
pub use config::{
    DEFAULT_IDLE_TRANSACTION_TIMEOUT, DEFAULT_LOCK_TIMEOUT, DatabaseConfig, SslMode, TlsPolicy,
};
pub use directory::{OwnerAssignment, OwnerRecoveryFault, PostgresDirectory};
pub use execution::{PostgresExecutionStore, PostgresSession};
pub use live_events::{LiveEvent, LiveEvents};
pub use migrate::SCHEMA_VERSION;
pub use pool::{DEFAULT_POOL_SIZE, PoolStats};
use std::fmt;
use tokio::task::JoinHandle;
use tokio_postgres::Client;
pub use webhooks::PostgresWebhooks;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
impl From<tokio_postgres::Error> for Error {
    fn from(_: tokio_postgres::Error) -> Self {
        Self("postgres.database_error")
    }
}

/// Each connection owns its I/O task. Dropping it closes any open transaction.
/// Runtime sessions will own independent connections; no process lock serializes writes.
pub struct PostgresStore {
    client: Client,
    connection: JoinHandle<()>,
    /// UPD1: from [`DatabaseConfig::allow_newer_schema`].
    allow_newer_schema: bool,
}
impl PostgresStore {
    pub async fn connect(config: &DatabaseConfig) -> Result<Self, Error> {
        Self::connect_tls(config, config.tls().connector()?).await
    }
    pub(crate) async fn connect_tls(
        config: &DatabaseConfig,
        tls: native_tls::TlsConnector,
    ) -> Result<Self, Error> {
        let (client, connection) = config
            .primary()
            .connect(postgres_native_tls::MakeTlsConnector::new(tls))
            .await
            .map_err(|error| {
                let cause = match error.code().map(|code| code.code()) {
                    Some("53300") => "connection_limit",
                    Some("28000" | "28P01" | "42501") => "connection_auth",
                    Some("57P03") => "database_starting",
                    Some(_) => "connection_sql_error",
                    None => "connection_transport",
                };
                config.pool().failure(cause);
                Error("postgres.connection_failed")
            })?;
        let store = Self {
            client,
            connection: tokio::spawn(async move {
                let _ = connection.await;
            }),
            allow_newer_schema: config.allows_newer_schema(),
        };
        let version: String = store
            .client
            .query_one("SHOW server_version_num", &[])
            .await?
            .get(0);
        if version
            .parse::<u32>()
            .map_err(|_| Error("postgres.invalid_server_version"))?
            < 140000
        {
            return Err(Error("postgres.requires_version_14"));
        }
        store
            .client
            .batch_execute(&config.session_settings())
            .await?;
        Ok(store)
    }

    /// Native adapter access, also used by the owned-database integration harness.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Apply pending migrations. Returns the database's schema version:
    /// [`SCHEMA_VERSION`], or a newer release's when the config allows it
    /// (UPD1; refused with `postgres.schema_newer_than_code` otherwise).
    pub async fn migrate(&mut self) -> Result<u32, Error> {
        migrate::run(&mut self.client, self.allow_newer_schema).await
    }

    /// Check that the database is at this code's schema without changing
    /// it. Returns the schema the database holds, which is newer only
    /// when the config explicitly allows running on it.
    pub async fn check_schema(&self) -> Result<u32, Error> {
        migrate::check(&self.client, self.allow_newer_schema).await
    }
}
/// HOST1 readiness: one pooled connection answers `SELECT 1`.
pub async fn ping(config: &DatabaseConfig) -> Result<(), Error> {
    pool::Pool::new(config.clone()).get().await?.ping().await
}

impl Drop for PostgresStore {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

mod authoring;
pub use authoring::{
    PostgresAuthoring, PostgresAuthoringCatalog, RUNBOOK_BY_SLUG_SQL, RUNBOOK_LISTING_CHUNK_SQL,
};

mod step_lookup;

mod actor_replay;
