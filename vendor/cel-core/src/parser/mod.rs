//! CEL (Common Expression Language) parser.

mod lexer;
pub mod macros;
mod parse;

use crate::types::{Span, SpannedExpr};

pub use lexer::{lex_bounded, LexError, SpannedToken, Token};
pub use macros::{
    ArgCount, Macro, MacroContext, MacroExpander, MacroExpansion, MacroRegistry, MacroStyle,
};
pub use parse::MacroCalls;

/// A parse error with source location.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {}..{}",
            self.message, self.span.start, self.span.end
        )
    }
}

impl std::error::Error for ParseError {}

/// Options for parsing CEL expressions.
#[derive(Debug, Clone, Default)]
pub struct ParseOptions {
    /// Custom macro registry. If None, uses standard CEL macros.
    pub macros: Option<MacroRegistry>,
}

impl ParseOptions {
    /// Create parse options with default settings (standard macros).
    pub fn new() -> Self {
        Self::default()
    }

    /// Create parse options with a custom macro registry.
    pub fn with_macros(macros: MacroRegistry) -> Self {
        Self {
            macros: Some(macros),
        }
    }

    /// Create parse options with no macros (all macro calls become regular calls).
    pub fn without_macros() -> Self {
        Self {
            macros: Some(MacroRegistry::new()),
        }
    }
}

/// Result of parsing a CEL expression.
///
/// Supports error recovery: may return both an AST and errors.
/// The AST may contain `Expr::Error` nodes where parsing failed.
/// Includes macro_calls map for IDE features.
#[derive(Debug, Clone)]
pub struct ParseResult {
    /// The parsed AST, if any parsing succeeded.
    /// May contain `Expr::Error` nodes for unparseable sections.
    /// Macros (has, all, exists, exists_one, map, filter) are expanded inline.
    pub ast: Option<SpannedExpr>,
    /// Map of comprehension/expansion IDs to original macro call expressions.
    /// Used for IDE features like hover to show the original source form.
    pub macro_calls: MacroCalls,
    /// Any parse errors encountered.
    pub errors: Vec<ParseError>,
}

impl ParseResult {
    /// Returns true if parsing completed without errors.
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty() && self.ast.is_some()
    }

    /// Returns true if there are any parse errors.
    pub fn is_err(&self) -> bool {
        !self.errors.is_empty()
    }

    /// Converts to a Result, discarding partial AST on error.
    pub fn into_result(self) -> Result<SpannedExpr, Vec<ParseError>> {
        if self.errors.is_empty() {
            self.ast.ok_or_else(Vec::new)
        } else {
            Err(self.errors)
        }
    }

    /// Unwraps the errors, panicking if there are none.
    pub fn unwrap_err(self) -> Vec<ParseError> {
        if self.errors.is_empty() {
            panic!("called unwrap_err on a ParseResult with no errors");
        }
        self.errors
    }
}

/// Parse a CEL expression from source.
///
/// Returns a `ParseResult` which may contain both a partial AST and errors
/// when error recovery is successful. Macros (has, all, exists, exists_one,
/// map, filter) are expanded inline during parsing.
///
/// The `macro_calls` field in the result maps comprehension/expansion IDs
/// to the original macro call expressions, which is useful for IDE features.
pub fn parse(input: &str) -> ParseResult {
    parse_with_options(input, ParseOptions::new())
}

/// Parse a CEL expression with custom options.
///
/// This allows configuring the macro registry used during parsing.
/// Use `ParseOptions::without_macros()` to disable macro expansion,
/// or `ParseOptions::with_macros(registry)` for custom macros.
pub fn parse_with_options(input: &str, options: ParseOptions) -> ParseResult {
    // First, lex the input
    let tokens = match lexer::lex(input) {
        Ok(tokens) => tokens,
        Err(e) => {
            return ParseResult {
                ast: None,
                macro_calls: MacroCalls::new(),
                errors: vec![ParseError {
                    message: e.message,
                    span: e.span,
                }],
            };
        }
    };

    // Get macro registry
    let macros = options.macros.unwrap_or_else(MacroRegistry::standard);

    // Parse the tokens (with inline macro expansion)
    let (ast, parse_errors, macro_calls) = parse::parse_tokens_with_macros(&tokens, macros);

    let errors: Vec<ParseError> = parse_errors
        .into_iter()
        .map(|e| ParseError {
            message: e.message,
            span: e.span,
        })
        .collect();

    ParseResult {
        ast,
        macro_calls,
        errors,
    }
}

/// Parse under the fixed runbook v1 resource envelope.
/// Syntax errors remain in `ParseResult::errors`; resource errors are terminal.
pub fn parse_bounded(
    input: &str,
    _limits: &crate::CompileLimits,
) -> Result<ParseResult, crate::CompileLimitError> {
    if input.len() > crate::CompileLimits::SOURCE_BYTES {
        return Err(crate::CompileLimitError {
            kind: crate::CompileLimitKind::SourceBytes,
            span: 0..input.len(),
        });
    }
    let tokens = match lexer::lex_bounded(input)? {
        Ok(tokens) => tokens,
        Err(e) => {
            return Ok(ParseResult {
                ast: None,
                macro_calls: MacroCalls::new(),
                errors: vec![ParseError {
                    message: e.message,
                    span: e.span,
                }],
            })
        }
    };
    parse::parse_tokens_bounded(&tokens)
}

#[cfg(test)]
mod bounded_counter_tests {
    use super::*;
    #[test]
    fn resource_hooks_precede_work() {
        stats::reset();
        assert!(parse_bounded(&"'".repeat(4097), &crate::CompileLimits::runbook_v1()).is_err());
        assert_eq!(stats::get().lex_callbacks, 0);
        stats::reset();
        assert!(parse_bounded(&"true ".repeat(513), &crate::CompileLimits::runbook_v1()).is_err());
        assert_eq!(stats::get().tokens, 512);
        assert_eq!(stats::get().nodes, 0);
        stats::reset();
        assert!(parse_bounded(
            "xs.exists(x, true) || xs.exists(x, true)",
            &crate::CompileLimits::runbook_v1()
        )
        .is_err());
        assert_eq!(stats::get().macro_callbacks, 1);
        assert_eq!(stats::get().cloned_nodes, 0);
        stats::reset();
        let source = format!("xs.exists(x, f({}))", vec!["0"; 250].join(","));
        assert!(parse_bounded(&source, &crate::CompileLimits::runbook_v1())
            .unwrap()
            .is_ok());
        assert_eq!(stats::get().macro_callbacks, 1);
        assert_eq!(stats::get().cloned_nodes, 0);
        assert_eq!(stats::get().nodes, 262);
        for source in [
            format!("{}true", "!".repeat(300)),
            format!("{}true{}", "(".repeat(100), ")".repeat(100)),
            format!("a{}", ".a".repeat(65)),
        ] {
            stats::reset();
            assert!(parse_bounded(&source, &crate::CompileLimits::runbook_v1()).is_err());
            assert!(stats::get().max_frames <= 256);
            assert!(stats::get().max_height <= 64);
            assert!(stats::get().nodes <= 512);
            assert_eq!(stats::get().live_frames, 0);
        }
    }
}

#[cfg(test)]
mod stats {
    use std::cell::Cell;
    #[derive(Clone, Copy, Default)]
    pub(super) struct Stats {
        pub lex_callbacks: usize,
        pub tokens: usize,
        pub nodes: usize,
        pub live_frames: usize,
        pub max_frames: usize,
        pub max_height: usize,
        pub macro_callbacks: usize,
        pub cloned_nodes: usize,
    }
    thread_local! { static STATS: Cell<Stats> = Cell::new(Stats::default()); }
    pub(super) fn get() -> Stats {
        STATS.with(Cell::get)
    }
    pub(super) fn reset() {
        STATS.with(|s| s.set(Stats::default()));
    }
    pub(super) fn update(f: impl FnOnce(&mut Stats)) {
        STATS.with(|s| {
            let mut stats = s.get();
            f(&mut stats);
            s.set(stats);
        });
    }
}
