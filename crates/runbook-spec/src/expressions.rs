//! Spec-facing adapter to the reviewed, bounded expression implementation.

use crate::Diagnostic;
// Keep adopted types intact, including full revision/iteration provenance.
pub use runbook_expr::{
    Bindings, EvalError, EvalStats, EvaluatedValue, EventTime, FieldDecl, InputNode, InputValue,
    IterationIdentity, Origin, ReadKind, ReadRef, RecordIdentity, SchemaDef, TypeDef, TypeEnv,
    TypeId, TypedExpression, evaluate,
};

/// Check a standalone expression. Locations refer to its decoded source text.
pub fn check_expression(source: &str, env: &TypeEnv) -> Result<TypedExpression, Diagnostic> {
    runbook_expr::check_expression(source, env).map_err(|error| {
        let prefix = &source[..error.span.start];
        Diagnostic {
            message: None,
            code: error.code.into(),
            path: String::new(),
            line: prefix.bytes().filter(|&byte| byte == b'\n').count() + 1,
            column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
        }
    })
}

/// Check decoded CEL from a YAML scalar, retaining its document path and anchor.
///
/// YAML quoting, escapes and folding prevent translating a decoded CEL byte
/// offset into a raw YAML column without an explicit source map. This adapter
/// therefore points to the scalar itself, rather than inventing that mapping.
pub fn check_expression_at(
    source: &str,
    env: &TypeEnv,
    path: &str,
    anchor: crate::SourceSpan,
) -> Result<TypedExpression, Diagnostic> {
    runbook_expr::check_expression(source, env).map_err(|error| Diagnostic {
        message: None,
        code: error.code.into(),
        path: path.into(),
        line: anchor.line,
        column: anchor.column,
    })
}
