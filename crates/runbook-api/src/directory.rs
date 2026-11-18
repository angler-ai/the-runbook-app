use crate::{Request, discovery::DiscoveryConfig, problem::Response};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use runbook_core::{credentials::*, directory::*, directory_operations::*, identity::OrgAction};
use runbook_model::{
    CreateProjectRequest, CreateTeamRequest, CreateUserRequest, DIRECTORY_ROUTES, OrganizationId,
    ProjectId, ProjectMemberRequest, ProjectRole, ProjectVariable, ProjectVariables, PublicRoute,
    TeamMemberRequest, UpdateUserRequest,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::BTreeMap;
type Error = (u16, &'static str);
fn invalid() -> Error {
    (422, "request.invalid")
}
fn parse<T: DeserializeOwned>(v: &Value) -> Result<T, Error> {
    serde_json::from_value(v.clone()).map_err(|_| invalid())
}
fn id<T: std::str::FromStr>(s: &str) -> Result<T, Error> {
    s.parse().map_err(|_| (404, "resource.not_found"))
}
fn db(e: DirectoryError) -> Error {
    match e {
        DirectoryError::NotFound => (404, "resource.not_found"),
        DirectoryError::Duplicate => (409, "directory.duplicate"),
        DirectoryError::Invalid("last owner") => (409, "directory.last_owner"),
        DirectoryError::Invalid(_) => (422, "directory.invalid"),
        _ => (503, "storage.unavailable"),
    }
}
fn auth(e: AuthError) -> Error {
    match e {
        AuthError::Forbidden => (403, "auth.forbidden"),
        _ => (401, "auth.invalid_credential"),
    }
}
pub fn match_path(template: &str, path: &str) -> Option<BTreeMap<String, String>> {
    let a: Vec<_> = template.split('/').collect();
    let b: Vec<_> = path.split('/').collect();
    if a.len() != b.len() {
        return None;
    }
    let mut p = BTreeMap::new();
    for (a, b) in a.into_iter().zip(b) {
        if let Some(key) = a.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            let value = percent_encoding::percent_decode_str(b).decode_utf8().ok()?;
            if value.is_empty() {
                return None;
            }
            p.insert(key.to_owned(), value.into_owned());
        } else if a != b {
            return None;
        }
    }
    Some(p)
}
#[derive(Clone)]
pub struct Service<D, C> {
    pub directory: D,
    pub credentials: C,
    pub discovery: DiscoveryConfig,
}
impl<D: DirectoryOperations, C: CredentialStore> Service<D, C> {
    pub fn new(directory: D, credentials: C, discovery: DiscoveryConfig) -> Self {
        Self {
            directory,
            credentials,
            discovery,
        }
    }
    pub async fn handle(&self, r: Request) -> Response {
        match self.dispatch(&r).await {
            Ok(v) => {
                let mut response = Response::json(v.status, v.body, &r.request_id);
                response.etag = v.etag;
                response
            }
            Err((s, c)) => Response::error(s, c, &r.request_id),
        }
    }
    async fn dispatch(&self, r: &Request) -> Result<StoredResponse, Error> {
        let mut recognized = false;
        let mut matched = None;
        for route in DIRECTORY_ROUTES {
            if let Some(p) = match_path(route.path, &r.path) {
                recognized = true;
                if route.method == r.method {
                    matched = Some((route, p));
                    break;
                }
            }
        }
        let (route, p) = matched.ok_or(if recognized {
            (405, "request.method_not_allowed")
        } else {
            (404, "resource.not_found")
        })?;
        if route.path == "/.well-known/runbook" {
            return Ok(StoredResponse {
                status: 200,
                body: json!(self.discovery.discovery()),
                etag: None,
            });
        }
        if route.path == "/v1/meta" {
            return Ok(StoredResponse {
                status: 200,
                body: json!(self.discovery.meta()),
                etag: None,
            });
        }
        for _ in 0..4 {
            let record = r
                .credential_record(&self.credentials)
                .await
                .map_err(|e| e.status())?;
            let now = self.directory.current_time_ms().await.map_err(db)?;
            let org: OrganizationId = if let Some(org) = p.get("org") {
                id(org)?
            } else {
                record.org_id.clone()
            };
            if org != record.org_id {
                return Err((404, "resource.not_found"));
            }
            let s = self.directory.snapshot(&org).await.map_err(db)?;
            let project = p.get("p").map(|p| id::<ProjectId>(p)).transpose()?;
            let principal = if route.path.contains("/vars") {
                let project = project.as_ref().ok_or(invalid())?;
                if !s.projects.contains_key(project) {
                    return Err((404, "resource.not_found"));
                }
                authorize(
                    &record,
                    &s,
                    now,
                    project,
                    if r.method == "GET" {
                        Scope::RunbooksRead
                    } else {
                        Scope::ProjectAdmin
                    },
                )
                .map_err(auth)?
            } else if r.method == "GET" {
                let principal = authorize_directory_read(&record, &s, now).map_err(auth)?;
                if project
                    .as_ref()
                    .is_some_and(|p| !directory_project_visible(&record, &s, p))
                {
                    return Err((404, "resource.not_found"));
                }
                principal
            } else {
                if project
                    .as_ref()
                    .is_some_and(|p| !s.projects.contains_key(p))
                {
                    return Err((404, "resource.not_found"));
                }
                authorize_organization(&record, &s, now, OrgAction::ManageMemberships)
                    .map_err(auth)?
            };
            // F47: the D17/R38 rank rule for users. An admin may change
            // members and admins, never an owner. Checked before replay.
            if route.path == "/v1/orgs/{org}/users/{user}" && r.method == "PATCH" {
                let rank = |u: Option<&UserRecord>| match u.map(|u| u.role) {
                    Some(OrgRole::Owner) => 2,
                    Some(OrgRole::Admin) => 1,
                    _ => 0,
                };
                let caller = match &record.principal {
                    CredentialPrincipal::User { user_id, .. } => s.users.get(user_id),
                    CredentialPrincipal::Service { .. } => None,
                };
                let target = p
                    .get("user")
                    .and_then(|u| u.parse().ok())
                    .and_then(|u| s.users.get(&u));
                if target.is_some() && rank(target) > rank(caller) {
                    return Err((403, "auth.forbidden"));
                }
            }
            self.credentials
                .touch_last_used(&org, &record.id, now)
                .await
                .map_err(|_| (503, "storage.unavailable"))?;
            let operation = format!("{}:{}:{}", r.method, route.path, json!(p));
            let principal = match principal {
                Principal::Human { user_id } => format!("human:{user_id}"),
                Principal::Agent { sponsor } => format!("agent:{sponsor}"),
                Principal::Service { token_id, .. } => format!("service:{token_id}"),
            };
            let replay = if r.method == "POST" {
                let key = r
                    .idempotency_key
                    .as_deref()
                    .filter(|k| !k.trim().is_empty() && k.len() <= 256)
                    .ok_or((428, "request.idempotency_key_required"))?;
                Some(RequestReplay {
                    principal: principal.clone(),
                    operation: operation.clone(),
                    key: key.into(),
                    payload: r.body.clone(),
                })
            } else {
                None
            };
            if let Some(replay) = &replay {
                match self
                    .directory
                    .lookup_replay(&org, s.revision, replay)
                    .await
                    .map_err(db)?
                {
                    ReplayResult::Applied(response) => return Ok(response),
                    ReplayResult::KeyReused => return Err((409, "request.idempotency_key_reused")),
                    ReplayResult::Conflict => continue,
                    ReplayResult::Missing => {}
                }
            }
            let mut mutations = vec![];
            let mut variables = None;
            let mut etag = None;
            let mut body = self
                .prepare(
                    r,
                    route,
                    &p,
                    &s,
                    project.as_ref(),
                    &mut mutations,
                    &mut variables,
                    &mut etag,
                )
                .await?;
            if route.path == "/v1/orgs/{org}/projects" && r.method == "GET" {
                body = json!(
                    s.projects
                        .values()
                        .filter(|p| directory_project_visible(&record, &s, &p.id))
                        .collect::<Vec<_>>()
                );
            }
            if route.path == "/v1/orgs/{org}/projects"
                && r.method == "POST"
                && let CredentialPrincipal::User { user_id, .. } = &record.principal
            {
                mutations.push(DirectoryMutation::GrantProjectRole {
                    project_id: id(&format!("prj_{}", r.new_id))?,
                    user_id: user_id.clone(),
                    role: ProjectRole::Admin,
                });
            }
            if r.method == "GET" {
                let body = if body.is_array() {
                    page(body, r, &operation, &principal, s.revision)?
                } else {
                    body
                };
                return Ok(StoredResponse {
                    status: route.status,
                    body,
                    etag,
                });
            }
            let result = self
                .directory
                .apply_operation(DirectoryOperation {
                    commit: DirectoryCommit {
                        org_id: org,
                        expected_revision: s.revision,
                        mutations,
                    },
                    variables,
                    replay,
                    response: StoredResponse {
                        status: route.status,
                        body,
                        etag,
                    },
                })
                .await
                .map_err(db)?;
            match result {
                OperationResult::Applied(v) => return Ok(v),
                OperationResult::KeyReused => return Err((409, "request.idempotency_key_reused")),
                OperationResult::VariablesConflict => return Err((412, "vars.revision_mismatch")),
                OperationResult::Conflict => continue,
            }
        }
        Err((409, "directory.concurrent_change"))
    }
    #[allow(clippy::too_many_arguments)]
    async fn prepare(
        &self,
        r: &Request,
        route: &PublicRoute,
        p: &BTreeMap<String, String>,
        s: &DirectorySnapshot,
        project: Option<&ProjectId>,
        m: &mut Vec<DirectoryMutation>,
        vars: &mut Option<VariablesWrite>,
        etag: &mut Option<String>,
    ) -> Result<Value, Error> {
        let new_id = |prefix: &str| format!("{prefix}{}", r.new_id);
        match (route.path, r.method.as_str()) {
            ("/v1/orgs", "GET") => Ok(json!([s.organization])),
            ("/v1/orgs/{org}/projects", "GET") => {
                Ok(json!(s.projects.values().collect::<Vec<_>>()))
            }
            ("/v1/orgs/{org}/users", "GET") => Ok(json!(s.users.values().collect::<Vec<_>>())),
            ("/v1/orgs/{org}/teams", "GET") => Ok(json!(s.teams.values().collect::<Vec<_>>())),
            ("/v1/orgs/{org}/projects", "POST") => {
                let b: CreateProjectRequest = parse(&r.body)?;
                crate::field_caps::text("/title", &b.title, crate::field_caps::NAME_BYTES)?;
                let v = ProjectRecord {
                    id: id(&new_id("prj_"))?,
                    org_id: s.org_id.clone(),
                    slug: b.slug,
                    title: b.title,
                };
                m.push(DirectoryMutation::CreateProject(v.clone()));
                Ok(json!(v))
            }
            ("/v1/orgs/{org}/users", "POST") => {
                let b: CreateUserRequest = parse(&r.body)?;
                crate::field_caps::text("/name", &b.name, crate::field_caps::NAME_BYTES)?;
                let v = UserRecord {
                    id: id(&new_id("usr_"))?,
                    name: b.name,
                    active: true,
                    role: OrgRole::Member,
                };
                m.push(DirectoryMutation::CreateUser(v.clone()));
                Ok(json!(v))
            }
            ("/v1/orgs/{org}/teams", "POST") => {
                let b: CreateTeamRequest = parse(&r.body)?;
                crate::field_caps::text("/title", &b.title, crate::field_caps::NAME_BYTES)?;
                let v = TeamRecord {
                    id: id(&new_id("team_"))?,
                    org_id: s.org_id.clone(),
                    slug: b.slug,
                    title: b.title,
                };
                m.push(DirectoryMutation::CreateTeam(v.clone()));
                Ok(json!(v))
            }
            ("/v1/orgs/{org}/users/{user}", "PATCH") => {
                let b: UpdateUserRequest = parse(&r.body)?;
                let user = id(&p["user"])?;
                let mut v = s
                    .users
                    .get(&user)
                    .cloned()
                    .ok_or((404, "resource.not_found"))?;
                v.active = b.active;
                m.push(DirectoryMutation::SetUserActive {
                    user_id: user,
                    active: b.active,
                });
                Ok(json!(v))
            }
            ("/v1/orgs/{org}/teams/{team}/members", "GET") => {
                let team = id(&p["team"])?;
                if !s.teams.contains_key(&team) {
                    return Err((404, "resource.not_found"));
                }
                Ok(json!(
                    s.team_members
                        .iter()
                        .filter(|(t, _)| *t == team)
                        .map(|(_, u)| json!({"user_id":u}))
                        .collect::<Vec<_>>()
                ))
            }
            ("/v1/orgs/{org}/teams/{team}/members", "POST") => {
                let b: TeamMemberRequest = parse(&r.body)?;
                m.push(DirectoryMutation::AddTeamMember {
                    team_id: id(&p["team"])?,
                    user_id: b.user_id.clone(),
                });
                Ok(json!(b))
            }
            ("/v1/orgs/{org}/teams/{team}/members/{user}", "DELETE") => {
                m.push(DirectoryMutation::RemoveTeamMember {
                    team_id: id(&p["team"])?,
                    user_id: id(&p["user"])?,
                });
                Ok(Value::Null)
            }
            ("/v1/projects/{p}/members", "GET") => {
                let project = project.ok_or(invalid())?;
                Ok(json!(
                    s.project_members
                        .iter()
                        .filter(|((pid, _), _)| pid == project)
                        .map(|((_, u), role)| json!({"user_id":u,"role":role}))
                        .collect::<Vec<_>>()
                ))
            }
            ("/v1/projects/{p}/members", "POST") => {
                let b: ProjectMemberRequest = parse(&r.body)?;
                m.push(DirectoryMutation::GrantProjectRole {
                    project_id: project.ok_or(invalid())?.clone(),
                    user_id: b.user_id.clone(),
                    role: b.role,
                });
                Ok(json!(b))
            }
            ("/v1/projects/{p}/members/{user}", "DELETE") => {
                m.push(DirectoryMutation::RevokeProjectRole {
                    project_id: project.ok_or(invalid())?.clone(),
                    user_id: id(&p["user"])?,
                });
                Ok(Value::Null)
            }
            (path, _) if path.contains("/vars") => {
                let project = project.ok_or(invalid())?;
                let stored = self
                    .directory
                    .project_variables(&s.org_id, project)
                    .await
                    .map_err(db)?;
                *etag = Some(format!("\"vars-{}\"", stored.revision));
                if r.method != "GET" {
                    let supplied = r
                        .if_match
                        .as_deref()
                        .ok_or((428, "request.if_match_required"))?;
                    if Some(supplied) != etag.as_deref() {
                        return Err((412, "vars.revision_mismatch"));
                    }
                    *etag = Some(format!(
                        "\"vars-{}\"",
                        stored.revision.checked_add(1).ok_or(invalid())?
                    ));
                }
                let mut v = stored.variables;
                let name = p.get("name");
                let result = match (r.method.as_str(), name) {
                    ("GET", None) => return Ok(json!(v)),
                    ("GET", Some(n)) => {
                        return Ok(json!(ProjectVariable {
                            declaration: v
                                .declarations
                                .get(n)
                                .cloned()
                                .ok_or((404, "resource.not_found"))?,
                            value: v
                                .values
                                .get(n)
                                .cloned()
                                .ok_or((404, "resource.not_found"))?
                        }));
                    }
                    ("PUT", None) => {
                        v = parse(&r.body)?;
                        json!(v)
                    }
                    ("PUT", Some(n)) => {
                        let b: ProjectVariable = parse(&r.body)?;
                        v.declarations.insert(n.clone(), b.declaration.clone());
                        v.values.insert(n.clone(), b.value.clone());
                        json!(b)
                    }
                    ("DELETE", None) => {
                        v = ProjectVariables::default();
                        Value::Null
                    }
                    ("DELETE", Some(n)) => {
                        v.declarations.remove(n);
                        v.values.remove(n);
                        Value::Null
                    }
                    _ => return Err(invalid()),
                };
                let schema = runbook_spec::parse_declarations(&json!(v.declarations))
                    .map_err(|_| (422, "project.vars_invalid"))?;
                if serde_json::to_vec(&v).map_err(|_| invalid())?.len() > 1_048_576
                    || !runbook_spec::validate_data(&schema, &json!(v.values)).is_empty()
                {
                    return Err((422, "project.vars_invalid"));
                }
                if crate::variables::secret_like(&json!(v.values))
                    || crate::variables::secret_default(&json!(v.declarations))
                {
                    return Err((422, "vars.secret_like"));
                }
                *vars = Some(VariablesWrite {
                    project_id: project.clone(),
                    expected_revision: stored.revision,
                    variables: v,
                });
                Ok(result)
            }
            _ => Err((404, "resource.not_found")),
        }
    }
}
fn page(
    body: Value,
    r: &Request,
    op: &str,
    principal: &str,
    revision: u64,
) -> Result<Value, Error> {
    let limit = r
        .query
        .get("limit")
        .map(|s| s.parse::<usize>().map_err(|_| invalid()))
        .transpose()?
        .unwrap_or(100);
    if !(1..=100).contains(&limit) {
        return Err(invalid());
    }
    let mut after = String::new();
    if let Some(cursor) = r.query.get("cursor") {
        let bytes = URL_SAFE_NO_PAD
            .decode(cursor)
            .map_err(|_| (400, "cursor.invalid"))?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| (400, "cursor.invalid"))?;
        if v["operation"] != op || v["principal"] != principal {
            return Err((400, "cursor.invalid"));
        }
        if v["revision"] != revision {
            return Err((409, "cursor.stale"));
        }
        after = v["after"].as_str().ok_or((400, "cursor.invalid"))?.into();
    }
    let key = |v: &Value| {
        v.get("id")
            .or_else(|| v.get("user_id"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut items = body
        .as_array()
        .ok_or(invalid())?
        .iter()
        .filter(|v| key(v) > after)
        .cloned()
        .collect::<Vec<_>>();
    items.sort_by_key(&key);
    let more = items.len() > limit;
    items.truncate(limit);
    let cursor = if more {
        Some(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"operation":op,"principal":principal,"revision":revision,"after":key(items.last().ok_or(invalid())?)})).map_err(|_|invalid())?))
    } else {
        None
    };
    Ok(json!({"items":items,"next_cursor":cursor}))
}
