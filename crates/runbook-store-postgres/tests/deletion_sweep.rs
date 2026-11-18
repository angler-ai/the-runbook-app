//! Store hardening: run deletion leaves no orphan rows. A run is driven
//! through every run-owned table (start, pooled admission, activation,
//! leases, claim with replay, logs, credentials, evidence blob), finished
//! and deleted. Tables come from the schema catalog, not a hand list, and
//! a row counts as the run's if its text mentions the run's ID, so a new
//! table or an indirect reference cannot slip past the sweep.
mod support;
use runbook_core::{
    actor::{Replay, begin_actor},
    blob::{BlobKey, BlobStore},
    catalog::{AdmissionState, CatalogStore},
    claims::{append_logs, claim_step},
    control::cancel,
    credentials::{
        CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind, secret_hash,
    },
    execution_credentials::{MintEntropy, mint_execution_pair},
    retention::RunStore,
    work_queue::WorkQueue,
};
use runbook_model::{ClaimRequest, LogsRequest, MintTokenRequest, TokenId};
use runbook_store_conformance::{
    parity,
    portable::{self, agent, exe, human, p},
};
use runbook_store_postgres::PostgresCredentials;
use std::collections::{BTreeMap, BTreeSet};
use support::factory::{PostgresFactory, with_factory};

const SWEEP: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: sweep, title: Sweep}\n\
    spec:\n  execution: {timeout: 1h, concurrency: {key: api, limit: 1}}\n  \
    steps: [{id: only, title: Only}]\n";

/// Per table, the rows whose text mentions `needle`, for every table in
/// the schema except the migration ledger.
async fn mentions(f: &PostgresFactory, needle: &str) -> BTreeMap<String, i64> {
    let client = f.admin.client();
    let tables: Vec<String> = client
        .query(
            "SELECT table_name::text FROM information_schema.tables \
             WHERE table_schema = 'runbook' AND table_type = 'BASE TABLE' \
             AND table_name <> 'schema_migration' ORDER BY 1",
            &[],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|r| r.get(0))
        .collect();
    assert!(tables.len() > 40, "schema catalog read: {tables:?}");
    let mut out = BTreeMap::new();
    for table in tables {
        let n: i64 = client
            .query_one(
                &format!("SELECT count(*) FROM runbook.{table} t WHERE strpos(t::text, $1) > 0"),
                &[&needle],
            )
            .await
            .unwrap()
            .get(0);
        if n > 0 {
            out.insert(table, n);
        }
    }
    out
}
/// Blob chunks whose blob row is gone.
async fn chunks(f: &PostgresFactory) -> i64 {
    f.admin
        .client()
        .query_one(
            "SELECT count(*) FROM runbook.blob_chunk c \
             LEFT JOIN runbook.blob b \
             ON b.org_id = c.org_id AND b.project_id = c.project_id AND b.key = c.key \
             WHERE b.key IS NULL",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}
fn tok(n: u8) -> TokenId {
    format!("tok_{n:026}").parse().unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn deleted_run_leaves_only_its_tombstone() {
    with_factory(async |f| {
        let store = f.fresh_store(p()).await;
        let directory = f.fresh_directory();
        portable::directory(&directory).await;
        parity::publish(&store, 1, "sweep", SWEEP, 1).await;
        portable::start(&store, &directory, &p(), 1, 1).await;
        portable::start(&store, &directory, &p(), 2, 1).await;
        assert_eq!(
            store.admission(&exe(2)).await?.unwrap().state,
            AdmissionState::Queued
        );

        // Claim with replay, stream logs, mint credentials, upload evidence.
        let work = begin_actor(&store, &directory, p(), &exe(1)).await.unwrap();
        let step = work
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "only")
            .and_then(|s| s.id.clone())
            .unwrap();
        let claim = claim_step(
            work,
            &agent(),
            &step,
            &ClaimRequest {
                take_over: false,
                session_id: "s1".into(),
                actor: None,
            },
            "clm_00000000000000000000000001".parse().unwrap(),
            Some(&Replay {
                key: "claim-1".into(),
                principal: agent().principal,
                request_hash: [1; 32],
            }),
        )
        .await
        .unwrap();
        append_logs(
            &store,
            &agent(),
            &exe(1),
            &step,
            &LogsRequest {
                claim,
                logs: vec!["checking".into()],
            },
            None,
        )
        .await
        .unwrap();
        let credentials = PostgresCredentials::new(f.config.clone());
        let issuer = CredentialRecord {
            id: tok(200),
            kind: TokenKind::Service,
            hash: secret_hash("trb_svc_sweep-launcher"),
            principal: CredentialPrincipal::Service {
                project_id: p().project_id,
                role: runbook_model::ProjectRole::Operator,
            },
            org_id: p().org_id,
            scopes: [Scope::ExecutionsAct].into(),
            projects: None,
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        };
        credentials.insert(issuer.clone()).await.unwrap();
        mint_execution_pair(
            &store,
            &credentials,
            &directory,
            &issuer,
            &exe(1),
            &MintTokenRequest {
                session_id: "sess-1".into(),
            },
            "mint-1",
            [2; 32],
            MintEntropy {
                family_id: "fam-sweep".into(),
                access_token_id: tok(2),
                refresh_token_id: tok(3),
                access_random: [2; 32],
                refresh_random: [3; 32],
            },
        )
        .await
        .unwrap();
        let big = vec![7u8; runbook_core::blob::BLOB_CHUNK_BYTES + 10];
        store
            .put(
                BlobKey::Execution {
                    execution_id: exe(1),
                    name: "evidence/1".into(),
                },
                &big,
            )
            .await?;
        // Lease whatever is due (notifications) so work_lease holds rows.
        store.claim_due(16, 60_000).await?;

        // Finish the run; the waiter is promoted.
        let work = begin_actor(&store, &directory, p(), &exe(1)).await.unwrap();
        cancel(&store, work, &human(4), None).await.unwrap();
        assert_eq!(
            store.admission(&exe(2)).await?.unwrap().state,
            AdmissionState::Admitted
        );

        let target = exe(1).to_string();
        let before = mentions(f, &target).await;
        let populated: BTreeSet<&str> = before.keys().map(String::as_str).collect();
        let expected: BTreeSet<&str> = [
            "actor_replay",
            "admission",
            "blob",
            "claim_log",
            "credential_family",
            "execution",
            "execution_claim",
            "execution_credential",
            "execution_derived_outbox",
            "execution_event",
            "execution_outbox",
            "execution_owner",
            "execution_start",
            "execution_timer_history",
            "run_summary",
            "start_replay",
            "step_run_index",
            "work_lease",
        ]
        .into();
        assert!(
            expected.is_subset(&populated),
            "fixture must reach every run-owned table: missing {:?}",
            expected.difference(&populated).collect::<Vec<_>>()
        );
        let sibling = mentions(f, exe(2).as_str()).await;
        assert_eq!(chunks(f).await, 0);
        let total_chunks: i64 = f
            .admin
            .client()
            .query_one("SELECT count(*) FROM runbook.blob_chunk", &[])
            .await
            .unwrap()
            .get(0);
        assert!(total_chunks >= 2, "the evidence blob spans two chunks");

        store
            .delete_execution(&exe(1), "retention window ended")
            .await?;

        let after = mentions(f, &target).await;
        assert_eq!(
            after,
            // The run's tombstone, plus the step-run tombstone that keeps its
            // log link answering 410 instead of 404 (F46, migration 0024).
            BTreeMap::from([
                ("step_run_tombstone".to_string(), 1),
                ("tombstone".to_string(), 1)
            ]),
            "rows that still mention the deleted run"
        );
        assert_eq!(chunks(f).await, 0, "orphan blob chunks");
        assert_eq!(
            mentions(f, exe(2).as_str()).await,
            sibling,
            "the sibling run is untouched"
        );
        // The issuer's own credential row is not run-owned and survives.
        assert!(credentials.find_by_id(&tok(200)).await.unwrap().is_some());
        Ok(())
    })
    .await;
}
