use super::*;
use sites::{Expected, SourceValue};

/// Checked value tree borrowed from a sealed compiled definition. Template indices
/// resolve only against that definition; constructing a tree does not admit it.
#[derive(Debug)]
pub enum CheckedValue {
    Literal(Value),
    Template(usize),
    List(Vec<CheckedValue>),
    Object(BTreeMap<String, CheckedValue>),
}
#[derive(Debug)]
pub(super) struct Site {
    pub path: String,
    pub expected: Expected,
    pub scope: usize,
    pub source: String,
    pub template: CheckedTemplate,
}

#[cfg(test)]
thread_local! { pub(super) static CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
pub(super) fn check_template(source: &str, env: &TypeEnv, path: &str) -> Result<CheckedTemplate> {
    #[cfg(test)]
    CHECKS.with(|n| n.set(n.get() + 1));
    runbook_expr::check_template(source, env).map_err(|e| expr_error(e, path))
}

pub(super) struct ValueChecker<'a> {
    pub arena: &'a mut Arena,
    pub env: &'a TypeEnv,
    pub sites: &'a mut Vec<Site>,
    pub scope: usize,
    /// Scratch programs must die before the next inference stage.
    pub retain: bool,
}
impl ValueChecker<'_> {
    pub fn value(
        &mut self,
        value: SourceValue<'_>,
        path: &str,
        expected: Expected,
        destination: Option<&FieldDecl>,
        schema: Option<&DataSchema>,
        infer: bool,
    ) -> Result<(CheckedValue, Option<FieldDecl>)> {
        if matches!(value, SourceValue::Json(Value::Null))
            && destination.is_some_and(|d| d.nullable)
        {
            return Ok((CheckedValue::Literal(Value::Null), destination.cloned()));
        }
        let needs_type = infer || destination.is_some() || expected != Expected::Any;
        let has_templates = match value {
            SourceValue::Text(s) => s.contains("${{"),
            SourceValue::Json(v) => contains_templates(v),
        };
        if let (Some(schema), SourceValue::Json(v)) = (schema, value)
            && !has_templates
            && let Some(e) = crate::validate_data(schema, v).first()
        {
            return Err(error(&e.code, path));
        }
        let value = match value {
            SourceValue::Json(Value::String(s)) => SourceValue::Text(s),
            other => other,
        };
        let (checked, ty) = match value {
            SourceValue::Text(s) if s.contains("${{") => {
                let template = check_template(s, self.env, path)?;
                let result = if template.mode() == TemplateMode::Text {
                    ResultType::String
                } else {
                    template
                        .expressions()
                        .next()
                        .unwrap()
                        .expression
                        .result_type()
                };
                if matches!(
                    expected,
                    Expected::Bool | Expected::List | Expected::Timestamp
                ) && template.mode() != TemplateMode::WholeValue
                {
                    return Err(error("expression.type_error", path));
                }
                let ty = if result == ResultType::Null && destination.is_some_and(|d| d.nullable) {
                    destination.cloned()
                } else if needs_type {
                    Some(self.arena.result(&result, self.env, path)?)
                } else {
                    None
                };
                let id = self.sites.len();
                if self.retain {
                    self.sites.push(Site {
                        path: path.into(),
                        expected,
                        scope: self.scope,
                        source: s.into(),
                        template,
                    });
                }
                (CheckedValue::Template(id), ty)
            }
            SourceValue::Text(s) => {
                if matches!(expected, Expected::Bool | Expected::List) {
                    return Err(error("expression.type_error", path));
                }
                let primitive = if expected == Expected::Timestamp {
                    // Reuse the adopted timestamp literal validator. No evaluation,
                    // no persisted literal-only program, no second RFC3339 parser.
                    let quoted = serde_json::to_string(s).expect("string");
                    runbook_expr::check_expression(&format!("timestamp({quoted})"), self.env)
                        .map_err(|e| expr_error(e, path))?;
                    TypeDef::Timestamp
                } else {
                    TypeDef::String
                };
                (
                    CheckedValue::Literal(Value::String(s.into())),
                    if needs_type {
                        Some(field(self.arena.intern(primitive, path)?, true))
                    } else {
                        None
                    },
                )
            }
            SourceValue::Json(Value::Array(values)) => {
                let item_schema = match schema.map(|s| &s.kind) {
                    Some(DataType::List { items }) => Some(items.as_ref()),
                    _ => None,
                };
                let item_destination =
                    destination.and_then(|d| match self.arena.types[d.ty.0 as usize] {
                        TypeDef::List(id) => Some(field(id, true)),
                        _ => None,
                    });
                if needs_type && values.is_empty() && item_destination.is_none() {
                    return Err(types::uninferable(path));
                }
                let mut out = Vec::new();
                let mut element = item_destination.clone();
                for (i, v) in values.iter().enumerate() {
                    let (value, ty) = self.value(
                        SourceValue::Json(v),
                        &pointer(path, &i.to_string()),
                        Expected::Any,
                        item_destination.as_ref(),
                        item_schema,
                        needs_type,
                    )?;
                    // A known item destination has already checked this child
                    // one way. Keep that common shape instead of narrowing it
                    // to one element's present/omitted optional members.
                    if let Some(ty) = ty
                        && item_destination.is_none()
                    {
                        // TypeDef::List cannot represent nullable elements.
                        // Never turn a nullable value into a non-null promise.
                        if ty.nullable {
                            return Err(types::uninferable(path));
                        }
                        if element.as_ref().is_some_and(|e| {
                            !self.arena.compatible(&ty, e) || !self.arena.compatible(e, &ty)
                        }) {
                            return Err(error("expression.type_error", path));
                        }
                        element = Some(ty);
                    }
                    out.push(value);
                }
                let ty = if needs_type {
                    Some(field(
                        self.arena
                            .intern(TypeDef::List(element.unwrap().ty), path)?,
                        true,
                    ))
                } else {
                    None
                };
                (CheckedValue::List(out), ty)
            }
            SourceValue::Json(Value::Object(values)) => {
                let mut out = BTreeMap::new();
                let mut fields = BTreeMap::new();
                let properties = match schema.map(|s| &s.kind) {
                    Some(DataType::Object { properties }) => Some(properties),
                    _ => None,
                };
                let declarations =
                    destination.and_then(|d| match &self.arena.types[d.ty.0 as usize] {
                        TypeDef::Record(fields) => Some(fields.clone()),
                        _ => None,
                    });
                if needs_type && values.len() > 256 {
                    return Err(error("expression.schema_limit", path));
                }
                for (name, v) in values {
                    let p = pointer(path, name);
                    if declarations.as_ref().is_some_and(|d| !d.contains_key(name)) {
                        return Err(error("data.unknown_field", &p));
                    }
                    let (value, ty) = self.value(
                        SourceValue::Json(v),
                        &p,
                        Expected::Any,
                        declarations.as_ref().and_then(|d| d.get(name)),
                        properties.and_then(|p| p.get(name)),
                        needs_type,
                    )?;
                    out.insert(name.clone(), value);
                    if let Some(ty) = ty {
                        fields.insert(name.clone(), ty);
                    }
                }
                let ty = if needs_type {
                    Some(field(self.arena.record(fields, path)?, true))
                } else {
                    None
                };
                (CheckedValue::Object(out), ty)
            }
            SourceValue::Json(v) => {
                let ty = if needs_type {
                    let primitive = match v {
                        Value::Bool(_) => TypeDef::Bool,
                        Value::Null => return Err(types::uninferable(path)),
                        Value::Number(n) => match schema.map(|s| &s.kind) {
                            Some(DataType::Integer) => {
                                if crate::schema::integer_value(v).is_none() {
                                    return Err(error("data.type_error", path));
                                }
                                TypeDef::Int
                            }
                            Some(DataType::Number) => TypeDef::Double,
                            _ => {
                                if n.as_str().contains(['.', 'e', 'E']) {
                                    if !n.as_f64().is_some_and(f64::is_finite) {
                                        return Err(error("data.type_error", path));
                                    }
                                    TypeDef::Double
                                } else {
                                    if crate::schema::integer_value(v).is_none() {
                                        return Err(error("data.type_error", path));
                                    }
                                    TypeDef::Int
                                }
                            }
                        },
                        _ => unreachable!(),
                    };
                    Some(field(self.arena.intern(primitive, path)?, true))
                } else {
                    None
                };
                (CheckedValue::Literal(v.clone()), ty)
            }
        };
        if let Some(ty) = &ty {
            let matches = match expected {
                Expected::String => {
                    matches!(self.arena.types[ty.ty.0 as usize], TypeDef::String) && !ty.nullable
                }
                Expected::Bool => {
                    matches!(self.arena.types[ty.ty.0 as usize], TypeDef::Bool) && !ty.nullable
                }
                Expected::List => {
                    matches!(self.arena.types[ty.ty.0 as usize], TypeDef::List(_)) && !ty.nullable
                }
                Expected::Timestamp => {
                    matches!(self.arena.types[ty.ty.0 as usize], TypeDef::Timestamp) && !ty.nullable
                }
                _ => true,
            };
            if !matches || destination.is_some_and(|d| !self.arena.compatible(ty, d)) {
                return Err(error("expression.type_error", path));
            }
        }
        Ok((checked, ty))
    }
}
pub(super) fn contains_templates(v: &Value) -> bool {
    match v {
        Value::String(s) => s.contains("${{"),
        Value::Array(v) => v.iter().any(contains_templates),
        Value::Object(v) => v.values().any(contains_templates),
        _ => false,
    }
}
