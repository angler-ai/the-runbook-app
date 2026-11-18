//! CF6: one organization's quota counters as a host reaches them, in that
//! organization's OrgDO (`runbook_store_do::org`), over the same
//! transports as the object protocol: a Durable Object stub on Workers, an
//! in-process call natively, a `ConformanceDO` in the conformance Worker.
use crate::protocol::{Reply, Transport, decode_reply, encode_reply};
use runbook_core::{StoreError, quotas::QuotaLimits};
use runbook_model::{ExecutionId, OrganizationId};
use runbook_store_do::{DoSql, org::OrgStore};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::future::Future;

/// One operation on an organization's quota.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaOp {
    Reserve {
        execution_id: ExecutionId,
        month: String,
        now_ms: i64,
        limits: QuotaLimits,
    },
    Refund {
        execution_id: ExecutionId,
    },
    Started {
        month: String,
    },
    ReservationsBefore {
        before_ms: i64,
        limit: usize,
    },
    Settle {
        ids: Vec<ExecutionId>,
    },
}

/// One request to an organization's OrgDO.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuotaRequest {
    pub org: OrganizationId,
    pub op: QuotaOp,
}

fn to_value<T: Serialize>(value: T) -> Reply {
    serde_json::to_value(value).map_err(|_| StoreError::Backend("quota.encode"))
}

/// Answer `request` inside the organization's OrgDO.
pub fn serve_quota<B: DoSql>(store: &OrgStore<B>, request: QuotaRequest) -> Reply {
    let org = request.org;
    match request.op {
        QuotaOp::Reserve {
            execution_id,
            month,
            now_ms,
            limits,
        } => to_value(store.reserve(&org, &execution_id, &month, now_ms, limits)?),
        QuotaOp::Refund { execution_id } => to_value(store.refund(&execution_id)?),
        QuotaOp::Started { month } => to_value(store.started(&org, &month)?),
        QuotaOp::ReservationsBefore { before_ms, limit } => {
            to_value(store.reservations_before(before_ms, limit)?)
        }
        QuotaOp::Settle { ids } => to_value(store.settle_reservations(&ids)?),
    }
}

/// The wire form of a quota reply (as the object protocol's).
pub fn encode_quota_reply(reply: &Reply) -> serde_json::Value {
    encode_reply(reply)
}
pub fn decode_quota_reply(wire: serde_json::Value) -> Reply {
    decode_reply(wire)
}

/// One organization's quota, as the composed store uses it.
pub trait QuotaHandle: Clone + 'static {
    /// Count `execution_id`'s start in `month`; refused with
    /// `limit.executions_monthly` when the month is full. Idempotent.
    fn reserve(
        &self,
        execution_id: &ExecutionId,
        month: &str,
        now_ms: i64,
        limits: QuotaLimits,
    ) -> impl Future<Output = Result<(), StoreError>>;
    /// Give a start's count back (idempotent).
    fn refund(&self, execution_id: &ExecutionId) -> impl Future<Output = Result<(), StoreError>>;
    fn started(&self, month: &str) -> impl Future<Output = Result<u64, StoreError>>;
    fn reservations_before(
        &self,
        before_ms: i64,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ExecutionId>, StoreError>>;
    fn settle(&self, ids: Vec<ExecutionId>) -> impl Future<Output = Result<(), StoreError>>;
}

/// An organization's OrgDO over a transport.
#[derive(Clone)]
pub struct RemoteQuota<T: Transport> {
    transport: T,
    org: OrganizationId,
}
impl<T: Transport> RemoteQuota<T> {
    pub fn new(transport: T, org: OrganizationId) -> Self {
        Self { transport, org }
    }
    async fn call<R: DeserializeOwned>(&self, op: QuotaOp) -> Result<R, StoreError> {
        let value = self
            .transport
            .call_org(QuotaRequest {
                org: self.org.clone(),
                op,
            })
            .await?;
        serde_json::from_value(value).map_err(|_| StoreError::Backend("quota.decode"))
    }
}
impl<T: Transport> QuotaHandle for RemoteQuota<T> {
    async fn reserve(
        &self,
        execution_id: &ExecutionId,
        month: &str,
        now_ms: i64,
        limits: QuotaLimits,
    ) -> Result<(), StoreError> {
        self.call::<bool>(QuotaOp::Reserve {
            execution_id: execution_id.clone(),
            month: month.to_owned(),
            now_ms,
            limits,
        })
        .await
        .map(|_| ())
    }
    async fn refund(&self, execution_id: &ExecutionId) -> Result<(), StoreError> {
        self.call::<bool>(QuotaOp::Refund {
            execution_id: execution_id.clone(),
        })
        .await
        .map(|_| ())
    }
    async fn started(&self, month: &str) -> Result<u64, StoreError> {
        self.call(QuotaOp::Started {
            month: month.to_owned(),
        })
        .await
    }
    async fn reservations_before(
        &self,
        before_ms: i64,
        limit: usize,
    ) -> Result<Vec<ExecutionId>, StoreError> {
        self.call(QuotaOp::ReservationsBefore { before_ms, limit })
            .await
    }
    async fn settle(&self, ids: Vec<ExecutionId>) -> Result<(), StoreError> {
        self.call::<()>(QuotaOp::Settle { ids }).await
    }
}

/// A host with no OrgDO (native router tests): counts nothing, refuses
/// nothing.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoQuotas;
impl QuotaHandle for NoQuotas {
    async fn reserve(
        &self,
        _: &ExecutionId,
        _: &str,
        _: i64,
        _: QuotaLimits,
    ) -> Result<(), StoreError> {
        Ok(())
    }
    async fn refund(&self, _: &ExecutionId) -> Result<(), StoreError> {
        Ok(())
    }
    async fn started(&self, _: &str) -> Result<u64, StoreError> {
        Ok(0)
    }
    async fn reservations_before(&self, _: i64, _: usize) -> Result<Vec<ExecutionId>, StoreError> {
        Ok(vec![])
    }
    async fn settle(&self, _: Vec<ExecutionId>) -> Result<(), StoreError> {
        Ok(())
    }
}

/// What one sweep of an organization's reservations did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Starts that never reached D1 (their batch failed and the refund was
    /// lost): given back.
    pub refunded: usize,
    /// Runs admitted, finished or deleted: counted for good, forgotten.
    pub settled: usize,
}

/// How old a reservation must be before the sweep judges it (a start
/// finishes its D1 batch within its request).
pub const SWEEP_GRACE_MS: i64 = 3_600_000;

/// CF6 compensation: judge the organization's reservations made before
/// `before_ms` against D1. A run that never ran is refunded: a start whose
/// run has no owner row and no tombstone (it never committed), one
/// released from its queue before admission, or one the reconciler
/// abandoned (`start.abandoned`), whose own refunds were lost (gate fix,
/// low). A queued run keeps its reservation (its cancellation refunds it).
/// Anything else is counted for good, and its reservation is forgotten so
/// the table stays small.
pub async fn sweep<D: runbook_store_d1::D1, Q: QuotaHandle>(
    db: &D,
    quota: &Q,
    before_ms: i64,
    limit: usize,
) -> Result<Swept, StoreError> {
    use runbook_store_d1::sql::{Columns, prepare};
    let mut swept = Swept::default();
    let mut settled = vec![];
    for id in quota.reservations_before(before_ms, limit).await? {
        let rows = db
            .all(
                prepare(
                    "SELECT (SELECT count(*) FROM execution_owner WHERE execution_id = ?1), \
                     (SELECT count(*) FROM tombstone WHERE execution_id = ?1), \
                     (SELECT count(*) FROM admission WHERE execution_id = ?1 AND state = 'queued'), \
                     (SELECT count(*) FROM admission WHERE execution_id = ?1 \
                       AND state = 'released' AND admitted_at_ms IS NULL), \
                     (SELECT count(*) FROM tombstone WHERE execution_id = ?1 \
                       AND json_extract(body, '$.reason') = 'start.abandoned')",
                )
                .bind(id.as_str()),
            )
            .await
            .map_err(|_| StoreError::Backend("d1.quota_sweep"))?;
        let row = rows.first().ok_or(StoreError::Backend("d1.quota_sweep"))?;
        let count = |i| row.integer(i).map_err(|_| StoreError::Backend("d1.decode"));
        let (owned, deleted, queued) = (count(0)? > 0, count(1)? > 0, count(2)? > 0);
        let never_ran = count(3)? > 0 || count(4)? > 0;
        if (!owned && !deleted) || never_ran {
            // Gate fix (low): one refund that fails does not stop the rest.
            if quota.refund(&id).await.is_ok() {
                swept.refunded += 1;
            }
        } else if !queued {
            settled.push(id);
        }
    }
    swept.settled = settled.len();
    if !settled.is_empty() {
        quota.settle(settled).await?;
    }
    Ok(swept)
}
