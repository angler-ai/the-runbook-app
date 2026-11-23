//! The D1 schema (`cloudflare/migrations/d1/`): CF3a's translation of the
//! Postgres migrations (versions 1–29, checked against Postgres by the
//! parity test) plus D1-only files from version 1001 (the batch guard, the
//! execution-token route). `schema_migration` records each applied
//! version with its SHA-256; an applied file never runs again, and one
//! whose bytes changed is refused (`d1.migration_history_mismatch`), as on
//! Postgres and in the ExecutionDO.
//!
//! Each migration runs as one D1 batch together with its history row, so a
//! migration applies whole or not at all. Two isolates migrating at once
//! race on the history row's primary key; the loser re-reads and finds the
//! version applied.
use crate::sql::{Columns, D1, D1Error, Statement, prepare};
use sha2::{Digest, Sha256};

/// One migration file.
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every D1 migration under `cloudflare/migrations/d1/`, by version.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_history.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0001_history.sql"),
    },
    Migration {
        version: 2,
        name: "0002_execution_catalog.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0002_execution_catalog.sql"),
    },
    Migration {
        version: 3,
        name: "0003_directory.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0003_directory.sql"),
    },
    Migration {
        version: 4,
        name: "0004_directory_member_org.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0004_directory_member_org.sql"),
    },
    Migration {
        version: 5,
        name: "0005_work_lease.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0005_work_lease.sql"),
    },
    Migration {
        version: 6,
        name: "0006_org_roles.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0006_org_roles.sql"),
    },
    Migration {
        version: 7,
        name: "0007_blobs_tombstones.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0007_blobs_tombstones.sql"),
    },
    Migration {
        version: 8,
        name: "0008_credentials.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0008_credentials.sql"),
    },
    Migration {
        version: 9,
        name: "0009_catalog_publication.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0009_catalog_publication.sql"),
    },
    Migration {
        version: 10,
        name: "0010_skills.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0010_skills.sql"),
    },
    Migration {
        version: 11,
        name: "0011_instance_bootstrap.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0011_instance_bootstrap.sql"),
    },
    Migration {
        version: 12,
        name: "0012_run_search.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0012_run_search.sql"),
    },
    Migration {
        version: 13,
        name: "0013_directory_api.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0013_directory_api.sql"),
    },
    Migration {
        version: 14,
        name: "0014_start_admission.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0014_start_admission.sql"),
    },
    Migration {
        version: 15,
        name: "0015_draft_source_bytes.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0015_draft_source_bytes.sql"),
    },
    Migration {
        version: 16,
        name: "0016_owner_recovery_audit.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0016_owner_recovery_audit.sql"),
    },
    Migration {
        version: 17,
        name: "0017_claims.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0017_claims.sql"),
    },
    Migration {
        version: 18,
        name: "0018_start_replay_retention.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0018_start_replay_retention.sql"),
    },
    Migration {
        version: 19,
        name: "0019_execution_credentials.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0019_execution_credentials.sql"),
    },
    Migration {
        version: 20,
        name: "0020_actor_replay_step_index.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0020_actor_replay_step_index.sql"),
    },
    Migration {
        version: 21,
        name: "0021_webhooks.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0021_webhooks.sql"),
    },
    Migration {
        version: 22,
        name: "0022_webhook_follow_ups.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0022_webhook_follow_ups.sql"),
    },
    Migration {
        version: 23,
        name: "0023_review_indexes.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0023_review_indexes.sql"),
    },
    Migration {
        version: 24,
        name: "0024_step_run_tombstone.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0024_step_run_tombstone.sql"),
    },
    Migration {
        version: 25,
        name: "0025_inbox_index.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0025_inbox_index.sql"),
    },
    Migration {
        version: 26,
        name: "0026_memory.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0026_memory.sql"),
    },
    Migration {
        version: 27,
        name: "0027_auth_links_invitations_device.sql",
        sql: include_str!(
            "../../../cloudflare/migrations/d1/0027_auth_links_invitations_device.sql"
        ),
    },
    Migration {
        version: 28,
        name: "0028_uploads.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0028_uploads.sql"),
    },
    Migration {
        version: 29,
        name: "0029_archives_retention.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/0029_archives_retention.sql"),
    },
    Migration {
        version: 1001,
        name: "1001_d1_guard.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1001_d1_guard.sql"),
    },
    Migration {
        version: 1002,
        name: "1002_execution_token_route.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1002_execution_token_route.sql"),
    },
    Migration {
        version: 1003,
        name: "1003_memory_fts_words.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1003_memory_fts_words.sql"),
    },
    Migration {
        version: 1004,
        name: "1004_run_search_words_and_admission_notice.sql",
        sql: include_str!(
            "../../../cloudflare/migrations/d1/1004_run_search_words_and_admission_notice.sql"
        ),
    },
    Migration {
        version: 1005,
        name: "1005_admission_pool_revision.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1005_admission_pool_revision.sql"),
    },
    Migration {
        version: 1006,
        name: "1006_object_purge.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1006_object_purge.sql"),
    },
    Migration {
        version: 1007,
        name: "1007_archive_version.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1007_archive_version.sql"),
    },
    Migration {
        version: 1008,
        name: "1008_supersede_notice.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1008_supersede_notice.sql"),
    },
    Migration {
        version: 1009,
        name: "1009_webhook_log_to_objects.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1009_webhook_log_to_objects.sql"),
    },
    Migration {
        version: 1010,
        name: "1010_runbook_source_digests.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1010_runbook_source_digests.sql"),
    },
    Migration {
        version: 1011,
        name: "1011_run_release.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1011_run_release.sql"),
    },
    Migration {
        version: 1012,
        name: "1012_cron_backoff.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1012_cron_backoff.sql"),
    },
    Migration {
        version: 1013,
        name: "1013_webhook_subscription_deleted.sql",
        sql: include_str!(
            "../../../cloudflare/migrations/d1/1013_webhook_subscription_deleted.sql"
        ),
    },
    Migration {
        version: 1014,
        name: "1014_run_release_backoff.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1014_run_release_backoff.sql"),
    },
    Migration {
        version: 1015,
        name: "1015_replay_sweep_backoff.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1015_replay_sweep_backoff.sql"),
    },
    Migration {
        version: 1016,
        name: "1016_run_summary_runbook_indexes.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1016_run_summary_runbook_indexes.sql"),
    },
    Migration {
        version: 1018,
        name: "1018_runbook_listing.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1018_runbook_listing.sql"),
    },
    Migration {
        version: 1019,
        name: "1019_notify.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1019_notify.sql"),
    },
    Migration {
        version: 1020,
        name: "1020_project_slack.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1020_project_slack.sql"),
    },
    Migration {
        version: 1021,
        name: "1021_pictures_invitation_name.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1021_pictures_invitation_name.sql"),
    },
    Migration {
        version: 1022,
        name: "1022_project_archived.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1022_project_archived.sql"),
    },
    Migration {
        version: 1023,
        name: "1023_signing_keys.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1023_signing_keys.sql"),
    },
    Migration {
        version: 1024,
        name: "1024_runners.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1024_runners.sql"),
    },
    Migration {
        version: 1025,
        name: "1025_agent_directory.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1025_agent_directory.sql"),
    },
    Migration {
        version: 1026,
        name: "1026_runner_ready.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1026_runner_ready.sql"),
    },
    Migration {
        version: 1027,
        name: "1027_runner_ready_heads.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1027_runner_ready_heads.sql"),
    },
    Migration {
        version: 1028,
        name: "1028_vault_identity.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1028_vault_identity.sql"),
    },
    Migration {
        version: 1029,
        name: "1029_vault_identity_audit.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1029_vault_identity_audit.sql"),
    },
    Migration {
        version: 1030,
        name: "1030_org_security.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1030_org_security.sql"),
    },
    Migration {
        version: 1031,
        name: "1031_runner_decisions.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1031_runner_decisions.sql"),
    },
    Migration {
        version: 1032,
        name: "1032_runner_decision_contract.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1032_runner_decision_contract.sql"),
    },
    Migration {
        version: 1033,
        name: "1033_project_slack_run_started.sql",
        sql: include_str!("../../../cloudflare/migrations/d1/1033_project_slack_run_started.sql"),
    },
];

pub fn checksum(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Split a migration file into statements: `--` comments dropped, `;`
/// ends a statement, except inside a `CREATE TRIGGER … BEGIN … END;` body
/// or a quoted string.
pub fn statements(sql: &str) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    let mut in_trigger = false;
    for line in sql.lines() {
        let line = strip_comment(line);
        if line.trim().is_empty() {
            continue;
        }
        current.push_str(line);
        current.push('\n');
        let upper = line.trim().to_ascii_uppercase();
        if upper.starts_with("CREATE TRIGGER") {
            in_trigger = true;
        }
        let ends = if in_trigger {
            upper == "END;" || upper.ends_with(" END;")
        } else {
            upper.ends_with(';')
        };
        if ends {
            in_trigger = false;
            out.push(current.trim().trim_end_matches(';').trim().to_owned());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().trim_end_matches(';').trim().to_owned());
    }
    out
}

/// Drop a `--` comment that is not inside a quoted string.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\'' => quoted = !quoted,
            b'-' if !quoted && bytes.get(index + 1) == Some(&b'-') => return &line[..index],
            _ => {}
        }
    }
    line
}

/// The applied versions and checksums, or none before the first migration.
async fn applied<D: D1>(db: &D) -> Result<Vec<(i64, String)>, D1Error> {
    let exists = db
        .all(prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_migration'",
        ))
        .await?;
    if exists.is_empty() {
        return Ok(vec![]);
    }
    db.all(prepare(
        "SELECT version, checksum FROM schema_migration ORDER BY version",
    ))
    .await?
    .iter()
    .map(|row| Ok((row.integer(0)?, row.string(1)?)))
    .collect()
}

/// How many of this code's migrations are applied with their recorded
/// checksums; refuses a changed history.
///
/// UPD1: versions this code does not know are a newer release's (a
/// `wrangler rollback` runs this code on a database the next release
/// migrated, which expand/contract keeps compatible). They are accepted
/// only when every file this code knows is applied unchanged.
fn check(applied: &[(i64, String)]) -> Result<usize, D1Error> {
    let mut current = 0;
    let mut unknown = 0;
    for (version, sum) in applied {
        match MIGRATIONS.iter().find(|m| m.version == *version) {
            Some(m) if checksum(m.sql) == *sum => current += 1,
            Some(_) => return Err(D1Error::Other("d1.migration_history_mismatch".into())),
            None => unknown += 1,
        }
    }
    if unknown > 0 && current < MIGRATIONS.len() {
        return Err(D1Error::Other("d1.migration_history_mismatch".into()));
    }
    Ok(current)
}

/// Deploy finding: an operator applies these files before traffic with
/// `wrangler d1 migrations apply` (README), which records them in its own
/// `d1_migrations` table, not here. With an empty history the runner
/// would re-run every file and fail on the first `ALTER TABLE`. So whenever
/// files are pending (the first deploy, and every update, UPD1) it adopts
/// what wrangler recorded: each of its files wrangler applied is marked
/// applied (with this file's checksum, as the runner would have), and
/// anything wrangler did not apply is applied as usual.
async fn adopt_wrangler_history<D: D1>(db: &D) -> Result<Vec<(i64, String)>, D1Error> {
    let exists = db
        .all(prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'd1_migrations'",
        ))
        .await?;
    if exists.is_empty() {
        return Ok(vec![]);
    }
    let names: Vec<String> = db
        .all(prepare("SELECT name FROM d1_migrations"))
        .await?
        .iter()
        .map(|row| row.string(0))
        .collect::<Result<_, _>>()?;
    let adopted: Vec<Statement> = MIGRATIONS
        .iter()
        .filter(|m| names.iter().any(|n| n == m.name))
        .map(|m| {
            prepare(
                "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?, ?, ?) \
                 ON CONFLICT (version) DO NOTHING",
            )
            .bind(m.version)
            .bind(checksum(m.sql))
            .bind(db.now_ms())
        })
        .collect();
    if !adopted.is_empty() {
        db.batch(adopted).await?;
    }
    applied(db).await
}

/// Bring the database up to date. Returns the versions applied now.
pub async fn migrate<D: D1>(db: &D) -> Result<Vec<i64>, D1Error> {
    for migration in MIGRATIONS {
        if migration.sql.contains('\r') {
            return Err(D1Error::Other("d1.migration_crlf".into()));
        }
    }
    let mut done = applied(db).await?;
    // UPD1: not only the first time. An update applies its new files with
    // wrangler before deploying, so they are in `d1_migrations` only.
    if check(&done)? != MIGRATIONS.len() {
        done = adopt_wrangler_history(db).await?;
    }
    if check(&done)? == MIGRATIONS.len() {
        return Ok(vec![]);
    }
    let mut now_applied = vec![];
    for migration in MIGRATIONS {
        if done
            .iter()
            .any(|(version, _)| *version == migration.version)
        {
            continue;
        }
        let mut batch: Vec<Statement> =
            statements(migration.sql).into_iter().map(prepare).collect();
        batch.push(
            prepare(
                "INSERT INTO schema_migration (version, checksum, applied_at) VALUES (?, ?, ?)",
            )
            .bind(migration.version)
            .bind(checksum(migration.sql))
            .bind(db.now_ms()),
        );
        match db.batch(batch).await {
            Ok(_) => now_applied.push(migration.version),
            Err(error) => {
                // Another isolate may have applied it meanwhile.
                let again = applied(db).await?;
                check(&again)?;
                if !again
                    .iter()
                    .any(|(version, _)| *version == migration.version)
                {
                    return Err(error);
                }
            }
        }
    }
    Ok(now_applied)
}

/// Whether the schema is current, without changing anything (a Worker
/// checks this once per isolate).
pub async fn is_current<D: D1>(db: &D) -> Result<bool, D1Error> {
    Ok(check(&applied(db).await?)? == MIGRATIONS.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn triggers_and_comments_split_into_whole_statements() {
        let parts = statements(
            "-- head\nCREATE TABLE a (x TEXT DEFAULT '--not');\n\
             CREATE TRIGGER t AFTER INSERT ON a BEGIN\n  INSERT INTO b VALUES (1);\n  INSERT INTO b VALUES (2);\nEND;\n\
             CREATE INDEX i ON a (x);",
        );
        assert_eq!(parts.len(), 3, "{parts:?}");
        assert!(parts[0].contains("'--not'"));
        assert!(parts[1].starts_with("CREATE TRIGGER") && parts[1].ends_with("END"));
    }
    /// Gate fix (low): every applied file is pinned by its SHA-256, as the
    /// ExecutionDO's are; an edit to an applied migration fails here
    /// before a deployed database refuses it.
    #[test]
    fn applied_files_are_pinned_by_sha256() {
        let pinned = [
            (
                "0001_history.sql",
                "d8e5d97278669ecbd96f2750933d0366504fc69ad2205ab2f263543965c6179d",
            ),
            (
                "0002_execution_catalog.sql",
                "f22dc20f16b3e4ad5be6610fbf316aa78eaa8740135a9f6973fc7668e20214a4",
            ),
            (
                "0003_directory.sql",
                "5b4e75035c531fc30590cd2464f4e68521488bc467856051e5b51aa7242cb4c3",
            ),
            (
                "0004_directory_member_org.sql",
                "b96d4c42b621cad023c4c5d8e932dfeba56f2c3d0d7225443107792c03b26848",
            ),
            (
                "0005_work_lease.sql",
                "45f4c06c3ad76eb643e04e749f26ef1117ad0c077449cd7906339579e8449992",
            ),
            (
                "0006_org_roles.sql",
                "22896854a5eacd1984212cf0ad7834505f7f78fab27e0a59a7986d7a84d6a192",
            ),
            (
                "0007_blobs_tombstones.sql",
                "30dc610c1e77357eb4267fdf1b7d6e6c9dd5f721da90c5ca7401f78c2084ed2e",
            ),
            (
                "0008_credentials.sql",
                "c583619190f2be2dfca078a7d7c6a71eb5311462b7e9c8f1a321fd5d9826b29f",
            ),
            (
                "0009_catalog_publication.sql",
                "3b5f772364e4f5f06597e4cfcf9bfba337043ca02cd00406e3348ac995a5e9ce",
            ),
            (
                "0010_skills.sql",
                "900de82b69eedc7e54a51d0cdd53fc01d82c8fc1277a1a42e69d83a56c3b6783",
            ),
            (
                "0011_instance_bootstrap.sql",
                "ffecc172354128f998c4f940d0dfa987e8b0f35cbf462e109c1ba1f4df42ebd2",
            ),
            (
                "0012_run_search.sql",
                "3e239adafa3b878443180f933f4f655793521476df2f44d8038459c40fa3d8f1",
            ),
            (
                "0013_directory_api.sql",
                "8ab3325c2a9462c6a2ced18e58130f9138f1848be011187243146291dadc2968",
            ),
            (
                "0014_start_admission.sql",
                "90bae0e3b0c46f7c17bc010d06b5e6ddb263d56a077f88576a065429ee923367",
            ),
            (
                "0015_draft_source_bytes.sql",
                "06b8c00de42849a96cdc67003e68bd2f06b602c2af21c4a733eed40e2bf13bb5",
            ),
            (
                "0016_owner_recovery_audit.sql",
                "8bfc88048c6c8fe4897b3b67c6ad70dae4fab3bd012268286d37c101b94f3f87",
            ),
            (
                "0017_claims.sql",
                "8c2820ffab638b9ed4c9ce62c955a74a0819f8a6a6dc93fcc7f07b4dc8830372",
            ),
            (
                "0018_start_replay_retention.sql",
                "2428b1ff2b37ae3bdba7f28694f5a4842556bbb028001e799e4f5fc85750e8b3",
            ),
            (
                "0019_execution_credentials.sql",
                "c5481e2eb88ee0eaffc0e4d48333465ce1f9d94c8fa6cb19aeff2f8b5943c0dd",
            ),
            (
                "0020_actor_replay_step_index.sql",
                "c9c5a2813c32bc271b3bededdd7d593821e61a4fde00a2750ce471a3412e5820",
            ),
            (
                "0021_webhooks.sql",
                "8fa3ccc013d026059f7783fe59d8d83ba810b6204a32a8dec679afa3abed3de7",
            ),
            (
                "0022_webhook_follow_ups.sql",
                "b40dae4c3e04a76ba3017607cabb6dcdc96dd5bda6d18ac3cb8885888dc475c2",
            ),
            (
                "0023_review_indexes.sql",
                "8aafb63aad925c7c51a78e171762607846496031e9487d807489c1cce224d085",
            ),
            (
                "0024_step_run_tombstone.sql",
                "dda3cc257cafa81ebcf094304b1412ba108cda5251df2ba792366fff2526cc9c",
            ),
            (
                "0025_inbox_index.sql",
                "a1a4154d5e9f1ef7129ac1a92787c87d5bf56e783a088327283c6e2d05dfe070",
            ),
            (
                "0026_memory.sql",
                "1bfef0f0a0e1ed0475f11847f9a9b8291bec33c41901cf813bd1def7d36d47c9",
            ),
            (
                "0027_auth_links_invitations_device.sql",
                "e51360625774d91bdd556625e41a055d0d21de56cdad02daf5f4a4b4f04cf360",
            ),
            (
                "0028_uploads.sql",
                "db90b251fed28f60b21e3db8509ac33759f262292b5ff7a112557c9dfafcee14",
            ),
            (
                "0029_archives_retention.sql",
                "a83155396518a9d614f7d292f64b2fab2003756f21da7d1228880540c79f83bd",
            ),
            (
                "1001_d1_guard.sql",
                "c992f7a81fb40f58f33bc84633d538aa5268f8d4e9361fa188c1cb8fb99e0d98",
            ),
            (
                "1002_execution_token_route.sql",
                "e04b7abb2daef71095b312c2a21ab45b201da8355eac990931109a299c82f118",
            ),
            (
                "1003_memory_fts_words.sql",
                "c4a02ff2ad791f3ea349139542593fee953d6d383b0e304981eca9928ebd9a8d",
            ),
            (
                "1004_run_search_words_and_admission_notice.sql",
                "580197ff8216ff00c9d2a3d4a05e0313983a2edf0c1408b66bd216c0e814b588",
            ),
            (
                "1005_admission_pool_revision.sql",
                "a19e049c71cde26a8e39697d96bac979058f2e10dc280dd2f9541cb0d969df49",
            ),
            (
                "1006_object_purge.sql",
                "4d7e23094ee422b3772dee00ee52075c628b17d2d7efbf4f6213f52455fc2108",
            ),
            (
                "1007_archive_version.sql",
                "1d04797f8717d5d5013b668cf7d3e000fd417d31fbde45d1f210e33c33c0b08a",
            ),
            (
                "1008_supersede_notice.sql",
                "7dc66baaa7dc8bdd69ecd57cda8a5da066cf31909e847941a7b2c32160670aca",
            ),
            (
                "1009_webhook_log_to_objects.sql",
                "2ab6f4b1c59b87e7c96687c42a695aea2a55e22ee43759a1f2a92184eef94641",
            ),
            (
                "1010_runbook_source_digests.sql",
                "4dac7ef1829d3e361731546831feabd7cd6f6589e83bcf72ea223e4ce948fce4",
            ),
            (
                "1011_run_release.sql",
                "444f6b10e38e5f689edcc79601215eb5f47e3e2be0d5fcc8b0b9fb2179a00853",
            ),
            (
                "1012_cron_backoff.sql",
                "17f19d1f9763c225197f8810f9d5813bfaadfaee8615ef6ab65be5511253ffbe",
            ),
            (
                "1013_webhook_subscription_deleted.sql",
                "163af70835d43e8e9897f03e6db38bada6f81e8d7e34d94883f763192e78d58a",
            ),
            (
                "1014_run_release_backoff.sql",
                "5c892c4c439b355687e262327a0670bd5095960642216005f56c02cc2b28a552",
            ),
            (
                "1015_replay_sweep_backoff.sql",
                "c50b64e98379e94f7f107ec830f4dcedc8b837d8d440d4e414372a53724661f5",
            ),
            (
                "1016_run_summary_runbook_indexes.sql",
                "8b55114079b93e2922d25f8a49694b807dbe23e2be2acc8994f72b494ea7b67b",
            ),
            (
                "1018_runbook_listing.sql",
                "f687a801b15415fa88075881009294fe3cf6150ee89f6663ac399a36874ef0f4",
            ),
            (
                "1019_notify.sql",
                "0d5e562ee598789ac03cf8032d57551328a1bb178ae25386eee42d917c217694",
            ),
            (
                "1020_project_slack.sql",
                "9206e12a09b74d1e65764db161532b8427ec2c4eae8b0632a22d74a74bb4bedc",
            ),
            (
                "1021_pictures_invitation_name.sql",
                "3d3924f55c52473aced36efc42c149625e8b95372a8088d0243c89832261ba84",
            ),
            (
                "1022_project_archived.sql",
                "b96303e177a3f1b899c4b3eb4a98acae0877b1759b79ee7d439bd04a48cd0f76",
            ),
            (
                "1023_signing_keys.sql",
                "9d4b3e616e74b03c9941444cb6a3ed7bd820408ec03cfe90d6cf40f8eaee5578",
            ),
            (
                "1024_runners.sql",
                "c961007591a140d5256f68369a48dcf661cd43f83c2ed6f2f8756e05980efe25",
            ),
            (
                "1025_agent_directory.sql",
                "472807df570a191220865e90156f90e1aa2e13a916ca1f87c695ae9a70688ac6",
            ),
            (
                "1026_runner_ready.sql",
                "4f48500bc040d8e6b01f9f20f7a123abe055539dacbec18d0add993d9d946a2c",
            ),
            (
                "1027_runner_ready_heads.sql",
                "7b6630f648a366ccadde3128d38cd08962c399d45e9187c17b5a6446c7e3f1b3",
            ),
            (
                "1028_vault_identity.sql",
                "95793e6a71264cc0f81dcedb8afe0f042a80937c7dde601f0872ad78685d4a69",
            ),
            (
                "1029_vault_identity_audit.sql",
                "7e4cab6281d4a5d0abf08e50bec22cd37e1dd0f12b79aa3b3e43ae02e5c2f558",
            ),
            (
                "1030_org_security.sql",
                "e347a5d8e82bffff70e3aa7b5605385e0d146d0ffb55cccec8455777068ca3ad",
            ),
            (
                "1031_runner_decisions.sql",
                "d58ff93f2cfd3730ac148300a614fef912e904b9d8fa61f1f174ef4e4203c634",
            ),
            (
                "1032_runner_decision_contract.sql",
                "89a3e1cda762ba407bcdb33bf89f125b9cdb7d561998ac8ce0152f874c2355ed",
            ),
            (
                "1033_project_slack_run_started.sql",
                "16e12a245653a733b4d2992534fd712f347b20f6a8d660b323075224958729c7",
            ),
        ];
        assert_eq!(MIGRATIONS.len(), pinned.len(), "pin every migration");
        for (migration, (name, sha)) in MIGRATIONS.iter().zip(pinned) {
            assert_eq!(migration.name, name);
            assert_eq!(checksum(migration.sql), sha, "{name} changed");
        }
    }
    #[test]
    fn versions_are_unique_ascending_and_lf_only() {
        let mut last = 0;
        for m in MIGRATIONS {
            assert!(m.version > last, "{}", m.name);
            last = m.version;
            assert!(!m.sql.contains('\r'), "{}", m.name);
        }
    }
}
