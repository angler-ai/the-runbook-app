//! Browser OAuth termination shared by the native and Workers hosts.
use crate::{
    Credential, Request,
    backends::Backends,
    http::Headers,
    problem::{Navigation, Response},
    runner_proxy::{self, RunnerHttpResponse},
};
use runbook_core::{runners::RunnerStore, signing::body_hash};
use serde_json::{Map, Value, json};

const PREFIX: &str = "/v1/runner/oauth/callback/";
const RETURN_PATH: &str = "/settings/organization/runner?tab=credentials";

pub(crate) fn credential_id(path: &str) -> Option<&str> {
    path.strip_prefix(PREFIX).filter(|id| {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

fn nonce_text(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn cookie_name(id: &str) -> String {
    format!("__Host-runbook-mcp-{id}")
}

fn cookie(id: &str, value: &str, age: u16) -> String {
    // __Host- cookies require Path=/ and prohibit Domain. The credential's
    // name isolates concurrent connects, and only its callback consumes it.
    format!(
        "{}={value}; Path=/; Max-Age={age}; Secure; HttpOnly; SameSite=Lax",
        cookie_name(id)
    )
}

pub(crate) fn callback_body(id: &str, query: Option<&str>, headers: &Headers) -> Value {
    let Some(query) = query.filter(|raw| raw.len() <= 12_288) else {
        return Value::Null;
    };
    let Ok(fields) = serde_urlencoded::from_str::<Vec<(String, String)>>(query) else {
        return Value::Null;
    };
    let mut body = Map::new();
    for (name, value) in fields {
        let limit = match name.as_str() {
            "state" | "error" => 256,
            "code" | "iss" | "error_description" | "error_uri" => 4096,
            _ => continue,
        };
        if value.is_empty()
            || value.len() > limit
            || value.chars().any(char::is_control)
            || body.insert(name, value.into()).is_some()
        {
            return Value::Null;
        }
    }
    if body.contains_key("code") == body.contains_key("error")
        || !body
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(nonce_text)
    {
        return Value::Null;
    }
    let name = cookie_name(id);
    let mut cookies = headers
        .get_all_str("cookie")
        .flat_map(|header| header.split(';'))
        .filter_map(|item| item.trim().split_once('='))
        .filter_map(|(key, value)| (key == name).then_some(value));
    let Some(expected) = cookies.next().filter(|_| cookies.next().is_none()) else {
        return Value::Null;
    };
    let parts: Vec<_> = expected.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|part| nonce_text(part)) {
        return Value::Null;
    }
    body.remove("error_description");
    body.remove("error_uri");
    body.insert("nonce".into(), parts[0].into());
    body.insert("session_hash".into(), parts[1].into());
    body.insert("state_hash".into(), parts[2].into());
    Value::Object(body)
}

pub(crate) fn start_response(
    answer: RunnerHttpResponse,
    id: &str,
    install: &runbook_core::runners::InstallRecord,
    claims: &runbook_core::signing::UserAssertionClaims,
    start: &Value,
    now_ms: i64,
    request_id: &str,
) -> Result<Response, (u16, &'static str)> {
    let bad = (502, "runner.bad_response");
    let body: Value = serde_json::from_slice(&answer.body).map_err(|_| bad)?;
    let nonce = body["nonce"]
        .as_str()
        .filter(|value| nonce_text(value))
        .ok_or(bad)?;
    let url = body["authorization_url"]
        .as_str()
        .filter(|value| {
            value.starts_with("https://")
                && value.len() <= 16_384
                && !value.chars().any(char::is_control)
        })
        .ok_or(bad)?;
    let (_, query) = url.split_once('?').ok_or(bad)?;
    let fields = serde_urlencoded::from_str::<Vec<(String, String)>>(query).map_err(|_| bad)?;
    let mut state = fields
        .iter()
        .filter_map(|(name, value)| (name == "state").then_some(value));
    let state_value = state.next().filter(|value| nonce_text(value)).ok_or(bad)?;
    if state.next().is_some() || credential_id(&format!("{PREFIX}{id}")).is_none() {
        return Err(bad);
    }
    let session_hash = claims.session_hash.as_deref().ok_or(bad)?;
    let nonce_hash = body_hash(nonce.as_bytes());
    let public = install.vault_identity_key.as_deref().ok_or(bad)?;
    let signature = body["identity_signature"].as_str().ok_or(bad)?;
    let expires = body["expires_at_ms"].as_i64().ok_or(bad)?;
    let handoff = match start.get("authorization_handoff") {
        None => "",
        Some(value) => value
            .as_str()
            .filter(|value| nonce_text(value))
            .ok_or(bad)?,
    };
    if body["request_id"].as_str() != Some(&claims.jti)
        || !runbook_core::signing::verify_vault_statement(
            public,
            signature,
            &[
                "runbook-runner/oauth-start/v1",
                &claims.org_id,
                install.id.as_str(),
                id,
                start["binding_hash"].as_str().ok_or(bad)?,
                url,
                &nonce_hash,
                session_hash,
                &claims.jti,
                &expires.to_string(),
                handoff,
            ],
        )
    {
        return Err(bad);
    }
    if expires <= now_ms || expires > now_ms.saturating_add(720_000) {
        return Err((502, "runner.oauth_clock_skew"));
    }
    let state_hash = body_hash(state_value.as_bytes());
    let mut response = Response::json(
        200,
        json!({
            "authorization_url": url, "identity_signature": signature, "expires_at_ms": expires,
            "nonce_hash": nonce_hash, "session_hash": session_hash, "request_id": claims.jti,
        }),
        request_id,
    );
    response.navigation = Some(Box::new(Navigation::OAuth {
        redirect: None,
        cookie: Some(cookie(
            id,
            &format!("{nonce}.{session_hash}.{state_hash}"),
            600,
        )),
    }));
    Ok(response)
}

pub(crate) async fn callback<B: Backends>(backends: &B, request: &Request, id: &str) -> Response {
    let mut body = request.body.clone();
    let mut cleared = false;
    let result = async {
        let Some(Credential::Session(session)) = &request.credential else {
            return None;
        };
        if session.scopes.is_some()
            || body["session_hash"].as_str()? != body_hash(session.session_id.as_bytes())
            || body["state_hash"].as_str()? != body_hash(body["state"].as_str()?.as_bytes())
        {
            return None;
        }
        let principal = session.principal.as_ref()?;
        let install = backends
            .runners()
            .install_of(&principal.org_id)
            .await
            .ok()??;
        let inner = body.as_object_mut()?;
        inner.remove("session_hash");
        inner.remove("state_hash");
        cleared = true;
        let forward = Request {
            method: "POST".into(),
            path: format!(
                "/v1/runners/{}/proxy/runner/v1/oauth/callback/{id}",
                install.id
            ),
            query: Default::default(),
            body: runner_proxy::envelope(Some("1"), None, body),
            credential: Some(Credential::Session(session.clone())),
            request_id: request.request_id.clone(),
            new_id: String::new(),
            idempotency_key: None,
            if_match: None,
            client_ip: request.client_ip.clone(),
        };
        let response = runner_proxy::proxy(backends, &forward, true).await.ok()?;
        let message = response
            .proxied
            .and_then(|response| serde_json::from_slice::<Value>(&response.bytes).ok())?;
        Some(if message["awaiting_confirmation"] == true {
            "confirm-account"
        } else if message["connected"] == true {
            if message["test"]["ok"] == false {
                "test-failed"
            } else {
                "connected"
            }
        } else {
            "failed"
        })
    }
    .await
    .unwrap_or("failed");
    redirect(id, result, cleared, &request.request_id)
}

pub(crate) fn redirect(id: &str, result: &str, clear_cookie: bool, request_id: &str) -> Response {
    let mut response = Response::json(303, Value::Null, request_id);
    response.navigation = Some(Box::new(Navigation::OAuth {
        redirect: Some(format!("{RETURN_PATH}#oauth-result={id}.{result}")),
        cookie: clear_cookie.then(|| cookie(id, "", 0)),
    }));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_checks_cardinality_and_nonce_cookie() {
        let state = "a".repeat(43);
        let query = format!("code=fixture-code&state={state}");
        let mut headers = Headers::new();
        assert!(callback_body("example", Some(&query), &headers).is_null());
        let value = ["b".repeat(43), "c".repeat(43), "d".repeat(43)].join(".");
        headers.push("cookie", format!("x=y; {}={value}", cookie_name("example")));
        assert_eq!(
            callback_body("example", Some(&query), &headers)["code"],
            "fixture-code"
        );
        assert!(callback_body("example", Some(&format!("{query}&code=other")), &headers).is_null());
        assert!(
            callback_body("example", Some(&format!("{query}&error=denied")), &headers).is_null()
        );
        assert!(callback_body("another", Some(&query), &headers).is_null());
    }
}
