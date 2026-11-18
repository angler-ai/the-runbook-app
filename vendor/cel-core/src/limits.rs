//! Fixed, versioned compilation limits for the opt-in runbook parser.
use std::ops::Range;

#[derive(Debug, Clone, Copy)]
pub struct CompileLimits {
    _private: (),
}
impl CompileLimits {
    pub const fn runbook_v1() -> Self {
        Self { _private: () }
    }
    pub(crate) const SOURCE_BYTES: usize = 4096;
    pub(crate) const TOKENS: usize = 512;
    pub(crate) const LITERAL_BYTES: usize = 4096;
    pub(crate) const PARSE_DEPTH: usize = 256;
    pub(crate) const PARSE_NODES: usize = 512;
    pub(crate) const TREE_DEPTH: usize = 64;
    pub(crate) const DIAGNOSTICS: usize = 16;
    pub(crate) const DIAGNOSTIC_BYTES: usize = 4096;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileLimitKind {
    SourceBytes,
    Tokens,
    LiteralBytes,
    ParseDepth,
    ParseNodes,
    TreeDepth,
    MacroCount,
    Diagnostics,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileLimitError {
    pub kind: CompileLimitKind,
    pub span: Range<usize>,
}
impl std::fmt::Display for CompileLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "compilation limit {:?} at {}..{}",
            self.kind, self.span.start, self.span.end
        )
    }
}
impl std::error::Error for CompileLimitError {}
