use std::collections::BTreeMap;

use runbook_expr::{FieldDecl, SchemaDef, TypeDef, TypeEnv, TypeId};

fn field(ty: u32) -> FieldDecl {
    FieldDecl {
        ty: TypeId(ty),
        nullable: false,
        required: true,
    }
}

fn schema(types: Vec<TypeDef>) -> SchemaDef {
    SchemaDef {
        types,
        roots: BTreeMap::new(),
    }
}

fn chain(edges: u32) -> SchemaDef {
    let mut types = vec![TypeDef::Int];
    for child in 0..edges {
        types.push(TypeDef::List(TypeId(child)));
    }
    schema(types)
}

fn code(def: &SchemaDef) -> &'static str {
    TypeEnv::new(def).unwrap_err().code
}

#[test]
fn closed_schema_validation_is_bounded() {
    // Removing any size gate accepts the corresponding limit + 1 fixture.
    assert!(TypeEnv::new(&chain(16)).is_ok());
    let mut depth_and_width = chain(15);
    depth_and_width.types.push(TypeDef::Record(
        (0..256).map(|i| (format!("f{i}"), field(15))).collect(),
    ));
    assert!(TypeEnv::new(&depth_and_width).is_ok());
    assert_eq!(code(&chain(17)), "expression.schema_limit");
    assert_eq!(
        code(&schema(vec![TypeDef::List(TypeId(0))])),
        "expression.schema_error"
    );
    assert_eq!(
        code(&schema(vec![TypeDef::List(TypeId(1))])),
        "expression.schema_error"
    );
    assert!(TypeEnv::new(&schema(vec![TypeDef::Int; 1024])).is_ok());
    assert_eq!(
        code(&schema(vec![TypeDef::Int; 1025])),
        "expression.schema_limit"
    );

    let fields: BTreeMap<_, _> = (0..256).map(|i| (format!("f{i}"), field(0))).collect();
    let mut def = schema(vec![TypeDef::Int, TypeDef::Record(fields.clone())]);
    assert!(TypeEnv::new(&def).is_ok());
    let TypeDef::Record(fields) = &mut def.types[1] else {
        unreachable!()
    };
    fields.insert("overflow".into(), field(0));
    assert_eq!(code(&def), "expression.schema_limit");

    let mut def = schema(vec![TypeDef::Int]);
    def.roots = (0..256).map(|i| (format!("r{i}"), field(0))).collect();
    assert!(TypeEnv::new(&def).is_ok());
    def.roots.insert("overflow".into(), field(0));
    assert_eq!(code(&def), "expression.schema_limit");

    let mut def = schema(vec![TypeDef::Int]);
    for _ in 0..16 {
        def.types.push(TypeDef::Record(
            (0..256).map(|i| (format!("f{i}"), field(0))).collect(),
        ));
    }
    assert!(TypeEnv::new(&def).is_ok());
    def.types.push(TypeDef::Record(BTreeMap::from([(
        "extra".into(),
        field(0),
    )])));
    assert_eq!(code(&def), "expression.schema_limit");
}

#[test]
fn shared_subgraphs_do_not_hide_a_deep_path_or_cycle() {
    // Memoized graph validation must retain subtree height, not only visited state.
    let mut def = chain(16);
    def.types.push(TypeDef::Record(BTreeMap::from([
        ("short".into(), field(0)),
        ("long".into(), field(16)),
    ])));
    assert_eq!(code(&def), "expression.schema_limit");
    let def = schema(vec![
        TypeDef::Record(BTreeMap::from([("next".into(), field(1))])),
        TypeDef::List(TypeId(0)),
    ]);
    assert_eq!(code(&def), "expression.schema_error");
}

#[test]
fn names_are_bounded_ascii_cel_identifiers() {
    let mut def = schema(vec![TypeDef::Int]);
    for name in ["a".repeat(128), "_".into(), "camel_12".into()] {
        def.roots = BTreeMap::from([(name, field(0))]);
        assert!(TypeEnv::new(&def).is_ok());
    }
    def.roots = BTreeMap::from([("a".repeat(129), field(0))]);
    assert_eq!(code(&def), "expression.schema_limit");
    for name in ["", "1bad", "has.dot", "é", "true", "null", "in"] {
        def.roots = BTreeMap::from([(name.into(), field(0))]);
        assert_eq!(code(&def), "expression.schema_error", "{name:?}");
        def.roots.clear();
        def.types = vec![
            TypeDef::Int,
            TypeDef::Record(BTreeMap::from([(name.into(), field(0))])),
        ];
        assert_eq!(code(&def), "expression.schema_error", "field {name:?}");
        def.types.truncate(1);
    }
}

#[test]
fn declarations_reject_invalid_references_and_nullable_collections() {
    let mut def = schema(vec![TypeDef::Int, TypeDef::List(TypeId(0))]);
    def.roots.insert("bad".into(), field(2));
    assert_eq!(code(&def), "expression.schema_error");
    def.roots.insert(
        "bad".into(),
        FieldDecl {
            ty: TypeId(1),
            nullable: true,
            required: false,
        },
    );
    assert_eq!(code(&def), "expression.unsupported");
    def.roots.clear();
    def.types.push(TypeDef::Record(BTreeMap::from([(
        "bad".into(),
        FieldDecl {
            ty: TypeId(1),
            nullable: true,
            required: true,
        },
    )])));
    assert_eq!(code(&def), "expression.unsupported");
}

#[test]
fn schema_fingerprints_are_canonical_and_cover_declarations() {
    let empty = schema(vec![]);
    // Independently derived from the version prefix and two zero u32 lengths.
    assert_eq!(
        TypeEnv::new(&empty).unwrap().fingerprint(),
        [
            135, 239, 252, 43, 227, 126, 60, 145, 97, 48, 152, 166, 195, 163, 63, 107, 2, 179, 186,
            161, 162, 245, 242, 249, 114, 150, 145, 63, 84, 78, 248, 207,
        ]
    );
    let mut a = schema(vec![TypeDef::Int, TypeDef::String]);
    a.roots.insert("a".into(), field(0));
    a.roots.insert("b".into(), field(1));
    let mut b = schema(a.types.clone());
    b.roots.insert("b".into(), field(1));
    b.roots.insert("a".into(), field(0));
    let fingerprint = TypeEnv::new(&a).unwrap().fingerprint();
    assert_eq!(fingerprint, TypeEnv::new(&b).unwrap().fingerprint());
    for variant in 0..3 {
        let mut changed = b.clone();
        let decl = changed.roots.get_mut("a").unwrap();
        match variant {
            0 => decl.required = false,
            1 => decl.nullable = true,
            _ => decl.ty = TypeId(1),
        }
        assert_ne!(fingerprint, TypeEnv::new(&changed).unwrap().fingerprint());
    }
    b.types[0] = TypeDef::Double;
    assert_ne!(fingerprint, TypeEnv::new(&b).unwrap().fingerprint());
}

#[test]
fn accepted_schema_owns_only_validated_data() {
    let mut def = schema(vec![TypeDef::Int]);
    def.roots.insert("value".into(), field(0));
    let env = TypeEnv::new(&def).unwrap();
    let fingerprint = env.fingerprint();
    def.types[0] = TypeDef::String;
    def.roots.clear();
    assert_eq!(env.type_definition(TypeId(0)), Some(&TypeDef::Int));
    assert_eq!(env.type_definition(TypeId(1)), None);
    assert_eq!(env.fingerprint(), fingerprint);
}

#[test]
fn record_fingerprints_cover_field_order_and_flags() {
    let fields = BTreeMap::from([("a".into(), field(0)), ("z".into(), field(1))]);
    let def = schema(vec![TypeDef::Int, TypeDef::String, TypeDef::Record(fields)]);
    let fingerprint = TypeEnv::new(&def).unwrap().fingerprint();
    let mut reordered = def.clone();
    reordered.types[2] = TypeDef::Record(BTreeMap::from([
        ("z".into(), field(1)),
        ("a".into(), field(0)),
    ]));
    assert_eq!(fingerprint, TypeEnv::new(&reordered).unwrap().fingerprint());
    for variant in 0..3 {
        let mut changed = def.clone();
        let TypeDef::Record(fields) = &mut changed.types[2] else {
            unreachable!()
        };
        let declaration = fields.get_mut("a").unwrap();
        match variant {
            0 => declaration.required = false,
            1 => declaration.nullable = true,
            _ => declaration.ty = TypeId(1),
        }
        assert_ne!(fingerprint, TypeEnv::new(&changed).unwrap().fingerprint());
    }
}

#[test]
fn root_declarations_count_toward_total_declared_fields() {
    let mut def = schema(vec![TypeDef::Int]);
    for _ in 0..15 {
        def.types.push(TypeDef::Record(
            (0..256).map(|i| (format!("f{i}"), field(0))).collect(),
        ));
    }
    def.roots = (0..256).map(|i| (format!("r{i}"), field(0))).collect();
    assert!(TypeEnv::new(&def).is_ok());
    def.types.push(TypeDef::Record(BTreeMap::from([(
        "extra".into(),
        field(0),
    )])));
    assert_eq!(code(&def), "expression.schema_limit");
}
