// UPD1 review #2: the rollback floor, shared by build.rs (which bakes it
// into the binary) and the crate's tests. Std only: build.rs includes it.
//
// The floor is the oldest release whose code is safe on this release's
// schema: the newest `-- contract(unused-since X.Y.Z)` in the Postgres
// migrations, or `[workspace.metadata.runbook] rollback-floor` in the
// workspace Cargo.toml when that is newer. None: any release since UPD1.

/// `X.Y.Z` as numbers.
pub fn floor_semver(text: &str) -> Option<(u64, u64, u64)> {
    let mut parts = text.split('.');
    let v = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    parts.next().is_none().then_some(v)
}

/// The floor from migration file contents and the workspace Cargo.toml.
///
/// UPD1 re-check #5: one strict grammar shared with the migration check
/// (`scripts/check-migrations.mjs`, which rejects any near miss) and
/// `cloudflare/scripts/update.mjs`: a line that is exactly
/// `-- contract(unused-since X.Y.Z): <reason>`, at column 0, and
/// `rollback-floor = "X.Y.Z"` under `[workspace.metadata.runbook]`.
pub fn rollback_floor<'a>(
    migrations: impl IntoIterator<Item = &'a str>,
    cargo_toml: &str,
) -> Option<String> {
    let mut floor: Option<String> = None;
    let mut raise = |candidate: &str| {
        let Some(v) = floor_semver(candidate) else {
            return;
        };
        if floor
            .as_deref()
            .and_then(floor_semver)
            .is_none_or(|current| current < v)
        {
            floor = Some(candidate.to_owned());
        }
    };
    for sql in migrations {
        for line in sql.lines() {
            if let Some(rest) = line.strip_prefix("-- contract(unused-since ")
                && let Some((version, reason)) = rest.split_once("): ")
                && reason.starts_with(|c: char| !c.is_whitespace())
            {
                raise(version);
            }
        }
    }
    let mut in_section = false;
    for line in cargo_toml.lines() {
        if line.starts_with('[') {
            in_section = line.trim_end() == "[workspace.metadata.runbook]";
        } else if in_section
            && let Some(value) = line
                .strip_prefix("rollback-floor = \"")
                .and_then(|v| v.strip_suffix('"'))
        {
            raise(value);
        }
    }
    floor
}
