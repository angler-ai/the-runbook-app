// Test-owned databases only. The maintenance connection runs database DDL and
// pg_database metadata queries, never application queries or migrations.
use runbook_store_postgres::{DatabaseConfig, Error, PostgresStore};
use std::{
    any::Any,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
/// Pooled connections per test database (F32). With the maintenance and
/// admin connections, a fixture stays near six backends, so a full
/// `--test-threads` run on a laptop stays under the default 100.
const TEST_POOL_SIZE: usize = 4;

pub struct TestDatabase {
    admin: PostgresStore,
    config: DatabaseConfig,
    name: String,
    receipt: Option<String>,
}
impl TestDatabase {
    pub async fn create_from_env() -> Result<Self, Error> {
        let config = DatabaseConfig::from_env()?.direct_connection();
        let admin = PostgresStore::connect(&config).await?;
        // Database DDL on the maintenance connection must not inherit the
        // adapter's bounded lock wait: under parallel suites, DROP can
        // legitimately wait for backends to drain (R12).
        admin
            .client()
            .batch_execute("SET lock_timeout = 0")
            .await
            .map_err(|_| Error("test_database.lock_timeout"))?;
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error("test_database.clock"))?
            .as_nanos();
        let name = format!(
            "rb_test_{:x}_{:x}_{:x}",
            std::process::id(),
            epoch,
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        // Generated ASCII identifier, <63 bytes. No user-supplied identifier enters SQL.
        admin
            .client()
            .batch_execute(&format!("CREATE DATABASE {name} TEMPLATE template0"))
            .await
            .map_err(|_| Error("test_database.create_failed"))?;
        // Receipt exists ONLY after CREATE succeeds. Collision/error never grants cleanup.
        Ok(Self {
            admin,
            config: config.with_database(&name).with_pool_size(TEST_POOL_SIZE),
            receipt: Some(name.clone()),
            name,
        })
    }
    #[allow(dead_code)] // used by the bootstrap tests only
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn config(&self) -> &DatabaseConfig {
        &self.config
    }
    #[allow(dead_code)] // used by the bootstrap tests only
    pub async fn exists(&self) -> Result<bool, Error> {
        Ok(self
            .admin
            .client()
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname=$1)",
                &[&self.name],
            )
            .await?
            .get(0))
    }
    pub async fn cleanup_named(&mut self, name: &str) -> Result<(), Error> {
        if self.receipt.as_deref() != Some(name) || name != self.name {
            return Err(Error("test_database.not_owned"));
        }
        // Retry only this owned database (receipt checked above): FORCE
        // drops can still lose races against connecting backends.
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self
                .admin
                .client()
                .batch_execute(&format!("DROP DATABASE {name} WITH (FORCE)"))
                .await
            {
                Ok(()) => break,
                // Workers stopped by the test may still be closing pooled
                // connections; give an owned drop about five seconds.
                Err(_) if attempt < 6 => {
                    tokio::time::sleep(std::time::Duration::from_millis(250 * attempt)).await;
                }
                Err(_) => return Err(Error("test_database.cleanup_failed")),
            }
        }
        self.receipt = None;
        Ok(())
    }
    pub async fn cleanup(&mut self) -> Result<(), Error> {
        if self.receipt.is_none() {
            return Ok(());
        }
        self.cleanup_named(&self.name.clone()).await
    }
    /// Catch assertion panics around the body, clean up, then resume the failure.
    pub async fn finish(&mut self, result: Result<(), Box<dyn Any + Send>>) {
        let cleanup = self.cleanup().await;
        assert!(
            cleanup.is_ok(),
            "owned test database {} needs cleanup: {:?}",
            self.name,
            cleanup
        );
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
