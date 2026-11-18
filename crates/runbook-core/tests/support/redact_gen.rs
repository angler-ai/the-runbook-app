//! CF12c review round 1: a seeded generator of log text for the redactor's
//! property tests (`tests/redact_fuzz.rs`). No dependency: a xorshift64*
//! generator, so a failing seed reproduces anywhere. Plain Rust with no
//! crate imports, so a differential harness outside the workspace can
//! include it with `#[path]` and compare two redactors on the same inputs.
//!
//! A case is fragments joined by separators. Each fragment is either
//! filler (ordinary text the redactor must keep as it is) or a construct
//! holding planted secrets the redactor must remove: `key=value` forms,
//! headers, token prefixes, private-key blocks, quoted and list values,
//! and the F1 shapes (a key, a token prefix or a PEM header glued to a
//! value: `token=[REDACTED:token]password: …`, `token=xpassword: …`).
#![allow(dead_code)]

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
    pub fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

/// Text with the secrets planted in it.
#[derive(Clone, Debug, Default)]
pub struct Case {
    pub text: String,
    pub secrets: Vec<String>,
}

/// A planted secret: 12 characters no filler or marker holds, so finding
/// any 6 of them in a row in the output is a leak.
pub fn secret(rng: &mut Rng) -> String {
    const ALPHABET: &[u8] = b"BCDFGHJKLMNPQSVWXZ23456789";
    (0..12)
        .map(|_| ALPHABET[rng.below(ALPHABET.len())] as char)
        .collect()
}

/// The first planted secret with 6 bytes in a row in `text`.
pub fn leaked<'a>(text: &str, secrets: &'a [String]) -> Option<&'a str> {
    secrets
        .iter()
        .find(|s| {
            s.as_bytes()
                .windows(6)
                .any(|w| text.contains(std::str::from_utf8(w).unwrap()))
        })
        .map(String::as_str)
}

/// Text the redactor keeps exactly as it is.
pub const FILLER: [&str; 34] = [
    "ordinary",
    "deploy",
    "finished",
    "in",
    "42ms",
    "status=200",
    "user=bob",
    "passwords",
    "tokenize",
    "secretary",
    "bearerish",
    "authorized",
    "[REDACTED:token]",
    "[REDACTED:private_key]",
    "[REDACTED:pa",
    "[x]",
    "[]",
    "a]b",
    "\"quoted\"",
    "'single'",
    "{\"user\": \"bob\"}",
    "(paren)",
    "密文",
    "café",
    "a-b_c",
    "x=y&z=w",
    "-----",
    "-----BEGIN CERTIFICATE-----\nMIIBpublic\n-----END CERTIFICATE-----",
    "AKIA-short",
    "ghp",
    "xox",
    "trb_",
    "path/to/file.rs:12",
    "C:\\\\dir",
];

/// What goes between fragments: always a break that starts a new word.
pub const SEPARATORS: [&str; 7] = [" ", "\n", "\t", ", ", "; ", " \n", "\r\n"];

/// A construct whose value the redactor removes wherever it starts; `¤`
/// and `§` stand for planted secrets.
const KEYED: [&str; 18] = [
    "password=¤",
    "password: ¤",
    "password = ¤",
    "DB_PASSWORD=¤",
    "client_secret=¤",
    "api_token: ¤",
    "GITHUB_TOKEN=¤",
    "\"password\": \"¤\"",
    "\"api_token\":\"¤ §\"",
    "token='¤ §'",
    "secret=\"¤\\\"§\"",
    "{\"password\":\"¤\"}",
    "\"token\": [\"¤\", \"§\"]",
    "\"token\": [\n  \"¤\",\n  \"§\"\n]",
    "password=[ ¤",
    "token: [¤]",
    "password=[REDACTED:password]¤",
    "token=[REDACTED:tok¤",
];

/// Constructs that are recognized only at the start of a word.
const WORD_START: [&str; 5] = [
    "Authorization: Bearer ¤",
    "authorization: Basic ¤ §",
    "Bearer ¤",
    "bearer ¤",
    "AUTHORIZATION : Digest username=\"¤\", response=\"§\"",
];

/// Constructs recognized anywhere, even right after a letter.
const ANYWHERE: [&str; 6] = [
    "ghp_¤",
    "xoxb-¤-§",
    "github_pat_¤",
    "-----BEGIN PRIVATE KEY-----\n¤\n§\n-----END PRIVATE KEY-----",
    "-----BEGIN RSA PRIVATE KEY-----\n¤\n-----END RSA PRIVATE KEY----- tail",
    "AKIA",
];

/// F1: a value glued to what follows it, after which the construct must
/// still be found. A marker prefix ends with `]`, so a word-start
/// construct can follow it; a plain value prefix ends with a letter.
const AFTER_MARKER: [&str; 6] = [
    "token=[REDACTED:token]",
    "bearer [REDACTED:bearer]",
    "api token: [REDACTED:token]",
    "password=[REDACTED:password]",
    "secret=[REDACTED:secret]",
    "token=[REDACTED:token][REDACTED:private_key]",
];
const AFTER_WORD: [&str; 5] = [
    "token=x",
    "secret=abc",
    "password=x9",
    "DB_PASSWORD=q",
    "password=[REDACTED:pass",
];

fn plant(template: &str, rng: &mut Rng, secrets: &mut Vec<String>) -> String {
    let mut out = String::new();
    for c in template.chars() {
        if c == '¤' || c == '§' {
            let s = secret(rng);
            out.push_str(&s);
            secrets.push(s);
        } else {
            out.push(c);
        }
    }
    out
}

/// One construct, planted.
fn construct(rng: &mut Rng, secrets: &mut Vec<String>) -> String {
    match rng.below(6) {
        0 | 1 => plant(rng.pick(&KEYED), rng, secrets),
        2 => plant(rng.pick(&WORD_START), rng, secrets),
        3 => {
            let template = rng.pick(&ANYWHERE);
            if template == "AKIA" {
                // An AWS key ID is its own secret: AKIA and 16 more.
                let s = format!("{}{}", secret(rng), &secret(rng)[..4]);
                secrets.push(s.clone());
                format!("AKIA{s}")
            } else {
                plant(template, rng, secrets)
            }
        }
        4 => {
            let prefix = rng.pick(&AFTER_MARKER);
            let inner = match rng.below(3) {
                0 => rng.pick(&KEYED),
                1 => rng.pick(&WORD_START),
                _ => rng.pick(&ANYWHERE),
            };
            if inner == "AKIA" {
                return plant(&format!("{prefix}password: ¤"), rng, secrets);
            }
            plant(&format!("{prefix}{inner}"), rng, secrets)
        }
        _ => {
            let prefix = rng.pick(&AFTER_WORD);
            // After a letter, a word-start construct is not one (as it
            // is not alone after a letter); keys that end a longer name
            // and token prefixes are.
            let inner = match rng.below(2) {
                0 => rng.pick(&[
                    "password=¤",
                    "password: ¤",
                    "_secret=¤",
                    "token: ¤",
                    "\"password\": \"¤\"",
                ]),
                _ => rng.pick(&ANYWHERE[..5]),
            };
            plant(&format!("{prefix}{inner}"), rng, secrets)
        }
    }
}

/// A case of up to `fragments` fragments, `percent` of them constructs.
pub fn case(rng: &mut Rng, fragments: usize, percent: usize) -> Case {
    let mut case = Case::default();
    let n = 1 + rng.below(fragments);
    for i in 0..n {
        if i > 0 {
            case.text.push_str(rng.pick(&SEPARATORS));
        }
        if rng.chance(percent) {
            let text = construct(rng, &mut case.secrets);
            case.text.push_str(&text);
        } else {
            case.text.push_str(rng.pick(&FILLER));
        }
    }
    case
}

/// `text` as `redact_lines` takes it.
pub fn lines(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

/// Strip every marker the redactor writes from `text`.
pub fn without_markers(text: &str) -> String {
    const MARKERS: [&str; 13] = [
        "[REDACTED:runbook_pat]",
        "[REDACTED:runbook_svc]",
        "[REDACTED:runbook_exe]",
        "[REDACTED:runbook_exr]",
        "[REDACTED:aws_access_key]",
        "[REDACTED:github_token]",
        "[REDACTED:slack_token]",
        "[REDACTED:private_key]",
        "[REDACTED:authorization]",
        "[REDACTED:bearer]",
        "[REDACTED:password]",
        "[REDACTED:secret]",
        "[REDACTED:token]",
    ];
    let mut text = text.to_owned();
    for marker in MARKERS {
        text = text.replace(marker, "");
    }
    text
}

/// Whether `small` is `big` with characters removed.
pub fn is_subsequence(small: &str, big: &str) -> bool {
    let mut big = big.chars();
    small.chars().all(|c| big.any(|b| b == c))
}
