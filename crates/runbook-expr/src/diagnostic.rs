use std::ops::Range;

/// A bounded diagnostic with a UTF-8 byte span in the expression source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprDiagnostic {
    pub code: &'static str,
    pub span: Range<usize>,
    pub message: String,
}
