//! Language completeness (plan §2.9): keys that moved from rejected to
//! supported, at the compiler boundary. Engine semantics, conformance traces
//! and store tests live with the engine, core and store crates.
use runbook_spec::{CompileContext, compile, parse, restore};
use serde_json::{Value, json};

fn source(body: Value) -> runbook_spec::Spec {
    parse(
        &json!({"apiVersion":"therunbook.app/v1","kind":"Runbook","metadata":{"id":"test","title":"Test"},"spec":body})
            .to_string(),
    )
    .unwrap()
}
fn compiled(body: Value) -> runbook_spec::CompiledRunbook {
    compile(&source(body), &CompileContext::default()).unwrap()
}
fn errors(body: Value) -> Vec<(String, String)> {
    compile(&source(body), &CompileContext::default())
        .map(|_| ())
        .expect_err("must not compile")
        .into_iter()
        .map(|d| (d.code, d.path))
        .collect()
}
fn loop_body(condition: Value) -> Value {
    let mut l = json!({"max_iterations": 3, "steps": [{"id": "probe", "title": "Probe", "outputs": {"ok": {"type": "boolean", "required": true}}}]});
    for (k, v) in condition.as_object().unwrap() {
        l[k] = v.clone();
    }
    json!({"steps": [{"id": "poll", "loop": l}]})
}

// LANG1: `loop.while`.

#[test]
fn loop_while_compiles_and_survives_restore() {
    let c = compiled(loop_body(
        json!({"while": "${{ !steps.probe.outputs.ok }}"}),
    ));
    let runbook_spec::ir::NodeKind::Loop(l) = &c.ir().steps[0].kind else {
        panic!("not a loop")
    };
    assert_eq!(l.until, None);
    assert_eq!(l.r#while.as_deref(), Some("${{ !steps.probe.outputs.ok }}"));
    assert_eq!(
        l.condition(),
        runbook_spec::ir::LoopCondition::While("${{ !steps.probe.outputs.ok }}")
    );
    assert!(c.checked_value("/spec/steps/0/loop/while").is_some());
    assert!(c.checked_value("/spec/steps/0/loop/until").is_none());
    let restored = restore(c.snapshot_bytes()).unwrap();
    assert_eq!(restored.ir(), c.ir());
    assert_eq!(restored.hash(), c.hash());
}

#[test]
fn while_and_until_have_distinct_semantic_hashes() {
    let w = compiled(loop_body(json!({"while": "${{ steps.probe.outputs.ok }}"})));
    let u = compiled(loop_body(json!({"until": "${{ steps.probe.outputs.ok }}"})));
    assert_ne!(w.hash(), u.hash());
}

#[test]
fn a_loop_takes_exactly_one_condition() {
    assert_eq!(
        errors(loop_body(
            json!({"while": "${{ true }}", "until": "${{ true }}"})
        )),
        [("spec.invalid_shape".into(), "/spec/steps/0/loop".into())]
    );
    assert_eq!(
        errors(loop_body(json!({}))),
        [("spec.required".into(), "/spec/steps/0/loop/until".into())]
    );
}

#[test]
fn loop_while_is_a_typed_boolean_with_the_iteration_cap() {
    let codes = errors(loop_body(json!({"while": "${{ 'yes' }}"})));
    assert_eq!(codes[0].1, "/spec/steps/0/loop/while", "{codes:?}");
    // `iteration` is in scope, as for `until`.
    compiled(loop_body(json!({"while": "${{ iteration < 2 }}"})));
    // max_iterations stays required and capped at 50 (PRD §17).
    let mut body = loop_body(json!({"while": "${{ true }}"}));
    body["steps"][0]["loop"]["max_iterations"] = json!(51);
    assert_eq!(errors(body)[0].0, "spec.limit_exceeded");
    let mut body = loop_body(json!({"while": "${{ true }}"}));
    body["steps"][0]["loop"]
        .as_object_mut()
        .unwrap()
        .remove("max_iterations");
    assert_eq!(errors(body)[0].0, "spec.required");
}

#[test]
fn until_loops_keep_their_semantic_hash_shape() {
    // The IR of an `until` loop serializes exactly as before LANG1, so
    // existing snapshots restore byte-for-byte.
    let c = compiled(loop_body(json!({"until": "${{ true }}"})));
    let ir = serde_json::to_value(c.ir()).unwrap();
    let l = &ir["steps"][0]["kind"]["Loop"];
    assert_eq!(l["until"], json!("${{ true }}"));
    assert!(l.get("while").is_none(), "{l}");
}

// LANG2: `join: any` and `join: {at_least: n}`.

fn parallel(join: Value) -> Value {
    json!({"steps": [{"id": "fan", "join": join, "parallel": [
        {"id": "a", "title": "A"}, {"id": "b", "title": "B"}, {"id": "c", "title": "C"}
    ]}]})
}
fn join_of(c: &runbook_spec::CompiledRunbook) -> runbook_spec::ir::Join {
    let runbook_spec::ir::NodeKind::Parallel { join, .. } = &c.ir().steps[0].kind else {
        panic!("not parallel")
    };
    *join
}

#[test]
fn join_any_and_at_least_compile_and_survive_restore() {
    use runbook_spec::ir::Join;
    for (value, want) in [
        (json!("all"), Join::All),
        (json!("any"), Join::AtLeast(1)),
        (json!({"at_least": 2}), Join::AtLeast(2)),
        (json!({"at_least": 3}), Join::AtLeast(3)),
    ] {
        let c = compiled(parallel(value.clone()));
        assert_eq!(join_of(&c), want, "{value}");
        let restored = restore(c.snapshot_bytes()).unwrap();
        assert_eq!(restored.ir(), c.ir(), "{value}");
        assert_eq!(restored.hash(), c.hash(), "{value}");
    }
    // `any` is `at_least: 1`, the same semantics and hash.
    assert_eq!(
        compiled(parallel(json!("any"))).hash(),
        compiled(parallel(json!({"at_least": 1}))).hash()
    );
    assert_ne!(
        compiled(parallel(json!("any"))).hash(),
        compiled(parallel(json!("all"))).hash()
    );
}

#[test]
fn join_all_keeps_its_stored_shape() {
    // A join-all parallel serializes exactly as before LANG2.
    let c = compiled(parallel(json!("all")));
    let ir = serde_json::to_value(c.ir()).unwrap();
    let p = &ir["steps"][0]["kind"]["Parallel"];
    assert!(p.get("join").is_none(), "{p}");
}

#[test]
fn join_counts_are_bounded_by_the_branches() {
    for bad in [json!({"at_least": 0}), json!({"at_least": 4})] {
        let codes = errors(parallel(bad.clone()));
        assert_eq!(
            codes,
            [(
                "spec.invalid_value".into(),
                "/spec/steps/0/join/at_least".into()
            )],
            "{bad}"
        );
    }
    for bad in [json!("ALL"), json!(2), json!({"at_most": 1})] {
        let codes = errors(parallel(bad.clone()));
        assert!(
            codes
                .iter()
                .any(|(_, path)| path.starts_with("/spec/steps/0/join")),
            "{bad}: {codes:?}"
        );
    }
}

#[test]
fn fail_fast_applies_to_join_all_only() {
    let mut body = parallel(json!("any"));
    body["steps"][0]["fail_fast"] = json!(true);
    assert_eq!(
        errors(body),
        [(
            "spec.invalid_shape".into(),
            "/spec/steps/0/fail_fast".into()
        )]
    );
    let mut body = parallel(json!({"at_least": 2}));
    body["steps"][0]["fail_fast"] = json!(false);
    compiled(body);
    let mut body = parallel(json!("all"));
    body["steps"][0]["fail_fast"] = json!(true);
    compiled(body);
}

// LANG3: `concurrency.on_conflict: reject | cancel_previous`.

fn with_policy(policy: Option<&str>) -> Value {
    let mut concurrency = json!({"key": "${{ inputs.service }}", "limit": 1});
    if let Some(p) = policy {
        concurrency["on_conflict"] = json!(p);
    }
    json!({
        "execution": {"concurrency": concurrency},
        "inputs": {"service": {"type": "string", "required": true}},
        "steps": [{"id": "a", "title": "A"}]
    })
}

#[test]
fn on_conflict_policies_compile_and_survive_restore() {
    use runbook_spec::ir::ConcurrencyPolicy;
    for (policy, want) in [
        (None, ConcurrencyPolicy::Queue),
        (Some("queue"), ConcurrencyPolicy::Queue),
        (Some("reject"), ConcurrencyPolicy::Reject),
        (Some("cancel_previous"), ConcurrencyPolicy::CancelPrevious),
    ] {
        let c = compiled(with_policy(policy));
        let got = c.ir().execution.concurrency.as_ref().unwrap().on_conflict;
        assert_eq!(got, want, "{policy:?}");
        let restored = restore(c.snapshot_bytes()).unwrap();
        assert_eq!(restored.ir(), c.ir());
        assert_eq!(restored.hash(), c.hash());
    }
    // `queue` is the default: one hash with and without the key.
    assert_eq!(
        compiled(with_policy(None)).hash(),
        compiled(with_policy(Some("queue"))).hash()
    );
    let hashes: std::collections::BTreeSet<_> = ["queue", "reject", "cancel_previous"]
        .map(|p| *compiled(with_policy(Some(p))).hash())
        .into_iter()
        .collect();
    assert_eq!(hashes.len(), 3);
}

#[test]
fn other_on_conflict_values_are_refused() {
    for bad in ["cancel", "Queue", "drop"] {
        let codes = errors(with_policy(Some(bad)));
        assert_eq!(
            codes[0].1, "/spec/execution/concurrency/on_conflict",
            "{bad}: {codes:?}"
        );
    }
}

// Golden hashes: the semantic hash and the snapshot byte digest of an
// `until` loop, a `join: all` parallel and a `queue` policy, computed with
// the compiler at e045f8b (before LANG1–LANG3). Existing versions keep both.

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn pre_lang_hashes_and_snapshot_digests_are_unchanged() {
    use sha2::Digest;
    const HEAD: &str =
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {id: golden, title: Golden}\n";
    for (name, body, hash, digest) in [
        (
            "until_loop",
            "spec: {steps: [{id: poll, loop: {until: '${{ iteration == 2 }}', max_iterations: 3, delay: 1m, steps: [{id: probe, title: Probe}]}}]}\n",
            "9e1c6b889a10ebfa2764fcbb76172f3789ae482fb431e86ec9aab1c7aeb9f2d0",
            "9f6523b8d6c6c0faf781a41aeaa156ba19ce3ad75cca704843c782ec421d9e73",
        ),
        (
            "join_all",
            "spec: {steps: [{id: fan, join: all, fail_fast: true, parallel: [{id: a, title: A}, {id: b, title: B}]}]}\n",
            "c6cc69b154c2345e39709f769109d8a6ebe022b238cd1fbe1fa94f8bd4ac0e6a",
            "4b719fc0c9084e5e6d40c10930cd18695600fa6b11f7f478da5bfec0f74ee529",
        ),
        (
            "queue",
            "spec: {execution: {concurrency: {key: prod, limit: 2, on_conflict: queue}}, steps: [{id: a, title: A}]}\n",
            "0465d19ad4a02ca1b7dbe05d584d064f57d6f661574f64c0d8dc8520dbceefff",
            "353facb18ec25b4b19a8d307d25efcc03b9b11d8930d2937abe273dc08cce7ba",
        ),
    ] {
        let c = compile(
            &parse(&format!("{HEAD}{body}")).unwrap(),
            &CompileContext::default(),
        )
        .unwrap();
        assert_eq!(hex(c.hash()), hash, "{name} semantic hash");
        assert_eq!(
            hex(&sha2::Sha256::digest(c.snapshot_bytes())),
            digest,
            "{name} snapshot digest"
        );
    }
}

// LANG4: `call`.

fn call_context(entries: &[(&str, runbook_spec::ir::CallResolution)]) -> CompileContext {
    let mut c = CompileContext::default();
    for (key, r) in entries {
        c.runbooks.insert((*key).into(), r.clone());
    }
    c
}
fn child(
    depth: u32,
    reaches: &[&str],
    inputs: &[(&str, bool)],
) -> runbook_spec::ir::CallResolution {
    runbook_spec::ir::CallResolution {
        runbook_id: "rb_00000000000000000000000001".into(),
        version_id: "rbv_00000000000000000000000001".into(),
        version: 2,
        depth,
        reaches: reaches.iter().map(|s| (*s).into()).collect(),
        destructive: false,
        agent_decidable: Some(false),
        inputs: inputs.iter().map(|(n, r)| ((*n).into(), *r)).collect(),
        input_schemas: inputs
            .iter()
            .map(|(n, _)| {
                (
                    (*n).into(),
                    runbook_spec::parse_schema(&json!({"type": "string"})).unwrap(),
                )
            })
            .collect(),
        outputs: [("rows".to_string(), runbook_spec::ir::OutputKind::Integer)]
            .into_iter()
            .collect(),
    }
}
fn call_body(call: Value) -> Value {
    json!({
        "inputs": {"env": {"type": "string", "required": true}},
        "steps": [
            {"id": "migrate", "call": call},
            {"id": "after", "title": "Count ${{ steps.migrate.outputs.rows }}"}
        ]
    })
}
fn call_errors(body: Value, context: &CompileContext) -> Vec<(String, String)> {
    compile(&source(body), context)
        .map(|_| ())
        .expect_err("must not compile")
        .into_iter()
        .map(|d| (d.code, d.path))
        .collect()
}

#[test]
fn call_pins_the_child_version_and_survives_restore() {
    let context = call_context(&[("db-migrate", child(1, &["db-backup"], &[("env", true)]))]);
    let c = compile(
        &source(call_body(json!({
            "runbook": "db-migrate",
            "with": {"env": "${{ inputs.env }}"},
            "outputs": {"rows": {"type": "integer"}}
        }))),
        &context,
    )
    .unwrap();
    let runbook_spec::ir::NodeKind::Call(call) = &c.ir().steps[0].kind else {
        panic!("not a call")
    };
    let target = call.target.as_ref().unwrap();
    assert_eq!(target.version_id, "rbv_00000000000000000000000001");
    assert_eq!(target.version, 2);
    assert_eq!(target.depth, 2);
    assert_eq!(target.reaches, ["db-backup", "db-migrate"]);
    assert!(c.checked_value("/spec/steps/0/call/with/env").is_some());
    let restored = restore(c.snapshot_bytes()).unwrap();
    assert_eq!(restored.ir(), c.ir());
    assert_eq!(restored.hash(), c.hash());
    // The pin is part of the version's identity.
    let mut other = child(1, &["db-backup"], &[("env", true)]);
    other.version_id = "rbv_00000000000000000000000002".into();
    let repinned = compile(
        &source(call_body(json!({
            "runbook": "db-migrate",
            "with": {"env": "${{ inputs.env }}"},
            "outputs": {"rows": {"type": "integer"}}
        }))),
        &call_context(&[("db-migrate", other)]),
    )
    .unwrap();
    assert_ne!(repinned.hash(), c.hash());
    // What a caller of this runbook learns about it.
    let r = c.call_resolution(
        "rb_00000000000000000000000009",
        "rbv_00000000000000000000000009",
        1,
    );
    assert_eq!(r.depth, 2);
    assert_eq!(r.reaches, ["db-backup", "db-migrate"]);
    assert_eq!(r.inputs.get("env"), Some(&true));
}

#[test]
fn call_needs_a_resolved_child_in_the_project() {
    assert_eq!(
        call_errors(
            call_body(json!({"runbook": "db-migrate", "version": 4})),
            &call_context(&[("db-migrate", child(0, &[], &[]))]),
        ),
        [(
            "spec.call_unresolved".into(),
            "/spec/steps/0/call/runbook".into()
        )]
    );
    assert_eq!(
        call_errors(
            call_body(json!({"runbook": "Not A Slug"})),
            &CompileContext::default()
        ),
        [(
            "spec.invalid_value".into(),
            "/spec/steps/0/call/runbook".into()
        )]
    );
    assert_eq!(
        call_errors(
            call_body(json!({"runbook": "x", "version": 0})),
            &CompileContext::default()
        ),
        [(
            "spec.invalid_value".into(),
            "/spec/steps/0/call/version".into()
        )]
    );
    assert_eq!(
        call_errors(
            call_body(json!({"runbook": "x", "timeout": "1h"})),
            &CompileContext::default()
        ),
        [(
            "spec.unknown_key".into(),
            "/spec/steps/0/call/timeout".into()
        )]
    );
}

#[test]
fn call_cannot_recurse_or_nest_beyond_three() {
    // `test` is this runbook's slug.
    assert_eq!(
        call_errors(
            call_body(json!({"runbook": "test"})),
            &call_context(&[("test", child(0, &[], &[]))]),
        ),
        [(
            "spec.call_cycle".into(),
            "/spec/steps/0/call/runbook".into()
        )]
    );
    assert_eq!(
        call_errors(
            call_body(json!({"runbook": "a"})),
            &call_context(&[("a", child(1, &["test"], &[]))]),
        ),
        [(
            "spec.call_cycle".into(),
            "/spec/steps/0/call/runbook".into()
        )]
    );
    assert_eq!(
        call_errors(
            call_body(json!({"runbook": "a"})),
            &call_context(&[("a", child(3, &["b", "c", "d"], &[]))]),
        ),
        [(
            "spec.call_depth_exceeded".into(),
            "/spec/steps/0/call/runbook".into()
        )]
    );
    // Depth 3 itself is allowed.
    compile(
        &source(call_body(
            json!({"runbook": "a", "outputs": {"rows": {"type": "integer"}}}),
        )),
        &call_context(&[("a", child(2, &["b", "c"], &[]))]),
    )
    .unwrap();
}

#[test]
fn call_inputs_match_the_childs_declarations() {
    let context = call_context(&[("a", child(0, &[], &[("env", true), ("dry_run", false)]))]);
    let codes = call_errors(
        call_body(json!({"runbook": "a", "with": {"region": "eu"}})),
        &context,
    );
    assert_eq!(
        codes,
        [
            (
                "spec.call_unknown_input".into(),
                "/spec/steps/0/call/with/region".into()
            ),
            (
                "spec.call_missing_input".into(),
                "/spec/steps/0/call/with".into()
            ),
        ]
    );
    // An output the call does not declare is not addressable.
    let codes = call_errors(
        json!({"steps": [
            {"id": "migrate", "call": {"runbook": "a", "with": {"env": "x"}}},
            {"id": "after", "title": "Count ${{ steps.migrate.outputs.rows }}"}
        ]}),
        &context,
    );
    assert_eq!(codes[0].1, "/spec/steps/1/title", "{codes:?}");
}

#[test]
fn a_call_to_a_destructive_child_counts_as_destructive_after_an_auto_decision() {
    let mut destructive = child(0, &[], &[]);
    destructive.destructive = true;
    let c = compile(
        &source(json!({"steps": [
            {"id": "gate", "approval": {"prompt": "Go?", "options": ["go", {"id": "auto", "auto": true}],
             "approvers": {"users": ["ops"]}, "timeout": "1h", "on_timeout": {"decide": "auto"}}},
            {"id": "run", "call": {"runbook": "a"}}
        ]})),
        &{
            let mut c = call_context(&[("a", destructive)]);
            c.users.insert("ops".into(), "usr_00000000000000000000000001".parse().unwrap());
            c
        },
    )
    .unwrap();
    let codes: Vec<_> = runbook_spec::validate_policy(&c)
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert_eq!(codes, ["spec.auto_decision_destructive"]);
}

#[test]
fn call_outputs_and_literal_inputs_match_the_child() {
    let context = call_context(&[("a", child(0, &[], &[("env", true)]))]);
    // An output the child never produces, and one of another type.
    let codes = call_errors(
        json!({"steps": [{"id": "x", "call": {"runbook": "a", "with": {"env": "prod"},
            "outputs": {"missing": {"type": "string"}, "rows": {"type": "boolean"}}}}]}),
        &context,
    );
    assert_eq!(
        codes,
        [
            (
                "spec.call_unknown_output".into(),
                "/spec/steps/0/call/outputs/missing".into()
            ),
            (
                "spec.call_output_type".into(),
                "/spec/steps/0/call/outputs/rows".into()
            ),
        ]
    );
    // An integer output satisfies a number; a literal input of the wrong type
    // is refused, an expression is left to the start.
    let codes = call_errors(
        json!({"steps": [{"id": "x", "call": {"runbook": "a", "with": {"env": 3},
            "outputs": {"rows": {"type": "number"}}}}]}),
        &context,
    );
    assert_eq!(
        codes,
        [(
            "spec.call_input_type".into(),
            "/spec/steps/0/call/with/env".into()
        )]
    );
    compile(
        &source(json!({"inputs": {"n": {"type": "integer", "default": 1}}, "steps": [{"id": "x", "call": {"runbook": "a",
            "with": {"env": "${{ inputs.n }}"}, "outputs": {"rows": {"type": "number"}}}}]})),
        &context,
    )
    .unwrap();
}

#[test]
fn a_compiled_runbook_reports_its_output_shapes_to_callers() {
    let c = compiled(json!({
        "inputs": {"n": {"type": "integer", "default": 1}},
        "steps": [{"id": "w", "title": "W", "outputs": {"ok": {"type": "boolean"}}}],
        "outputs": {
            "count": "${{ inputs.n }}",
            "label": "n is ${{ inputs.n }}",
            "flag": true,
            "ratio": 1.5,
            "ok": "${{ steps.w.outputs.ok }}"
        }
    }));
    let r = c.call_resolution(
        "rb_00000000000000000000000009",
        "rbv_00000000000000000000000009",
        1,
    );
    use runbook_spec::ir::OutputKind as K;
    assert_eq!(
        r.outputs.into_iter().collect::<Vec<_>>(),
        [
            ("count".to_string(), K::Integer),
            ("flag".to_string(), K::Boolean),
            ("label".to_string(), K::String),
            ("ok".to_string(), K::Boolean),
            ("ratio".to_string(), K::Number),
        ]
    );
    assert_eq!(r.input_schemas.len(), 1);
}

/// Re-check 1: a literal `with` value is checked the way the start checks
/// it, defaults applied: a nested required property with a default may be
/// left out.
#[test]
fn literal_call_inputs_are_checked_with_defaults_applied() {
    let mut resolution = child(0, &[], &[("cfg", true)]);
    resolution.input_schemas.insert(
        "cfg".into(),
        runbook_spec::parse_schema(&json!({"type": "object", "properties": {
            "mode": {"type": "string", "required": true, "default": "fast"}
        }}))
        .unwrap(),
    );
    let context = call_context(&[("a", resolution)]);
    compile(
        &source(json!({"steps": [{"id": "x", "call": {"runbook": "a", "with": {"cfg": {}}}}]})),
        &context,
    )
    .unwrap();
    // A value the start would refuse is still refused at publish.
    assert_eq!(
        call_errors(
            json!({"steps": [{"id": "x", "call": {"runbook": "a", "with": {"cfg": {"mode": 3}}}}]}),
            &context,
        ),
        [(
            "spec.call_input_type".into(),
            "/spec/steps/0/call/with/cfg".into()
        )]
    );
}

/// Re-check 2: a `number` output satisfies an `integer` declaration at
/// publish; whether the value is whole is the runtime's check.
#[test]
fn a_number_output_may_satisfy_an_integer_declaration() {
    let mut resolution = child(0, &[], &[]);
    resolution
        .outputs
        .insert("rows".into(), runbook_spec::ir::OutputKind::Number);
    compile(
        &source(json!({"steps": [{"id": "x", "call": {"runbook": "a",
            "outputs": {"rows": {"type": "integer"}}}}]})),
        &call_context(&[("a", resolution)]),
    )
    .unwrap();
}

#[test]
fn nested_call_decisions_reach_destructive_continuations() {
    let compile_named = |name: &str, body: Value, context: &CompileContext| {
        let spec = parse(
            &json!({
                "apiVersion":"therunbook.app/v1", "kind":"Runbook",
                "metadata":{"id":name,"title":"Test"}, "spec":body
            })
            .to_string(),
        )
        .unwrap();
        compile(&spec, context).unwrap()
    };
    let mut review_context = CompileContext::default();
    review_context.teams.insert(
        "oncall".into(),
        "team_00000000000000000000000001".parse().unwrap(),
    );
    let review = compile_named(
        "review",
        json!({"steps":[{"id":"gate","approval":{
            "prompt":"Ship?", "options":["ship","hold"], "approvers":{"teams":["oncall"]}, "allow_agents":true
        }}]}),
        &review_context,
    );
    let review_pin = review.call_resolution(
        "rb_00000000000000000000000001",
        "rbv_00000000000000000000000001",
        1,
    );
    let wrapper = compile_named(
        "wrapper",
        json!({"steps":[{"id":"review","call":{"runbook":"review"}}]}),
        &call_context(&[("review", review_pin)]),
    );
    let wrapper_pin = wrapper.call_resolution(
        "rb_00000000000000000000000002",
        "rbv_00000000000000000000000002",
        1,
    );
    let context = call_context(&[("wrapper", wrapper_pin)]);
    let call = json!({"id":"child","call":{"runbook":"wrapper"}});
    let task = json!({"id":"deploy","title":"Deploy","destructive":true});
    let checked = |body| compile_named("parent", body, &context);
    for body in [
        json!({"steps":[call, task]}),
        json!({"steps":[{"id":"batch","loop":{"max_iterations":2,"until":"${{ false }}","steps":[task,call]}}]}),
        json!({"steps":[{"id":"fork","parallel":[call,{"id":"independent","title":"Read"}]},task]}),
        json!({"steps":[call],"finally":[task]}),
    ] {
        let parent = checked(body);
        assert!(
            runbook_spec::validate_policy(&parent)
                .iter()
                .any(|d| d.code == "spec.agent_approval_destructive"),
            "{}",
            parent.ir().source_yaml
        );
    }
    let independent = checked(json!({"steps":[{"id":"fork","parallel":[call,task]}]}));
    assert!(runbook_spec::validate_policy(&independent).is_empty());
    assert!(runbook_spec::approval_reaches_destructive(
        checked(json!({"steps":[call,task]})).ir(),
        "child"
    ));
}
