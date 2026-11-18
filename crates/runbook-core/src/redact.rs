//! D34 streaming sanitization. Ordinary text and key names are preserved;
//! sensitive values become stable `[REDACTED:<kind>]` markers. Recognized
//! unbounded values are discarded incrementally, never buffered in full.
use std::fmt;
const MAX_TAIL: usize = 512;
/// CF12c review round 3: how far a quote opened inside a discarded header
/// line may run before it gives up at a line break.
const HEADER_QUOTE_BYTES: usize = 8 * 1024;
#[derive(Clone, Copy)]
enum Kind {
    Pat,
    Svc,
    Exe,
    Exr,
    Aws,
    Github,
    Slack,
    Pem,
    Authorization,
    Bearer,
    Password,
    Secret,
    Token,
    Signature,
}
impl Kind {
    const ALL: [Kind; 14] = [
        Self::Pat,
        Self::Svc,
        Self::Exe,
        Self::Exr,
        Self::Aws,
        Self::Github,
        Self::Slack,
        Self::Pem,
        Self::Authorization,
        Self::Bearer,
        Self::Password,
        Self::Secret,
        Self::Token,
        Self::Signature,
    ];
    fn marker(self) -> &'static str {
        match self {
            Self::Pat => "[REDACTED:runbook_pat]",
            Self::Svc => "[REDACTED:runbook_svc]",
            Self::Exe => "[REDACTED:runbook_exe]",
            Self::Exr => "[REDACTED:runbook_exr]",
            Self::Aws => "[REDACTED:aws_access_key]",
            Self::Github => "[REDACTED:github_token]",
            Self::Slack => "[REDACTED:slack_token]",
            Self::Pem => "[REDACTED:private_key]",
            Self::Authorization => "[REDACTED:authorization]",
            Self::Bearer => "[REDACTED:bearer]",
            Self::Password => "[REDACTED:password]",
            Self::Secret => "[REDACTED:secret]",
            Self::Token => "[REDACTED:token]",
            Self::Signature => "[REDACTED:signature]",
        }
    }
}
#[derive(Clone, Copy)]
enum Separator {
    /// `key=value` or `key: value`; a quoted key (`"password":`) may close
    /// its quote first.
    Equals {
        quote_allowed: bool,
    },
    Header {
        quote_allowed: bool,
    },
    Space,
}
#[derive(Default)]
enum Mode {
    #[default]
    Normal,
    Separator {
        kind: Kind,
        separator: Separator,
    },
    AwaitValue {
        kind: Kind,
        line: bool,
        /// CF12c review round 1: inside a `[` list that is the value
        /// (`"token": ["…", "…"]`): whitespace and `,` separate items.
        array: bool,
    },
    Token,
    /// CF12c: an unquoted value that so far reads as a marker this
    /// redactor writes (`[REDACTED:password]`), held until it is whole and
    /// its value ends, so redacting the redactor's own output changes
    /// nothing. At most one marker's bytes.
    Marker {
        kind: Kind,
        seen: String,
    },
    Quoted {
        quote: char,
        escaped: bool,
        /// The key of the list this quoted item belongs to, if any.
        array: Option<Kind>,
        /// CF12c review round 2 (N3): opened inside a discarded header
        /// line, so its opening quote was never written, and its closing
        /// quote is not written either.
        ///
        /// CF12c review round 3: it runs past that line's end to its
        /// closing quote, as any quoted value does (round 2 ended it with
        /// the line, which stored a private key held in a quoted value on a
        /// header line), and gives up only at the first line break after
        /// [`HEADER_QUOTE_BYTES`] inside it.
        in_header: bool,
        /// Bytes read inside it, when `in_header`.
        header_bytes: usize,
    },
    /// After a quoted list item: `,` starts the next item; `:` or `=`
    /// makes the item a key (`["access_token": "…"]`) whose value follows.
    ArrayNext {
        kind: Kind,
    },
    Pem(EndMarker),
    // An invalid, overlong PEM header cannot be safely distinguished with a
    // bounded tail. Conservatively redact the remainder of this stream.
    OversizedPem,
}
struct EndMarker {
    bytes: Vec<u8>,
    fallback: Vec<usize>,
    matched: usize,
}
impl EndMarker {
    fn new(value: String) -> Self {
        let bytes = value.into_bytes();
        let mut fallback = vec![0; bytes.len()];
        let mut matched = 0;
        for i in 1..bytes.len() {
            while matched > 0 && bytes[i] != bytes[matched] {
                matched = fallback[matched - 1];
            }
            if bytes[i] == bytes[matched] {
                matched += 1;
            }
            fallback[i] = matched;
        }
        Self {
            bytes,
            fallback,
            matched: 0,
        }
    }
    fn push(&mut self, c: char) -> bool {
        if !c.is_ascii() {
            self.matched = 0;
            return false;
        }
        let b = c as u8;
        while self.matched > 0 && b != self.bytes[self.matched] {
            self.matched = self.fallback[self.matched - 1];
        }
        if b == self.bytes[self.matched] {
            self.matched += 1;
        }
        self.matched == self.bytes.len()
    }
}
/// Holds at most 512 bytes of unclassified input. Never Debug-print that tail.
/// How far a discarded value runs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Hide {
    /// To the end of its word (`value_end`).
    Word,
    /// To the end of its line: a header's value, whose quoted parameters
    /// (Digest, for example) are still part of it.
    Line,
}
#[derive(Default)]
pub struct Redactor {
    pending: String,
    mode: Mode,
    previous_word: bool,
    /// CF12c review round 1 (F1): the rest of an unquoted value is being
    /// discarded, to the end of its word or, for a header, of its line.
    /// Its characters still go through the state machine, so a key, a
    /// token prefix or a PEM header glued to the value
    /// (`token=[REDACTED:token]password: hunter2`) still redacts what
    /// follows it; nothing is written until the value ends.
    hidden: Option<Hide>,
    /// The leading bytes of `pending` that the discarded word held: never
    /// written, though they may still begin a match (a PEM header's
    /// `-----BEGIN` before its space).
    hidden_pending: usize,
    /// CF12c review round 3: how many discarded header lines, and how many
    /// quoted values and private keys, have begun (see
    /// [`Redactor::open_line`], [`Redactor::open_value`]).
    lines_opened: u64,
    values_opened: u64,
}
impl fmt::Debug for Redactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Redactor")
            .field("buffered_bytes", &self.pending.len())
            .finish_non_exhaustive()
    }
}
impl Redactor {
    pub fn push(&mut self, chunk: &str) -> String {
        let mut output = String::new();
        self.push_into(chunk, &mut output);
        output
    }
    /// Finish one stream. An incomplete recognized credential is redacted;
    /// ordinary partial prefixes are released. The redactor can then be reused.
    pub fn finish(&mut self) -> String {
        let mut output = String::new();
        if let Mode::Marker { kind, seen } = std::mem::take(&mut self.mode) {
            self.emit(
                if is_marker(&seen) {
                    &seen
                } else {
                    kind.marker()
                },
                &mut output,
            );
        }
        self.drain(true, &mut output);
        self.mode = Mode::Normal;
        self.previous_word = false;
        self.hidden = None;
        self.hidden_pending = 0;
        output
    }
    /// [`Redactor::finish`], and when the stream ends inside a written
    /// quoted value, that value's closing quote after its marker
    /// (`password="[REDACTED:password]"`). CF12c review round 1 (F4): the
    /// client redacts a long line before the server redacts it again, and
    /// a quote left open there would make the server's pass read the text
    /// after it as the rest of the value.
    pub fn finish_closed(&mut self) -> String {
        let quote = match self.mode {
            Mode::Quoted {
                quote,
                in_header: false,
                ..
            } if self.hidden.is_none() => Some(quote),
            _ => None,
        };
        let mut output = self.finish();
        output.extend(quote);
        output
    }
    /// CF12c review round 1 (F4): give up the outermost
    /// [open construct](Redactor::open_construct), where a caller bounds
    /// how far one may run (the client's carry past a request's end).
    ///
    /// CF12c review round 2 (N1, N2): a cut never ends a discard or a value
    /// in the middle of a word, and never forgets what is being read.
    /// - A discarded header line is discarded only to the end of the word
    ///   the cut falls in; what the line was reading (a key glued into it
    ///   and waiting for its value, a quote, a private key) goes on.
    /// - Otherwise a quoted value ends (its closing quote written, when its
    ///   opening one was; a list item goes back to its list) and a private
    ///   key ends; the rest of the word the cut falls in is discarded.
    /// - Anything else is bounded and stays as it is: a key waiting for its
    ///   value, a list between items, a held prefix, a discarded word.
    pub fn cut_short(&mut self) -> String {
        let mut out = String::new();
        if self.hidden == Some(Hide::Line) {
            self.hidden = Some(Hide::Word);
            return out;
        }
        match std::mem::take(&mut self.mode) {
            Mode::Quoted {
                quote,
                array,
                in_header,
                ..
            } => {
                if !in_header && self.hidden.is_none() {
                    out.push(quote);
                }
                if let Some(kind) = array {
                    self.mode = Mode::ArrayNext { kind };
                }
            }
            Mode::Pem(_) | Mode::OversizedPem => {}
            other => {
                self.mode = other;
                return out;
            }
        }
        self.hide(Hide::Word);
        out
    }
    /// CF12c review round 2 (N2): whether the redactor is inside a
    /// construct that only its own end closes and that may run without
    /// bound while nothing is written: a quoted value, a private key, a
    /// header line being discarded. Everything else it can be in is
    /// bounded or written as it goes (a held prefix, a key waiting for its
    /// value across whitespace, a list between items, a value word).
    pub fn open_construct(&self) -> bool {
        self.hidden == Some(Hide::Line)
            || matches!(
                self.mode,
                Mode::Quoted { .. } | Mode::Pem(_) | Mode::OversizedPem
            )
    }
    /// CF12c review round 3: when a header line is being discarded, which
    /// one: the count of discarded header lines begun, which changes when
    /// a header is named on the line being discarded (it discards the same
    /// line from there). Quoted values and private keys that open and
    /// close inside the line leave it as it is, so a caller bounding how
    /// far the line runs counts from the line's own start.
    pub fn open_line(&self) -> Option<u64> {
        (self.hidden == Some(Hide::Line)).then_some(self.lines_opened)
    }
    /// CF12c review round 3: when a quoted value or a private key is open
    /// (on a discarded header line or not), which one: the count of those
    /// begun.
    pub fn open_value(&self) -> Option<u64> {
        matches!(
            self.mode,
            Mode::Quoted { .. } | Mode::Pem(_) | Mode::OversizedPem
        )
        .then_some(self.values_opened)
    }
    /// [`Redactor::push`], writing to `out`.
    pub fn push_into(&mut self, chunk: &str, out: &mut String) {
        for c in chunk.chars() {
            self.feed(c, out);
        }
    }
    pub fn buffered_bytes(&self) -> usize {
        self.pending.len()
    }
    /// Whether the redactor holds nothing: no held bytes, no open value.
    /// A stream cut here redacts as it would whole.
    pub fn idle(&self) -> bool {
        matches!(self.mode, Mode::Normal) && self.pending.is_empty() && self.hidden.is_none()
    }
    fn feed(&mut self, c: char, out: &mut String) {
        let ends = match self.hidden {
            None => false,
            Some(Hide::Word) => value_end(c),
            Some(Hide::Line) => matches!(c, '\n' | '\r'),
        };
        if ends {
            // The discarded value ends here; what it left held stays unwritten.
            self.hidden = None;
            self.hidden_pending = self.pending.len();
        }
        match std::mem::take(&mut self.mode) {
            Mode::Normal => {
                if self.pending.len() + c.len_utf8() > MAX_TAIL {
                    // Only a PEM header can reach this ceiling; shorter patterns never do.
                    self.emit(Kind::Pem.marker(), out);
                    self.hidden_pending = 0;
                    self.pending.clear();
                    self.mode = Mode::OversizedPem;
                    self.values_opened = self.values_opened.wrapping_add(1);
                } else {
                    self.pending.push(c);
                    self.drain(false, out);
                }
            }
            Mode::Separator { kind, separator } => {
                let line = matches!(separator, Separator::Header { .. });
                if c == '\n' || c == '\r' {
                    self.literal(c, out);
                } else if matches!(separator, Separator::Space) && horizontal(c) {
                    self.literal(c, out);
                    self.mode = Mode::AwaitValue {
                        kind,
                        line: false,
                        array: false,
                    };
                } else if horizontal(c) {
                    self.literal(c, out);
                    self.mode = Mode::Separator { kind, separator };
                } else if (matches!(separator, Separator::Equals { .. }) || line)
                    && (c == ':' || c == '=')
                {
                    self.literal(c, out);
                    self.mode = Mode::AwaitValue {
                        kind,
                        line,
                        array: false,
                    };
                } else if let (
                    Separator::Header {
                        quote_allowed: true,
                    }
                    | Separator::Equals {
                        quote_allowed: true,
                    },
                    '"' | '\'',
                ) = (separator, c)
                {
                    self.literal(c, out);
                    self.mode = Mode::Separator {
                        kind,
                        separator: match separator {
                            Separator::Equals { .. } => Separator::Equals {
                                quote_allowed: false,
                            },
                            _ => Separator::Header {
                                quote_allowed: false,
                            },
                        },
                    };
                } else {
                    self.feed(c, out);
                }
            }
            Mode::AwaitValue { kind, line, array } => {
                if array && (c.is_whitespace() || c == ',') {
                    self.literal(c, out);
                    self.mode = Mode::AwaitValue { kind, line, array };
                } else if c == '\n' || c == '\r' || matches!(c, ',' | ';' | '}' | ']' | ')' | '&') {
                    self.literal(c, out);
                } else if horizontal(c) {
                    self.literal(c, out);
                    self.mode = Mode::AwaitValue { kind, line, array };
                } else if c == '"' || c == '\'' {
                    self.literal(c, out);
                    self.emit(kind.marker(), out);
                    self.mode = Mode::Quoted {
                        quote: c,
                        escaped: false,
                        array: array.then_some(kind),
                        in_header: self.hidden == Some(Hide::Line),
                        header_bytes: 0,
                    };
                    self.values_opened = self.values_opened.wrapping_add(1);
                } else if c == '[' && !line {
                    self.mode = Mode::Marker {
                        kind,
                        seen: c.to_string(),
                    };
                } else {
                    self.emit(kind.marker(), out);
                    self.discard(c, if line { Hide::Line } else { Hide::Word }, out);
                }
            }
            Mode::Token => {
                if token_char(c) {
                    self.mode = Mode::Token;
                } else {
                    self.previous_word = true;
                    self.feed(c, out);
                }
            }
            Mode::Marker { kind, mut seen } => {
                if is_marker(&seen) {
                    if value_end(c) {
                        // A whole marker ends the value: it passes through.
                        self.emit(&seen, out);
                        self.previous_word = false;
                        self.feed(c, out);
                    } else if c == '[' {
                        // Another marker may follow (a value's marker, then
                        // a private key's glued to it): this one passes
                        // through, being a marker whatever follows, and the
                        // next is read the same way.
                        self.emit(&seen, out);
                        self.mode = Mode::Marker {
                            kind,
                            seen: c.to_string(),
                        };
                    } else {
                        // More value follows: the whole value is a secret,
                        // and the rest of its word is still scanned (F1).
                        self.emit(kind.marker(), out);
                        self.discard(c, Hide::Word, out);
                    }
                } else if seen == "[" && c != 'R' {
                    // CF12c review round 1: `[` opens a list
                    // (`"token": ["…"]`, `password=[ …`); its items are
                    // the values.
                    self.literal('[', out);
                    self.mode = Mode::AwaitValue {
                        kind,
                        line: false,
                        array: true,
                    };
                    self.feed(c, out);
                } else {
                    seen.push(c);
                    if Kind::ALL.iter().any(|k| k.marker().starts_with(&seen)) {
                        self.mode = Mode::Marker { kind, seen };
                    } else {
                        // Not a marker: an ordinary secret value. What was
                        // held is scanned as the start of its word (F1).
                        self.emit(kind.marker(), out);
                        self.hide(Hide::Word);
                        for held in seen.chars() {
                            self.feed(held, out);
                        }
                    }
                }
            }
            Mode::Quoted {
                quote,
                escaped,
                array,
                in_header,
                header_bytes,
            } => {
                if in_header && (c == '\n' || c == '\r') && header_bytes >= HEADER_QUOTE_BYTES {
                    // A quote a header line opened gives up at the first
                    // line break after its bound; what follows is read
                    // afresh.
                    self.feed(c, out);
                    return;
                }
                let header_bytes = if in_header {
                    header_bytes + c.len_utf8()
                } else {
                    0
                };
                if escaped {
                    self.mode = Mode::Quoted {
                        quote,
                        escaped: false,
                        array,
                        in_header,
                        header_bytes,
                    };
                } else if c == '\\' {
                    self.mode = Mode::Quoted {
                        quote,
                        escaped: true,
                        array,
                        in_header,
                        header_bytes,
                    };
                } else if c == quote {
                    if in_header {
                        self.previous_word = false;
                    } else {
                        self.literal(c, out);
                    }
                    if let Some(kind) = array {
                        self.mode = Mode::ArrayNext { kind };
                    }
                } else {
                    self.mode = Mode::Quoted {
                        quote,
                        escaped: false,
                        array,
                        in_header,
                        header_bytes,
                    };
                }
            }
            Mode::ArrayNext { kind } => {
                if c.is_whitespace() {
                    self.literal(c, out);
                    self.mode = Mode::ArrayNext { kind };
                } else if matches!(c, ',' | ':' | '=') {
                    // `,` starts the next item; `:` or `=` makes this item
                    // a key, and the value after it is redacted (N4).
                    self.literal(c, out);
                    self.mode = Mode::AwaitValue {
                        kind,
                        line: false,
                        array: true,
                    };
                } else {
                    self.feed(c, out);
                }
            }
            Mode::Pem(mut end) => {
                if end.push(c) {
                    self.previous_word = false;
                } else {
                    self.mode = Mode::Pem(end);
                }
            }
            Mode::OversizedPem => self.mode = Mode::OversizedPem,
        }
    }
    /// Discard what follows, to the end of the word or line (a value
    /// inside a discarded line is discarded to the end of that line).
    fn hide(&mut self, until: Hide) {
        if until == Hide::Line {
            self.lines_opened = self.lines_opened.wrapping_add(1);
        }
        self.hidden = Some(self.hidden.map_or(until, |h| h.max(until)));
        self.hidden_pending = 0;
        self.previous_word = false;
    }
    /// Start discarding an unquoted value at `c` (its marker is already
    /// written). The value is still scanned, unwritten (F1).
    fn discard(&mut self, c: char, until: Hide, out: &mut String) {
        self.hide(until);
        if until == Hide::Word && value_end(c) {
            // A value that starts with a separator no other rule takes
            // (Unicode whitespace): that character is the value's first,
            // as before.
            return;
        }
        self.feed(c, out);
    }
    fn emit(&self, text: &str, out: &mut String) {
        if self.hidden.is_none() {
            out.push_str(text);
        }
    }
    fn literal(&mut self, c: char, out: &mut String) {
        if self.hidden.is_none() {
            out.push(c);
        }
        self.previous_word = c.is_alphanumeric() || c == '_';
    }
    fn drain(&mut self, finish: bool, out: &mut String) {
        while !self.pending.is_empty() {
            match classify(&self.pending, self.previous_word, finish) {
                Match::Hold => break,
                Match::Literal => {
                    let c = self.pending.chars().next().expect("nonempty tail");
                    self.pending.drain(..c.len_utf8());
                    if self.hidden_pending > 0 {
                        // Held by a discarded word: never written.
                        self.hidden_pending -= c.len_utf8();
                        self.previous_word = c.is_alphanumeric() || c == '_';
                    } else {
                        self.literal(c, out);
                    }
                }
                Match::Secret(kind, mode) => {
                    self.emit(kind.marker(), out);
                    self.pending.clear();
                    self.hidden_pending = 0;
                    self.previous_word = true;
                    if matches!(mode, Mode::Pem(_) | Mode::OversizedPem) {
                        self.values_opened = self.values_opened.wrapping_add(1);
                    }
                    self.mode = mode;
                    break;
                }
                Match::Keyword(kind, separator) => {
                    let shown = self.pending[self.hidden_pending..].to_owned();
                    self.emit(&shown, out);
                    self.pending.clear();
                    self.hidden_pending = 0;
                    self.previous_word = true;
                    self.mode = Mode::Separator { kind, separator };
                    break;
                }
            }
        }
    }
}
/// Whether `text` is one of the markers this redactor writes.
fn is_marker(text: &str) -> bool {
    Kind::ALL.iter().any(|k| k.marker() == text)
}
fn horizontal(c: char) -> bool {
    c == ' ' || c == '\t'
}
fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}
fn value_end(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';' | ')' | ']' | '}' | '&')
}
enum Match {
    Hold,
    Literal,
    Secret(Kind, Mode),
    Keyword(Kind, Separator),
}
fn classify(text: &str, previous_word: bool, finish: bool) -> Match {
    let mut partial = false;
    for (prefix, kind) in [
        ("trb_pat_", Kind::Pat),
        ("trb_svc_", Kind::Svc),
        ("trb_exe_", Kind::Exe),
        ("trb_exr_", Kind::Exr),
        ("AKIA", Kind::Aws),
        ("ASIA", Kind::Aws),
    ] {
        if prefix.starts_with(text) {
            partial = true;
        }
        if let Some(tail) = text.strip_prefix(prefix) {
            let aws = matches!(kind, Kind::Aws);
            let width = if aws { 16 } else { 43 };
            let valid = tail.chars().all(|c| {
                if aws {
                    c.is_ascii_uppercase() || c.is_ascii_digit()
                } else {
                    token_char(c)
                }
            });
            if valid && (tail.len() == width || finish && !tail.is_empty()) {
                return Match::Secret(kind, Mode::Normal);
            }
            if valid && tail.len() < width {
                partial = true;
            }
        }
    }
    for (prefix, kind) in [
        ("ghp_", Kind::Github),
        ("gho_", Kind::Github),
        ("ghs_", Kind::Github),
        ("github_pat_", Kind::Github),
    ] {
        if prefix.starts_with(text) {
            partial = true;
        }
        if let Some(tail) = text.strip_prefix(prefix)
            && !tail.is_empty()
            && tail.chars().all(token_char)
        {
            return Match::Secret(kind, Mode::Token);
        }
    }
    // Slack: every `xox?-` prefix (D34), where `?` is one lowercase letter.
    let slack = text.as_bytes();
    if slack.len() <= 5
        && b"xox".starts_with(&slack[..slack.len().min(3)])
        && slack.get(3).is_none_or(u8::is_ascii_lowercase)
        && slack.get(4).is_none_or(|b| *b == b'-')
    {
        partial = true;
    }
    if slack.len() > 5
        && slack.starts_with(b"xox")
        && slack[3].is_ascii_lowercase()
        && slack[4] == b'-'
        && text[5..].chars().all(token_char)
    {
        return Match::Secret(Kind::Slack, Mode::Token);
    }
    let equals = Separator::Equals {
        quote_allowed: true,
    };
    for (word, kind, separator) in [
        (
            "authorization",
            Kind::Authorization,
            Separator::Header {
                quote_allowed: true,
            },
        ),
        ("bearer", Kind::Bearer, Separator::Space),
        ("password", Kind::Password, equals),
        ("secret", Kind::Secret, equals),
        ("token", Kind::Token, equals),
        // Signed URLs (user-content uploads, S3/GCS): the signature is a
        // credential until it expires; the rest of the URL stays readable.
        ("signature", Kind::Signature, equals),
        ("sig", Kind::Signature, equals),
    ] {
        // Value keywords also end longer key names (`DB_PASSWORD=`,
        // `GITHUB_TOKEN=`, `client_secret=`); the separator that must follow
        // keeps ordinary words intact. Header words must start a word.
        let suffix_ok = matches!(separator, Separator::Equals { .. });
        if (!previous_word || suffix_ok)
            && text.len() <= word.len()
            && word.as_bytes()[..text.len()].eq_ignore_ascii_case(text.as_bytes())
        {
            if text.len() == word.len() {
                return Match::Keyword(kind, separator);
            }
            partial = true;
        }
    }
    const BEGIN: &str = "-----BEGIN ";
    if BEGIN.starts_with(text) {
        partial = true;
    }
    if let Some(label) = text.strip_prefix(BEGIN) {
        if let Some(label) = label.strip_suffix("-----") {
            if label == "PRIVATE KEY" || label.ends_with(" PRIVATE KEY") {
                return Match::Secret(
                    Kind::Pem,
                    Mode::Pem(EndMarker::new(format!("-----END {label}-----"))),
                );
            }
        } else if label
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b' ' || b == b'-')
        {
            if text.len() >= MAX_TAIL || finish {
                return Match::Secret(Kind::Pem, Mode::OversizedPem);
            }
            partial = true;
        }
    }
    if partial && !finish {
        Match::Hold
    } else {
        Match::Literal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceUrlError {
    Invalid,
    SchemeNotAllowed,
    CredentialsNotAllowed,
}
/// Pure scheme policy, not an SSRF/network-fetch check. The operator may add
/// schemes; executable/local-content schemes remain forbidden. No credentials
/// in URLs. Outer whitespace/control characters are trimmed from the URL and
/// allow-list entries; embedded whitespace/control characters remain invalid.
pub fn validate_evidence_url(
    value: &str,
    operator_schemes: &[&str],
) -> Result<(), EvidenceUrlError> {
    let value = trim_url_boundary(value);
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(EvidenceUrlError::Invalid);
    }
    let iri = fluent_uri::Iri::parse(value).map_err(|_| EvidenceUrlError::Invalid)?;
    let scheme = iri.scheme().as_str();
    if ["javascript", "data", "vbscript", "file", "blob"]
        .iter()
        .any(|s| scheme.eq_ignore_ascii_case(s))
    {
        return Err(EvidenceUrlError::SchemeNotAllowed);
    }
    if !scheme.eq_ignore_ascii_case("https")
        && !operator_schemes
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(trim_url_boundary(s)))
    {
        return Err(EvidenceUrlError::SchemeNotAllowed);
    }
    if let Some(authority) = iri.authority() {
        if authority.userinfo().is_some() {
            return Err(EvidenceUrlError::CredentialsNotAllowed);
        }
        if authority.host().is_empty() {
            return Err(EvidenceUrlError::Invalid);
        }
    } else if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") {
        return Err(EvidenceUrlError::Invalid);
    }
    Ok(())
}

fn trim_url_boundary(value: &str) -> &str {
    value.trim_matches(|c: char| c.is_whitespace() || c.is_control())
}

/// Sanitize complete log entries as one request-scoped stream. Embedded
/// newlines split entries; no streaming state survives this call.
pub fn redact_lines(lines: Vec<String>) -> Vec<String> {
    let mut redactor = Redactor::default();
    let mut output = String::new();
    let orphan = orphan_pem_end(&lines);
    let mut lines = lines.into_iter();
    // D38 carries no redactor state between appends, so a private key cut by
    // an append boundary arrives here as an orphan body ending in its END
    // line (the earlier request already redacted from its BEGIN line). Drop
    // everything before that END line (F24).
    if let Some(end) = orphan {
        output.push_str(Kind::Pem.marker());
        let mut last = String::new();
        for _ in 0..=end {
            last = lines.next().unwrap_or_default();
        }
        let rest = last
            .find("-----END ")
            .and_then(|at| last[at + 9..].find("-----").map(|close| at + 9 + close + 5))
            .map_or("", |after| &last[after..]);
        output.push_str(&redactor.push(rest));
        output.push_str(&redactor.push("\n"));
    }
    for line in lines {
        output.push_str(&redactor.push(&line));
        output.push_str(&redactor.push("\n"));
    }
    output.push_str(&redactor.finish());
    output.lines().map(str::to_owned).collect()
}

/// The index of the first line holding a private-key END marker, when no
/// private-key BEGIN marker comes before it in this request.
fn orphan_pem_end(lines: &[String]) -> Option<usize> {
    for (index, line) in lines.iter().enumerate() {
        if line.contains("-----BEGIN ") {
            return None;
        }
        if let Some(at) = line.find("-----END ") {
            let label = &line[at + 9..];
            return label
                .find("-----")
                .map(|close| &label[..close])
                .filter(|label| *label == "PRIVATE KEY" || label.ends_with(" PRIVATE KEY"))
                .map(|_| index);
        }
    }
    None
}
