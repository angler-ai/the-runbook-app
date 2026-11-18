//! An authenticated API session. With a stored execution pair it refreshes
//! transparently: before a request when the access token is about to expire,
//! or once after the server rejects it as expired. Refreshes are serialized
//! across processes by the store's lock file and rotate both secrets. A
//! failed refresh is never retried; the caller must obtain a fresh pair from
//! a trusted launcher (PRD §9, "lost refresh response").
use crate::api::{ApiClient, ApiError};
use crate::credstore::{ExecutionCredential, StateDir};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Refresh when less than this much access-token lifetime remains.
const REFRESH_MARGIN_MS: i64 = 60_000;
/// Longer than one refresh can take (connect plus read timeouts).
pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(90);

pub enum Credential {
    /// `RUNBOOK_TOKEN`: a PAT, service token or raw execution token. Never refreshed.
    Static(String),
    Execution(ExecutionCredential),
}

#[derive(Debug)]
pub enum SessionError {
    Api(ApiError),
    /// The execution pair could not be refreshed; `code` is the server's
    /// problem code (or `network.error`).
    Refresh {
        code: String,
        execution_id: String,
        /// The family is dead (reused, revoked, expired, run closed) and was
        /// forgotten; only a trusted launcher can issue a new pair.
        terminal: bool,
        /// Stable exit code: 4 for a rate/quota limit, otherwise 1.
        exit: i32,
    },
    Local(String),
}

impl From<ApiError> for SessionError {
    fn from(e: ApiError) -> Self {
        SessionError::Api(e)
    }
}

pub struct Session {
    host: String,
    credential: Credential,
    store: Option<StateDir>,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub fn random_hex() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| "cannot generate random bytes".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Session {
    pub fn new_static(host: String, token: String) -> Self {
        Self {
            host,
            credential: Credential::Static(token),
            store: None,
        }
    }
    pub fn new_execution(host: String, credential: ExecutionCredential, store: StateDir) -> Self {
        Self {
            host,
            credential: Credential::Execution(credential),
            store: Some(store),
        }
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn credential(&self) -> &Credential {
        &self.credential
    }
    /// The execution a stored pair is bound to.
    pub fn execution_id(&self) -> Option<&str> {
        match &self.credential {
            Credential::Execution(c) => Some(&c.execution_id),
            Credential::Static(_) => None,
        }
    }
    fn client(&self) -> ApiClient {
        let token = match &self.credential {
            Credential::Static(t) => t.clone(),
            Credential::Execution(c) => c.access_token.clone(),
        };
        ApiClient::new(self.host.clone(), token)
    }

    pub fn get(&mut self, path: &str, query: &[(&str, &str)]) -> Result<Value, SessionError> {
        self.call(|c| c.get_json_query(path, query))
    }
    pub fn post(&mut self, path: &str, key: &str, body: &Value) -> Result<Value, SessionError> {
        self.call(|c| c.post_json(path, key, body))
    }
    /// Any request (authoring, MCP); returns the body and its `ETag`.
    pub fn send(&mut self, r: &crate::api::Request<'_>) -> Result<crate::api::Reply, SessionError> {
        self.call(|c| c.send(r))
    }
    pub fn sse(
        &mut self,
        path: &str,
        last_event_id: Option<&str>,
    ) -> Result<Box<dyn std::io::Read + Send>, SessionError> {
        self.ensure_fresh()?;
        Ok(self.client().get_sse_stream(path, last_event_id)?)
    }

    fn ensure_fresh(&mut self) -> Result<(), SessionError> {
        if let Credential::Execution(c) = &self.credential
            && c.expires_at_ms - now_ms() < REFRESH_MARGIN_MS
        {
            self.refresh()?;
        }
        Ok(())
    }

    fn call<T, F: Fn(&ApiClient) -> Result<T, ApiError>>(
        &mut self,
        f: F,
    ) -> Result<T, SessionError> {
        self.ensure_fresh()?;
        let result = match f(&self.client()) {
            Err(ApiError::Problem {
                status: 401, code, ..
            }) if code == "credentials.expired"
                && matches!(self.credential, Credential::Execution(_)) =>
            {
                self.refresh()?;
                f(&self.client())
            }
            other => other,
        };
        if let Err(ApiError::Problem {
            status: 401, code, ..
        }) = &result
            && code == "credentials.revoked"
        {
            self.forget_revoked();
        }
        Ok(result?)
    }

    /// A revoked execution token is permanent (rotation never revokes the
    /// previous access token; only a revoked family or a closed run does):
    /// forget the stored pair, unless a launcher has meanwhile installed a
    /// newer family for the execution (F65).
    fn forget_revoked(&self) {
        let (Credential::Execution(used), Some(store)) = (&self.credential, &self.store) else {
            return;
        };
        let Ok(_guard) = store.lock(&used.execution_id, LOCK_WAIT) else {
            return;
        };
        if let Ok(Some(current)) = store.load_execution(&used.execution_id)
            && current.host == used.host
            && current.family_id == used.family_id
        {
            let _ = store.remove_execution(&used.execution_id);
        }
    }

    fn refresh(&mut self) -> Result<(), SessionError> {
        let Credential::Execution(used) = &self.credential else {
            return Ok(());
        };
        let used = used.clone();
        let store = self
            .store
            .clone()
            .ok_or_else(|| SessionError::Local("no credential store".into()))?;
        let local = |e: std::io::Error| SessionError::Local(e.to_string());
        let _guard = store.lock(&used.execution_id, LOCK_WAIT).map_err(local)?;
        // The stored pair is the newest: another process may have rotated it
        // while this one waited. Never spend a refresh token that a newer
        // stored pair has replaced; that is reuse and revokes the family.
        let current = match store.load_execution(&used.execution_id).map_err(local)? {
            Some(current) if current.host == self.host => current,
            // Another process found the family dead and forgot it.
            _ => {
                return Err(SessionError::Refresh {
                    code: "credentials.missing".into(),
                    execution_id: used.execution_id,
                    terminal: true,
                    exit: 1,
                });
            }
        };
        if current.access_token != used.access_token
            && current.expires_at_ms - now_ms() >= REFRESH_MARGIN_MS
        {
            self.credential = Credential::Execution(current);
            return Ok(());
        }
        let key = random_hex().map_err(SessionError::Local)?;
        let anonymous = ApiClient::new(self.host.clone(), String::new());
        let path = format!("/v1/executions/{}/tokens/refresh", current.execution_id);
        match anonymous.post_json_anonymous(
            &path,
            &key,
            &json!({"refresh_token": current.refresh_token}),
        ) {
            Ok(pair) => {
                let rotated = rotated_pair(&current, &pair)
                    .ok_or_else(|| SessionError::Local("malformed refresh response".into()))?;
                store.save_execution(&rotated).map_err(local)?;
                self.credential = Credential::Execution(rotated);
                Ok(())
            }
            Err(e) => {
                // Forget the pair only when the server says the family is
                // dead. Transient problems (429, 5xx, transport) keep it; if a
                // lost response consumed it, the next use is reuse-detected.
                let terminal = e.code().is_some_and(terminal_refresh_code);
                if terminal {
                    let _ = store.remove_execution(&current.execution_id);
                }
                Err(SessionError::Refresh {
                    code: e.code().unwrap_or("network.error").to_string(),
                    execution_id: current.execution_id,
                    terminal,
                    exit: e.exit_code(),
                })
            }
        }
    }
}

/// Refresh rejections after which the family can never be used again.
fn terminal_refresh_code(code: &str) -> bool {
    matches!(
        code,
        "credentials.refresh_reused"
            | "credentials.revoked"
            // D31: the issuer lost the right; the family is refused from now on.
            | "credentials.forbidden"
            | "credentials.invalid"
            | "credentials.expired"
            | "auth.invalid_credential"
            | "execution.closed"
    )
}

/// The successor pair. A missing access expiry is treated as "unknown": the
/// CLI then refreshes only after the server rejects the token as expired,
/// never pre-emptively with a token it cannot date.
fn rotated_pair(previous: &ExecutionCredential, pair: &Value) -> Option<ExecutionCredential> {
    Some(ExecutionCredential {
        host: previous.host.clone(),
        execution_id: previous.execution_id.clone(),
        family_id: pair["family_id"]
            .as_str()
            .unwrap_or(&previous.family_id)
            .into(),
        generation: pair["generation"]
            .as_u64()
            .unwrap_or(previous.generation + 1),
        access_token: pair["access_token"].as_str()?.into(),
        refresh_token: pair["refresh_token"].as_str()?.into(),
        expires_at_ms: pair["expires_at_ms"].as_i64().unwrap_or(i64::MAX),
        refresh_expires_at_ms: pair["refresh_expires_at_ms"].as_i64().unwrap_or(i64::MAX),
    })
}
