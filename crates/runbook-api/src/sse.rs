//! The execution event stream's wire contract (`GET /v1/executions/{exe}/events`),
//! shared by the native server and the Workers bridge (CF4, D58) so both
//! serve byte-identical streams:
//! - the cursor is the `Last-Event-ID` header: absent means 0; present it
//!   is exactly one header of ASCII digits naming a sequence at or below
//!   the head, else `400 request.invalid_cursor`;
//! - a log with a gap after the cursor is `410 execution.history_expired`;
//! - one frame per event: `id: {seq}`, `event: {name}`, `data: {envelope}`;
//! - an idle stream sends `: keep-alive` comments; a failure mid-stream
//!   sends one `event: error` frame and ends;
//! - the stream ends after `ExecutionCompleted` (or when the run is closed).
use runbook_engine::{Event, EventEnvelope};

/// Response headers of an event stream.
pub const HEADERS: &[(&str, &str)] = &[
    ("content-type", "text/event-stream"),
    ("cache-control", "no-store"),
    ("x-accel-buffering", "no"),
];

/// An idle stream's comment frame.
pub const KEEP_ALIVE: &str = ": keep-alive\n\n";

/// The cursor code for every refused `Last-Event-ID`.
pub const INVALID_CURSOR: &str = "request.invalid_cursor";

/// The cursor from every `Last-Event-ID` header value the request carried.
pub fn cursor<'a>(values: impl IntoIterator<Item = Option<&'a str>>) -> Result<u64, &'static str> {
    let mut values = values.into_iter();
    let Some(first) = values.next() else {
        return Ok(0);
    };
    if values.next().is_some() {
        return Err(INVALID_CURSOR);
    }
    first
        .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(INVALID_CURSOR)
}

/// The frame for one event, or `None` if it cannot be encoded.
pub fn frame(event: &EventEnvelope) -> Option<String> {
    let json = serde_json::to_string(event).ok()?;
    Some(format!(
        "id: {}\nevent: {}\ndata: {}\n\n",
        event.seq,
        event.event.name(),
        json
    ))
}

/// The frame a stream ends with when reading fails mid-stream.
pub fn error_frame(code: &str) -> String {
    format!("event: error\ndata: {{\"code\":\"{code}\"}}\n\n")
}

/// Whether the stream ends after this event.
pub fn is_last(event: &EventEnvelope) -> bool {
    matches!(event.event, Event::ExecutionCompleted { .. })
}

/// Check a stored log against the cursor: past the head is
/// `(400, request.invalid_cursor)`; a gap between the cursor and the head
/// (retention compacted it) is `(410, execution.history_expired)`.
pub fn check_history(
    events: &[EventEnvelope],
    revision: u64,
    after: u64,
) -> Result<(), (u16, &'static str)> {
    if after > revision {
        return Err((400, INVALID_CURSOR));
    }
    let mut expected = after;
    for event in events.iter().filter(|e| e.seq > after) {
        if expected.checked_add(1) != Some(event.seq) {
            return Err((410, "execution.history_expired"));
        }
        expected = event.seq;
    }
    if expected != revision {
        return Err((410, "execution.history_expired"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use runbook_model::ActorRef;

    fn event(seq: u64, last: bool) -> EventEnvelope {
        EventEnvelope {
            execution_id: "exe_00000000000000000000000001".parse().unwrap(),
            seq,
            at_ms: 10,
            actor: ActorRef::System,
            event: if last {
                Event::ExecutionCompleted {
                    status: runbook_model::ExecutionStatus::Succeeded,
                    outputs: Default::default(),
                }
            } else {
                Event::ManualPauseChanged { paused: false }
            },
        }
    }

    #[test]
    fn the_cursor_is_one_header_of_digits() {
        assert_eq!(cursor([]), Ok(0));
        assert_eq!(cursor([Some("0")]), Ok(0));
        assert_eq!(cursor([Some("42")]), Ok(42));
        for bad in ["", "-1", "1.5", "abc", " 1", "18446744073709551616"] {
            assert_eq!(cursor([Some(bad)]), Err(INVALID_CURSOR), "{bad:?}");
        }
        assert_eq!(cursor([None]), Err(INVALID_CURSOR), "not text");
        assert_eq!(
            cursor([Some("1"), Some("2")]),
            Err(INVALID_CURSOR),
            "two headers"
        );
    }

    #[test]
    fn frames_carry_the_sequence_name_and_envelope() {
        let text = frame(&event(3, false)).unwrap();
        assert!(
            text.starts_with("id: 3\nevent: ManualPauseChanged\ndata: {"),
            "{text}"
        );
        assert!(text.ends_with("}\n\n"));
        let data: EventEnvelope =
            serde_json::from_str(text.lines().nth(2).unwrap().strip_prefix("data: ").unwrap())
                .unwrap();
        assert_eq!(data, event(3, false));
        assert!(is_last(&event(4, true)) && !is_last(&event(4, false)));
        assert_eq!(
            error_frame("storage.unavailable"),
            "event: error\ndata: {\"code\":\"storage.unavailable\"}\n\n"
        );
    }

    #[test]
    fn the_history_must_be_whole_from_the_cursor_to_the_head() {
        let log = [event(1, false), event(2, false), event(3, false)];
        assert_eq!(check_history(&log, 3, 0), Ok(()));
        assert_eq!(check_history(&log, 3, 3), Ok(()));
        assert_eq!(check_history(&log, 3, 4), Err((400, INVALID_CURSOR)));
        assert_eq!(
            check_history(&log[1..], 3, 0),
            Err((410, "execution.history_expired"))
        );
        assert_eq!(
            check_history(&log, 4, 0),
            Err((410, "execution.history_expired"))
        );
    }
}
