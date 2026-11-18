use runbook_core::redact::{Redactor, validate_evidence_url};
fn cases() -> Vec<(String, String)> {
    let mut cases = vec![];
    for kind in ["pat", "svc", "exe", "exr"] {
        cases.push((
            format!("before trb_{kind}_{} after", "A".repeat(43)),
            format!("before [REDACTED:runbook_{kind}] after"),
        ));
    }
    for prefix in ["AKIA", "ASIA"] {
        cases.push((
            format!("{prefix}{}", "A".repeat(16)),
            "[REDACTED:aws_access_key]".into(),
        ));
    }
    for prefix in ["ghp_", "gho_", "ghs_", "github_pat_"] {
        cases.push((
            format!("{prefix}Sensitive123"),
            "[REDACTED:github_token]".into(),
        ));
    }
    for prefix in ["xoxa-", "xoxb-", "xoxp-", "xoxo-", "xoxs-", "xoxr-"] {
        cases.push((
            format!("{prefix}Sensitive-123"),
            "[REDACTED:slack_token]".into(),
        ));
    }
    for label in [
        "PRIVATE KEY",
        "RSA PRIVATE KEY",
        "EC PRIVATE KEY",
        "OPENSSH PRIVATE KEY",
        "ENCRYPTED PRIVATE KEY",
    ] {
        cases.push((
            format!("before -----BEGIN {label}-----\nU2Vuc2l0aXZl\n-----END {label}----- after"),
            "before [REDACTED:private_key] after".into(),
        ));
    }
    cases.extend(
        [
            (
                "Authorization: Bearer sensitive\nafter",
                "Authorization: [REDACTED:authorization]\nafter",
            ),
            (
                "AUTHORIZATION : Basic sensitive\r\nafter",
                "AUTHORIZATION : [REDACTED:authorization]\r\nafter",
            ),
            (
                "Authorization: Digest username=\"alice\", nonce=\"abc\", response=\"secret-digest\"\nafter",
                "Authorization: [REDACTED:authorization]\nafter",
            ),
            ("Bearer sensitive after", "Bearer [REDACTED:bearer] after"),
            (
                "password=sensitive after",
                "password=[REDACTED:password] after",
            ),
            (
                "secret=\"sensitive with spaces\" after",
                "secret=\"[REDACTED:secret]\" after",
            ),
            (
                "token='sensitive with spaces' after",
                "token='[REDACTED:token]' after",
            ),
            (
                "secret=\"sensitive\\\"still secret\" after",
                "secret=\"[REDACTED:secret]\" after",
            ),
            // F7: key names that end in a keyword, `:` separators, quoted
            // (JSON) keys and every `xox?-` Slack prefix (D34).
            (
                "DB_PASSWORD=sensitive after",
                "DB_PASSWORD=[REDACTED:password] after",
            ),
            (
                "GITHUB_TOKEN=sensitive after",
                "GITHUB_TOKEN=[REDACTED:token] after",
            ),
            (
                "client_secret=sensitive after",
                "client_secret=[REDACTED:secret] after",
            ),
            (
                "{\"password\":\"sensitive\"} after",
                "{\"password\":\"[REDACTED:password]\"} after",
            ),
            (
                "{\"api_token\": \"sensitive\", \"user\": \"bob\"}",
                "{\"api_token\": \"[REDACTED:token]\", \"user\": \"bob\"}",
            ),
            (
                "password: sensitive after",
                "password: [REDACTED:password] after",
            ),
            ("xoxc-Sensitive-123 after", "[REDACTED:slack_token] after"),
            ("xoxe-Sensitive-123", "[REDACTED:slack_token]"),
        ]
        .map(|(a, b)| (a.into(), b.into())),
    );
    cases
}
#[test]
fn logs_and_links_are_safe_and_scoped() {
    for (input, expected) in cases() {
        for split in 0..=input.len() {
            let mut redactor = Redactor::default();
            let mut actual = redactor.push(&input[..split]);
            assert!(redactor.buffered_bytes() <= 512);
            actual.push_str(&redactor.push(&input[split..]));
            assert!(redactor.buffered_bytes() <= 512);
            actual.push_str(&redactor.finish());
            assert_eq!(actual, expected, "split={split} input={input:?}");
        }
    }
    for good in [
        "https://example.com/evidence?q=1#part",
        "HTTPS://example.com",
        "https://[::1]/image",
        "https://example.com/中文",
    ] {
        assert!(validate_evidence_url(good, &[]).is_ok(), "{good}");
    }
    for bad in [
        "http://example.com",
        "//example.com",
        "/relative",
        "https:",
        "https:///missing-host",
        "https://user:password@example.com",
        "https://exam\nple.com",
        "javascript:alert(1)",
        "data:text/html,test",
    ] {
        assert!(validate_evidence_url(bad, &[]).is_err(), "{bad}");
    }
    assert!(validate_evidence_url("http://localhost/report", &["http"]).is_ok());
    assert!(validate_evidence_url("s3://bucket/evidence", &["s3"]).is_ok());
    assert!(validate_evidence_url("http://example.com", &["s3"]).is_err());
}
#[test]
fn redaction_bounds_retained_text_for_unbounded_values() {
    for input in [
        format!("secret={} done", "密".repeat(100_000)),
        format!("ghp_{} done", "a".repeat(100_000)),
        format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY----- done",
            "a".repeat(100_000)
        ),
    ] {
        let mut r = Redactor::default();
        let mut out = String::new();
        for c in input.chars() {
            out.push_str(&r.push(c.encode_utf8(&mut [0; 4])));
            assert!(r.buffered_bytes() <= 512);
        }
        out.push_str(&r.finish());
        assert!(out.contains("[REDACTED:"));
        assert!(out.ends_with(" done"));
        assert!(out.len() < 100);
    }
    let mut r = Redactor::default();
    let output = r.push(&format!(
        "-----BEGIN {} PRIVATE KEY-----\nsecret",
        "A".repeat(600)
    )) + &r.finish();
    assert!(output.contains("[REDACTED:private_key]"));
    assert!(!output.contains("secret"));
}
#[test]
fn ordinary_text_and_unicode_survive_all_chunk_boundaries() {
    let text = "ordinary 密文 text; passwords are not password=, Bearerish words; AKIA-short; certificate:\n-----BEGIN CERTIFICATE-----\npublic\n-----END CERTIFICATE-----\n";
    for split in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
        let mut r = Redactor::default();
        let got = r.push(&text[..split]) + &r.push(&text[split..]) + &r.finish();
        assert_eq!(got, text);
        assert_eq!(r.finish(), "");
    }
}

#[test]
fn unfinished_secrets_and_debug_never_release_retained_values() {
    let mut r = Redactor::default();
    assert_eq!(r.push("trb_pat_sensitive"), "");
    assert!(!format!("{r:?}").contains("sensitive"));
    assert_eq!(r.finish(), "[REDACTED:runbook_pat]");
    assert_eq!(r.push("ordinary!") + &r.finish(), "ordinary!");
    for (input, expected) in cases() {
        let mut r = Redactor::default();
        let mut actual = String::new();
        for ch in input.chars() {
            actual.push_str(&r.push(ch.encode_utf8(&mut [0; 4])));
        }
        actual.push_str(&r.finish());
        assert_eq!(actual, expected);
    }
}

#[test]
fn evidence_url_deny_list_overrides_operator_schemes() {
    use runbook_core::redact::EvidenceUrlError::SchemeNotAllowed;
    for (scheme, url) in [
        ("javascript", "javascript:alert(1)"),
        ("javascript", "\tJaVaScRiPt:alert(1)"),
        ("data", "DATA:text/html,test"),
        ("vbscript", "VbScRiPt:msgbox(1)"),
        ("file", "file:///etc/passwd"),
        ("blob", "blob:https://example.com/id"),
    ] {
        assert_eq!(
            validate_evidence_url(url, &[scheme]),
            Err(SchemeNotAllowed),
            "{url:?}"
        );
    }
}
#[test]
fn evidence_url_scheme_comparison_trims_and_ignores_case() {
    assert!(validate_evidence_url(" \tHTTPS://example.com/report\r\n", &[]).is_ok());
    assert!(
        validate_evidence_url("\u{0}HtTp://example.com/report\u{7f}", &[" \tHTTP\r\n"]).is_ok()
    );
    assert!(validate_evidence_url("S3://bucket/report", &["\u{0}S3\u{7f}"]).is_ok());
    assert!(validate_evidence_url("http://example.com", &[]).is_err());
    assert!(validate_evidence_url("ht\ttps://example.com", &["ht\ttps"]).is_err());
}
#[test]
fn evidence_url_rejects_credentials_for_allowed_schemes() {
    use runbook_core::redact::EvidenceUrlError::CredentialsNotAllowed;
    for url in [
        "https://user:pass@example.com",
        "\tHTTP://user:pass@example.com\r\n",
        "s3://user@bucket/key",
    ] {
        assert_eq!(
            validate_evidence_url(url, &["http", "s3"]),
            Err(CredentialsNotAllowed)
        );
    }
}

// F24: D38 keeps no redactor state between appends, yet D34 promises PEM
// blocks are redacted across append boundaries. A block cut by an append
// ends the first request redacted; the second request's orphan body up to
// its END line is recognised without carried state.
#[test]
fn pem_block_split_across_appends_is_redacted() {
    use runbook_core::redact::redact_lines;
    let first = redact_lines(vec![
        "before".into(),
        "-----BEGIN RSA PRIVATE KEY-----".into(),
        "U2Vuc2l0aXZlMQ==".into(),
    ]);
    assert_eq!(first, ["before", "[REDACTED:private_key]"]);
    let second = redact_lines(vec![
        "U2Vuc2l0aXZlMg==".into(),
        "-----END RSA PRIVATE KEY----- done".into(),
        "after".into(),
    ]);
    assert_eq!(second, ["[REDACTED:private_key] done", "after"]);
    // A certificate END line is public and untouched.
    let public = vec!["MIIB".to_string(), "-----END CERTIFICATE-----".to_string()];
    assert_eq!(redact_lines(public.clone()), public);
}

/// One stream through a fresh redactor, cut at `split`.
fn redact_at(input: &str, split: usize) -> String {
    let mut r = Redactor::default();
    let out = r.push(&input[..split]) + &r.push(&input[split..]) + &r.finish();
    assert!(r.buffered_bytes() <= 512);
    out
}

/// CF12c: the CLI and MCP redact a line too large for one request before
/// cutting it, and the server redacts it again, so redacting the
/// redactor's own output must change nothing. Before, an unquoted value
/// that was already a marker lost its end to the value's word boundary
/// and gained a `]` each time: `password = [REDACTED:password]` became
/// `password = [REDACTED:password]]`.
#[test]
fn redacting_its_own_output_changes_nothing() {
    let mut inputs: Vec<String> = cases().into_iter().map(|(_, expected)| expected).collect();
    inputs.extend(
        [
            "password = [REDACTED:password] and secret=[REDACTED:secret], token: [REDACTED:token]",
            "Bearer [REDACTED:bearer] after",
            "bearer [REDACTED:bearer]",
            "password=[REDACTED:password]",
            "DB_PASSWORD=[REDACTED:password]\nnext",
            "token=[REDACTED:github_token] (another kind's marker is a marker too)",
            "{password=[REDACTED:password]}",
            "Authorization: [REDACTED:authorization]",
            "[REDACTED:bearer] [REDACTED:token] [REDACTED:private_key] tail",
        ]
        .map(str::to_owned),
    );
    for input in inputs {
        for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
            assert_eq!(redact_at(&input, split), input, "split={split}");
        }
    }
}

/// CF12c: only a whole marker that ends the value passes through; anything
/// else after a key is still the secret it was.
#[test]
fn a_value_that_only_looks_like_a_marker_is_still_redacted() {
    for (input, expected) in [
        (
            "password=[REDACTED:password]hunter2 after",
            "password=[REDACTED:password] after",
        ),
        (
            "password=[REDACTED:pass after",
            "password=[REDACTED:password] after",
        ),
        (
            "password=[REDACTED:nokind] after",
            "password=[REDACTED:password]] after",
        ),
        // CF12c review round 1: `[` opens a list whose items are the
        // values.
        (
            "password=[hunter2] after",
            "password=[[REDACTED:password]] after",
        ),
        ("password=[] after", "password=[] after"),
        ("password=[REDAC", "password=[REDACTED:password]"),
        ("token: [", "token: [REDACTED:token]"),
    ] {
        for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
            assert_eq!(redact_at(input, split), expected, "split={split} {input}");
        }
    }
}

/// CF12c review round 1, F1: the value after a key is scanned for further
/// keys, token prefixes and PEM headers even while it is being discarded.
/// Before, a whole marker followed by more of the word (and any value word
/// before CF12c: `token=xpassword: hunter2`) discarded the rest of the
/// word unread, so a key or a PEM header glued to it was lost and its value
/// stored in clear. Also a bracketed value (`"token": ["hunter2"]`, and
/// `password=[ hunter2`), whose `[` ended the value before its first item.
#[test]
fn a_key_inside_a_discarded_value_still_redacts_its_own_value() {
    for (input, secrets) in [
        (
            "token=[REDACTED:token]-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----",
            &["MIIEvQIBADANBgkqhkiG9w0BAQEFAASC"][..],
        ),
        ("token=[REDACTED:token]password: hunter2", &["hunter2"]),
        (
            "api token: [REDACTED:token]Authorization: Basic dXNlcjpwYXNz",
            &["dXNlcjpwYXNz"],
        ),
        ("bearer [REDACTED:bearer]bearer hunter2", &["hunter2"]),
        ("password=[REDACTED:password]hunter2 after", &["hunter2"]),
        ("token=xpassword: hunter2", &["hunter2"]),
        ("password=x,bearer hunter2", &["hunter2"]),
        ("password= bearer hunter2", &["hunter2"]),
        ("password=[REDACTED:pass-secret: hunter2", &["hunter2"]),
        ("token=abc\"password\": \"hunter2\"", &["hunter2"]),
        (
            "token=x-----BEGIN PRIVATE KEY-----\nU2Vuc2l0aXZl\n-----END PRIVATE KEY-----",
            &["U2Vuc2l0aXZl"],
        ),
        ("\"token\": [\"hunter2\"]", &["hunter2"]),
        (
            "\"token\": [\"hunter2\", \"hunter3\"]",
            &["hunter2", "hunter3"],
        ),
        (
            "\"token\": [\n  \"hunter2\",\n  \"hunter3\"\n]",
            &["hunter2", "hunter3"],
        ),
        ("password=[ hunter2", &["hunter2"]),
        ("token=[ghp_hunter2]", &["hunter2"]),
    ] {
        for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
            let out = redact_at(input, split);
            for secret in secrets {
                assert!(!out.contains(secret), "split={split} {input:?} -> {out:?}");
            }
            let lines =
                runbook_core::redact::redact_lines(input.split('\n').map(str::to_owned).collect());
            for secret in secrets {
                assert!(!lines.concat().contains(secret), "{input:?} -> {lines:?}");
            }
        }
    }
    // What the scanned value keeps is only what it would have kept alone:
    // the glued key's own marker.
    for (input, expected) in [
        (
            "token=[REDACTED:token]password: hunter2 after",
            "token=[REDACTED:token] [REDACTED:password] after",
        ),
        (
            "token=xpassword: hunter2 after",
            "token=[REDACTED:token] [REDACTED:password] after",
        ),
        (
            "\"token\": [\"hunter2\", \"hunter3\"], \"user\": \"bob\"",
            "\"token\": [\"[REDACTED:token]\", \"[REDACTED:token]\"], \"user\": \"bob\"",
        ),
        (
            "password=[ hunter2 after",
            "password=[ [REDACTED:password] after",
        ),
        // A value word with no key in it is discarded as before.
        ("token=hunter2 after", "token=[REDACTED:token] after"),
        (
            "token=a_tokenized_value after",
            "token=[REDACTED:token] after",
        ),
    ] {
        for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
            assert_eq!(redact_at(input, split), expected, "split={split} {input:?}");
        }
    }
}

/// CF12c review round 1 (found by the fuzz test): a header's value runs to
/// the end of its line and was discarded unread, so a private key whose
/// BEGIN line sat on that line had its body stored. It is read now, as a
/// discarded word is (F1).
#[test]
fn a_private_key_inside_a_header_value_is_redacted() {
    let input = "Authorization: Bearer abc; x=-----BEGIN PRIVATE KEY-----\nU2Vuc2l0aXZl\n-----END PRIVATE KEY----- after\nnext";
    for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
        assert_eq!(
            redact_at(input, split),
            "Authorization: [REDACTED:authorization] after\nnext",
            "split={split}"
        );
    }
    // A Digest header's quoted parameters stay part of its line.
    let digest = "Authorization: Digest username=\"a b\", response=\"x\"\nnext";
    assert_eq!(
        redact_at(digest, 0),
        "Authorization: [REDACTED:authorization]\nnext"
    );
}

/// CF12c review round 1 (F4): `finish_closed` closes a quoted value left
/// open, so a second pass over the output and more text does not read that
/// text as the value.
#[test]
fn finish_closed_closes_an_open_quoted_value() {
    let mut r = Redactor::default();
    assert_eq!(
        r.push("password=\"abc def") + &r.finish_closed(),
        "password=\"[REDACTED:password]\""
    );
    assert_eq!(
        r.push("password='abc") + &r.finish_closed(),
        "password='[REDACTED:password]'"
    );
    assert_eq!(r.push("plain") + &r.finish_closed(), "plain");
    // A quote inside a discarded header line was never written.
    assert_eq!(
        r.push("Authorization: Basic token=\"abc") + &r.finish_closed(),
        "Authorization: [REDACTED:authorization]"
    );
    let mut second = Redactor::default();
    let closed = "password=\"[REDACTED:password]\" after";
    assert_eq!(second.push(closed) + &second.finish(), closed);
}

/// CF12c review round 1 (F4): `cut_short` ends a discarded value but not a
/// key read inside it, which still redacts its value.
#[test]
fn cut_short_ends_a_discarded_value_but_not_a_key_inside_it() {
    let mut r = Redactor::default();
    let mut out = r.push("Authorization: Basic abc password=");
    out += &r.cut_short();
    out += &r.push(" hunter2 after");
    out += &r.finish();
    assert_eq!(
        out,
        "Authorization: [REDACTED:authorization] [REDACTED:password] after"
    );
    // CF12c review round 2 (N1): the word the cut falls in is still
    // discarded to its end, its value with it.
    let mut r = Redactor::default();
    let out = r.push("Authorization: Basic abc password=")
        + &r.cut_short()
        + &r.push("hunter2 after")
        + &r.finish();
    assert_eq!(out, "Authorization: [REDACTED:authorization] after");
    let mut r = Redactor::default();
    let out = r.push("password=\"abc") + &r.cut_short() + &r.push(" after") + &r.finish();
    assert_eq!(out, "password=\"[REDACTED:password]\" after");
}

/// CF12c review round 2 (N1, N2): a cut (`cut_short`, where a client bounds
/// how far one construct may run past a request's end) never ends a
/// discard or a value in the middle of a word, and keeps what is still
/// being read: a key waiting for its value, a list, a held prefix. Before,
/// a cut inside a discarded header line wrote the rest of the word it fell
/// in (`PERSECRETVALUE tail`), and a cut anywhere else ended the stream
/// (`finish`), releasing held prefixes and forgetting a pending key.
#[test]
fn a_cut_never_ends_a_secret_mid_word_and_keeps_what_is_being_read() {
    let header = "Authorization: Basic abc zzzz";
    let cases: &[(String, &str)] = &[
        // N1: the cut falls inside a value inside a discarded header line.
        (
            format!("{header} password=hunter2SUPER"),
            "SECRETVALUE tail",
        ),
        (
            format!("{header} Authorization: Bearer hunter2SUPER"),
            "SECRETVALUE tail",
        ),
        (format!("{header} bearer hunter2SUPER"), "SECRETVALUE tail"),
        (
            format!("{header} \\r\\nX-Api-Token: hunter2SUPER"),
            "SECRETVALUE tail",
        ),
        // The header's own value word.
        (
            "Authorization: Basic hunter2SUPER".into(),
            "SECRETVALUE tail",
        ),
        // A quoted value and a private key's body.
        ("password=\"hunter2SUPER".into(), "SECRETVALUE more\" tail"),
        (
            "-----BEGIN PRIVATE KEY-----\nMIIhunter2SUPER".into(),
            "SECRETVALUE\n-----END PRIVATE KEY----- tail",
        ),
        // N2: a key waiting for its value, a list, a held prefix.
        ("password=".into(), "SUPERSECRETVALUE tail"),
        ("password=   ".into(), "SUPERSECRETVALUE tail"),
        ("\"token\": [\"a\", ".into(), "\"SUPERSECRETVALUE\"] tail"),
        ("\"token\": [\"a\"".into(), ", \"SUPERSECRETVALUE\"] tail"),
        ("pass".into(), "word=SUPERSECRETVALUE tail"),
        ("ghp".into(), "_SUPERSECRETVALUE tail"),
        ("xxxx".into(), "password=SUPERSECRETVALUE tail"),
        ("----".into(), "ghp_SUPERSECRETVALUE tail"),
    ];
    for (before, after) in cases {
        let mut r = Redactor::default();
        let out = r.push(before) + &r.cut_short() + &r.push(after) + &r.finish();
        assert!(
            !out.contains("SECRETVALUE"),
            "{before:?} | {after:?} -> {out:?}"
        );
        assert!(out.ends_with(" tail"), "{before:?} | {after:?} -> {out:?}");
    }
}

/// CF12c review round 3: a quote opened inside a discarded header line
/// runs past the line's end to its closing quote, as it would on any other
/// line, and gives up only at the first line break after 8 KiB inside it.
/// Round 2 (N3) ended it with the line, which stored a private key held in
/// a quoted value on a header line.
#[test]
fn a_quote_on_a_discarded_header_line_runs_to_its_closing_quote() {
    let curl = "+ curl -H Authorization: Bearer abc123 --data-urlencode client_secret=\"-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----\n\" https://example.test";
    let basic = "Authorization: Basic x API_SECRET=\"line1\nhunter2SUPERSECRET\" after";
    for (input, expected, secrets) in [
        (
            curl,
            "+ curl -H Authorization: [REDACTED:authorization] https://example.test",
            &[
                "abc123",
                "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC",
                "END PRIVATE KEY",
            ][..],
        ),
        (
            basic,
            "Authorization: [REDACTED:authorization] after",
            &["line1", "hunter2SUPERSECRET"],
        ),
    ] {
        for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
            let out = redact_at(input, split);
            assert_eq!(out, expected, "split={split}");
            for secret in secrets {
                assert!(!out.contains(secret), "split={split} -> {out:?}");
            }
        }
        let lines =
            runbook_core::redact::redact_lines(input.split('\n').map(str::to_owned).collect());
        assert_eq!(lines, [expected]);
    }
    // Closed on its line, it is still part of the line's value.
    let closed = "Authorization: Digest response=\"a b\" token=x\nnext";
    assert_eq!(
        redact_at(closed, 0),
        "Authorization: [REDACTED:authorization]\nnext"
    );
}

/// CF12c review round 3: the round 2 (N3) case. A quote a header line opens
/// and never closes swallows at most 8 KiB and the rest of the line it has
/// reached then, not the rest of the input.
#[test]
fn an_unclosed_quote_on_a_header_line_is_bounded() {
    let head = "Authorization: Digest username=\"u\", secret=\"abc\nnext line stays?\n";
    let short = format!("{head}third");
    assert_eq!(
        redact_at(&short, 0),
        "Authorization: [REDACTED:authorization]"
    );
    let filler: String = (0..120)
        .map(|i| format!("filler line {i:04} {}\n", "f".repeat(60)))
        .collect();
    let input = format!("{head}{filler}kept line\nthird");
    let open = input.find("secret=\"").unwrap() + "secret=\"".len();
    // The first line break with at least 8 KiB of the quote before it.
    let give_up = input[open..]
        .char_indices()
        .find(|(i, c)| *i >= 8192 && matches!(c, '\n' | '\r'))
        .map(|(i, _)| open + i)
        .unwrap();
    assert!(give_up < input.find("kept line").unwrap());
    let expected = format!(
        "Authorization: [REDACTED:authorization]{}",
        &input[give_up..]
    );
    for split in [
        0,
        open,
        open + 4000,
        give_up - 1,
        give_up,
        give_up + 1,
        input.len(),
    ] {
        assert_eq!(redact_at(&input, split), expected, "split={split}");
    }
    assert!(expected.ends_with("\nkept line\nthird"));
    // CRLF line breaks end it the same way.
    let crlf = input.replace('\n', "\r\n");
    let out = redact_at(&crlf, 0);
    assert!(out.ends_with("\r\nkept line\r\nthird"), "{out:?}");
    assert!(out.len() < crlf.len() - 8192);
}

/// CF12c review round 2 (N4): a quoted item followed by `:` or `=` is a key
/// inside the list, and what follows it is a value.
#[test]
fn a_quoted_key_inside_a_list_takes_its_value() {
    for (input, secrets) in [
        ("token=[\"access_token\": \"hunter2\"]", &["hunter2"][..]),
        (
            "\"token\": [\"k\": \"hunter2\", \"k2\" = \"hunter3\"] after",
            &["hunter2", "hunter3"],
        ),
        ("token=[\"k\":hunter2 ]", &["hunter2"]),
    ] {
        for split in input.char_indices().map(|(i, _)| i).chain([input.len()]) {
            let out = redact_at(input, split);
            for secret in secrets {
                assert!(!out.contains(secret), "split={split} {input:?} -> {out:?}");
            }
        }
    }
    assert_eq!(
        redact_at("token=[\"k\": \"hunter2\"] after", 0),
        "token=[\"[REDACTED:token]\": \"[REDACTED:token]\"] after"
    );
}

#[test]
fn signed_url_query_signatures_are_redacted() {
    let mut r = Redactor::default();
    let mut out = r.push(
        "PUT https://content.example.test/u/exe_1/evd_1?org=o&prj=p&size=10&type=image/png&expires=99&sig=deadbeefcafe HTTP/1.1\n",
    );
    out.push_str(&r.finish());
    assert!(out.contains("sig=[REDACTED:signature]"), "{out}");
    assert!(!out.contains("deadbeefcafe"), "{out}");
    // The rest of the URL stays readable.
    assert!(out.contains("type=image/png"), "{out}");
}
