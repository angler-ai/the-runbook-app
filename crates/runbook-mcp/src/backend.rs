//! The transport seam. This crate builds requests; the host (the `runbook`
//! CLI) sends them with its own HTTP client, credential store and refresh,
//! so no HTTP client or credential handling is duplicated here and the model
//! never sees a credential.
use serde_json::Value;
use std::future::Future;

/// Which stored credential a call should use. The host maps this to its
/// precedence rules (an explicit token, then the execution's stored pair).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialHint {
    Default,
    Execution(String),
    StepRun(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpCall {
    pub method: &'static str,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
    pub idempotency_key: Option<String>,
    pub if_match: Option<String>,
    pub credentials: CredentialHint,
}

impl HttpCall {
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: "GET",
            path: path.into(),
            query: vec![],
            body: None,
            idempotency_key: None,
            if_match: None,
            credentials: CredentialHint::Default,
        }
    }
    pub fn post(path: impl Into<String>, body: Value, key: impl Into<String>) -> Self {
        Self::write("POST", path, body, key)
    }
    pub fn write(
        method: &'static str,
        path: impl Into<String>,
        body: Value,
        key: impl Into<String>,
    ) -> Self {
        Self {
            method,
            path: path.into(),
            query: vec![],
            body: Some(body),
            idempotency_key: Some(key.into()),
            if_match: None,
            credentials: CredentialHint::Default,
        }
    }
    pub fn query(mut self, key: &str, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }
    pub fn if_match(mut self, etag: impl Into<String>) -> Self {
        self.if_match = Some(etag.into());
        self
    }
    pub fn for_execution(mut self, execution_id: &str) -> Self {
        self.credentials = CredentialHint::Execution(execution_id.into());
        self
    }
    pub fn for_step(mut self, step_run_id: &str) -> Self {
        self.credentials = CredentialHint::StepRun(step_run_id.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpReply {
    pub body: Value,
    pub etag: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendError {
    /// An API problem response (or a local check that mirrors one).
    Problem {
        status: u16,
        code: String,
        message: String,
    },
    /// The stored execution pair could not be refreshed. `terminal` means
    /// the family is dead and only a trusted launcher can issue a new pair.
    Credentials {
        code: String,
        execution_id: String,
        terminal: bool,
    },
    /// Invalid arguments or missing local configuration.
    Usage(String),
    Transport(String),
}

impl BackendError {
    pub fn problem(status: u16, code: &str, message: impl Into<String>) -> Self {
        Self::Problem {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
    /// An API problem response as the CLI's client reads it: the `code`,
    /// the `detail` (or `title`, or `message`) and, when present, the
    /// `errors` list that says which value failed and why. Hosts that call
    /// the API in process (`/mcp`) use this so their tool results match the
    /// CLI's.
    pub fn from_problem(status: u16, body: &Value) -> Self {
        let code = body["code"].as_str().unwrap_or("error").to_owned();
        let mut message = body
            .get("detail")
            .or_else(|| body.get("title"))
            .or_else(|| body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if let Some(errors) = body["errors"].as_array().filter(|e| !e.is_empty()) {
            message = format!("{message} (errors: {})", Value::Array(errors.clone()));
        }
        Self::Problem {
            status,
            code,
            message,
        }
    }
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Problem { code, .. } | Self::Credentials { code, .. } => Some(code),
            _ => None,
        }
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Problem { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Problem { code, message, .. } if message.is_empty() => write!(f, "{code}"),
            Self::Problem { code, message, .. } => write!(f, "{code}: {message}"),
            Self::Credentials {
                code, execution_id, ..
            } => write!(
                f,
                "{code}: the execution credentials for {execution_id} could not be refreshed"
            ),
            Self::Usage(msg) => write!(f, "{msg}"),
            Self::Transport(msg) => write!(f, "network error: {msg}"),
        }
    }
}

pub trait Backend {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError>;
    /// A fresh random token (idempotency keys, data-block nonces).
    fn random_token(&mut self) -> Result<String, BackendError>;
    /// Record which execution a step run belongs to, so later calls for the
    /// step pick that execution's credentials (as `runbook step next` does).
    fn remember_step(&mut self, _step_run_id: &str, _execution_id: &str) {}
    /// The execution a step run belongs to, when known.
    fn execution_of_step(&mut self, _step_run_id: &str) -> Option<String> {
        None
    }
    /// Record whether this host's last write on the execution ended its
    /// work there (a report, release, request_human or decision succeeded;
    /// `false` after a new claim). A revoked credential after that is the
    /// run ending, not a failure.
    fn note_finished(&mut self, _execution_id: &str, _finished: bool) {}
    /// Whether [`Backend::note_finished`] last recorded `true`.
    fn finished(&mut self, _execution_id: &str) -> bool {
        false
    }
    /// The web app's origin (`https://host`, no trailing slash), for the
    /// links `search` and `fetch` return. `None` gives host-relative links.
    fn web_origin(&self) -> Option<String> {
        None
    }
}

/// The tools' backend as the handlers use it (CF0): `call` and the step
/// lookup may wait, so a host can answer them in process on a
/// single-threaded executor (the native server's worker, a Workers
/// `fetch` handler). No `Send` bound. A synchronous [`Backend`] serves
/// through [`Blocking`].
pub trait AsyncBackend {
    fn call(&mut self, call: HttpCall) -> impl Future<Output = Result<HttpReply, BackendError>>;
    /// A fresh random token (idempotency keys, data-block nonces).
    fn random_token(&mut self) -> Result<String, BackendError>;
    /// See [`Backend::remember_step`].
    fn remember_step(&mut self, _step_run_id: &str, _execution_id: &str) {}
    /// See [`Backend::execution_of_step`].
    fn execution_of_step(&mut self, _step_run_id: &str) -> impl Future<Output = Option<String>> {
        std::future::ready(None)
    }
    /// See [`Backend::note_finished`].
    fn note_finished(&mut self, _execution_id: &str, _finished: bool) {}
    /// See [`Backend::finished`].
    fn finished(&mut self, _execution_id: &str) -> bool {
        false
    }
    /// See [`Backend::web_origin`].
    fn web_origin(&self) -> Option<String> {
        None
    }
}

/// A synchronous [`Backend`] (the CLI's blocking client, test fakes) as an
/// [`AsyncBackend`] whose futures are ready at once.
pub struct Blocking<'a, B: ?Sized>(pub &'a mut B);

impl<B: Backend + ?Sized> AsyncBackend for Blocking<'_, B> {
    fn call(&mut self, call: HttpCall) -> impl Future<Output = Result<HttpReply, BackendError>> {
        std::future::ready(self.0.call(call))
    }
    fn random_token(&mut self) -> Result<String, BackendError> {
        self.0.random_token()
    }
    fn remember_step(&mut self, step_run_id: &str, execution_id: &str) {
        self.0.remember_step(step_run_id, execution_id);
    }
    fn execution_of_step(&mut self, step_run_id: &str) -> impl Future<Output = Option<String>> {
        std::future::ready(self.0.execution_of_step(step_run_id))
    }
    fn note_finished(&mut self, execution_id: &str, finished: bool) {
        self.0.note_finished(execution_id, finished);
    }
    fn finished(&mut self, execution_id: &str) -> bool {
        self.0.finished(execution_id)
    }
    fn web_origin(&self) -> Option<String> {
        self.0.web_origin()
    }
}

/// Drive a future whose every wait is already satisfied, as a [`Blocking`]
/// backend's are. A future that is still pending is polled again after
/// yielding the thread, so this never deadlocks, but it is meant for ready
/// futures only.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
        std::thread::yield_now();
    }
}
