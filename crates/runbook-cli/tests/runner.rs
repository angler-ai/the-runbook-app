mod fake_api;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use fake_api::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::process::Command;

const INSTALL: &str = "ri_00000000000000000000000001";
const CREDENTIAL: &str = "secret-id";
const SECRET: &str = "test secret café";
const PUBLIC: &str = "hSDwCYkwp1R0i33ctD73Wg2_Og0mOBr066SpjqqbTmo";

#[test]
fn credential_types_needs_only_runner_scopes() {
    let home = tempfile::tempdir().unwrap();
    let manifests = json!({"items":[{"type":"api-key"}]});
    let (host, server) = serve(vec![
        Reply::json(
            200,
            json!({"org":{"id":ORG},"credential":{"scopes":["runner:read","runner:admin"]}}),
        ),
        Reply::json(200, json!({"install_id":INSTALL})),
        Reply::json(200, manifests.clone()),
    ]);
    let result = cli(
        &host,
        home.path(),
        &["runner", "credential", "types", "--json"],
        &token(),
        None,
    );
    assert!(result.status.success(), "{}", err(&result));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&result)).unwrap(),
        manifests
    );
    let requests = server.join().unwrap();
    assert_eq!(line(&requests[0]), "GET /v1/me");
    assert_eq!(line(&requests[1]), format!("GET /v1/orgs/{ORG}/runner"));
    assert_eq!(
        line(&requests[2]),
        format!("GET /v1/runners/{INSTALL}/proxy/runner/v1/credentials?presets=true")
    );
}

#[test]
fn credential_set_seals_locally_and_pins_fingerprint() {
    let vectors: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../api/runner-binding-vectors.json")).unwrap();
    let fixture = &vectors[0];
    let metadata = json!({
        "binding_hash": "lying-runner-api-hash", "secret_meta": { "key": { "version": 3 } },
        "type": fixture["type"], "config": fixture["config"], "groups": fixture["groups"],
    });
    let key = json!({"ingest_key_id":"ingest-1", "public_key":PUBLIC, "expires_at":"2099-01-01T00:00:00Z"});
    let home = tempfile::tempdir().unwrap();
    let (host, server) = serve(vec![
        Reply::json(200, metadata.clone()),
        Reply::json(200, key.clone()),
        Reply::json(200, metadata),
        Reply::json(200, key),
        Reply::json(200, json!({"version":4})),
    ]);
    let args = [
        "runner",
        "credential",
        "set",
        CREDENTIAL,
        "key",
        "--org",
        ORG,
        "--install",
        INSTALL,
        "--pin",
    ];
    let unpinned = cli(
        &host,
        home.path(),
        &args[..args.len() - 1],
        &token(),
        Some(SECRET),
    );
    assert!(!unpinned.status.success());
    assert!(err(&unpinned).contains("requires --binding-pin or --expected-config"));
    let mut mismatch = args.to_vec();
    mismatch.extend([
        "0000000000000000000000000000000000000000000000000000000000000000",
        "--binding-pin",
        fixture["hash"].as_str().unwrap(),
    ]);
    let refused = cli(&host, home.path(), &mismatch, &token(), Some(SECRET));
    assert!(!refused.status.success());
    assert!(
        err(&refused).contains("fingerprint does not match --pin"),
        "{}",
        err(&refused)
    );

    let pin = format!(
        "{:x}",
        Sha256::digest(URL_SAFE_NO_PAD.decode(PUBLIC).unwrap())
    );
    let mut matching = args.to_vec();
    matching.push(&pin);
    let expected = home.path().join("expected.json");
    std::fs::write(&expected, fixture.to_string()).unwrap();
    matching.extend(["--expected-config", expected.to_str().unwrap()]);
    let sealed = cli(&host, home.path(), &matching, &token(), Some(SECRET));
    assert!(sealed.status.success(), "{}", err(&sealed));
    assert!(!out(&sealed).contains(SECRET));

    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        line(&requests[2]),
        format!("GET /v1/runners/{INSTALL}/proxy/runner/v1/credentials/{CREDENTIAL}")
    );
    assert_eq!(
        line(&requests[4]),
        format!("POST /v1/runners/{INSTALL}/proxy/runner/v1/credentials/{CREDENTIAL}/secrets/key")
    );
    assert!(
        requests
            .iter()
            .all(|request| header(request, "x-runbook-csrf") == Some("1"))
    );
    assert!(requests.iter().all(|request| !request.contains(SECRET)));
    let body = body_of(&requests[4]);
    let blob = body["blob"].as_str().unwrap();
    let envelope: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(blob).unwrap()).unwrap();
    assert_eq!(envelope["credential_id"], CREDENTIAL);
    assert_eq!(envelope["org_id"], ORG);
    assert_eq!(envelope["field"], "key");
    assert_eq!(envelope["expected_version"], 3);
    assert_eq!(envelope["binding_hash"], fixture["hash"]);

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root
        .join("runner/node_modules/@hpke/core/package.json")
        .exists()
    {
        eprintln!("Skipping TypeScript vault interop: runner npm dependencies are unavailable");
        return;
    }
    let output = Command::new("node")
        .args([
            "--conditions=development",
            "crates/runbook-cli/tests/open-runner-seal.mjs",
            blob,
        ])
        .current_dir(root)
        .output();
    let output = match output {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Skipping TypeScript vault interop: node is unavailable");
            return;
        }
        result => result.unwrap(),
    };
    assert!(output.status.success(), "{}", err(&output));
}

#[test]
fn runner_scopes_off_by_default_at_consent() {
    use runbook_core::credentials::Scope;
    assert!(
        Scope::RUNNER
            .iter()
            .all(|scope| !Scope::ALL.contains(scope))
    );

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !root
        .join("auth/node_modules/better-auth/package.json")
        .exists()
    {
        eprintln!("Skipping OAuth consent check: auth npm dependencies are unavailable");
        return;
    }
    let result = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "
            import assert from 'node:assert/strict';
            import { registerHooks } from 'node:module';
            registerHooks({
                resolve(specifier, context, nextResolve) {
                    if (specifier === './config' && context.parentURL?.endsWith('/auth/src/oauth.ts')) {
                        return nextResolve('./config.ts', context);
                    }
                    return nextResolve(specifier, context);
                },
            });
            const { OPT_IN, SCOPES } = await import('./auth/src/oauth.ts');
            for (const scope of ['runner:read', 'runner:admin']) {
                assert.ok(SCOPES.some(([name]) => name === scope));
                assert.ok(OPT_IN.has(scope));
            }
        ",
        ])
        .current_dir(root)
        .output();
    match result {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("Skipping OAuth consent check: node is unavailable");
        }
        result => {
            let output = result.unwrap();
            assert!(output.status.success(), "{}", err(&output));
        }
    }
}

#[test]
fn credential_set_verifies_the_ingest_key_against_the_direct_core_identity() {
    use ed25519_dalek::Signer;
    let vectors: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../api/runner-binding-vectors.json")).unwrap();
    let fixture = &vectors[0];
    let metadata = json!({
        "type": fixture["type"], "config": fixture["config"], "groups": fixture["groups"],
        "binding_hash": "untrusted", "secret_meta": {},
    });
    let key = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let expiry = "2099-01-01T00:00:00Z";
    let fields = [
        "runbook-runner/ingest-key/v1",
        ORG,
        INSTALL,
        "ik_fixture",
        PUBLIC,
        expiry,
    ];
    let mut message = Vec::new();
    for field in fields {
        message.extend_from_slice(&(field.len() as u32).to_be_bytes());
        message.extend_from_slice(field.as_bytes());
    }
    let signed = json!({
        "ingest_key_id": "ik_fixture", "public_key": PUBLIC, "expires_at": expiry,
        "org_id": ORG, "install_id": INSTALL,
        "identity_signature": URL_SAFE_NO_PAD.encode(key.sign(&message).to_bytes()),
    });
    let mut substituted = signed.clone();
    substituted["public_key"] = json!(URL_SAFE_NO_PAD.encode([8; 32]));
    let identity = json!({
        "install_id": INSTALL, "vault_identity_key": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
    });
    let (host, server) = serve(vec![
        Reply::json(200, metadata.clone()),
        Reply::json(200, substituted),
        Reply::json(200, identity.clone()),
        Reply::json(200, metadata),
        Reply::json(200, signed),
        Reply::json(200, identity),
        Reply::json(200, json!({"version":1})),
    ]);
    let home = tempfile::tempdir().unwrap();
    let args = [
        "runner",
        "credential",
        "set",
        CREDENTIAL,
        "key",
        "--org",
        ORG,
        "--install",
        INSTALL,
        "--binding-pin",
        fixture["hash"].as_str().unwrap(),
    ];
    let denied = cli(&host, home.path(), &args, &token(), Some(SECRET));
    assert!(!denied.status.success());
    assert!(
        err(&denied).contains("verify the vault identity"),
        "{}",
        err(&denied)
    );
    let accepted = cli(&host, home.path(), &args, &token(), Some(SECRET));
    assert!(accepted.status.success(), "{}", err(&accepted));
    let requests = server.join().unwrap();
    for request in [&requests[2], &requests[5]] {
        assert_eq!(line(request), format!("GET /v1/orgs/{ORG}/runner"));
    }
    assert!(line(&requests[6]).ends_with("/secrets/key"));
}

#[test]
fn a_correct_ingest_pin_does_not_override_the_confirmed_destination() {
    let vectors: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../api/runner-binding-vectors.json")).unwrap();
    let fixture = &vectors[0];
    let mut config = fixture["config"].clone();
    config["hosts"] = json!(["different.example"]);
    let metadata = json!({
        "type": fixture["type"], "config": config, "groups": fixture["groups"],
        "binding_hash": fixture["hash"], "secret_meta": {},
    });
    let (host, server) = serve(vec![Reply::json(200, metadata)]);
    let home = tempfile::tempdir().unwrap();
    let pin = format!(
        "{:x}",
        Sha256::digest(URL_SAFE_NO_PAD.decode(PUBLIC).unwrap())
    );
    let refused = cli(
        &host,
        home.path(),
        &[
            "runner",
            "credential",
            "set",
            CREDENTIAL,
            "key",
            "--org",
            ORG,
            "--install",
            INSTALL,
            "--pin",
            &pin,
            "--binding-pin",
            fixture["hash"].as_str().unwrap(),
        ],
        &token(),
        Some(SECRET),
    );
    assert!(!refused.status.success());
    assert!(
        err(&refused).contains("destination does not match --binding-pin"),
        "{}",
        err(&refused)
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn credential_create_verifies_the_id_before_printing_it() {
    use ed25519_dalek::Signer;
    let vectors: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../../../api/runner-binding-vectors.json")).unwrap();
    let fixture = &vectors[0];
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let identity = json!({
        "install_id": INSTALL, "vault_identity_key": URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes()),
    });
    let mut statement = Vec::new();
    for part in [
        "runbook-runner/credential-create/v1",
        ORG,
        INSTALL,
        CREDENTIAL,
        "Created credential",
        fixture["hash"].as_str().unwrap(),
        "read",
        "\"all\"",
    ] {
        statement.extend_from_slice(&(part.len() as u32).to_be_bytes());
        statement.extend_from_slice(part.as_bytes());
    }
    let receipt = URL_SAFE_NO_PAD.encode(key.sign(&statement).to_bytes());
    let (host, server) = serve(vec![
        Reply::json(
            201,
            json!({ "id": "unrelated", "creation_receipt": receipt }),
        ),
        Reply::json(200, identity.clone()),
        Reply::json(
            201,
            json!({ "id": CREDENTIAL, "creation_receipt": receipt }),
        ),
        Reply::json(200, identity),
        Reply::json(
            202,
            json!({ "id": "proposed", "expires_at": "2099-01-01T00:00:00Z" }),
        ),
    ]);
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("credential.json");
    std::fs::write(
        &file,
        json!({
            "name": "Created credential", "type": fixture["type"], "config": fixture["config"],
            "groups": fixture["groups"], "label": "read", "sharing": { "projects": "all" },
        })
        .to_string(),
    )
    .unwrap();
    let args = [
        "runner",
        "credential",
        "create",
        "--org",
        ORG,
        "--install",
        INSTALL,
        "--file",
        file.to_str().unwrap(),
    ];
    let rejected = cli(&host, home.path(), &args, &token(), None);
    assert!(!rejected.status.success());
    assert!(err(&rejected).contains("vault credential receipt"));
    assert!(out(&rejected).is_empty());
    let accepted = cli(&host, home.path(), &args, &token(), None);
    assert!(accepted.status.success(), "{}", err(&accepted));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&out(&accepted)).unwrap()["id"],
        CREDENTIAL
    );
    let proposed = cli(&host, home.path(), &args, &token(), None);
    assert!(proposed.status.success(), "{}", err(&proposed));
    let proposal: serde_json::Value = serde_json::from_str(&out(&proposed)).unwrap();
    assert_eq!(proposal["proposal_id"], "proposed");
    assert!(proposal.get("id").is_none());
    assert_eq!(server.join().unwrap().len(), 5);
}

#[test]
fn runner_status_shows_the_core_vault_identity_fingerprint() {
    let (host, server) = serve(vec![Reply::json(
        200,
        json!({
            "install_id": INSTALL, "vault_identity_key": URL_SAFE_NO_PAD.encode([0; 32]),
        }),
    )]);
    let home = tempfile::tempdir().unwrap();
    let output = cli(
        &host,
        home.path(),
        &["runner", "status", "--org", ORG, "--json"],
        &token(),
        None,
    );
    assert!(output.status.success(), "{}", err(&output));
    let status: serde_json::Value = serde_json::from_str(&out(&output)).unwrap();
    assert_eq!(
        status["vault_identity_fingerprint"],
        "66687aadf862bd776c8fc18b8e9f8e20089714856ee233b3902a591d0d5f2925",
    );
    assert_eq!(
        line(&server.join().unwrap()[0]),
        format!("GET /v1/orgs/{ORG}/runner")
    );
}

#[test]
fn runner_configuration_uses_the_existing_admin_routes() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("change.json");
    let file_arg = file.to_str().unwrap();
    let project = "prj_00000000000000000000000001";
    let group = "rg_00000000000000000000000001";
    let cases = [
        (vec!["provider", "list"], "GET providers", None),
        (
            vec!["provider", "get", "provider"],
            "GET providers/provider",
            None,
        ),
        (
            vec!["provider", "create", "--file", file_arg],
            "POST providers",
            Some(json!({"credential_id":"cred","auth":"stored"})),
        ),
        (
            vec!["provider", "update", "provider", "--file", file_arg],
            "PATCH providers/provider",
            Some(json!({"credential_id":"cred"})),
        ),
        (
            vec!["provider", "delete", "provider"],
            "DELETE providers/provider",
            None,
        ),
        (vec!["profile", "list"], "GET profiles", None),
        (
            vec!["profile", "get", "profile"],
            "GET profiles/profile",
            None,
        ),
        (
            vec!["profile", "create", "--file", file_arg],
            "POST profiles",
            Some(json!({"model":"fixture"})),
        ),
        (
            vec!["profile", "update", "profile", "--file", file_arg],
            "PATCH profiles/profile",
            Some(json!({"provider_id":"provider"})),
        ),
        (
            vec!["profile", "delete", "profile"],
            "DELETE profiles/profile",
            None,
        ),
        (vec!["mcp-server", "list"], "GET mcp-servers", None),
        (
            vec!["mcp-server", "get", "mcp"],
            "GET mcp-servers/mcp",
            None,
        ),
        (
            vec!["mcp-server", "create", "--file", file_arg],
            "POST mcp-servers",
            Some(json!({"url":"https://example.com/mcp"})),
        ),
        (
            vec!["mcp-server", "update", "mcp", "--file", file_arg],
            "PATCH mcp-servers/mcp",
            Some(json!({"name":"new"})),
        ),
        (
            vec!["mcp-server", "delete", "mcp"],
            "DELETE mcp-servers/mcp",
            None,
        ),
        (
            vec!["mcp-server", "discover", "mcp"],
            "POST mcp-servers/mcp/discover",
            Some(json!({})),
        ),
        (
            vec!["mcp-server", "pin", "mcp", "snapshot"],
            "POST mcp-servers/mcp/pin",
            Some(json!({"snapshot_id":"snapshot"})),
        ),
        (
            vec!["mcp-server", "label-tools", "mcp", "--file", file_arg],
            "PATCH mcp-servers/mcp",
            Some(json!({"tool_labels":{"lookup":"read"}})),
        ),
        (vec!["policy", "get"], "GET policy", None),
        (
            vec!["policy", "update", "--file", file_arg],
            "PUT policy",
            Some(json!({"org_cost_per_day_usd":2})),
        ),
        (vec!["group", "list"], "GET groups", None),
        (
            vec!["group", "get", group],
            "GET groups/rg_00000000000000000000000001",
            None,
        ),
        (
            vec!["group", "update", group, "--file", file_arg],
            "PATCH groups/rg_00000000000000000000000001",
            Some(json!({"concurrency":2})),
        ),
        (
            vec!["credential", "types"],
            "GET credentials?presets=true",
            None,
        ),
        (
            vec!["credential", "label", "cred", "read"],
            "PATCH credentials/cred",
            Some(json!({"label":"read"})),
        ),
        (
            vec!["credential", "share", "cred", "--file", file_arg],
            "PATCH credentials/cred",
            Some(json!({"sharing":{"projects":[project]}})),
        ),
        (
            vec!["credential", "test", "cred"],
            "POST credentials/cred/test",
            Some(json!({})),
        ),
        (
            vec![
                "agent-profiles",
                "agent",
                "--project",
                project,
                "--file",
                file_arg,
            ],
            "PATCH projects/prj_00000000000000000000000001/agents/agent",
            Some(json!({"profiles":["default"]})),
        ),
        (
            vec!["group", "projects", group, "--file", file_arg],
            "PUT /v1/orgs/{org}/runner/groups/rg_00000000000000000000000001/projects",
            Some(json!({"project_ids":[project]})),
        ),
    ];
    for (args, expected, body) in cases {
        let (method, route) = expected.split_once(' ').unwrap();
        if let Some(body) = &body {
            std::fs::write(&file, body.to_string()).unwrap();
        }
        let response = json!({"fixture":true});
        let (host, server) = serve(vec![Reply::json(200, response.clone())]);
        let mut command = vec!["runner", "--org", ORG, "--json"];
        if !route.starts_with('/') {
            command.extend(["--install", INSTALL]);
        }
        command.extend(args);
        let result = cli(&host, home.path(), &command, &token(), None);
        assert!(result.status.success(), "{command:?}: {}", err(&result));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&out(&result)).unwrap(),
            response
        );
        let requests = server.join().unwrap();
        let path = if route.starts_with('/') {
            route.replace("{org}", ORG)
        } else {
            format!("/v1/runners/{INSTALL}/proxy/runner/v1/{route}")
        };
        assert_eq!(line(&requests[0]), format!("{method} {path}"));
        if let Some(body) = body {
            assert_eq!(body_of(&requests[0]), body);
        }
    }
}

#[test]
fn runner_configuration_reports_mfa_step_up_and_revoked_credentials() {
    let home = tempfile::tempdir().unwrap();
    let change = home.path().join("provider.json");
    std::fs::write(&change, r#"{"name":"fixture","credential_id":"cred"}"#).unwrap();
    let (host, server) = serve(vec![
        Reply::json(
            403,
            json!({"code":"auth.mfa_step_up","detail":"Second factor required"}),
        ),
        Reply::json(
            409,
            json!({"error":{"code":"credential.revoked","message":"the credential is revoked"}}),
        ),
    ]);
    let base = ["runner", "--install", INSTALL];
    let denied = cli(
        &host,
        home.path(),
        &[
            base.as_slice(),
            &["provider", "create", "--file", change.to_str().unwrap()],
        ]
        .concat(),
        &token(),
        None,
    );
    assert!(!denied.status.success());
    let error = err(&denied);
    assert!(
        error.contains("auth.mfa_step_up: Second factor required"),
        "{error}"
    );
    assert!(error.contains("passkey or authenticator code"), "{error}");
    let revoked = cli(
        &host,
        home.path(),
        &[base.as_slice(), &["credential", "test", "cred"]].concat(),
        &token(),
        None,
    );
    assert!(!revoked.status.success());
    assert!(err(&revoked).contains("credential.revoked: the credential is revoked"));
    server.join().unwrap();
}

#[test]
fn provider_configuration_rejects_a_plaintext_key_before_sending() {
    let home = tempfile::tempdir().unwrap();
    let change = home.path().join("provider.json");
    std::fs::write(&change, r#"{"name":"fixture","api_key":"must-stay-local"}"#).unwrap();
    let (host, server) = serve(vec![]);
    let result = cli(
        &host,
        home.path(),
        &[
            "runner",
            "--install",
            INSTALL,
            "provider",
            "create",
            "--file",
            change.to_str().unwrap(),
        ],
        &token(),
        None,
    );
    assert!(!result.status.success());
    assert!(err(&result).contains("runbook runner credential set"));
    assert!(!err(&result).contains("must-stay-local"));
    assert!(server.join().unwrap().is_empty());
}
