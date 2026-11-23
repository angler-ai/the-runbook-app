//! Profile photos, project images and project settings (2026-11-17).
//!
//! - `PUT` / `DELETE /v1/me/picture`: the signed-in person's own photo.
//! - `GET /v1/users/{user}/picture`: anyone in an org the person belongs
//!   to; the image, or a redirect to their Google photo.
//! - `GET /v1/projects/{p}/picture`: anyone in the project's org (the
//!   project list is the org's); `PUT` / `DELETE` need project admin.
//! - `PATCH /v1/projects/{p}`: a project's display name (project admin).
use crate::{
    Request,
    backends::Backends,
    problem::{Attachment, Response},
};
use base64::Engine as _;
use runbook_core::{
    credentials::{ActorKind, CredentialPrincipal, Scope, TokenKind, authorize},
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore},
    directory_operations::DirectoryOperations,
    pictures::{Picture, PictureOwner, PictureStore, inspect},
};
use runbook_model::{
    PictureUpload, PictureView, ProjectId, ProjectView, UpdateProjectRequest, UserId,
};
use serde_json::{Value, json};

type Failure = (u16, &'static str);
const UNAVAILABLE: Failure = (503, "storage.unavailable");
const NOT_FOUND: Failure = (404, "resource.not_found");

const ME: &str = "/v1/me/picture";
const USER: &str = "/v1/users/{user}/picture";
const PROJECT: &str = "/v1/projects/{p}";
const PROJECT_PICTURE: &str = "/v1/projects/{p}/picture";

pub fn recognizes(path: &str) -> bool {
    path == ME
        || [USER, PROJECT, PROJECT_PICTURE]
            .iter()
            .any(|template| crate::directory::match_path(template, path).is_some())
}

enum Reply {
    Json(u16, Value),
    Image(Picture),
    Redirect(String),
}

pub async fn handle<B: Backends>(backends: &B, r: Request) -> Response {
    match dispatch(backends, &r).await {
        Ok(Reply::Json(status, body)) => Response::json(status, body, &r.request_id),
        Ok(Reply::Image(picture)) => {
            let extension = if picture.content_type == "image/png" {
                "png"
            } else {
                "jpg"
            };
            let mut response = Response::json(200, Value::Null, &r.request_id);
            response.attachment = Some(Box::new(Attachment {
                content_type: if extension == "png" {
                    "image/png"
                } else {
                    "image/jpeg"
                },
                filename: format!("picture.{extension}"),
                bytes: picture.bytes,
                inline: true,
            }));
            response
        }
        Ok(Reply::Redirect(location)) => {
            let mut response = Response::json(302, Value::Null, &r.request_id);
            response.navigation = Some(Box::new(crate::problem::Navigation::Redirect(location)));
            response
        }
        Err((status, code)) => Response::error(status, code, &r.request_id),
    }
}

/// The base64 upload, checked: a PNG or JPEG within the limits.
fn decode(body: &Value, now: i64) -> Result<(Picture, PictureView), Failure> {
    let upload: PictureUpload =
        serde_json::from_value(body.clone()).map_err(|_| (422, "request.invalid"))?;
    let data = upload
        .data
        .split_once(";base64,")
        .map_or(upload.data.as_str(), |(_, rest)| rest);
    if data.len() > runbook_core::pictures::MAX_BYTES / 3 * 4 + 8 {
        return Err((422, "picture.too_large"));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .map_err(|_| (422, "request.invalid"))?;
    let (content_type, width, height) = inspect(&bytes).map_err(|error| (422, error.code()))?;
    Ok((
        Picture {
            content_type: content_type.into(),
            bytes,
            source_url: None,
            updated_at_ms: now,
        },
        PictureView {
            content_type: content_type.into(),
            width,
            height,
            updated_at_ms: now,
        },
    ))
}

async fn dispatch<B: Backends>(backends: &B, r: &Request) -> Result<Reply, Failure> {
    let credentials = backends.credentials();
    let directory = backends.directory();
    let record = r
        .credential_record(&credentials)
        .await
        .map_err(|e| e.status())?;
    if record.validate().is_err() || record.revoked_at.is_some() {
        return Err((401, "auth.invalid_credential"));
    }
    let now = directory.current_time_ms().await.map_err(|_| UNAVAILABLE)?;
    if record.expires_at.is_some_and(|at| at <= now) {
        return Err((401, "auth.invalid_credential"));
    }
    let snapshot = directory
        .snapshot(&record.org_id)
        .await
        .map_err(|_| UNAVAILABLE)?;
    // Anyone reading here is someone active in this org (or its service token).
    let me = match &record.principal {
        CredentialPrincipal::User { user_id, actor } => {
            if !snapshot.users.get(user_id).is_some_and(|u| u.active) {
                return Err((403, "auth.forbidden"));
            }
            Some((user_id.clone(), *actor))
        }
        CredentialPrincipal::Service { .. } => None,
    };

    if r.path == ME {
        // The person themself (session or own human token), not an agent.
        let Some((user, ActorKind::Human)) = me else {
            return Err((403, "auth.forbidden"));
        };
        if record.kind != TokenKind::Personal {
            return Err((403, "auth.forbidden"));
        }
        let owner = PictureOwner::User(user);
        return match r.method.as_str() {
            "PUT" => {
                let (picture, view) = decode(&r.body, now)?;
                directory
                    .set_picture(&owner, Some(picture))
                    .await
                    .map_err(|_| UNAVAILABLE)?;
                Ok(Reply::Json(200, json!(view)))
            }
            "DELETE" => {
                directory
                    .set_picture(&owner, None)
                    .await
                    .map_err(|_| UNAVAILABLE)?;
                Ok(Reply::Json(204, Value::Null))
            }
            _ => Err((405, "request.method_not_allowed")),
        };
    }

    if let Some(params) = crate::directory::match_path(USER, &r.path) {
        if r.method != "GET" {
            return Err((405, "request.method_not_allowed"));
        }
        let user: UserId = params
            .get("user")
            .and_then(|u| u.parse().ok())
            .ok_or(NOT_FOUND)?;
        if !snapshot.users.contains_key(&user) {
            return Err(NOT_FOUND);
        }
        return match directory
            .picture(&PictureOwner::User(user))
            .await
            .map_err(|_| UNAVAILABLE)?
        {
            Some(picture) if !picture.bytes.is_empty() => Ok(Reply::Image(picture)),
            Some(Picture {
                source_url: Some(url),
                ..
            }) => Ok(Reply::Redirect(url)),
            _ => Err((404, "picture.none")),
        };
    }

    let template = if crate::directory::match_path(PROJECT_PICTURE, &r.path).is_some() {
        PROJECT_PICTURE
    } else {
        PROJECT
    };
    let params = crate::directory::match_path(template, &r.path).ok_or(NOT_FOUND)?;
    let project_ref = params.get("p").ok_or(NOT_FOUND)?;
    let project: ProjectId = snapshot
        .projects
        .values()
        .find(|p| p.slug == *project_ref || p.id.as_str() == project_ref)
        .map(|p| p.id.clone())
        .ok_or(NOT_FOUND)?;
    let admin = || {
        authorize(&record, &snapshot, now, &project, Scope::ProjectAdmin)
            .map(|_| ())
            .map_err(|_| (403, "auth.forbidden"))
    };
    let owner = PictureOwner::Project(record.org_id.clone(), project.clone());
    match (template, r.method.as_str()) {
        (PROJECT_PICTURE, "GET") => {
            match directory.picture(&owner).await.map_err(|_| UNAVAILABLE)? {
                Some(picture) if !picture.bytes.is_empty() => Ok(Reply::Image(picture)),
                _ => Err((404, "picture.none")),
            }
        }
        (PROJECT_PICTURE, "PUT") => {
            admin()?;
            let (picture, view) = decode(&r.body, now)?;
            directory
                .set_picture(&owner, Some(picture))
                .await
                .map_err(|_| UNAVAILABLE)?;
            Ok(Reply::Json(200, json!(view)))
        }
        (PROJECT_PICTURE, "DELETE") => {
            admin()?;
            directory
                .set_picture(&owner, None)
                .await
                .map_err(|_| UNAVAILABLE)?;
            Ok(Reply::Json(204, Value::Null))
        }
        (PROJECT, "PATCH") => {
            let request: UpdateProjectRequest =
                serde_json::from_value(r.body.clone()).map_err(|_| (422, "request.invalid"))?;
            let mut mutations = vec![];
            // Archive or restore: org owners and admins (2026-11-18). This
            // comes first so a restore works on a read-only project.
            if let Some(archived) = request.archived {
                runbook_core::credentials::authorize_organization(
                    &record,
                    &snapshot,
                    now,
                    runbook_core::identity::OrgAction::CreateProject,
                )
                .map_err(|_| (403, "auth.forbidden"))?;
                mutations.push(DirectoryMutation::SetProjectArchived {
                    project_id: project.clone(),
                    archived,
                });
            }
            let mut title = snapshot.projects[&project].title.clone();
            if let Some(new_title) = &request.title {
                let new_title = new_title.trim().to_owned();
                if new_title.is_empty() || new_title.chars().count() > 100 {
                    return Err((422, "request.invalid"));
                }
                // Renaming an archived project waits for its restore,
                // unless this same request restores it.
                if request.archived != Some(false) {
                    admin()?;
                }
                title = new_title.clone();
                mutations.push(DirectoryMutation::SetProjectTitle {
                    project_id: project.clone(),
                    title: new_title,
                });
            }
            // R0-3: the project's default runner agent, kept beside the
            // directory (it changes only what later publishes resolve).
            if let Some(default_agent) = &request.default_agent {
                admin()?;
                use runbook_core::agent_directory::AgentDirectory;
                let store = backends.runners();
                if let Some(name) = default_agent
                    && name != runbook_model::EXTERNAL_AGENT
                    && !store
                        .project_agents(&record.org_id, &project)
                        .await
                        .map_err(|_| UNAVAILABLE)?
                        .iter()
                        .any(|a| {
                            &a.name == name && a.status != runbook_model::AgentStatus::Archived
                        })
                {
                    return Err((422, "spec.agent_unknown"));
                }
                store
                    .set_default_agent(&record.org_id, &project, default_agent.as_deref())
                    .await
                    .map_err(|e| e.status())?;
                if mutations.is_empty() {
                    let view = &snapshot.projects[&project];
                    let mut body = json!(ProjectView {
                        id: project.clone(),
                        org_id: record.org_id.clone(),
                        slug: view.slug.clone(),
                        title: view.title.clone(),
                        archived: snapshot.archived_projects.contains(&project),
                    });
                    body["default_agent"] = json!(default_agent);
                    return Ok(Reply::Json(200, body));
                }
            }
            if mutations.is_empty() {
                return Err((422, "request.invalid"));
            }
            let archived = request
                .archived
                .unwrap_or_else(|| snapshot.archived_projects.contains(&project));
            let mut revision = snapshot.revision;
            for _ in 0..3 {
                let committed = directory
                    .commit(DirectoryCommit {
                        org_id: record.org_id.clone(),
                        expected_revision: revision,
                        mutations: mutations.clone(),
                    })
                    .await
                    .map_err(|_| UNAVAILABLE)?;
                if matches!(committed, runbook_core::CommitResult::Committed(_)) {
                    let slug = snapshot.projects[&project].slug.clone();
                    return Ok(Reply::Json(
                        200,
                        json!(ProjectView {
                            id: project,
                            org_id: record.org_id.clone(),
                            slug,
                            title,
                            archived,
                        }),
                    ));
                }
                revision = directory
                    .snapshot(&record.org_id)
                    .await
                    .map_err(|_| UNAVAILABLE)?
                    .revision;
            }
            Err((409, "directory.conflict"))
        }
        _ => Err((405, "request.method_not_allowed")),
    }
}
