//! SLACK1 stage 2 (2026-11-17): `POST /v1/slack/interactions`, where Slack
//! sends button clicks from approval messages.
//!
//! 1. The request must carry a valid Slack signature over its exact body,
//!    made with this server's signing secret, at most five minutes old.
//! 2. The clicking Slack user is matched to a Runbook user by email
//!    (`users.info`, scope `users:read.email`), who must be an active
//!    member of the organization the message came from.
//! 3. The vote is cast through the ordinary decision route, with a
//!    one-minute token of that person's own (revoked right after), so
//!    every eligibility rule applies exactly as in the app.
//! 4. The person gets an ephemeral reply saying what happened.
use crate::{Credential, Request, backends::Backends, problem::Response};
use runbook_core::{
    credentials::*,
    directory::DirectoryStore,
    slack::{DECIDE_ACTION, DecisionButton, verify_signature},
};
use runbook_model::{
    OrganizationId, SLACK_INTERACTIONS, SlackInteractionEnvelope, TokenId, UserId,
};
use serde_json::{Value, json};

pub fn recognizes(path: &str) -> bool {
    path == SLACK_INTERACTIONS
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    if r.method != "POST" {
        return Response::error(405, "request.method_not_allowed", &r.request_id);
    }
    let Some(app) = backends.slack_app() else {
        return Response::error(404, "resource.not_found", &r.request_id);
    };
    let Ok(envelope) = serde_json::from_value::<SlackInteractionEnvelope>(r.body.clone()) else {
        return Response::error(400, "slack.invalid_request", &r.request_id);
    };
    let now_ms = backends.wall_clock_ms();
    let signed = match (&envelope.timestamp, &envelope.signature) {
        (Some(ts), Some(sig)) => {
            verify_signature(&app.signing_secret, ts, &envelope.raw, sig, now_ms / 1000)
        }
        _ => false,
    };
    if !signed {
        return Response::error(401, "slack.invalid_signature", &r.request_id);
    }
    let payload: Option<Value> = serde_urlencoded::from_str::<Vec<(String, String)>>(&envelope.raw)
        .ok()
        .and_then(|fields| {
            fields
                .into_iter()
                .find(|(k, _)| k == "payload")
                .and_then(|(_, v)| serde_json::from_str(&v).ok())
        });
    let Some(payload) = payload else {
        return Response::error(400, "slack.invalid_request", &r.request_id);
    };
    // Only our buttons; anything else is acknowledged and ignored.
    let action = &payload["actions"][0];
    let is_decision = payload["type"] == "block_actions"
        && action["action_id"]
            .as_str()
            .is_some_and(|id| id.starts_with(DECIDE_ACTION));
    if !is_decision {
        return Response::json(200, Value::Null, &r.request_id);
    }
    let text = match decide(backends, &app.bot_token, &payload, action, &r, now_ms).await {
        Ok(text) | Err(text) => text,
    };
    if let Some(url) = payload["response_url"]
        .as_str()
        .filter(|u| u.starts_with("https://hooks.slack.com/"))
    {
        let _ = backends
            .slack_respond(
                url,
                &json!({"response_type": "ephemeral", "replace_original": false, "text": text}),
            )
            .await;
    }
    Response::json(200, Value::Null, &r.request_id)
}

/// Cast the clicked vote as the matching Runbook user. Both arms are the
/// sentence the person sees.
async fn decide<B: Backends>(
    backends: &B,
    token: &str,
    payload: &Value,
    action: &Value,
    r: &Request,
    now_ms: i64,
) -> Result<String, String> {
    let button: DecisionButton = action["value"]
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .ok_or("This button is not one The Runbook sent.")?;
    let org: OrganizationId = button
        .org
        .parse()
        .map_err(|_| "This button is not one The Runbook sent.")?;
    let slack_user = payload["user"]["id"]
        .as_str()
        .ok_or("Slack did not say who clicked.")?;
    let info = backends
        .slack_api(token, "users.info", &[("user", slack_user)], None)
        .await
        .map_err(|_| "Could not look you up in Slack. Try again, or decide in The Runbook.")?;
    let email = info["user"]["profile"]["email"]
        .as_str()
        .filter(|e| !e.is_empty())
        .ok_or(
            "Slack did not share your email (the app needs users:read.email). Decide in The Runbook.",
        )?;
    let directory = backends.directory();
    let user: UserId = directory
        .user_by_verified_email(email)
        .await
        .map_err(|_| "The Runbook is unavailable. Try again.")?
        .ok_or("No Runbook account signs in with your Slack email. Decide in The Runbook.")?
        .id;
    let snapshot = directory
        .snapshot(&org)
        .await
        .map_err(|_| "The Runbook is unavailable. Try again.")?;
    if !snapshot.users.get(&user).is_some_and(|u| u.active) {
        return Err("Your Runbook account is not an active member of this organization.".into());
    }

    // A one-minute token of the person's own, used for this one call.
    let mut random = [0u8; 32];
    let mut id_random = [0u8; 10];
    if backends.fill_random(&mut random).is_err() || backends.fill_random(&mut id_random).is_err() {
        return Err("The Runbook is unavailable. Try again.".into());
    }
    let token_id: TokenId = crate::backends::encode_id("tok_", now_ms, id_random)
        .and_then(|id| id.parse().ok())
        .ok_or("The Runbook is unavailable. Try again.")?;
    let secret = encode_secret(TokenKind::Personal, random);
    let credentials = backends.credentials();
    credentials
        .insert(CredentialRecord {
            id: token_id.clone(),
            kind: TokenKind::Personal,
            hash: secret_hash(&secret),
            principal: CredentialPrincipal::User {
                user_id: user,
                actor: ActorKind::Human,
            },
            org_id: org.clone(),
            scopes: [Scope::ExecutionsRead, Scope::ExecutionsAct].into(),
            projects: None,
            expires_at: Some(now_ms + 60_000),
            revoked_at: None,
            last_used_at: None,
        })
        .await
        .map_err(|_| "The Runbook is unavailable. Try again.")?;
    let action_ts = action["action_ts"].as_str().unwrap_or_default();
    let vote = Request {
        method: "POST".into(),
        path: format!("/v1/step-runs/{}/decisions", button.sr),
        query: Default::default(),
        body: json!({"option": button.option, "comment": "Decided in Slack"}),
        credential: Some(Credential::Bearer(secret)),
        idempotency_key: Some(format!("slack:{slack_user}:{action_ts}:{}", button.sr)),
        if_match: None,
        client_ip: None,
        request_id: r.request_id.clone(),
        new_id: r.new_id.clone(),
    };
    let answer = crate::actors::ActorService {
        directory: backends.directory(),
        credentials: backends.credentials(),
        backend: crate::actor_store::StoreActorBackend {
            backends: backends.clone(),
        },
        evidence_schemes: vec![],
    }
    .handle(vote)
    .await;
    let _ = credentials.revoke(&org, &token_id, now_ms).await;
    let option = &button.option;
    match (answer.status, answer.body["code"].as_str().unwrap_or("")) {
        (200..=299, _) => Ok(format!("Your vote ({option}) is recorded.")),
        (_, code) if code.starts_with("approval.not_open") || code == "step.not_waiting" => {
            Err("This approval is already decided.".into())
        }
        (_, "approval.not_eligible") | (403, _) => {
            Err("You are not an approver for this step.".into())
        }
        (_, "approval.already_voted") => Err("You already voted on this approval.".into()),
        (_, code) => Err(format!(
            "Your vote was not recorded ({code}). Decide in The Runbook."
        )),
    }
}
