//! HOST1: sliding-window rate limits every server instance on one database
//! shares (`runbook.rate_window`), so a limit holds however many replicas
//! serve the API. Keys are stored as SHA-256 hashes (they name client
//! addresses and users) and times come from the store clock, so replicas
//! with skewed clocks agree.
use crate::{DatabaseConfig, rows::int};
use runbook_core::StoreError;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio_postgres::Client;

const NOW_MS: &str = "SELECT floor(extract(epoch FROM clock_timestamp()) * 1000)::bigint";

fn key_hash(key: &str) -> String {
    format!("{:x}", Sha256::digest(key.as_bytes()))
}

/// Why a hit could not be counted (HOST1 review). The server fails closed
/// on [`Self::Busy`] and [`Self::Failed`] and falls back to its own
/// in-memory window only on [`Self::Unreachable`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RateLimitError {
    /// No connection could be opened, or the one in use was closed: the
    /// database is down.
    Unreachable,
    /// No connection within the acquire timeout (the pool is busy, or the
    /// database accepts connections and does not answer).
    Busy,
    /// The database answered with an error.
    Failed,
}

/// Count one hit for `key`. `Ok(Err(retry_after_seconds))` once `limit` hits
/// were already counted in the last `window_ms` (the refused hit is not
/// counted). `acquire` bounds the wait for a connection, opening one
/// included, so a busy pool never holds a limited request for the pool's
/// own 30 s.
pub async fn rate_limit_hit(
    config: &DatabaseConfig,
    key: &str,
    window_ms: u64,
    limit: u32,
    acquire: Duration,
) -> Result<Result<(), u64>, RateLimitError> {
    let window = i64::try_from(window_ms.max(1)).map_err(|_| RateLimitError::Failed)?;
    let hash = key_hash(key);
    let pool = crate::pool::Pool::new(config.clone());
    let mut conn = match tokio::time::timeout(acquire, pool.get()).await {
        Err(_) => return Err(RateLimitError::Busy),
        Ok(Err(crate::Error("postgres.pool_timeout"))) => return Err(RateLimitError::Busy),
        Ok(Err(crate::Error("postgres.connection_failed"))) => {
            return Err(RateLimitError::Unreachable);
        }
        Ok(Err(_)) => return Err(RateLimitError::Failed),
        Ok(Ok(conn)) => conn,
    };
    let classify = |e: tokio_postgres::Error| {
        if e.is_closed() {
            RateLimitError::Unreachable
        } else {
            RateLimitError::Failed
        }
    };
    conn.begin(crate::execution::READ_COMMITTED)
        .await
        .map_err(classify)?;
    match hit(conn.client(), &hash, window, limit).await {
        Ok(verdict) => {
            conn.commit().await.map_err(classify)?;
            Ok(verdict)
        }
        Err(e) => {
            conn.rollback().await;
            Err(classify(e))
        }
    }
}

async fn hit(
    c: &Client,
    hash: &str,
    window: i64,
    limit: u32,
) -> Result<Result<(), u64>, tokio_postgres::Error> {
    // One statement creates the window or locks the existing one and reads
    // it with the store clock: a purge between two statements can no
    // longer leave nothing to read, and racing first hits serialize on the
    // row lock.
    let row = c
        .query_one(
            &format!(
                "INSERT INTO runbook.rate_window (key_hash, hits, expires_at_ms) \
                 VALUES ($1, '{{}}', 0) \
                 ON CONFLICT (key_hash) DO UPDATE SET key_hash = EXCLUDED.key_hash \
                 RETURNING hits, ({NOW_MS})"
            ),
            &[&hash],
        )
        .await?;
    let (mut hits, now): (Vec<i64>, i64) = (row.get(0), row.get(1));
    hits.retain(|at| now - at < window);
    let verdict = if hits.len() >= limit as usize {
        let oldest = hits.first().copied().unwrap_or(now);
        Err((u64::try_from(window - (now - oldest)).unwrap_or(0) / 1000).max(1))
    } else {
        hits.push(now);
        Ok(())
    };
    let newest = hits.last().copied().unwrap_or(now);
    c.execute(
        "UPDATE runbook.rate_window SET hits = $2, expires_at_ms = $3 WHERE key_hash = $1",
        &[&hash, &hits, &(newest + window)],
    )
    .await?;
    Ok(verdict)
}

/// Delete up to `batch` windows whose newest hit is older than its window.
pub async fn purge_rate_windows(
    config: &DatabaseConfig,
    batch: usize,
) -> Result<usize, StoreError> {
    let conn = crate::pool::Pool::new(config.clone())
        .get()
        .await
        .map_err(crate::rows::connection)?;
    let deleted = conn
        .client()
        .execute(
            &format!(
                "DELETE FROM runbook.rate_window WHERE key_hash IN (\
                 SELECT key_hash FROM runbook.rate_window WHERE expires_at_ms <= ({NOW_MS}) \
                 ORDER BY expires_at_ms LIMIT $1 FOR UPDATE SKIP LOCKED)"
            ),
            &[&int(batch as u64)?],
        )
        .await
        .map_err(crate::rows::db)?;
    Ok(deleted as usize)
}
