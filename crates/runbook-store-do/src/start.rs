//! The execution-local half of a start (B5a, D24). On Cloudflare a start
//! spans two stores: D1 checks the start replay key, claims the execution
//! ID for its tenant and admits it with one conditional statement (CF3);
//! then this object records the head, the pinned start facts, the queued
//! notification and the start's derived row, all in one transaction. A run
//! queued in D1 is activated later through [`DoStore::admit`].
//!
//! `runbook_core::start::StartStore` (replay lookup, pooled admission,
//! `cancel_queued`, LANG3 preemption across executions) is composed in CF3
//! from D1 and these calls; nothing here reads or writes another run.
use crate::{
    DoSql, DoStore,
    sql::{Columns, decode, encode, int},
};
use runbook_core::{
    ExecutionProjection, StoreError,
    catalog::{Admission, AdmissionState, DerivedUpdate},
    start::{StartRecord, queued_outbox},
};
use runbook_model::ExecutionId;

/// What a start writes into the new execution's object.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StartInit {
    pub projection: ExecutionProjection,
    pub record: StartRecord,
    /// D1's admission decision; `None` when the version has no concurrency key.
    pub admission: Option<Admission>,
    /// The start's runs-list row (sequence 0). `summary.created_at_ms` is
    /// overwritten with the store clock, as native adapters do.
    pub derived: DerivedUpdate,
}

fn invalid(code: &'static str) -> StoreError {
    StoreError::InvalidBatch(code)
}

impl<B: DoSql> DoStore<B> {
    /// Initialize this object for a start D1 accepted. Refused when the
    /// object already holds an execution (`store.execution_exists`).
    pub fn initialize_start(&self, init: StartInit) -> Result<(), StoreError> {
        self.write(move |store| {
            let id = init.projection.execution_id.clone();
            store.named(&id)?;
            let summary = &init.derived.summary;
            if summary.execution_id != id
                || summary.sequence != 0
                || init
                    .admission
                    .as_ref()
                    .is_some_and(|a| a.execution_id != id)
                || init.derived.release_concurrency
            {
                return Err(invalid("store.invalid_derived_update"));
            }
            store.insert_head(&init.projection)?;
            let now = store.sql.now_ms();
            // D24: the deadline counts from admission. An unqueued run is
            // activated from its start time; a queued one waits for `admit`.
            let activation = match &init.admission {
                None => Some(now),
                Some(a) if a.state == AdmissionState::Admitted => {
                    Some(a.admitted_at_ms.unwrap_or(now))
                }
                Some(_) => None,
            };
            if let Some(at) = activation {
                crate::queue::alarm_time(at, "store.time_out_of_range")?;
            }
            store.put(
                "INSERT INTO start_record (id, org_id, project_id, execution_id, body, \
                 activation_at_ms) VALUES (1, ?, ?, ?, ?, ?)",
                &store.owned(
                    &id,
                    vec![
                        encode(&init.record)?,
                        activation
                            .map(int)
                            .transpose()?
                            .unwrap_or(crate::Value::Null),
                    ],
                ),
            )?;
            if let Some(intent) = queued_outbox(&id, &init.record, init.admission.as_ref(), now) {
                store.insert_intent(&id, &intent)?;
            }
            let mut derived = init.derived;
            derived.summary.created_at_ms = now;
            store.write_derived(&id, 0, now, &derived)?;
            Ok(())
        })
    }

    /// D1 admitted this queued run at `admitted_at_ms` (its deadline anchor,
    /// D24): the activation becomes due. Idempotent for the same time;
    /// refused once the run has started or was admitted at another time.
    pub fn admit(&self, id: &ExecutionId, admitted_at_ms: i64) -> Result<(), StoreError> {
        let id = id.clone();
        self.write(move |store| {
            let head = store.owned_head(&id)?;
            crate::queue::alarm_time(admitted_at_ms, "store.time_out_of_range")?;
            let current = store.sql.query(
                "SELECT activation_at_ms FROM start_record WHERE id = 1",
                &[],
            )?;
            let current = current
                .first()
                .ok_or(StoreError::NotFound)?
                .optional_integer(0)?;
            match current {
                Some(at) if at == admitted_at_ms => Ok(()),
                Some(_) => Err(invalid("start.already_admitted")),
                None if head.revision > 0 => Err(invalid("start.already_started")),
                None => {
                    store.sql.exec(
                        "UPDATE start_record SET activation_at_ms = ? WHERE id = 1",
                        &[int(admitted_at_ms)?],
                    )?;
                    store.bump_writes()
                }
            }
        })
    }

    /// The pinned start facts, for this tenant's execution only.
    pub fn start_record(&self, id: &ExecutionId) -> Result<Option<StartRecord>, StoreError> {
        match self.owned_head(id) {
            Ok(_) => {}
            Err(StoreError::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        }
        self.sql
            .query("SELECT body FROM start_record WHERE id = 1", &[])?
            .first()
            .map(|row| decode(row.text(0)?))
            .transpose()
    }
}
