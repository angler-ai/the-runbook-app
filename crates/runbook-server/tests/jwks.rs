//! R0-1 on Postgres: the signing keys are created sealed on first use,
//! published at `/.well-known/jwks.json`, and a rotated key stays listed
//! (and verifies) for 24 hours.
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
mod database;
#[path = "support/runner_fixture.rs"]
mod runner_fixture;
#[path = "../../runbook-store-conformance/src/signing.rs"]
mod signing_conformance;
use runbook_core::signing::{Jwks, KeyPurpose, RETIRED_KEY_TTL_S};
use runbook_server::signing::ServerSigner;
use runner_fixture::{with_fixture, with_fixture_config};
use serde_json::Value;

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn jwks_route_lists_current_and_previous_keys() {
    with_fixture(async |f| {
        let signer = ServerSigner::new(&f.config.database, &f.config.webhooks);
        let sql = f.sql().await;
        signing_conformance::rotation_keeps_the_previous_key_until_grace_expires(
            async || signer.ring().await.unwrap(),
            async || {
                let response = f.request("GET", "/.well-known/jwks.json", None, &[], Value::Null).await;
                assert_eq!(response.status, 200, "{}", response.body);
                serde_json::from_value::<Jwks>(response.body).unwrap()
            },
            async || signer.rotate(KeyPurpose::Grant).await.unwrap(),
            async |old: &str| {
                sql.client()
                    .execute(
                        "UPDATE runbook.signing_keys SET retired_at_ms = retired_at_ms - $1 WHERE kid = $2",
                        &[&(RETIRED_KEY_TTL_S * 1000 + 1000), &old],
                    )
                    .await
                    .unwrap();
            },
        )
        .await;

        // Private keys are stored sealed, never as their raw seed.
        let rows = sql
            .client()
            .query("SELECT sealed_private FROM runbook.signing_keys", &[])
            .await
            .unwrap();
        for row in rows {
            let sealed: String = row.get(0);
            assert!(sealed.starts_with("v2."), "sealed with the AEAD sealer");
        }
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn jwks_is_empty_without_a_secret_key() {
    with_fixture_config(
        |config| config.webhooks.secret_key = None,
        async |f| {
            let jwks = f
                .request("GET", "/.well-known/jwks.json", None, &[], Value::Null)
                .await;
            assert_eq!(jwks.status, 200);
            assert_eq!(jwks.body["keys"], serde_json::json!([]));
        },
    )
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; owned databases only"]
async fn jwks_omits_a_database_public_key_that_does_not_match_its_seed() {
    with_fixture(async |f| {
        let _ = f
            .request("GET", "/.well-known/jwks.json", None, &[], Value::Null)
            .await;
        let sql = f.sql().await;
        sql.client()
            .execute(
                "UPDATE runbook.signing_keys SET public = $1 WHERE purpose = 'grant'",
                &[&&[7u8; 32][..]],
            )
            .await
            .unwrap();
        let jwks = f
            .request("GET", "/.well-known/jwks.json", None, &[], Value::Null)
            .await;
        assert_eq!(jwks.status, 200);
        let published = jwks.body["keys"].as_array().unwrap();
        assert_eq!(published.len(), 1);
        assert!(published[0]["kid"].as_str().unwrap().starts_with("user-"));
    })
    .await;
}
