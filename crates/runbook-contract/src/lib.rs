//! Public contract. Each operation explicitly declares implemented or planned status.
mod actors;
mod archives;
mod auth;
mod authoring;
mod credentials;
mod evidence;
mod executions;
mod memory;
mod mfa;
mod post_pilot;
mod runners;
mod tokens;
mod webhooks;
use runbook_model::*;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

/// Generate OpenAPI 3.1 JSON Schema components directly from the shared wire types.
pub fn openapi() -> Value {
    let settings = SchemaSettings::draft2020_12().with(|settings| {
        settings.definitions_path = "#/components/schemas".into();
    });
    let mut requests = settings.clone().into_generator();
    macro_rules! register { ($generator:expr; $($ty:ty),+ $(,)?) => {
        $(let _ = $generator.subschema_for::<$ty>();)+
    }; }
    register!(requests; ExecutionTokenPair, ClaimedStepRead, ClaimedDecisionRead, SkillFileRead);
    register!(requests; StartRequest, RerunRequest, ReportRequest, ClaimRequest,
        HeartbeatRequest, ReleaseRequest, LogsRequest, ChecklistRequest, DecisionRequest, SignalRequest,
        ExecutionPatchRequest, RefreshRequest, MintTokenRequest, OverrideRequest,
        CreateProjectRequest, CreateUserRequest, UpdateUserRequest, CreateTeamRequest,
        ProjectMemberRequest, TeamMemberRequest, CreateRunbookRequest, SaveDraftRequest,
        ValidateRequest, DraftView, PublishedVersionView, VersionView, ValidateResponse);
    register!(requests; RunSearchPage, StepView, ExecutionView, ApprovalView, ResumeBrief, Problem, ClaimRef, DecisionRecord);
    register!(requests; MemoryEntryView, SaveMemoryRequest, MemoryReadRef, MemoryPatchRequest, MemoryPage, MemoryRevisionPage, ExecutionDocumentView);
    register!(requests; InboxPage);
    register!(requests; TriggerResponse);
    register!(requests; MeView, VersionFormView, ConnectedAppPage, ConnectedAppView, EmailPreferencesView, EmailPreferencesRequest, UnsubscribeRequest, UnsubscribeView, ProjectSlackView, ProjectSlackRequest, UniversalSearchView, UpdateMeRequest, MeUser, PictureUpload, PictureView, UpdateProjectRequest, SyncRequest, SyncFile, SyncView, SyncRunbookView, SyncFileErrors, DetachView);
    register!(requests; StepLogPage);
    register!(requests; Jwks, RunnerBootstrapRequest, RunnerBootstrapView, RunnerRegisterRequest,
        RunnerRegisteredView, RunnerJoinRequest, RunnerJoinedView, RunnerHeartbeatRequest,
        RunnerStatusView, RunnerGroupRegistryView, RunnerGroupProjectsRequest, RunnerVaultIdentityRequest, AgentDirectoryEntry, RunnerWaitingRequest,
        RunnerEventRequest, RunnerDecideRequest, RunnerAbstainRequest, ApprovalAgentNote, ReadyStepPage, RunnerMemoryProposal, RunnerSkillPin, GrantRequest, GrantView, GrantRevokedRequest);
    register!(requests; CreateTokenRequest, TokenView, TokenSecretView, TokenPage);
    register!(requests; CreateWebhookRequest, UpdateWebhookRequest, WebhookView, WebhookSecretView,
        WebhookPage, DeliveryView, DeliveryPage, UndeliveredNotification);
    register!(requests; DiscoveryView, MetaView, ApiLimits, OrganizationView, DirectoryUserView, ProjectView, TeamView, ProjectVariables, ProjectVariable,
        DirectoryPage<OrganizationView>, DirectoryPage<DirectoryUserView>, DirectoryPage<ProjectView>, DirectoryPage<TeamView>, DirectoryPage<ProjectMemberRequest>, DirectoryPage<TeamMemberRequest>);
    register!(requests; DraftWriteView, RunbookCatalogView, VersionSourceView, VersionDiffView, SkillUploadRequest, SkillVersionView, DirectoryPage<RunbookCatalogView>, DirectoryPage<VersionView>, DirectoryPage<SkillVersionView>);
    register!(requests; AuthDiscovery, UploadLimits, DeviceCodeRequest, DeviceCodeView,
        DeviceTokenRequest, DeviceTokenView, DeviceApproveRequest, DeviceLookupRequest,
        DeviceLookupView, CreateInvitationRequest,
        InvitationView, DirectoryPage<InvitationView>, CreateEvidenceRequest, EvidenceSlotView,
        EvidenceView, EvidencePage, ArchiveView, ArchivePage, ArchivedRunView, ArchivedFileView,
        ArchivedEvidenceView);
    let schemas = requests.definitions().clone();

    let mut paths = Map::new();
    for (path, method, request, response) in [
        ("/v1/runbooks/{rb}/memory", "get", None, Some("MemoryPage")),
        (
            "/v1/runbooks/{rb}/memory",
            "post",
            Some("SaveMemoryRequest"),
            Some("MemoryEntryView"),
        ),
        ("/v1/projects/{p}/memory", "get", None, Some("MemoryPage")),
        (
            "/v1/projects/{p}/memory",
            "post",
            Some("SaveMemoryRequest"),
            Some("MemoryEntryView"),
        ),
        ("/v1/memory/{mem}", "get", None, Some("MemoryEntryView")),
        (
            "/v1/memory/{mem}",
            "patch",
            Some("MemoryPatchRequest"),
            Some("MemoryEntryView"),
        ),
        (
            "/v1/memory/{mem}/revisions",
            "get",
            None,
            Some("MemoryRevisionPage"),
        ),
        (
            "/v1/projects/{p}/runbooks",
            "post",
            Some("CreateRunbookRequest"),
            Some("DraftView"),
        ),
        ("/v1/runbooks/{rb}/draft", "get", None, Some("DraftView")),
        (
            "/v1/runbooks/{rb}/draft",
            "patch",
            Some("SaveDraftRequest"),
            Some("DraftView"),
        ),
        (
            "/v1/runbooks/{rb}/versions",
            "post",
            None,
            Some("PublishedVersionView"),
        ),
        (
            "/v1/validate",
            "post",
            Some("ValidateRequest"),
            Some("ValidateResponse"),
        ),
        ("/v1/me/tasks", "get", None, Some("InboxPage")),
        (
            "/v1/orgs/{org}/search",
            "get",
            None,
            Some("UniversalSearchView"),
        ),
        (
            "/v1/projects/{p}/slack",
            "get",
            None,
            Some("ProjectSlackView"),
        ),
        (
            "/v1/projects/{p}/slack",
            "put",
            Some("ProjectSlackRequest"),
            Some("ProjectSlackView"),
        ),
        ("/v1/projects/{p}/slack", "delete", None, None),
        ("/v1/slack/interactions", "post", None, None),
        (
            "/v1/me/picture",
            "put",
            Some("PictureUpload"),
            Some("PictureView"),
        ),
        ("/v1/me/picture", "delete", None, None),
        ("/v1/users/{user}/picture", "get", None, None),
        (
            "/v1/projects/{p}",
            "patch",
            Some("UpdateProjectRequest"),
            Some("ProjectView"),
        ),
        ("/v1/projects/{p}/picture", "get", None, None),
        (
            "/v1/projects/{p}/picture",
            "put",
            Some("PictureUpload"),
            Some("PictureView"),
        ),
        ("/v1/projects/{p}/picture", "delete", None, None),
        (
            "/v1/projects/{p}/sync",
            "post",
            Some("SyncRequest"),
            Some("SyncView"),
        ),
        ("/v1/runbooks/{rb}/detach", "post", None, Some("DetachView")),
        ("/v1/me", "get", None, Some("MeView")),
        ("/v1/me", "patch", Some("UpdateMeRequest"), Some("MeUser")),
        ("/v1/me/token", "delete", None, None),
        ("/v1/me/apps", "get", None, Some("ConnectedAppPage")),
        ("/v1/me/apps/{client}", "delete", None, None),
        (
            "/v1/me/email-preferences",
            "get",
            None,
            Some("EmailPreferencesView"),
        ),
        (
            "/v1/me/email-preferences",
            "put",
            Some("EmailPreferencesRequest"),
            Some("EmailPreferencesView"),
        ),
        (
            "/v1/unsubscribe",
            "post",
            Some("UnsubscribeRequest"),
            Some("UnsubscribeView"),
        ),
        ("/v1/meta", "get", None, None),
        ("/v1/orgs", "get", None, None),
        ("/v1/orgs/{org}/projects", "get", None, None),
        (
            "/v1/orgs/{org}/projects",
            "post",
            Some("CreateProjectRequest"),
            None,
        ),
        ("/v1/orgs/{org}/users", "get", None, None),
        (
            "/v1/orgs/{org}/users",
            "post",
            Some("CreateUserRequest"),
            None,
        ),
        (
            "/v1/orgs/{org}/users/{user}",
            "patch",
            Some("UpdateUserRequest"),
            None,
        ),
        ("/v1/orgs/{org}/teams", "get", None, None),
        (
            "/v1/orgs/{org}/teams",
            "post",
            Some("CreateTeamRequest"),
            None,
        ),
        ("/v1/orgs/{org}/teams/{team}/members", "get", None, None),
        (
            "/v1/orgs/{org}/teams/{team}/members",
            "post",
            Some("TeamMemberRequest"),
            None,
        ),
        (
            "/v1/orgs/{org}/teams/{team}/members/{user}",
            "delete",
            None,
            None,
        ),
        ("/v1/projects/{p}/members", "get", None, None),
        (
            "/v1/projects/{p}/members",
            "post",
            Some("ProjectMemberRequest"),
            None,
        ),
        ("/v1/projects/{p}/members/{user}", "delete", None, None),
        ("/v1/projects/{p}/executions", "get", None, None),
        (
            "/v1/projects/{p}/executions",
            "post",
            Some("StartRequest"),
            Some("ExecutionView"),
        ),
        (
            runbook_model::TRIGGER_PATH,
            "post",
            None,
            Some("TriggerResponse"),
        ),
        ("/v1/executions/{exe}", "get", None, Some("ExecutionView")),
        (
            "/v1/executions/{exe}",
            "patch",
            Some("ExecutionPatchRequest"),
            Some("ExecutionView"),
        ),
        (
            "/v1/executions/{exe}/rerun",
            "post",
            Some("RerunRequest"),
            Some("ExecutionView"),
        ),
        (
            "/v1/executions/{exe}/tokens",
            "post",
            Some("MintTokenRequest"),
            None,
        ),
        (
            "/v1/executions/{exe}/tokens/refresh",
            "post",
            Some("RefreshRequest"),
            None,
        ),
        (
            "/v1/executions/{exe}/signals/{name}",
            "post",
            Some("SignalRequest"),
            None,
        ),
        ("/v1/executions/{exe}/next", "get", None, None),
        (
            "/v1/executions/{exe}/report",
            "get",
            None,
            Some("ExecutionDocumentView"),
        ),
        ("/v1/executions/{exe}/events", "get", None, None),
        (
            "/v1/step-runs/{sr}/claim",
            "post",
            Some("ClaimRequest"),
            None,
        ),
        (
            "/v1/step-runs/{sr}/heartbeat",
            "post",
            Some("HeartbeatRequest"),
            None,
        ),
        (
            "/v1/step-runs/{sr}/report",
            "post",
            Some("ReportRequest"),
            None,
        ),
        (
            "/v1/step-runs/{sr}/release",
            "post",
            Some("ReleaseRequest"),
            None,
        ),
        ("/v1/step-runs/{sr}/logs", "post", Some("LogsRequest"), None),
        ("/v1/step-runs/{sr}/logs", "get", None, Some("StepLogPage")),
        (
            "/v1/step-runs/{sr}/checklist",
            "post",
            Some("ChecklistRequest"),
            None,
        ),
        (
            "/v1/step-runs/{sr}/decisions",
            "post",
            Some("DecisionRequest"),
            Some("ApprovalView"),
        ),
        (
            "/v1/step-runs/{sr}/overrides",
            "post",
            Some("OverrideRequest"),
            Some("StepView"),
        ),
        (
            "/v1/step-runs/{sr}/evidence",
            "post",
            Some("CreateEvidenceRequest"),
            Some("EvidenceSlotView"),
        ),
        (
            "/v1/step-runs/{sr}/evidence/{evd}/finalize",
            "post",
            None,
            Some("EvidenceView"),
        ),
        (
            "/v1/executions/{exe}/evidence",
            "get",
            None,
            Some("EvidencePage"),
        ),
    ] {
        let mut parameters: Vec<Value> = path
            .split('/')
            .filter_map(|part| {
                part.strip_prefix('{').and_then(|name| name.strip_suffix('}')).map(|name| {
                json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}})
            })
            })
            .collect();
        if method == "post" {
            parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1}}));
        }
        let mut operation = json!({
            "operationId":format!("{}_{}",method,path.trim_start_matches('/').replace(['/', '{', '}'], "_")),
            "summary":"Planned pilot operation",
            "description":"Draft contract only. This operation has no server implementation in Gate A.",
            "x-implementation":"planned",
            "parameters":parameters,
            "security":[{"bearerAuth":[]}],
            "responses":{
                "default":{"description":"Response details will be finalized with the durable API implementation."},
                "422":{"description":"Invalid request, including incompatible rerun inputs.","content":{
                    "application/problem+json":{"schema":{"$ref":"#/components/schemas/Problem"}}
                }}
            }
        });
        if let Some(request) = request {
            operation["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        if let Some(response) = response {
            let status = if method == "post"
                && matches!(
                    response,
                    "ExecutionView"
                        | "DraftView"
                        | "PublishedVersionView"
                        | "MemoryEntryView"
                        | "EvidenceSlotView"
                ) {
                "201"
            } else {
                "200"
            };
            operation["responses"][status] = json!({"description":"Planned typed response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}});
        }
        let authoring = match (path, method) {
            ("/v1/projects/{p}/runbooks", "post") => Some((
                "runbooks:write",
                "Create a runbook with its first draft; no version is published.",
            )),
            ("/v1/runbooks/{rb}/draft", "get") => Some((
                "runbooks:read",
                "Read full draft source YAML and its positive revision for editing.",
            )),
            ("/v1/runbooks/{rb}/draft", "patch") => Some((
                "runbooks:write",
                "Save source YAML verbatim without publishing; revision changes on a successful save.",
            )),
            ("/v1/runbooks/{rb}/versions", "post") => Some((
                "runbooks:publish",
                "Publish the draft revision selected by If-Match, never a concurrent unseen edit. Assign the next positive published number and an rbv_ ID; identical latest content returns the existing version. An agent denied publication may receive next_action: ask_human_to_publish.",
            )),
            ("/v1/validate", "post") => Some((
                "runbooks:write",
                "Compile source YAML without saving; return diagnostics and a step budget (null when unavailable). With project, references resolve against that project and the caller needs runbooks:read on it; without it, skill, team, user and variable references are diagnostics.",
            )),
            _ => None,
        };
        if let Some((scope, description)) = authoring {
            operation["summary"] = json!(description);
            operation["description"] = json!(format!(
                "Draft contract only; no server implementation in Gate A. {description} Authoring uses an appropriately scoped PAT, service token or OAuth credential. Execution tokens never carry runbooks:* scopes and cannot read full draft or published source. Project editor/admin permissions include write and publish; token scopes may narrow them."
            ));
            operation["x-required-scopes"] = json!([scope]);
            operation["responses"]["403"] = problem_response(
                "Required scope missing; no mutation.",
                &["auth.scope_missing"],
            );
            if path == "/v1/runbooks/{rb}/draft" {
                operation["responses"]["200"]["headers"]["ETag"] = json!({
                    "description":"Strong opaque ETag identifying this draft revision; preserve it verbatim for If-Match when saving or publishing.",
                    "schema":{"type":"string"}
                });
            }
            if path == "/v1/projects/{p}/runbooks" {
                operation["responses"]["201"]["headers"]["ETag"] = json!({
                    "description":"Strong opaque ETag identifying the first draft revision.","schema":{"type":"string"}
                });
            }
            if method == "patch" || path == "/v1/runbooks/{rb}/versions" {
                operation["parameters"].as_array_mut().unwrap().push(json!({
                    "name":"If-Match", "in":"header", "required":true,
                    "description":"The strong ETag selecting the expected draft revision returned by draft read/create/save. Supply that exact revision tag, not a wildcard. Check atomically before saving or publishing.",
                    "schema":{"type":"string","pattern":"^\"[^\"]+\"$"}
                }));
                operation["responses"]["412"] = problem_response(
                    "Stale or nonmatching draft revision precondition. No mutation; read the draft and review changes before retrying.",
                    &[],
                );
                operation["responses"]["428"] = problem_response(
                    "Missing draft revision precondition. No mutation; obtain the draft ETag first.",
                    &[],
                );
                operation["responses"]["409"] = problem_response(
                    "Git-managed runbooks reject API draft writes/publication; change source through the repository pull request flow. No mutation.",
                    &["runbook.git_managed"],
                );
            }
        }
        if path.ends_with("/memory") || path.starts_with("/v1/memory/{mem}") {
            memory::describe(path, method, &mut operation);
        }
        if path.contains("/evidence") {
            evidence::describe(path, &mut operation);
        }
        if path == "/v1/me/tasks" {
            operation["summary"] =
                json!("List my open human tasks and approvals across accessible projects");
            operation["description"] = json!(
                "Implemented on Postgres and Cloudflare (B8f). Query parameters: kind, limit and cursor. Newest assignment first, then execution and step run ID. Requires an active human directory user. Users with no project grants receive an empty page. Reading tasks for accessible projects requires executions:read. Service and execution credentials cannot use this inbox. Return open human steps (including checklists and handoffs) and approvals assigned to that user or their directory teams, limited to the intersection of directory project access and any credential project restrictions. Team membership alone grants no project access. Return each step run once even when several assignments match; unrelated users receive an empty page. Completion or a completed approval decision removes the item. This summary grants no claim or voting authority; mutations recheck eligibility. Rows derive only from execution outbox entries applied idempotently by execution and sequence, ignoring older sequences. Postgres updates them in the same transaction; Cloudflare permits the adapter's documented bounded derived-row delay."
            );
            operation["x-required-scopes"] = json!(["executions:read"]);
            operation["x-required-principal"] = json!("human");
            operation["x-implementation"] = json!("implemented");
            operation["x-project-access"] = json!("directory_and_credential_intersection");
            // U28: the query me_tasks.rs accepts; any other name is 400.
            operation["parameters"].as_array_mut().unwrap().extend([
                json!({
                    "name":"kind", "in":"query",
                    "description":"Comma-separated item kinds: approval, human_task (open human steps, checklists and handoffs). Omitted lists both. Any other value is 400 request.invalid_query.",
                    "schema":{"type":"string","pattern":"^(approval|human_task)(,(approval|human_task))*$"}
                }),
                json!({
                    "name":"limit", "in":"query",
                    "description":"Page size. Outside 1..100 is 400 request.invalid_query.",
                    "schema":{"type":"integer","minimum":1,"maximum":100,"default":50}
                }),
                json!({
                    "name":"cursor", "in":"query",
                    "description":"Opaque cursor for the caller's authorized inbox, bound to the caller, organization and kind filter; a mismatch is 400 request.invalid_cursor.",
                    "schema":{"type":"string"}
                }),
            ]);
            operation["responses"]["403"] = problem_response(
                "Credential lacks executions:read or does not represent an active human user.",
                &[],
            );
        }
        if path == "/v1/me" {
            operation["summary"] = json!("Who the presenting credential is (experimental)");
            operation["description"] = json!(
                "Implemented on Postgres (B1, experimental). Any live personal or service credential reads its own identity: user id/name (absent for service tokens), organization id/title/role, team id/slug memberships, and the credential kind, actor kind, scopes, project restriction and expiry. Identity derives only from the stored credential record and directory snapshot. Revoked, expired, execution and refresh credentials return 401. Scopes are reported, not required, on this route."
            );
            operation["x-implementation"] = json!("implemented");
            operation["responses"]["401"] = problem_response(
                "Missing, revoked, expired or non-directory credential.",
                &["auth.required", "auth.invalid_credential"],
            );
            operation["responses"]["403"] = problem_response(
                "Credential's directory user is inactive.",
                &["auth.forbidden"],
            );
        }
        if path == "/v1/orgs/{org}/search" {
            operation["summary"] = json!("Search everything the caller can read (SEARCH2)");
            operation["description"] = json!(
                "Implemented. One call across runbooks, runs, open steps, memory and skills in every project the credential can read (each project authorized on its own; at most 50 projects scanned, 8 hits per kind). Steps are open assignments. q is whole-word text for runs and memory, substring for the rest."
            );
            operation["x-implementation"] = json!("implemented");
            operation["x-required-scopes"] = json!(["runbooks:read"]);
            operation["parameters"].as_array_mut().unwrap().push(json!({
                "name": "q", "in": "query", "required": false,
                "description": "The search text; empty answers no hits.",
                "schema": {"type": "string"}
            }));
        }
        if path == "/v1/me" && method == "patch" {
            operation["summary"] = json!("Change your own name");
            operation["description"] = json!(
                "Implemented. The signed-in person (a browser session or their own human token) sets their display name, 1 to 100 characters, shown in every organization they belong to."
            );
            operation["x-implementation"] = json!("implemented");
        }
        if path == "/v1/me/picture" || path == "/v1/projects/{p}/picture" {
            operation["summary"] = json!(if path == "/v1/me/picture" {
                "Your profile photo"
            } else {
                "A project's image"
            });
            operation["description"] = json!(
                "Implemented. A PNG or JPEG of at most 512x512 pixels and 1 MiB, sent base64-encoded and kept in the database. Reading a project's image needs only membership of its organization; setting or removing it needs project admin. A larger or other image is 422 picture.too_large or picture.unsupported_format."
            );
            operation["x-implementation"] = json!("implemented");
        }
        if path == "/v1/slack/interactions" {
            operation["summary"] = json!("Slack button clicks (the Slack app)");
            operation["description"] = json!(
                "Implemented. Slack posts here (application/x-www-form-urlencoded, signed with X-Slack-Signature and X-Slack-Request-Timestamp) when someone clicks an approval button. The signature over the exact body, made with RUNBOOK_SLACK_SIGNING_SECRET, is the credential; older than five minutes is 401 slack.invalid_signature. The clicking user is matched by email and votes with their own permissions; the answer to them goes to the interaction's response_url. 404 when the server has no Slack app. Not for API clients."
            );
            operation["security"] = json!([]);
            operation["parameters"] = json!([]);
            operation["x-implementation"] = json!("implemented");
        }
        if path == "/v1/users/{user}/picture" {
            operation["summary"] = json!("A person's profile photo");
            operation["description"] = json!(
                "Implemented. The image itself, or a redirect to the person's Google photo when they signed in with Google and uploaded none; 404 picture.none without either. Any member of an organization the person belongs to may read it."
            );
            operation["x-implementation"] = json!("implemented");
        }
        if path == "/v1/projects/{p}" {
            operation["summary"] = json!("Change a project's settings");
            operation["description"] = json!(
                "Implemented. Project admins (and organization owners and admins) set the project's display name, 1 to 100 characters. The slug stays."
            );
            operation["x-implementation"] = json!("implemented");
        }
        if path == "/v1/projects/{p}/slack" {
            operation["summary"] = json!("Project Slack notifications");
            operation["description"] = json!(
                "Configure an incoming webhook or Slack app channel for a project. The webhook credential is write-only. Run-start notifications are off by default and are best effort alongside email and webhooks."
            );
            if method == "put" {
                operation["description"] = json!(
                    "Set an incoming webhook or Slack app channel. Omit both to update notify_run_started on an existing target. This preference defaults to false on first configuration and is retained when omitted."
                );
                operation["responses"]["409"] = problem_response(
                    "Slack configuration conflict.",
                    &["slack.app_not_configured", "slack.target_required"],
                );
            }
            operation["x-implementation"] = json!("implemented");
            operation["x-required-scopes"] = json!([if method == "get" {
                "runbooks:read"
            } else {
                "project:admin"
            }]);
        }
        if path == "/v1/me/email-preferences" {
            operation["summary"] = json!("Notification email preference (NOTIFY1)");
            operation["description"] = json!(
                "Implemented. The presenting person's own preference: all (the default), approvals_only, daily_summary, or none. Service tokens have no person and are refused."
            );
            operation["x-implementation"] = json!("implemented");
            operation["responses"]["401"] = problem_response(
                "Missing, revoked, expired or non-directory credential.",
                &["auth.required", "auth.invalid_credential"],
            );
        }
        if path == "/v1/unsubscribe" {
            operation["summary"] = json!("One-click unsubscribe (NOTIFY1)");
            operation["description"] = json!(
                "Implemented. The signed token from a notification email link is the whole credential; it only ever turns that person's mail off. A wrong or unknown token answers 404."
            );
            operation["x-implementation"] = json!("implemented");
            operation["responses"]["404"] =
                problem_response("Unknown unsubscribe token.", &["notify.invalid_token"]);
        }
        if path == "/v1/me/token" {
            operation["summary"] = json!("Revoke the presenting personal token (experimental)");
            operation["description"] = json!(
                "Implemented on Postgres (B1.5, experimental). Revokes exactly the presenting personal access token; no tokens:manage scope is required. Service tokens return 403 and are revoked through token administration. After success the same secret returns 401 everywhere."
            );
            operation["x-implementation"] = json!("implemented");
            operation["responses"]["204"] = json!({"description":"Token revoked"});
            operation["responses"]["401"] = problem_response(
                "Missing, revoked, expired or non-directory credential.",
                &["auth.required", "auth.invalid_credential"],
            );
            operation["responses"]["403"] =
                problem_response("Only personal tokens can self-revoke.", &["auth.forbidden"]);
        }
        if path == "/v1/me/apps" || path == "/v1/me/apps/{client}" {
            let list = path == "/v1/me/apps";
            operation["summary"] = json!(if list {
                "List the OAuth apps you have connected (MCP2)"
            } else {
                "Disconnect an OAuth app (MCP2)"
            });
            operation["description"] = json!(if list {
                "The remote MCP clients you have granted access through the OAuth consent page, from the auth service: client ID, its self-registered (unverified) name, where its redirect URIs point, the granted scopes, when it was granted and last received a token, and when the grant ends (90 days after your last consent). Empty when sign-in is not configured. A person only: a human personal token with tokens:manage, or a browser session."
            } else {
                "Ends the grant now: the consent is removed and every refresh and access token of that client for you is revoked. The client must send you through consent again. 404 when you have no grant for that client."
            });
            operation["x-implementation"] = json!("implemented");
            if list {
                operation["responses"]["200"] = json!({"description":"Your connected apps","content":{"application/json":{"schema":{"$ref":"#/components/schemas/ConnectedAppPage"}}}});
            } else {
                operation["parameters"] = json!([{"name":"client","in":"path","required":true,"schema":{"type":"string"}}]);
                operation["responses"]["204"] = json!({"description":"Disconnected"});
                operation["responses"]["404"] =
                    problem_response("No grant for that client.", &["resource.not_found"]);
            }
            operation["responses"]["401"] = problem_response(
                "Missing, revoked, expired or non-directory credential.",
                &["auth.required", "auth.invalid_credential"],
            );
            operation["responses"]["403"] = problem_response(
                "Not a person with tokens:manage (a service, agent or OAuth credential).",
                &["auth.forbidden"],
            );
            operation["responses"]["503"] = problem_response(
                "The sign-in service did not answer.",
                &["auth.unavailable", "storage.unavailable"],
            );
        }
        if path == "/v1/executions/{exe}/report" {
            operation["description"] = json!(
                "Planned shared execution document: JSON uses ExecutionDocumentView both while running and after completion. The same document supports Markdown, PDF and JUnit exports; there is no separate report DTO. Authorization, rendering and live event transport are not implemented in Gate A."
            );
        }
        paths.entry(path.to_owned()).or_insert_with(|| json!({}))[method] = operation;
    }
    for route in DIRECTORY_ROUTES {
        let method = route.method.to_ascii_lowercase();
        let mut parameters:Vec<Value>=route.path.split('/').filter_map(|part|part.strip_prefix('{').and_then(|p|p.strip_suffix('}')).map(|name|json!({"name":name,"in":"path","required":true,"schema":{"type":"string"}}))).collect();
        if route.method == "POST" {
            parameters.push(json!({"name":"Idempotency-Key","in":"header","required":true,"schema":{"type":"string","minLength":1,"maxLength":256}}));
        }
        if route.path.contains("/vars") && matches!(route.method, "PUT" | "DELETE") {
            parameters.push(json!({"name":"If-Match","in":"header","required":true,"schema":{"type":"string"},"description":"Exact strong ETag from the variables GET. Missing is 428; stale is 412."}));
        }
        if route.method == "GET"
            && route
                .response
                .is_some_and(|r| r.starts_with("DirectoryPage"))
        {
            parameters.extend([json!({"name":"limit","in":"query","schema":{"type":"integer","minimum":1,"maximum":100,"default":100}}),json!({"name":"cursor","in":"query","schema":{"type":"string"},"description":"Opaque principal/route/revision-bound cursor; directory changes invalidate it (409 cursor.stale)."})]);
        }
        let public = matches!(route.path, "/v1/meta" | "/.well-known/runbook");
        let mut op = json!({"operationId":format!("{}_{}",method,route.path.trim_start_matches('/').replace(['/', '{', '}'],"_")),"x-implementation":"implemented","summary":"Public pilot directory operation","description":"Bearer credentials only. D37 directory reads require active membership and a directory-capable read scope (runbooks:read, executions:read or project:admin). Users/teams are org-wide recipient identities; project lists intersect membership, org role, and token project restrictions. Service credentials may read recipients in their own project's org. Directory mutations require a human owner/admin plus project:admin and an unrestricted project list. Project variable reads require runbooks:read, writes project:admin with project role checks; If-Match fences writes, and D34 secret-like values return 422 vars.secret_like. No cookie/session authorization or private UI API.","security":if public {json!([])}else{json!([{"bearerAuth":[]}])},"parameters":parameters,"responses":{}});
        for status in [
            "400", "401", "403", "404", "405", "409", "412", "413", "415", "422", "428", "503",
            "default",
        ] {
            op["responses"][status] =
                problem_response("Stable problem details and X-Request-ID.", &[]);
        }
        // CF12b review round 1: a project or team title or a user's name
        // past 1 KiB (`field_caps::NAME_BYTES`).
        if route.method == "POST"
            && matches!(
                route.path,
                "/v1/orgs/{org}/projects" | "/v1/orgs/{org}/teams" | "/v1/orgs/{org}/users"
            )
        {
            op["responses"]["422"] = problem_response(
                "Stable problem details and X-Request-ID. A name or title past 1 KiB is request.value_too_large.",
                &["request.value_too_large"],
            );
        }
        op["responses"][route.status.to_string()] = if let Some(response) = route.response {
            json!({"description":"Typed response","content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{response}")}}}})
        } else {
            json!({"description":"No content"})
        };
        op["responses"][route.status.to_string()]["headers"] = json!({"X-Request-ID":{"schema":{"type":"string"}},"X-RateLimit-Limit":{"schema":{"type":"string"},"description":"unlimited on native"}});
        if route.path.contains("/vars") {
            op["responses"][route.status.to_string()]["headers"]["ETag"] = json!({"schema":{"type":"string"},"description":"Strong project-variable revision ETag"});
        }
        if let Some(request) = route.request {
            op["requestBody"] = json!({"required":true,"content":{"application/json":{"schema":{"$ref":format!("#/components/schemas/{request}")}}}});
        }
        paths.entry(route.path).or_insert_with(|| json!({}))[method] = op;
    }
    authoring::register(&mut paths);
    executions::register(&mut paths);
    actors::register(&mut paths);
    credentials::register(&mut paths);
    webhooks::register(&mut paths);
    tokens::register(&mut paths);
    post_pilot::register(&mut paths);
    archives::register(&mut paths);
    auth::register(&mut paths);
    auth::register_device(&mut paths);
    runners::register(&mut paths);
    auth::cookie_alternatives(&mut paths);
    mfa::register(&mut paths);
    json!({
        "openapi":"3.1.0",
        "jsonSchemaDialect":"https://json-schema.org/draft/2020-12/schema",
        "info":{"title":"The Runbook — draft pilot contract","version":"0.1.0-draft","description":"Early UI design contract generated from Rust DTOs. Directory, discovery, authoring and execution start/read routes are implemented; other operations remain explicitly planned."},
        "x-contract-stage":"draft",
        "x-authoring-policy": {
            "roles":"Project editors and admins hold runbooks:write and runbooks:publish; credential scopes may narrow these permissions.",
            "credentials":"Scoped PATs, service tokens and OAuth credentials may author. Execution access/refresh credentials grant no runbooks:* scopes and cannot read full sources.",
            "cli-push":"Planned Gate C behavior: publishing push without runbooks:publish must fail with 403 auth.scope_missing before any create/save and suggest --draft. push --draft saves with runbooks:write without publishing.",
            "implementation":"Authorization, concurrency enforcement, catalog persistence and client preflight belong to Gates B/C. These are planned requirements, not live guarantees."
        },
        "x-webhook-delivery":webhooks::delivery_contract(),
        "paths":paths,
        "components":{"schemas":schemas,"securitySchemes":{
            "bearerAuth":{"type":"http","scheme":"bearer"},
            "cookieAuth":{"type":"apiKey","in":"cookie","name":"__Host-runbook.session_token","description":"AUTH2 (D44, D46): the web app's Better Auth session cookie. httpOnly, Secure, Path=/, no Domain, SameSite=Lax, 30 days with sliding renewal; named runbook.session_token on a plain-http loopback origin. The earlier https name __Secure-runbook.session_token is still accepted for one release. When an Authorization header is present only the bearer is used. Cookie-authenticated unsafe methods also need Origin equal to the server origin (or Sec-Fetch-Site: same-origin) and X-Runbook-CSRF: 1, otherwise 403 auth.csrf. /mcp and /hooks ignore cookies. Only operations that list cookieAuth accept it: every /v1 operation a person can call with a bearer token, except minting, refreshing or revoking execution credentials and DELETE /v1/me/token. A session authorizes as its linked human with the scopes their roles allow, never as an agent."}
        }}
    })
}

fn problem_response(description: &str, codes: &[&str]) -> Value {
    json!({"description":description,"x-error-codes":codes,"content":{
        "application/problem+json":{"schema":{"$ref":"#/components/schemas/Problem"}}
    }})
}
