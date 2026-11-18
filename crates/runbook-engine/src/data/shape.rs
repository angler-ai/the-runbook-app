//! Borrowed validation of already-evaluated facts against checked result types.
use runbook_expr::{ResultType, TypeDef, TypeEnv, TypeId};
use serde_json::Value;
pub(super) fn result(value: &Value, ty: &ResultType, env: &TypeEnv) -> bool {
    match ty {
        ResultType::Null => value.is_null(),
        ResultType::Nullable(inner) => value.is_null() || result(value, inner, env),
        ResultType::Bool => value.is_boolean(),
        ResultType::Int => runbook_spec::schema::integer_value(value).is_some(),
        ResultType::Double => value.as_f64().is_some_and(f64::is_finite),
        ResultType::String => value.is_string(),
        ResultType::Timestamp => value
            .as_str()
            .is_some_and(|s| s.parse::<runbook_model::Timestamp>().is_ok()),
        ResultType::List(inner) => value
            .as_array()
            .is_some_and(|v| v.iter().all(|v| result(v, inner, env))),
        ResultType::Record { type_id, .. } => declared(value, *type_id, env),
    }
}
fn declared(value: &Value, id: TypeId, env: &TypeEnv) -> bool {
    match env.type_definition(id) {
        Some(TypeDef::Bool) => value.is_boolean(),
        Some(TypeDef::Int) => runbook_spec::schema::integer_value(value).is_some(),
        Some(TypeDef::Double) => value.as_f64().is_some_and(f64::is_finite),
        Some(TypeDef::String) => value.is_string(),
        Some(TypeDef::Timestamp) => value
            .as_str()
            .is_some_and(|s| s.parse::<runbook_model::Timestamp>().is_ok()),
        Some(TypeDef::List(inner)) => value
            .as_array()
            .is_some_and(|v| v.iter().all(|v| declared(v, *inner, env))),
        Some(TypeDef::Record(fields)) => value.as_object().is_some_and(|v| {
            v.keys().all(|k| fields.contains_key(k))
                && fields.iter().all(|(name, field)| match v.get(name) {
                    None => !field.required,
                    Some(value) => {
                        (field.nullable && value.is_null()) || declared(value, field.ty, env)
                    }
                })
        }),
        None => false,
    }
}
