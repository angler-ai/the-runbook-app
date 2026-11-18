use runbook_spec::parse;

const MINIMAL: &str = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: { id: store-opening, title: Store opening }
spec:
  steps:
    - { id: lights, title: Turn on lights and music }
    - { id: till, title: Count the till float, outputs: { amount: { type: number } } }
    - { id: doors, title: Unlock the doors at 9:00 }
"#;

#[test]
fn minimal_runbook_has_three_tasks() {
    assert_eq!(parse(MINIMAL).unwrap().steps().len(), 3);
}

// Removing duplicate detection must make this test fail: the second ID must
// never silently replace the first one.
#[test]
fn duplicate_keys_fail_at_source() {
    let duplicate_yaml = r#"apiVersion: therunbook.app/v1
kind: Runbook
metadata: {id: one, id: two}
spec: {steps: []}
"#;
    let errors = parse(duplicate_yaml).unwrap_err();
    assert_eq!(errors[0].code, "spec.duplicate_key");
    assert_eq!(errors[0].path, "/metadata/id");
    assert_eq!((errors[0].line, errors[0].column), (3, 21));
}

// Removing any syntax guard permits YAML outside the JSON-compatible profile.
#[test]
fn non_json_yaml_is_rejected() {
    let cases = [
        (
            "anchor",
            "metadata: &meta {id: one, title: One}\nspec: {steps: []}",
        ),
        ("alias", "metadata: *meta\nspec: {steps: []}"),
        (
            "merge",
            "metadata: {<<: {id: one}, title: One}\nspec: {steps: []}",
        ),
        (
            "custom_tag",
            "metadata: !custom {id: one, title: One}\nspec: {steps: []}",
        ),
    ];
    for (name, body) in cases {
        let yaml = format!("apiVersion: therunbook.app/v1\nkind: Runbook\n{body}\n");
        let errors = parse(&yaml).unwrap_err();
        assert_eq!(errors[0].code, "spec.yaml_feature_forbidden", "{name}");
        assert!(errors[0].line > 0, "{name}");
        assert!(errors[0].column > 0, "{name}");
    }
}

#[test]
fn yaml12_on_is_string() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: example, title: Example}\nspec:\n  inputs:\n    enabled: {type: string, default: on}\n    checked: {type: boolean, default: true}\n  steps: []\n";
    let spec = parse(yaml).unwrap();
    assert_eq!(spec.spec.inputs["enabled"]["default"], "on");
    assert_eq!(spec.spec.inputs["checked"]["default"], true);
}

// Dropping flattened keys, reserializing YAML, or removing nested value spans
// would lose source information needed by capability checks and editors.
#[test]
fn retains_source_extensions_unknown_keys_and_node_fields() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nx-owner: {team: ops}\nfuture-root: true\nmetadata: {id: example, title: Example, x-label: blue}\nspec:\n  report: '# Report'\n  x-config: {a/b~c: [one, two]}\n  steps:\n    - id: group\n      parallel:\n        - {id: child, title: Child, x-task: {enabled: true}}\n";
    let spec = parse(yaml).unwrap();
    assert_eq!(spec.raw_source, yaml);
    assert_eq!(spec.extra["x-owner"]["team"], "ops");
    assert_eq!(spec.extra["future-root"], true);
    assert_eq!(spec.metadata.extra["x-label"], "blue");
    assert_eq!(spec.spec.extra["report"], "# Report");
    assert_eq!(
        spec.steps()[0].fields["parallel"][0]["x-task"]["enabled"],
        true
    );
    assert_eq!(spec.source_json()["spec"]["x-config"]["a/b~c"][1], "two");
    let span = spec.source_spans["/spec/x-config/a~1b~0c/1"];
    assert_eq!((span.line, span.column), (8, 27));
}

#[test]
fn malformed_multiple_and_empty_documents_are_errors() {
    for (yaml, code) in [
        ("[unclosed", "spec.yaml_syntax"),
        ("", "spec.yaml_syntax"),
        ("{}\n---\n{}\n", "spec.yaml_document_count"),
        ("spec: []", "spec.source_shape"),
    ] {
        assert_eq!(parse(yaml).unwrap_err()[0].code, code, "{yaml:?}");
    }
}

#[test]
fn non_string_mapping_keys_are_rejected() {
    for key in ["true", "42", "null", "[a, b]"] {
        let yaml = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: example, title: Example}}\nspec: {{steps: []}}\nx-map: {{{key}: value}}\n"
        );
        assert_eq!(
            parse(&yaml).unwrap_err()[0].code,
            "spec.non_string_key",
            "{key}"
        );
    }
}

#[test]
fn scalar_values_preserve_json_types_and_integer_precision() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: example, title: Example}\nspec: {steps: []}\nx-values: [on, off, yes, no, true, false, Null, NULL, null, 18446744073709551615, 0o17, 0x10, 1.25, 'true']\n";
    let spec = parse(yaml).unwrap();
    assert_eq!(
        spec.extra["x-values"],
        serde_json::json!([
            "on",
            "off",
            "yes",
            "no",
            true,
            false,
            null,
            null,
            null,
            18446744073709551615_u64,
            15,
            16,
            1.25,
            "true"
        ])
    );
}

#[test]
fn non_json_floats_are_rejected() {
    for scalar in [".inf", "-.Inf", ".NaN"] {
        let yaml = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: example, title: Example}}\nspec: {{steps: []}}\nx-number: {scalar}\n"
        );
        assert_eq!(parse(&yaml).unwrap_err()[0].code, "spec.non_json_scalar");
    }
}

#[test]
fn yaml11_directives_are_rejected() {
    let yaml = format!("%YAML 1.1\n---\n{MINIMAL}");
    assert_eq!(
        parse(&yaml).unwrap_err()[0].code,
        "spec.yaml_feature_forbidden"
    );
}

#[test]
fn large_json_numbers_are_not_rounded() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: example, title: Example}\nspec: {steps: []}\nx-large: 340282366920938463463374607431768211455\nx-fraction: 0.1234567890123456789012345\n";
    let spec = parse(yaml).unwrap();
    assert_eq!(
        spec.extra["x-large"].to_string(),
        "340282366920938463463374607431768211455"
    );
    assert_eq!(
        spec.extra["x-fraction"].to_string(),
        "0.1234567890123456789012345"
    );
}

#[test]
fn source_shape_errors_point_to_invalid_field() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: example, title: Example}\nspec:\n  steps:\n    - id: 42\n";
    let errors = parse(yaml).unwrap_err();
    assert_eq!(errors[0].code, "spec.source_shape");
    assert_eq!(errors[0].path, "/spec/steps/0/id");
    assert_eq!((errors[0].line, errors[0].column), (6, 11));
}

#[test]
fn extreme_nesting_returns_a_diagnostic() {
    let yaml = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: example, title: Example}}\nspec: {{steps: []}}\nx-nested: {}null{}\n",
        "[".repeat(140),
        "]".repeat(140)
    );
    assert_eq!(parse(&yaml).unwrap_err()[0].code, "spec.yaml_nesting_limit");
}

#[test]
fn full_prd_example_preserves_all_pilot_source_syntax() {
    let spec = parse(include_str!("fixtures/post-deploy.yaml")).unwrap();
    assert_eq!(spec.steps().len(), 4);
    assert_eq!(spec.spec.finally_steps.len(), 1);
    assert_eq!(
        spec.spec.extra["execution"]["concurrency"]["on_conflict"],
        "queue"
    );
    assert_eq!(spec.spec.fields["severity"]["indexed"], true);
    assert_eq!(
        spec.steps()[0].fields["parallel"][1]["checklist"][0],
        "Login page loads"
    );
    assert_eq!(
        spec.steps()[1].fields["foreach"]["steps"][0]["with"]["window"],
        "10m"
    );
    assert_eq!(spec.spec.extra["agents"]["params"]["repo"], "acme/api");
}

#[test]
fn syntax_markers_inside_strings_are_plain_data() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: example, title: Example}\nspec: {steps: []}\nx-notes: ['&anchor', '*alias', '!tag', '<<']\n";
    assert_eq!(parse(yaml).unwrap().extra["x-notes"][1], "*alias");
}

#[test]
fn yaml_decimal_spellings_do_not_lose_precision() {
    let yaml = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: example, title: Example}\nspec: {steps: []}\nx-plus: +0.1234567890123456789012345\nx-leading: .1234567890123456789012345\nx-trailing: 01.\n";
    let spec = parse(yaml).unwrap();
    assert_eq!(
        spec.extra["x-plus"].to_string(),
        "0.1234567890123456789012345"
    );
    assert_eq!(
        spec.extra["x-leading"].to_string(),
        "0.1234567890123456789012345"
    );
    assert_eq!(spec.extra["x-trailing"].to_string(), "1.0");
}

#[test]
fn extension_maps_preserve_private_number_like_keys() {
    for value in ["'42'", "'not-number'"] {
        for sibling in ["", ", sibling: kept"] {
            let yaml = format!(
                "{MINIMAL}\nx-value: {{'$serde_json::private::Number': {value}{sibling}}}\n"
            );
            let spec = parse(&yaml).unwrap();
            assert!(spec.extra["x-value"].is_object());
            assert_eq!(spec.extra["x-value"], spec.source_json()["x-value"]);
        }
    }
}

#[test]
fn large_radix_integers_remain_numbers_and_cannot_be_keys() {
    for scalar in ["0x8000000000000000", "0o1000000000000000000000"] {
        let parsed = parse(&format!("{MINIMAL}\nx-value: {scalar}\n")).unwrap();
        assert_eq!(
            parsed.source_json()["x-value"].to_string(),
            "9223372036854775808"
        );
        assert!(parsed.extra["x-value"].is_number());
        let errors = parse(&format!("{MINIMAL}\nx-value: {{{scalar}: accepted}}\n")).unwrap_err();
        assert_eq!(errors[0].code, "spec.non_string_key");
    }
    let errors = parse(&format!(
        "{MINIMAL}\nx-value: 0x100000000000000000000000000000000\n"
    ))
    .unwrap_err();
    assert_eq!(errors[0].code, "spec.non_json_scalar");
}
