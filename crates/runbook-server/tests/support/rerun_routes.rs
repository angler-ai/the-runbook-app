//! B9 HTTP: `POST /v1/executions/{exe}/rerun` starts a new run from the
//! original's start facts (PRD §6, B9 plan).
use super::*;

async fn rerun(f: &Fixture, exe: &str, body: Value, key: Option<&str>) -> Reply {
    call(
        &f.app,
        &f.token,
        "POST",
        &format!("/v1/executions/{exe}/rerun"),
        body,
        key,
        None,
    )
    .await
}

fn with_region(slug: &str, title: &str, region: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: {slug}, title: {title}}}\nspec:\n  inputs:\n    service: {{type: string, default: api}}\n{region}  fields:\n    severity: {{type: integer, default: 1}}\n  steps: [{{id: work, title: {title}}}]\n"
    )
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn rerun_pins_original_or_explicit_version() {
    with_fixture(async |f, _| {
        let rb = f.create("deploy").await;
        let v1 = f.publish(&rb, spec("deploy", "One", false), "v1").await;
        let original = f
            .start(
                json!({"runbook":"deploy","inputs":{"service":"web"}}),
                Some("orig"),
            )
            .await;
        assert_eq!(original.status, 201, "{}", original.body);
        let exe = original.body["id"].as_str().unwrap().to_owned();
        let v2 = f.publish(&rb, spec("deploy", "Two", false), "v2").await;
        assert_ne!(v1["runbook_version_id"], v2["runbook_version_id"]);

        assert_eq!(rerun(f, &exe, json!({}), None).await.status, 428);
        // Omitted version: the original v1 still reruns v1 after v2 publishes.
        let same = rerun(f, &exe, json!({}), Some("again")).await;
        assert_eq!(same.status, 201, "{}", same.body);
        assert_ne!(same.body["id"], original.body["id"], "a rerun is a new run");
        assert_eq!(same.body["runbook_version_id"], v1["runbook_version_id"]);
        assert_eq!(same.body["rerun_of"], exe.as_str());
        assert_eq!(
            same.body["inputs"]["service"], "web",
            "original inputs reused"
        );
        // D20: the same key and body replays; a changed body is 409.
        let replay = rerun(f, &exe, json!({}), Some("again")).await;
        assert_eq!(replay.status, 201);
        assert_eq!(replay.body["id"], same.body["id"]);
        assert_eq!(
            rerun(f, &exe, json!({"version":2}), Some("again"))
                .await
                .status,
            409
        );
        // Explicit 2 selects v2.
        let two = rerun(f, &exe, json!({"version":2}), Some("two")).await;
        assert_eq!(two.status, 201, "{}", two.body);
        assert_eq!(two.body["runbook_version_id"], v2["runbook_version_id"]);
        // A foreign or unknown selector is the same 404 as a nonexistent one.
        let other = f.create("other").await;
        let foreign = f
            .publish(&other, spec("other", "Foreign", false), "foreign")
            .await;
        for version in [json!(99), foreign["runbook_version_id"].clone()] {
            let r = rerun(
                f,
                &exe,
                json!({"version":version}),
                Some(&format!("bad-{version}")),
            )
            .await;
            assert_eq!(r.status, 404, "{}", r.body);
            assert_eq!(r.body["code"], "runbook.version_not_found");
        }
        // An unknown run is 404.
        let missing = rerun(f, &format!("exe_{:026}", 7), json!({}), Some("missing")).await;
        assert_eq!(missing.status, 404, "{}", missing.body);
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn rerun_validates_original_initial_data() {
    with_fixture(async |f, _| {
        let rb = f.create("checks").await;
        f.publish(&rb, with_region("checks", "One", ""), "v1").await;
        let original = f
            .start(
                json!({"runbook":"checks","fields":{"severity":3}}),
                Some("orig"),
            )
            .await;
        assert_eq!(original.status, 201, "{}", original.body);
        let exe = original.body["id"].as_str().unwrap().to_owned();
        // Later field updates do not leak into a rerun: it uses initial fields.
        let required = with_region(
            "checks",
            "Two",
            "    region: {type: string, required: true}\n",
        );
        f.publish(&rb, required, "v2").await;
        let rejected = rerun(f, &exe, json!({"version":2}), Some("v2")).await;
        assert_eq!(rejected.status, 422, "{}", rejected.body);
        assert_eq!(rejected.body["code"], "execution.inputs_invalid");
        assert!(
            rejected.body["errors"]
                .to_string()
                .contains("inputs.region"),
            "{}",
            rejected.body
        );
        let optional = with_region(
            "checks",
            "Three",
            "    region: {type: string, default: eu}\n",
        );
        f.publish(&rb, optional, "v3").await;
        let defaulted = rerun(f, &exe, json!({"version":3}), Some("v3")).await;
        assert_eq!(defaulted.status, 201, "{}", defaulted.body);
        assert_eq!(
            defaulted.body["inputs"]["region"], "eu",
            "new optional default applies"
        );
        assert_eq!(
            defaulted.body["fields"]["severity"], 3,
            "original initial fields"
        );
    })
    .await;
}

/// R28: a rerun keeps the caller's start tags and takes spec tags from the
/// target version, never the original version's spec tags.
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn rerun_takes_spec_tags_from_target_version() {
    with_fixture(async |f, _| {
        let tagged = |title: &str, tag: &str| {
            format!(
                "apiVersion: therunbook.app/v1\nkind: Runbook\nmetadata: {{id: tagged, title: {title}}}\nspec:\n  execution: {{tags: [{tag}]}}\n  steps: [{{id: work, title: {title}}}]\n"
            )
        };
        let rb = f.create("tagged").await;
        f.publish(&rb, tagged("One", "legacy"), "v1").await;
        let original = f
            .start(json!({"runbook":"tagged","tags":["user"]}), Some("orig"))
            .await;
        assert_eq!(original.status, 201, "{}", original.body);
        assert_eq!(original.body["tags"], json!(["legacy", "user"]));
        let exe = original.body["id"].as_str().unwrap().to_owned();
        f.publish(&rb, tagged("Two", "current"), "v2").await;
        let same = rerun(f, &exe, json!({}), Some("same")).await;
        assert_eq!(same.status, 201, "{}", same.body);
        assert_eq!(same.body["tags"], json!(["legacy", "user"]), "same version");
        let newer = rerun(f, &exe, json!({"version":2}), Some("newer")).await;
        assert_eq!(newer.status, 201, "{}", newer.body);
        assert_eq!(
            newer.body["tags"],
            json!(["current", "user"]),
            "v1's spec tag must not carry over"
        );
    })
    .await;
}
