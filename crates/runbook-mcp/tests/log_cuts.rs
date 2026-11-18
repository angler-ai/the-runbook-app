//! CF12c review round 1 (F4): a seeded fuzz test of how a client cuts a
//! line too large for one request (`logs::batches`), at small sizes so a
//! few kilobytes cross many windows. Each case is generated log text (core's
//! `tests/support/redact_gen.rs`) as one line, after padding that moves
//! the windows' ends. The server is simulated as it works: each request
//! redacted on its own (core's `redact_lines`). No planted secret may be
//! stored, wherever the windows end, as long as the carry is longer than
//! any one construct.
#[path = "../../runbook-core/tests/support/redact_gen.rs"]
mod redact_gen;

use redact_gen::{Rng, case, leaked};
use runbook_mcp::logs::{Limits, batches_within, json_len};

#[test]
fn no_planted_secret_is_stored_wherever_a_long_line_is_cut() {
    for seed in 0..3_000 {
        let mut rng = Rng::new(seed);
        let case = case(&mut rng, 14, 50);
        let line_json = 40 + rng.below(400);
        let limits = Limits {
            line_json,
            batch_json: line_json + rng.below(line_json),
            batch_lines: 1 + rng.below(4),
            carry: 600 + rng.below(1_000),
        };
        let pad = "ab ".repeat(rng.below(80)) + &"c".repeat(rng.below(40));
        let text = format!("{pad} {}", case.text.replace(['\n', '\r'], " "));
        let sent = batches_within(vec!["before".into(), text.clone(), "after".into()], limits);
        for batch in &sent.batches {
            assert!(
                batch.iter().all(|l| json_len(l) <= line_json),
                "seed={seed}"
            );
        }
        let stored: Vec<String> = sent
            .batches
            .iter()
            .flat_map(|batch| runbook_core::redact::redact_lines(batch.clone()))
            .collect();
        let joined = stored.concat();
        assert_eq!(
            leaked(&joined, &case.secrets),
            None,
            "seed={seed} {limits:?}\ninput={text:?}\nsent={:?}\nstored={stored:?}",
            sent.batches
        );
        assert_eq!(
            stored.first().map(String::as_str),
            Some("before"),
            "seed={seed}"
        );
        assert_eq!(
            stored.last().map(String::as_str),
            Some("after"),
            "seed={seed}"
        );
    }
}

/// One construct (often over several lines: a private key, a JSON list)
/// that begins at the end of a long line: the client reads it on into the
/// lines after, which the server would otherwise see without its start.
#[test]
fn no_planted_secret_is_stored_from_a_construct_a_long_line_opens() {
    for seed in 0..3_000 {
        let mut rng = Rng::new(seed);
        let construct = case(&mut rng, 1, 100);
        let line_json = 40 + rng.below(400);
        let limits = Limits {
            line_json,
            batch_json: line_json,
            batch_lines: 1,
            carry: 600 + rng.below(1_000),
        };
        let mut lines: Vec<String> = construct.text.split('\n').map(str::to_owned).collect();
        lines[0] = format!(
            "{} {}",
            "ab ".repeat(line_json / 3 + rng.below(40)),
            lines[0]
        );
        lines.extend(["plain one".to_owned(), "after".to_owned()]);
        let sent = batches_within(lines.clone(), limits);
        let stored: Vec<String> = sent
            .batches
            .iter()
            .flat_map(|batch| runbook_core::redact::redact_lines(batch.clone()))
            .collect();
        assert_eq!(
            leaked(&stored.concat(), &construct.secrets),
            None,
            "seed={seed} {limits:?}\nlines={lines:?}\nsent={:?}\nstored={stored:?}",
            sent.batches
        );
        assert_eq!(
            stored.last().map(String::as_str),
            Some("after"),
            "seed={seed}"
        );
    }
}

/// CF12c review round 2 (N1, N2): long runs where a window's carry ends:
/// a run of a prefix's first letter, a header line discarded to its end,
/// whitespace between a key and its value, a long list of quoted items.
/// The client then the server may store no planted secret that the same
/// text redacted as one stream does not. Before, the carry ran out on the
/// run and the cut split a key, a token or a value it fell in.
#[test]
fn no_planted_secret_is_stored_after_a_long_run_at_a_cut() {
    for seed in 0..2_000 {
        let mut rng = Rng::new(seed);
        let line_json = 200 + rng.below(400);
        let carry = 100 + rng.below(300);
        let limits = Limits {
            line_json,
            batch_json: line_json * 2,
            batch_lines: 1_000,
            carry,
        };
        let mut text = "ab ".repeat(rng.below(line_json / 3));
        let mut secrets = vec![];
        for _ in 0..3 + rng.below(6) {
            let run = rng.below(carry * 3);
            let s = redact_gen::secret(&mut rng);
            match rng.below(4) {
                // A run of a prefix's first letter glued to a key or a
                // token prefix.
                0 => {
                    let letter = rng.pick(&["x", "-", "A", "t", "p", "g", "\\"]);
                    let key = rng.pick(&["password=", "ghp_", "_token: ", "secret="]);
                    text.push_str(&format!("{}{key}{s} ", letter.repeat(run)));
                    secrets.push(s);
                }
                // A header line, discarded to its end in one stream.
                1 => text.push_str(&format!(" Authorization: Basic {s}{} ", "z".repeat(run))),
                // A key, whitespace, then its value.
                2 => {
                    let key = rng.pick(&[" password=", " \"token\":", " secret ="]);
                    text.push_str(&format!("{key}{}{s} ", " ".repeat(run)));
                    secrets.push(s);
                }
                // A long list of quoted items.
                _ => {
                    text.push_str(" \"device_token\": [");
                    for _ in 0..run / 20 + 1 {
                        let item = redact_gen::secret(&mut rng);
                        text.push_str(&format!("\"{item}\", "));
                        secrets.push(item);
                    }
                    text.push_str(&format!("\"{s}\"] "));
                    secrets.push(s);
                }
            }
            // A generated construct, apart from the run.
            let construct = case(&mut rng, 1, 100);
            text.push(' ');
            text.push_str(&construct.text.replace(['\n', '\r'], " "));
            text.push(' ');
            secrets.extend(construct.secrets);
        }
        let whole = runbook_core::redact::redact_lines(vec![text.clone()]).concat();
        let sent = batches_within(vec![text.clone(), "after".into()], limits);
        let stored: Vec<String> = sent
            .batches
            .iter()
            .flat_map(|batch| runbook_core::redact::redact_lines(batch.clone()))
            .collect();
        let joined = stored.concat();
        let leak = secrets.iter().find(|s| {
            leaked(&joined, std::slice::from_ref(s)).is_some()
                && leaked(&whole, std::slice::from_ref(s)).is_none()
        });
        assert_eq!(
            leak, None,
            "seed={seed} {limits:?}\ninput={text:?}\nstored={stored:?}"
        );
        assert_eq!(
            stored.last().map(String::as_str),
            Some("after"),
            "seed={seed}"
        );
    }
}
