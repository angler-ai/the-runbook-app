use super::*;
use serde_json::json;
fn normalized(body: Value) -> NormalizedRunbook {
    crate::normalize(&crate::parse(&json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":body}).to_string()).unwrap()).unwrap()
}
fn with_pins<T>(f: impl FnOnce(&SelectedPins<'_>) -> T) -> T {
    f(&SelectedPins {
        skills: &BTreeMap::new(),
        users: &BTreeMap::new(),
        teams: &BTreeMap::new(),
        project_variables: &crate::parse_declarations(&json!({})).unwrap(),
    })
}
#[test]
fn semantic_projection_covers_normalized_configuration() {
    let ir = normalized(json!({"steps":[]}));
    with_pins(|pins| {
        let p = projection(&ir, pins);
        assert_eq!(
            p["metadata"],
            json!({"id":"test","title":"Test","description":null,"tags":[],"owners":[]})
        );
        assert_eq!(
            p["defaults"],
            json!({"actor":"any","timeout":null,"retry":{"max_attempts":1,"backoff":null},"on_failure":"fail"})
        );
        assert_eq!(p["steps"], json!([]));
        assert_eq!(p["pins"]["skills"], json!({}));
    });
}

#[test]
fn semantic_hash_ignores_source_presentation_and_extensions() {
    let source = "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: test, title: Test}\nspec:\n  inputs: {value: {type: string}}\n  steps: [{id: a, title: A}]\n";
    let a = crate::normalize(&crate::parse(source).unwrap()).unwrap();
    let b = crate::normalize(
        &crate::parse(&format!(
            "# comment\n{}",
            source
                .replace("{id: a, title: A}", "{title: A, x-note: opaque, id: a}")
                .replace("{type: string}", "{x-editor: ignored, type: string}")
                .replace("inputs: {value:", "inputs: {x-catalog: ignored, value:")
        ))
        .unwrap(),
    )
    .unwrap();
    let extended = normalized(
        json!({"steps":[{"id":"t","title":"Task","with":{"x-note":"ignored","payload":{"x-value":1}}}]}),
    );
    let plain =
        normalized(json!({"steps":[{"id":"t","title":"Task","with":{"payload":{"x-value":1}}}]}));
    with_pins(|pins| assert_eq!(hash(&extended, pins), hash(&plain, pins)));
    assert_ne!(a.source_json, b.source_json);
    assert_ne!(a.inputs, b.inputs);
    with_pins(|pins| assert_eq!(hash(&a, pins), hash(&b, pins)));
}

#[test]
fn semantic_hash_tracks_pattern_and_new_types() {
    let base = normalized(json!({"inputs":{"code":{"type":"string"}},"steps":[]}));
    let with_pat =
        normalized(json!({"inputs":{"code":{"type":"string","pattern":"^[A-Z]{3}$"}},"steps":[]}));
    let diff_pat =
        normalized(json!({"inputs":{"code":{"type":"string","pattern":"^[0-9]+$"}},"steps":[]}));
    let with_date = normalized(json!({"inputs":{"code":{"type":"date"}},"steps":[]}));
    with_pins(|pins| {
        let p_base = hash(&base, pins);
        let p_with = hash(&with_pat, pins);
        let p_diff = hash(&diff_pat, pins);
        let p_date = hash(&with_date, pins);
        assert_ne!(p_base, p_with);
        assert_ne!(p_with, p_diff);
        assert_ne!(p_base, p_date);
    });
}

#[test]
fn semantic_hash_preserves_order_and_opaque_configuration() {
    let original = json!({"agents":{"params":{"x-data":1}},"steps":[{"id":"a","title":"A","instructions":"One","checklist":["one","two"],"with":{"payload":{"x-data":1}},"agent":{"tools":{"x-data":1},"commands":{"x-data":1}}},{"id":"b","title":"B"}]});
    let ir = normalized(original.clone());
    with_pins(|pins| {
        let baseline = hash(&ir, pins);
        for path in [
            "/agents/params/x-data",
            "/steps/0/with/payload/x-data",
            "/steps/0/agent/tools/x-data",
            "/steps/0/agent/commands/x-data",
            "/steps/0/instructions",
        ] {
            let mut changed = original.clone();
            *changed.pointer_mut(path).unwrap() = json!("changed");
            assert_ne!(baseline, hash(&normalized(changed), pins), "{path}");
        }
        for path in ["/steps", "/steps/0/checklist"] {
            let mut changed = original.clone();
            changed
                .pointer_mut(path)
                .unwrap()
                .as_array_mut()
                .unwrap()
                .reverse();
            assert_ne!(baseline, hash(&normalized(changed), pins), "{path}");
        }
        let mut changed = original.clone();
        changed["steps"][0]["retry"] = json!({"max_attempts":2});
        assert_ne!(baseline, hash(&normalized(changed), pins));
        let mut explicit = original.clone();
        explicit["defaults"] =
            json!({"actor":"any","retry":{"max_attempts":1},"on_failure":"fail"});
        assert_eq!(baseline, hash(&normalized(explicit), pins));
    });
    let a =
        normalized(json!({"steps":[{"id":"a","approval":{"prompt":"P","options":["yes","no"]}}]}));
    let mut b = a.clone();
    let crate::ir::NodeKind::Approval(approval) = &mut b.steps[0].kind else {
        panic!()
    };
    approval.escalate_to = Some(vec![]);
    with_pins(|pins| assert_ne!(hash(&a, pins), hash(&b, pins)));
}

#[test]
fn semantic_hash_numbers_are_exact_and_object_safe() {
    let mut ir = normalized(json!({"steps":[]}));
    let mut seen = std::collections::BTreeSet::new();
    for text in [
        "1",
        "1.0",
        "9223372036854775808",
        "12345678901234567890123456789",
        "1e-400",
        "-0.0",
        "1e+400",
        "\"1\"",
    ] {
        let value: Value = serde_json::from_str(text).unwrap();
        ir.agent_params = Some([("opaque".into(), value.clone())].into_iter().collect());
        with_pins(|pins| {
            assert_eq!(projection(&ir, pins)["agent_params"]["opaque"], value);
            assert!(seen.insert(hash(&ir, pins)), "{text}");
        });
    }
    // Construct a literal map directly: serde's bare-Value decoder recognizes
    // this marker, so neither the projection nor test may roundtrip it there.
    let literal = Value::Object(
        [("$serde_json::private::Number".into(), json!("1"))]
            .into_iter()
            .collect(),
    );
    ir.agent_params = Some([("opaque".into(), literal.clone())].into_iter().collect());
    with_pins(|pins| {
        assert_eq!(projection(&ir, pins)["agent_params"]["opaque"], literal);
        assert!(seen.insert(hash(&ir, pins)));
    });
}

#[test]
fn flagship_semantic_hash_is_repeatable() {
    let ir = crate::normalize(
        &crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap(),
    )
    .unwrap();
    let skills = [
        ("error_rates".into(), [0x11; 32]),
        ("rollback".into(), [0x22; 32]),
    ]
    .into();
    let teams = [
        (
            "oncall".into(),
            "team_00000000000000000000000001".parse().unwrap(),
        ),
        (
            "eng-leads".into(),
            "team_00000000000000000000000002".parse().unwrap(),
        ),
    ]
    .into();
    with_pins(|empty| {
        let pins = SelectedPins {
            skills: &skills,
            teams: &teams,
            ..*empty
        };
        let p = projection(&ir, &pins);
        assert_eq!(p, projection(&ir, &pins));
        assert_eq!(
            p["metadata"],
            json!({"id":"post-deploy","title":"Post-deploy verification","description":"Verify a production deploy and decide whether to keep it.","tags":["deploy","production"],"owners":[{"team":"oncall"}]})
        );
        assert_eq!(
            p["skills"],
            json!({"error_rates":"./skills/check-error-rates","rollback":"./skills/rollback-service"})
        );
        assert_eq!(
            p["agent_params"],
            json!({"repo":"acme/api","workflow":"runbook-resume.yml"})
        );
        assert_eq!(
            p["pins"]["teams"],
            json!({"oncall":"team_00000000000000000000000001","eng-leads":"team_00000000000000000000000002"})
        );
        let approval =
            &p["steps"][3]["kind"]["switch"]["cases"][0]["children"][0]["kind"]["approval"];
        assert_eq!(approval["on_timeout"], json!({"escalate":null}));
        assert_eq!(approval["on_expired"], "pause");
        assert_eq!(approval["escalate_to"], json!([{"team":"eng-leads"}]));
        assert_eq!(hash(&ir, &pins), hash(&ir.clone(), &pins));
        assert_ne!(hash(&ir, &pins), hash(&ir, empty));
        assert_eq!(
            projection(&ir, &pins)["pins"]["skills"]["error_rates"],
            json!("11".repeat(32))
        );
    });
    assert_eq!(crate::estimate_step_runs(&ir).unwrap(), 20);
}

#[test]
fn semantic_hash_has_a_fixed_v1_vector() {
    assert_eq!(vectors::corpus().as_array().unwrap().len(), 26);
}

#[test]
fn native_wasm_semantic_corpus_matches_from_fresh_build() {
    use std::{path::PathBuf, process::Command};
    struct FreshTarget(PathBuf);
    impl Drop for FreshTarget {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove test-owned Wasm target");
        }
    }
    let target = (0..1000)
        .find_map(|attempt| {
            let path = std::env::temp_dir().join(format!(
                "runbook-semantic-parity-{}-{attempt}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => Some(FreshTarget(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => panic!("create fresh semantic Wasm target: {error}"),
            }
        })
        .expect("allocate fresh semantic Wasm target");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let build = Command::new(env!("CARGO"))
        .current_dir(&manifest)
        .args([
            "build",
            "--locked",
            "--package",
            "runbook-spec",
            "--example",
            "semantic_parity",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(&target.0)
        .output()
        .expect("build fresh semantic Wasm example");
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let output = Command::new("node")
        .arg(manifest.join("../runbook-expr/tests/wasm-runner.mjs"))
        .arg(
            target
                .0
                .join("wasm32-unknown-unknown/debug/examples/semantic_parity.wasm"),
        )
        .output()
        .expect("execute semantic Wasm example");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let wasm: Value = serde_json::from_slice(&output.stdout).expect("semantic corpus JSON");
    let native = vectors::corpus();
    assert_eq!(native.as_array().unwrap().len(), 26);
    assert_eq!(native, wasm);
}

#[test]
fn semantic_projection_covers_companion_nodes_and_schema_fields() {
    let ir = normalized(json!({
        "execution":{"title":"Run","tags":["tag"],"timeout":"2h","cleanup_timeout":"1m","concurrency":{"key":"key","limit":2}},
        "defaults":{"actor":"agent","timeout":"3m","retry":{"max_attempts":3,"backoff":"1s"},"on_failure":"handoff"},
        "inputs":{"obj":{"type":"object","properties":{"list":{"type":"list","items":{"type":"text","min":1,"max":8,"title":"Label","description":"Help"}}}},"enum":{"type":"enum","options":["a","b"],"default":"a"},"int":{"type":"integer","required":true,"min":0,"max":5},"number":{"type":"number"},"bool":{"type":"boolean"}},
        "fields":{"field":{"type":"string","locked":true,"indexed":false}},"vars":{"v":1},"outputs":{"result":"v"},"report":"Report",
        "steps":[
            {"id":"t","title":"Task","if":"${{ true }}","on_failure":"continue","timeout":"4s","actor":"human","assignee":"user:alice","retry":{"max_attempts":2,"backoff":"2s"},"instructions":{"default":"D","human":"H","agent":"A"},"skill":"s","with":{"payload":{"x-literal":1}},"checklist":["a","b"],"check":true,"skippable":true,"destructive":true,"allow_auto_decision":true,"outputs":{"ok":{"type":"boolean"}},"evidence":{"required":["link"],"accepted":["log","link"]},"agent":{"tools":["curl"],"commands":["curl /health"]}},
            {"id":"g","steps":[{"id":"gc","title":"Child"}],"catch":[{"id":"catch","end":{"status":"failed","message":"Caught"}}],"finally":[{"id":"clean","end":{"status":"cancelled"}}]},
            {"id":"p","parallel":[{"id":"pc","title":"Parallel child"}],"fail_fast":true},
            {"id":"f","foreach":{"items":"${{ inputs.xs }}","as":"x","key":"${{ x }}","concurrency":2,"max_items":3,"steps":[]}},
            {"id":"l","loop":{"until":"${{ true }}","max_iterations":2,"delay":"1s","steps":[]}},
            {"id":"s","switch":{"cases":[{"when":"${{ true }}","steps":[]},{"when":"${{ false }}","steps":[]}],"default":{"steps":[{"id":"done","end":{"status":"succeeded","message":"Done"}}]}}},
            {"id":"wf","wait":{"for":"1s"}}, {"id":"wu","wait":{"until":"${{ true }}","timeout":"2s"}}, {"id":"ws","wait":{"signal":"go","timeout":"3s"}},
            {"id":"a","approval":{"prompt":"Decide","options":[{"id":"yes","title":"Yes","auto":true},"no"],"approvers":{"users":["alice"],"teams":["oncall"]},"min":2,"timeout":"4s","on_timeout":{"decide":"yes"},"escalate_to":[],"escalation_timeout":"5s","on_expired":"fail","allow_agents":true}},
            {"id":"set","set":{"vars":{"v":2},"fields":{"field":"ok"}}},
            {"id":"expr","title":"Expression","assignee":"${{ inputs.user }}","instructions":"Markdown"}
        ],"finally":[{"id":"cleanup","title":"Cleanup"}]
    }));
    with_pins(|pins| {
        let p = projection(&ir, pins);
        assert_eq!(
            p["execution"],
            json!({"title":"Run","tags":["tag"],"timeout":7200000,"cleanup_timeout":60000,"concurrency":{"key":"key","limit":2,"on_conflict":"queue"}})
        );
        assert_eq!(
            p["defaults"],
            json!({"actor":"agent","timeout":180000,"retry":{"max_attempts":3,"backoff":1000},"on_failure":"handoff"})
        );
        assert_eq!(p["variables"], json!({"v":1}));
        assert_eq!(p["outputs"], json!({"result":"v"}));
        assert_eq!(p["report"], "Report");
        let t = &p["steps"][0];
        assert_eq!(t["id"], "t");
        assert_eq!(t["title"], "Task");
        assert_eq!(t["guard"], "${{ true }}");
        assert_eq!(t["on_failure"], "continue");
        assert_eq!(t["timeout"], 4000);
        let mut expected_outputs: Value =
            serde_json::from_str(include_str!("minimal-projection.json")).unwrap();
        expected_outputs = expected_outputs["inputs"].clone();
        let mut bool_schema = expected_outputs.clone();
        bool_schema["kind"] = json!({"boolean":null});
        expected_outputs["kind"]["object"]["ok"] = bool_schema;
        assert_eq!(
            t["kind"]["task"],
            json!({"actor":"human","assignee":{"recipient":{"user":"alice"}},"retry":{"max_attempts":2,"backoff":2000},"instructions":{"audience":{"default":"D","human":"H","agent":"A"}},"skill":"s","parameters":{"payload":{"x-literal":1}},"checklist":["a","b"],"check":true,"skippable":true,"destructive":true,"allow_auto_decision":true,"outputs":expected_outputs,"evidence":{"required":["link"],"accepted":["log","link"]},"agent":{"tools":{"value":["curl"]},"commands":{"value":["curl /health"]}}})
        );
        let group = &p["steps"][1]["kind"]["group"];
        assert_eq!(group["children"][0]["id"], "gc");
        assert_eq!(
            group["catch"][0]["kind"]["end"],
            json!({"status":"failed","message":"Caught"})
        );
        assert_eq!(
            group["finally"][0]["kind"]["end"],
            json!({"status":"cancelled","message":null})
        );
        let parallel = &p["steps"][2]["kind"]["parallel"];
        assert_eq!(parallel["fail_fast"], true);
        assert_eq!(parallel["children"][0]["id"], "pc");
        assert_eq!(
            p["steps"][3]["kind"],
            json!({"foreach":{"items":"${{ inputs.xs }}","alias":"x","key":"${{ x }}","concurrency":2,"max_items":3,"children":[]}})
        );
        assert_eq!(
            p["steps"][4]["kind"],
            json!({"loop":{"until":"${{ true }}","max_iterations":2,"delay":1000,"children":[]}})
        );
        assert_eq!(
            p["steps"][5]["kind"]["switch"]["cases"],
            json!([{"when":"${{ true }}","children":[]},{"when":"${{ false }}","children":[]}])
        );
        assert_eq!(
            p["steps"][5]["kind"]["switch"]["default"][0]["kind"]["end"],
            json!({"status":"succeeded","message":"Done"})
        );
        for (index, expected) in [
            (6, json!({"kind":{"for":1000},"timeout":null})),
            (7, json!({"kind":{"until":"${{ true }}"},"timeout":2000})),
            (8, json!({"kind":{"signal":"go"},"timeout":3000})),
        ] {
            assert_eq!(p["steps"][index]["kind"]["wait"], expected);
        }
        assert_eq!(
            p["steps"][9]["kind"]["approval"],
            json!({"prompt":"Decide","options":[{"id":"yes","title":"Yes","auto":true},{"id":"no","title":"no","auto":false}],"approvers":[{"user":"alice"},{"team":"oncall"}],"min":2,"timeout":4000,"on_timeout":{"decide":"yes"},"escalate_to":[],"escalation_timeout":5000,"on_expired":"fail","allow_agents":true})
        );
        assert_eq!(
            p["steps"][10]["kind"],
            json!({"set":{"variables":{"v":2},"fields":{"field":"ok"}}})
        );
        assert_eq!(
            p["steps"][11]["kind"]["task"]["assignee"],
            json!({"expression":"${{ inputs.user }}"})
        );
        assert_eq!(
            p["steps"][11]["kind"]["task"]["instructions"],
            json!({"markdown":"Markdown"})
        );
        assert_eq!(p["finally"][0]["id"], "cleanup");
        let properties = &p["inputs"]["kind"]["object"];
        assert_eq!(
            properties["obj"]["kind"]["object"]["list"]["kind"]["list"],
            json!({"kind":{"text":null},"required":false,"default":{"missing":null},"min":1,"max":8,"title":"Label","description":"Help","indexed":null,"locked":null})
        );
        assert_eq!(properties["enum"]["kind"], json!({"enum":["a","b"]}));
        assert_eq!(properties["enum"]["default"], json!({"value":"a"}));
        assert_eq!(properties["int"]["kind"], json!({"integer":null}));
        assert_eq!(properties["int"]["required"], true);
        assert_eq!(properties["int"]["min"], 0);
        assert_eq!(properties["int"]["max"], 5);
        assert_eq!(properties["number"]["kind"], json!({"number":null}));
        assert_eq!(properties["bool"]["kind"], json!({"boolean":null}));
        assert_eq!(p["fields"]["kind"]["object"]["field"]["indexed"], false);
        assert_eq!(p["fields"]["kind"]["object"]["field"]["locked"], true);
        let text = p.to_string();
        for forbidden in ["source_path", "source_json", "source_yaml", "source_spans"] {
            assert!(!text.contains(forbidden));
        }
    });
}

#[test]
fn semantic_hash_distinguishes_absent_and_null_advisory_values() {
    let absent = normalized(json!({"steps":[{"id":"t","title":"Task"}]}));
    for key in ["tools", "commands"] {
        let present = normalized(json!({"steps":[{"id":"t","title":"Task","agent":{key:null}}]}));
        with_pins(|pins| assert_ne!(hash(&absent, pins), hash(&present, pins), "{key}"));
    }
}

#[test]
fn semantic_hash_tracks_bounds_cases_schemas_and_selected_facts() {
    let base = normalized(
        json!({"inputs":{"n":{"type":"integer","default":1}},"steps":[{"id":"s","switch":{"cases":[{"when":"${{ true }}","steps":[]},{"when":"${{ false }}","steps":[]}]}},{"id":"loop","loop":{"until":"${{ true }}","max_iterations":2,"steps":[]}},{"id":"each","foreach":{"items":"${{ inputs.xs }}","max_items":3,"steps":[]}}]}),
    );
    with_pins(|pins| {
        let baseline = hash(&base, pins);
        let mut changed = base.clone();
        let NodeKind::Switch(s) = &mut changed.steps[0].kind else {
            panic!()
        };
        s.cases.reverse();
        assert_ne!(baseline, hash(&changed, pins));
        let mut changed = base.clone();
        let NodeKind::Loop(l) = &mut changed.steps[1].kind else {
            panic!()
        };
        l.max_iterations += 1;
        assert_ne!(baseline, hash(&changed, pins));
        let mut changed = base.clone();
        let NodeKind::Foreach(f) = &mut changed.steps[2].kind else {
            panic!()
        };
        f.max_items += 1;
        assert_ne!(baseline, hash(&changed, pins));
        for default in [
            DefaultValue::Missing,
            DefaultValue::Value(json!(2)),
            DefaultValue::Value(Value::Null),
        ] {
            let mut changed = base.clone();
            let DataType::Object { properties } = &mut changed.inputs.kind else {
                panic!()
            };
            properties.get_mut("n").unwrap().default = default;
            assert_ne!(baseline, hash(&changed, pins));
        }
        let mut changed = base.clone();
        let DataType::Object { properties } = &mut changed.inputs.kind else {
            panic!()
        };
        properties.get_mut("n").unwrap().min = Some(0.into());
        assert_ne!(baseline, hash(&changed, pins));
        let skills = [("chosen".into(), [1; 32])].into();
        let changed_skills = [("chosen".into(), [2; 32])].into();
        let selected = SelectedPins {
            skills: &skills,
            ..*pins
        };
        let changed = SelectedPins {
            skills: &changed_skills,
            ..*pins
        };
        assert_ne!(hash(&base, &selected), hash(&base, &changed));
        let users = [(
            "alice".into(),
            "usr_00000000000000000000000001".parse().unwrap(),
        )]
        .into();
        let changed_users = [(
            "alice".into(),
            "usr_00000000000000000000000002".parse().unwrap(),
        )]
        .into();
        assert_ne!(
            hash(
                &base,
                &SelectedPins {
                    users: &users,
                    ..*pins
                }
            ),
            hash(
                &base,
                &SelectedPins {
                    users: &changed_users,
                    ..*pins
                }
            )
        );
        let teams = [(
            "oncall".into(),
            "team_00000000000000000000000001".parse().unwrap(),
        )]
        .into();
        let changed_teams = [(
            "oncall".into(),
            "team_00000000000000000000000002".parse().unwrap(),
        )]
        .into();
        assert_ne!(
            hash(
                &base,
                &SelectedPins {
                    teams: &teams,
                    ..*pins
                }
            ),
            hash(
                &base,
                &SelectedPins {
                    teams: &changed_teams,
                    ..*pins
                }
            )
        );
        let project_int = crate::parse_declarations(&json!({"n":{"type":"integer"}})).unwrap();
        let project_string = crate::parse_declarations(&json!({"n":{"type":"string"}})).unwrap();
        assert_ne!(
            hash(
                &base,
                &SelectedPins {
                    project_variables: &project_int,
                    ..*pins
                }
            ),
            hash(
                &base,
                &SelectedPins {
                    project_variables: &project_string,
                    ..*pins
                }
            )
        );
        let mut moved = base.clone();
        moved.source_json = json!({"irrelevant":true});
        moved.source_yaml = "ignored".into();
        moved.steps[0].source_path = "/moved".into();
        let NodeKind::Switch(s) = &mut moved.steps[0].kind else {
            panic!()
        };
        s.cases[0].source_path = "/another".into();
        assert_eq!(baseline, hash(&moved, pins));
    });
}

#[test]
fn opaque_values_are_preserved_at_every_projected_data_site() {
    let mut ir = normalized(json!({"steps":[{"id":"t","title":"Task"},{"id":"s","set":{}}]}));
    let mut opaque = serde_json::Map::new();
    for (index, text) in [
        "9223372036854775808",
        "12345678901234567890123456789",
        "1.0",
        "1e-400",
        "-0.0",
        "1e+400",
    ]
    .iter()
    .enumerate()
    {
        opaque.insert(format!("n{index}"), serde_json::from_str(text).unwrap());
    }
    opaque.insert("x-literal".into(), json!("real data"));
    opaque.insert(
        "marker".into(),
        Value::Object(
            [("$serde_json::private::Number".into(), json!("1"))]
                .into_iter()
                .collect(),
        ),
    );
    let opaque = Value::Object(opaque);
    ir.variables.insert("v".into(), opaque.clone());
    ir.outputs.insert("v".into(), opaque.clone());
    let NodeKind::Task(task) = &mut ir.steps[0].kind else {
        panic!()
    };
    task.parameters.insert("v".into(), opaque.clone());
    task.agent.tools = Some(opaque.clone());
    task.agent.commands = Some(opaque.clone());
    task.outputs.default = DefaultValue::Value(opaque.clone());
    let NodeKind::Set(set) = &mut ir.steps[1].kind else {
        panic!()
    };
    set.variables.insert("v".into(), opaque.clone());
    set.fields.insert("v".into(), opaque.clone());
    ir.inputs.default = DefaultValue::Value(opaque.clone());
    ir.fields.default = DefaultValue::Value(opaque.clone());
    ir.agent_params = Some([("v".into(), opaque.clone())].into_iter().collect());
    with_pins(|pins| {
        let p = projection(&ir, pins);
        for path in [
            "/variables/v",
            "/outputs/v",
            "/steps/0/kind/task/parameters/v",
            "/steps/0/kind/task/agent/tools/value",
            "/steps/0/kind/task/agent/commands/value",
            "/steps/0/kind/task/outputs/default/value",
            "/steps/1/kind/set/variables/v",
            "/steps/1/kind/set/fields/v",
            "/inputs/default/value",
            "/fields/default/value",
            "/agent_params/v",
        ] {
            assert_eq!(p.pointer(path), Some(&opaque), "{path}");
        }
    });
}

#[test]
fn projection_scratch_measurements() {
    fn depth(value: &Value) -> usize {
        match value {
            Value::Array(v) => 1 + v.iter().map(depth).max().unwrap_or(0),
            Value::Object(v) => 1 + v.values().map(depth).max().unwrap_or(0),
            _ => 0,
        }
    }
    let flagship = crate::normalize(
        &crate::parse(include_str!("../../tests/fixtures/post-deploy.yaml")).unwrap(),
    )
    .unwrap();
    let mut nested = json!({"id":"leaf","title":"Leaf","agent":{"tools":[]}});
    let mut opaque = json!(0);
    for _ in 0..56 {
        opaque = json!([opaque]);
    }
    nested["agent"]["tools"] = opaque;
    for i in 0..32 {
        nested = json!({"id":format!("g{i}"),"steps":[nested]});
    }
    let nested = normalized(json!({"steps":[nested]}));
    with_pins(|pins| {
        for (name, ir) in [
            ("flagship", flagship),
            ("nested_controls_32_opaque_56", nested),
        ] {
            let p = projection(&ir, pins);
            println!(
                "{name}: JSON bytes={}, depth={}",
                p.to_string().len(),
                depth(&p)
            );
        }
    });
}

#[test]
fn canonical_numbers_ignore_redundant_decimal_spelling() {
    let value = |s: &str| Value::Number(s.parse().unwrap());
    for (spellings, canonical) in [
        (vec!["1.1", "1.10", "11e-1", "1.100"], "1.1"),
        (vec!["1e1", "10.0", "100e-1"], "10.0"),
        (
            vec!["-0.0", "0.000", "0e999999999999999999999999999999999999999"],
            "0.0",
        ),
        (vec!["1e400", "10e399", "0.1e401"], "1.0e400"),
        (vec!["1e-400", "10e-401", "0.1e-399"], "1.0e-400"),
    ] {
        let mut expected = vec![2];
        expected.extend_from_slice(&(canonical.len() as u64).to_le_bytes());
        expected.extend_from_slice(canonical.as_bytes());
        for spelling in spellings {
            let mut actual = vec![];
            frame(&value(spelling), &mut |part| actual.extend_from_slice(part));
            assert_eq!(actual, expected, "{spelling}");
            assert_eq!(hash_value(&value(spelling)), hash_value(&value(canonical)));
        }
    }
    assert_ne!(hash_value(&value("1")), hash_value(&value("1.0")));
    assert_eq!(hash_value(&value("-0")), hash_value(&value("0")));
    assert_ne!(
        hash_value(&value("1.100000000000000000001")),
        hash_value(&value("1.1"))
    );
    assert_eq!(
        hash_value(&value("1e999999999999999999999999999999999999999")),
        hash_value(&value("10e999999999999999999999999999999999999998"))
    );
}
