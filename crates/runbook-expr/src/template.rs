use std::ops::Range;

use crate::{
    Bindings, EvalError, EvalSession, EvaluatedValue, ExprDiagnostic, ReadKind, TypeEnv,
    TypedExpression, check_expression,
    compile::diagnostic,
    evaluate_in,
    output::{charge_json_string_content, charge_output, scalar_text},
    runtime::evaluate_raw_with_source,
};

const SOURCE_BYTES: usize = 65_536;
const EXPRESSION_BYTES: usize = 4_096;
const EXPRESSIONS: usize = 64;

/// Whether interpolation preserves a single expression's type or produces text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateMode {
    WholeValue,
    Text,
}

/// A bounded template whose expressions have passed the closed-schema checker.
/// Literal whitespace is retained verbatim; executable segments cannot be supplied
/// by callers or deserialized.
#[derive(Debug, Clone)]
pub struct CheckedTemplate {
    mode: TemplateMode,
    segments: Vec<Segment>,
    schema_fingerprint: [u8; 32],
}

#[derive(Debug, Clone)]
enum Segment {
    Literal(String),
    Expression(Box<TypedExpression>, Range<usize>),
}

/// A checked expression borrowed from its original template. The byte span is
/// the untrimmed interior, excluding delimiters; it is not a YAML source span.
#[derive(Debug, Clone)]
pub struct TemplateExpressionRef<'a> {
    pub source_span: Range<usize>,
    pub expression: &'a TypedExpression,
}

impl CheckedTemplate {
    pub fn expressions(&self) -> impl Iterator<Item = TemplateExpressionRef<'_>> + '_ {
        self.segments.iter().filter_map(|segment| match segment {
            Segment::Expression(expression, span) => Some(TemplateExpressionRef {
                source_span: span.clone(),
                expression,
            }),
            Segment::Literal(_) => None,
        })
    }

    pub fn mode(&self) -> TemplateMode {
        self.mode
    }
}

/// Syntax-only metadata has no schema, checked type or execution authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpressionSyntaxSummary {
    pub source_span: Range<usize>,
    pub dependencies: Vec<crate::StaticDependency>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSyntaxSummary {
    pub mode: TemplateMode,
    pub expressions: Vec<ExpressionSyntaxSummary>,
}

/// Inspect bounded syntax before a closed type environment exists. Success does
/// not imply type validity: check_template is still required before execution.
pub fn summarize_template_syntax(source: &str) -> Result<TemplateSyntaxSummary, ExprDiagnostic> {
    let mut expressions = Vec::new();
    let mode = visit_expressions(source, |span| {
        let expression = crate::compile::parse_expression(&source[span.clone()])?;
        crate::profile::validate_parsed(&expression)?;
        expressions.push(ExpressionSyntaxSummary {
            source_span: span,
            dependencies: crate::metadata::dependencies(&expression),
        });
        Ok(())
    })?;
    Ok(TemplateSyntaxSummary { mode, expressions })
}

/// Check interpolation boundaries and compile each expression without trimming.
/// Text-mode collection rejection belongs to rendering, not compilation.
pub fn check_template(source: &str, env: &TypeEnv) -> Result<CheckedTemplate, ExprDiagnostic> {
    let mut segments = Vec::new();
    let mut cursor = 0;
    let mode = visit_expressions(source, |span| {
        let expression = check_expression(&source[span.clone()], env)?;
        let open = span.start - 3;
        if cursor != open {
            segments.push(Segment::Literal(source[cursor..open].to_owned()));
        }
        cursor = span.end + 2;
        segments.push(Segment::Expression(Box::new(expression), span));
        Ok(())
    })?;
    if cursor < source.len() || segments.is_empty() {
        segments.push(Segment::Literal(source[cursor..].to_owned()));
    }
    Ok(CheckedTemplate {
        mode,
        segments,
        schema_fingerprint: env.fingerprint(),
    })
}

// Private sequential scanner shared by checking and syntax metadata. Processing
// occurs before scanning the next segment, preserving existing error precedence.
// No caller-provided callback is accepted by either public boundary.
fn visit_expressions(
    source: &str,
    mut visit: impl FnMut(Range<usize>) -> Result<(), ExprDiagnostic>,
) -> Result<TemplateMode, ExprDiagnostic> {
    // No source scan, literal copy, or segment allocation precedes this gate.
    if source.len() > SOURCE_BYTES {
        return Err(diagnostic(
            "expression.compile_limit",
            0..source.len(),
            "Template exceeds 65,536 bytes",
        ));
    }
    let mut cursor = 0;
    let mut expressions = 0;
    let mut mode = TemplateMode::Text;
    while let Some(relative) = source[cursor..].find("${{") {
        let open = cursor + relative;
        if expressions == EXPRESSIONS {
            return Err(diagnostic(
                "expression.compile_limit",
                open..open + 3,
                "Template exceeds 64 expressions",
            ));
        }
        let start = open + 3;
        let end = expression_end(source, open)?;
        visit(start..end).map_err(|mut error| {
            // Compiler/profile diagnostics use byte spans local to the interior.
            error.span = error.span.start + start..error.span.end + start;
            error
        })?;
        if open == 0 && end + 2 == source.len() {
            mode = TemplateMode::WholeValue;
        }
        expressions += 1;
        cursor = end + 2;
    }
    Ok(mode)
}

/// Render with cumulative fuel, reads, and encoded JSON output limits.
/// Whole-value expressions retain their type; text accepts actual scalars only.
pub fn render_template(
    template: &CheckedTemplate,
    bindings: &Bindings,
    session: &mut EvalSession,
) -> Result<EvaluatedValue, EvalError> {
    // Literal-only and empty templates have the same entry checks as expressions.
    if let Some(error) = session.terminal_error() {
        return Err(error);
    }
    if template.schema_fingerprint != bindings.schema.fingerprint() {
        return Err(EvalError::SchemaMismatch);
    }
    if template.mode == TemplateMode::WholeValue {
        let [Segment::Expression(expression, _)] = template.segments.as_slice() else {
            unreachable!("checked WholeValue has exactly one expression");
        };
        return evaluate_in(expression, bindings, session);
    }

    // All segments produce one JSON string: one node and one pair of quotes.
    charge_output(session, 1, 2, 0)?;
    let mut text = String::new();
    for segment in &template.segments {
        match segment {
            Segment::Literal(literal) => {
                charge_json_string_content(session, literal)?;
                text.push_str(literal);
            }
            Segment::Expression(expression, _) => {
                let (value, source) = evaluate_raw_with_source(expression, bindings, session)?;
                // Refuse collections before reading descendants or formatting them.
                // The actual value may be Null even for a nullable record descriptor.
                let fragment = scalar_text(&value)?;
                if let Some(source) = source {
                    session
                        .reads
                        .record(&source.origin, ReadKind::Value, &session.budget)?;
                }
                charge_json_string_content(session, fragment.as_str())?;
                text.push_str(fragment.as_str());
            }
        }
    }
    if let Some(error) = session.terminal_error() {
        return Err(error);
    }
    Ok(EvaluatedValue {
        value: serde_json::Value::String(text),
        stats: session.stats(),
    })
}

fn expression_end(source: &str, open: usize) -> Result<usize, ExprDiagnostic> {
    let bytes = source.as_bytes();
    let start = open + 3;
    let mut cursor = start;
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    loop {
        if cursor - start > EXPRESSION_BYTES {
            // Byte scanning may stop within a multibyte character. Include that
            // character in the diagnostic without allocating or scanning further.
            let mut end = cursor;
            while !source.is_char_boundary(end) {
                end += 1;
            }
            return Err(diagnostic(
                "expression.compile_limit",
                start..end,
                "Template expression exceeds 4,096 bytes",
            ));
        }
        let Some(&byte) = bytes.get(cursor) else {
            return Err(diagnostic(
                "expression.syntax_error",
                open..source.len(),
                "Unterminated template expression",
            ));
        };
        if comment {
            // cel-core's line comment is //[^\n]*; CR alone does not end it.
            comment = byte != b'\n';
        } else if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
        } else if matches!(byte, b'\'' | b'"') {
            quote = Some(byte);
        } else if bytes[cursor..].starts_with(b"//") {
            comment = true;
        } else if bytes[cursor..].starts_with(b"}}") {
            return Ok(cursor);
        } else if bytes[cursor..].starts_with(b"${{") {
            return Err(diagnostic(
                "expression.syntax_error",
                cursor..cursor + 3,
                "Nested template expressions are unsupported",
            ));
        }
        // ASCII quote/escape/comment markers cannot occur in UTF-8 continuation
        // bytes. The compiler alone validates escapes and all other CEL syntax;
        // this scanner does not decode or recursively interpolate string values.
        cursor += 1;
    }
}

/// Whether every true branch of a boolean guard compares the named field for
/// equality with one of the given values. Other expression forms are ignored.
pub fn template_requires_string_equality(source: &str, path: &[&str], allowed: &[&str]) -> bool {
    use cel_core::types::{BinaryOp, Expr, SpannedExpr};

    fn selects(node: &SpannedExpr, path: &[&str]) -> bool {
        let Some((field, parent)) = path.split_last() else {
            return false;
        };
        match &node.node {
            Expr::Ident(name) => parent.is_empty() && name == field,
            Expr::Member {
                expr, field: name, ..
            } => name == field && selects(expr, parent),
            Expr::Index { expr, index, .. } => {
                matches!(&index.node, Expr::String(name) if name == field) && selects(expr, parent)
            }
            _ => false,
        }
    }

    fn requires(node: &SpannedExpr, path: &[&str], allowed: &[&str]) -> bool {
        let Expr::Binary { op, left, right } = &node.node else {
            return false;
        };
        match op {
            BinaryOp::And => requires(left, path, allowed) || requires(right, path, allowed),
            BinaryOp::Or => requires(left, path, allowed) && requires(right, path, allowed),
            BinaryOp::Eq => [(left, right), (right, left)].into_iter().any(|(field, value)| {
                selects(field, path)
                    && matches!(&value.node, Expr::String(value) if allowed.contains(&value.as_str()))
            }),
            _ => false,
        }
    }

    let mut required = false;
    let mode = visit_expressions(source, |span| {
        let expr = crate::compile::parse_expression(&source[span])?;
        crate::profile::validate_parsed(&expr)?;
        required = requires(&expr, path, allowed);
        Ok(())
    });
    matches!(mode, Ok(TemplateMode::WholeValue)) && required
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldDecl, ResultType, SchemaDef, TypeDef, TypeId};
    use std::collections::BTreeMap;

    fn env() -> TypeEnv {
        TypeEnv::new(&SchemaDef {
            types: vec![TypeDef::String, TypeDef::List(TypeId(0))],
            roots: BTreeMap::from([(
                "regions".into(),
                FieldDecl {
                    ty: TypeId(1),
                    nullable: false,
                    required: true,
                },
            )]),
        })
        .unwrap()
    }

    #[test]
    fn renderer_receives_verbatim_literals_and_checked_untrimmed_expressions() {
        let template =
            check_template(" \t${{ regions }}\n${{'${{not_recursive}}'}} é ", &env()).unwrap();
        let [
            Segment::Literal(prefix),
            Segment::Expression(regions, _),
            Segment::Literal(middle),
            Segment::Expression(quoted, _),
            Segment::Literal(suffix),
        ] = template.segments.as_slice()
        else {
            panic!("expected interleaved literal and checked-expression segments");
        };
        assert_eq!(prefix, " \t");
        assert_eq!(middle, "\n");
        assert_eq!(suffix, " é ");
        assert_eq!(regions.descriptor().source, " regions ");
        assert_eq!(
            regions.result_type(),
            ResultType::List(Box::new(ResultType::String))
        );
        assert_eq!(quoted.descriptor().source, "'${{not_recursive}}'");
        assert_eq!(quoted.result_type(), ResultType::String);
    }

    #[test]
    fn schema_fingerprint_is_retained_even_without_expressions() {
        let env = env();
        let other = TypeEnv::new(&SchemaDef {
            types: vec![],
            roots: BTreeMap::new(),
        })
        .unwrap();
        for source in ["", "literal", "${{true}}"] {
            let checked = check_template(source, &env).unwrap();
            assert_eq!(checked.schema_fingerprint, env.fingerprint());
            assert_ne!(checked.schema_fingerprint, other.fingerprint());
        }
    }

    #[test]
    fn maximum_expression_count_retains_all_interleaved_literals() {
        let checked = check_template(&("x${{true}}".repeat(64) + "z"), &env()).unwrap();
        assert_eq!(checked.segments.len(), 129);
        assert!(matches!(checked.segments.last(), Some(Segment::Literal(value)) if value == "z"));
    }
}
