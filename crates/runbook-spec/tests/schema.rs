use runbook_spec::{parse_declarations, parse_schema, resolve_data, schema_json, validate_data};
use serde_json::json;

#[test]
fn declared_integer_rejects_text() {
    let schema = parse_declarations(&json!({
        "status_code": {"type": "integer", "required": true}
    }))
    .unwrap();
    let errors = validate_data(&schema, &json!({"status_code": "503"}));
    assert_eq!(errors[0].code, "data.type_error");
    assert_eq!(errors[0].path, "status_code");
    assert_eq!((errors[0].line, errors[0].column), (0, 0));
    assert!(validate_data(&schema, &json!({"status_code": 503})).is_empty());
    assert_eq!(
        validate_data(&schema, &json!({"status_code": true}))[0].code,
        "data.type_error"
    );
    assert_eq!(
        validate_data(&schema, &json!([]))[0].code,
        "data.type_error"
    );
}

#[test]
fn recursive_values_are_checked() {
    let schema = parse_declarations(&json!({
        "services": {"type": "list", "items": {"type": "object", "properties": {
            "port": {"type": "integer", "required": true}
        }}},
        "a.b": {"type": "integer"}
    }))
    .unwrap();
    let errors = validate_data(&schema, &json!({"services": [{"port": "80"}]}));
    assert_eq!(
        (errors[0].code.as_str(), errors[0].path.as_str()),
        ("data.type_error", "services[0].port")
    );
    let errors = validate_data(&schema, &json!({"services": [{}]}));
    assert_eq!(
        (errors[0].code.as_str(), errors[0].path.as_str()),
        ("data.required", "services[0].port")
    );
    let errors = validate_data(&schema, &json!({"services": [{"port": 80, "extra": 0}]}));
    assert_eq!(
        (errors[0].code.as_str(), errors[0].path.as_str()),
        ("data.unknown_field", "services[0].extra")
    );
    assert!(validate_data(&schema, &json!({"services": [{"port": 80}]})).is_empty());
    assert_eq!(
        validate_data(&schema, &json!({"a.b": "bad"}))[0].path,
        "[\"a.b\"]"
    );
    assert_eq!(
        validate_data(&schema, &json!({"": false}))[0].path,
        "[\"\"]"
    );
}

#[test]
fn defaults_only_fill_missing_values() {
    let schema = parse_declarations(&json!({
        "code": {"type": "integer", "required": true, "default": 200},
        "config": {"type": "object", "default": {}, "properties": {
            "port": {"type": "integer", "default": 80}
        }},
        "services": {"type": "list", "items": {"type": "object", "properties": {
            "port": {"type": "integer", "default": 443}
        }}}
    }))
    .unwrap();
    let source = json!({"services": [{}]});
    assert_eq!(
        resolve_data(&schema, &source).unwrap(),
        json!({
            "code": 200, "config": {"port": 80}, "services": [{"port": 443}]
        })
    );
    assert_eq!(source, json!({"services": [{}]}));
    assert_eq!(validate_data(&schema, &source)[0].code, "data.required");
    let errors = resolve_data(&schema, &json!({"code": null})).unwrap_err();
    assert_eq!(
        (errors[0].code.as_str(), errors[0].path.as_str()),
        ("data.type_error", "code")
    );
    assert_eq!(
        resolve_data(&schema, &json!({"code": 201})).unwrap()["code"],
        201
    );
    let stored = serde_json::to_value(&schema).unwrap();
    let restored = serde_json::from_value(stored).unwrap();
    assert_eq!(resolve_data(&restored, &source).unwrap()["code"], 200);
}

#[test]
fn constraints_and_defaults_are_checked() {
    let cases = [
        (
            json!({"type": "number", "min": 1.5, "max": 3}),
            json!(1.5),
            json!(3.1),
            "data.range",
        ),
        (
            json!({"type": "integer", "min": -2, "max": 2}),
            json!(2),
            json!(-3),
            "data.range",
        ),
        (
            json!({"type": "string", "min": 2, "max": 2}),
            json!("é🦀"),
            json!("é"),
            "data.range",
        ),
        (
            json!({"type": "text", "max": 1}),
            json!("🦀"),
            json!("ab"),
            "data.range",
        ),
        (
            json!({"type": "list", "items": {"type": "boolean"}, "min": 1, "max": 2}),
            json!([true]),
            json!([]),
            "data.range",
        ),
        (
            json!({"type": "enum", "options": ["minor", "major"]}),
            json!("minor"),
            json!("none"),
            "data.enum",
        ),
        (
            json!({"type": "boolean"}),
            json!(true),
            json!(1),
            "data.type_error",
        ),
        (
            json!({"type": "string", "pattern": "^[a-z]{3}$"}),
            json!("abc"),
            json!("abcd"),
            "data.pattern",
        ),
        (
            json!({"type": "multi_enum", "options": ["minor", "major"]}),
            json!(["minor", "major"]),
            json!(["none"]),
            "data.enum",
        ),
        (
            json!({"type": "multi_enum", "options": ["a", "b"]}),
            json!(["a"]),
            json!(["a", "a"]),
            "data.enum",
        ),
        (
            json!({"type": "date"}),
            json!("2026-11-12"),
            json!("2026-02-30"),
            "data.format",
        ),
        (
            json!({"type": "datetime"}),
            json!("2026-11-12T10:00:00Z"),
            json!("2026-11-12 10:00:00"),
            "data.format",
        ),
        (
            json!({"type": "duration"}),
            json!("1h30m"),
            json!("1hour"),
            "data.format",
        ),
        (
            json!({"type": "url"}),
            json!("https://example.com/api"),
            json!("not a url"),
            "data.format",
        ),
        (
            json!({"type": "email"}),
            json!("user@example.com"),
            json!("not-an-email"),
            "data.format",
        ),
        (
            json!({"type": "user"}),
            json!("user:jo"),
            json!("not a user"),
            "data.format",
        ),
    ];
    for (declaration, valid, invalid, code) in cases {
        let schema = parse_schema(&declaration).unwrap();
        assert!(validate_data(&schema, &valid).is_empty(), "{declaration}");
        assert_eq!(
            validate_data(&schema, &invalid)[0].code,
            code,
            "{declaration}"
        );
    }
    let invalid = [
        json!({"type": "number", "min": 5, "max": 4}),
        json!({"type": "number", "min": "1"}),
        json!({"type": "string", "min": -1}),
        json!({"type": "text", "max": 1.5}),
        json!({"type": "list"}),
        json!({"type": "enum"}),
        json!({"type": "enum", "options": []}),
        json!({"type": "enum", "options": [1]}),
        json!({"type": "enum", "options": ["x", "x"]}),
        json!({"type": "multi_enum"}),
        json!({"type": "multi_enum", "options": []}),
        json!({"type": "multi_enum", "options": [1]}),
        json!({"type": "multi_enum", "options": ["x", "x"]}),
        json!({"type": "object", "properties": []}),
        json!({"type": "integer", "required": "yes"}),
        json!({"type": "integer", "default": "1"}),
        json!({"type": "string", "default": null}),
        json!({"type": "enum", "options": ["ok"], "default": "bad"}),
        json!({"type": "integer", "typo": true}),
        json!({"type": "boolean", "min": 1}),
        json!({"type": "string", "items": {"type": "integer"}}),
        json!({"type": "string", "indexed": "yes"}),
        json!({"type": "string", "title": 1}),
        json!({"type": "integer", "pattern": ".*"}),
        json!({"type": "boolean", "pattern": ".*"}),
        json!({"type": "list", "items": {"type": "string"}, "pattern": ".*"}),
        json!({"type": "string", "pattern": "("}),
        json!({"type": "string", "pattern": "(?=lookaround)"}),
        json!({"required": true}),
        json!(null),
    ];
    for declaration in invalid {
        assert!(parse_schema(&declaration).is_err(), "{declaration}");
    }
    assert_eq!(
        parse_schema(&json!({"type": "string", "pattern": "("})).unwrap_err()[0].code,
        "spec.invalid_pattern"
    );
    assert_eq!(
        parse_schema(&json!({"type": "string", "pattern": "a".repeat(1025)})).unwrap_err()[0].code,
        "spec.pattern_limit"
    );
    for declaration in [
        json!({"type": "secret_ref"}),
        json!({"type": "future_type"}),
    ] {
        assert_eq!(
            parse_schema(&declaration).unwrap_err()[0].code,
            "spec.unsupported_feature"
        );
    }
    let annotated = parse_schema(&json!({"type": "integer", "title": "Count", "description": "Items", "indexed": true, "locked": true,
        "x-custom": {"type": "unsupported", "default": null}})).unwrap();
    assert!(validate_data(&annotated, &json!(7)).is_empty());
    let stored = serde_json::to_value(&annotated).unwrap();
    assert!(stored.to_string().contains("x-custom"));
    let number = parse_schema(&json!({"type": "number"})).unwrap();
    let arbitrary: serde_json::Value =
        serde_json::from_str("12345678901234567890123456789").unwrap();
    assert!(validate_data(&number, &arbitrary).is_empty());
    let restored = serde_json::from_value(serde_json::to_value(&number).unwrap()).unwrap();
    assert!(validate_data(&restored, &arbitrary).is_empty());
}

#[test]
fn schema_emission_matches_validation() {
    let cases = [
        (
            json!({"type": "string", "min": 1, "max": 2}),
            json!({"type": "string", "minLength": 1, "maxLength": 2}),
            vec![json!("é🦀"), json!(""), json!(3)],
        ),
        (
            json!({"type": "text", "max": 2}),
            json!({"type": "string", "maxLength": 2}),
            vec![json!("a\nb"), json!("🦀"), json!(null)],
        ),
        (
            json!({"type": "number", "min": -1.5, "max": 2}),
            json!({"type": "number", "minimum": -1.5, "maximum": 2}),
            vec![json!(0), json!(-2), json!(false)],
        ),
        (
            json!({"type": "integer", "min": -2, "max": 2}),
            json!({"type": "integer", "minimum": -2, "maximum": 2}),
            vec![json!(1), json!(1.0), json!(1.5), json!(3), json!("1")],
        ),
        (
            json!({"type": "boolean"}),
            json!({"type": "boolean"}),
            vec![json!(false), json!(0), json!(null)],
        ),
        (
            json!({"type": "enum", "options": ["a", "b"]}),
            json!({"type": "string", "enum": ["a", "b"]}),
            vec![json!("a"), json!("c"), json!(1)],
        ),
        (
            json!({"type": "list", "items": {"type": "boolean"}, "min": 1, "max": 2}),
            json!({"type": "array", "items": {"type": "boolean"}, "minItems": 1, "maxItems": 2}),
            vec![
                json!([false]),
                json!([]),
                json!([1]),
                json!([true, false, true]),
            ],
        ),
        (
            json!({"type": "object", "properties": {"port": {"type": "integer", "required": true}, "enabled": {"type": "boolean", "default": true}}}),
            json!({"type": "object", "properties": {"port": {"type": "integer", "minimum": -9223372036854775808_i64, "maximum": 9223372036854775807_i64}, "enabled": {"type": "boolean", "default": true}}, "required": ["port"], "additionalProperties": false}),
            vec![
                json!({"port": 80}),
                json!({}),
                json!({"port": 80, "extra": true}),
                json!({"port": null}),
            ],
        ),
    ];
    for (declaration, mut expected, samples) in cases {
        let schema = parse_schema(&declaration).unwrap();
        expected.as_object_mut().unwrap().insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        let emitted = schema_json(&schema);
        assert_eq!(emitted, expected, "{declaration}");
        let validator = jsonschema::validator_for(&emitted).unwrap();
        for sample in samples {
            assert_eq!(
                validator.is_valid(&sample),
                validate_data(&schema, &sample).is_empty(),
                "{declaration}: {sample}"
            );
        }
    }
    let schema = parse_declarations(&json!({"label": {"type": "string", "title": "Label", "description": "Display", "indexed": true, "locked": true, "x-extra": {"type": "null"}}})).unwrap();
    let emitted = schema_json(&schema);
    assert_eq!(emitted["additionalProperties"], false);
    assert_eq!(
        emitted["properties"]["label"],
        json!({"type": "string", "title": "Label", "description": "Display"})
    );
}

#[test]
fn numeric_domains_do_not_round_integers() {
    let integer = parse_schema(&json!({"type": "integer"})).unwrap();
    let number = parse_schema(&json!({"type": "number"})).unwrap();
    let integer_validator = jsonschema::validator_for(&schema_json(&integer)).unwrap();
    let number_validator = jsonschema::validator_for(&schema_json(&number)).unwrap();
    for (text, int_valid, num_valid) in [
        ("1.0", true, true),
        ("1e2", true, true),
        ("-0.0", true, true),
        ("9223372036854775807", true, true),
        ("-9223372036854775808", true, true),
        ("9223372036854775808", false, true),
        ("-9223372036854775809", false, true),
        ("9007199254740993.0", true, true),
        ("1.0000000000000001", false, true),
        ("1.7976931348623157e308", false, true),
        ("1e309", false, false),
        ("-1e309", false, false),
        ("1e-999", false, true),
        ("0", true, true),
    ] {
        let value = serde_json::from_str(text).unwrap();
        assert_eq!(
            validate_data(&integer, &value).is_empty(),
            int_valid,
            "integer {text}"
        );
        assert_eq!(
            validate_data(&number, &value).is_empty(),
            num_valid,
            "number {text}"
        );
        assert_eq!(
            integer_validator.is_valid(&value),
            int_valid,
            "JSON Schema integer {text}"
        );
        assert_eq!(
            number_validator.is_valid(&value),
            num_valid,
            "JSON Schema number {text}"
        );
    }
    let exact = parse_schema(&json!({"type": "integer", "min": 9007199254740993_i64})).unwrap();
    assert_eq!(
        validate_data(&exact, &json!(9007199254740992_i64))[0].code,
        "data.range"
    );
}

#[test]
fn excessive_depth_is_a_diagnostic() {
    fn list_schema(levels: usize) -> serde_json::Value {
        let mut declaration = json!({"type": "integer"});
        for _ in 0..levels {
            declaration = json!({"type": "list", "items": declaration});
        }
        declaration
    }
    fn nested_value(levels: usize) -> serde_json::Value {
        let mut value = json!(1);
        for _ in 0..levels {
            value = json!([value]);
        }
        value
    }
    let schema = parse_schema(&list_schema(64)).unwrap();
    assert!(validate_data(&schema, &nested_value(64)).is_empty());
    assert!(resolve_data(&schema, &nested_value(64)).is_ok());
    assert_eq!(
        parse_schema(&list_schema(65)).unwrap_err()[0].code,
        "spec.depth_limit"
    );
    assert_eq!(
        validate_data(&schema, &nested_value(65))[0].code,
        "data.depth_limit"
    );
    assert_eq!(
        resolve_data(&schema, &nested_value(65)).unwrap_err()[0].code,
        "data.depth_limit"
    );
    let shallow = parse_schema(&json!({"type": "integer"})).unwrap();
    assert_eq!(
        resolve_data(&shallow, &nested_value(100)).unwrap_err()[0].code,
        "data.depth_limit"
    );
    let extension = json!({"type": "integer", "x-deep": nested_value(65)});
    assert_eq!(
        parse_schema(&extension).unwrap_err()[0].code,
        "spec.depth_limit"
    );
    let mut wide = serde_json::Map::new();
    for index in 0..4096 {
        wide.insert(format!("f{index}"), json!({"type": "integer"}));
    }
    assert_eq!(
        parse_declarations(&serde_json::Value::Object(wide)).unwrap_err()[0].code,
        "spec.schema_limit"
    );
}

#[test]
fn decimal_constraints_preserve_precision_even_for_extreme_exponents() {
    let min: serde_json::Value =
        serde_json::from_str("1e-999999999999999999999999999999999999998").unwrap();
    let max: serde_json::Value =
        serde_json::from_str("1e-999999999999999999999999999999999999999").unwrap();
    assert_eq!(
        parse_schema(&json!({"type": "number", "min": min, "max": max})).unwrap_err()[0].code,
        "spec.invalid_schema"
    );
    let schema = parse_schema(&json!({"type": "number", "min": max})).unwrap();
    assert!(validate_data(&schema, &min).is_empty());
    let smaller: serde_json::Value =
        serde_json::from_str("1e-1000000000000000000000000000000000000000").unwrap();
    assert_eq!(validate_data(&schema, &smaller)[0].code, "data.range");
    let schema =
        parse_schema(&json!({"type": "number", "default": 12345678901234567890_u64})).unwrap();
    let stored = serde_json::to_value(&schema).unwrap();
    let restored: runbook_spec::DataSchema = serde_json::from_value(stored).unwrap();
    assert_eq!(schema, restored);
    let disguised = json!({"$serde_json::private::Number": "1"});
    assert_eq!(
        validate_data(&schema, &disguised)[0].code,
        "data.type_error"
    );
    let marker_default = parse_schema(&json!({
        "type": "object",
        "properties": {"$serde_json::private::Number": {"type": "string"}},
        "default": disguised,
        "x-data": disguised
    }))
    .unwrap();
    let restored: runbook_spec::DataSchema =
        serde_json::from_value(serde_json::to_value(&marker_default).unwrap()).unwrap();
    assert_eq!(marker_default, restored);
}
