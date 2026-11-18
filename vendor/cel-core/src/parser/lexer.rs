//! CEL lexer using logos.

use crate::types::Span;
use logos::Logos;

/// A token with its source span.
pub type SpannedToken = (Token, Span);

/// Lexer error with span information.
#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub message: String,
    pub span: Span,
}

#[derive(Default)]
pub struct LexerExtras {
    bounded: bool,
    error: Option<crate::CompileLimitError>,
}

fn check_literal(lex: &mut logos::Lexer<Token>, current: usize, additional: usize) -> Option<()> {
    if lex.extras.bounded
        && current
            .checked_add(additional)
            .is_none_or(|n| n > crate::CompileLimits::LITERAL_BYTES)
    {
        lex.extras.error = Some(crate::CompileLimitError {
            kind: crate::CompileLimitKind::LiteralBytes,
            span: lex.span(),
        });
        return None;
    }
    Some(())
}

macro_rules! push_literal {
    ($lex:expr, $buffer:expr, $value:expr, $bytes:expr) => {{
        let value = $value;
        check_literal($lex, $buffer.len(), $bytes(value))?;
        $buffer.push(value);
    }};
}

/// CEL tokens.
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(extras = LexerExtras)]
#[logos(skip r"[ \t\n\r\f]+")]
#[logos(skip r"//[^\n]*")]
pub enum Token {
    // === Numeric Literals ===
    // Order matters: more specific patterns first

    // Hex unsigned: 0x1Fu, 0X1FU
    #[regex(r"0[xX][0-9a-fA-F]+[uU]", lex_hex_uint)]
    // Decimal unsigned: 123u, 123U
    #[regex(r"[0-9]+[uU]", lex_decimal_uint, priority = 4)]
    UInt(u64),

    // Hex int: 0x1F, 0X1F
    #[regex(r"0[xX][0-9a-fA-F]+", lex_hex_int, priority = 3)]
    // Decimal int: 123 (lowest priority for numbers)
    #[regex(r"[0-9]+", lex_decimal_int, priority = 1)]
    Int(i64),

    // Integer overflow - specifically handle the i64::MIN absolute value case
    // 9223372036854775808 = |i64::MIN| = i64::MAX + 1
    // This allows "-9223372036854775808" to parse correctly
    #[token("9223372036854775808", |_| "9223372036854775808".to_string(), priority = 2)]
    IntOverflow(String),

    // Float with decimal point and optional exponent: 1.5, 1.5e10
    #[regex(r"[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?", lex_float, priority = 5)]
    // Float with exponent only: 1e10, 1E-5
    #[regex(r"[0-9]+[eE][+-]?[0-9]+", lex_float, priority = 2)]
    // Float starting with decimal point: .5, .99, .5e3
    #[regex(r"\.[0-9]+([eE][+-]?[0-9]+)?", lex_float, priority = 6)]
    Float(f64),

    // === String Literals ===
    // Triple-quoted strings (must come before single/double to match first)
    #[regex(r#"""""#, lex_triple_double_string)]
    #[regex(r"'''", lex_triple_single_string)]
    // Raw triple-quoted strings (must come before raw single/double to match first)
    #[regex(r#"[rR]""""#, lex_raw_triple_double_string)]
    #[regex(r"[rR]'''", lex_raw_triple_single_string)]
    // Raw strings
    #[regex(r#"[rR]""#, lex_raw_double_string)]
    #[regex(r"[rR]'", lex_raw_single_string)]
    // Regular strings
    #[regex(r#"""#, lex_double_string)]
    #[regex(r"'", lex_single_string)]
    String(String),

    // === Bytes Literals ===
    // Raw bytes triple-quoted (must come first)
    #[regex(r#"[bB][rR]""""#, lex_raw_bytes_triple_double)]
    #[regex(r"[bB][rR]'''", lex_raw_bytes_triple_single)]
    #[regex(r#"[rR][bB]""""#, lex_raw_bytes_triple_double)]
    #[regex(r"[rR][bB]'''", lex_raw_bytes_triple_single)]
    // Triple-quoted bytes (must come before single/double)
    #[regex(r#"[bB]""""#, lex_bytes_triple_double)]
    #[regex(r"[bB]'''", lex_bytes_triple_single)]
    // Raw bytes single/double-quoted
    #[regex(r#"[bB][rR]""#, lex_raw_bytes_double)]
    #[regex(r"[bB][rR]'", lex_raw_bytes_single)]
    #[regex(r#"[rR][bB]""#, lex_raw_bytes_double)]
    #[regex(r"[rR][bB]'", lex_raw_bytes_single)]
    // Regular bytes
    #[regex(r#"[bB]""#, lex_bytes_double)]
    #[regex(r"[bB]'", lex_bytes_single)]
    Bytes(Vec<u8>),

    // === Keywords ===
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("null")]
    Null,
    #[token("in")]
    In,

    // === Reserved Words ===
    #[token("as", |_| "as".to_string())]
    #[token("break", |_| "break".to_string())]
    #[token("const", |_| "const".to_string())]
    #[token("continue", |_| "continue".to_string())]
    #[token("else", |_| "else".to_string())]
    #[token("for", |_| "for".to_string())]
    #[token("function", |_| "function".to_string())]
    #[token("if", |_| "if".to_string())]
    #[token("import", |_| "import".to_string())]
    #[token("let", |_| "let".to_string())]
    #[token("loop", |_| "loop".to_string())]
    #[token("package", |_| "package".to_string())]
    #[token("namespace", |_| "namespace".to_string())]
    #[token("return", |_| "return".to_string())]
    #[token("var", |_| "var".to_string())]
    #[token("void", |_| "void".to_string())]
    #[token("while", |_| "while".to_string())]
    Reserved(String),

    // === Identifier ===
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*", |lex| lex.slice().to_string(), priority = 0)]
    #[regex(r"`[^`]+`", lex_backtick_ident)]
    Ident(String),

    // === Operators (multi-char first) ===
    #[token("==")]
    EqEq,
    #[token("!=")]
    Ne,
    #[token("<=")]
    Le,
    #[token(">=")]
    Ge,
    #[token("&&")]
    And,
    #[token("||")]
    Or,

    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("%")]
    Percent,
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("!")]
    Not,
    #[token("?")]
    Question,
    #[token(":")]
    Colon,

    // === Delimiters ===
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token(".")]
    Dot,
    #[token(",")]
    Comma,
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Int(n) => write!(f, "{}", n),
            Token::IntOverflow(s) => write!(f, "{}", s),
            Token::UInt(n) => write!(f, "{}u", n),
            Token::Float(n) => write!(f, "{}", n),
            Token::String(s) => write!(f, "\"{}\"", s),
            Token::Bytes(b) => write!(f, "b\"{}\"", String::from_utf8_lossy(b)),
            Token::True => write!(f, "true"),
            Token::False => write!(f, "false"),
            Token::Null => write!(f, "null"),
            Token::In => write!(f, "in"),
            Token::Reserved(s) => write!(f, "{}", s),
            Token::Ident(s) => write!(f, "{}", s),
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::Percent => write!(f, "%"),
            Token::EqEq => write!(f, "=="),
            Token::Ne => write!(f, "!="),
            Token::Lt => write!(f, "<"),
            Token::Le => write!(f, "<="),
            Token::Gt => write!(f, ">"),
            Token::Ge => write!(f, ">="),
            Token::And => write!(f, "&&"),
            Token::Or => write!(f, "||"),
            Token::Not => write!(f, "!"),
            Token::Question => write!(f, "?"),
            Token::Colon => write!(f, ":"),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::Dot => write!(f, "."),
            Token::Comma => write!(f, ","),
        }
    }
}

// === Lexer Callbacks for Numbers ===

fn lex_decimal_int(lex: &mut logos::Lexer<Token>) -> Option<i64> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex.slice().parse().ok()
}

fn lex_decimal_uint(lex: &mut logos::Lexer<Token>) -> Option<u64> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let s = lex.slice();
    s[..s.len() - 1].parse().ok() // Remove trailing u/U
}

fn lex_hex_int(lex: &mut logos::Lexer<Token>) -> Option<i64> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let s = lex.slice();
    i64::from_str_radix(&s[2..], 16).ok() // Skip 0x
}

fn lex_hex_uint(lex: &mut logos::Lexer<Token>) -> Option<u64> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let s = lex.slice();
    u64::from_str_radix(&s[2..s.len() - 1], 16).ok() // Skip 0x, remove u
}

fn lex_float(lex: &mut logos::Lexer<Token>) -> Option<f64> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex.slice().parse().ok()
}

fn lex_backtick_ident(lex: &mut logos::Lexer<Token>) -> String {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let s = lex.slice();
    s[1..s.len() - 1].to_string() // Strip backticks
}

// === Lexer Callbacks for Strings ===

fn lex_double_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_quoted_string(lex, '"')
}

fn lex_single_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_quoted_string(lex, '\'')
}

fn lex_quoted_string(lex: &mut logos::Lexer<Token>, quote: char) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let remainder = lex.remainder();
    let mut chars = remainder.chars().peekable();
    let mut result = std::string::String::new();
    let mut consumed = 0;

    while let Some(c) = chars.next() {
        consumed += c.len_utf8();
        if c == quote {
            lex.bump(consumed);
            return Some(result);
        } else if c == '\\' {
            let escape_char = chars.next()?;
            consumed += escape_char.len_utf8();
            match escape_char {
                '\\' => push_literal!(lex, result, '\\', |c: char| c.len_utf8()),
                '/' => push_literal!(lex, result, '/', |c: char| c.len_utf8()),
                '"' => push_literal!(lex, result, '"', |c: char| c.len_utf8()),
                '\'' => push_literal!(lex, result, '\'', |c: char| c.len_utf8()),
                '`' => push_literal!(lex, result, '`', |c: char| c.len_utf8()),
                'a' => push_literal!(lex, result, '\x07', |c: char| c.len_utf8()),
                'b' => push_literal!(lex, result, '\x08', |c: char| c.len_utf8()),
                'f' => push_literal!(lex, result, '\x0C', |c: char| c.len_utf8()),
                'n' => push_literal!(lex, result, '\n', |c: char| c.len_utf8()),
                'r' => push_literal!(lex, result, '\r', |c: char| c.len_utf8()),
                't' => push_literal!(lex, result, '\t', |c: char| c.len_utf8()),
                'v' => push_literal!(lex, result, '\x0B', |c: char| c.len_utf8()),
                '?' => push_literal!(lex, result, '?', |c: char| c.len_utf8()),
                'x' | 'X' => {
                    // \xHH or \XHH - 2 hex digits
                    let h1 = chars.next()?;
                    let h2 = chars.next()?;
                    consumed += h1.len_utf8() + h2.len_utf8();
                    let hex = format!("{}{}", h1, h2);
                    let val = u8::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, val as char, |c: char| c.len_utf8());
                }
                'u' => {
                    // \uXXXX - 4 hex digits
                    let hex: String = chars.by_ref().take(4).collect();
                    consumed += hex.len();
                    if hex.len() != 4 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, char::from_u32(val)?, |c: char| c.len_utf8());
                }
                'U' => {
                    // \UXXXXXXXX - 8 hex digits
                    let hex: String = chars.by_ref().take(8).collect();
                    consumed += hex.len();
                    if hex.len() != 8 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, char::from_u32(val)?, |c: char| c.len_utf8());
                }
                c @ '0'..='3' => {
                    // \DDD - octal (first digit 0-3, then 2 more digits)
                    let d2 = chars.next()?;
                    let d3 = chars.next()?;
                    consumed += d2.len_utf8() + d3.len_utf8();
                    if !matches!(d2, '0'..='7') || !matches!(d3, '0'..='7') {
                        return None;
                    }
                    let octal = format!("{}{}{}", c, d2, d3);
                    let val = u8::from_str_radix(&octal, 8).ok()?;
                    push_literal!(lex, result, val as char, |c: char| c.len_utf8());
                }
                _ => return None, // Invalid escape
            }
        } else if c == '\n' {
            // Newline not allowed in regular strings
            return None;
        } else {
            push_literal!(lex, result, c, |c: char| c.len_utf8());
        }
    }

    None // Unclosed string
}

fn lex_raw_double_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_string(lex, '"')
}

fn lex_raw_single_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_string(lex, '\'')
}

fn lex_raw_string(lex: &mut logos::Lexer<Token>, quote: char) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let remainder = lex.remainder();
    let mut result = std::string::String::new();
    let mut consumed = 0;

    for c in remainder.chars() {
        consumed += c.len_utf8();
        if c == quote {
            lex.bump(consumed);
            return Some(result);
        }
        push_literal!(lex, result, c, |c: char| c.len_utf8());
    }

    None // Unclosed string
}

fn lex_triple_double_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_triple_string(lex, "\"\"\"")
}

fn lex_triple_single_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_triple_string(lex, "'''")
}

fn lex_triple_string(lex: &mut logos::Lexer<Token>, end_quote: &str) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let remainder = lex.remainder();
    let end_bytes = end_quote.as_bytes();
    let end_len = end_bytes.len();
    let mut chars = remainder.chars().peekable();
    let mut result = std::string::String::new();
    let mut consumed = 0;

    while consumed + end_len <= remainder.len() {
        // Check if we've reached the end quote
        if &remainder.as_bytes()[consumed..consumed + end_len] == end_bytes {
            lex.bump(consumed + end_len);
            return Some(result);
        }

        let c = chars.next()?;
        consumed += c.len_utf8();

        if c == '\\' {
            let escape_char = chars.next()?;
            consumed += escape_char.len_utf8();
            match escape_char {
                '\\' => push_literal!(lex, result, '\\', |c: char| c.len_utf8()),
                '/' => push_literal!(lex, result, '/', |c: char| c.len_utf8()),
                '"' => push_literal!(lex, result, '"', |c: char| c.len_utf8()),
                '\'' => push_literal!(lex, result, '\'', |c: char| c.len_utf8()),
                '`' => push_literal!(lex, result, '`', |c: char| c.len_utf8()),
                'a' => push_literal!(lex, result, '\x07', |c: char| c.len_utf8()),
                'b' => push_literal!(lex, result, '\x08', |c: char| c.len_utf8()),
                'f' => push_literal!(lex, result, '\x0C', |c: char| c.len_utf8()),
                'n' => push_literal!(lex, result, '\n', |c: char| c.len_utf8()),
                'r' => push_literal!(lex, result, '\r', |c: char| c.len_utf8()),
                't' => push_literal!(lex, result, '\t', |c: char| c.len_utf8()),
                'v' => push_literal!(lex, result, '\x0B', |c: char| c.len_utf8()),
                '?' => push_literal!(lex, result, '?', |c: char| c.len_utf8()),
                'x' | 'X' => {
                    let h1 = chars.next()?;
                    let h2 = chars.next()?;
                    consumed += h1.len_utf8() + h2.len_utf8();
                    let hex = format!("{}{}", h1, h2);
                    let val = u8::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, val as char, |c: char| c.len_utf8());
                }
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    consumed += hex.len();
                    if hex.len() != 4 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, char::from_u32(val)?, |c: char| c.len_utf8());
                }
                'U' => {
                    let hex: String = chars.by_ref().take(8).collect();
                    consumed += hex.len();
                    if hex.len() != 8 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, char::from_u32(val)?, |c: char| c.len_utf8());
                }
                c @ '0'..='3' => {
                    let d2 = chars.next()?;
                    let d3 = chars.next()?;
                    consumed += d2.len_utf8() + d3.len_utf8();
                    if !matches!(d2, '0'..='7') || !matches!(d3, '0'..='7') {
                        return None;
                    }
                    let octal = format!("{}{}{}", c, d2, d3);
                    let val = u8::from_str_radix(&octal, 8).ok()?;
                    push_literal!(lex, result, val as char, |c: char| c.len_utf8());
                }
                _ => return None,
            }
        } else {
            // Allow literal newlines in triple-quoted strings
            push_literal!(lex, result, c, |c: char| c.len_utf8());
        }
    }

    // Check if the remaining content is exactly the end quote
    if consumed < remainder.len()
        && remainder.len() - consumed >= end_len
        && &remainder.as_bytes()[consumed..consumed + end_len] == end_bytes
    {
        lex.bump(consumed + end_len);
        return Some(result);
    }

    None // Unclosed triple-quoted string
}

fn lex_raw_triple_double_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_triple_string(lex, "\"\"\"")
}

fn lex_raw_triple_single_string(lex: &mut logos::Lexer<Token>) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_triple_string(lex, "'''")
}

/// Lex a raw triple-quoted string (no escape processing).
fn lex_raw_triple_string(lex: &mut logos::Lexer<Token>, end_quote: &str) -> Option<String> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let remainder = lex.remainder();
    if let Some(end_pos) = remainder.find(end_quote) {
        let content = &remainder[..end_pos];
        check_literal(lex, 0, content.len())?;
        lex.bump(end_pos + end_quote.len());
        Some(content.to_string())
    } else {
        None
    }
}

// === Lexer Callbacks for Bytes ===

fn lex_bytes_double(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_quoted_bytes(lex, '"')
}

fn lex_bytes_single(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_quoted_bytes(lex, '\'')
}

/// Lex a quoted byte string, handling escapes to produce raw bytes.
/// Unlike `lex_quoted_string`, octal/hex escapes push raw byte values
/// rather than interpreting them as Unicode code points.
fn lex_quoted_bytes(lex: &mut logos::Lexer<Token>, quote: char) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let remainder = lex.remainder();
    let mut chars = remainder.chars().peekable();
    let mut result = Vec::new();
    let mut consumed = 0;

    while let Some(c) = chars.next() {
        consumed += c.len_utf8();
        if c == quote {
            lex.bump(consumed);
            return Some(result);
        } else if c == '\\' {
            let escape_char = chars.next()?;
            consumed += escape_char.len_utf8();
            match escape_char {
                '\\' => push_literal!(lex, result, b'\\', |_: u8| 1),
                '/' => push_literal!(lex, result, b'/', |_: u8| 1),
                '"' => push_literal!(lex, result, b'"', |_: u8| 1),
                '\'' => push_literal!(lex, result, b'\'', |_: u8| 1),
                '`' => push_literal!(lex, result, b'`', |_: u8| 1),
                'a' => push_literal!(lex, result, 0x07, |_: u8| 1),
                'b' => push_literal!(lex, result, 0x08, |_: u8| 1),
                'f' => push_literal!(lex, result, 0x0C, |_: u8| 1),
                'n' => push_literal!(lex, result, b'\n', |_: u8| 1),
                'r' => push_literal!(lex, result, b'\r', |_: u8| 1),
                't' => push_literal!(lex, result, b'\t', |_: u8| 1),
                'v' => push_literal!(lex, result, 0x0B, |_: u8| 1),
                '?' => push_literal!(lex, result, b'?', |_: u8| 1),
                'x' | 'X' => {
                    // \xHH or \XHH - 2 hex digits -> raw byte
                    let h1 = chars.next()?;
                    let h2 = chars.next()?;
                    consumed += h1.len_utf8() + h2.len_utf8();
                    let hex = format!("{}{}", h1, h2);
                    let val = u8::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, val, |_: u8| 1);
                }
                'u' => {
                    // \uXXXX - 4 hex digits -> encode as UTF-8 bytes
                    let hex: String = chars.by_ref().take(4).collect();
                    consumed += hex.len();
                    if hex.len() != 4 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    let ch = char::from_u32(val)?;
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    check_literal(lex, result.len(), encoded.len())?;
                    result.extend_from_slice(encoded.as_bytes());
                }
                'U' => {
                    // \UXXXXXXXX - 8 hex digits -> encode as UTF-8 bytes
                    let hex: String = chars.by_ref().take(8).collect();
                    consumed += hex.len();
                    if hex.len() != 8 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    let ch = char::from_u32(val)?;
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    check_literal(lex, result.len(), encoded.len())?;
                    result.extend_from_slice(encoded.as_bytes());
                }
                c @ '0'..='3' => {
                    // \DDD - octal (first digit 0-3, then 2 more digits) -> raw byte
                    let d2 = chars.next()?;
                    let d3 = chars.next()?;
                    consumed += d2.len_utf8() + d3.len_utf8();
                    if !matches!(d2, '0'..='7') || !matches!(d3, '0'..='7') {
                        return None;
                    }
                    let octal = format!("{}{}{}", c, d2, d3);
                    let val = u8::from_str_radix(&octal, 8).ok()?;
                    push_literal!(lex, result, val, |_: u8| 1);
                }
                _ => return None, // Invalid escape
            }
        } else if c == '\n' {
            // Newline not allowed in regular byte strings
            return None;
        } else {
            // Regular character: encode as UTF-8 bytes
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            check_literal(lex, result.len(), encoded.len())?;
            result.extend_from_slice(encoded.as_bytes());
        }
    }

    None // Unclosed byte string
}

fn lex_bytes_triple_double(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_triple_bytes(lex, "\"\"\"")
}

fn lex_bytes_triple_single(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_triple_bytes(lex, "'''")
}

fn lex_triple_bytes(lex: &mut logos::Lexer<Token>, end_quote: &str) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    let remainder = lex.remainder();
    let end_bytes = end_quote.as_bytes();
    let end_len = end_bytes.len();
    let mut chars = remainder.chars().peekable();
    let mut result = Vec::new();
    let mut consumed = 0;

    while consumed + end_len <= remainder.len() {
        if &remainder.as_bytes()[consumed..consumed + end_len] == end_bytes {
            lex.bump(consumed + end_len);
            return Some(result);
        }

        let c = chars.next()?;
        consumed += c.len_utf8();

        if c == '\\' {
            let escape_char = chars.next()?;
            consumed += escape_char.len_utf8();
            match escape_char {
                '\\' => push_literal!(lex, result, b'\\', |_: u8| 1),
                '/' => push_literal!(lex, result, b'/', |_: u8| 1),
                '"' => push_literal!(lex, result, b'"', |_: u8| 1),
                '\'' => push_literal!(lex, result, b'\'', |_: u8| 1),
                '`' => push_literal!(lex, result, b'`', |_: u8| 1),
                'a' => push_literal!(lex, result, 0x07, |_: u8| 1),
                'b' => push_literal!(lex, result, 0x08, |_: u8| 1),
                'f' => push_literal!(lex, result, 0x0C, |_: u8| 1),
                'n' => push_literal!(lex, result, b'\n', |_: u8| 1),
                'r' => push_literal!(lex, result, b'\r', |_: u8| 1),
                't' => push_literal!(lex, result, b'\t', |_: u8| 1),
                'v' => push_literal!(lex, result, 0x0B, |_: u8| 1),
                '?' => push_literal!(lex, result, b'?', |_: u8| 1),
                'x' | 'X' => {
                    let h1 = chars.next()?;
                    let h2 = chars.next()?;
                    consumed += h1.len_utf8() + h2.len_utf8();
                    let hex = format!("{}{}", h1, h2);
                    let val = u8::from_str_radix(&hex, 16).ok()?;
                    push_literal!(lex, result, val, |_: u8| 1);
                }
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    consumed += hex.len();
                    if hex.len() != 4 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    let ch = char::from_u32(val)?;
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    check_literal(lex, result.len(), encoded.len())?;
                    result.extend_from_slice(encoded.as_bytes());
                }
                'U' => {
                    let hex: String = chars.by_ref().take(8).collect();
                    consumed += hex.len();
                    if hex.len() != 8 {
                        return None;
                    }
                    let val = u32::from_str_radix(&hex, 16).ok()?;
                    let ch = char::from_u32(val)?;
                    let mut buf = [0u8; 4];
                    let encoded = ch.encode_utf8(&mut buf);
                    check_literal(lex, result.len(), encoded.len())?;
                    result.extend_from_slice(encoded.as_bytes());
                }
                c @ '0'..='3' => {
                    let d2 = chars.next()?;
                    let d3 = chars.next()?;
                    consumed += d2.len_utf8() + d3.len_utf8();
                    if !matches!(d2, '0'..='7') || !matches!(d3, '0'..='7') {
                        return None;
                    }
                    let octal = format!("{}{}{}", c, d2, d3);
                    let val = u8::from_str_radix(&octal, 8).ok()?;
                    push_literal!(lex, result, val, |_: u8| 1);
                }
                _ => return None,
            }
        } else {
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            check_literal(lex, result.len(), encoded.len())?;
            result.extend_from_slice(encoded.as_bytes());
        }
    }

    if consumed < remainder.len()
        && remainder.len() - consumed >= end_len
        && &remainder.as_bytes()[consumed..consumed + end_len] == end_bytes
    {
        lex.bump(consumed + end_len);
        return Some(result);
    }

    None
}

fn lex_raw_bytes_double(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_string(lex, '"').map(|s| s.into_bytes())
}

fn lex_raw_bytes_single(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_string(lex, '\'').map(|s| s.into_bytes())
}

fn lex_raw_bytes_triple_double(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_triple_string(lex, "\"\"\"").map(|s| s.into_bytes())
}

fn lex_raw_bytes_triple_single(lex: &mut logos::Lexer<Token>) -> Option<Vec<u8>> {
    #[cfg(test)]
    super::stats::update(|s| s.lex_callbacks += 1);
    lex_raw_triple_string(lex, "'''").map(|s| s.into_bytes())
}

// === Public Lexer API ===

/// Tokenize the input string.
pub fn lex(input: &str) -> Result<Vec<SpannedToken>, LexError> {
    let mut tokens = Vec::new();
    let mut lexer = Token::lexer(input);

    while let Some(result) = lexer.next() {
        let span = lexer.span();
        match result {
            Ok(token) => tokens.push((token, span)),
            Err(_) => {
                return Err(LexError {
                    message: format!("unexpected character '{}'", &input[span.clone()]),
                    span,
                })
            }
        }
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_tokens(input: &str) -> Vec<Token> {
        lex(input)
            .unwrap()
            .into_iter()
            .map(|(tok, _)| tok)
            .collect()
    }

    #[test]
    fn lex_integers() {
        assert_eq!(lex_tokens("123"), vec![Token::Int(123)]);
        assert_eq!(lex_tokens("0"), vec![Token::Int(0)]);
        assert_eq!(lex_tokens("0x1F"), vec![Token::Int(31)]);
        assert_eq!(lex_tokens("0XAB"), vec![Token::Int(171)]);
    }

    #[test]
    fn lex_unsigned_integers() {
        assert_eq!(lex_tokens("123u"), vec![Token::UInt(123)]);
        assert_eq!(lex_tokens("123U"), vec![Token::UInt(123)]);
        assert_eq!(lex_tokens("0x1Fu"), vec![Token::UInt(31)]);
    }

    #[test]
    fn lex_floats() {
        assert_eq!(lex_tokens("1.5"), vec![Token::Float(1.5)]);
        assert_eq!(lex_tokens("1e10"), vec![Token::Float(1e10)]);
        assert_eq!(lex_tokens("1.5e-3"), vec![Token::Float(1.5e-3)]);
    }

    #[test]
    fn lex_strings() {
        assert_eq!(
            lex_tokens(r#""hello""#),
            vec![Token::String("hello".to_string())]
        );
        assert_eq!(
            lex_tokens("'world'"),
            vec![Token::String("world".to_string())]
        );
        assert_eq!(
            lex_tokens(r#""hello\nworld""#),
            vec![Token::String("hello\nworld".to_string())]
        );
    }

    #[test]
    fn lex_raw_strings() {
        assert_eq!(
            lex_tokens(r#"r"hello\n""#),
            vec![Token::String(r"hello\n".to_string())]
        );
        assert_eq!(
            lex_tokens(r"r'hello\n'"),
            vec![Token::String(r"hello\n".to_string())]
        );
    }

    #[test]
    fn lex_triple_strings() {
        assert_eq!(
            lex_tokens(
                r#""""multi
line""""#
            ),
            vec![Token::String("multi\nline".to_string())]
        );
    }

    #[test]
    fn lex_bytes() {
        assert_eq!(
            lex_tokens(r#"b"hello""#),
            vec![Token::Bytes(b"hello".to_vec())]
        );
        assert_eq!(
            lex_tokens("b'world'"),
            vec![Token::Bytes(b"world".to_vec())]
        );
    }

    #[test]
    fn lex_keywords() {
        assert_eq!(lex_tokens("true"), vec![Token::True]);
        assert_eq!(lex_tokens("false"), vec![Token::False]);
        assert_eq!(lex_tokens("null"), vec![Token::Null]);
        assert_eq!(lex_tokens("in"), vec![Token::In]);
    }

    #[test]
    fn lex_identifiers() {
        assert_eq!(lex_tokens("foo"), vec![Token::Ident("foo".to_string())]);
        assert_eq!(lex_tokens("_bar"), vec![Token::Ident("_bar".to_string())]);
        assert_eq!(
            lex_tokens("baz123"),
            vec![Token::Ident("baz123".to_string())]
        );
    }

    #[test]
    fn lex_operators() {
        assert_eq!(
            lex_tokens("+ - * / %"),
            vec![
                Token::Plus,
                Token::Minus,
                Token::Star,
                Token::Slash,
                Token::Percent
            ]
        );
        assert_eq!(
            lex_tokens("== != < <= > >="),
            vec![
                Token::EqEq,
                Token::Ne,
                Token::Lt,
                Token::Le,
                Token::Gt,
                Token::Ge
            ]
        );
        assert_eq!(
            lex_tokens("&& || !"),
            vec![Token::And, Token::Or, Token::Not]
        );
        assert_eq!(lex_tokens("? :"), vec![Token::Question, Token::Colon]);
    }

    #[test]
    fn lex_delimiters() {
        assert_eq!(
            lex_tokens("( ) [ ] { } . ,"),
            vec![
                Token::LParen,
                Token::RParen,
                Token::LBracket,
                Token::RBracket,
                Token::LBrace,
                Token::RBrace,
                Token::Dot,
                Token::Comma
            ]
        );
    }

    #[test]
    fn lex_expression() {
        assert_eq!(
            lex_tokens("a + b * 2"),
            vec![
                Token::Ident("a".to_string()),
                Token::Plus,
                Token::Ident("b".to_string()),
                Token::Star,
                Token::Int(2)
            ]
        );
    }

    #[test]
    fn lex_with_comments() {
        assert_eq!(
            lex_tokens("a // comment\n+ b"),
            vec![
                Token::Ident("a".to_string()),
                Token::Plus,
                Token::Ident("b".to_string())
            ]
        );
    }

    #[test]
    fn lex_unicode_escapes() {
        // \uXXXX - 4 hex digits
        assert_eq!(
            lex_tokens(r#""\u0041""#),
            vec![Token::String("A".to_string())]
        );
        assert_eq!(
            lex_tokens(r#""\u03B1""#), // Greek alpha
            vec![Token::String("α".to_string())]
        );
        // \UXXXXXXXX - 8 hex digits
        assert_eq!(
            lex_tokens(r#""\U00000041""#),
            vec![Token::String("A".to_string())]
        );
        assert_eq!(
            lex_tokens(r#""\U0001F600""#), // Emoji
            vec![Token::String("😀".to_string())]
        );
    }

    #[test]
    fn lex_octal_escapes() {
        // \DDD - 3 octal digits (000-377)
        assert_eq!(
            lex_tokens(r#""\101""#), // 'A' = 65 = 0o101
            vec![Token::String("A".to_string())]
        );
        assert_eq!(
            lex_tokens(r#""\000""#), // NUL
            vec![Token::String("\0".to_string())]
        );
        assert_eq!(
            lex_tokens(r#""\377""#), // 255 = 0o377
            vec![Token::String("\u{FF}".to_string())]
        );
    }

    #[test]
    fn lex_reserved_words() {
        // Reserved words should produce Token::Reserved, not Token::Ident
        assert_eq!(lex_tokens("if"), vec![Token::Reserved("if".to_string())]);
        assert_eq!(
            lex_tokens("else"),
            vec![Token::Reserved("else".to_string())]
        );
        assert_eq!(lex_tokens("for"), vec![Token::Reserved("for".to_string())]);
        assert_eq!(
            lex_tokens("while"),
            vec![Token::Reserved("while".to_string())]
        );
        assert_eq!(
            lex_tokens("return"),
            vec![Token::Reserved("return".to_string())]
        );
        assert_eq!(lex_tokens("let"), vec![Token::Reserved("let".to_string())]);
        assert_eq!(
            lex_tokens("const"),
            vec![Token::Reserved("const".to_string())]
        );
        assert_eq!(lex_tokens("var"), vec![Token::Reserved("var".to_string())]);
        assert_eq!(
            lex_tokens("function"),
            vec![Token::Reserved("function".to_string())]
        );
        assert_eq!(
            lex_tokens("namespace"),
            vec![Token::Reserved("namespace".to_string())]
        );
    }

    #[test]
    fn lex_integer_overflow() {
        // i64::MAX = 9223372036854775807 (fits)
        assert_eq!(
            lex_tokens("9223372036854775807"),
            vec![Token::Int(9223372036854775807)]
        );

        // i64::MAX + 1 = 9223372036854775808 (overflow - this is |i64::MIN|)
        // This specific value is needed for "-9223372036854775808" to work
        assert_eq!(
            lex_tokens("9223372036854775808"),
            vec![Token::IntOverflow("9223372036854775808".to_string())]
        );

        // Normal 19-digit number that fits
        assert_eq!(
            lex_tokens("1000000000000000000"),
            vec![Token::Int(1000000000000000000)]
        );
    }
}

/// Tokenize under the fixed runbook envelope, before allocating unbounded tokens.
pub fn lex_bounded(
    input: &str,
) -> Result<Result<Vec<SpannedToken>, LexError>, crate::CompileLimitError> {
    if input.len() > crate::CompileLimits::SOURCE_BYTES {
        return Err(crate::CompileLimitError {
            kind: crate::CompileLimitKind::SourceBytes,
            span: 0..input.len(),
        });
    }
    lex_bounded_tokens(input)
}

fn lex_bounded_tokens(
    input: &str,
) -> Result<Result<Vec<SpannedToken>, LexError>, crate::CompileLimitError> {
    let mut lexer = Token::lexer(input);
    lexer.extras.bounded = true;
    let mut tokens = Vec::new();
    while let Some(result) = lexer.next() {
        if let Some(error) = lexer.extras.error.take() {
            return Err(error);
        }
        let span = lexer.span();
        if tokens.len() == crate::CompileLimits::TOKENS {
            return Err(crate::CompileLimitError {
                kind: crate::CompileLimitKind::Tokens,
                span,
            });
        }
        match result {
            Ok(token) => {
                #[cfg(test)]
                super::stats::update(|s| s.tokens += 1);
                tokens.push((token, span));
            }
            Err(_) => {
                return Ok(Err(LexError {
                    message: "invalid token or literal".into(),
                    span,
                }))
            }
        }
    }
    Ok(Ok(tokens))
}

#[cfg(test)]
mod bounded_literal_tests {
    use super::*;
    #[test]
    fn callbacks_stop_before_decoded_literal_overflow() {
        // Exercise the callback's own guard independently of the public source guard.
        for quote in ["\"", "'", "\"\"\"", "r\"", "b\""] {
            let end = if quote == "\"\"\"" {
                "\"\"\""
            } else if quote == "'" {
                "'"
            } else {
                "\""
            };
            let source = format!("{quote}{}{end}", "x".repeat(4097));
            assert_eq!(
                lex_bounded_tokens(&source).unwrap_err().kind,
                crate::CompileLimitKind::LiteralBytes
            );
            let source = format!("{quote}{}{end}", "x".repeat(4096));
            assert!(lex_bounded_tokens(&source).unwrap().is_ok());
        }
    }
}
