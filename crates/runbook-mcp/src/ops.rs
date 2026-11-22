//! Authoring operations shared by the CLI (`list`, `get`, `pull`, `push`,
//! `publish`) and the MCP tools, so both interfaces send the same requests.
//!
//! Drafts are saved and published with the ETag of the draft revision the
//! caller expects (`If-Match`). A caller that names an expected revision gets
//! `412 catalog.draft_conflict` without any write when the draft has moved.
use crate::backend::{AsyncBackend, BackendError, HttpCall};
use serde_json::{Value, json};

/// Pages walked when resolving a slug (100 runbooks each).
const MAX_PAGES: usize = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    pub runbook_id: String,
    pub source: String,
    pub revision: u64,
    pub etag: String,
}

/// A project ID or slug, or a runbook slug, used as one path segment.
pub fn check_segment<'a>(what: &str, value: &'a str) -> Result<&'a str, BackendError> {
    let ok = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(value)
    } else {
        Err(BackendError::Usage(format!("invalid {what}: '{value}'")))
    }
}

pub fn is_runbook_id(value: &str) -> bool {
    value.starts_with("rb_")
}

pub async fn list_runbooks<B: AsyncBackend + ?Sized>(
    b: &mut B,
    project: &str,
    limit: Option<u32>,
    cursor: Option<&str>,
) -> Result<Value, BackendError> {
    let project = check_segment("project", project)?;
    let mut call = HttpCall::get(format!("/v1/projects/{project}/runbooks"));
    if let Some(limit) = limit {
        call = call.query("limit", limit.to_string());
    }
    if let Some(cursor) = cursor {
        call = call.query("cursor", cursor);
    }
    Ok(b.call(call).await?.body)
}

/// The catalog entry with this slug, walking every page of the project.
pub async fn find_by_slug<B: AsyncBackend + ?Sized>(
    b: &mut B,
    project: &str,
    slug: &str,
) -> Result<Option<Value>, BackendError> {
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let page = list_runbooks(b, project, Some(100), cursor.as_deref()).await?;
        if let Some(found) = page["items"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|item| item["slug"].as_str() == Some(slug))
        {
            return Ok(Some(found.clone()));
        }
        match page["next_cursor"].as_str() {
            Some(next) if !next.is_empty() => cursor = Some(next.to_owned()),
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// An `rb_` ID as given, or a slug looked up in `project`.
pub async fn resolve_runbook<B: AsyncBackend + ?Sized>(
    b: &mut B,
    runbook: &str,
    project: Option<&str>,
) -> Result<String, BackendError> {
    let runbook = check_segment("runbook", runbook)?;
    if is_runbook_id(runbook) {
        return Ok(runbook.to_owned());
    }
    let project = project.ok_or_else(|| {
        BackendError::Usage(format!(
            "'{runbook}' is a slug; pass the project to look it up, or use the rb_ ID"
        ))
    })?;
    find_by_slug(b, project, runbook)
        .await?
        .and_then(|item| item["runbook_id"].as_str().map(str::to_owned))
        .ok_or_else(|| {
            BackendError::problem(
                404,
                "resource.not_found",
                format!("no runbook '{runbook}' in project '{project}'"),
            )
        })
}

/// Catalog entry plus the identity of the latest published version, its
/// input schema (`input_schema`, the start form's JSON Schema) and its step
/// outline (`outline: {steps, finally}`, titles only; CLI3). All three are
/// null when nothing is published yet; the schema and outline are also null
/// when the form cannot be read (the outline alone for an older server). The source is not included. The
/// form is read at the version's `rbv_` ID, so all three describe the same
/// version even if another is published meanwhile.
pub async fn get_runbook<B: AsyncBackend + ?Sized>(
    b: &mut B,
    runbook_id: &str,
) -> Result<Value, BackendError> {
    let mut view = b
        .call(HttpCall::get(format!("/v1/runbooks/{runbook_id}")))
        .await?
        .body;
    let latest = match b
        .call(HttpCall::get(format!(
            "/v1/runbooks/{runbook_id}/versions/latest"
        )))
        .await
    {
        Ok(reply) => json!({
            "runbook_version_id": reply.body["runbook_version_id"],
            "runbook_version": reply.body["runbook_version"],
        }),
        Err(BackendError::Problem { status: 404, .. }) => Value::Null,
        Err(e) => return Err(e),
    };
    // The form is extra detail: a read that fails (or a version the server
    // cannot build a form for) leaves both null rather than failing the
    // call, and an older server's form without `steps` has no outline.
    let (input_schema, outline) = match latest["runbook_version_id"].as_str() {
        Some(rbv) => {
            let rbv = check_segment("version", rbv)?;
            match b
                .call(HttpCall::get(format!(
                    "/v1/runbooks/{runbook_id}/versions/{rbv}/form"
                )))
                .await
            {
                Ok(reply) => {
                    let form = reply.body;
                    let outline = if form.get("steps").is_some_and(|s| !s.is_null()) {
                        json!({"steps": form["steps"], "finally": form["finally"]})
                    } else {
                        Value::Null
                    };
                    (form["inputs"].clone(), outline)
                }
                Err(_) => (Value::Null, Value::Null),
            }
        }
        None => (Value::Null, Value::Null),
    };
    if let Value::Object(map) = &mut view {
        map.insert("latest_version".into(), latest);
        map.insert("input_schema".into(), input_schema);
        map.insert("outline".into(), outline);
    }
    Ok(view)
}

/// One stored version with its source (`VersionSourceView`); `selector` is
/// a version number, an `rbv_` ID or `latest`.
pub async fn get_version<B: AsyncBackend + ?Sized>(
    b: &mut B,
    runbook_id: &str,
    selector: &str,
) -> Result<Value, BackendError> {
    let selector = check_segment("version", selector)?;
    Ok(b.call(HttpCall::get(format!(
        "/v1/runbooks/{runbook_id}/versions/{selector}"
    )))
    .await?
    .body)
}

pub async fn get_draft<B: AsyncBackend + ?Sized>(
    b: &mut B,
    runbook_id: &str,
) -> Result<Draft, BackendError> {
    let reply = b
        .call(HttpCall::get(format!("/v1/runbooks/{runbook_id}/draft")))
        .await?;
    let malformed = || BackendError::Transport("malformed draft response".into());
    Ok(Draft {
        runbook_id: reply.body["runbook_id"]
            .as_str()
            .unwrap_or(runbook_id)
            .to_owned(),
        source: reply.body["source"]
            .as_str()
            .ok_or_else(malformed)?
            .to_owned(),
        revision: reply.body["revision"].as_u64().ok_or_else(malformed)?,
        etag: reply.etag.ok_or_else(malformed)?,
    })
}

pub fn expect_revision(draft: &Draft, expected: Option<u64>) -> Result<(), BackendError> {
    match expected {
        Some(expected) if expected != draft.revision => Err(BackendError::problem(
            412,
            "catalog.draft_conflict",
            format!(
                "the draft of {} is at revision {}, not {expected}; reload it and reapply your change",
                draft.runbook_id, draft.revision
            ),
        )),
        _ => Ok(()),
    }
}

/// Create a runbook in `project` from `source` (its slug is `metadata.id`).
pub async fn create_runbook<B: AsyncBackend + ?Sized>(
    b: &mut B,
    project: &str,
    source: &str,
    key: &str,
) -> Result<Value, BackendError> {
    let project = check_segment("project", project)?;
    Ok(b.call(HttpCall::post(
        format!("/v1/projects/{project}/runbooks"),
        json!({"source": source}),
        key,
    ))
    .await?
    .body)
}

/// Save `source` as the next draft revision. With `expected`, the save only
/// applies on top of that revision.
pub async fn save_draft<B: AsyncBackend + ?Sized>(
    b: &mut B,
    runbook_id: &str,
    source: &str,
    expected: Option<u64>,
    key: &str,
) -> Result<Value, BackendError> {
    let draft = get_draft(b, runbook_id).await?;
    expect_revision(&draft, expected)?;
    save_over(b, &draft, source, key).await
}

/// Save `source` on top of `draft` (its ETag is the precondition).
pub async fn save_over<B: AsyncBackend + ?Sized>(
    b: &mut B,
    draft: &Draft,
    source: &str,
    key: &str,
) -> Result<Value, BackendError> {
    let call = HttpCall::write(
        "PATCH",
        format!("/v1/runbooks/{}/draft", draft.runbook_id),
        json!({"source": source}),
        key,
    )
    .if_match(draft.etag.clone());
    Ok(b.call(call).await?.body)
}

/// Publish the current draft (or exactly revision `expected`).
pub async fn publish<B: AsyncBackend + ?Sized>(
    b: &mut B,
    runbook_id: &str,
    expected: Option<u64>,
    key: &str,
) -> Result<(u64, Value), BackendError> {
    let draft = get_draft(b, runbook_id).await?;
    expect_revision(&draft, expected)?;
    let call = HttpCall::post(
        format!("/v1/runbooks/{runbook_id}/versions"),
        json!({}),
        key,
    )
    .if_match(draft.etag);
    Ok((draft.revision, b.call(call).await?.body))
}

/// Confirm the credential may publish in the runbook's project without
/// changing anything. There is no token-introspection route, but the
/// publish route authorizes the credential's scopes and project role before
/// it reads replay records or the draft precondition: without `If-Match` an
/// authorized caller gets `428 request.if_match_required` (a Git-managed
/// runbook `409 runbook.git_managed`), and nothing is written either way.
pub async fn probe_publish<B: AsyncBackend + ?Sized>(
    b: &mut B,
    runbook_id: &str,
) -> Result<(), BackendError> {
    let key = b.random_token()?;
    let call = HttpCall::post(
        format!("/v1/runbooks/{runbook_id}/versions"),
        json!({}),
        key,
    );
    match b.call(call).await {
        Err(BackendError::Problem {
            status: 428, code, ..
        }) if code == "request.if_match_required" => Ok(()),
        Err(BackendError::Problem {
            status: 409, code, ..
        }) if code == "runbook.git_managed" => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(BackendError::Transport(
            "the publish permission check was unexpectedly accepted".into(),
        )),
    }
}

/// Compile without saving. With a project, skills, users, teams and
/// variables resolve against it.
pub async fn validate<B: AsyncBackend + ?Sized>(
    b: &mut B,
    source: &str,
    project: Option<&str>,
    key: &str,
) -> Result<Value, BackendError> {
    let mut body = json!({"source": source});
    if let Some(project) = project {
        body["project"] = json!(check_segment("project", project)?);
    }
    Ok(b.call(HttpCall::post("/v1/validate", body, key))
        .await?
        .body)
}

/// The draft write view without the echoed source.
pub fn without_source(mut view: Value) -> Value {
    if let Value::Object(map) = &mut view {
        map.remove("source");
    }
    view
}

/// Synchronous wrappers over a blocking [`Backend`] (the CLI), with the
/// same names and arguments as the async operations above.
pub mod blocking {
    use super::Draft;
    pub use super::{check_segment, expect_revision, is_runbook_id, without_source};
    use crate::backend::{Backend, BackendError, Blocking, block_on};
    use serde_json::Value;

    pub fn list_runbooks(
        b: &mut dyn Backend,
        project: &str,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Value, BackendError> {
        block_on(super::list_runbooks(
            &mut Blocking(b),
            project,
            limit,
            cursor,
        ))
    }
    pub fn find_by_slug(
        b: &mut dyn Backend,
        project: &str,
        slug: &str,
    ) -> Result<Option<Value>, BackendError> {
        block_on(super::find_by_slug(&mut Blocking(b), project, slug))
    }
    pub fn resolve_runbook(
        b: &mut dyn Backend,
        runbook: &str,
        project: Option<&str>,
    ) -> Result<String, BackendError> {
        block_on(super::resolve_runbook(&mut Blocking(b), runbook, project))
    }
    pub fn get_runbook(b: &mut dyn Backend, runbook_id: &str) -> Result<Value, BackendError> {
        block_on(super::get_runbook(&mut Blocking(b), runbook_id))
    }
    pub fn get_version(
        b: &mut dyn Backend,
        runbook_id: &str,
        selector: &str,
    ) -> Result<Value, BackendError> {
        block_on(super::get_version(&mut Blocking(b), runbook_id, selector))
    }
    pub fn get_draft(b: &mut dyn Backend, runbook_id: &str) -> Result<Draft, BackendError> {
        block_on(super::get_draft(&mut Blocking(b), runbook_id))
    }
    pub fn create_runbook(
        b: &mut dyn Backend,
        project: &str,
        source: &str,
        key: &str,
    ) -> Result<Value, BackendError> {
        block_on(super::create_runbook(
            &mut Blocking(b),
            project,
            source,
            key,
        ))
    }
    pub fn save_draft(
        b: &mut dyn Backend,
        runbook_id: &str,
        source: &str,
        expected: Option<u64>,
        key: &str,
    ) -> Result<Value, BackendError> {
        block_on(super::save_draft(
            &mut Blocking(b),
            runbook_id,
            source,
            expected,
            key,
        ))
    }
    pub fn save_over(
        b: &mut dyn Backend,
        draft: &Draft,
        source: &str,
        key: &str,
    ) -> Result<Value, BackendError> {
        block_on(super::save_over(&mut Blocking(b), draft, source, key))
    }
    pub fn publish(
        b: &mut dyn Backend,
        runbook_id: &str,
        expected: Option<u64>,
        key: &str,
    ) -> Result<(u64, Value), BackendError> {
        block_on(super::publish(&mut Blocking(b), runbook_id, expected, key))
    }
    pub fn probe_publish(b: &mut dyn Backend, runbook_id: &str) -> Result<(), BackendError> {
        block_on(super::probe_publish(&mut Blocking(b), runbook_id))
    }
    pub fn validate(
        b: &mut dyn Backend,
        source: &str,
        project: Option<&str>,
        key: &str,
    ) -> Result<Value, BackendError> {
        block_on(super::validate(&mut Blocking(b), source, project, key))
    }
}

/// Runner identifiers are opaque URL segments. Also safe in the credential
/// handoff shell command, whose arguments use the same alphabet.
pub fn runner_segment(value: &str) -> Result<&str, BackendError> {
    if !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
    {
        Ok(value)
    } else {
        Err(BackendError::Usage("invalid runner identifier".into()))
    }
}

/// Refuse secret-bearing fields before a catalog request leaves the client.
pub fn check_runner_metadata(resource: &str, body: &Value) -> Result<(), BackendError> {
    let fields: &[&str] = match resource {
        "providers" => &[
            "name",
            "type",
            "auth",
            "credential_id",
            "endpoint",
            "region",
            "models",
            "sharing",
        ],
        "profiles" => &["name", "provider_id", "model", "options", "sharing"],
        "mcp-servers" => &[
            "name",
            "url",
            "transport",
            "credential_id",
            "tool_labels",
            "never_allowed",
            "sharing",
        ],
        "groups" => &[
            "name",
            "concurrency",
            "cost_per_day_usd",
            "cost_per_month_usd",
            "sandbox",
            "compute_rate",
        ],
        _ => {
            return Err(BackendError::Usage(
                "unknown runner catalog resource".into(),
            ));
        }
    };
    let invalid = || {
        BackendError::Usage("expected public runner configuration fields only. Use `runbook runner credential set` for secrets and `runbook runner group projects` for group assignments".into())
    };
    let data = body.as_object().ok_or_else(invalid)?;
    if data.keys().any(|field| !fields.contains(&field.as_str())) {
        return Err(invalid());
    }
    if resource == "providers"
        && let Some(models) = data.get("models")
    {
        let model_fields = [
            "id",
            "input_usd_per_million",
            "output_usd_per_million",
            "cached_usd_per_million",
            "cache_write_usd_per_million",
        ];
        if !models.as_array().is_some_and(|models| {
            models.iter().all(|model| {
                model.as_object().is_some_and(|model| {
                    model
                        .keys()
                        .all(|field| model_fields.contains(&field.as_str()))
                })
            })
        }) {
            return Err(invalid());
        }
    }
    if let Some(sharing) = data.get("sharing") {
        check_runner_sharing(&sharing["projects"])?;
    }
    Ok(())
}

pub fn check_runner_sharing(projects: &Value) -> Result<(), BackendError> {
    if matches!(projects.as_str(), Some("none" | "all")) {
        return Ok(());
    }
    if let Some(projects) = projects.as_array()
        && projects
            .iter()
            .all(|id| id.as_str().is_some_and(|id| runner_segment(id).is_ok()))
    {
        return Ok(());
    }
    Err(BackendError::Usage(
        "expected sharing.projects to be none, all or an array of project IDs".into(),
    ))
}

pub fn check_runner_tool_labels(labels: &Value) -> Result<(), BackendError> {
    if labels.as_object().is_some_and(|labels| {
        labels
            .values()
            .all(|label| matches!(label.as_str(), Some("read" | "write" | "destructive")))
    }) {
        return Ok(());
    }
    Err(BackendError::Usage(
        "expected a tool_labels object with read, write or destructive labels".into(),
    ))
}
