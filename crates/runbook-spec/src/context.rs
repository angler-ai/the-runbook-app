//! Count canonical selected-context JSON directly from borrows before any
//! external declaration data is copied or passed to the scope checker.
use crate::{DataSchema, DataType, DefaultValue, Diagnostic, bindings::Bindings};
use std::collections::BTreeMap;
type Result<T> = std::result::Result<T, Vec<Diagnostic>>;
fn error(code: &str) -> Vec<Diagnostic> {
    vec![Diagnostic {
        code: code.into(),
        message: None,
        path: "/project/vars".into(),
        line: 0,
        column: 0,
    }]
}
struct Budget {
    left: usize,
    declarations: usize,
}
impl Budget {
    fn charge(&mut self, n: usize) -> Result<()> {
        self.left = self
            .left
            .checked_sub(n)
            .ok_or_else(|| error("spec.context_limit"))?;
        Ok(())
    }
    fn string(&mut self, s: &str) -> Result<()> {
        let n = crate::size::quoted_len(s, self.left).map_err(|_| error("spec.context_limit"))?;
        self.charge(n)
    }
    fn object(&mut self, n: usize) -> Result<()> {
        self.charge(2)?;
        self.charge(n)?;
        self.charge(n.saturating_sub(1))
    }
    fn data(&mut self, v: &serde_json::Value) -> Result<()> {
        let n = crate::size::measure(v, self.left, 64).map_err(|_| error("spec.context_limit"))?;
        self.charge(n)
    }
    fn properties(&mut self, p: &BTreeMap<String, DataSchema>, depth: usize) -> Result<()> {
        self.object(p.len())?;
        for (name, schema) in p {
            self.string(name)?;
            self.schema(schema, depth)?;
        }
        Ok(())
    }
    fn schema(&mut self, s: &DataSchema, depth: usize) -> Result<()> {
        self.declarations += 1;
        if self.declarations > 4096 {
            return Err(error("spec.schema_limit"));
        }
        if depth > 64 {
            return Err(error("spec.depth_limit"));
        }
        let extra_kind = usize::from(matches!(
            s.kind,
            DataType::Enum { .. }
                | DataType::MultiEnum { .. }
                | DataType::List { .. }
                | DataType::Object { .. }
        ));
        let count = 2
            + extra_kind
            + s.extensions.len()
            + usize::from(matches!(s.default, DefaultValue::Value(_)))
            + usize::from(s.min.is_some())
            + usize::from(s.max.is_some())
            + usize::from(s.pattern.is_some())
            + usize::from(s.title.is_some())
            + usize::from(s.description.is_some())
            + usize::from(s.indexed.is_some())
            + usize::from(s.locked.is_some());
        self.object(count)?;
        self.string("type")?;
        self.string(match &s.kind {
            DataType::String => "string",
            DataType::Text => "text",
            DataType::Number => "number",
            DataType::Integer => "integer",
            DataType::Boolean => "boolean",
            DataType::Enum { .. } => "enum",
            DataType::MultiEnum { .. } => "multi_enum",
            DataType::List { .. } => "list",
            DataType::Object { .. } => "object",
            DataType::Date => "date",
            DataType::DateTime => "datetime",
            DataType::Duration => "duration",
            DataType::Url => "url",
            DataType::Email => "email",
            DataType::User => "user",
        })?;
        self.string("required")?;
        self.charge(if s.required { 4 } else { 5 })?;
        match &s.kind {
            DataType::Enum { options } | DataType::MultiEnum { options } => {
                self.string("options")?;
                self.charge(2 + options.len().saturating_sub(1))?;
                for s in options {
                    self.string(s)?;
                }
            }
            DataType::List { items } => {
                self.string("items")?;
                self.schema(items, depth + 1)?;
            }
            DataType::Object { properties } => {
                self.string("properties")?;
                self.properties(properties, depth + 1)?;
            }
            _ => (),
        }
        if let Some(pattern) = &s.pattern {
            self.string("pattern")?;
            self.string(pattern)?;
        }
        if let DefaultValue::Value(v) = &s.default {
            self.string("default")?;
            self.data(v)?;
        }
        for (key, n) in [("min", &s.min), ("max", &s.max)] {
            if let Some(n) = n {
                self.string(key)?;
                self.charge(n.as_str().len())?;
            }
        }
        for (key, v) in [("title", &s.title), ("description", &s.description)] {
            if let Some(v) = v {
                self.string(key)?;
                self.string(v)?;
            }
        }
        for (key, v) in [("indexed", s.indexed), ("locked", s.locked)] {
            if let Some(v) = v {
                self.string(key)?;
                self.charge(if v { 4 } else { 5 })?;
            }
        }
        for (key, v) in &s.extensions {
            if !key.starts_with("x-") {
                return Err(error("spec.invalid_schema"));
            }
            self.string(key)?;
            self.data(v)?;
        }
        Ok(())
    }
}
pub(crate) fn admit(pins: &Bindings, project: &DataSchema) -> Result<()> {
    let DataType::Object { properties } = &project.kind else {
        return Err(error("spec.invalid_schema"));
    };
    if project.required
        || !matches!(project.default, DefaultValue::Missing)
        || project.min.is_some()
        || project.max.is_some()
        || project.title.is_some()
        || project.description.is_some()
        || project.indexed.is_some()
        || project.locked.is_some()
        || !project.extensions.is_empty()
    {
        return Err(error("spec.invalid_schema"));
    }
    let mut b = Budget {
        left: crate::compile::MAX_COMPILE_CONTEXT_BYTES,
        declarations: 1,
    };
    b.object(4)?;
    b.string("skills")?;
    b.object(pins.skills.len())?;
    for name in pins.skills.keys() {
        b.string(name)?;
        b.charge(66)?;
    }
    b.string("users")?;
    b.object(pins.users.len())?;
    for (name, id) in &pins.users {
        b.string(name)?;
        b.string(id.as_str())?;
    }
    b.string("teams")?;
    b.object(pins.teams.len())?;
    for (name, id) in &pins.teams {
        b.string(name)?;
        b.string(id.as_str())?;
    }
    b.string("project_variables")?;
    b.properties(properties, 1)
}
