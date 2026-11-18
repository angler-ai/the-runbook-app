//! LANG9b: `spec.triggers`, schedules and webhooks that start runs.
use runbook_spec::{CompileContext, compile, normalize, parse};

const INPUTS: &str =
    "  inputs:\n    service: {type: string, required: true}\n    version: {type: string}\n";

fn source(triggers: &str) -> String {
    format!(
        "metadata: {{id: deploy-check}}\nspec:\n{INPUTS}  triggers:\n{triggers}  steps:\n    - {{id: check, title: Check}}\n"
    )
}

fn compiled(text: &str) -> runbook_spec::CompiledRunbook {
    compile(&parse(text).unwrap(), &CompileContext::default()).unwrap()
}

fn first_error(text: &str) -> (String, String) {
    let spec = parse(text).unwrap();
    let diagnostics = match normalize(&spec) {
        Err(d) => d,
        Ok(_) => compile(&spec, &CompileContext::default()).expect_err("must not compile"),
    };
    let d = diagnostics.into_iter().next().unwrap();
    (d.code, d.path)
}

const BOTH: &str = r#"    schedule:
      - {id: nightly, cron: "0 2 * * *", timezone: America/Los_Angeles, inputs: {service: api}, catch_up: 1h}
    webhook:
      - id: on_deploy
        payload:
          status: {type: string, required: true}
          repository: {type: object, properties: {name: {type: string}}}
          deployment: {type: object, properties: {id: {type: integer}}}
        when: "${{ payload.status == 'success' }}"
        inputs:
          service: "${{ payload.repository.name }}"
          version: v1
        dedupe: "${{ payload.deployment.id }}"
"#;

#[test]
fn triggers_compile_hash_and_survive_restore() {
    let with = compiled(&source(BOTH));
    let triggers = &with.ir().triggers;
    assert_eq!(triggers.schedule[0].id, "nightly");
    assert_eq!(triggers.schedule[0].timezone, "America/Los_Angeles");
    assert_eq!(triggers.schedule[0].catch_up.unwrap().millis, 3_600_000);
    assert_eq!(triggers.webhook[0].id, "on_deploy");
    assert!(triggers.webhook[0].payload.is_some());
    let restored = runbook_spec::restore(with.snapshot_bytes()).expect("restores");
    assert_eq!(restored.hash(), with.hash());
    assert_eq!(restored.ir().triggers, with.ir().triggers);
    let without = compiled(&format!(
        "metadata: {{id: deploy-check}}\nspec:\n{INPUTS}  steps:\n    - {{id: check, title: Check}}\n"
    ));
    assert_ne!(with.hash(), without.hash());
    let stored = serde_json::to_string(without.ir()).unwrap();
    assert!(!stored.contains("\"triggers\":"), "{stored}");
}

#[test]
fn a_schedule_defaults_to_utc() {
    let c = compiled(&source(
        "    schedule:\n      - {id: hourly, cron: \"0 * * * *\", inputs: {service: api}}\n",
    ));
    assert_eq!(c.ir().triggers.schedule[0].timezone, "UTC");
}

#[test]
fn triggers_are_checked() {
    let cases = [
        (
            "    schedule:\n      - {id: a, cron: \"0 2 * *\", inputs: {service: api}}\n",
            "spec.invalid_cron",
            "/spec/triggers/schedule/0/cron",
        ),
        (
            "    schedule:\n      - {id: a, cron: \"0 2 * * *\", timezone: Mars/Base, inputs: {service: api}}\n",
            "spec.invalid_timezone",
            "/spec/triggers/schedule/0/timezone",
        ),
        (
            "    schedule:\n      - {id: a, cron: \"0 2 * * *\"}\n",
            "spec.trigger_input_invalid",
            "/spec/triggers/schedule/0/inputs/service",
        ),
        (
            "    schedule:\n      - {id: a, cron: \"0 2 * * *\", inputs: {service: api, region: eu}}\n",
            "spec.trigger_input_invalid",
            "/spec/triggers/schedule/0/inputs/region",
        ),
        (
            "    schedule:\n      - {id: a, cron: \"0 2 * * *\", inputs: {service: 5}}\n",
            "spec.trigger_input_invalid",
            "/spec/triggers/schedule/0/inputs/service",
        ),
        (
            "    schedule:\n      - {id: a, cron: \"0 2 * * *\", inputs: {service: api}}\n    webhook:\n      - {id: a}\n",
            "spec.duplicate_trigger",
            "/spec/triggers/webhook/0/id",
        ),
        (
            "    webhook:\n      - {id: a, payload: {n: {type: integer}}, when: \"${{ payload.n }}\"}\n",
            "spec.trigger_when_type",
            "/spec/triggers/webhook/0/when",
        ),
        (
            "    webhook:\n      - {id: a, inputs: {region: x}}\n",
            "spec.trigger_input_invalid",
            "/spec/triggers/webhook/0/inputs/region",
        ),
        (
            "    webhook:\n      - {id: Bad}\n",
            "spec.invalid_id",
            "/spec/triggers/webhook/0/id",
        ),
    ];
    for (triggers, code, path) in cases {
        assert_eq!(
            first_error(&source(triggers)),
            (code.to_owned(), path.to_owned()),
            "{triggers}"
        );
    }
    // An expression may only read the declared payload.
    let (code, path) = first_error(&source(
        "    webhook:\n      - {id: a, inputs: {service: \"${{ payload.name }}\"}}\n",
    ));
    assert_eq!(path, "/spec/triggers/webhook/0/inputs/service", "{code}");
}
