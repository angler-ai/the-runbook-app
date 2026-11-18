use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

use sha2::{Digest, Sha256};

use crate::{ExprDiagnostic, limits};

/// An index into the closed type arena, resolved only by `TypeEnv::new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDef {
    Bool,
    Int,
    Double,
    String,
    Timestamp,
    List(TypeId),
    Record(BTreeMap<String, FieldDecl>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub ty: TypeId,
    pub nullable: bool,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaDef {
    pub types: Vec<TypeDef>,
    pub roots: BTreeMap<String, FieldDecl>,
}

/// A validated, immutable, acyclic schema. Unvalidated descriptions are borrowed
/// so rejecting a large input never copies or destroys its unvisited remainder.
#[derive(Debug, Clone)]
pub struct TypeEnv {
    def: SchemaDef,
    fingerprint: [u8; 32],
    resolver: OnceLock<Arc<ClosedResolver>>,
}

impl TypeEnv {
    pub fn new(def: &SchemaDef) -> Result<Self, ExprDiagnostic> {
        validate(def)?;
        Ok(Self {
            fingerprint: fingerprint(def),
            def: def.clone(),
            resolver: OnceLock::new(),
        })
    }

    /// Borrow the sealed root declarations for type-directed runtime binding.
    pub fn binding_roots(&self) -> &BTreeMap<String, FieldDecl> {
        &self.def.roots
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    pub fn type_definition(&self, id: TypeId) -> Option<&TypeDef> {
        self.def.types.get(id.0 as usize)
    }
}

fn error(code: &'static str, message: &'static str) -> ExprDiagnostic {
    ExprDiagnostic {
        code,
        span: 0..0,
        message: message.into(),
    }
}

fn limit(message: &'static str) -> ExprDiagnostic {
    error("expression.schema_limit", message)
}

fn invalid(message: &'static str) -> ExprDiagnostic {
    error("expression.schema_error", message)
}

pub(crate) fn validate_name(name: &str) -> Result<(), ExprDiagnostic> {
    // Check the byte bound before scanning any caller-controlled name.
    if name.len() > limits::IDENTIFIER_BYTES {
        return Err(limit("Schema identifier exceeds 128 bytes"));
    }
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        || matches!(
            name,
            "true"
                | "false"
                | "null"
                | "in"
                | "as"
                | "break"
                | "const"
                | "continue"
                | "else"
                | "for"
                | "function"
                | "if"
                | "import"
                | "let"
                | "loop"
                | "package"
                | "namespace"
                | "return"
                | "var"
                | "void"
                | "while"
        )
    {
        return Err(invalid("Schema name must be an ASCII CEL identifier"));
    }
    Ok(())
}

fn validate_decl(decl: &FieldDecl, def: &SchemaDef) -> Result<(), ExprDiagnostic> {
    let ty = def
        .types
        .get(decl.ty.0 as usize)
        .ok_or_else(|| invalid("Schema declaration references an unknown type"))?;
    if decl.nullable && matches!(ty, TypeDef::List(_)) {
        return Err(error(
            "expression.unsupported",
            "Nullable lists are unsupported in this profile",
        ));
    }
    Ok(())
}

fn validate(def: &SchemaDef) -> Result<(), ExprDiagnostic> {
    // Constant-time cardinality gates precede scans, graph storage and copying.
    if def.types.len() > limits::TYPE_NODES {
        return Err(limit("Schema exceeds 1,024 type nodes"));
    }
    if def.roots.len() > limits::ROOT_VARIABLES {
        return Err(limit("Schema exceeds 256 root variables"));
    }
    let mut fields = def.roots.len();
    for ty in &def.types {
        if let TypeDef::Record(record) = ty {
            if record.len() > limits::RECORD_FIELDS {
                return Err(limit("Schema record exceeds 256 fields"));
            }
            fields = fields
                .checked_add(record.len())
                .filter(|&n| n <= limits::DECLARED_FIELDS)
                .ok_or_else(|| limit("Schema exceeds 4,096 declared fields"))?;
        }
    }
    for (name, decl) in &def.roots {
        validate_name(name)?;
        validate_decl(decl, def)?;
    }
    let mut edges = Vec::with_capacity(def.types.len());
    for ty in &def.types {
        let children = match ty {
            TypeDef::List(child) => {
                if child.0 as usize >= def.types.len() {
                    return Err(invalid("Schema list references an unknown type"));
                }
                vec![child.0 as usize]
            }
            TypeDef::Record(record) => {
                let mut children = Vec::with_capacity(record.len());
                for (name, decl) in record {
                    validate_name(name)?;
                    validate_decl(decl, def)?;
                    children.push(decl.ty.0 as usize);
                }
                children
            }
            _ => Vec::new(),
        };
        edges.push(children);
    }

    // Three-color iterative DFS validates every arena node, including unused
    // types. Memoized subtree heights still catch longer paths through shared
    // subgraphs. Both traversal storage and partial cleanup are flat and bounded.
    let mut colors = vec![0_u8; def.types.len()];
    let mut heights = vec![0_usize; def.types.len()];
    let mut stack = Vec::with_capacity(def.types.len());
    for root in 0..def.types.len() {
        if colors[root] != 0 {
            continue;
        }
        colors[root] = 1;
        stack.push((root, 0));
        while let Some((node, cursor)) = stack.last_mut() {
            if let Some(&child) = edges[*node].get(*cursor) {
                *cursor += 1;
                match colors[child] {
                    0 => {
                        colors[child] = 1;
                        stack.push((child, 0));
                    }
                    1 => return Err(invalid("Schema type graph contains a cycle")),
                    _ => {}
                }
            } else {
                let node = *node;
                let height = edges[node]
                    .iter()
                    .map(|&child| heights[child] + 1)
                    .max()
                    .unwrap_or(0);
                if height > limits::TYPE_DEPTH {
                    return Err(limit("Schema type graph exceeds depth 16"));
                }
                heights[node] = height;
                colors[node] = 2;
                stack.pop();
            }
        }
    }
    Ok(())
}

fn fingerprint(def: &SchemaDef) -> [u8; 32] {
    // Versioned, length-prefixed binary encoding: arena order defines TypeIds;
    // BTreeMap iteration defines field/root order. No JSON/debug formatting or
    // temporary serialization buffer is involved. IDs and lengths are LE u32.
    fn number(hash: &mut Sha256, value: usize) {
        hash.update((value as u32).to_le_bytes());
    }
    fn decls(hash: &mut Sha256, declarations: &BTreeMap<String, FieldDecl>) {
        number(hash, declarations.len());
        for (name, decl) in declarations {
            number(hash, name.len());
            hash.update(name.as_bytes());
            hash.update(decl.ty.0.to_le_bytes());
            hash.update([u8::from(decl.nullable), u8::from(decl.required)]);
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"runbook-schema/1\0");
    number(&mut hash, def.types.len());
    for ty in &def.types {
        match ty {
            TypeDef::Bool => hash.update([0]),
            TypeDef::Int => hash.update([1]),
            TypeDef::Double => hash.update([2]),
            TypeDef::String => hash.update([3]),
            TypeDef::Timestamp => hash.update([4]),
            TypeDef::List(child) => {
                hash.update([5]);
                hash.update(child.0.to_le_bytes());
            }
            TypeDef::Record(fields) => {
                hash.update([6]);
                decls(&mut hash, fields);
            }
        }
    }
    decls(&mut hash, &def.roots);
    hash.finalize().into()
}

// Crate-private integration points for the bounded compiler.
impl TypeEnv {
    pub(crate) fn root_declarations(&self) -> Vec<cel_core::VariableDecl> {
        self.def
            .roots
            .iter()
            .map(|(name, decl)| {
                cel_core::VariableDecl::new(name.clone(), self.declaration_type(decl))
            })
            .collect()
    }

    pub(crate) fn closed_resolver(&self) -> Arc<ClosedResolver> {
        // Only the validated immutable definition is captured. Checked programs
        // share its field maps; neither the cache nor resolver points back here.
        Arc::clone(
            self.resolver
                .get_or_init(|| Arc::new(self.build_closed_resolver())),
        )
    }

    fn build_closed_resolver(&self) -> ClosedResolver {
        #[cfg(test)]
        RESOLVER_BUILDS.with(|builds| builds.set(builds.get() + 1));
        let messages = self
            .def
            .types
            .iter()
            .enumerate()
            .filter_map(|(id, ty)| {
                let TypeDef::Record(fields) = ty else {
                    return None;
                };
                Some((
                    record_name(TypeId(id as u32)),
                    fields
                        .iter()
                        .map(|(name, decl)| (name.clone(), self.declaration_type(decl)))
                        .collect(),
                ))
            })
            .collect();
        ClosedResolver { messages }
    }

    fn declaration_type(&self, decl: &FieldDecl) -> cel_core::CelType {
        let ty = self.cel_type(decl.ty);
        if decl.nullable && !matches!(ty, cel_core::CelType::Message(_)) {
            cel_core::CelType::wrapper(ty)
        } else {
            ty
        }
    }

    fn cel_type(&self, id: TypeId) -> cel_core::CelType {
        // Only validated IDs enter here; recursive list conversion has at most
        // 16 edges. Records remain opaque and never expand their fields.
        use cel_core::CelType;
        match &self.def.types[id.0 as usize] {
            TypeDef::Bool => CelType::Bool,
            TypeDef::Int => CelType::Int,
            TypeDef::Double => CelType::Double,
            TypeDef::String => CelType::String,
            TypeDef::Timestamp => CelType::Timestamp,
            TypeDef::List(child) => CelType::list(self.cel_type(*child)),
            TypeDef::Record(_) => CelType::message(&record_name(id)),
        }
    }
}

fn record_name(id: TypeId) -> String {
    format!("runbook_record_{}", id.0)
}

#[derive(Debug)]
pub(crate) struct ClosedResolver {
    messages: BTreeMap<String, BTreeMap<String, cel_core::CelType>>,
}

impl cel_core::ProtoTypeResolver for ClosedResolver {
    fn get_field_type(&self, message: &str, field: &str) -> Option<cel_core::CelType> {
        self.messages.get(message)?.get(field).cloned()
    }
    fn has_message(&self, message: &str) -> bool {
        self.messages.contains_key(message)
    }
    fn is_extension(&self, _message: &str, _ext_name: &str) -> bool {
        false
    }
    fn get_enum_value(&self, _enum_name: &str, _value_name: &str) -> Option<i32> {
        None
    }
    fn resolve_qualified(
        &self,
        parts: &[&str],
        container: &str,
    ) -> Option<cel_core::ResolvedProtoType> {
        if !container.is_empty() || parts.len() != 1 {
            return None;
        }
        let (name, _) = self.messages.get_key_value(parts[0])?;
        Some(cel_core::ResolvedProtoType::Message {
            name: name.clone(),
            cel_type: cel_core::CelType::message(name),
        })
    }
    fn resolve_message_name(&self, name: &str, container: &str) -> Option<String> {
        if !container.is_empty() {
            return None;
        }
        self.messages
            .get_key_value(name)
            .map(|(name, _)| name.clone())
    }
    fn message_field_names(&self, message: &str) -> Option<Vec<String>> {
        Some(self.messages.get(message)?.keys().cloned().collect())
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
thread_local! { pub(crate) static RESOLVER_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests {
    use super::*;
    use cel_core::{ProtoTypeResolver, types::CelType};

    fn nullable(ty: u32) -> FieldDecl {
        FieldDecl {
            ty: TypeId(ty),
            nullable: true,
            required: true,
        }
    }

    #[test]
    fn closed_resolver_preserves_scalar_wrappers_and_opaque_records() {
        let env = TypeEnv::new(&SchemaDef {
            types: vec![
                TypeDef::Bool,
                TypeDef::Int,
                TypeDef::Double,
                TypeDef::String,
                TypeDef::Timestamp,
                TypeDef::Record(BTreeMap::from([("score".into(), nullable(1))])),
                TypeDef::List(TypeId(5)),
            ],
            roots: BTreeMap::from([
                ("bool_value".into(), nullable(0)),
                ("int_value".into(), nullable(1)),
                ("double_value".into(), nullable(2)),
                ("string_value".into(), nullable(3)),
                ("time_value".into(), nullable(4)),
                ("record_value".into(), nullable(5)),
                (
                    "list_value".into(),
                    FieldDecl {
                        ty: TypeId(6),
                        nullable: false,
                        required: true,
                    },
                ),
            ]),
        })
        .unwrap();
        let vars: BTreeMap<_, _> = env
            .root_declarations()
            .into_iter()
            .map(|v| (v.name, v.cel_type))
            .collect();
        assert_eq!(vars.get("int_value"), Some(&CelType::wrapper(CelType::Int)));
        assert_eq!(
            vars.get("bool_value"),
            Some(&CelType::wrapper(CelType::Bool))
        );
        assert_eq!(
            vars.get("double_value"),
            Some(&CelType::wrapper(CelType::Double))
        );
        assert_eq!(
            vars.get("string_value"),
            Some(&CelType::wrapper(CelType::String))
        );
        assert_eq!(
            vars.get("time_value"),
            Some(&CelType::wrapper(CelType::Timestamp))
        );
        let record = vars.get("record_value").unwrap();
        assert!(matches!(record, CelType::Message(_)));
        assert_eq!(vars.get("list_value"), Some(&CelType::list(record.clone())));
        let name = record.message_name().unwrap();
        let resolver = env.closed_resolver();
        assert!(resolver.has_message(name));
        assert_eq!(
            resolver.get_field_type(name, "score"),
            Some(CelType::wrapper(CelType::Int))
        );
        assert_eq!(resolver.get_field_type(name, "undeclared"), None);
        assert!(!resolver.has_message("unknown"));
        assert!(!resolver.is_extension(name, "score"));
        assert_eq!(resolver.get_enum_value(name, "score"), None);
        assert_eq!(
            resolver.message_field_names(name),
            Some(vec!["score".into()])
        );
        assert_eq!(resolver.message_field_names("unknown"), None);
        assert!(matches!(
            resolver.resolve_qualified(&[name], ""),
            Some(cel_core::ResolvedProtoType::Message { .. })
        ));
        assert!(resolver.resolve_qualified(&[name], "external").is_none());
        assert!(resolver.resolve_qualified(&[name, "score"], "").is_none());
        assert!(resolver.resolve_qualified(&["unknown"], "").is_none());
        assert_eq!(resolver.resolve_message_name(name, ""), Some(name.into()));
        assert_eq!(resolver.resolve_message_name(name, "external"), None);
        assert_eq!(resolver.resolve_message_name("unknown", ""), None);
        assert!(resolver.as_any().is::<ClosedResolver>());
    }
}
