//! Operations the contract marks `x-implementation: planned` (later-plans
//! rule 5). The server answers `501 request.not_implemented` for each before
//! authentication, and clients must not call them. A package deletes its rows
//! when it ships the route.
use crate::PublicRoute;

#[allow(dead_code)]
const fn planned(
    method: &'static str,
    path: &'static str,
    request: Option<&'static str>,
    response: Option<&'static str>,
    status: u16,
) -> PublicRoute {
    PublicRoute {
        method,
        path,
        request,
        response,
        status,
    }
}

pub const PLANNED_ROUTES: &[PublicRoute] = &[
    // ARC3 shipped the daily archive routes (`ARCHIVE_ROUTES`); nothing
    // from the T0 freeze is still planned.
];
