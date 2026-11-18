//! Planned operations answer `501 request.not_implemented` (later-plans
//! rule 5), decided from the method and path alone, before authentication or
//! reading a body. The list is `runbook_model::PLANNED_ROUTES`, the same one
//! the contract marks `x-implementation: planned`.
use runbook_model::PLANNED_ROUTES;

pub const NOT_IMPLEMENTED: (u16, &str) = (501, "request.not_implemented");

pub fn recognizes(method: &str, path: &str) -> bool {
    PLANNED_ROUTES.iter().any(|route| {
        route.method.eq_ignore_ascii_case(method)
            && crate::directory::match_path(route.path, path).is_some()
    })
}
