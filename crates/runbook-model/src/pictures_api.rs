//! Profile photos, project images and project settings (2026-11-17).
use crate::PublicRoute;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PICTURE_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "PUT",
        path: "/v1/me/picture",
        request: Some("PictureUpload"),
        response: Some("PictureView"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/me/picture",
        request: None,
        response: None,
        status: 204,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/users/{user}/picture",
        request: None,
        response: None,
        status: 200,
    },
    PublicRoute {
        method: "PATCH",
        path: "/v1/projects/{p}",
        request: Some("UpdateProjectRequest"),
        response: Some("ProjectView"),
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/projects/{p}/picture",
        request: None,
        response: None,
        status: 200,
    },
    PublicRoute {
        method: "PUT",
        path: "/v1/projects/{p}/picture",
        request: Some("PictureUpload"),
        response: Some("PictureView"),
        status: 200,
    },
    PublicRoute {
        method: "DELETE",
        path: "/v1/projects/{p}/picture",
        request: None,
        response: None,
        status: 204,
    },
];

/// A PNG or JPEG of at most 512×512 pixels and 1 MiB, base64-encoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PictureUpload {
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PictureView {
    pub content_type: String,
    pub width: u32,
    pub height: u32,
    pub updated_at_ms: i64,
}

/// `PATCH /v1/projects/{p}`: a project's settings (project admins).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateProjectRequest {
    /// Display name (1 to 100 characters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Archive (true) or restore (false) the project: org owners and
    /// admins only. Archived projects are hidden and read-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    /// R0-3: the runner agent unnamed agent steps target in runbooks
    /// published from now on (`null` clears it; `external` keeps them
    /// open to external agents). Project admins only.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "present"
    )]
    pub default_agent: Option<Option<String>>,
}
/// A present field, `null` included, is `Some`; an absent one stays `None`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}
