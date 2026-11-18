//! SLACK1's HTTP calls from the native server: stage 1's incoming webhook,
//! and stage 2's Web API (`chat.postMessage`, `users.info`) and
//! `response_url` replies. Blocking `ureq` on the blocking pool.
pub(crate) struct SlackHttp;

fn failure(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, _) => format!("slack_{code}"),
        _ => "slack_unreachable".to_owned(),
    }
}

impl SlackHttp {
    /// A JSON POST to an interaction's `response_url`.
    pub(crate) async fn respond(&self, url: &str, body: &serde_json::Value) -> Result<(), String> {
        let url = url.to_owned();
        let body = body.to_string();
        tokio::task::spawn_blocking(move || {
            ureq::post(&url)
                .timeout(std::time::Duration::from_secs(10))
                .set("content-type", "application/json")
                .send_string(&body)
                .map(|_| ())
                .map_err(failure)
        })
        .await
        .map_err(|_| "slack_join".to_owned())?
    }
}

impl runbook_core::slack::SlackTransport for SlackHttp {
    async fn post_slack(&self, url: &str, text: &str) -> Result<(), String> {
        let url = url.to_owned();
        let text = text.to_owned();
        tokio::task::spawn_blocking(move || {
            ureq::post(&url)
                .timeout(std::time::Duration::from_secs(10))
                .set("content-type", "application/json")
                .send_string(&serde_json::json!({ "text": text }).to_string())
                .map(|_| ())
                .map_err(failure)
        })
        .await
        .map_err(|_| "slack_join".to_owned())?
    }

    async fn slack_api(
        &self,
        token: &str,
        method: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let url = format!("https://slack.com/api/{method}");
        let token = token.to_owned();
        let query: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let body = body.cloned();
        tokio::task::spawn_blocking(move || {
            let mut request = match &body {
                Some(_) => ureq::post(&url),
                None => ureq::get(&url),
            }
            .timeout(std::time::Duration::from_secs(10))
            .set("authorization", &format!("Bearer {token}"));
            for (k, v) in &query {
                request = request.query(k, v);
            }
            let reply = match &body {
                Some(body) => request
                    .set("content-type", "application/json; charset=utf-8")
                    .send_string(&body.to_string()),
                None => request.call(),
            }
            .map_err(failure)?;
            let text = reply
                .into_string()
                .map_err(|_| "slack_invalid_reply".to_owned())?;
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|_| "slack_invalid_reply".to_owned())?;
            if value["ok"] == serde_json::json!(true) {
                Ok(value)
            } else {
                Err(format!(
                    "slack_{}",
                    value["error"].as_str().unwrap_or("not_ok")
                ))
            }
        })
        .await
        .map_err(|_| "slack_join".to_owned())?
    }
}
