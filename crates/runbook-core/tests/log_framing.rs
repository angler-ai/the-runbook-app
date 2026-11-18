use runbook_core::{
    ClaimWrite, StoreError,
    actor::{ActorPrincipal, Replay},
    claims::{ClaimStore, append_logs},
};
use runbook_model::{ActorRef, ClaimRef, ExecutionId, LogsRequest, StepRunId};
use std::{
    cell::RefCell,
    future::Future,
    pin::pin,
    task::{Context, Poll, Waker},
};

#[derive(Default)]
struct RecordingStore(RefCell<Vec<Vec<String>>>);
impl ClaimStore for RecordingStore {
    async fn heartbeat(
        &self,
        _: &ExecutionId,
        _: &StepRunId,
        _: &ClaimRef,
        _: &ActorRef,
        _: Option<&Replay>,
    ) -> Result<Option<i64>, StoreError> {
        unreachable!()
    }
    async fn append_logs(
        &self,
        _: &ExecutionId,
        _: &StepRunId,
        _: &ClaimRef,
        _: &ActorRef,
        lines: Vec<String>,
        _: Option<&Replay>,
    ) -> Result<u64, StoreError> {
        let count = lines.len() as u64;
        self.0.borrow_mut().push(lines);
        Ok(count)
    }
    async fn claim_record(
        &self,
        _: &ExecutionId,
        _: &StepRunId,
    ) -> Result<Option<ClaimWrite>, StoreError> {
        unreachable!()
    }
    async fn read_logs(
        &self,
        _: &ExecutionId,
        _: &StepRunId,
        _: u64,
        _: usize,
    ) -> Result<Vec<(u64, String)>, StoreError> {
        unreachable!()
    }
}
fn ready<T>(future: impl Future<Output = T>) -> T {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("recording store is synchronous"),
    }
}
#[test]
fn logs_are_redacted_per_request() {
    let store = RecordingStore::default();
    let principal = ActorPrincipal {
        principal: "test".into(),
        actor: ActorRef::System,
        credential_ref: "test".into(),
    };
    let id = "exe_00000000000000000000000001".parse().unwrap();
    let step = "sr_00000000000000000000000001".parse().unwrap();
    let claim = ClaimRef {
        id: "clm_00000000000000000000000001".parse().unwrap(),
        generation: 1.try_into().unwrap(),
    };
    let requests = [
        vec![
            "Authorization: Bearer credential",
            "ordinary\nsecond",
            "-----BEGIN PRIVATE KEY-----",
            "private material",
            "-----END PRIVATE KEY-----",
            "after",
            "",
        ],
        vec!["-----BEGIN PRIVATE KEY-----", "unfinished private material"],
        vec!["next request", "Authorization:", "safe next entry"],
        vec![],
    ];
    for lines in requests {
        let request = LogsRequest {
            claim: claim.clone(),
            logs: lines.into_iter().map(str::to_owned).collect(),
        };
        ready(append_logs(&store, &principal, &id, &step, &request, None)).unwrap();
    }
    assert_eq!(
        *store.0.borrow(),
        vec![
            vec![
                "Authorization: [REDACTED:authorization]",
                "ordinary",
                "second",
                "[REDACTED:private_key]",
                "after",
                ""
            ],
            vec!["[REDACTED:private_key]"],
            vec!["next request", "Authorization:", "safe next entry"],
            vec![],
        ]
    );
}

/// CF12b review round 1 (findings 1 and 7): a line longer than one log
/// entry ([`MAX_LOG_ENTRY_BYTES`]) is stored as several entries, split on a
/// UTF-8 boundary after redaction, so neither an old client's long line
/// nor a redaction that lengthened a line is refused or stored past the
/// entry size.
#[test]
fn an_over_long_line_is_split_into_entries_after_redaction() {
    use runbook_core::claims::MAX_LOG_ENTRY_BYTES;
    let store = RecordingStore::default();
    let principal = ActorPrincipal {
        principal: "test".into(),
        actor: ActorRef::System,
        credential_ref: "test".into(),
    };
    let id = "exe_00000000000000000000000001".parse().unwrap();
    let step = "sr_00000000000000000000000001".parse().unwrap();
    let claim = ClaimRef {
        id: "clm_00000000000000000000000001".parse().unwrap(),
        generation: 1.try_into().unwrap(),
    };
    // Two-byte characters, so the cut falls inside one unless it moves.
    let long = format!("a{}", "é".repeat(MAX_LOG_ENTRY_BYTES));
    // Short secrets whose markers are longer than they are: redaction
    // makes a line at the entry size longer than it.
    let pair = "token=x ";
    let grows = pair.repeat(MAX_LOG_ENTRY_BYTES / pair.len());
    let request = LogsRequest {
        claim: claim.clone(),
        logs: vec![long.clone(), "short".into(), grows.clone()],
    };
    ready(append_logs(&store, &principal, &id, &step, &request, None)).unwrap();
    let stored = store.0.borrow()[0].clone();
    assert!(stored.iter().all(|e| e.len() <= MAX_LOG_ENTRY_BYTES));
    // The long line, whole and in order, then the short one.
    let n = long.len().div_ceil(MAX_LOG_ENTRY_BYTES);
    assert_eq!(stored[..n].concat(), long);
    assert_eq!(stored[n], "short");
    let redacted: String = stored[n + 1..].concat();
    assert!(
        redacted.len() > MAX_LOG_ENTRY_BYTES,
        "redaction lengthened it"
    );
    assert!(!redacted.contains("token=x "), "redacted");
    assert!(stored.len() > n + 2, "split after redaction");
}
