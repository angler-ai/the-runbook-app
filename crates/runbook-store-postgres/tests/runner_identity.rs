mod support;

use futures_util::FutureExt;
use runbook_core::runners::{NewInstall, RunnerStore, token_hash};
use runbook_store_postgres::{PostgresRunners, PostgresStore};
use std::panic::AssertUnwindSafe;
use support::database::TestDatabase;

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only a test-owned database"]
async fn runner_identity_enrollment_and_rotation_are_audited() {
    let mut db = TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(async {
        let mut store = PostgresStore::connect(db.config()).await.unwrap();
        store.migrate().await.unwrap();
        let runners = PostgresRunners::new(db.config().clone());
        let url = "https://runner.example";
        let org = "org_00000000000000000000000001".parse().unwrap();
        let owner = "usr_00000000000000000000000001".parse().unwrap();
        let id = "ri_00000000000000000000000001".parse().unwrap();
        let bootstrap = token_hash("bootstrap");
        runners.create_bootstrap(&org, url, &bootstrap, 1).await.unwrap();
        let install = runners.register(&bootstrap, NewInstall {
            id,
            url: url.to_owned(),
            version: "1".to_owned(),
            contract_version: 3,
            directory: token_hash("directory"),
            release: token_hash("release"),
            join: token_hash("join"),
            vault_identity_key: Some("initial-public-key".to_owned()),
        }, 2).await.unwrap();
        runners.set_vault_identity(&org, &owner, "rotated-public-key", 3).await.unwrap();
        let events = store.client().query(
            "SELECT action, actor, public_key FROM runbook.runner_vault_identity_audit WHERE install_id = $1 ORDER BY sequence",
            &[&install.id.as_str()],
        ).await.unwrap();
        let events: Vec<(String, String, String)> = events.into_iter()
            .map(|row| (row.get(0), row.get(1), row.get(2))).collect();
        assert_eq!(events, [
            ("runner.vault_identity_enrolled".into(), "installer bootstrap".into(), "initial-public-key".into()),
            ("runner.vault_identity_changed".into(), owner.to_string(), "rotated-public-key".into()),
        ]);
    }).catch_unwind().await;
    db.finish(result).await;
}
