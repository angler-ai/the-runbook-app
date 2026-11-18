use crate::{
    COST_MODEL, ExprDiagnostic, PROFILE, TypeEnv, TypeId, profile, schema::ClosedResolver,
};
use cel_core::parser::{Token, lex_bounded, parse_bounded};
use cel_core::types::{Expr, SpannedExpr};
use cel_core::{
    Ast, CelType, CheckErrorKind, Env, MessageValue, Program, ProtoRegistry, ProtoTypeResolver,
    ResolvedProtoType, StructFieldValue, Timestamp, Value,
};
use serde::{Deserialize, Serialize};
use std::{any::Any, collections::BTreeMap, ops::Range, sync::Arc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultType {
    Null,
    Bool,
    Int,
    Double,
    String,
    Timestamp,
    List(Box<ResultType>),
    Nullable(Box<ResultType>),
    Record {
        type_id: TypeId,
        schema_fingerprint: [u8; 32],
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpressionDescriptor {
    pub source: String,
    pub profile: String,
    pub cost_model: String,
    pub schema_fingerprint: [u8; 32],
}

#[derive(Debug, Clone)]
pub struct TypedExpression {
    descriptor: ExpressionDescriptor,
    result_type: ResultType,
    static_dependencies: Vec<crate::StaticDependency>,
    // Runtime consumes this immutable checked program and substitutes literal calls
    // from this per-node cache, charging their original expression dispatch.
    pub(crate) program: Program,
    pub(crate) timestamp_literals: BTreeMap<i64, Timestamp>,
}
impl TypedExpression {
    pub fn static_dependencies(&self) -> &[crate::StaticDependency] {
        &self.static_dependencies
    }
    pub fn descriptor(&self) -> ExpressionDescriptor {
        self.descriptor.clone()
    }
    pub fn result_type(&self) -> ResultType {
        self.result_type.clone()
    }
    pub(crate) fn schema_fingerprint(&self) -> [u8; 32] {
        self.descriptor.schema_fingerprint
    }
}
pub(crate) fn diagnostic(code: &'static str, span: Range<usize>, message: &str) -> ExprDiagnostic {
    ExprDiagnostic {
        code,
        span,
        message: message.into(),
    }
}

fn safe_span(source: &str, span: Range<usize>) -> Range<usize> {
    let mut start = span.start.min(source.len());
    let mut end = span.end.min(source.len()).max(start);
    while !source.is_char_boundary(start) {
        start -= 1;
    }
    while !source.is_char_boundary(end) {
        end += 1;
    }
    start..end
}

fn limit_error(source: &str, error: cel_core::CompileLimitError) -> ExprDiagnostic {
    let code = if error.kind == cel_core::CompileLimitKind::MacroCount {
        "expression.unsupported"
    } else {
        "expression.compile_limit"
    };
    diagnostic(
        code,
        safe_span(source, error.span),
        "Expression exceeds the supported compilation envelope",
    )
}

fn validate_source(source: &str) -> Result<(), ExprDiagnostic> {
    let tokens = lex_bounded(source)
        .map_err(|e| limit_error(source, e))?
        .map_err(|e| {
            diagnostic(
                "expression.syntax_error",
                safe_span(source, e.span),
                "Invalid CEL token or literal",
            )
        })?;
    let mut previous: Option<&Token> = None;
    for (token, span) in &tokens {
        let spelling = &source[span.clone()];
        if matches!(token, Token::Ident(name) if name.len() > 128) {
            return Err(diagnostic(
                "expression.compile_limit",
                span.clone(),
                "Identifier exceeds 128 bytes",
            ));
        }
        let unsupported = match token {
            Token::UInt(_)
            | Token::Bytes(_)
            | Token::In
            | Token::Reserved(_)
            | Token::Plus
            | Token::Star
            | Token::Slash
            | Token::Percent
            | Token::LBrace
            | Token::RBrace => true,
            Token::String(_) => {
                spelling.starts_with(['r', 'R'])
                    || spelling.starts_with("\"\"\"")
                    || spelling.starts_with("'''")
            }
            Token::Ident(_) => spelling.starts_with('`'),
            Token::Float(value) => !value.is_finite(),
            Token::Question => matches!(previous, Some(Token::Dot | Token::LBracket)),
            _ => false,
        };
        if unsupported {
            return Err(diagnostic(
                "expression.unsupported",
                span.clone(),
                "Token is outside runbook-cel/1",
            ));
        }
        previous = Some(token);
    }
    Ok(())
}

pub(crate) fn parse_expression(source: &str) -> Result<SpannedExpr, ExprDiagnostic> {
    validate_source(source)?;
    // Outer Ok is not parser success: recovered syntax errors must never reach
    // the recursive checker, even when a partial AST is available.
    let parsed = parse_bounded(source, &cel_core::CompileLimits::runbook_v1())
        .map_err(|e| limit_error(source, e))?;
    if let Some(error) = parsed.errors.first() {
        return Err(diagnostic(
            "expression.syntax_error",
            safe_span(source, error.span.clone()),
            "Invalid CEL expression syntax",
        ));
    }
    let expr = parsed.ast.ok_or_else(|| {
        diagnostic(
            "expression.syntax_error",
            0..source.len(),
            "Expected an expression",
        )
    })?;
    Ok(expr)
}

pub fn check_expression(source: &str, env: &TypeEnv) -> Result<TypedExpression, ExprDiagnostic> {
    let expr = parse_expression(source)?;
    let nodes = profile::validate_parsed(&expr)?;
    let mut checker =
        Env::new().with_proto_registry(Arc::new(SealedProtoRegistry(env.closed_resolver())));
    for var in env.root_declarations() {
        checker.add_variable(var.name, var.cel_type);
    }
    for function in profile::declarations() {
        checker.add_function(function);
    }
    #[cfg(test)]
    CHECKER_ENTRIES.with(|entries| entries.set(entries.get() + 1));
    let checked = checker.check(&expr);
    profile::validate_operations(&nodes, &checked)?;
    if !checked.errors.is_empty() {
        // Public API returns one bounded diagnostic. Aggregate at most sixteen
        // static structured summaries; never format arbitrary CEL types/values.
        let mut message = String::new();
        for error in checked.errors.iter().take(16) {
            let summary = match &error.kind {
                CheckErrorKind::UndeclaredReference { .. } => "Undeclared identifier",
                CheckErrorKind::UndefinedField { .. } => "Undeclared record field",
                CheckErrorKind::NoMatchingOverload { .. } => "No matching typed overload",
                CheckErrorKind::TypeMismatch { .. } | CheckErrorKind::NotAssignable { .. } => {
                    "Incompatible expression types"
                }
                _ => "Invalid expression type",
            };
            if !message.is_empty() {
                message.push_str("; ");
            }
            message.push_str(summary);
        }
        return Err(ExprDiagnostic {
            code: "expression.type_error",
            span: safe_span(source, checked.errors[0].span.clone()),
            message,
        });
    }
    let result_type = profile::validate_checked(&nodes, &checked, env)?;
    let mut timestamp_literals = BTreeMap::new();
    for node in nodes {
        if let Expr::Call { expr, args } = &node.node
            && matches!(&expr.node, Expr::Ident(name) if name == "timestamp")
        {
            let Expr::String(literal) = &args[0].node else {
                unreachable!("validated literal timestamp")
            };
            let value = cel_core::time::parse_timestamp(literal).map_err(|_| {
                diagnostic(
                    "expression.type_error",
                    node.span.clone(),
                    "Invalid timestamp literal: expected UTC years 0001–9999 and valid nanoseconds",
                )
            })?;
            if !(Timestamp::MIN_SECONDS..=Timestamp::MAX_SECONDS).contains(&value.seconds)
                || !(0..1_000_000_000).contains(&value.nanos)
            {
                return Err(diagnostic(
                    "expression.type_error",
                    node.span.clone(),
                    "Timestamp is outside UTC years 0001–9999 or nanosecond range",
                ));
            }
            timestamp_literals.insert(node.id, value);
        }
    }
    let static_dependencies = crate::metadata::dependencies(&expr);
    let ast = Ast::new_checked(expr, source, checked);
    let program = checker.program(&ast).map_err(|_| {
        diagnostic(
            "expression.unsupported",
            0..source.len(),
            "Unable to construct checked program",
        )
    })?;
    Ok(TypedExpression {
        descriptor: ExpressionDescriptor {
            source: source.into(),
            profile: PROFILE.into(),
            cost_model: COST_MODEL.into(),
            schema_fingerprint: env.fingerprint(),
        },
        result_type,
        static_dependencies,
        program,
        timestamp_literals,
    })
}
pub fn restore_expression(
    desc: &ExpressionDescriptor,
    env: &TypeEnv,
) -> Result<TypedExpression, ExprDiagnostic> {
    if desc.profile != PROFILE
        || desc.cost_model != COST_MODEL
        || desc.schema_fingerprint != env.fingerprint()
    {
        return Err(diagnostic(
            "expression.unsupported",
            0..0,
            "Descriptor profile, cost model or schema fingerprint does not match",
        ));
    }
    check_expression(&desc.source, env)
}

/// The broader schema resolver is deliberately not exposed to the checker:
/// qualified/type/enum resolution always fails under the audited envelope.
#[derive(Debug)]
struct SealedProtoRegistry(Arc<ClosedResolver>);
impl ProtoTypeResolver for SealedProtoRegistry {
    fn get_field_type(&self, message: &str, field: &str) -> Option<CelType> {
        self.0.get_field_type(message, field)
    }
    fn has_message(&self, message: &str) -> bool {
        self.0.has_message(message)
    }
    fn is_extension(&self, _: &str, _: &str) -> bool {
        false
    }
    fn get_enum_value(&self, _: &str, _: &str) -> Option<i32> {
        None
    }
    fn resolve_qualified(&self, _: &[&str], _: &str) -> Option<ResolvedProtoType> {
        None
    }
    fn resolve_message_name(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn message_field_names(&self, message: &str) -> Option<Vec<String>> {
        self.0.message_field_names(message)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
impl ProtoRegistry for SealedProtoRegistry {
    fn construct_message(&self, _: &str, _: &[StructFieldValue], _: bool) -> Value {
        Value::error("Message construction is unsupported")
    }
    fn message_field_access(&self, _: &dyn MessageValue, _: &str, _: bool, _: bool) -> Value {
        Value::error("Runtime field adapter is not installed")
    }
    fn message_has_field(&self, _: &dyn MessageValue, _: &str) -> Value {
        Value::error("Presence tests are unsupported")
    }
    fn get_extension_value(
        &self,
        _: &dyn MessageValue,
        _: &str,
        _: bool,
        _: bool,
    ) -> Option<Value> {
        None
    }
    fn has_extension(&self, _: &dyn MessageValue, _: &str) -> Option<bool> {
        None
    }
}

#[cfg(test)]
thread_local! { static CHECKER_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldDecl, SchemaDef, TypeDef};

    fn empty() -> TypeEnv {
        TypeEnv::new(&SchemaDef {
            types: vec![],
            roots: BTreeMap::new(),
        })
        .unwrap()
    }
    fn balanced(n: usize) -> String {
        if n == 1 {
            "true".into()
        } else {
            format!("({} && {})", balanced(n / 2), balanced(n - n / 2))
        }
    }

    // Rebuilding a resolver for each real compilation recreates every record
    // field map and makes retained programs multiply schema storage.
    #[test]
    fn repeated_checks_share_one_closed_resolver_build() {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![
                TypeDef::Int,
                TypeDef::Record(BTreeMap::from([(
                    "value".into(),
                    FieldDecl {
                        ty: TypeId(0),
                        required: true,
                        nullable: false,
                    },
                )])),
            ],
            roots: BTreeMap::from([(
                "record".into(),
                FieldDecl {
                    ty: TypeId(1),
                    required: true,
                    nullable: false,
                },
            )]),
        })
        .unwrap();
        crate::schema::RESOLVER_BUILDS.with(|builds| builds.set(0));
        let first = check_expression("record.value == 1", &env).unwrap();
        let second = check_expression("record.value > 0", &env).unwrap();
        assert_eq!(first.result_type(), ResultType::Bool);
        assert_eq!(second.result_type(), ResultType::Bool);
        crate::schema::RESOLVER_BUILDS.with(|builds| assert_eq!(builds.get(), 1));
        let resolver = env.closed_resolver();
        let owner = Arc::downgrade(&resolver);
        let cloned = env.clone();
        assert!(Arc::ptr_eq(&resolver, &cloned.closed_resolver()));
        let third = check_expression("record.value <= 2", &cloned).unwrap();
        crate::schema::RESOLVER_BUILDS.with(|builds| assert_eq!(builds.get(), 1));
        drop(resolver);
        drop(env);
        drop(cloned);
        assert!(
            owner.upgrade().is_some(),
            "programs retain the shared resolver"
        );
        drop((first, second, third));
        assert!(
            owner.upgrade().is_none(),
            "the cache introduces no ownership cycle"
        );
    }

    #[test]
    fn resolver_cache_isolated_from_other_schemas_and_caller_mutation() {
        let field = |ty| FieldDecl {
            ty: TypeId(ty),
            required: true,
            nullable: false,
        };
        let mut source = SchemaDef {
            types: vec![
                TypeDef::Int,
                TypeDef::Record(BTreeMap::from([("value".into(), field(0))])),
            ],
            roots: BTreeMap::from([("record".into(), field(1))]),
        };
        let integers = TypeEnv::new(&source).unwrap();
        let original_fingerprint = integers.fingerprint();
        // The cache is still uninitialized: it must read the accepted snapshot,
        // not the caller's now-mutated schema or another environment's cache.
        source.types[0] = TypeDef::String;
        let strings = TypeEnv::new(&source).unwrap();
        source.types[0] = TypeDef::Bool;
        assert_eq!(
            check_expression("record.value > 1", &integers)
                .unwrap()
                .result_type(),
            ResultType::Bool
        );
        assert_eq!(
            check_expression("record.value == 'text'", &strings)
                .unwrap()
                .result_type(),
            ResultType::Bool
        );
        assert_eq!(
            check_expression("record.value > 1", &strings)
                .unwrap_err()
                .code,
            "expression.type_error"
        );
        assert_eq!(integers.fingerprint(), original_fingerprint);
        assert_ne!(integers.fingerprint(), strings.fingerprint());
        assert!(!Arc::ptr_eq(
            &integers.closed_resolver(),
            &strings.closed_resolver()
        ));
    }

    #[test]
    fn syntax_summary_never_enters_real_checker() {
        CHECKER_ENTRIES.with(|entries| entries.set(0));
        for source in [
            "${{ unknown.value }}",
            "${{ true > 'wrong' }}",
            "${{ xs.exists(x, x > y) }}",
        ] {
            assert!(crate::summarize_template_syntax(source).is_ok());
        }
        assert!(crate::summarize_template_syntax("${{ 1 + 1 }}").is_err());
        CHECKER_ENTRIES.with(|entries| assert_eq!(entries.get(), 0));
        assert!(check_expression("true", &empty()).is_ok());
        CHECKER_ENTRIES.with(|entries| assert_eq!(entries.get(), 1));
    }

    #[test]
    fn rejected_envelopes_and_partial_syntax_never_enter_real_checker() {
        let wide = format!("{} && {}", balanced(64), balanced(65));
        assert_eq!(lex_bounded(&wide).unwrap().unwrap().len(), 511);
        for source in [&wide, &format!("{}true", "!".repeat(33))] {
            let parsed = parse_bounded(source, &cel_core::CompileLimits::runbook_v1()).unwrap();
            assert!(parsed.is_ok());
            assert_eq!(
                profile::validate_parsed(parsed.ast.as_ref().unwrap())
                    .unwrap_err()
                    .code,
                "expression.compile_limit"
            );
        }
        for source in [
            format!("{}true", "!".repeat(33)),
            wide,
            "true &&".into(),
            "now(1)".into(),
            "f()".into(),
            "[1]".into(),
        ] {
            CHECKER_ENTRIES.with(|entries| entries.set(0));
            assert!(check_expression(&source, &empty()).is_err());
            CHECKER_ENTRIES.with(|entries| assert_eq!(entries.get(), 0, "{source}"));
        }
        CHECKER_ENTRIES.with(|entries| entries.set(0));
        assert!(check_expression("true", &empty()).is_ok());
        CHECKER_ENTRIES.with(|entries| assert_eq!(entries.get(), 1));
        assert_eq!(
            check_expression("true ? 1 : 'bad'", &empty())
                .unwrap_err()
                .code,
            "expression.unsupported"
        );
        CHECKER_ENTRIES.with(|entries| assert_eq!(entries.get(), 2));
    }

    #[test]
    fn sealed_registry_only_resolves_declared_fields() {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![
                TypeDef::Int,
                TypeDef::Record(BTreeMap::from([(
                    "value".into(),
                    FieldDecl {
                        ty: TypeId(0),
                        required: true,
                        nullable: false,
                    },
                )])),
            ],
            roots: BTreeMap::new(),
        })
        .unwrap();
        let registry = SealedProtoRegistry(env.closed_resolver());
        assert_eq!(
            registry.get_field_type("runbook_record_1", "value"),
            Some(CelType::Int)
        );
        assert_eq!(registry.get_field_type("runbook_record_1", "missing"), None);
        assert_eq!(registry.get_field_type("unknown", "value"), None);
        assert!(
            registry
                .resolve_qualified(&["runbook_record_1"], "")
                .is_none()
        );
        assert!(
            registry
                .resolve_message_name("runbook_record_1", "")
                .is_none()
        );
        assert!(
            registry
                .get_enum_value("runbook_record_1", "value")
                .is_none()
        );
        assert!(!registry.is_extension("runbook_record_1", "value"));
        assert!(
            registry
                .construct_message("runbook_record_1", &[], true)
                .is_error()
        );
    }

    #[test]
    fn timestamp_literals_are_cached_as_immutable_validated_values() {
        let expression =
            check_expression("timestamp('2000-01-01T01:00:00.123400+01:00')", &empty()).unwrap();
        assert_eq!(expression.timestamp_literals.len(), 1);
        let timestamp = expression.timestamp_literals.values().next().unwrap();
        assert_eq!(timestamp.seconds, 946684800);
        assert_eq!(timestamp.nanos, 123400000);
        assert_eq!(
            cel_core::time::format_timestamp(timestamp),
            "2000-01-01T00:00:00.1234Z"
        );
        assert!(expression.program.ast().is_checked());
    }
}
