//! CF12b review round 2: how a client sends log lines to a step's logs
//! route. `runbook step exec`, `step log`, `step report --log-file` and MCP
//! `append_log` all use this module.
//!
//! A client only batches. It never splits a line into entries: the server
//! redacts secrets first and then splits a long line into 64 KiB entries
//! (`runbook_core::claims::split_log_lines`). A client split made before
//! redaction could cut a secret in two, and then the redactor would miss
//! the tail.
//!
//! The one exception is a line too large for one request. The server takes
//! a JSON body of at most 2 MiB + 64 KiB (`runbook_api::http::JSON_BODY_BYTES`).
//! A line whose JSON is over [`LINE_JSON_BYTES`] (2 MiB) is cut into pieces
//! of at most that size. Each cut falls after the last whitespace in the
//! second half of its piece, else exactly at the cap. [`Batches::cut_lines`]
//! counts such lines so the caller can say so.
//!
//! CF12c: a whitespace cut can still fall between a key and its value
//! (`"password": "…"`, `password = …`, `password="… with spaces …"`,
//! `Authorization: Bearer …`), and then the server would store the value
//! in clear. So a line that must be cut is first redacted with the
//! server's redactor (`runbook_core::redact`), and what that leaves is
//! cut ([`batches`]: CF12c review round 1 bounds what one bad construct
//! there can drop to about one request). The server redacts each
//! request again. A second pass never reveals anything and never redacts
//! less (CF12c review round 1, F3): its text is the first's with
//! characters removed. It is not always a no-op: an orphan private-key END
//! line drops the lines before it in its request. Redacting each side of
//! any cut of the first pass's output reveals nothing either
//! (`runbook-core/tests/redact_fuzz.rs`).
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

/// At most this much JSON in one `/logs` request's `logs`.
pub const BATCH_JSON_BYTES: usize = 256 * 1024;
/// At most this many entries in one `/logs` request.
pub const BATCH_LINES: usize = 1000;
/// The most one line takes as JSON in one request: the server's JSON body
/// cap (2 MiB + 64 KiB) less 64 KiB for the claim and the framing.
pub const LINE_JSON_BYTES: usize = 2 * 1_048_576;
/// The most a report's `logs` take as JSON (the API's
/// `field_caps::REPORT_LOG_BYTES`).
pub const REPORT_LOG_JSON_BYTES: usize = 1_048_576;
/// The longest `Idempotency-Key` the server takes, in bytes.
pub const KEY_BYTES: usize = 256;
/// CF12c review round 1 (F4): how far past a window's end an open
/// construct (a quoted value, a private key, a header's line) may run
/// before the client's redactor gives it up there (one stored entry's
/// worth). Review round 2 (N2): only such a construct counts.
pub const CARRY_BYTES: usize = 65_536;

/// How many bytes `c` takes inside a JSON string, as `serde_json` writes
/// it.
fn char_json_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\u{8}' | '\u{c}' | '\n' | '\r' | '\t' => 2,
        c if (c as u32) < 0x20 => 6,
        c => c.len_utf8(),
    }
}

/// How many bytes `text` takes as a JSON string, quotes included.
pub fn json_len(text: &str) -> usize {
    2 + text.chars().map(char_json_len).sum::<usize>()
}

/// `line` as pieces whose JSON is at most `max` bytes each. A line that
/// fits is one piece. A piece ends after the last whitespace in its second
/// half, or else exactly at the cap.
pub fn cut_line(line: &str, max: usize) -> Vec<&str> {
    if json_len(line) <= max {
        return vec![line];
    }
    // The content budget (no quotes). A character takes at most 6 bytes,
    // so every piece makes progress.
    let budget = max.saturating_sub(2).max(12);
    let mut pieces = vec![];
    let (mut start, mut used) = (0, 0);
    // The end of the last whitespace in the piece's second half.
    let mut space: Option<usize> = None;
    for (i, c) in line.char_indices() {
        let n = char_json_len(c);
        if used + n > budget {
            let at = space.unwrap_or(i);
            pieces.push(&line[start..at]);
            used = line[at..i].chars().map(char_json_len).sum();
            start = at;
            space = None;
        }
        used += n;
        if c.is_whitespace() && used >= budget / 2 {
            space = Some(i + c.len_utf8());
        }
    }
    pieces.push(&line[start..]);
    pieces
}

/// Log lines as `/logs` requests.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Batches {
    pub batches: Vec<Vec<String>>,
    /// Lines too large for one request, sent as several entries.
    pub cut_lines: usize,
}

/// `lines` as `/logs` requests: each line whole unless it is too large for
/// one request ([`cut_line`] at [`LINE_JSON_BYTES`]), then batched
/// ([`take_batch`]).
///
/// CF12c: a line too large for one request is redacted first, so no cut
/// separates a secret from its key. CF12c review round 1 (F4): that line
/// only, read as the server would have read its requests, and the lines
/// after it while a construct it opened stays open ([`LongLine`]); other
/// lines go as they are (the server redacts each request).
pub fn batches(lines: impl IntoIterator<Item = String>) -> Batches {
    batches_within(lines, Limits::default())
}

/// The sizes [`batches`] works to. The default is the API's; the review's
/// fuzz test (`tests/log_cuts.rs`) uses small ones, so a few kilobytes of
/// text cross many windows.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// [`LINE_JSON_BYTES`].
    pub line_json: usize,
    /// [`BATCH_JSON_BYTES`].
    pub batch_json: usize,
    /// [`BATCH_LINES`].
    pub batch_lines: usize,
    /// [`CARRY_BYTES`].
    pub carry: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            line_json: LINE_JSON_BYTES,
            batch_json: BATCH_JSON_BYTES,
            batch_lines: BATCH_LINES,
            carry: CARRY_BYTES,
        }
    }
}

/// [`batches`] to `limits`.
pub fn batches_within(lines: impl IntoIterator<Item = String>, limits: Limits) -> Batches {
    let mut lines = lines.into_iter();
    let mut pending = VecDeque::new();
    let mut cut_lines = 0;
    while let Some(line) = lines.next() {
        if json_len(&line) <= limits.line_json {
            pending.push_back(line);
            continue;
        }
        cut_lines += 1;
        let mut long = LongLine {
            carry: limits.carry,
            ..LongLine::default()
        };
        for window in cut_line(&line, limits.line_json) {
            long.push(window);
        }
        // A construct still open at the line's end (a private key whose
        // body follows, a quoted value or a list that continues) is read
        // on into the next lines, an open construct within its carry.
        while !long.redactor.idle() {
            let Some(next) = lines.next() else {
                break;
            };
            if json_len(&next) > limits.line_json {
                cut_lines += 1;
            }
            long.push("\n");
            for window in cut_line(&next, limits.line_json) {
                long.push(window);
            }
        }
        for entry in long.finish() {
            pending.extend(
                cut_line(&entry, limits.line_json)
                    .into_iter()
                    .map(str::to_owned),
            );
        }
    }
    let mut batches = vec![];
    while !pending.is_empty() {
        batches.push(take_batch_within(&mut pending, limits));
    }
    Batches { batches, cut_lines }
}

/// CF12c review round 1 (F4): a line too large for one request, redacted
/// with core's redactor (the server's) before it is cut.
///
/// One redactor reads the line's windows (its [`cut_line`] pieces at
/// [`LINE_JSON_BYTES`]: the requests the server would have redacted one by
/// one) in order, so a key and its value on either side of a window's end
/// stay together. Past a window's end, an open construct (one that only
/// its own end closes and that writes nothing while it runs: a quoted
/// value, a private key, a header's line) may run [`CARRY_BYTES`] from
/// where it began or from the window's end, whichever is later, into the
/// next window or the next lines. Anything else still open there is
/// bounded or written as it goes (a held prefix, a key waiting for its
/// value across whitespace, a list between its items, a value word) and
/// is read on without a bound (CF12c review round 2, N2).
///
/// An open construct that has not ended by then is given up there
/// (`Redactor::cut_short`), and the redactor reads on, as the server reads
/// a new request, in a new entry (so the server's own pass ends a header
/// line or a word it reads again there). A cut never ends a discard or a
/// value mid-word (N1): a header line is discarded to the end of the word
/// the cut falls in, and what it was reading goes on (a key glued into it
/// still takes its value, a quote or a private key it opened runs one more
/// carry); a quoted value or a private key ends (an open quote closed) and
/// the rest of the word is discarded. A header line and a quoted value or
/// private key each count their own carry (CF12c review round 3): one that
/// opens and closes inside the line does not restart the line's count. A
/// header named on a discarded header line starts its own carry. So one
/// bad construct (an unterminated quote, an overlong `-----BEGIN` header)
/// drops at most the rest of its window and twice the carry: about what
/// the server alone drops, one request's worth. Redacting the whole input as one stream let
/// it drop the rest of the input, up to the step's 5 MiB.
#[derive(Default)]
struct LongLine {
    redactor: runbook_core::redact::Redactor,
    /// The redacted text, `\n` between lines; each a run of entries.
    segments: Vec<String>,
    out: String,
    /// Bytes read, since a boundary, inside the open construct now being
    /// read (CF12c review round 2, N2: only an
    /// [open construct](runbook_core::redact::Redactor::open_construct)
    /// counts, each from its own start).
    ///
    /// CF12c review round 3: a discarded header line and a quoted value or
    /// private key each keep their own count, so one that opens and closes
    /// inside the line does not restart the line's.
    line: Carried,
    value: Carried,
    /// [`CARRY_BYTES`].
    carry: usize,
}

/// How far one open construct has run since a boundary.
#[derive(Default)]
struct Carried {
    /// Which construct (`Redactor::open_line` or `open_value`), while one
    /// is open.
    id: Option<u64>,
    bytes: usize,
}

impl Carried {
    /// `len` more bytes read, `now` the construct open after them.
    fn read(&mut self, now: Option<u64>, len: usize) {
        self.bytes = match now {
            None => 0,
            Some(_) if now != self.id => len,
            Some(_) => self.bytes + len,
        };
        self.id = now;
    }
    fn over(&self, carry: usize) -> bool {
        self.id.is_some() && self.bytes >= carry
    }
}

impl LongLine {
    /// Read `text`, which follows a boundary (a window's end, a line's).
    fn push(&mut self, text: &str) {
        let mut rest = text;
        while !self.redactor.idle() {
            let Some(c) = rest.chars().next() else {
                return;
            };
            if self.line.over(self.carry) || self.value.over(self.carry) {
                // The outermost construct gives up; what it held open (a
                // quote or a key a header line opened) runs one more carry.
                self.out.push_str(&self.redactor.cut_short());
                self.segments.push(std::mem::take(&mut self.out));
                self.line.bytes = 0;
                self.value.bytes = 0;
                continue;
            }
            self.redactor
                .push_into(c.encode_utf8(&mut [0; 4]), &mut self.out);
            // A construct that began here starts its count (also a header
            // named on a discarded header line: its own line from there).
            self.line.read(self.redactor.open_line(), c.len_utf8());
            self.value.read(self.redactor.open_value(), c.len_utf8());
            rest = &rest[c.len_utf8()..];
        }
        self.line = Carried::default();
        self.value = Carried::default();
        self.redactor.push_into(rest, &mut self.out);
    }
    /// The entries: each line of each segment.
    fn finish(mut self) -> Vec<String> {
        self.out.push_str(&self.redactor.finish_closed());
        if !self.out.is_empty() || self.segments.is_empty() {
            self.segments.push(self.out);
        }
        self.segments
            .iter()
            .flat_map(|segment| segment.split('\n'))
            .map(str::to_owned)
            .collect()
    }
}

/// The next `/logs` request's entries from the front of `pending`: at most
/// [`BATCH_LINES`] and [`BATCH_JSON_BYTES`] of JSON, and at least one (a
/// line of up to [`LINE_JSON_BYTES`] goes alone).
pub fn take_batch(pending: &mut VecDeque<String>) -> Vec<String> {
    take_batch_within(pending, Limits::default())
}

/// [`take_batch`] to `limits`.
pub fn take_batch_within(pending: &mut VecDeque<String>, limits: Limits) -> Vec<String> {
    let mut lines = vec![];
    let mut bytes = 0;
    while let Some(line) = pending.front() {
        let size = json_len(line) + 1;
        if !lines.is_empty()
            && (lines.len() >= limits.batch_lines || bytes + size > limits.batch_json)
        {
            break;
        }
        bytes += size;
        lines.push(pending.pop_front().unwrap_or_default());
    }
    lines
}

/// Whether `lines` fit in one report's `logs` ([`REPORT_LOG_JSON_BYTES`]).
pub fn fits_report(lines: &[String]) -> bool {
    2 + lines.iter().map(|l| json_len(l) + 1).sum::<usize>() <= REPORT_LOG_JSON_BYTES + 1
}

/// The key of one of several requests made under the caller's key `base`:
/// `base` and `suffix` (`-logs-1`, `-report`…), or, when that would pass
/// [`KEY_BYTES`], `sha256-` and the hex SHA-256 of `base`, then `suffix`.
/// The same `base` always gives the same key, so a retry replays.
pub fn sub_key(base: &str, suffix: &str) -> String {
    if base.len() + suffix.len() <= KEY_BYTES {
        return format!("{base}{suffix}");
    }
    let digest: String = Sha256::digest(base.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256-{digest}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_lengths_match_serde() {
        for text in [
            "",
            "plain",
            "q\"b\\",
            "\u{1}\u{1f}\t\n\r\u{8}\u{c}\u{7f}",
            "é🙂",
        ] {
            assert_eq!(
                json_len(text),
                serde_json::to_string(text).unwrap().len(),
                "{text:?}"
            );
        }
    }

    /// The review's reproduction: a secret after 65,518 bytes stays whole
    /// in one entry for the server to redact.
    #[test]
    fn a_long_line_is_sent_whole() {
        let line = format!(
            "{} password=hunter2SUPERSECRETVALUE done",
            "x".repeat(65_518)
        );
        let sent = batches(vec!["a".into(), line.clone(), "b".into()]);
        assert_eq!(sent.cut_lines, 0);
        let entries: Vec<&String> = sent.batches.iter().flatten().collect();
        assert_eq!(entries, [&"a".to_string(), &line, &"b".to_string()]);
    }

    #[test]
    fn a_line_over_one_request_is_cut_after_whitespace() {
        let secret = "password=hunter2SUPERSECRETVALUE";
        // The secret straddles the cap: the cut falls before it.
        let head = "y".repeat(LINE_JSON_BYTES - 2 - 10);
        let line = format!("{head} {secret} {}", "z".repeat(1_000));
        let pieces = cut_line(&line, LINE_JSON_BYTES);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0], format!("{head} "));
        assert!(pieces[1].starts_with(secret), "{}", &pieces[1][..40]);
        assert_eq!(pieces.concat(), line);
        // Text with no whitespace is cut at the cap; control characters
        // count as JSON escapes.
        let dense = "\u{1}".repeat(LINE_JSON_BYTES);
        let pieces = cut_line(&dense, LINE_JSON_BYTES);
        assert!(pieces.iter().all(|p| json_len(p) <= LINE_JSON_BYTES));
        assert_eq!(pieces.concat(), dense);
        let sent = batches(vec![line, dense]);
        assert_eq!(sent.cut_lines, 2);
        // Each request stays under the body cap.
        for batch in &sent.batches {
            let body = serde_json::to_vec(&serde_json::json!({
                "claim": {"id": "clm_00000000000000000000000001", "generation": 1},
                "logs": batch,
            }))
            .unwrap();
            assert!(body.len() <= 2 * 1_048_576 + 65_536, "{}", body.len());
        }
    }

    /// CF12b's final review, item 1: the secrets whose key and value a
    /// whitespace cut can separate, as (the text, the secret in it).
    const SPACED_SECRETS: [(&str, &str); 4] = [
        (
            r#""password": "hunter2SUPERSECRETVALUE""#,
            "hunter2SUPERSECRETVALUE",
        ),
        (
            "password = hunter2SUPERSECRETVALUE",
            "hunter2SUPERSECRETVALUE",
        ),
        (
            r#"password="correct horse battery staple SUPERSECRETVALUE""#,
            "correct horse battery staple SUPERSECRETVALUE",
        ),
        (
            "Authorization: Bearer abcdefSUPERSECRETVALUE",
            "abcdefSUPERSECRETVALUE",
        ),
    ];

    /// A line over one request holding `text` where a cut after whitespace
    /// lands `back` content bytes after the last whitespace in `text`, so
    /// the whitespace cut separates the key from (part of) its value.
    fn straddling(text: &str, back: usize) -> String {
        let after_space = text.rfind(' ').unwrap() + 1;
        let lead = json_len(&text[..after_space + back]) - 2;
        // The cut's content budget is LINE_JSON_BYTES - 2.
        let head = "x".repeat(LINE_JSON_BYTES - 2 - lead);
        format!("{head}{text} {}", "y".repeat(1_000))
    }

    /// Whether `stored` holds any part of `secret` (any 5 bytes of it in a
    /// row), in one entry or across two.
    fn leaks(stored: &[String], secret: &str) -> Option<String> {
        let joined = stored.concat();
        secret
            .as_bytes()
            .windows(5)
            .map(|w| std::str::from_utf8(w).unwrap())
            .filter(|w| !w.contains(' ') && !w.contains('"'))
            .find(|w| joined.contains(w))
            .map(str::to_owned)
    }

    /// What the server stores for these requests: each request redacted as
    /// one stream (core's redactor), then split into 64 KiB entries.
    fn stored(sent: &Batches) -> Vec<String> {
        sent.batches
            .iter()
            .flat_map(|batch| {
                runbook_core::claims::split_log_lines(runbook_core::redact::redact_lines(
                    batch.clone(),
                ))
            })
            .collect()
    }

    /// CF12b's final review, item 1, with the real redactor: a line over
    /// one request, cut after whitespace, separated a key from its value
    /// and the server stored the value in clear. Now the client redacts
    /// the line first, so no stored entry holds any part of the secret,
    /// wherever the cut falls.
    #[test]
    fn a_cut_line_is_redacted_before_it_is_cut() {
        for (text, secret) in SPACED_SECRETS {
            // Whole, the redactor finds the secret.
            let whole = runbook_core::redact::redact_lines(vec![straddling(text, 0)]);
            assert_eq!(leaks(&whole, secret), None, "{text}");
            for back in [0, 6] {
                let line = straddling(text, back);
                let sent = batches(vec!["before".into(), line, "after".into()]);
                assert_eq!(sent.cut_lines, 1, "{text}");
                let kept = stored(&sent);
                assert_eq!(leaks(&kept, secret), None, "{text} back={back}");
                assert!(kept.concat().contains("[REDACTED:"), "{text}");
                assert_eq!(kept.first().map(String::as_str), Some("before"));
                assert_eq!(kept.last().map(String::as_str), Some("after"));
                for batch in &sent.batches {
                    assert!(batch.iter().all(|l| json_len(l) <= LINE_JSON_BYTES));
                }
            }
        }
    }

    /// Redacting twice (the client, then the server) stores what one
    /// redaction does: markers are kept as they are.
    #[test]
    fn redacting_twice_is_redacting_once() {
        let lines: Vec<String> = [
            r#"{"password": "hunter2", "token": "t0k", "user": "ok"}"#,
            "password = hunter2 and secret=abc token: xyz",
            r#"password="correct horse battery staple" next"#,
            "Authorization: Bearer abcdef.ghi",
            "authorization: Basic dXNlcjpwYXNz",
            "curl -H 'Authorization: token ghp_abcdefghijklmnopqrstuvwxyz0123456789'",
            "AKIAIOSFODNN7EXAMPLE xoxb-1234-5678-abcdef",
            "-----BEGIN PRIVATE KEY-----",
            "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC",
            "-----END PRIVATE KEY----- tail",
            "rb_pat_0123456789abcdef password=[REDACTED:password]",
            "[REDACTED:bearer] [REDACTED:token] plain text",
            "",
            "   ",
        ]
        .map(str::to_owned)
        .to_vec();
        let once = runbook_core::redact::redact_lines(lines.clone());
        let twice = runbook_core::redact::redact_lines(once.clone());
        assert_eq!(twice, once);
        for (text, secret) in SPACED_SECRETS {
            let once = runbook_core::redact::redact_lines(vec![text.to_owned()]);
            assert_eq!(leaks(&once, secret), None, "{text}: {once:?}");
            assert_eq!(runbook_core::redact::redact_lines(once.clone()), once);
        }
    }

    /// CF12c review round 1, F4: one bad construct in a line over one
    /// request (an unterminated quoted value, an overlong `-----BEGIN`
    /// header, an orphan private-key END line) costs at most about what it
    /// costs the server alone: the rest of that request's window and
    /// [`CARRY_BYTES`] more. Before, the client redacted the whole input as
    /// one stream, and each of these dropped the rest of the input (or, for
    /// the orphan END line, everything before it): up to the step's 5 MiB.
    #[test]
    fn one_bad_construct_in_a_long_line_costs_about_one_request() {
        let filler = |n: usize| "ab ".repeat(n / 3);
        let window = LINE_JSON_BYTES;
        let cases = [
            ("unterminated", format!("password=\"{}", filler(window * 2))),
            (
                "overlong header",
                format!("x -----BEGIN {} {}", "A".repeat(600), filler(window * 2)),
            ),
            (
                "open key",
                format!("x -----BEGIN PRIVATE KEY----- {}", filler(window * 2)),
            ),
        ];
        for (name, line) in cases {
            let lines: Vec<String> = ["first".to_owned(), line.clone()]
                .into_iter()
                .chain((0..50).map(|n| format!("after-{n}")))
                .collect();
            let sent = batches(lines);
            let kept = stored(&sent);
            for n in 0..50 {
                assert!(kept.contains(&format!("after-{n}")), "{name}: after-{n}");
            }
            assert_eq!(kept.first().map(String::as_str), Some("first"), "{name}");
            // Of the long line, at most one window and the carry are lost.
            let survived: usize = kept.iter().map(|e| e.matches("ab").count() * 3).sum();
            let wanted = line.len() - window - CARRY_BYTES - 2 * 600;
            assert!(survived >= wanted, "{name}: {survived} < {wanted}");
        }
        // An orphan END line (a key begun in an earlier request) drops what
        // comes before it in its own request only.
        let lines: Vec<String> = (0..50)
            .map(|n| format!("before-{n}"))
            .chain([format!("{} tail", filler(window + 100_000))])
            .chain([
                "-----END PRIVATE KEY----- done".to_owned(),
                "after".to_owned(),
            ])
            .collect();
        let kept = stored(&batches(lines));
        for n in 0..50 {
            assert!(kept.contains(&format!("before-{n}")), "orphan: before-{n}");
        }
        assert!(kept.contains(&"after".to_owned()));
    }

    /// F1's shapes, cut between a key and its value inside a line over one
    /// request: the redactor's state crosses the window's end.
    #[test]
    fn a_glued_key_cut_from_its_value_is_still_redacted() {
        for (text, secret) in [
            (
                "token=[REDACTED:token]password: hunter2SUPERSECRETVALUE",
                "hunter2SUPERSECRETVALUE",
            ),
            (
                "api token: [REDACTED:token]Authorization: Basic dXNlcjpwYXNzSUPERSECRET",
                "dXNlcjpwYXNzSUPERSECRET",
            ),
            (
                "bearer [REDACTED:bearer]bearer hunter2SUPERSECRETVALUE",
                "hunter2SUPERSECRETVALUE",
            ),
            (
                "token=xpassword: hunter2SUPERSECRETVALUE",
                "hunter2SUPERSECRETVALUE",
            ),
            (
                r#""token": ["one", "hunter2SUPERSECRETVALUE"]"#,
                "hunter2SUPERSECRETVALUE",
            ),
        ] {
            for back in [0, 6] {
                let sent = batches(vec![straddling(text, back), "after".into()]);
                let kept = stored(&sent);
                assert_eq!(leaks(&kept, secret), None, "{text} back={back}");
                assert_eq!(kept.last().map(String::as_str), Some("after"));
            }
        }
    }

    /// F4: a construct the long line opens and the next lines close (a
    /// private key, a quoted value, a JSON list) is read on into them, so
    /// the server never sees their part of it alone.
    #[test]
    fn a_construct_a_long_line_opens_is_read_into_the_next_lines() {
        let head = "ab ".repeat(LINE_JSON_BYTES / 3);
        for (tail, next, secrets) in [
            (
                "-----BEGIN PRIVATE KEY-----",
                &[
                    "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC",
                    "SUPERSECRETKEYBODY",
                    "-----END PRIVATE KEY----- done",
                ][..],
                &["MIIEvQIBADANBgkqhkiG9w0BAQEFAASC", "SUPERSECRETKEYBODY"][..],
            ),
            (
                r#"password="correct horse"#,
                &["battery stapleSUPERSECRET\" done"],
                &["stapleSUPERSECRET"],
            ),
            (
                r#""token": ["#,
                &[
                    r#"  "hunter2SUPERSECRET","#,
                    r#"  "hunter3SUPERSECRET""#,
                    "] done",
                ],
                &["hunter2SUPERSECRET", "hunter3SUPERSECRET"],
            ),
        ] {
            // Padding fills the batch with the long line's tail piece, so
            // the next lines would reach the server in another request.
            let lines: Vec<String> = [format!("{head}{} {tail}", "c".repeat(200_000))]
                .into_iter()
                .chain(next.iter().map(|l| l.to_string()))
                .chain(["after".to_owned()])
                .collect();
            let kept = stored(&batches(lines));
            for secret in secrets {
                assert_eq!(leaks(&kept, secret), None, "{tail}");
            }
            assert!(
                kept.iter().any(|e| e.ends_with("done")),
                "{tail}: {:?}",
                &kept[kept.len().saturating_sub(4)..]
            );
            assert_eq!(kept.last().map(String::as_str), Some("after"), "{tail}");
        }
    }

    const SECRET: &str = "hunter2SUPERSECRETVALUE";

    /// `head`, `fill` repeated, then `tail`, placed so `tail` starts `back`
    /// bytes before the end of the first window plus the carry: where the
    /// client gives up a construct still open (the review's harness).
    fn place(limits: Limits, head: &str, fill: &str, tail: &str, back: isize) -> String {
        let probe = format!("{head}{}", fill.repeat(limits.line_json / fill.len() + 10));
        let end = cut_line(&probe, limits.line_json)[0].len();
        let n = usize::try_from((end + limits.carry) as isize - back - head.len() as isize)
            .unwrap()
            / fill.len();
        format!("{head}{}{tail}", fill.repeat(n))
    }

    /// Small limits, so a sweep over where the carry ends is quick.
    const SMALL: Limits = Limits {
        line_json: 4_096,
        batch_json: 8_192,
        batch_lines: 1_000,
        carry: 512,
    };

    /// Where, of `back` in -8..60, the client and then the server store
    /// part of [`SECRET`] for `line(back)`; the same line redacted as one
    /// stream stores none of it.
    fn sweep(limits: Limits, line: impl Fn(isize) -> String) -> Vec<isize> {
        (-8..60)
            .filter(|&back| {
                let text = line(back);
                let whole = runbook_core::redact::redact_lines(vec![text.clone()]);
                assert_eq!(leaks(&whole, SECRET), None, "one stream, back={back}");
                let sent = batches_within(vec![text, "after".into()], limits);
                assert!(sent.cut_lines > 0);
                let kept = stored(&sent);
                assert_eq!(kept.last().map(String::as_str), Some("after"));
                leaks(&kept, SECRET).is_some()
            })
            .collect()
    }

    /// CF12c review round 2, N1: a line over one request holding a header
    /// line (discarded to its end) whose discard the carry ends. Before,
    /// the cut stopped the discard mid-word and the rest of the word was
    /// stored: with a secret straddling the carry's end, `PERSECRETVALUE
    /// tail` (17 of 68 offsets at the real limits, for each form). A
    /// header named on that line (the last form) was found by the fuzz
    /// test written for this round: the cut ended its discard too, at the
    /// next quote.
    #[test]
    fn a_cut_inside_a_discarded_header_line_discards_the_rest_of_its_word() {
        let forms = [
            "password=",
            "Authorization: Bearer ",
            "bearer ",
            "\\r\\nX-Api-Token: ",
            // A header named on the discarded line: its own line (and
            // Digest parameters) is discarded from there, a carry of its
            // own.
            "Authorization: Digest username=\"u\", response=\"",
        ];
        for form in forms {
            let line = |limits: Limits| {
                move |back: isize| {
                    let head = format!(
                        "{} Authorization: Basic abc ",
                        "w".repeat(limits.line_json - 300)
                    );
                    let tail = format!(" {form}{SECRET} tail");
                    place(limits, &head, "z", &tail, back + form.len() as isize)
                }
            };
            assert_eq!(sweep(SMALL, line(SMALL)), [0; 0], "{form}");
            let real = Limits::default();
            for back in [2, 10, 18] {
                let sent = batches_within(vec![line(real)(back), "after".into()], real);
                assert_eq!(leaks(&stored(&sent), SECRET), None, "{form} back={back}");
            }
        }
    }

    /// CF12c review round 2, N2: the carry counts only the bytes a
    /// construct that may run without bound holds open (a quoted value, a
    /// private key, a header line), not a held prefix, a key waiting for
    /// its value across whitespace or a list between its items. Before, a
    /// long run of a prefix's first letter or of whitespace used up the
    /// carry, and the cut split the key or the token after it from its
    /// value (26 of 68 offsets at the real limits), and a long list lost
    /// its key at the cut and stored every item after it.
    #[test]
    fn the_carry_counts_only_open_constructs() {
        type Line = Box<dyn Fn(Limits, isize) -> String>;
        let cases: Vec<(&str, Line)> = vec![
            (
                "run of x",
                Box::new(|l, back| {
                    let head = "w".repeat(l.line_json - 300);
                    place(l, &head, "x", &format!("password={SECRET} tail"), back)
                }),
            ),
            (
                "run of -",
                Box::new(|l, back| {
                    let head = "w".repeat(l.line_json - 300);
                    place(l, &head, "-", &format!("password={SECRET} tail"), back)
                }),
            ),
            (
                "run of A",
                Box::new(|l, back| {
                    let head = "w".repeat(l.line_json - 300);
                    place(l, &head, "A", &format!("password={SECRET} tail"), back)
                }),
            ),
            (
                "run of - then a token",
                Box::new(|l, back| {
                    let head = "w".repeat(l.line_json - 300);
                    place(l, &head, "-", &format!("ghp_{SECRET} tail"), back)
                }),
            ),
            (
                "spaces between key and value",
                Box::new(|l, back| {
                    let head = format!("{}password=", "w".repeat(l.line_json - 300));
                    place(l, &head, " ", &format!("{SECRET} tail"), back)
                }),
            ),
        ];
        for (name, line) in &cases {
            assert_eq!(sweep(SMALL, |back| line(SMALL, back)), [0; 0], "{name}");
            let real = Limits::default();
            for back in [1, 5, 14] {
                let sent = batches_within(vec![line(real, back), "after".into()], real);
                assert_eq!(leaks(&stored(&sent), SECRET), None, "{name} back={back}");
            }
        }
        // A list of short quoted items running past a window's end and
        // the carry: each item is its own construct.
        for (limits, items) in [(SMALL, 60), (Limits::default(), 1_200)] {
            let mut list = String::from("\"device_token\": [");
            for i in 0..items {
                list.push_str(&format!("\"{i:064x}\", "));
            }
            list.push_str(&format!("\"{SECRET}\"]"));
            let line = format!("{}{list} tail", "w".repeat(limits.line_json - 200));
            let sent = batches_within(vec![line, "after".into()], limits);
            let kept = stored(&sent);
            assert_eq!(leaks(&kept, SECRET), None, "list at {limits:?}");
            assert!(!kept.concat().contains(&format!("{:064x}", items - 1)));
            assert!(kept.concat().ends_with(" tailafter"));
        }
    }

    /// CF12c review round 3: a header line counts its carry from its own
    /// start, whatever opens and closes inside it. Before, a quoted value
    /// or a private key that opened and closed on the line restarted the
    /// count, so a header line followed by 600 KiB of words with a
    /// `"token":"t"` every 8 KiB was discarded to its end.
    #[test]
    fn a_header_line_counts_its_carry_from_its_own_start() {
        for (limits, every, times) in [(SMALL, 200, 60), (Limits::default(), 8 * 1024, 75)] {
            // The header opens 300 bytes before the first window's end.
            let mut line = format!(
                "{} Authorization: Basic abc ",
                "w".repeat(limits.line_json - 300)
            );
            for _ in 0..times {
                line.push_str(&"ab ".repeat(every / 3));
                line.push_str("\"token\":\"t\" ");
            }
            line.push_str("-----BEGIN PRIVATE KEY----- MIIx -----END PRIVATE KEY----- ");
            line.push_str(&"ab ".repeat(every / 3));
            line.push_str("tail");
            let sent = batches_within(vec![line.clone(), "after".into()], limits);
            assert!(sent.cut_lines > 0);
            let kept = stored(&sent);
            assert_eq!(kept.last().map(String::as_str), Some("after"));
            assert!(!kept.concat().contains("MIIx"));
            let survived: usize = kept.iter().map(|e| e.matches("ab").count() * 3).sum();
            let words = line.matches("ab").count() * 3;
            // At most the rest of the window the header opens in, and the
            // carry after it.
            assert!(
                survived + 300 + limits.carry >= words,
                "{limits:?}: {survived} of {words} survived"
            );
        }
    }

    #[test]
    fn batches_hold_at_most_their_bytes_and_lines() {
        let sent = batches((0..2_500).map(|n| format!("{n:04}")));
        assert_eq!(
            sent.batches.iter().map(Vec::len).collect::<Vec<_>>(),
            [1000, 1000, 500]
        );
        let sent = batches(vec!["x".repeat(200 * 1024); 3]);
        assert_eq!(sent.batches.len(), 3);
        for batch in &sent.batches {
            let json = serde_json::to_vec(batch).unwrap().len();
            assert!(json <= BATCH_JSON_BYTES + 2 || batch.len() == 1, "{json}");
        }
    }

    #[test]
    fn report_logs_fit_by_their_json() {
        let line = "x".repeat(1022);
        // 1,024 lines of 1,024 bytes as JSON with commas: 1 MiB + 1.
        let lines = vec![line.clone(); 1024];
        let json = serde_json::to_vec(&lines).unwrap().len();
        assert_eq!(fits_report(&lines), json <= REPORT_LOG_JSON_BYTES, "{json}");
        let fewer = vec![line; 1023];
        assert!(fits_report(&fewer));
        assert!(fits_report(&[]));
    }

    #[test]
    fn sub_keys_always_fit() {
        assert_eq!(sub_key("k1", "-logs-2"), "k1-logs-2");
        let long = "k".repeat(250);
        let key = sub_key(&long, "-logs-12");
        assert!(key.len() <= KEY_BYTES, "{}", key.len());
        assert!(key.starts_with("sha256-") && key.ends_with("-logs-12"));
        assert_eq!(key, sub_key(&long, "-logs-12"), "deterministic");
        assert_ne!(key, sub_key(&"j".repeat(250), "-logs-12"));
        assert_eq!(
            sub_key(&"k".repeat(249), "-logs-1"),
            format!("{}-logs-1", "k".repeat(249))
        );
    }
}
