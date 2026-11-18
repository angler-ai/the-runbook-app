use runbook_expr::{EvalSession, EvalStats};

#[test]
fn session_rejects_fuel_above_the_hard_cap() {
    assert!(EvalSession::new(100_000).is_ok());
    assert!(EvalSession::new(100_001).is_err());
    assert!(EvalSession::new(0).is_ok());
}

#[test]
fn cancellation_preserves_zero_statistics() {
    let mut session = EvalSession::new(100).unwrap();
    let expected = EvalStats {
        cost: 0,
        loop_entries: 0,
        reads: vec![],
    };
    assert_eq!(session.stats(), expected);
    session.cancel();
    session.cancel();
    assert_eq!(session.stats(), expected);
}
