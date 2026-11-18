//! `BlobKey` ↔ object key (D57: per-run prefixes, tenant first).
//!
//! | `BlobKey`   | bucket       | object key                                              |
//! |-------------|--------------|---------------------------------------------------------|
//! | `Execution` | blobs        | `t/{org}/{project}/runs/{exe}/blobs/{name}`             |
//! | `Upload`    | user content | `t/{org}/{project}/runs/{exe}/uploads/{evd}`            |
//! | `Shared`    | blobs        | `t/{org}/{project}/shared/{sha256 hex}`                 |
//! | `Archive`   | blobs        | `t/{org}/{project}/archives/{runbook}/{YYYY-MM-DD}/{digest hex}.zip` |
//!
//! CF12b: request replay responses too large for their D1 row sit under
//! `t/{org}/{project}/replay/{name}` in the blobs bucket. They are no
//! `BlobKey` ([`parse`] names none of them), so a project's listing, and
//! with it its export, never carries them; each goes with its row.
//!
//! Everything one run owns sits under `…/runs/{exe}/` in each bucket, so a
//! run exports, archives or deletes as a unit (PRD §14). Uploads, which
//! people supply, live in their own bucket, served from a separate
//! hostname (D57).
use runbook_core::{blob::BlobKey, catalog::TenantScope};
use runbook_model::{EvidenceId, ExecutionId, RunbookId};

/// Which bucket a key lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    Blobs,
    UserContent,
}

pub fn tenant(scope: &TenantScope) -> String {
    format!("t/{}/{}/", scope.org_id, scope.project_id)
}
pub fn run_prefix(scope: &TenantScope, id: &ExecutionId) -> String {
    format!("{}runs/{id}/", tenant(scope))
}
pub fn shared_prefix(scope: &TenantScope) -> String {
    format!("{}shared/", tenant(scope))
}
/// CF12b: the object of replay response `name`.
pub fn replay_object(scope: &TenantScope, name: &str) -> String {
    format!("{}replay/{name}", tenant(scope))
}
pub fn archive_prefix(scope: &TenantScope, runbook: &RunbookId, day: i64) -> String {
    format!("{}archives/{runbook}/{}/", tenant(scope), date(day))
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// UTC date of a day number (days since the Unix epoch).
pub fn date(day: i64) -> String {
    let rendered = runbook_core::webhooks::rfc3339(day.saturating_mul(86_400_000));
    rendered.get(..10).unwrap_or_default().to_owned()
}
/// The day number of a `YYYY-MM-DD` date (Howard Hinnant's days_from_civil).
pub fn day(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-');
    let (y, m, d): (i64, i64, i64) = (
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    );
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || date.len() != 10 {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The bucket and object key of `key` for `scope`.
pub fn object(scope: &TenantScope, key: &BlobKey) -> (Bucket, String) {
    match key {
        BlobKey::Execution { execution_id, name } => (
            Bucket::Blobs,
            format!("{}blobs/{name}", run_prefix(scope, execution_id)),
        ),
        BlobKey::Upload {
            execution_id,
            evd_id,
        } => (
            Bucket::UserContent,
            format!("{}uploads/{evd_id}", run_prefix(scope, execution_id)),
        ),
        BlobKey::Shared { hash } => (
            Bucket::Blobs,
            format!("{}{}", shared_prefix(scope), hex(hash)),
        ),
        BlobKey::Archive {
            runbook_id,
            day,
            digest,
        } => (
            Bucket::Blobs,
            format!(
                "{}{}.zip",
                archive_prefix(scope, runbook_id, *day),
                hex(digest)
            ),
        ),
    }
}

/// The `BlobKey` an object key under `scope` names, if it is one.
pub fn parse(scope: &TenantScope, bucket: Bucket, object: &str) -> Option<BlobKey> {
    let rest = object.strip_prefix(&tenant(scope))?;
    if let Some(run) = rest.strip_prefix("runs/") {
        let (id, tail) = run.split_once('/')?;
        let execution_id: ExecutionId = id.parse().ok()?;
        return match bucket {
            Bucket::Blobs => tail.strip_prefix("blobs/").map(|name| BlobKey::Execution {
                execution_id,
                name: name.to_owned(),
            }),
            Bucket::UserContent => {
                let evd_id: EvidenceId = tail.strip_prefix("uploads/")?.parse().ok()?;
                Some(BlobKey::Upload {
                    execution_id,
                    evd_id,
                })
            }
        };
    }
    if bucket != Bucket::Blobs {
        return None;
    }
    if let Some(hash) = rest.strip_prefix("shared/") {
        return Some(BlobKey::Shared { hash: unhex(hash)? });
    }
    let archive = rest.strip_prefix("archives/")?;
    let mut parts = archive.splitn(3, '/');
    let runbook_id: RunbookId = parts.next()?.parse().ok()?;
    let day = day(parts.next()?)?;
    let digest = unhex(parts.next()?.strip_suffix(".zip")?)?;
    Some(BlobKey::Archive {
        runbook_id,
        day,
        digest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> TenantScope {
        TenantScope {
            org_id: "org_00000000000000000000000001".parse().unwrap(),
            project_id: "prj_00000000000000000000000001".parse().unwrap(),
        }
    }
    #[test]
    fn every_key_round_trips_through_its_object_key() {
        let exe: ExecutionId = "exe_00000000000000000000000001".parse().unwrap();
        let keys = [
            BlobKey::Execution {
                execution_id: exe.clone(),
                name: "logs/step 1/chunk-0".into(),
            },
            BlobKey::Upload {
                execution_id: exe.clone(),
                evd_id: "evd_00000000000000000000000001".parse().unwrap(),
            },
            BlobKey::Shared { hash: [0xab; 32] },
            BlobKey::Archive {
                runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
                day: 20_404,
                digest: [7; 32],
            },
        ];
        for key in keys {
            let (bucket, object) = object(&scope(), &key);
            assert!(object.starts_with("t/org_00000000000000000000000001/"));
            assert_eq!(parse(&scope(), bucket, &object), Some(key));
        }
        let (_, archive) = object(
            &scope(),
            &BlobKey::Archive {
                runbook_id: "rb_00000000000000000000000001".parse().unwrap(),
                day: 20_404,
                digest: [7; 32],
            },
        );
        assert!(archive.contains("/archives/rb_00000000000000000000000001/2025-11-12/"));
    }
    /// CF12b: a replay response is no blob of the project's.
    #[test]
    fn a_replay_object_names_no_blob() {
        let object = replay_object(
            &scope(),
            &format!("{}.{}", "ab".repeat(32), "cd".repeat(16)),
        );
        assert!(object.starts_with(&tenant(&scope())));
        assert_eq!(parse(&scope(), Bucket::Blobs, &object), None);
    }
    #[test]
    fn days_and_dates_agree() {
        for day in [-1_000, 0, 1, 20_404, 60_000] {
            assert_eq!(super::day(&date(day)), Some(day), "{day}");
        }
        assert_eq!(super::day("2025-13-01"), None);
    }
}
