use crate::agent::Env;
use crate::api::{ApiClient, Reply, Request};
use crate::cliutil::{Exit, OK, api_failed, client, fail, idempotency_key, print_json, usage};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use clap::{Args, Subcommand};
use hpke::{
    Deserializable, Kem, OpModeS, Serializable, aead::AesGcm256, kdf::HkdfSha256,
    kem::X25519HkdfSha256, setup_sender,
};
use runbook_mcp::ops::runner_segment;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use zeroize::Zeroizing;

mod config;
use config::{CatalogCommand, GroupCommand, McpServerCommand, PolicyCommand};

const INFO: &str = "runbook-runner/seal/v1";
const MAX_SECRET: usize = 64 * 1024;

#[derive(Args, Debug)]
pub struct RunnerArgs {
    #[arg(
        long,
        global = true,
        help = "Organization ID (defaults to the only visible organization)"
    )]
    org: Option<String>,
    #[arg(long, global = true, help = "Runner install ID for proxied operations")]
    install: Option<String>,
    #[arg(
        long,
        global = true,
        help = "Output JSON (including the token for bootstrap)"
    )]
    pub json: bool,
    #[command(subcommand)]
    command: RunnerCommand,
}

#[derive(Subcommand, Debug)]
enum RunnerCommand {
    #[command(about = "Issue a one-time runner bootstrap token")]
    Bootstrap {
        #[arg(long, help = "Runner API HTTPS base URL")]
        url: String,
    },
    #[command(about = "Show the organization's runner installation status")]
    Status,
    #[command(about = "Register or replace the vault identity as an organization owner")]
    VaultIdentity {
        #[arg(long, help = "Base64url Ed25519 public key printed by the vault")]
        public_key: String,
    },
    #[command(subcommand, about = "Manage runner credential metadata and secrets")]
    Credential(CredentialCommand),
    #[command(
        subcommand,
        about = "Manage model providers. Enter API keys with runner credential set."
    )]
    Provider(CatalogCommand),
    #[command(subcommand, about = "Manage model profiles")]
    Profile(CatalogCommand),
    #[command(
        subcommand,
        about = "Manage upstream MCP servers and trusted tool snapshots"
    )]
    McpServer(McpServerCommand),
    #[command(subcommand, about = "Manage runner policy and budgets")]
    Policy(PolicyCommand),
    #[command(
        subcommand,
        about = "Manage runner groups, limits and allowed projects"
    )]
    Group(GroupCommand),
    /// Replace the profiles an agent is allowed to use.
    AgentProfiles {
        agent: String,
        #[arg(long, env = "RUNBOOK_PROJECT")]
        project: String,
        #[arg(long, help = "JSON file containing a profiles array of profile IDs")]
        file: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum CredentialCommand {
    #[command(about = "Create runner credential metadata from JSON")]
    Create {
        #[arg(long, help = "JSON file containing metadata only")]
        file: std::path::PathBuf,
    },
    #[command(about = "Seal a secret from stdin or a hidden prompt and store it in the vault")]
    Set {
        credential: String,
        field: String,
        #[arg(
            long,
            help = "Expected SHA-256 hex fingerprint of the vault ingest key"
        )]
        pin: Option<String>,
        #[arg(
            long,
            help = "Expected base64url SHA-256 hash of the credential destination binding"
        )]
        binding_pin: Option<String>,
        #[arg(
            long,
            conflicts_with = "binding_pin",
            help = "Local JSON file with the expected type, config and groups"
        )]
        expected_config: Option<PathBuf>,
    },
    #[command(about = "Revoke a runner credential")]
    Revoke { credential: String },
    /// List credential types and their secret field names.
    Types,
    /// Test a stored credential against its configured destination.
    Test { credential: String },
    /// Set the credential's access label.
    Label {
        credential: String,
        label: CredentialLabel,
    },
    /// Replace project sharing from JSON containing sharing.projects.
    Share {
        credential: String,
        #[arg(long)]
        file: PathBuf,
    },
}

#[derive(clap::ValueEnum, Clone, Debug)]
enum CredentialLabel {
    Read,
    Write,
    Destructive,
}

#[derive(Args, Debug)]
pub struct AgentsArgs {
    #[arg(
        long,
        global = true,
        help = "Organization ID (defaults to the only visible organization)"
    )]
    org: Option<String>,
    #[arg(long, global = true, help = "Runner install ID")]
    install: Option<String>,
    #[arg(long, global = true, env = "RUNBOOK_PROJECT", help = "Project ID")]
    project: Option<String>,
    #[arg(long, global = true, help = "Output JSON")]
    pub json: bool,
    #[command(subcommand)]
    command: AgentCommand,
}

#[derive(Subcommand, Debug)]
enum AgentCommand {
    #[command(about = "List the project's runner agents")]
    List,
    #[command(about = "Show a runner agent")]
    Get { agent: String },
    #[command(about = "Create a runner agent from JSON")]
    Create {
        #[arg(long, help = "JSON file containing the agent configuration")]
        file: std::path::PathBuf,
    },
    #[command(about = "Update a runner agent from a JSON patch")]
    Update {
        agent: String,
        #[arg(long, help = "JSON file containing the agent fields to change")]
        file: std::path::PathBuf,
    },
    #[command(about = "Pause a runner agent")]
    Pause { agent: String },
}

fn segment(value: &str) -> Exit<&str> {
    runner_segment(value).map_err(|e| usage(e.to_string()))
}

fn install(api: &ApiClient, org: Option<&str>, id: Option<&str>) -> Exit<String> {
    if let Some(id) = id {
        return crate::cliutil::parse_id::<runbook_model::RunnerInstallId>(id, "runner install");
    }
    let org = crate::admin::resolve_org(api, org)?;
    let status = api
        .get_json(&format!("/v1/orgs/{org}/runner"))
        .map_err(api_failed)?;
    status["install_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| fail("no runner is registered"))
}

fn proxy(
    api: &ApiClient,
    install: &str,
    method: &str,
    suffix: &str,
    body: Option<&Value>,
) -> Exit<Value> {
    proxy_reply(api, install, method, suffix, body).map(|reply| reply.body)
}

fn proxy_reply(
    api: &ApiClient,
    install: &str,
    method: &str,
    suffix: &str,
    body: Option<&Value>,
) -> Exit<Reply> {
    let path = format!("/v1/runners/{}/proxy/runner/v1/{suffix}", segment(install)?);
    let key = if method == "POST" {
        Some(idempotency_key(&None)?)
    } else {
        None
    };
    api.send(&Request {
        method,
        path: &path,
        query: &[],
        body,
        idempotency_key: key.as_deref(),
        if_match: None,
    })
    .map_err(api_failed)
}

fn file_json(path: &std::path::Path) -> Exit<Value> {
    let bytes =
        std::fs::read(path).map_err(|e| usage(format!("cannot read {}: {e}", path.display())))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|e| usage(format!("invalid JSON: {e}")))?;
    if !value.is_object() {
        return Err(usage("expected a JSON object"));
    }
    Ok(value)
}

fn display(value: &Value, as_json: bool) {
    if as_json || !value.is_null() {
        print_json(value);
    }
}

pub fn handle_runner(env: &Env, args: RunnerArgs) -> i32 {
    run_runner(env, args).unwrap_or_else(|exit| exit)
}

fn run_runner(env: &Env, args: RunnerArgs) -> Exit<i32> {
    let api = client(env)?;
    let org = args.org.as_deref();
    let result = match args.command {
        RunnerCommand::Bootstrap { url } => {
            let org = crate::admin::resolve_org(&api, org)?;
            let reply = api
                .post_json(
                    &format!("/v1/orgs/{org}/runner/bootstrap-tokens"),
                    &idempotency_key(&None)?,
                    &json!({"url":url}),
                )
                .map_err(api_failed)?;
            if !args.json {
                println!(
                    "{}",
                    reply["token"]
                        .as_str()
                        .ok_or_else(|| fail("missing bootstrap token"))?
                );
                return Ok(OK);
            }
            reply
        }
        RunnerCommand::Status => {
            let org = crate::admin::resolve_org(&api, org)?;
            let mut status = api
                .get_json(&format!("/v1/orgs/{org}/runner"))
                .map_err(api_failed)?;
            if let Some(key) = status["vault_identity_key"].as_str() {
                let raw = B64
                    .decode(key)
                    .map_err(|_| fail("invalid core vault identity"))?;
                status["vault_identity_fingerprint"] = json!(format!("{:x}", Sha256::digest(raw)));
            }
            status
        }
        RunnerCommand::VaultIdentity { public_key } => {
            let org = crate::admin::resolve_org(&api, org)?;
            let body = json!({ "public_key": public_key });
            api.send(&Request {
                method: "PUT",
                path: &format!("/v1/orgs/{org}/runner/vault-identity"),
                query: &[],
                body: Some(&body),
                idempotency_key: None,
                if_match: None,
            })
            .map_err(api_failed)?
            .body
        }
        RunnerCommand::Provider(command) => command.run(
            &api,
            &install(&api, org, args.install.as_deref())?,
            "providers",
        )?,
        RunnerCommand::Profile(command) => command.run(
            &api,
            &install(&api, org, args.install.as_deref())?,
            "profiles",
        )?,
        RunnerCommand::McpServer(command) => {
            command.run(&api, &install(&api, org, args.install.as_deref())?)?
        }
        RunnerCommand::Policy(command) => {
            command.run(&api, &install(&api, org, args.install.as_deref())?)?
        }
        RunnerCommand::Group(command) => command.run(&api, org, args.install.as_deref())?,
        RunnerCommand::AgentProfiles {
            agent,
            project,
            file,
        } => config::agent_profiles(
            &api,
            &install(&api, org, args.install.as_deref())?,
            &project,
            &agent,
            file,
        )?,
        RunnerCommand::Credential(command) => {
            let install = install(&api, org, args.install.as_deref())?;
            match command {
                CredentialCommand::Types => {
                    proxy(&api, &install, "GET", "credentials?presets=true", None)?
                }
                CredentialCommand::Test { credential } => proxy(
                    &api,
                    &install,
                    "POST",
                    &format!("credentials/{}/test", segment(&credential)?),
                    Some(&json!({})),
                )?,
                CredentialCommand::Label { credential, label } => {
                    let label = match label {
                        CredentialLabel::Read => "read",
                        CredentialLabel::Write => "write",
                        CredentialLabel::Destructive => "destructive",
                    };
                    proxy(
                        &api,
                        &install,
                        "PATCH",
                        &format!("credentials/{}", segment(&credential)?),
                        Some(&json!({"label": label})),
                    )?
                }
                CredentialCommand::Share { credential, file } => {
                    let change = file_json(&file)?;
                    let projects = &change["sharing"]["projects"];
                    runbook_mcp::ops::check_runner_sharing(projects)
                        .map_err(|error| usage(error.to_string()))?;
                    proxy(
                        &api,
                        &install,
                        "PATCH",
                        &format!("credentials/{}", segment(&credential)?),
                        Some(&json!({"sharing": {"projects": projects}})),
                    )?
                }
                CredentialCommand::Create { file } => {
                    let expected = file_json(&file)?;
                    let reply =
                        proxy_reply(&api, &install, "POST", "credentials", Some(&expected))?;
                    if reply.status == 202 {
                        let proposal_id = reply.body["id"]
                            .as_str()
                            .ok_or_else(|| fail("missing credential proposal ID"))?;
                        json!({ "proposal_id": proposal_id, "expires_at": reply.body["expires_at"] })
                    } else {
                        let org = crate::admin::resolve_org(&api, org)?;
                        let anchor = core_vault_identity(&api, &org, &install)?;
                        verify_credential_receipt(&reply.body, &expected, &anchor, &org, &install)
                            .map_err(fail)?;
                        reply.body
                    }
                }
                CredentialCommand::Revoke { credential } => {
                    let credential = segment(&credential)?;
                    proxy(
                        &api,
                        &install,
                        "POST",
                        &format!("credentials/{credential}/revoke"),
                        Some(&json!({})),
                    )?
                }
                CredentialCommand::Set {
                    credential,
                    field,
                    pin,
                    binding_pin,
                    expected_config,
                } => {
                    let binding_pin = if let Some(path) = expected_config {
                        let file = std::fs::File::open(path)
                            .map_err(|_| fail("cannot open --expected-config"))?;
                        let expected: Value = serde_json::from_reader(file.take(64 * 1024))
                            .map_err(|_| fail("invalid --expected-config JSON"))?;
                        let binding = destination_binding(&expected).map_err(fail)?;
                        Some(destination_hash(&expected["type"], &binding))
                    } else {
                        binding_pin
                    };
                    let org = crate::admin::resolve_org(&api, org)?;
                    set_credential(
                        &api,
                        &install,
                        &org,
                        &credential,
                        &field,
                        pin.as_deref(),
                        binding_pin.as_deref(),
                    )?
                }
            }
        }
    };
    display(&result, args.json);
    Ok(OK)
}

pub fn handle_agents(env: &Env, args: AgentsArgs) -> i32 {
    run_agents(env, args).unwrap_or_else(|exit| exit)
}

fn run_agents(env: &Env, args: AgentsArgs) -> Exit<i32> {
    let api = client(env)?;
    let install = install(&api, args.org.as_deref(), args.install.as_deref())?;
    let project = crate::cliutil::parse_id::<runbook_model::ProjectId>(
        args.project
            .as_deref()
            .ok_or_else(|| usage("pass --project <project_id>"))?,
        "project",
    )?;
    let path = format!("projects/{project}/agents");
    let (method, suffix, body) = match args.command {
        AgentCommand::List => ("GET", path, None),
        AgentCommand::Get { agent } => ("GET", format!("{path}/{}", segment(&agent)?), None),
        AgentCommand::Create { file } => ("POST", path, Some(file_json(&file)?)),
        AgentCommand::Update { agent, file } => (
            "PATCH",
            format!("{path}/{}", segment(&agent)?),
            Some(file_json(&file)?),
        ),
        AgentCommand::Pause { agent } => (
            "POST",
            format!("{path}/{}/pause", segment(&agent)?),
            Some(json!({})),
        ),
    };
    display(
        &proxy(&api, &install, method, &suffix, body.as_ref())?,
        args.json,
    );
    Ok(OK)
}

#[derive(Deserialize)]
struct IngestKey {
    ingest_key_id: String,
    public_key: String,
    expires_at: String,
    org_id: Option<String>,
    install_id: Option<String>,
    identity_signature: Option<String>,
}

#[derive(serde::Serialize)]
struct SealBinding<'a> {
    org_id: &'a str,
    credential_id: &'a str,
    field: &'a str,
    expected_version: u64,
    binding_hash: &'a str,
    ingest_key_id: &'a str,
    expires_at: i64,
}

type X25519 = X25519HkdfSha256;
type PublicKey = <X25519 as Kem>::PublicKey;

fn checked_key(key: &IngestKey, pin: Option<&str>) -> Result<PublicKey, &'static str> {
    let expires_at = chrono::DateTime::parse_from_rfc3339(&key.expires_at)
        .map_err(|_| "invalid vault ingest key expiry")?;
    if expires_at <= chrono::Utc::now() {
        return Err("the vault ingest key has expired");
    }
    let raw = B64
        .decode(&key.public_key)
        .map_err(|_| "invalid vault ingest key encoding")?;
    let fingerprint = format!("{:x}", Sha256::digest(&raw));
    if pin.is_some_and(|pin| !pin.eq_ignore_ascii_case(&fingerprint)) {
        return Err("vault ingest key fingerprint does not match --pin");
    }
    PublicKey::from_bytes(&raw).map_err(|_| "invalid vault ingest public key")
}

fn seal(key: &PublicKey, binding: &SealBinding<'_>, value: &[u8]) -> Result<String, &'static str> {
    if value.is_empty() || value.len() > MAX_SECRET {
        return Err("secret must contain 1 byte to 64 KiB");
    }
    let version = binding.expected_version.to_string();
    let expiry = binding.expires_at.to_string();
    let mut aad = Vec::new();
    for part in [
        INFO,
        binding.org_id,
        binding.credential_id,
        binding.field,
        &version,
        binding.binding_hash,
        binding.ingest_key_id,
        &expiry,
    ] {
        let length = u32::try_from(part.len()).map_err(|_| "seal binding is too large")?;
        aad.extend_from_slice(&length.to_be_bytes());
        aad.extend_from_slice(part.as_bytes());
    }
    let (enc, mut sender) = setup_sender::<AesGcm256, HkdfSha256, X25519, _>(
        &OpModeS::Base,
        key,
        INFO.as_bytes(),
        &mut rand_core::OsRng,
    )
    .map_err(|_| "could not initialize HPKE sealing")?;
    let ct = sender
        .seal(value, &aad)
        .map_err(|_| "could not seal the secret")?;
    #[derive(serde::Serialize)]
    struct Blob<'a> {
        v: u8,
        #[serde(flatten)]
        binding: &'a SealBinding<'a>,
        enc: String,
        ct: String,
    }
    let blob = Blob {
        v: 1,
        binding,
        enc: B64.encode(enc.to_bytes()),
        ct: B64.encode(ct),
    };
    serde_json::to_vec(&blob)
        .map(|bytes| B64.encode(bytes))
        .map_err(|_| "could not encode sealed secret")
}

fn verify_ingest_identity(
    key: &IngestKey,
    anchor: &str,
    org: &str,
    install: &str,
) -> Result<(), &'static str> {
    const INVALID: &str = "could not verify the vault identity for secret entry";
    if key.org_id.as_deref() != Some(org) || key.install_id.as_deref() != Some(install) {
        return Err(INVALID);
    }
    verify_vault_signature(
        anchor,
        key.identity_signature.as_deref().ok_or(INVALID)?,
        [
            "runbook-runner/ingest-key/v1",
            org,
            install,
            &key.ingest_key_id,
            &key.public_key,
            &key.expires_at,
        ],
    )
    .map_err(|_| INVALID)
}

fn verify_vault_signature<const N: usize>(
    anchor: &str,
    signature: &str,
    fields: [&str; N],
) -> Result<(), ()> {
    let raw: [u8; 32] = B64
        .decode(anchor)
        .map_err(|_| ())?
        .try_into()
        .map_err(|_| ())?;
    let public = ed25519_dalek::VerifyingKey::from_bytes(&raw).map_err(|_| ())?;
    let signature = B64.decode(signature).map_err(|_| ())?;
    let signature = ed25519_dalek::Signature::from_slice(&signature).map_err(|_| ())?;
    let mut message = Vec::new();
    for field in fields {
        let length = u32::try_from(field.len()).map_err(|_| ())?;
        message.extend_from_slice(&length.to_be_bytes());
        message.extend_from_slice(field.as_bytes());
    }
    public.verify_strict(&message, &signature).map_err(|_| ())
}

fn verify_credential_receipt(
    reply: &Value,
    expected: &Value,
    anchor: &str,
    org: &str,
    install: &str,
) -> Result<(), &'static str> {
    const INVALID: &str = "could not verify the vault credential receipt";
    let id = reply["id"]
        .as_str()
        .filter(|id| runner_segment(id).is_ok())
        .ok_or(INVALID)?;
    let name = expected["name"].as_str().ok_or(INVALID)?;
    let label = expected["label"]
        .as_str()
        .unwrap_or(if expected["type"] == "model-provider" {
            "read"
        } else {
            "destructive"
        });
    let mut destination = expected.clone();
    if destination["groups"].is_null() {
        destination["groups"] = json!("all");
    }
    let binding = destination_binding(&destination).map_err(|_| INVALID)?;
    let binding_hash = destination_hash(&expected["type"], &binding);
    let projects = match &expected["sharing"] {
        Value::Null => json!("none"),
        sharing => sharing["projects"].clone(),
    };
    verify_vault_signature(
        anchor,
        reply["creation_receipt"].as_str().ok_or(INVALID)?,
        [
            "runbook-runner/credential-create/v1",
            org,
            install,
            id,
            name,
            &binding_hash,
            label,
            &projects.to_string(),
        ],
    )
    .map_err(|_| INVALID)
}

fn core_vault_identity(api: &ApiClient, org: &str, install: &str) -> Exit<String> {
    let status = api
        .get_json(&format!("/v1/orgs/{org}/runner"))
        .map_err(api_failed)?;
    if status["install_id"].as_str() != Some(install) {
        return Err(fail(
            "core runner status does not match the requested install",
        ));
    }
    status["vault_identity_key"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| fail("the core has no registered vault identity"))
}

fn read_secret() -> Exit<Zeroizing<Vec<u8>>> {
    if io::stdin().is_terminal() {
        let value = rpassword::prompt_password("Secret: ")
            .map_err(|_| fail("cannot read secret prompt"))?;
        return Ok(Zeroizing::new(value.into_bytes()));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    io::stdin()
        .take((MAX_SECRET + 3) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| fail("cannot read secret from stdin"))?;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    Ok(bytes)
}

const BAD_DESTINATION: &str = "invalid credential destination";

fn bound_text(value: &Value) -> Result<&str, &'static str> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(BAD_DESTINATION)
}

fn bound_endpoint(value: &Value, allow_http: bool) -> Result<String, &'static str> {
    let url = url::Url::parse(bound_text(value)?.trim()).map_err(|_| BAD_DESTINATION)?;
    if !(url.scheme() == "https" || allow_http && url.scheme() == "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some_and(|query| !query.is_empty())
        || url.fragment().is_some_and(|fragment| !fragment.is_empty())
    {
        return Err(BAD_DESTINATION);
    }
    Ok(format!(
        "{}{}",
        url.origin().ascii_serialization(),
        url.path().trim_end_matches('/')
    ))
}

fn destination_binding(metadata: &Value) -> Result<Value, &'static str> {
    let kind = bound_text(&metadata["type"])?;
    let config = &metadata["config"];
    let groups = if metadata["groups"] == "all" {
        json!("all")
    } else {
        let mut groups = metadata["groups"]
            .as_array()
            .ok_or(BAD_DESTINATION)?
            .iter()
            .map(bound_text)
            .collect::<Result<Vec<_>, _>>()?;
        groups.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
        groups.dedup();
        json!(groups)
    };
    match kind {
        "api-key" => {
            let mut hosts = config["hosts"]
                .as_array()
                .filter(|hosts| !hosts.is_empty())
                .ok_or(BAD_DESTINATION)?
                .iter()
                .map(|value| {
                    let normalized = bound_text(value)?.trim().to_lowercase();
                    Ok(normalized
                        .strip_suffix('.')
                        .unwrap_or(&normalized)
                        .to_owned())
                })
                .collect::<Result<Vec<_>, &'static str>>()?;
            hosts.sort_unstable();
            hosts.dedup();
            let method = &config["test"]["method"];
            let method = if method.is_null() {
                "GET"
            } else {
                bound_text(method)?
            };
            if !matches!(method, "GET" | "HEAD") {
                return Err(BAD_DESTINATION);
            }
            let path = &config["test"]["path"];
            let path = if path.is_null() {
                "/"
            } else {
                bound_text(path)?
            };
            Ok(json!({
                "hosts": hosts, "header_name": bound_text(&config["header_name"])?.to_lowercase(),
                "header_format": bound_text(&config["header_format"])?,
                "test": { "method": method, "path": path }, "groups": groups,
            }))
        }
        "model-provider" => {
            let provider = bound_text(&config["provider"])?;
            let (header, format) = match provider {
                "anthropic" => ("x-api-key", "{}"),
                "google" => ("x-goog-api-key", "{}"),
                "azure-openai" => ("api-key", "{}"),
                "openai" | "openai-compatible" | "vertex-gemini" => ("authorization", "Bearer {}"),
                _ => return Err(BAD_DESTINATION),
            };
            Ok(json!({
                "provider": provider, "endpoint": bound_endpoint(&config["endpoint"], provider == "openai-compatible")?,
                "header_name": header, "header_format": format, "groups": groups,
            }))
        }
        "mcp-bearer" => Ok(json!({
            "url": bound_endpoint(&config["url"], false)?, "header_name": "authorization",
            "header_format": "Bearer {}", "groups": groups,
        })),
        _ => Err(BAD_DESTINATION),
    }
}

fn destination_hash(kind: &Value, binding: &Value) -> String {
    // All object member names in a binding are ASCII. Serialize in JCS
    // order even when another workspace crate enables preserve_order.
    fn sort_members(value: &mut Value) {
        if let Value::Object(object) = value {
            object.sort_keys();
            for value in object.values_mut() {
                sort_members(value);
            }
        }
    }
    let mut document = json!({ "binding": binding, "type": kind });
    sort_members(&mut document);
    B64.encode(Sha256::digest(document.to_string()))
}

fn set_credential(
    api: &ApiClient,
    install: &str,
    org: &str,
    credential: &str,
    field: &str,
    pin: Option<&str>,
    binding_pin: Option<&str>,
) -> Exit<Value> {
    let interactive = io::stdin().is_terminal();
    if !interactive && binding_pin.is_none() {
        return Err(fail(
            "non-interactive credential rotation requires --binding-pin or --expected-config",
        ));
    }
    let credential = segment(credential)?;
    let field = segment(field)?;
    let path = format!("credentials/{credential}");
    let metadata = proxy(api, install, "GET", &path, None)?;
    let destination = destination_binding(&metadata).map_err(fail)?;
    let binding_hash = destination_hash(&metadata["type"], &destination);
    if binding_pin.is_some_and(|pin| pin != binding_hash) {
        return Err(fail("credential destination does not match --binding-pin"));
    }
    let expected_version = match metadata["secret_meta"][field].get("version") {
        Some(value) => value
            .as_u64()
            .ok_or_else(|| fail("invalid credential field version"))?,
        None => 0,
    };
    let ingest: IngestKey = serde_json::from_value(proxy(api, install, "GET", "ingest-key", None)?)
        .map_err(|_| fail("invalid vault ingest key response"))?;
    if pin.is_none() {
        let anchor = core_vault_identity(api, org, install)?;
        verify_ingest_identity(&ingest, &anchor, org, install).map_err(fail)?;
    }
    let key = checked_key(&ingest, pin).map_err(fail)?;
    eprintln!("Credential destination: {destination}");
    eprintln!("Destination binding hash: {binding_hash}");
    if interactive {
        eprintln!("Confirm this destination by typing 'yes':");
        let mut confirmation = String::new();
        io::stdin()
            .read_line(&mut confirmation)
            .map_err(|_| fail("cannot read destination confirmation"))?;
        if confirmation.trim() != "yes" {
            return Err(fail("credential destination was not confirmed"));
        }
    }
    let value = read_secret()?;
    std::str::from_utf8(&value).map_err(|_| fail("secret must be UTF-8"))?;
    let binding = SealBinding {
        org_id: org,
        credential_id: credential,
        field,
        expected_version,
        binding_hash: &binding_hash,
        ingest_key_id: &ingest.ingest_key_id,
        expires_at: chrono::Utc::now().timestamp() + 600,
    };
    let blob = seal(&key, &binding, &value).map_err(fail)?;
    drop(value);
    proxy(
        api,
        install,
        "POST",
        &format!("{path}/secrets/{field}"),
        Some(&json!({"blob":blob})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creation_receipt_authenticates_the_credential_id() {
        use ed25519_dalek::{Signer, SigningKey};
        let identity = SigningKey::from_bytes(&[7; 32]);
        let anchor = B64.encode(identity.verifying_key().to_bytes());
        let expected = json!({
            "name": "new", "type": "mcp-bearer", "groups": "all", "config": {"url": "https://mcp.example"}
        });
        let binding_hash =
            destination_hash(&expected["type"], &destination_binding(&expected).unwrap());
        let mut signed = Vec::new();
        for field in [
            "runbook-runner/credential-create/v1",
            "org",
            "install",
            "new-id",
            "new",
            &binding_hash,
            "destructive",
            "\"none\"",
        ] {
            signed.extend_from_slice(&(field.len() as u32).to_be_bytes());
            signed.extend_from_slice(field.as_bytes());
        }
        let mut reply = json!({ "id": "new-id", "creation_receipt": B64.encode(identity.sign(&signed).to_bytes()) });
        assert_eq!(
            verify_credential_receipt(&reply, &expected, &anchor, "org", "install"),
            Ok(())
        );
        reply["id"] = json!("another-id");
        assert!(verify_credential_receipt(&reply, &expected, &anchor, "org", "install").is_err());
    }

    #[test]
    fn destination_binding_matches_vault_vectors() {
        let vectors: Vec<Value> =
            serde_json::from_str(include_str!("../../../api/runner-binding-vectors.json")).unwrap();
        for fixture in vectors {
            let binding = destination_binding(&fixture).unwrap();
            assert_eq!(binding, fixture["binding"]);
            assert_eq!(
                destination_hash(&fixture["type"], &binding),
                fixture["hash"]
            );
        }
    }
}
