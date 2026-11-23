//! MEM3: runbook memory routes (PRD §7, §9; D40, D41; MEM6).
//!
//! - `GET|POST /v1/runbooks/{rb}/memory`, `GET|POST /v1/projects/{p}/memory`
//! - `GET|PATCH /v1/memory/{mem}` (`?revision=n` reads one prior revision)
//! - `GET /v1/memory/{mem}/revisions` (MEM6: every revision and its writer)
//!
//! A project reader (`runbooks:read`) searches and reads. Writes (MEM6,
//! user decision 2026-11-13): every writer with `runbooks:write` (a person,
//! an agent-kind personal token, a service token) saves live entries unless
//! it asks to propose, and edits live entries fenced by `If-Match`. Each
//! write names its writer, in the source and in the revision's actor.
//! Accepting, rejecting and archiving stay with people: an agent or a
//! service gets 403 `memory.person_required`. Agents and services read only
//! active, unexpired entries (a proposal, an archived entry and history are
//! 404 to them). An execution credential (D32) reads only active, unexpired
//! entries of its own runbook and project; its POST saves from the step run
//! it claims (D40: through the execution commit), and its PATCH edits an
//! active entry of its own runbook or project; anything else is 403. An
//! entry, runbook or project in another project of the organization is
//! 404, as for step reads.
use crate::{
    Request,
    actor_requests::post_identity,
    backends::Backends,
    execution_credentials::{access, storage},
    executions::ExecutionApiError as Error,
    problem::Response,
};
use runbook_core::{
    StoreError,
    actor::{ActorError, Replay, begin_actor},
    catalog::TenantScope,
    credentials::{
        AuthError, CredentialStore, Principal, Scope, TokenKind, authorize, parse_secret,
        secret_hash,
    },
    directory::DirectoryStore,
    directory_operations::DirectoryOperations,
    memory_entries::{self as memory, MemoryEntryRecord, MemoryQuery, MemoryStore},
    start::StartStore,
};
use runbook_model::*;
use serde_json::{Value, json};

const RUNBOOK: &str = "/v1/runbooks/{rb}/memory";
const PROJECT: &str = "/v1/projects/{p}/memory";
const ENTRY: &str = "/v1/memory/{mem}";
const REVISIONS: &str = "/v1/memory/{mem}/revisions";

pub fn recognizes(path: &str) -> bool {
    [RUNBOOK, PROJECT, ENTRY, REVISIONS]
        .iter()
        .any(|pattern| crate::directory::match_path(pattern, path).is_some())
}

struct Reply {
    status: u16,
    body: Value,
    etag: Option<String>,
}
fn entry_reply(status: u16, entry: &MemoryEntryRecord) -> Result<Reply, Error> {
    Ok(Reply {
        status,
        body: serde_json::to_value(memory::view(entry)).map_err(storage)?,
        etag: Some(etag(entry.revision)),
    })
}
/// The strong ETag of an entry revision; `If-Match` must repeat it.
pub fn etag(revision: u32) -> String {
    format!("\"r{revision}\"")
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok(reply) => {
            let mut out = Response::json(reply.status, reply.body, &r.request_id);
            out.etag = reply.etag;
            out
        }
        Err(e) => {
            let mut out = Response::error(e.status, e.code, &r.request_id);
            out.body["errors"] = json!(e.diagnostics);
            out
        }
    }
}

fn not_found() -> Error {
    Error::new(404, "resource.not_found")
}
fn forbidden() -> Error {
    Error::new(403, "auth.forbidden")
}

/// Map a memory use-case error to the route's problem.
pub(crate) fn rejection(e: ActorError) -> Error {
    let code = e.code();
    let status = match code {
        "memory.conflict" => 412,
        "execution.not_found" | "resource.not_found" => 404,
        "memory.revision_not_found" => 404,
        "auth.forbidden" | "memory.person_required" | "memory.locked" => 403,
        "store.backend" => 503,
        "memory.limit_reached"
        | "memory.edit_limit"
        | "memory.proposal_limit"
        | "memory.invalid_transition"
        | "memory.step_not_claimed"
        | "memory.step_mismatch"
        | "memory.step_run_ambiguous"
        | "memory.duplicate"
        | "execution.conflict"
        | "request.idempotency_key_reused" => 409,
        "request.invalid_cursor" => 400,
        _ => 422,
    };
    let mut error = Error::new(status, code);
    if let ActorError::Rejected { errors, .. } = e {
        error.diagnostics = errors
            .into_iter()
            .map(|f| Diagnostic {
                path: f.path,
                code: f.code,
                message: f.message,
                line: None,
                column: None,
            })
            .collect();
    }
    error
}
fn store_rejection(e: StoreError) -> Error {
    rejection(ActorError::Store(e))
}

fn parse_body<T: serde::de::DeserializeOwned>(body: &Value) -> Result<T, Error> {
    serde_json::from_value(body.clone()).map_err(|e| {
        let mut error = Error::new(422, "request.invalid");
        let message = e.to_string();
        let code = if message.contains("data.max_length") {
            "data.max_length"
        } else {
            "request.invalid"
        };
        error.diagnostics = vec![Diagnostic {
            path: if code == "data.max_length" {
                "body_markdown".into()
            } else {
                String::new()
            },
            code: code.into(),
            message: if code == "data.max_length" {
                "Memory body must be at most 4096 UTF-8 bytes.".into()
            } else {
                "The request body does not match the schema.".into()
            },
            line: None,
            column: None,
        }];
        error
    })
}

/// Search filters from the query string. Any other parameter is 400.
fn search_query(r: &Request) -> Result<(MemoryQuery, Option<String>, usize), Error> {
    let bad = || Error::new(400, "request.invalid_query");
    let mut query = MemoryQuery::default();
    let (mut cursor, mut limit) = (None, 50);
    for (key, value) in &r.query {
        match key.as_str() {
            "q" => query.text = Some(value.clone()),
            "tags" => {
                query.tags = value
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            "step_id" => query.step_id = Some(value.clone()),
            "status" => {
                query.status =
                    Some(serde_json::from_value(Value::String(value.clone())).map_err(|_| bad())?)
            }
            "include_expired" => {
                query.include_expired = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => return Err(bad()),
                }
            }
            "cursor" => cursor = Some(value.clone()),
            "limit" => {
                limit = value.parse().map_err(|_| bad())?;
                if !(1..=memory::MEMORY_PAGE_MAX).contains(&limit) {
                    return Err(bad());
                }
            }
            _ => return Err(bad()),
        }
    }
    Ok((query, cursor, limit))
}

fn page(page: memory::MemoryRecordPage) -> Result<Reply, Error> {
    Ok(Reply {
        status: 200,
        body: serde_json::to_value(MemoryPage {
            items: page.items.iter().map(memory::view).collect(),
            next_cursor: page.next_cursor,
        })
        .map_err(storage)?,
        etag: None,
    })
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<Reply, Error> {
    let (route, params) = [RUNBOOK, PROJECT, ENTRY, REVISIONS]
        .iter()
        .find_map(|pattern| crate::directory::match_path(pattern, &r.path).map(|p| (*pattern, p)))
        .ok_or_else(not_found)?;
    let allowed: &[&str] = match route {
        ENTRY => &["GET", "PATCH"],
        REVISIONS => &["GET"],
        _ => &["GET", "POST"],
    };
    if !allowed.contains(&r.method.as_str()) {
        return Err(Error::new(405, "request.method_not_allowed"));
    }
    // A browser session (or an OAuth grant riding one) authenticates as its
    // linked principal, as on every other route; before, memory was
    // bearer-only and answered a signed-in browser 401, which the web app
    // takes as a lost session and signs out.
    if r.is_session() {
        let invalid = || Error::new(401, "auth.invalid_credential");
        let record = r
            .credential_record(&backends.credentials())
            .await
            .map_err(|_| invalid())?;
        return principal(backends, r, record, None, route, &params).await;
    }
    let secret = r.bearer().ok_or_else(|| Error::new(401, "auth.required"))?;
    let (kind, _) = parse_secret(secret).map_err(|_| Error::new(401, "auth.invalid_credential"))?;
    if kind == TokenKind::Execution {
        return execution(backends, r, secret, route, &params).await;
    }
    let record = backends
        .credentials()
        .find_by_hash(&secret_hash(secret))
        .await
        .map_err(storage)?
        .ok_or_else(|| Error::new(401, "auth.invalid_credential"))?;
    principal(backends, r, record, Some(kind), route, &params).await
}

/// A PAT, service token, browser session or OAuth credential with
/// `runbooks:*` scopes. `kind` is the presented token's kind, checked
/// against the record; a session's principal has no secret to check.
async fn principal<B: Backends>(
    backends: &B,
    r: &Request,
    record: runbook_core::credentials::CredentialRecord,
    kind: Option<TokenKind>,
    route: &str,
    params: &std::collections::BTreeMap<String, String>,
) -> Result<Reply, Error> {
    let invalid = || Error::new(401, "auth.invalid_credential");
    let directory = backends.directory();
    let now = directory.current_time_ms().await.map_err(storage)?;
    if kind.is_some_and(|kind| record.kind != kind)
        || record.validate().is_err()
        || record.revoked_at.is_some()
        || record.expires_at.is_some_and(|t| t <= now)
    {
        return Err(invalid());
    }
    let scope = if r.method == "GET" {
        Scope::RunbooksRead
    } else {
        Scope::RunbooksWrite
    };
    // The scope check needs no resource, so it discloses nothing.
    if !record.scopes.contains(&scope) {
        return Err(Error::new(403, "auth.scope_missing"));
    }
    let snapshot = directory.snapshot(&record.org_id).await.map_err(storage)?;
    // The org-keyed lookups below need an org with at least one project.
    snapshot.projects.keys().next().ok_or_else(not_found)?;
    let mut runbook = None;
    let mut entry_id = None;
    let project = if let Some(p) = params.get("p") {
        snapshot
            .projects
            .values()
            .find(|project| project.id.as_str() == p || &project.slug == p)
            .map(|project| project.id.clone())
            .ok_or_else(not_found)?
    } else if let Some(rb) = params.get("rb") {
        let rb: RunbookId = rb.parse().map_err(|_| not_found())?;
        let project = backends
            .runbook_project(&record.org_id, &rb)
            .await
            .map_err(storage)?
            .ok_or_else(not_found)?;
        runbook = Some(rb);
        project
    } else {
        let id: MemoryId = params["mem"].parse().map_err(|_| not_found())?;
        let project = backends
            .memory_project(&record.org_id, &id)
            .await
            .map_err(storage)?
            .ok_or_else(not_found)?;
        entry_id = Some(id);
        project
    };
    // Another project of the organization reads as absent (step reads do the same).
    let principal = authorize(&record, &snapshot, now, &project, scope).map_err(|e| match e {
        AuthError::Forbidden => not_found(),
        _ => invalid(),
    })?;
    // A stored token records its last use; a session's principal is not a
    // stored token.
    if kind.is_some() {
        backends
            .credentials()
            .touch_last_used(&record.org_id, &record.id, now)
            .await
            .map_err(storage)?;
    }
    // The revision actor names the writer: the person, or the agent's or
    // service's token (MEM6), never a sponsor alone.
    let actor = match &principal {
        Principal::Agent { sponsor } => format!("agent:{}:{sponsor}", record.id),
        other => crate::actor_store::principal_key(other),
    };
    let tenant = TenantScope {
        org_id: record.org_id.clone(),
        project_id: project,
    };
    let store = backends.execution(tenant.clone());
    // People review, archive and read history; an agent or service writes
    // and edits live memory and reads only active, unexpired entries.
    let person = matches!(principal, Principal::Human { .. });
    let writer = if person {
        memory::MemoryWriter::Person
    } else {
        memory::MemoryWriter::Agent
    };
    if let Some(id) = entry_id {
        if route == REVISIONS {
            return revisions::<B>(&store, &tenant, &id, r, person).await;
        }
        if r.method == "GET" {
            return read_entry::<B>(&store, &tenant, &id, r, person).await;
        }
        // Review fix 7 and re-check fix 3: an agent learns nothing of an
        // entry it cannot see, so that check comes before every other
        // answer, a malformed body's included.
        let visible = if person {
            None
        } else {
            let now = store.memory_time_ms().await.map_err(storage)?;
            Some(
                store
                    .get_memory(&tenant, &id)
                    .await
                    .map_err(storage)?
                    .filter(|e| memory::visible_to_agent(e, now))
                    .ok_or_else(not_found)?,
            )
        };
        let request: MemoryPatchRequest = parse_body(&r.body)?;
        if let Some(current) = visible {
            if !writer.may(&request) {
                return Err(Error::new(403, "memory.person_required"));
            }
            if current.locked {
                return Err(Error::new(403, "memory.locked"));
            }
        }
        let expected = expected_revision(r)?;
        let steps = match &request {
            MemoryPatchRequest::Edit { .. } => {
                step_keys(
                    &store,
                    store
                        .get_memory(&tenant, &id)
                        .await
                        .map_err(storage)?
                        .and_then(|e| match e.memory_scope {
                            MemoryScope::Runbook { runbook_id } => Some(runbook_id),
                            MemoryScope::Project => None,
                        }),
                )
                .await?
            }
            _ => None,
        };
        let entry = memory::patch(
            &store,
            &tenant,
            &id,
            expected,
            &request,
            steps.as_deref(),
            &actor,
            writer,
        )
        .await
        .map_err(rejection)?;
        return entry_reply(200, &entry);
    }
    let memory_scope = match &runbook {
        Some(runbook_id) => MemoryScope::Runbook {
            runbook_id: runbook_id.clone(),
        },
        None => MemoryScope::Project,
    };
    if r.method == "GET" {
        let (mut query, cursor, limit) = search_query(r)?;
        // Searches read active memory unless a person asks for proposals
        // or history; an agent or service never sees them.
        if !person || query.status.is_none() {
            query.status = Some(MemoryStatus::Active);
        }
        query.include_expired &= person;
        // A runbook's memory is its own entries plus the project's.
        if route == RUNBOOK {
            query.within = vec![memory_scope, MemoryScope::Project];
        }
        return page(
            memory::search(&store, &tenant, &query, cursor, limit)
                .await
                .map_err(store_rejection)?,
        );
    }
    let key = r
        .idempotency_key
        .as_deref()
        .filter(|key| !key.trim().is_empty() && key.len() <= 256)
        .ok_or_else(|| Error::new(428, "request.idempotency_key_required"))?;
    // The source is who wrote it, naming an agent's or service's token. The
    // entry is live unless the request asks to propose (MEM6).
    let source = match principal {
        Principal::Human { user_id } => MemorySource::Person { user_id },
        Principal::Agent { sponsor } => MemorySource::Agent {
            token_id: record.id.clone(),
            sponsor_user_id: sponsor,
        },
        Principal::Service { token_id, .. } => MemorySource::Service { token_id },
    };
    let request: SaveMemoryRequest = parse_body(&r.body)?;
    let steps = step_keys(&store, runbook).await?;
    let id = memory::memory_id_for(&tenant, &actor, key);
    let entry = memory::create(
        &store,
        &tenant,
        id,
        memory_scope,
        &request,
        steps.as_deref(),
        source,
        &actor,
        writer,
    )
    .await
    .map_err(rejection)?;
    entry_reply(201, &entry)
}

/// The revision an `If-Match` names: missing is 428, unreadable 412.
fn expected_revision(r: &Request) -> Result<u32, Error> {
    let supplied = r
        .if_match
        .as_deref()
        .ok_or_else(|| Error::new(428, "request.if_match_required"))?;
    supplied
        .strip_prefix("\"r")
        .and_then(|s| s.strip_suffix('"'))
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| Error::new(412, "memory.conflict"))
}

/// MEM6: an entry's kept revisions with their writers, oldest first, a page
/// at a time (`limit` 1-100, default 50; `cursor` from the last page).
/// History is for people: an agent's or service's token gets 404, as for
/// `?revision=` reads.
async fn revisions<B: Backends>(
    store: &B::Execution,
    tenant: &TenantScope,
    id: &MemoryId,
    r: &Request,
    person: bool,
) -> Result<Reply, Error> {
    // History is for people; an agent learns nothing from a malformed
    // query either (re-check fix 3).
    if !person {
        return Err(not_found());
    }
    let bad = || Error::new(400, "request.invalid_query");
    let (mut after, mut limit) = (0_u32, 50_usize);
    for (key, value) in &r.query {
        match key.as_str() {
            "cursor" => {
                after = value
                    .parse()
                    .map_err(|_| Error::new(400, "request.invalid_cursor"))?
            }
            "limit" => {
                limit = value.parse().map_err(|_| bad())?;
                if !(1..=memory::MEMORY_PAGE_MAX).contains(&limit) {
                    return Err(bad());
                }
            }
            _ => return Err(bad()),
        }
    }
    let current = store
        .get_memory(tenant, id)
        .await
        .map_err(storage)?
        .ok_or_else(not_found)?;
    let mut page = store
        .memory_revision_page(tenant, id, after, limit + 1)
        .await
        .map_err(storage)?;
    let more = page.len() > limit;
    page.truncate(limit);
    let next_cursor = more
        .then(|| page.last().map(|r| r.entry.revision.to_string()))
        .flatten();
    Ok(Reply {
        status: 200,
        body: serde_json::to_value(MemoryRevisionPage {
            items: page.iter().map(memory::revision_view).collect(),
            next_cursor,
            pruned: current.revisions_pruned,
        })
        .map_err(storage)?,
        etag: None,
    })
}

/// The step keys of a runbook's draft and latest published version, which a
/// runbook entry's step ID must name; `None` for a project entry.
async fn step_keys<S: runbook_core::catalog::CatalogStore>(
    store: &S,
    runbook_id: Option<RunbookId>,
) -> Result<Option<Vec<String>>, Error> {
    let Some(runbook_id) = runbook_id else {
        return Ok(None);
    };
    let mut keys = vec![];
    if let Some(draft) = store.get_draft(&runbook_id).await.map_err(storage)? {
        keys.extend(memory::source_step_keys(&draft.source));
    }
    if let Ok(version) = store.resolve_version(&runbook_id, None).await
        && let Some(record) = store
            .version_record(&runbook_id, &version.runbook_version_id)
            .await
            .map_err(storage)?
    {
        keys.extend(memory::source_step_keys(&record.source));
    }
    Ok(Some(keys))
}

async fn read_entry<B: Backends>(
    store: &B::Execution,
    tenant: &TenantScope,
    id: &MemoryId,
    r: &Request,
    person: bool,
) -> Result<Reply, Error> {
    let now = store.memory_time_ms().await.map_err(storage)?;
    // An agent or service reads the current revision of an active entry
    // only: proposals, rejections and history are for people reviewing.
    // Any query from it is 404, whatever the entry (re-check fix 3).
    if !person {
        let current = store
            .get_memory(tenant, id)
            .await
            .map_err(storage)?
            .filter(|e| r.query.is_empty() && memory::visible_to_agent(e, now))
            .ok_or_else(not_found)?;
        return entry_reply(200, &current);
    }
    let bad = || Error::new(400, "request.invalid_query");
    let mut revision = None;
    for (key, value) in &r.query {
        match key.as_str() {
            "revision" => revision = Some(value.parse::<u32>().map_err(|_| bad())?),
            _ => return Err(bad()),
        }
    }
    let current = store
        .get_memory(tenant, id)
        .await
        .map_err(storage)?
        .ok_or_else(not_found)?;
    let Some(revision) = revision else {
        return entry_reply(200, &current);
    };
    // One revision by key (review fix 2): a long history is never loaded.
    let prior = store
        .memory_revision(tenant, id, revision)
        .await
        .map_err(storage)?
        .ok_or_else(not_found)?;
    let mut reply = entry_reply(200, &prior.entry)?;
    // The ETag always names the current revision, the one PATCH must match.
    reply.etag = Some(etag(current.revision));
    Ok(reply)
}

/// D32 execution credentials: own runbook and project, active entries. A
/// POST saves from the claimed step run (live, or a proposal when asked);
/// a PATCH edits an active entry of its own runbook or project under a
/// live claim (MEM6). Either write needs the credential's issuer to hold
/// `runbooks:write` (review fix 4).
async fn execution<B: Backends>(
    backends: &B,
    r: &Request,
    secret: &str,
    route: &str,
    params: &std::collections::BTreeMap<String, String>,
) -> Result<Reply, Error> {
    let verified = access(backends, secret).await?;
    let tenant = verified.scope.clone();
    let exe = verified.principal.execution_id.clone();
    let store = backends.execution(tenant.clone());
    let own = store
        .start_record(&exe)
        .await
        .map_err(storage)?
        .ok_or_else(forbidden)?
        .runbook_id;
    let readable = [
        MemoryScope::Runbook {
            runbook_id: own.clone(),
        },
        MemoryScope::Project,
    ];
    if let Some(rb) = params.get("rb")
        && rb != own.as_str()
    {
        return Err(forbidden());
    }
    if let Some(p) = params.get("p")
        && p != tenant.project_id.as_str()
    {
        return Err(forbidden());
    }
    if let Some(id) = params.get("mem") {
        if route == REVISIONS || !r.query.is_empty() {
            return Err(forbidden());
        }
        let id: MemoryId = id.parse().map_err(|_| forbidden())?;
        let now = store.memory_time_ms().await.map_err(storage)?;
        let entry = store
            .get_memory(&tenant, &id)
            .await
            .map_err(storage)?
            .filter(|e| {
                e.status == MemoryStatus::Active
                    && !memory::is_expired(e, now)
                    && readable.contains(&e.memory_scope)
            })
            .ok_or_else(forbidden)?;
        if r.method == "GET" {
            return entry_reply(200, &entry);
        }
        // MEM6: an agent edits live memory; reviewing, archiving and
        // locking stay with people.
        let request: MemoryPatchRequest = parse_body(&r.body)?;
        if !memory::MemoryWriter::Agent.may(&request) {
            return Err(Error::new(403, "memory.person_required"));
        }
        if entry.locked {
            return Err(Error::new(403, "memory.locked"));
        }
        let expected = expected_revision(r)?;
        issuer_writes_memory(backends, &store, &verified).await?;
        let steps = match &entry.memory_scope {
            MemoryScope::Runbook { runbook_id } => {
                step_keys(&store, Some(runbook_id.clone())).await?
            }
            MemoryScope::Project => None,
        };
        let identity = crate::actor_store::Identity {
            service_attribution: None,
            key: format!("execution:{}", verified.principal.family_id),
            human: None,
            credential_ref: verified.record.token_id.to_string(),
        };
        let work = begin_actor(&store, &backends.directory(), tenant.clone(), &exe)
            .await
            .map_err(crate::actor_store::actor_error)?;
        // Recheck revocation after waiting for the execution lock.
        access(backends, secret).await?;
        let principal = crate::actor_store::control_principal(&identity);
        let entry = memory::edit_in_run(
            work,
            &principal,
            &store,
            &tenant,
            &id,
            expected,
            &request,
            steps.as_deref(),
        )
        .await
        .map_err(rejection)?;
        return entry_reply(200, &entry);
    }
    if r.method == "GET" {
        let (mut query, cursor, limit) = search_query(r)?;
        if query.status.is_some_and(|s| s != MemoryStatus::Active) || query.include_expired {
            return Err(forbidden());
        }
        query.status = Some(MemoryStatus::Active);
        query.within = readable.to_vec();
        return page(
            memory::search(&store, &tenant, &query, cursor, limit)
                .await
                .map_err(store_rejection)?,
        );
    }
    // D40: a save (or, asked for, a proposal) from the step run this
    // credential claims, through the execution commit.
    let memory_scope = if route == RUNBOOK {
        MemoryScope::Runbook { runbook_id: own }
    } else {
        MemoryScope::Project
    };
    let request: SaveMemoryRequest = parse_body(&r.body)?;
    issuer_writes_memory(backends, &store, &verified).await?;
    let identity = crate::actor_store::Identity {
        service_attribution: None,
        key: format!("execution:{}", verified.principal.family_id),
        human: None,
        credential_ref: verified.record.token_id.to_string(),
    };
    let replay = post_identity(r, &identity.key)?;
    let key = r.idempotency_key.as_deref().unwrap_or_default();
    let id = memory::memory_id_for(&tenant, &identity.key, key);
    let work = begin_actor(&store, &backends.directory(), tenant.clone(), &exe)
        .await
        .map_err(crate::actor_store::actor_error)?;
    // Recheck revocation after waiting for the execution lock.
    access(backends, secret).await?;
    let principal = crate::actor_store::control_principal(&identity);
    let replay = Replay {
        key: replay.key,
        principal: replay.principal,
        request_hash: replay.request_hash,
    };
    let entry = memory::save_in_run(work, &principal, id, memory_scope, &request, Some(&replay))
        .await
        .map_err(rejection)?;
    // Review fix 7: a retry after people archived the entry, or after it
    // expired, learns nothing more (on Cloudflare a fresh save may not be
    // in the catalog yet; that is not a hidden entry).
    let now = store.memory_time_ms().await.map_err(storage)?;
    if store
        .get_memory(&tenant, &entry.id)
        .await
        .map_err(storage)?
        .is_some_and(|current| memory::hidden_after_replay(&current, now))
    {
        return Err(forbidden());
    }
    entry_reply(201, &entry)
}

/// Review fix 4: a run's memory write needs its credential's issuer to hold
/// `runbooks:write` now; `executions:act` alone is 403 `auth.scope_missing`.
async fn issuer_writes_memory<B: Backends>(
    backends: &B,
    store: &B::Execution,
    verified: &crate::execution_credentials::Access,
) -> Result<(), Error> {
    let now = backends
        .directory()
        .current_time_ms()
        .await
        .map_err(storage)?;
    let holds = runbook_core::execution_credentials::issuer_holds(
        store,
        &backends.credentials(),
        &backends.directory(),
        &verified.principal.family_id,
        now,
        Scope::RunbooksWrite,
    )
    .await
    .map_err(storage)?;
    if holds {
        Ok(())
    } else {
        Err(Error::new(403, "auth.scope_missing"))
    }
}

/// D41: the labeled memory block for a step read and the refs it returned.
pub async fn step_memory(
    store: &impl MemoryStore,
    scope: &TenantScope,
    runbook_id: &RunbookId,
    path: &str,
) -> Result<(Value, Vec<MemoryReadRef>), Error> {
    let entries = memory::read_for_step(store, scope, runbook_id, path)
        .await
        .map_err(storage)?;
    Ok((memory::step_block(&entries), memory::read_refs(&entries)))
}
