//! Sign-in, device login and invitation wire types (PRD §9, §10, §18;
//! D45–D48). Frozen at T0 as planned operations; AUTH2/AUTH3 implement them.
use crate::*;

/// The `auth` block of `/.well-known/runbook`. Empty `methods` and a null
/// endpoint mean sign-in is not configured (D48): the bootstrap PAT still works.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthDiscovery {
    /// Enabled sign-in methods, from `email` and `google`.
    pub methods: Vec<String>,
    /// RFC 8628 device authorization endpoint (`{origin}/v1/device/code`),
    /// null while device login is unavailable.
    pub device_authorization_endpoint: Option<String>,
}

/// `POST /v1/device/code`. No credential; rate-limited per IP.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceCodeRequest {
    /// Shown on the approval page, untrusted (e.g. "runbook CLI on laptop").
    #[serde(default)]
    pub client_name: Option<String>,
    /// PRD §9 scope names the CLI asks for; the approver may narrow them.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
}

/// RFC 8628 §3.2 device authorization response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceCodeView {
    pub device_code: String,
    /// Short code the person types, e.g. `WDJB-MJHT`.
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    /// Seconds.
    pub expires_in: u32,
    /// Minimum seconds between polls.
    pub interval: u32,
}

/// `POST /v1/device/token`. No credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceTokenRequest {
    /// `urn:ietf:params:oauth:grant-type:device_code`.
    pub grant_type: String,
    pub device_code: String,
}

/// The approved grant: an ordinary personal access token (D47), shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceTokenView {
    /// A `trb_pat_` secret; only its SHA-256 is stored.
    pub access_token: String,
    /// Always `bearer`.
    pub token_type: String,
    /// Seconds until the token expires (default 90 days).
    pub expires_in: u64,
    /// Space-separated granted scopes.
    pub scope: String,
    /// The token's ID, for `runbook token revoke`.
    pub token_id: TokenId,
}

/// `POST /v1/device/approve`: a signed-in person (session plus CSRF) approves
/// or denies a code. Every grant field narrows the approver's own access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceApproveRequest {
    pub user_code: String,
    pub approve: bool,
    /// Omitted: the requested scopes the approver holds.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// Project IDs or slugs; omitted: no project restriction beyond the approver's.
    #[serde(default)]
    pub projects: Option<Vec<String>>,
    /// UTC epoch milliseconds; omitted: 90 days, never past one year.
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
}

/// `POST /v1/device/lookup`: what the approval page shows before a person
/// decides (sign-in gate fix 3). A session plus CSRF, like approve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeviceLookupRequest {
    pub user_code: String,
}

/// A pending device login, for the approval page. Nothing here is proof of
/// who asked: the page shows `client_name` as the client's own claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceLookupView {
    /// The code, normalized (`XXXX-XXXX`).
    pub user_code: String,
    /// The name the CLI sent. Unverified: any client can send any name.
    pub client_name: String,
    /// Always false: show `client_name` as the client's claim, not a fact.
    pub client_name_verified: bool,
    /// The address that requested the code (the TCP peer, or the client a
    /// trusted proxy named); null when unknown.
    pub requester_ip: Option<String>,
    /// PRD §9 scope names the CLI asked for.
    pub requested_scopes: Vec<String>,
    /// UTC epoch milliseconds.
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InvitationState {
    Pending,
    Accepted,
    Revoked,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InvitationProjectGrant {
    /// Project ID or slug in the organization.
    pub project: String,
    pub role: ProjectRole,
}

/// `POST /v1/orgs/{org}/invitations` (owner or admin). Admins cannot
/// invite owners (D17 rank rule).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInvitationRequest {
    pub email: String,
    /// Default `member`.
    #[serde(default)]
    pub role: Option<OrganizationRole>,
    #[serde(default)]
    pub projects: Option<Vec<InvitationProjectGrant>>,
    /// The invitee's name, used for the account they get (1 to 100 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InvitationView {
    pub id: InvitationId,
    pub email: String,
    pub role: OrganizationRole,
    pub projects: Vec<InvitationProjectGrant>,
    pub state: InvitationState,
    pub invited_by: UserId,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// AUTH2 (D45): organization invitations, owner or admin only.
pub const INVITATION_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/orgs/{org}/invitations",
        request: None,
        response: Some("DirectoryPage_InvitationView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/orgs/{org}/invitations",
        request: Some("CreateInvitationRequest"),
        response: Some("InvitationView"),
        status: 201,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/orgs/{org}/invitations/{inv}",
        request: None,
        response: None,
        status: 204,
    },
];

/// AUTH3 (D47): the device authorization grant.
pub const DEVICE_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "POST",
        path: "/v1/device/code",
        request: Some("DeviceCodeRequest"),
        response: Some("DeviceCodeView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/device/token",
        request: Some("DeviceTokenRequest"),
        response: Some("DeviceTokenView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/device/lookup",
        request: Some("DeviceLookupRequest"),
        response: Some("DeviceLookupView"),
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/device/approve",
        request: Some("DeviceApproveRequest"),
        response: None,
        status: 204,
    },
];
