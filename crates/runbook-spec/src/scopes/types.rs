use super::*;

/// Flat interned DAG. Generated wrappers count exactly like declared types.
#[derive(Default)]
pub(super) struct Arena {
    pub types: Vec<TypeDef>,
    fields: usize,
}
impl Arena {
    pub fn from_types(types: Vec<TypeDef>) -> Self {
        let fields = types
            .iter()
            .map(|t| match t {
                TypeDef::Record(f) => f.len(),
                _ => 0,
            })
            .sum();
        Self { types, fields }
    }
    pub fn truncate(&mut self, length: usize) {
        self.types.truncate(length);
        self.fields = self
            .types
            .iter()
            .map(|t| match t {
                TypeDef::Record(f) => f.len(),
                _ => 0,
            })
            .sum();
    }
    pub fn environment(
        &mut self,
        roots: &BTreeMap<String, FieldDecl>,
        path: &str,
    ) -> Result<TypeEnv> {
        // Move the candidate into the borrowed description. TypeEnv makes its
        // single validated copy; do not clone a second full candidate first.
        let def = SchemaDef {
            types: std::mem::take(&mut self.types),
            roots: roots.clone(),
        };
        let env = TypeEnv::new(&def).map_err(|e| expr_error(e, path));
        self.types = def.types;
        env
    }
    pub fn intern(&mut self, ty: TypeDef, path: &str) -> Result<TypeId> {
        if let Some(i) = self.types.iter().position(|existing| existing == &ty) {
            return Ok(TypeId(i as u32));
        }
        let count = if let TypeDef::Record(fields) = &ty {
            fields.len()
        } else {
            0
        };
        if self.types.len() == 1024 || count > 256 || self.fields + count > 4096 {
            return Err(error("expression.schema_limit", path));
        }
        self.fields += count;
        let id = TypeId(self.types.len() as u32);
        self.types.push(ty);
        Ok(id)
    }
    pub fn record(&mut self, fields: BTreeMap<String, FieldDecl>, path: &str) -> Result<TypeId> {
        if fields.len() > 256 {
            return Err(error("expression.schema_limit", path));
        }
        for name in fields.keys() {
            validate_name(name, &pointer(path, name))?;
        }
        self.intern(TypeDef::Record(fields), path)
    }
    pub fn data(&mut self, schema: &DataSchema, path: &str, optional: bool) -> Result<TypeId> {
        self.data_inner(schema, path, optional, true, 0)
    }
    fn data_inner(
        &mut self,
        schema: &DataSchema,
        path: &str,
        optional: bool,
        declarations: bool,
        depth: usize,
    ) -> Result<TypeId> {
        if depth > 16 {
            return Err(error("expression.schema_limit", path));
        }
        let ty = match &schema.kind {
            DataType::String
            | DataType::Text
            | DataType::Enum { .. }
            | DataType::Date
            | DataType::DateTime
            | DataType::Duration
            | DataType::Url
            | DataType::Email
            | DataType::User => TypeDef::String,
            DataType::MultiEnum { .. } => TypeDef::List(self.intern(TypeDef::String, path)?),
            DataType::Number => TypeDef::Double,
            DataType::Integer => TypeDef::Int,
            DataType::Boolean => TypeDef::Bool,
            DataType::List { items } => TypeDef::List(self.data_inner(
                items,
                &pointer(path, "items"),
                false,
                false,
                depth + 1,
            )?),
            DataType::Object { properties } => {
                if properties.len() > 256 {
                    return Err(error("expression.schema_limit", path));
                }
                let mut fields = BTreeMap::new();
                for (name, value) in properties {
                    let p = pointer(
                        &if declarations {
                            path.into()
                        } else {
                            pointer(path, "properties")
                        },
                        name,
                    );
                    validate_name(name, &p)?;
                    fields.insert(
                        name.clone(),
                        field(
                            self.data_inner(value, &p, optional, false, depth + 1)?,
                            value.required && !optional,
                        ),
                    );
                }
                TypeDef::Record(fields)
            }
        };
        self.intern(ty, path)
    }
}
/// Delegate CEL naming policy to the adopted schema validator, with a local path.
pub(super) fn validate_name(name: &str, path: &str) -> Result<()> {
    TypeEnv::new(&SchemaDef {
        types: vec![TypeDef::Bool],
        roots: BTreeMap::from([(name.into(), field(TypeId(0), true))]),
    })
    .map(|_| ())
    .map_err(|e| expr_error(e, path))
}

impl Arena {
    pub fn result(&mut self, result: &ResultType, env: &TypeEnv, path: &str) -> Result<FieldDecl> {
        fn import(
            arena: &mut Arena,
            id: TypeId,
            env: &TypeEnv,
            memo: &mut BTreeMap<TypeId, TypeId>,
            path: &str,
        ) -> Result<TypeId> {
            if let Some(id) = memo.get(&id) {
                return Ok(*id);
            }
            let ty = match env
                .type_definition(id)
                .expect("checked result owns valid type IDs")
            {
                TypeDef::List(child) => TypeDef::List(import(arena, *child, env, memo, path)?),
                TypeDef::Record(fields) => {
                    let mut out = BTreeMap::new();
                    for (name, decl) in fields {
                        out.insert(
                            name.clone(),
                            FieldDecl {
                                ty: import(arena, decl.ty, env, memo, path)?,
                                ..decl.clone()
                            },
                        );
                    }
                    TypeDef::Record(out)
                }
                primitive => primitive.clone(),
            };
            let out = arena.intern(ty, path)?;
            memo.insert(id, out);
            Ok(out)
        }
        let ty = match result {
            ResultType::Null => return Err(uninferable(path)),
            ResultType::Bool => TypeDef::Bool,
            ResultType::Int => TypeDef::Int,
            ResultType::Double => TypeDef::Double,
            ResultType::String => TypeDef::String,
            ResultType::Timestamp => TypeDef::Timestamp,
            ResultType::Nullable(inner) => {
                let mut d = self.result(inner, env, path)?;
                d.nullable = true;
                return Ok(d);
            }
            ResultType::List(inner) => {
                let d = self.result(inner, env, path)?;
                if d.nullable {
                    return Err(uninferable(path));
                }
                TypeDef::List(d.ty)
            }
            ResultType::Record {
                type_id,
                schema_fingerprint,
            } => {
                assert_eq!(*schema_fingerprint, env.fingerprint());
                return Ok(field(
                    import(self, *type_id, env, &mut BTreeMap::new(), path)?,
                    true,
                ));
            }
        };
        Ok(field(self.intern(ty, path)?, true))
    }
    pub fn compatible(&self, actual: &FieldDecl, expected: &FieldDecl) -> bool {
        fn types(arena: &Arena, a: TypeId, b: TypeId, visited: &mut [u64]) -> bool {
            if a == b {
                return true;
            }
            let pair = a.0 as usize * arena.types.len() + b.0 as usize;
            let mask = 1_u64 << (pair % 64);
            if visited[pair / 64] & mask != 0 {
                return true;
            }
            visited[pair / 64] |= mask;
            #[cfg(test)]
            tests::compatibility_visit();
            match (&arena.types[a.0 as usize], &arena.types[b.0 as usize]) {
                (TypeDef::List(a), TypeDef::List(b)) => types(arena, *a, *b, visited),
                (TypeDef::Record(a), TypeDef::Record(b)) => {
                    a.keys().all(|k| b.contains_key(k))
                        && b.iter().all(|(k, b)| match a.get(k) {
                            Some(a) => {
                                (!a.nullable || b.nullable)
                                    && (!b.required || a.required)
                                    && types(arena, a.ty, b.ty, visited)
                            }
                            None => !b.required,
                        })
                }
                _ => false,
            }
        }
        if actual.nullable && !expected.nullable {
            return false;
        }
        if actual.ty == expected.ty {
            return true;
        }
        // At most 1,024² bits (128KiB), allocated on the heap. Acyclic types
        // bound recursion to depth16; each distinct ordered pair is visited once.
        let mut visited = vec![0_u64; (self.types.len() * self.types.len()).div_ceil(64)];
        types(self, actual.ty, expected.ty, &mut visited)
    }
}
pub(super) fn uninferable(path: &str) -> Diagnostic {
    Diagnostic {
        message: Some("The pilot cannot infer a type from null or an empty list".into()),
        ..error("spec.uninferable_type", path)
    }
}
