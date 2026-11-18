#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
use futures_util::FutureExt;
use runbook_core::directory::*;
use runbook_model::{OrganizationId, UserId};
use runbook_store_postgres::{OwnerAssignment, OwnerRecoveryFault, PostgresDirectory};
use std::panic::AssertUnwindSafe;
fn org(n: u8) -> OrganizationId {
    format!("org_{n:026}").parse().unwrap()
}
fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn assign_owner_only_when_ownerless() {
    let mut db = database::TestDatabase::create_from_env().await.unwrap();
    let result = AssertUnwindSafe(recovery_scenario(&db))
        .catch_unwind()
        .await;
    db.finish(result).await;
}
async fn recovery_scenario(db: &database::TestDatabase) {
    let mut store = runbook_store_postgres::PostgresStore::connect(db.config())
        .await
        .unwrap();
    store.migrate().await.unwrap();
    let d = PostgresDirectory::new(db.config().clone());
    for n in [1, 2, 3] {
        let users: Vec<_> = match n {
            1 => vec![1, 3],
            2 => vec![2, 4],
            _ => vec![n],
        };
        let mut mutations = vec![DirectoryMutation::CreateOrganization {
            title: format!("Org {n}"),
        }];
        mutations.extend(users.into_iter().map(|u| {
            DirectoryMutation::CreateUser(UserRecord {
                id: user(u),
                name: format!("User {u}"),
                active: n != 3,
                role: if u == 1 {
                    OrgRole::Owner
                } else {
                    OrgRole::Member
                },
            })
        }));
        d.commit(DirectoryCommit {
            org_id: org(n),
            expected_revision: 0,
            mutations,
        })
        .await
        .unwrap();
    }
    assert!(
        d.assign_owner(&org(1), &user(1), OwnerRecoveryFault::None)
            .await
            .is_err()
    );
    assert!(
        d.assign_owner(&org(2), &user(1), OwnerRecoveryFault::None)
            .await
            .is_err()
    );
    assert!(
        d.assign_owner(&org(3), &user(3), OwnerRecoveryFault::None)
            .await
            .is_err()
    );
    let before = d.snapshot(&org(2)).await.unwrap();
    for fault in [
        OwnerRecoveryFault::BeforeAudit,
        OwnerRecoveryFault::AfterAudit,
    ] {
        assert!(d.assign_owner(&org(2), &user(2), fault).await.is_err());
        assert_eq!(d.snapshot(&org(2)).await.unwrap(), before);
        assert_eq!(
            store
                .client()
                .query_one("SELECT count(*) FROM runbook.owner_recovery_audit", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
    let (a, b) = tokio::join!(
        async {
            d.assign_owner(&org(2), &user(2), OwnerRecoveryFault::None)
                .await
        },
        async {
            d.assign_owner(&org(2), &user(4), OwnerRecoveryFault::None)
                .await
        }
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let winner = if a.is_ok() { user(2) } else { user(4) };
    let s = d.snapshot(&org(2)).await.unwrap();
    assert_eq!(
        s.users
            .values()
            .filter(|u| u.active && u.role == OrgRole::Owner)
            .count(),
        1
    );
    assert_eq!(s.revision, before.revision + 1);
    assert_eq!(
        d.assign_owner(&org(2), &winner, OwnerRecoveryFault::None)
            .await
            .unwrap(),
        OwnerAssignment::AlreadyAssigned
    );
    assert_eq!(d.snapshot(&org(2)).await.unwrap(), s);
    let row=store.client().query_one("SELECT org_id,user_id,revision,from_role,to_role,actor,database_role,occurred_at IS NOT NULL FROM runbook.owner_recovery_audit",&[]).await.unwrap();
    assert_eq!(row.get::<_, &str>(0), org(2).as_str());
    assert_eq!(row.get::<_, &str>(1), winner.as_str());
    assert_eq!(row.get::<_, i64>(2), s.revision as i64);
    assert_eq!(row.get::<_, &str>(3), "member");
    assert_eq!(row.get::<_, &str>(4), "owner");
    assert_eq!(row.get::<_, &str>(5), "local_cli");
    assert!(!row.get::<_, &str>(6).is_empty());
    assert!(row.get::<_, bool>(7));
    let config = runbook_server::config::ServerConfig::new(
        db.config().clone(),
        "https://local.example",
        "127.0.0.1:0",
    )
    .unwrap();
    assert_eq!(
        runbook_server::owner_recovery::run(&config, &org(2), &winner)
            .await
            .unwrap(),
        OwnerAssignment::AlreadyAssigned
    );
}
