//! The CLI's implementation of the `runbook-mcp` backend: requests go
//! through the same `Session` (credential precedence, stored execution pairs,
//! serialized refresh) as every other command. Used by the authoring
//! commands and by `runbook mcp serve`.
use crate::agent::{Env, NoSession, resolve_session};
use crate::api::{ApiError, Request};
use crate::credstore::StepState;
use crate::session::{SessionError, random_hex};
use runbook_mcp::backend::{Backend, BackendError, CredentialHint, HttpCall, HttpReply};

pub struct CliBackend {
    env: Env,
}

impl CliBackend {
    pub fn new(env: Env) -> Self {
        Self { env }
    }
    pub fn env(&self) -> &Env {
        &self.env
    }
    fn step_state(&self, sr: &str) -> Option<StepState> {
        self.env
            .store
            .as_ref()?
            .load_step(sr)
            .ok()
            .flatten()
            .filter(|s| s.host == self.env.host)
    }
}

pub fn backend_error(e: SessionError) -> BackendError {
    match e {
        SessionError::Api(ApiError::Problem {
            status,
            code,
            message,
            ..
        }) => BackendError::Problem {
            status,
            code,
            message,
        },
        SessionError::Api(ApiError::Transport(t)) => BackendError::Transport(t),
        SessionError::Refresh {
            code,
            execution_id,
            terminal,
            ..
        } => BackendError::Credentials {
            code,
            execution_id,
            terminal,
        },
        SessionError::Local(msg) => BackendError::Transport(msg),
    }
}

impl Backend for CliBackend {
    fn call(&mut self, call: HttpCall) -> Result<HttpReply, BackendError> {
        let execution = match &call.credentials {
            CredentialHint::Default => None,
            CredentialHint::Execution(exe) => Some(exe.clone()),
            CredentialHint::StepRun(sr) => self.step_state(sr).and_then(|s| s.execution_id),
        };
        let mut session = resolve_session(&self.env, execution.as_deref()).map_err(|e| {
            match (e, &execution) {
                // The execution's pair was forgotten (its family is dead):
                // only a trusted launcher can install a new one.
                (NoSession::Missing(_), Some(exe)) if self.env.token.is_none() => {
                    BackendError::Credentials {
                        code: "credentials.missing".into(),
                        execution_id: exe.clone(),
                        terminal: true,
                    }
                }
                (NoSession::Missing(msg) | NoSession::Unreadable(msg), _) => {
                    BackendError::Usage(msg)
                }
            }
        })?;
        let request = Request {
            method: call.method,
            path: &call.path,
            query: &call.query,
            body: call.body.as_ref(),
            idempotency_key: call.idempotency_key.as_deref(),
            if_match: call.if_match.as_deref(),
        };
        let reply = session.send(&request).map_err(backend_error)?;
        Ok(HttpReply {
            body: reply.body,
            etag: reply.etag,
        })
    }

    fn random_token(&mut self) -> Result<String, BackendError> {
        random_hex().map_err(BackendError::Transport)
    }

    fn remember_step(&mut self, step_run_id: &str, execution_id: &str) {
        let Some(store) = &self.env.store else {
            return;
        };
        let mut state = self.step_state(step_run_id).unwrap_or_else(|| StepState {
            host: self.env.host.clone(),
            ..Default::default()
        });
        state.execution_id = Some(execution_id.to_owned());
        let _ = store.save_step(step_run_id, &state);
    }

    fn note_finished(&mut self, execution_id: &str, finished: bool) {
        if let Some(store) = &self.env.store {
            let _ = store.set_finished(&self.env.host, execution_id, finished);
        }
    }

    fn finished(&mut self, execution_id: &str) -> bool {
        self.env
            .store
            .as_ref()
            .is_some_and(|store| store.finished(&self.env.host, execution_id))
    }

    fn web_origin(&self) -> Option<String> {
        // The API host serves the web app too (`RUNBOOK_ORIGIN`).
        Some(self.env.host.trim_end_matches('/').to_owned())
    }

    fn execution_of_step(&mut self, step_run_id: &str) -> Option<String> {
        self.step_state(step_run_id)
            .and_then(|s| s.execution_id)
            .or_else(|| {
                // The active stored pair is bound to one execution.
                if self.env.token.is_some() {
                    return None;
                }
                let active = self.env.store.as_ref()?.active().ok().flatten()?;
                (active.host == self.env.host).then_some(active.execution_id)
            })
    }
}
