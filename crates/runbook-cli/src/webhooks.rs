//! C2 `runbook webhook …` (PRD §9 Webhooks, §10) against the C1 routes
//! under `/v1/projects/{p}/webhooks` (`webhooks:manage`). Every command
//! takes `--project` (ID or slug). POSTs carry an `Idempotency-Key`.
//!
//! The signing secret is printed exactly once, on stdout of `create` and
//! `rotate-secret` only; a replay of either is `409
//! webhooks.secret_already_issued` and prints nothing secret.
use crate::agent::Env;
use crate::api::ApiClient;
use crate::cliutil::{
    Exit, OK, api_failed, client, idempotency_key, parse_id, print_json, text, usage,
};
use clap::{Args, Subcommand};
use runbook_model::{DeliveryId, RunbookId, WebhookId};
use serde_json::{Map, Value, json};

/// The API's subscription name limit (characters).
const NAME_MAX: usize = 100;

#[derive(Subcommand, Debug)]
pub enum WebhookCommands {
    #[command(about = "List a project's webhook subscriptions")]
    List(ListArgs),
    #[command(about = "Create a subscription (prints its signing secret once)")]
    Create(CreateArgs),
    #[command(about = "Update a subscription; omitted options are unchanged")]
    Update(UpdateArgs),
    #[command(about = "Delete a subscription and its delivery log")]
    Delete(TargetArgs),
    #[command(about = "Send a webhook.test event to a subscription")]
    Test(PostTargetArgs),
    #[command(about = "Show a subscription's delivery log")]
    Deliveries(DeliveriesArgs),
    #[command(about = "Send a delivery's event again under a new delivery ID")]
    Redeliver(RedeliverArgs),
    #[command(
        name = "rotate-secret",
        about = "Rotate the signing secret (prints the new secret once)"
    )]
    RotateSecret(PostTargetArgs),
}

#[derive(Args, Debug)]
pub struct ProjectArg {
    #[arg(long, env = "RUNBOOK_PROJECT", help = "Project ID or slug")]
    pub project: String,
}

#[derive(Args, Debug)]
pub struct ListArgs {
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct CreateArgs {
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(long, help = "HTTPS endpoint")]
    pub url: String,
    #[arg(
        long = "event",
        required = true,
        help = "Event type, or * for all (repeat for each)"
    )]
    pub events: Vec<String>,
    #[arg(long, help = "Subscription name (default: the URL)")]
    pub name: Option<String>,
    #[arg(long = "runbook", help = "Only events of this runbook (rb_…; repeat)")]
    pub runbooks: Vec<String>,
    #[arg(long = "tag", help = "Only executions with this tag (repeat)")]
    pub tags: Vec<String>,
    #[arg(long, help = "Create the subscription disabled")]
    pub disabled: bool,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output the response (with the secret) as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct UpdateArgs {
    #[arg(help = "Webhook ID (whk_…)")]
    pub id: String,
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(long, help = "New name")]
    pub name: Option<String>,
    #[arg(long, help = "New HTTPS endpoint")]
    pub url: Option<String>,
    #[arg(long = "event", help = "Replace the event types (repeat)")]
    pub events: Vec<String>,
    #[arg(long = "runbook", help = "Replace the runbook filter (rb_…; repeat)")]
    pub runbooks: Vec<String>,
    #[arg(
        long = "clear-runbooks",
        conflicts_with = "runbooks",
        help = "Remove the runbook filter"
    )]
    pub clear_runbooks: bool,
    #[arg(long = "tag", help = "Replace the tag filter (repeat)")]
    pub tags: Vec<String>,
    #[arg(
        long = "clear-tags",
        conflicts_with = "tags",
        help = "Remove the tag filter"
    )]
    pub clear_tags: bool,
    #[arg(
        long,
        conflicts_with = "disable",
        help = "Enable (and reset delivery health)"
    )]
    pub enable: bool,
    #[arg(long, help = "Disable")]
    pub disable: bool,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct TargetArgs {
    #[arg(help = "Webhook ID (whk_…)")]
    pub id: String,
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct PostTargetArgs {
    #[arg(help = "Webhook ID (whk_…)")]
    pub id: String,
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct DeliveriesArgs {
    #[arg(help = "Webhook ID (whk_…)")]
    pub id: String,
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(
        long,
        help = "Page limit (1-100)",
        value_parser = clap::value_parser!(u32).range(1..=100)
    )]
    pub limit: Option<u32>,
    #[arg(long, help = "Pagination cursor")]
    pub cursor: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

#[derive(Args, Debug)]
pub struct RedeliverArgs {
    #[arg(help = "Delivery ID (whd_…)")]
    pub delivery: String,
    #[arg(long, help = "Webhook ID the delivery belongs to (whk_…)")]
    pub webhook: String,
    #[command(flatten)]
    pub project: ProjectArg,
    #[arg(long = "idempotency-key", help = "Idempotency key")]
    pub idempotency_key: Option<String>,
    #[arg(long, help = "Output response as JSON")]
    pub json: bool,
}

pub fn handle_webhook(env: &Env, command: WebhookCommands) -> i32 {
    let result = match command {
        WebhookCommands::List(a) => list(env, a),
        WebhookCommands::Create(a) => create(env, a),
        WebhookCommands::Update(a) => update(env, a),
        WebhookCommands::Delete(a) => delete(env, a),
        WebhookCommands::Test(a) => test(env, a),
        WebhookCommands::Deliveries(a) => deliveries(env, a),
        WebhookCommands::Redeliver(a) => redeliver(env, a),
        WebhookCommands::RotateSecret(a) => rotate(env, a),
    };
    result.unwrap_or_else(|code| code)
}

fn base(project: &ProjectArg) -> Exit<String> {
    crate::selector::validate_project_ref(&project.project).map_err(usage)?;
    Ok(format!("/v1/projects/{}/webhooks", project.project))
}

fn runbook_ids(given: &[String]) -> Exit<Vec<String>> {
    given
        .iter()
        .map(|r| parse_id::<RunbookId>(r, "runbook"))
        .collect()
}

/// `{webhook, secret}`: print the view summary and the secret, once.
fn print_secret_view(json_out: bool, verb: &str, created: &Value) -> Exit<i32> {
    if json_out {
        print_json(created);
        return Ok(OK);
    }
    let view = created.get("webhook").unwrap_or(&Value::Null);
    let Some(secret) = created.get("secret").and_then(Value::as_str) else {
        eprintln!("error: the response carried no secret");
        return Ok(1);
    };
    println!("{verb} webhook {} {}", text(view, "id"), text(view, "name"));
    println!("{secret}");
    eprintln!("Store this signing secret now; it is shown only once.");
    Ok(OK)
}

fn print_view(json_out: bool, verb: &str, view: &Value) -> Exit<i32> {
    if json_out {
        print_json(view);
    } else {
        println!("{verb} webhook {} {}", text(view, "id"), text(view, "name"));
    }
    Ok(OK)
}

fn print_delivery(json_out: bool, verb: &str, delivery: &Value) -> Exit<i32> {
    if json_out {
        print_json(delivery);
    } else {
        println!(
            "{verb} delivery {} ({})",
            text(delivery, "id"),
            text(delivery, "state")
        );
    }
    Ok(OK)
}

fn list(env: &Env, a: ListArgs) -> Exit<i32> {
    let path = base(&a.project)?;
    let page = client(env)?.get_json(&path).map_err(api_failed)?;
    if a.json {
        print_json(&page);
        return Ok(OK);
    }
    for w in page["items"].as_array().into_iter().flatten() {
        let events: Vec<&str> = w["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let state = if w["enabled"].as_bool() == Some(true) {
            "enabled"
        } else {
            "disabled"
        };
        println!(
            "{}  {state}  {}  {}  {}",
            text(w, "id"),
            text(w, "name"),
            text(w, "url"),
            events.join(",")
        );
    }
    Ok(OK)
}

fn create(env: &Env, a: CreateArgs) -> Exit<i32> {
    let path = base(&a.project)?;
    let runbooks = runbook_ids(&a.runbooks)?;
    let name = a
        .name
        .clone()
        .unwrap_or_else(|| a.url.chars().take(NAME_MAX).collect());
    let body = json!({
        "name": name,
        "url": a.url,
        "events": a.events,
        "runbooks": runbooks,
        "tags": a.tags,
        "enabled": !a.disabled,
    });
    let client = client(env)?;
    let key = idempotency_key(&a.idempotency_key)?;
    let created = client.post_json(&path, &key, &body).map_err(api_failed)?;
    print_secret_view(a.json, "created", &created)
}

fn update(env: &Env, a: UpdateArgs) -> Exit<i32> {
    let id = parse_id::<WebhookId>(&a.id, "webhook")?;
    let path = format!("{}/{id}", base(&a.project)?);
    let mut body = Map::new();
    if let Some(name) = &a.name {
        body.insert("name".into(), json!(name));
    }
    if let Some(url) = &a.url {
        body.insert("url".into(), json!(url));
    }
    if !a.events.is_empty() {
        body.insert("events".into(), json!(a.events));
    }
    if !a.runbooks.is_empty() || a.clear_runbooks {
        body.insert("runbooks".into(), json!(runbook_ids(&a.runbooks)?));
    }
    if !a.tags.is_empty() || a.clear_tags {
        body.insert("tags".into(), json!(a.tags));
    }
    if a.enable || a.disable {
        body.insert("enabled".into(), json!(a.enable));
    }
    if body.is_empty() {
        return Err(usage("nothing to update"));
    }
    let client = client(env)?;
    // PATCH needs no Idempotency-Key here; the API ignores one.
    let key = crate::session::random_hex().map_err(usage)?;
    let view = client
        .patch_json(&path, &key, &Value::Object(body))
        .map_err(api_failed)?;
    print_view(a.json, "updated", &view)
}

fn delete(env: &Env, a: TargetArgs) -> Exit<i32> {
    let id = parse_id::<WebhookId>(&a.id, "webhook")?;
    let path = format!("{}/{id}", base(&a.project)?);
    client(env)?.delete(&path).map_err(api_failed)?;
    if a.json {
        print_json(&json!({"deleted": true, "id": id}));
    } else {
        println!("deleted {id}");
    }
    Ok(OK)
}

fn post_empty(client: &ApiClient, path: &str, given_key: &Option<String>) -> Exit<Value> {
    let key = idempotency_key(given_key)?;
    client.post_json(path, &key, &json!({})).map_err(api_failed)
}

fn test(env: &Env, a: PostTargetArgs) -> Exit<i32> {
    let id = parse_id::<WebhookId>(&a.id, "webhook")?;
    let path = format!("{}/{id}/test", base(&a.project)?);
    let delivery = post_empty(&client(env)?, &path, &a.idempotency_key)?;
    print_delivery(a.json, "queued test", &delivery)
}

fn deliveries(env: &Env, a: DeliveriesArgs) -> Exit<i32> {
    let id = parse_id::<WebhookId>(&a.id, "webhook")?;
    let path = format!("{}/{id}/deliveries", base(&a.project)?);
    let limit = a.limit.map(|l| l.to_string());
    let mut query: Vec<(&str, &str)> = vec![];
    if let Some(limit) = &limit {
        query.push(("limit", limit));
    }
    if let Some(cursor) = &a.cursor {
        query.push(("cursor", cursor));
    }
    let page = client(env)?
        .get_json_query(&path, &query)
        .map_err(api_failed)?;
    if a.json {
        print_json(&page);
        return Ok(OK);
    }
    for d in page["items"].as_array().into_iter().flatten() {
        let attempts = d["attempts"].as_array().map_or(0, Vec::len);
        let failure = d["failure"].as_str().unwrap_or("");
        println!(
            "{}  {}  {}  attempts={attempts}  {failure}",
            text(d, "id"),
            text(d, "state"),
            text(d, "event_type"),
        );
    }
    if let Some(next) = page["next_cursor"].as_str().filter(|c| !c.is_empty()) {
        println!("next: {next}");
    }
    Ok(OK)
}

fn redeliver(env: &Env, a: RedeliverArgs) -> Exit<i32> {
    let delivery = parse_id::<DeliveryId>(&a.delivery, "delivery")?;
    let webhook = parse_id::<WebhookId>(&a.webhook, "webhook")?;
    let path = format!(
        "{}/{webhook}/deliveries/{delivery}/redeliver",
        base(&a.project)?
    );
    let queued = post_empty(&client(env)?, &path, &a.idempotency_key)?;
    print_delivery(a.json, "queued", &queued)
}

fn rotate(env: &Env, a: PostTargetArgs) -> Exit<i32> {
    let id = parse_id::<WebhookId>(&a.id, "webhook")?;
    let path = format!("{}/{id}/rotate-secret", base(&a.project)?);
    let rotated = post_empty(&client(env)?, &path, &a.idempotency_key)?;
    print_secret_view(a.json, "rotated", &rotated)
}
