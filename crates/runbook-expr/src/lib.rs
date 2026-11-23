//! Bounded, closed-schema CEL expressions for the runbook backend.
mod bindings;
mod compile;
mod diagnostic;
mod limits;
mod profile;
mod schema;

pub use compile::{
    ExpressionDescriptor, ResultType, TypedExpression, check_expression, restore_expression,
};

pub use diagnostic::ExprDiagnostic;
pub use schema::{FieldDecl, SchemaDef, TypeDef, TypeEnv, TypeId};

pub const PROFILE: &str = "runbook-cel/1";
pub const COST_MODEL: &str = "runbook-cost/1";

pub use bindings::{
    Bindings, EventTime, InputNode, InputValue, IterationIdentity, Origin, RecordIdentity,
};

mod runtime;
pub use runtime::{
    EvalError, EvalSession, EvalStats, EvaluatedValue, ReadKind, ReadRef, evaluate, evaluate_in,
};
mod output;

mod template;
pub use template::{
    CheckedTemplate, ExpressionSyntaxSummary, TemplateExpressionRef, TemplateMode,
    TemplateSyntaxSummary, check_template, render_template, summarize_template_syntax,
    template_requires_string_equality,
};

mod reads;

mod metadata;
pub use metadata::StaticDependency;
