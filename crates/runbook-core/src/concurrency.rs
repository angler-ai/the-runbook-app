//! Concurrency pool decisions (D25): a start's limit replaces the pool's
//! current limit. Raising it promotes waiters in the same transaction;
//! lowering it preempts nobody — it admits no one new until the active
//! count drops below the limit. There is no limit-conflict error.
//!
//! LANG3 adds the start's `on_conflict` policy. Its rules live here, in the
//! portable core, and every adapter (Postgres, memory, the Cloudflare
//! edition) applies them inside its start transaction under the pool lock:
//! the adapter only reads the pool ([`PoolView`]), calls [`resolve`], and
//! performs the writes it returns.
use crate::{
    ExecutionCommit, StoreError,
    catalog::{AdmissionState, TenantScope},
};
use runbook_engine::{EngineError, ExecutionState};
use runbook_model::ExecutionId;
pub use runbook_spec::ir::ConcurrencyPolicy;
use serde::{Deserialize, Serialize};

/// How many queued runs a pool with this active count may promote now.
pub fn promotable(active: u32, limit: u32) -> u32 {
    limit.saturating_sub(active)
}

/// The run's evaluated concurrency key and limit, from its compiled spec.
pub fn resolve_concurrency(state: &ExecutionState) -> Result<Option<(String, u32)>, EngineError> {
    let Some(concurrency) = &state.compiled().ir().execution.concurrency else {
        return Ok(None);
    };
    let key = runbook_engine::evaluate_concurrency_key(state)?.ok_or(EngineError::Rejected {
        code: "execution.concurrency_missing",
        errors: vec![],
    })?;
    Ok(Some((key, concurrency.limit)))
}

/// The stable code of a start refused by `on_conflict: reject` (409).
pub const CONFLICT_CODE: &str = "execution.concurrency_conflict";
/// The pool changed between the core's read and the adapter's locked read;
/// [`crate::start::commit_start_with_policy`] prepares again and retries.
pub const STALE_CODE: &str = "start.preemption_stale";
/// The retry budget for stale preemptions; exhausting it is
/// [`CONTENDED_CODE`] (409).
pub const MAX_PREEMPTION_ATTEMPTS: usize = 8;
/// A `cancel_previous` start that kept losing pool races (409).
pub const CONTENDED_CODE: &str = "execution.concurrency_contended";
/// The most runs one `cancel_previous` start prepares cancellations for.
pub const MAX_PREEMPTED_RUNS: usize = 50;
/// More than [`MAX_PREEMPTED_RUNS`] would have to be cancelled (409).
pub const TOO_MANY_HOLDERS_CODE: &str = "execution.concurrency_too_many_holders";
/// A run to be cancelled could not be prepared (409); the problem names it.
pub const HOLDER_UNAVAILABLE_CODE: &str = "execution.concurrency_holder_unavailable";

/// One run holding (admitted) or waiting for (queued) a concurrency key.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PoolHolder {
    pub execution_id: ExecutionId,
    pub state: AdmissionState,
}
impl PartialOrd for AdmissionState {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for AdmissionState {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let rank = |s: &Self| match s {
            Self::Admitted => 0,
            Self::Queued => 1,
            Self::Released => 2,
        };
        rank(self).cmp(&rank(other))
    }
}

/// A key's pool: the runs holding it in admission order (oldest first,
/// ties by execution ID), then its waiters in FIFO order. Released
/// admissions are not part of the pool.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolView {
    pub holders: Vec<PoolHolder>,
}
impl PoolView {
    pub fn active(&self) -> u32 {
        self.count(AdmissionState::Admitted)
    }
    pub fn queued(&self) -> u32 {
        self.count(AdmissionState::Queued)
    }
    fn count(&self, state: AdmissionState) -> u32 {
        self.holders.iter().filter(|h| h.state == state).count() as u32
    }
}

/// Whether a start with `limit` would wait in this pool. D25: the request
/// limit replaces the pool's, waiters are promoted first, so the new run is
/// admitted only if a slot is left after every waiter that fits.
pub fn would_queue(limit: u32, view: &PoolView) -> bool {
    view.queued() >= promotable(view.active(), limit)
}

/// LANG3 decision 6: the runs a conflicting `cancel_previous` start
/// cancels. Every waiter (all of them are queued ahead of it), then, with
/// `limit: 1`, every holder; with a wider pool only the oldest holder.
pub fn evictions(limit: u32, view: &PoolView) -> Vec<PoolHolder> {
    let waiters = view
        .holders
        .iter()
        .filter(|h| h.state == AdmissionState::Queued);
    let mut holders = view
        .holders
        .iter()
        .filter(|h| h.state == AdmissionState::Admitted);
    let holders: Vec<&PoolHolder> = if limit <= 1 {
        holders.collect()
    } else {
        holders.next().into_iter().collect()
    };
    waiters.chain(holders).cloned().collect()
}

/// A start's conflict policy, carried by its start commit. `None` on the
/// commit is `queue` (D24/D25).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum OnConflict {
    /// Refuse the start when it would wait. Nothing is written.
    Reject,
    /// When the start would wait, cancel [`evictions`] and queue it first in
    /// line. `view` is the pool the core read and `commits` the prepared
    /// cancellations (one per evicted run that still had something to
    /// cancel); the adapter applies them only if its locked pool is still
    /// `view`.
    CancelPrevious {
        view: PoolView,
        commits: Vec<ExecutionCommit>,
    },
}
impl OnConflict {
    pub fn for_policy(policy: ConcurrencyPolicy) -> Option<Self> {
        match policy {
            ConcurrencyPolicy::Queue => None,
            ConcurrencyPolicy::Reject => Some(Self::Reject),
            ConcurrencyPolicy::CancelPrevious => Some(Self::CancelPrevious {
                view: PoolView::default(),
                commits: vec![],
            }),
        }
    }
}

/// What the adapter does with the start's admission, decided by
/// [`resolve`] against its locked pool.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolution {
    /// Pooled admission as usual (D25): admitted, or queued under `queue`.
    Admit,
    /// LANG3 `cancel_previous`: release the evicted `waiters`' admissions,
    /// apply the prepared cancellations (an evicted holder keeps its slot
    /// until it closes; one that closes in its commit frees it there), then
    /// admit the start first in line: admitted if a slot is free, otherwise
    /// queued ahead of every other waiter.
    Preempt { waiters: Vec<ExecutionId> },
}

/// LANG3: resolve a start's `on_conflict` against the pool the adapter
/// read under its lock, in the start transaction.
///
/// - No conflict (the start would be admitted) or `queue`: [`Resolution::Admit`].
/// - `reject` on conflict: [`CONFLICT_CODE`]; the adapter rolls back.
/// - `cancel_previous` on conflict: [`Resolution::Preempt`] when the pool
///   is the one the core prepared against, otherwise [`STALE_CODE`].
pub fn resolve(
    on_conflict: Option<&OnConflict>,
    limit: u32,
    view: &PoolView,
) -> Result<Resolution, StoreError> {
    let Some(policy) = on_conflict else {
        return Ok(Resolution::Admit);
    };
    if !would_queue(limit, view) {
        return Ok(Resolution::Admit);
    }
    match policy {
        OnConflict::Reject => Err(StoreError::InvalidBatch(CONFLICT_CODE)),
        OnConflict::CancelPrevious {
            view: prepared,
            commits,
        } => {
            let evicted = evictions(limit, view);
            if prepared != view
                || commits
                    .iter()
                    .any(|c| !evicted.iter().any(|h| h.execution_id == c.execution_id))
            {
                return Err(StoreError::InvalidBatch(STALE_CODE));
            }
            Ok(Resolution::Preempt {
                waiters: evicted
                    .into_iter()
                    .filter(|h| h.state == AdmissionState::Queued)
                    .map(|h| h.execution_id)
                    .collect(),
            })
        }
    }
}

/// Why `cancel_previous` could not prepare its cancellations.
#[derive(Debug)]
pub(crate) enum PreparationError {
    TooMany,
    Unavailable(ExecutionId),
    Store(StoreError),
}

/// LANG3 `cancel_previous`: the cancellation each evicted run gets,
/// prepared through the ordinary actor path (the same events and settle as
/// a person's cancel, recorded by the system with the replacing run as
/// `superseded`) but not committed: the start transaction applies it,
/// fenced by the run's sequence. A run already cancelling has nothing to
/// append and gets no commit. The directory is read once; each run is read
/// through a released session, so preparing holds no lock or connection.
pub(crate) async fn cancellation_commits<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    evicted: &[PoolHolder],
    superseded: &runbook_engine::Supersession,
) -> Result<Vec<ExecutionCommit>, PreparationError>
where
    S: crate::start::StartStore + crate::blob::BlobStore + crate::session::SessionStore,
    D: crate::directory::DirectoryStore,
{
    if evicted.len() > MAX_PREEMPTED_RUNS {
        return Err(PreparationError::TooMany);
    }
    if evicted.is_empty() {
        return Ok(vec![]);
    }
    let snapshot = directory
        .snapshot(&scope.org_id)
        .await
        .map_err(|_| PreparationError::Store(StoreError::Backend("actor.directory_unavailable")))?;
    let mut commits = vec![];
    for run in evicted {
        let unavailable = || PreparationError::Unavailable(run.execution_id.clone());
        // A storage outage is a storage error (503), not a 409 naming the
        // holder. `NotFound` stays a 409: it also means the holder's pinned
        // snapshot is gone, which no retry fixes.
        let mut work = match crate::actor::begin_detached(
            store,
            snapshot.clone(),
            scope.clone(),
            &run.execution_id,
        )
        .await
        {
            Ok(work) => work,
            Err(crate::actor::ActorError::Store(error @ StoreError::Backend(_))) => {
                return Err(PreparationError::Store(error));
            }
            Err(_) => return Err(unavailable()),
        };
        crate::control::prepare_cancel_as(
            &mut work,
            runbook_model::ActorRef::System,
            Some(superseded),
        )
        .map_err(|_| unavailable())?;
        if work.has_pending() {
            work.settle().map_err(|_| unavailable())?;
            commits.push(work.into_commit());
        }
    }
    Ok(commits)
}

/// LANG3 on adapters whose start commits before its evictions can be
/// applied (the Cloudflare edition: the start's D1 batch decides, and each
/// evicted run lives in its own object). Prepare `run`'s cancellation now,
/// against its current state, exactly as [`cancellation_commits`] does, and
/// commit it, preparing again if the run moved meanwhile. `Ok(true)` when a
/// commit landed; `Ok(false)` when nothing was left to cancel or the run is
/// gone (its pinned snapshot or the run itself), which no retry fixes.
pub async fn cancel_superseded<S, D>(
    store: &S,
    directory: &D,
    scope: &TenantScope,
    run: &PoolHolder,
    superseded: &runbook_engine::Supersession,
) -> Result<bool, StoreError>
where
    S: crate::start::StartStore + crate::blob::BlobStore + crate::session::SessionStore,
    D: crate::directory::DirectoryStore,
{
    for _ in 0..MAX_PREEMPTION_ATTEMPTS {
        let commits = match cancellation_commits(
            store,
            directory,
            scope,
            std::slice::from_ref(run),
            superseded,
        )
        .await
        {
            Ok(commits) => commits,
            Err(PreparationError::Store(error)) => return Err(error),
            Err(PreparationError::Unavailable(_) | PreparationError::TooMany) => {
                return Ok(false);
            }
        };
        let Some(commit) = commits.into_iter().next() else {
            return Ok(false);
        };
        match store.commit(commit).await? {
            crate::CommitResult::Committed(_) => return Ok(true),
            crate::CommitResult::Conflict => continue,
        }
    }
    Err(StoreError::InvalidBatch(CONTENDED_CODE))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn holder(n: u8, state: AdmissionState) -> PoolHolder {
        PoolHolder {
            execution_id: format!("exe_{n:026}").parse().unwrap(),
            state,
        }
    }
    fn view(holders: &[(u8, AdmissionState)]) -> PoolView {
        PoolView {
            holders: holders.iter().map(|(n, s)| holder(*n, *s)).collect(),
        }
    }
    fn prepared(v: &PoolView) -> OnConflict {
        OnConflict::CancelPrevious {
            view: v.clone(),
            commits: vec![],
        }
    }
    use AdmissionState::{Admitted, Queued};

    #[test]
    fn a_start_waits_only_when_no_slot_is_left_after_the_waiters() {
        assert!(!would_queue(1, &view(&[])));
        assert!(would_queue(1, &view(&[(1, Admitted)])));
        assert!(!would_queue(2, &view(&[(1, Admitted)])));
        // A raised limit promotes the waiter first; nothing is left.
        assert!(would_queue(2, &view(&[(1, Admitted), (2, Queued)])));
        assert!(!would_queue(3, &view(&[(1, Admitted), (2, Queued)])));
        // A lowered limit (D25: active may exceed it).
        assert!(would_queue(1, &view(&[(1, Admitted), (2, Admitted)])));
    }

    #[test]
    fn queue_and_free_slots_admit() {
        let full = view(&[(1, Admitted)]);
        assert_eq!(resolve(None, 1, &full).unwrap(), Resolution::Admit);
        for policy in [OnConflict::Reject, prepared(&full)] {
            assert_eq!(resolve(Some(&policy), 2, &full).unwrap(), Resolution::Admit);
        }
    }

    #[test]
    fn reject_refuses_a_full_pool() {
        let full = view(&[(1, Admitted)]);
        assert!(matches!(
            resolve(Some(&OnConflict::Reject), 1, &full),
            Err(StoreError::InvalidBatch(CONFLICT_CODE))
        ));
    }

    #[test]
    fn evictions_take_every_waiter_and_the_holders_by_limit() {
        let pool = view(&[(1, Admitted), (2, Admitted), (3, Queued), (4, Queued)]);
        assert_eq!(
            evictions(1, &pool),
            [
                holder(3, Queued),
                holder(4, Queued),
                holder(1, Admitted),
                holder(2, Admitted)
            ]
        );
        // Decision 6: a wider pool loses only its oldest holder.
        assert_eq!(
            evictions(2, &pool),
            [holder(3, Queued), holder(4, Queued), holder(1, Admitted)]
        );
    }

    #[test]
    fn cancel_previous_preempts_only_the_prepared_pool() {
        let pool = view(&[(1, Admitted), (2, Queued), (3, Queued)]);
        let ids = |ns: &[u8]| -> Vec<ExecutionId> {
            ns.iter().map(|n| holder(*n, Queued).execution_id).collect()
        };
        assert_eq!(
            resolve(Some(&prepared(&pool)), 1, &pool).unwrap(),
            Resolution::Preempt {
                waiters: ids(&[2, 3])
            }
        );
        // A run that joined, left, changed state or moved: stale.
        for changed in [
            view(&[(1, Admitted), (2, Queued), (3, Queued), (4, Queued)]),
            view(&[(1, Admitted), (2, Queued)]),
            view(&[(1, Admitted), (2, Admitted), (3, Queued)]),
            view(&[(1, Admitted), (3, Queued), (2, Queued)]),
        ] {
            assert!(matches!(
                resolve(Some(&prepared(&pool)), 1, &changed),
                Err(StoreError::InvalidBatch(STALE_CODE))
            ));
        }
        // Prepared with no holders (the core saw a free slot): stale too.
        let empty = OnConflict::for_policy(ConcurrencyPolicy::CancelPrevious).unwrap();
        assert!(matches!(
            resolve(Some(&empty), 1, &pool),
            Err(StoreError::InvalidBatch(STALE_CODE))
        ));
    }
}
