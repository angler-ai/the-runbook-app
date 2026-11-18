//! CF12c review round 1: seeded property tests of the D34 redactor over
//! generated log text (`support/redact_gen.rs`). A failure names its seed;
//! `Rng::new(seed)` reproduces the case.
//!
//! - No planted secret survives one pass, streamed in any chunks or as
//!   `redact_lines` (F1: the glued-key shapes are among the constructs).
//! - Ordinary text passes through unchanged.
//! - F3: a second pass never reveals anything and never redacts less, and
//!   redacting each side of any cut of the first pass's output separately
//!   reveals nothing (the CLI and MCP redact before cutting a line, then
//!   the server redacts each request).
#[path = "support/redact_gen.rs"]
mod redact_gen;

use redact_gen::{Case, Rng, case, is_subsequence, leaked, lines, without_markers};
use runbook_core::redact::{Redactor, redact_lines};

const SEEDS: u64 = 4_000;

fn stream(text: &str) -> String {
    let mut r = Redactor::default();
    r.push(text) + &r.finish()
}

/// `text` pushed in random chunks, cut at character boundaries.
fn chunked(text: &str, rng: &mut Rng) -> String {
    let mut r = Redactor::default();
    let mut out = String::new();
    let mut rest = text;
    while !rest.is_empty() {
        let mut at = (1 + rng.below(8)).min(rest.len());
        while !rest.is_char_boundary(at) {
            at += 1;
        }
        out.push_str(&r.push(&rest[..at]));
        assert!(r.buffered_bytes() <= 512);
        rest = &rest[at..];
    }
    out + &r.finish()
}

fn show(case: &Case, out: &str) -> String {
    format!("input={:?}\noutput={out:?}", case.text)
}

#[test]
fn no_planted_secret_survives_one_pass_in_any_chunks() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let case = case(&mut rng, 12, 45);
        let once = stream(&case.text);
        assert_eq!(
            leaked(&once, &case.secrets),
            None,
            "seed={seed}\n{}",
            show(&case, &once)
        );
        assert_eq!(chunked(&case.text, &mut rng), once, "seed={seed}");
        let by_lines = redact_lines(lines(&case.text)).join("\n");
        assert_eq!(
            leaked(&by_lines, &case.secrets),
            None,
            "seed={seed}\n{}",
            show(&case, &by_lines)
        );
    }
}

#[test]
fn ordinary_text_is_kept_as_it_is() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let case = case(&mut rng, 16, 0);
        assert_eq!(stream(&case.text), case.text, "seed={seed}");
    }
}

/// F3: what holds of redacting the redactor's output. Not that it changes
/// nothing: a second pass can remove more (an orphan private-key END line
/// drops the lines before it; a marker glued to a key's marker is part of
/// that key's value). It never reveals anything and never redacts less:
/// its text, markers aside, is the first pass's with characters removed.
#[test]
fn a_second_pass_never_reveals_anything_and_never_redacts_less() {
    for seed in 0..SEEDS {
        let mut rng = Rng::new(seed);
        let case = case(&mut rng, 12, 45);
        let once = redact_lines(lines(&case.text)).join("\n");
        let twice = redact_lines(lines(&once)).join("\n");
        assert_eq!(
            leaked(&twice, &case.secrets),
            None,
            "seed={seed}\n{}",
            show(&case, &twice)
        );
        assert!(
            is_subsequence(&without_markers(&twice), &without_markers(&once)),
            "seed={seed}\nonce={once:?}\ntwice={twice:?}"
        );
        // Each side of a cut, redacted on its own (the server redacts
        // each request), reveals nothing either.
        let boundaries: Vec<usize> = once
            .char_indices()
            .map(|(i, _)| i)
            .chain([once.len()])
            .collect();
        for _ in 0..8 {
            let cut = boundaries[rng.below(boundaries.len())];
            let sides =
                [&once[..cut], &once[cut..]].map(|side| redact_lines(lines(side)).join("\n"));
            let joined = sides.concat();
            assert_eq!(
                leaked(&joined, &case.secrets),
                None,
                "seed={seed} cut={cut}\nonce={once:?}\nsides={sides:?}"
            );
            // A cut can fall inside a marker, whose halves are then text.
            assert!(
                is_subsequence(&without_markers(&joined), &once),
                "seed={seed} cut={cut}\nonce={once:?}\nsides={sides:?}"
            );
        }
    }
}

/// The second pass can remove more than the first: the examples F3 gives.
#[test]
fn a_second_pass_can_remove_more() {
    // A private-key END line the first pass keeps (a block ended before
    // it) is an orphan to the second, which drops the lines before it.
    let orphan = "-----BEGIN PRIVATE KEY-----\nU2Vuc2l0aXZl\n-----END PRIVATE KEY-----\nmid\n-----END PRIVATE KEY-----\ntail";
    let once = redact_lines(lines(orphan));
    assert_eq!(
        once,
        [
            "[REDACTED:private_key]",
            "mid",
            "-----END PRIVATE KEY-----",
            "tail"
        ]
    );
    assert_eq!(redact_lines(once), ["[REDACTED:private_key]", "tail"]);
    // A key glued to a marker is part of that marker's value; its own
    // marker, read inside it, passes through.
    let glued = "token=[REDACTED:token]password=hunter2 after";
    assert_eq!(stream(glued), "token=[REDACTED:token] after");
    let glued = "token=[REDACTED:token]password=[REDACTED:password] after";
    assert_eq!(
        stream(glued),
        "token=[REDACTED:token][REDACTED:password] after"
    );
    // Markers glued to each other pass through.
    let markers = "token=[REDACTED:token][REDACTED:private_key] after";
    assert_eq!(stream(markers), markers);
}
