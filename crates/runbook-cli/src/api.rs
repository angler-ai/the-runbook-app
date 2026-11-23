use std::fmt;
use std::time::Duration;

pub struct ApiClient {
    pub host: String,
    pub token: String,
    agent: ureq::Agent,
    stream_agent: ureq::Agent,
}

/// Every call has connect and read timeouts, so a hung request (notably a
/// credential refresh holding the refresh lock) cannot wait forever.
/// `RUNBOOK_HTTP_TIMEOUT_MS` overrides the read timeout (tests, slow links).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(45);
/// Event streams send a keep-alive at least every 30 s.
const STREAM_READ_TIMEOUT: Duration = Duration::from_secs(90);

fn read_timeout(default: Duration) -> Duration {
    std::env::var("RUNBOOK_HTTP_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(default, Duration::from_millis)
}

/// F73: never follow a redirect. A bearer token must reach only the
/// configured host and path; a 3xx answer is a `request.redirect_refused`
/// error instead.
fn agent(read: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .middleware(propagate_traceparent)
        .timeout_connect(CONNECT_TIMEOUT.min(read.max(Duration::from_millis(100))))
        .timeout_read(read)
        .timeout_write(read)
        .build()
}

// A CLI invoked inside a traced job may receive the job's W3C context.
#[allow(clippy::result_large_err)] // ureq fixes the middleware return type.
fn propagate_traceparent(
    mut request: ureq::Request,
    next: ureq::MiddlewareNext<'_>,
) -> Result<ureq::Response, ureq::Error> {
    if let Ok(value) = std::env::var("TRACEPARENT")
        && valid_traceparent(&value)
    {
        request = request.set("traceparent", &value);
    }
    next.handle(request)
}

fn valid_traceparent(value: &str) -> bool {
    let mut parts = value.split('-');
    let hex = |part: &str, length| {
        part.len() == length
            && part
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    parts.next() == Some("00")
        && parts
            .next()
            .is_some_and(|v| hex(v, 32) && v.bytes().any(|b| b != b'0'))
        && parts
            .next()
            .is_some_and(|v| hex(v, 16) && v.bytes().any(|b| b != b'0'))
        && parts.next().is_some_and(|v| hex(v, 2))
        && parts.next().is_none()
}

impl fmt::Debug for ApiClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiClient")
            .field("host", &self.host)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

/// A request with any method, optional body, idempotency key and
/// `If-Match` precondition (authoring writes).
#[derive(Debug, Default)]
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a [(String, String)],
    pub body: Option<&'a serde_json::Value>,
    pub idempotency_key: Option<&'a str>,
    pub if_match: Option<&'a str>,
}

/// A successful response status, body and its `ETag`, if any.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub status: u16,
    pub body: serde_json::Value,
    pub etag: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApiError {
    Problem {
        status: u16,
        code: String,
        message: String,
        /// The response's `Retry-After` in seconds, when it gave one.
        retry_after: Option<u64>,
    },
    Transport(String),
}

/// What to do about an MFA refusal (J-4): the CLI cannot present a second
/// factor itself, so it sends the person to the browser.
pub fn mfa_hint(code: &str) -> Option<&'static str> {
    match code {
        "auth.mfa_step_up" => Some(
            "this action needs a recent second-factor check, which a CLI token cannot give; \
             do it in the web app, which asks for your passkey or authenticator code",
        ),
        "auth.mfa_token_replacement_required" => Some(
            "this token was created without a recent second-factor check, so it can't do \
             sensitive actions; replace it with `runbook login`, approved in the browser with \
             your passkey or authenticator code",
        ),
        "auth.mfa_required" => Some(
            "your organization requires a second factor; sign in to the web app, set up a \
             passkey or authenticator app in Account > Security, then run `runbook login` again",
        ),
        _ => None,
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Problem { code, message, .. } => {
                write!(f, "{code}: {message}")?;
                match mfa_hint(code) {
                    Some(hint) => write!(f, "\nhint: {hint}"),
                    None => Ok(()),
                }
            }
            ApiError::Transport(err) => write!(f, "network error: {err}"),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, PartialEq, Eq)]
pub enum DeviceTokenPoll {
    Pending,
    SlowDown { server_interval: Option<u64> },
    Expired,
    Denied,
    Api(ApiError),
}

pub struct ArchiveDownload {
    pub bytes: Vec<u8>,
    pub filename: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ArchiveDownloadError {
    NotFound(ApiError),
    RewritePending { retry_after: Option<u64> },
    Api(ApiError),
}

impl fmt::Display for ArchiveDownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArchiveDownloadError::NotFound(e) => write!(f, "{e}"),
            ArchiveDownloadError::RewritePending {
                retry_after: Some(s),
            } => {
                write!(
                    f,
                    "archive.rewrite_pending: archive rewrite pending (retry after {s}s)"
                )
            }
            ArchiveDownloadError::RewritePending { retry_after: None } => {
                write!(f, "archive.rewrite_pending: archive rewrite pending")
            }
            ArchiveDownloadError::Api(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ArchiveDownloadError {}

impl ApiError {
    /// Stable CLI exit code (PRD §10): 4 when a limit or quota was reached
    /// (429, `limit.*`, `quota.*`, `*.limit_reached`), otherwise 1.
    pub fn exit_code(&self) -> i32 {
        match self {
            ApiError::Problem { status, code, .. }
                if *status == 429
                    || code.starts_with("limit.")
                    || code.starts_with("quota.")
                    || code.ends_with(".limit_reached") =>
            {
                4
            }
            _ => 1,
        }
    }
    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Problem { code, .. } => Some(code),
            ApiError::Transport(_) => None,
        }
    }
}

fn redirect_refused(status: u16) -> ApiError {
    ApiError::Problem {
        status,
        code: "request.redirect_refused".into(),
        message: "the server answered with a redirect; the CLI never follows redirects, so credentials stay on the configured host (check the host URL)".into(),
        retry_after: None,
    }
}

impl ApiClient {
    pub fn new(host: String, token: String) -> Self {
        Self {
            host,
            token,
            agent: agent(read_timeout(READ_TIMEOUT)),
            stream_agent: agent(read_timeout(STREAM_READ_TIMEOUT)),
        }
    }

    fn url(&self, path: &str) -> String {
        let base = self.host.trim_end_matches('/');
        let p = if path.starts_with('/') {
            path
        } else {
            &format!("/{path}")
        };
        format!("{base}{p}")
    }

    pub fn post_json(
        &self,
        path: &str,
        idempotency_key: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let url = self.url(path);
        let resp = self
            .agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json")
            .set("Idempotency-Key", idempotency_key)
            .send_json(body);

        Self::handle_response(resp)
    }

    /// A POST that authenticates by its body (execution refresh), so it
    /// sends no bearer credential.
    pub fn post_json_anonymous(
        &self,
        path: &str,
        idempotency_key: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let url = self.url(path);
        let resp = self
            .agent
            .post(&url)
            .set("Accept", "application/json")
            .set("Idempotency-Key", idempotency_key)
            .send_json(body);

        Self::handle_response(resp)
    }

    /// RFC 8628 §3.1: request device and user codes.
    pub fn device_code(
        &self,
        client_name: &str,
        scopes: &[String],
    ) -> Result<runbook_model::DeviceCodeView, ApiError> {
        let url = self.url("/v1/device/code");
        let body = serde_json::json!({
            "client_name": client_name,
            "scopes": scopes,
        });
        let resp = self
            .agent
            .post(&url)
            .set("Accept", "application/json")
            .send_json(&body);
        let val = Self::handle_response(resp)?;
        serde_json::from_value(val).map_err(|e| ApiError::Transport(e.to_string()))
    }

    /// RFC 8628 §3.4: poll the token endpoint with the device code.
    pub fn device_token(&self, device_code: &str) -> Result<serde_json::Value, DeviceTokenPoll> {
        let url = self.url("/v1/device/token");
        let body = serde_json::json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "device_code": device_code,
        });
        let resp = self
            .agent
            .post(&url)
            .set("Accept", "application/json")
            .send_json(&body);
        match resp {
            Ok(r) if (300..400).contains(&r.status()) => {
                Err(DeviceTokenPoll::Api(redirect_refused(r.status())))
            }
            Ok(r) => r
                .into_json()
                .map_err(|e| DeviceTokenPoll::Api(ApiError::Transport(e.to_string()))),
            Err(ureq::Error::Status(status, r)) => {
                let body: serde_json::Value = r.into_json().unwrap_or_default();
                let err_name = body
                    .get("error")
                    .or_else(|| body.get("code"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if err_name == "authorization_pending" || err_name == "device.authorization_pending"
                {
                    Err(DeviceTokenPoll::Pending)
                } else if err_name == "slow_down" || err_name == "device.slow_down" {
                    let server_interval = body.get("interval").and_then(|v| v.as_u64());
                    Err(DeviceTokenPoll::SlowDown { server_interval })
                } else if err_name == "expired_token" || err_name == "device.expired_token" {
                    Err(DeviceTokenPoll::Expired)
                } else if err_name == "access_denied" || err_name == "device.access_denied" {
                    Err(DeviceTokenPoll::Denied)
                } else {
                    let code = body["code"].as_str().unwrap_or("error").to_string();
                    let message = body
                        .get("detail")
                        .or_else(|| body.get("title"))
                        .or_else(|| body.get("message"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Err(DeviceTokenPoll::Api(ApiError::Problem {
                        status,
                        code,
                        message,
                        retry_after: None,
                    }))
                }
            }
            Err(ureq::Error::Transport(t)) => {
                Err(DeviceTokenPoll::Api(ApiError::Transport(t.to_string())))
            }
        }
    }

    pub fn patch_json(
        &self,
        path: &str,
        idempotency_key: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, ApiError> {
        let url = self.url(path);
        let resp = self
            .agent
            .request("PATCH", &url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json")
            .set("Idempotency-Key", idempotency_key)
            .send_json(body);

        Self::handle_response(resp)
    }

    pub fn get_json(&self, path: &str) -> Result<serde_json::Value, ApiError> {
        let url = self.url(path);
        let resp = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json")
            .call();

        Self::handle_response(resp)
    }

    pub fn get_json_query(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<serde_json::Value, ApiError> {
        let url = self.url(path);
        let mut req = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json");

        for (k, v) in query {
            req = req.query(k, v);
        }

        let resp = req.call();
        Self::handle_response(resp)
    }

    pub fn get_sse_stream(
        &self,
        path: &str,
        last_event_id: Option<&str>,
    ) -> Result<Box<dyn std::io::Read + Send>, ApiError> {
        let url = self.url(path);
        let mut req = self
            .stream_agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "text/event-stream");

        if let Some(id) = last_event_id {
            req = req.set("Last-Event-ID", id);
        }

        let resp = match req.call() {
            Ok(r) if (300..400).contains(&r.status()) => return Err(redirect_refused(r.status())),
            Ok(r) => r,
            Err(ureq::Error::Status(status, resp)) => {
                let body: serde_json::Value = resp.into_json().unwrap_or_default();
                let code = body
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string();
                let message = body
                    .get("detail")
                    .or_else(|| body.get("title"))
                    .or_else(|| body.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return Err(ApiError::Problem {
                    status,
                    code,
                    message,
                    retry_after: None,
                });
            }
            Err(ureq::Error::Transport(t)) => return Err(ApiError::Transport(t.to_string())),
        };

        Ok(Box::new(resp.into_reader()))
    }

    /// `DELETE` that answers `204 No Content` (or any 2xx; a body is ignored).
    pub fn delete(&self, path: &str) -> Result<(), ApiError> {
        let url = self.url(path);
        match self
            .agent
            .delete(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json")
            .call()
        {
            Ok(r) if (300..400).contains(&r.status()) => Err(redirect_refused(r.status())),
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, resp)) => Err(Self::problem(status, resp)),
            Err(ureq::Error::Transport(t)) => Err(ApiError::Transport(t.to_string())),
        }
    }

    /// A GET whose success body is text (for example `text/markdown`
    /// reports); errors are still problem+json.
    pub fn get_text(
        &self,
        path: &str,
        query: &[(&str, &str)],
        accept: &str,
    ) -> Result<String, ApiError> {
        let url = self.url(path);
        let mut req = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", accept);
        for (k, v) in query {
            req = req.query(k, v);
        }
        match req.call() {
            Ok(resp) if (300..400).contains(&resp.status()) => Err(redirect_refused(resp.status())),
            Ok(resp) => resp
                .into_string()
                .map_err(|e| ApiError::Transport(e.to_string())),
            Err(ureq::Error::Status(status, resp)) => Err(Self::problem(status, resp)),
            Err(ureq::Error::Transport(t)) => Err(ApiError::Transport(t.to_string())),
        }
    }

    /// A GET whose success body is bytes (REP2's PDF and JUnit reports);
    /// errors are still problem+json. Bodies over 64 MiB are refused.
    pub fn get_bytes(
        &self,
        path: &str,
        query: &[(&str, &str)],
        accept: &str,
    ) -> Result<Vec<u8>, ApiError> {
        use std::io::Read;
        const LIMIT: u64 = 64 * 1024 * 1024;
        let url = self.url(path);
        let mut req = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", accept);
        for (k, v) in query {
            req = req.query(k, v);
        }
        match req.call() {
            Ok(resp) if (300..400).contains(&resp.status()) => Err(redirect_refused(resp.status())),
            Ok(resp) => {
                let mut body = Vec::new();
                resp.into_reader()
                    .take(LIMIT + 1)
                    .read_to_end(&mut body)
                    .map_err(|e| ApiError::Transport(e.to_string()))?;
                if body.len() as u64 > LIMIT {
                    return Err(ApiError::Transport("response body too large".into()));
                }
                Ok(body)
            }
            Err(ureq::Error::Status(status, resp)) => Err(Self::problem(status, resp)),
            Err(ureq::Error::Transport(t)) => Err(ApiError::Transport(t.to_string())),
        }
    }

    /// Download an archive file or bundle as bytes with optional Content-Disposition filename.
    /// Returns ArchiveDownloadError::NotFound on 404, or ArchiveDownloadError::RewritePending on 409.
    pub fn download_archive(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<ArchiveDownload, ArchiveDownloadError> {
        let url = self.url(path);
        let mut req = self
            .agent
            .get(&url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/zip, application/problem+json");
        for (k, v) in query {
            req = req.query(k, v);
        }
        match req.call() {
            Ok(resp) if (300..400).contains(&resp.status()) => {
                Err(ArchiveDownloadError::Api(redirect_refused(resp.status())))
            }
            Ok(resp) => {
                let filename = resp.header("Content-Disposition").and_then(|cd| {
                    cd.split(';')
                        .map(str::trim)
                        .find_map(|part| part.strip_prefix("filename="))
                        .map(|name| name.trim_matches('"').trim_matches('\'').to_string())
                        .filter(|name| {
                            !name.is_empty() && !name.contains('/') && !name.contains('\\')
                        })
                });
                use std::io::Read;
                const LIMIT: u64 = 512 * 1024 * 1024;
                let mut bytes = Vec::new();
                resp.into_reader()
                    .take(LIMIT + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| ArchiveDownloadError::Api(ApiError::Transport(e.to_string())))?;
                if bytes.len() as u64 > LIMIT {
                    return Err(ArchiveDownloadError::Api(ApiError::Transport(
                        "response body too large".into(),
                    )));
                }
                Ok(ArchiveDownload { bytes, filename })
            }
            Err(ureq::Error::Status(404, resp)) => {
                Err(ArchiveDownloadError::NotFound(Self::problem(404, resp)))
            }
            Err(ureq::Error::Status(409, resp)) => {
                let retry_after = resp
                    .header("Retry-After")
                    .and_then(|val| val.trim().parse::<u64>().ok());
                let problem = Self::problem(409, resp);
                if problem.code() == Some("archive.rewrite_pending") {
                    Err(ArchiveDownloadError::RewritePending { retry_after })
                } else {
                    Err(ArchiveDownloadError::Api(problem))
                }
            }
            Err(ureq::Error::Status(status, resp)) => {
                Err(ArchiveDownloadError::Api(Self::problem(status, resp)))
            }
            Err(ureq::Error::Transport(t)) => Err(ArchiveDownloadError::Api(ApiError::Transport(
                t.to_string(),
            ))),
        }
    }

    fn problem(status: u16, resp: ureq::Response) -> ApiError {
        let retry_after = resp
            .header("Retry-After")
            .and_then(|val| val.trim().parse::<u64>().ok());
        let body: serde_json::Value = resp.into_json().unwrap_or_default();
        let code = body
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("error")
            .to_string();
        let message = body
            .get("detail")
            .or_else(|| body.get("title"))
            .or_else(|| body.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        ApiError::Problem {
            status,
            code,
            message,
            retry_after,
        }
    }

    /// Send any request. A problem's `errors` list (which field failed and
    /// why) is appended to its message so the caller can act on it.
    pub fn send(&self, r: &Request<'_>) -> Result<Reply, ApiError> {
        let mut req = self
            .agent
            .request(r.method, &self.url(r.path))
            .set("Accept", "application/json");
        if !self.token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.token));
        }
        if r.path.starts_with("/v1/runners/") && r.path.contains("/proxy/runner/v1/") {
            req = req.set("X-Runbook-CSRF", "1");
        }
        for (k, v) in r.query {
            req = req.query(k, v);
        }
        if let Some(key) = r.idempotency_key {
            req = req.set("Idempotency-Key", key);
        }
        if let Some(etag) = r.if_match {
            req = req.set("If-Match", etag);
        }
        let result = match r.body {
            Some(body) => req.send_json(body),
            None => req.call(),
        };
        match result {
            Ok(resp) if (300..400).contains(&resp.status()) => Err(redirect_refused(resp.status())),
            Ok(resp) => {
                let etag = resp.header("ETag").map(str::to_owned);
                let status = resp.status();
                let body = if status == 204 {
                    serde_json::Value::Null
                } else {
                    resp.into_json()
                        .map_err(|e| ApiError::Transport(e.to_string()))?
                };
                Ok(Reply { status, body, etag })
            }
            Err(ureq::Error::Status(status, resp)) => {
                let body: serde_json::Value = resp.into_json().unwrap_or_default();
                Err(problem_from_body(status, &body))
            }
            Err(ureq::Error::Transport(t)) => Err(ApiError::Transport(t.to_string())),
        }
    }

    /// Send a request and return any HTTP answer's status and JSON body,
    /// problems included (for commands that render a problem's own
    /// fields, like `sync push` listing each file's errors). Transport
    /// failures and redirects are still errors.
    pub fn send_raw(&self, r: &Request<'_>) -> Result<(u16, serde_json::Value), ApiError> {
        let mut req = self
            .agent
            .request(r.method, &self.url(r.path))
            .set("Accept", "application/json");
        if !self.token.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", self.token));
        }
        for (k, v) in r.query {
            req = req.query(k, v);
        }
        if let Some(key) = r.idempotency_key {
            req = req.set("Idempotency-Key", key);
        }
        if let Some(etag) = r.if_match {
            req = req.set("If-Match", etag);
        }
        let result = match r.body {
            Some(body) => req.send_json(body),
            None => req.call(),
        };
        match result {
            Ok(resp) if (300..400).contains(&resp.status()) => Err(redirect_refused(resp.status())),
            Ok(resp) => {
                let status = resp.status();
                let body = resp
                    .into_json()
                    .map_err(|e| ApiError::Transport(e.to_string()))?;
                Ok((status, body))
            }
            Err(ureq::Error::Status(status, resp)) => {
                Ok((status, resp.into_json().unwrap_or_default()))
            }
            Err(ureq::Error::Transport(t)) => Err(ApiError::Transport(t.to_string())),
        }
    }

    fn handle_response(
        result: Result<ureq::Response, ureq::Error>,
    ) -> Result<serde_json::Value, ApiError> {
        match result {
            Ok(resp) if (300..400).contains(&resp.status()) => Err(redirect_refused(resp.status())),
            Ok(resp) => {
                let value: serde_json::Value = resp
                    .into_json()
                    .map_err(|e| ApiError::Transport(e.to_string()))?;
                Ok(value)
            }
            Err(ureq::Error::Status(status, resp)) => Err(Self::problem(status, resp)),
            Err(ureq::Error::Transport(t)) => Err(ApiError::Transport(t.to_string())),
        }
    }
}

/// A problem response as an [`ApiError`], with the mapping `/mcp` uses too
/// (`runbook_mcp::backend::BackendError::from_problem`): the code, the
/// detail (or title, or message) and any `errors` list.
pub fn problem_from_body(status: u16, body: &serde_json::Value) -> ApiError {
    match runbook_mcp::backend::BackendError::from_problem(status, body) {
        runbook_mcp::backend::BackendError::Problem {
            status,
            code,
            message,
        } => ApiError::Problem {
            status,
            code,
            message,
            retry_after: None,
        },
        other => ApiError::Transport(other.to_string()),
    }
}

#[cfg(test)]
mod problem_mapping_tests {
    use super::{ApiError, problem_from_body};
    use serde_json::json;

    #[test]
    fn problems_map_as_the_mcp_server_maps_them() {
        let body = json!({"code": "request.invalid", "detail": "bad input",
                          "errors": [{"path": "/inputs/x", "message": "required"}]});
        let ApiError::Problem {
            status,
            code,
            message,
            ..
        } = problem_from_body(422, &body)
        else {
            panic!("not a problem");
        };
        assert_eq!((status, code.as_str()), (422, "request.invalid"));
        assert_eq!(
            message,
            "bad input (errors: [{\"message\":\"required\",\"path\":\"/inputs/x\"}])"
        );
        let ApiError::Problem { code, message, .. } = problem_from_body(500, &json!({})) else {
            panic!("not a problem");
        };
        assert_eq!((code.as_str(), message.as_str()), ("error", ""));
    }

    /// Release gate 2026-11-21: the core proxy relays the runner's own error
    /// envelope (`{"error":{"code","message"}}`), and `runbook runner
    /// credential set` printed a bare `error: ` for it.
    #[test]
    fn runner_error_envelopes_keep_their_code_and_message() {
        let body = json!({"error": {"code": "credential.invalid",
                                    "message": "invalid credential configuration"}});
        let ApiError::Problem {
            status,
            code,
            message,
            ..
        } = problem_from_body(400, &body)
        else {
            panic!("not a problem");
        };
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                "credential.invalid",
                "invalid credential configuration"
            )
        );
        // A core problem with its own code is read as before.
        let ApiError::Problem { code, .. } =
            problem_from_body(403, &json!({"code": "auth.csrf", "error": {"code": "x"}}))
        else {
            panic!("not a problem");
        };
        assert_eq!(code, "auth.csrf");
    }
}
