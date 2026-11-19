use crate::Error;
use sha2::{Digest, Sha256};
use tokio_postgres::{Client, GenericClient};

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_history.sql"),
    include_str!("../migrations/0002_execution_catalog.sql"),
    include_str!("../migrations/0003_directory.sql"),
    include_str!("../migrations/0004_directory_member_org.sql"),
    include_str!("../migrations/0005_work_lease.sql"),
    include_str!("../migrations/0006_org_roles.sql"),
    include_str!("../migrations/0007_blobs_tombstones.sql"),
    include_str!("../migrations/0008_credentials.sql"),
    include_str!("../migrations/0009_catalog_publication.sql"),
    include_str!("../migrations/0010_skills.sql"),
    include_str!("../migrations/0011_instance_bootstrap.sql"),
    include_str!("../migrations/0012_run_search.sql"),
    include_str!("../migrations/0013_directory_api.sql"),
    include_str!("../migrations/0014_start_admission.sql"),
    include_str!("../migrations/0015_draft_source_bytes.sql"),
    include_str!("../migrations/0016_owner_recovery_audit.sql"),
    include_str!("../migrations/0017_claims.sql"),
    include_str!("../migrations/0018_start_replay_retention.sql"),
    include_str!("../migrations/0019_execution_credentials.sql"),
    include_str!("../migrations/0020_actor_replay_step_index.sql"),
    include_str!("../migrations/0021_webhooks.sql"),
    include_str!("../migrations/0022_webhook_follow_ups.sql"),
    include_str!("../migrations/0023_review_indexes.sql"),
    include_str!("../migrations/0024_step_run_tombstone.sql"),
    include_str!("../migrations/0025_inbox_index.sql"),
    include_str!("../migrations/0026_memory.sql"),
    include_str!("../migrations/0027_auth_links_invitations_device.sql"),
    include_str!("../migrations/0028_uploads.sql"),
    include_str!("../migrations/0029_archives_retention.sql"),
    include_str!("../migrations/0030_quotas_usage.sql"),
    include_str!("../migrations/0031_rate_limits.sql"),
    include_str!("../migrations/0032_run_summary_runbook_indexes.sql"),
    include_str!("../migrations/0033_runbook_listing_order.sql"),
    include_str!("../migrations/0034_blob_objects.sql"),
    include_str!("../migrations/0035_notify.sql"),
    include_str!("../migrations/0036_project_slack.sql"),
    include_str!("../migrations/0037_pictures_invitation_name.sql"),
    include_str!("../migrations/0038_project_archived.sql"),
];
/// UPD1: the schema this code migrates to (its last migration's number).
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;
// Database-wide migration lock, held by PostgreSQL until transaction end.
const MIGRATION_LOCK: i64 = 0x72756e626f6f6b;

pub(crate) async fn run(client: &mut Client, allow_newer: bool) -> Result<u32, Error> {
    let transaction = client.transaction().await?;
    let version = apply(&transaction, allow_newer).await?;
    transaction.commit().await?;
    Ok(version)
}
/// Caller owns the transaction. The migration lock stays held until commit,
/// including all initial directory/credential writes during local bootstrap.
/// Returns the database's schema version afterwards: this code's, or (only
/// with `allow_newer`) a newer release's.
pub(crate) async fn apply(
    transaction: &impl GenericClient,
    allow_newer: bool,
) -> Result<u32, Error> {
    transaction
        .query_one("SELECT pg_advisory_xact_lock($1)", &[&MIGRATION_LOCK])
        .await?;
    transaction.batch_execute(MIGRATIONS[0]).await?;
    let history = transaction
        .query(
            "SELECT version, checksum FROM runbook.schema_migration ORDER BY version",
            &[],
        )
        .await?;
    let applied = verify(&history)?;
    if applied > MIGRATIONS.len() {
        return newer(applied, allow_newer);
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
        transaction.batch_execute(sql).await?;
        transaction
            .execute(
                "INSERT INTO runbook.schema_migration(version, checksum) VALUES ($1,$2)",
                &[&(index as i32 + 1), &digest(sql)],
            )
            .await?;
    }
    Ok(SCHEMA_VERSION)
}
fn digest(sql: &str) -> String {
    format!("{:x}", Sha256::digest(sql.as_bytes()))
}
/// The history must number 1, 2, … without gaps, and every file this code
/// knows must carry its checksum. Rows past this code's last file are a
/// newer release's (UPD1); the caller decides about those. Returns the
/// number of rows.
fn verify(history: &[tokio_postgres::Row]) -> Result<usize, Error> {
    for (index, row) in history.iter().enumerate() {
        let version: i32 = row.get(0);
        let checksum: String = row.get(1);
        if version != index as i32 + 1
            || MIGRATIONS
                .get(index)
                .is_some_and(|sql| checksum != digest(sql))
        {
            return Err(Error("postgres.migration_history_mismatch"));
        }
    }
    Ok(history.len())
}
/// UPD1: a newer release migrated this database. Refused unless the operator
/// allows it (`RUNBOOK_ALLOW_NEWER_SCHEMA`): never served silently.
fn newer(applied: usize, allow_newer: bool) -> Result<u32, Error> {
    if allow_newer {
        Ok(applied as u32)
    } else {
        Err(Error("postgres.schema_newer_than_code"))
    }
}

/// Read-only version and checksum check: doctor never performs migrations.
/// Returns the database's schema version.
pub(crate) async fn check(client: &Client, allow_newer: bool) -> Result<u32, Error> {
    let history: Option<String> = client
        .query_one("SELECT to_regclass('runbook.schema_migration')::text", &[])
        .await?
        .get(0);
    if history.is_none() {
        return Err(Error("postgres.schema_out_of_date"));
    }
    let rows = client
        .query(
            "SELECT version, checksum FROM runbook.schema_migration ORDER BY version",
            &[],
        )
        .await?;
    let applied = verify(&rows)?;
    if applied < MIGRATIONS.len() {
        return Err(Error("postgres.schema_out_of_date"));
    }
    if applied > MIGRATIONS.len() {
        return newer(applied, allow_newer);
    }
    Ok(SCHEMA_VERSION)
}
#[cfg(test)]
mod tests {
    /// R16: filename numbers must equal 1-based list positions, so the
    /// list can never drift from the assigned migration numbers again.
    #[test]
    fn migration_filenames_match_list_positions() {
        let source = include_str!("migrate.rs");
        let listed: Vec<&str> = source
            .lines()
            .filter(|line| line.trim().starts_with("include_str!(\"../migrations/"))
            .filter_map(|line| line.split('"').nth(1))
            .collect();
        assert_eq!(listed.len(), super::MIGRATIONS.len());
        // A file added without listing it (or listed without its file) must
        // not slip through: the directory is the migration set.
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let mut files: Vec<String> = std::fs::read_dir(directory)
            .expect("migrations directory is readable")
            .map(|entry| {
                entry
                    .expect("migration entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".sql"))
            .collect();
        files.sort();
        let listed_names: Vec<&str> = listed
            .iter()
            .map(|path| path.rsplit('/').next().unwrap())
            .collect();
        assert_eq!(
            listed_names,
            files.iter().map(String::as_str).collect::<Vec<_>>(),
            "every migration file must be listed once, in number order"
        );
        // A T0-reserved number whose package has not landed on this branch
        // may be skipped (packages build in parallel; the integrator merges
        // in number order, closing the gap). Nothing else may be skipped.
        let reserved = |n: usize| RESERVED.iter().any(|r| r[..4].parse() == Ok(n));
        let mut expected = 1;
        for path in listed {
            let name = path.rsplit('/').next().unwrap();
            let number: usize = name[..4].parse().expect("four-digit migration prefix");
            while number > expected && reserved(expected) {
                expected += 1;
            }
            assert_eq!(number, expected, "migration {name} out of position");
            expected += 1;
        }
    }

    /// Numbers reserved at the T0 contract freeze (plan 2026-11-12 §1) for the
    /// packages that own them. They are not in `MIGRATIONS`: an applied migration's
    /// checksum is permanent, so an empty placeholder would make every database
    /// that ran it refuse the real file (`postgres.migration_history_mismatch`).
    /// A package appends its file under exactly this name, in number order; the
    /// test below refuses any other name for these numbers.
    const RESERVED: &[&str] = &[
        "0026_memory.sql",                        // MEM2
        "0027_auth_links_invitations_device.sql", // AUTH2
        "0028_uploads.sql",                       // UP1
    ];

    /// T0 reservations: reserved numbers follow the last unreserved one in
    /// ascending order (a package removes its entry when its file lands, as
    /// ARC2 did for 0029), and a listed migration with a reserved number
    /// carries the reserved name.
    #[test]
    fn reserved_migration_numbers_keep_their_names() {
        let first: usize = RESERVED[0][..4].parse().unwrap();
        assert_eq!(first, 26, "reservations start after 0025_inbox_index");
        for pair in RESERVED.windows(2) {
            let (a, b): (usize, usize) =
                (pair[0][..4].parse().unwrap(), pair[1][..4].parse().unwrap());
            assert!(a < b, "{} and {} are out of order", pair[0], pair[1]);
        }
        let source = include_str!("migrate.rs");
        for path in source
            .lines()
            .filter(|line| line.trim().starts_with("include_str!(\"../migrations/"))
            .filter_map(|line| line.split('"').nth(1))
        {
            let name = path.rsplit('/').next().unwrap();
            if let Some(reserved) = RESERVED.iter().find(|r| r[..4] == name[..4]) {
                assert_eq!(
                    &name, reserved,
                    "migration {name} uses a reserved number under another name"
                );
            }
        }
    }
}
