//! Public organization and account security routes.
use crate::PublicRoute;

pub const MFA_ROUTES: &[PublicRoute] = &[
    PublicRoute {
        method: "GET",
        path: "/v1/me/security",
        request: None,
        response: None,
        status: 200,
    },
    PublicRoute {
        method: "GET",
        path: "/v1/orgs/{org}/security",
        request: None,
        response: None,
        status: 200,
    },
    PublicRoute {
        method: "PUT",
        path: "/v1/orgs/{org}/security",
        request: None,
        response: None,
        status: 200,
    },
    PublicRoute {
        method: "POST",
        path: "/v1/orgs/{org}/users/{user}/mfa/reset",
        request: None,
        response: None,
        status: 200,
    },
];
