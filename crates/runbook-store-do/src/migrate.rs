//! Versioned schema for one ExecutionDO, applied inside the object on first
//! use (once per object instance: [`crate::DoObject`] caches the result). `do_migration` records each applied file with its SHA-256; an
//! applied file is never re-run, and one whose bytes changed is refused
//! (`do.migration_history_mismatch`), as the Postgres runner does.
use crate::sql::{Columns, DoSql, Value};
use runbook_core::StoreError;
use sha2::{Digest, Sha256};

/// One migration file under `cloudflare/migrations/do/`.
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every DO migration, in order. Append only: positions are versions.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_execution.sql",
        sql: include_str!("../../../cloudflare/migrations/do/0001_execution.sql"),
    },
    Migration {
        version: 2,
        name: "0002_session_fence.sql",
        sql: include_str!("../../../cloudflare/migrations/do/0002_session_fence.sql"),
    },
    Migration {
        version: 3,
        name: "0003_log_chunk.sql",
        sql: include_str!("../../../cloudflare/migrations/do/0003_log_chunk.sql"),
    },
    Migration {
        version: 4,
        name: "0004_import_hold.sql",
        sql: include_str!("../../../cloudflare/migrations/do/0004_import_hold.sql"),
    },
    Migration {
        version: 5,
        name: "0005_import_hold_id.sql",
        sql: include_str!("../../../cloudflare/migrations/do/0005_import_hold_id.sql"),
    },
    Migration {
        version: 6,
        name: "0006_split_rows.sql",
        sql: include_str!("../../../cloudflare/migrations/do/0006_split_rows.sql"),
    },
];

const HISTORY: &str = "CREATE TABLE IF NOT EXISTS do_migration (
  version INTEGER PRIMARY KEY,
  name TEXT NOT NULL,
  checksum TEXT NOT NULL,
  applied_at_ms INTEGER NOT NULL
)";

/// Migration files are LF-only: a CR (a CRLF checkout) would change the
/// checksum and look like an edited history, so it is refused with its own
/// code instead. `.gitattributes` keeps the files LF on every checkout.
pub fn check_sql(sql: &str) -> Result<(), StoreError> {
    if sql.contains('\r') {
        return Err(StoreError::Backend("do.migration_crlf"));
    }
    Ok(())
}

pub fn checksum(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Bring this object's schema up to date in one transaction. Returns the
/// versions applied now (empty when it was current).
pub fn migrate<B: DoSql>(sql: &B) -> Result<Vec<i64>, StoreError> {
    migrate_with(sql, MIGRATIONS)
}

/// [`migrate`] for another object class's migrations (the OrgDO, CF6).
pub fn migrate_with<B: DoSql>(
    sql: &B,
    migrations: &'static [Migration],
) -> Result<Vec<i64>, StoreError> {
    for migration in migrations {
        check_sql(migration.sql)?;
    }
    let backend = sql.clone();
    sql.transaction(move || {
        backend.exec(HISTORY, &[])?;
        let applied = backend.query(
            "SELECT version, name, checksum FROM do_migration ORDER BY version",
            &[],
        )?;
        for (index, row) in applied.iter().enumerate() {
            let Some(m) = migrations.get(index) else {
                // UPD1: a newer release's file (a `wrangler rollback` runs
                // this code on an object the next release migrated, which
                // expand/contract keeps compatible). Versions stay
                // positional; every file this code knows was checked above.
                if row.integer(0).ok() != i64::try_from(index + 1).ok() {
                    return Err(StoreError::Backend("do.migration_history_mismatch"));
                }
                continue;
            };
            if row.integer(0).ok() != Some(m.version)
                || row.text(1).ok() != Some(m.name)
                || row.text(2).ok() != Some(checksum(m.sql).as_str())
            {
                return Err(StoreError::Backend("do.migration_history_mismatch"));
            }
        }
        let mut now_applied = vec![];
        for migration in migrations.iter().skip(applied.len()) {
            backend.exec_script(migration.sql)?;
            backend.exec(
                "INSERT INTO do_migration (version, name, checksum, applied_at_ms) \
                 VALUES (?, ?, ?, ?)",
                &[
                    Value::Integer(migration.version),
                    migration.name.into(),
                    checksum(migration.sql).into(),
                    Value::Integer(backend.now_ms()),
                ],
            )?;
            now_applied.push(migration.version);
        }
        Ok(now_applied)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An applied file never changes: its bytes are pinned here. Change a
    /// migration by adding the next numbered file, never by editing one.
    #[test]
    fn applied_files_are_pinned_by_sha256() {
        let pinned = [
            (
                "0001_execution.sql",
                "cca89faa1e3b9e04a370d07af3ad319cbb5a543143c53afe44921f509eb7a7cc",
            ),
            (
                "0002_session_fence.sql",
                "2ce74965f895ad097b7ed585864e7f167bf73a6acd0f6ba3a8a3eca082e8318e",
            ),
            (
                "0003_log_chunk.sql",
                "72e67e3a9f1d2ebb3d64edcc82bdbcedc8cf92c90a65ef70fbb5ee4a10bdb9a0",
            ),
            (
                "0004_import_hold.sql",
                "73d838983122b926a38aa3660f2fd2d485b1cb3b8fd9a4486b8384a00c020c55",
            ),
            (
                "0005_import_hold_id.sql",
                "99782956a868f3efac334c4eb3789e7943165fce55bc0950b1d16bc2471c39d3",
            ),
            (
                "0006_split_rows.sql",
                "07a2ca68ac6d64b657d6505cda3f07579552780042c5d820049ca8c81861d044",
            ),
        ];
        assert_eq!(MIGRATIONS.len(), pinned.len(), "pin every migration");
        for (migration, (name, sha)) in MIGRATIONS.iter().zip(pinned) {
            assert_eq!(migration.name, name);
            assert_eq!(checksum(migration.sql), sha, "{name} changed");
        }
    }

    /// Gate fix (low): the WebhookDO's and OrgDO's files are pinned too.
    #[test]
    fn webhook_and_org_object_files_are_pinned_by_sha256() {
        let webhook = [
            (
                "0001_delivery_log.sql",
                "fe76da5e3f2d39f11dd0aebcbc618d62e430180d456d0c608325520b22dd326e",
            ),
            (
                "0002_lifecycle.sql",
                "d0ba822b24a810d9d67ef0199b24a14cb7907e8fc9d4d9bc5c3b0bb5a427ceb9",
            ),
        ];
        let org = [(
            "0001_quotas.sql",
            "da0ea3e2caa4d4065d7f3a1dcd49c85d3f89eec1eda853f5d5e7a068249baab6",
        )];
        for (migrations, pinned) in [
            (crate::webhook::WEBHOOK_MIGRATIONS, &webhook[..]),
            (crate::org::ORG_MIGRATIONS, &org[..]),
        ] {
            assert_eq!(migrations.len(), pinned.len(), "pin every migration");
            for (migration, (name, sha)) in migrations.iter().zip(pinned) {
                assert_eq!(migration.name, *name);
                assert_eq!(checksum(migration.sql), *sha, "{name} changed");
                assert_eq!(check_sql(migration.sql), Ok(()), "{name}");
            }
        }
    }

    /// A CRLF checkout would hash differently and be refused as a changed
    /// history; refuse CR outright instead, with its own code.
    #[test]
    fn migrations_are_lf_only() {
        for migration in MIGRATIONS {
            assert_eq!(check_sql(migration.sql), Ok(()), "{}", migration.name);
        }
        assert_eq!(
            check_sql("CREATE TABLE t (x INTEGER);\r\n"),
            Err(StoreError::Backend("do.migration_crlf"))
        );
    }

    #[test]
    fn versions_are_positional_and_names_are_numbered() {
        for (index, migration) in MIGRATIONS.iter().enumerate() {
            let version = i64::try_from(index).unwrap() + 1;
            assert_eq!(migration.version, version);
            assert!(migration.name.starts_with(&format!("{version:04}_")));
            assert!(migration.name.ends_with(".sql"));
        }
    }
}
