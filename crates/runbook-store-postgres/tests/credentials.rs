//! B7a storage/core: credential lifetimes, atomic audit, reuse revocation.
mod support;
use runbook_core::start::StartStore;
use runbook_core::{
    ExecutionStore, WriteStage,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord},
    credentials::{
        CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind, secret_hash,
    },
    directory::{
        DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, TeamRecord, UserRecord,
    },
    execution_credentials::{
        ExecutionCredentialError, ExecutionCredentialStore, MintEntropy, mint_execution_pair,
        refresh_execution_pair, verify_execution_token,
    },
    publish::{prepare, publish_draft},
    start::{StartCommand, prepare_start_commit},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{ActorRef, Data, ExecutionStatus, MintTokenRequest, RefreshRequest, TokenId};
use runbook_store_conformance::{StoreFactory, scope};
use runbook_store_postgres::{PostgresCredentials, PostgresDirectory, PostgresExecutionStore};
use support::factory::{PostgresFactory, with_factory};

const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: cred-check, title: Cred check}\n\
    spec:\n  execution: {timeout: 2h, concurrency: {key: api, limit: 1, on_conflict: queue}}\n  \
    steps: [{id: only, title: Only}]\n";
fn rb() -> runbook_model::RunbookId {
    "rb_00000000000000000000000001".parse().unwrap()
}
fn exe() -> runbook_model::ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
fn tok(n: u8) -> TokenId {
    format!("tok_{n:026}").parse().unwrap()
}
fn entropy(n: u8) -> MintEntropy {
    MintEntropy {
        family_id: format!("fam-{n}"),
        access_token_id: tok(n * 2),
        refresh_token_id: tok(n * 2 + 1),
        access_random: [n; 32],
        refresh_random: [n.wrapping_add(50); 32],
    }
}
fn issuer_record() -> CredentialRecord {
    CredentialRecord {
        id: tok(200),
        kind: TokenKind::Service,
        hash: secret_hash("trb_svc_launcher-fixture"),
        principal: CredentialPrincipal::Service {
            project_id: scope(1).project_id,
            role: runbook_model::ProjectRole::Operator,
        },
        org_id: scope(1).org_id,
        scopes: [Scope::ExecutionsAct].into(),
        projects: None,
        expires_at: None,
        revoked_at: None,
        last_used_at: None,
    }
}
async fn setup(
    f: &PostgresFactory,
) -> (
    PostgresExecutionStore,
    PostgresDirectory,
    PostgresCredentials,
) {
    let store = f.fresh_store(scope(1)).await;
    let directory = f.fresh_directory();
    let mut mutations = vec![
        DirectoryMutation::CreateOrganization {
            title: "Org".into(),
        },
        DirectoryMutation::CreateProject(ProjectRecord {
            id: scope(1).project_id,
            org_id: scope(1).org_id,
            slug: "api".into(),
            title: "API".into(),
        }),
        DirectoryMutation::CreateUser(UserRecord {
            id: "usr_00000000000000000000000001".parse().unwrap(),
            name: "alice".into(),
            active: true,
            role: runbook_core::directory::OrgRole::Member,
        }),
        DirectoryMutation::GrantProjectRole {
            project_id: scope(1).project_id,
            user_id: "usr_00000000000000000000000001".parse().unwrap(),
            role: runbook_model::ProjectRole::Operator,
        },
        DirectoryMutation::CreateUser(UserRecord {
            id: "usr_00000000000000000000000002".parse().unwrap(),
            name: "viewer".into(),
            active: true,
            role: runbook_core::directory::OrgRole::Member,
        }),
        DirectoryMutation::GrantProjectRole {
            project_id: scope(1).project_id,
            user_id: "usr_00000000000000000000000002".parse().unwrap(),
            role: runbook_model::ProjectRole::Viewer,
        },
    ];
    mutations.push(DirectoryMutation::CreateTeam(TeamRecord {
        id: "team_00000000000000000000000001".parse().unwrap(),
        org_id: scope(1).org_id,
        slug: "oncall".into(),
        title: "Oncall".into(),
    }));
    directory
        .commit(DirectoryCommit {
            org_id: scope(1).org_id,
            expected_revision: 0,
            mutations,
        })
        .await
        .unwrap();
    let credentials = PostgresCredentials::new(f.config.clone());
    credentials.insert(issuer_record()).await.unwrap();
    store
        .create_runbook(
            RunbookRecord {
                runbook_id: rb(),
                slug: "cred-check".into(),
                origin: RunbookOrigin::Ui,
                archived: false,
            },
            SPEC.into(),
        )
        .await
        .unwrap();
    let prepared = prepare(
        "cred-check",
        SPEC,
        &runbook_spec::CompileContext::default(),
        "rbv_00000000000000000000000001".parse().unwrap(),
    )
    .unwrap();
    publish_draft(&store, &store, &rb(), 1, prepared)
        .await
        .unwrap();
    let command = StartCommand {
        execution_id: exe(),
        runbook_id: rb(),
        version: None,
        inputs: Data::new(),
        fields: Data::new(),
        project_vars: Data::new(),
        title: None,
        tags: vec![],
        url: String::new(),
        created_by: "alice".into(),
        limits: Default::default(),
        idempotency_key: "start-1".into(),
        principal: "user:alice".into(),
        request_hash: [1; 32],
    };
    let commit = prepare_start_commit(&store, &store, &command)
        .await
        .unwrap();
    store.commit_start(commit).await.unwrap();
    (store, directory, credentials)
}
async fn activate(store: &PostgresExecutionStore, directory: &PostgresDirectory) {
    let claims = store.claim_due(10, 60_000).await.unwrap();
    let lease = claims
        .iter()
        .find(|lease| lease.id.kind == WorkKind::Activation)
        .expect("activation work");
    runbook_core::start::activate(store, directory, scope(1), lease)
        .await
        .unwrap();
}
fn mint_req() -> MintTokenRequest {
    MintTokenRequest {
        session_id: "sess-1".into(),
    }
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn credential_lifetimes_and_atomic_audit() {
    with_factory(async |f| {
        let (store, directory, credentials) = setup(f).await;
        let issuer = issuer_record();
        // Queued (unactivated) runs cannot mint (D31).
        assert!(matches!(
            mint_execution_pair(
                &store,
                &credentials,
                &directory,
                &issuer,
                &exe(),
                &mint_req(),
                "m1",
                [2; 32],
                entropy(1)
            )
            .await,
            Err(ExecutionCredentialError::Rejected("execution.not_started"))
        ));
        activate(&store, &directory).await;
        // An injected failure changes neither credentials nor events.
        let revision = store.load(&exe()).await?.revision;
        f.fail_after(&store, WriteStage::Credentials).await?;
        assert!(
            mint_execution_pair(
                &store,
                &credentials,
                &directory,
                &issuer,
                &exe(),
                &mint_req(),
                "m1",
                [2; 32],
                entropy(1)
            )
            .await
            .is_err()
        );
        assert_eq!(store.load(&exe()).await?.revision, revision);
        assert_eq!(store.credential_family("fam-1").await?, None);
        let pair = mint_execution_pair(
            &store,
            &credentials,
            &directory,
            &issuer,
            &exe(),
            &mint_req(),
            "m1",
            [2; 32],
            entropy(1),
        )
        .await
        .unwrap();
        // One-hour access and start-plus-30-day refresh, exactly.
        let access = store
            .credential_by_hash(&secret_hash(&pair.access.secret))
            .await?
            .unwrap();
        assert_eq!(access.expires_at_ms - access.issued_at_ms, 3_600_000);
        let started_at = store.load(&exe()).await?.events[0].at_ms;
        assert_eq!(pair.refresh.expires_at_ms, started_at + 30 * 24 * 3_600_000);
        let now = access.issued_at_ms;
        verify_execution_token(&store, &credentials, &directory, &pair.access.secret, now)
            .await
            .unwrap();
        assert!(
            verify_execution_token(
                &store,
                &credentials,
                &directory,
                &pair.access.secret,
                access.expires_at_ms,
            )
            .await
            .is_err()
        );
        // Repeat mint keys: already issued, or reused with a new payload.
        assert!(matches!(
            mint_execution_pair(
                &store,
                &credentials,
                &directory,
                &issuer,
                &exe(),
                &mint_req(),
                "m1",
                [2; 32],
                entropy(2)
            )
            .await,
            Err(ExecutionCredentialError::Rejected(
                "credentials.already_issued"
            ))
        ));
        assert!(matches!(
            mint_execution_pair(
                &store,
                &credentials,
                &directory,
                &issuer,
                &exe(),
                &mint_req(),
                "m1",
                [9; 32],
                entropy(2)
            )
            .await,
            Err(ExecutionCredentialError::Rejected(
                "request.idempotency_key_reused"
            ))
        ));
        // Secrets appear in no stored row or event body.
        for table in [
            "credential_family",
            "execution_credential",
            "execution_event",
        ] {
            let bodies: Vec<String> = f
                .admin
                .client()
                .query(&format!("SELECT body FROM runbook.{table}"), &[])
                .await
                .unwrap()
                .iter()
                .map(|row| row.get(0))
                .collect();
            for body in bodies {
                assert!(!body.contains(&pair.access.secret), "{table}");
                assert!(!body.contains(&pair.refresh.secret), "{table}");
            }
        }
        // Terminal closure revokes the family immediately.
        let run = store.load(&exe()).await?;
        let mut projection = run.projection.clone();
        projection.sequence = run.revision + 1;
        projection.status = ExecutionStatus::Succeeded;
        projection.completed_at_ms = Some(now + 10);
        store
            .commit(runbook_core::ExecutionCommit {
                execution_id: exe(),
                expected_sequence: run.revision,
                events: vec![runbook_engine::EventEnvelope {
                    execution_id: exe(),
                    seq: run.revision + 1,
                    at_ms: now + 10,
                    actor: ActorRef::System,
                    event: runbook_engine::Event::ManualPauseChanged { paused: false },
                }],
                projection,
                timers: vec![],
                outbox: vec![],
                idempotency: vec![],
                claims: vec![],
                credentials: vec![],
                derived_outbox: vec![],
            })
            .await?;
        assert!(
            verify_execution_token(
                &store,
                &credentials,
                &directory,
                &pair.access.secret,
                now + 11
            )
            .await
            .is_err()
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn refresh_reuse_revokes_successor() {
    with_factory(async |f| {
        let (store, directory, credentials) = setup(f).await;
        activate(&store, &directory).await;
        let issuer = issuer_record();
        let pair = mint_execution_pair(
            &store,
            &credentials,
            &directory,
            &issuer,
            &exe(),
            &mint_req(),
            "m1",
            [2; 32],
            entropy(1),
        )
        .await
        .unwrap();
        // Race two refreshes of one token on independent pools: exactly one
        // pair is issued, the loser is told the token was reused (not some
        // unrelated failure), and the family ends revoked once reuse commits.
        let request = RefreshRequest {
            refresh_token: pair.refresh.secret.clone(),
        };
        let rival = f.fresh_store(scope(1)).await;
        let (a, b) = tokio::join!(
            refresh_execution_pair(&store, &credentials, &directory, &request, entropy(2)),
            refresh_execution_pair(&rival, &credentials, &directory, &request, entropy(3))
        );
        let (issued, refused): (Vec<_>, Vec<_>) = [a, b].into_iter().partition(Result::is_ok);
        let issued: Vec<_> = issued.into_iter().map(Result::unwrap).collect();
        assert_eq!(issued.len(), 1, "exactly one refresh wins: {refused:?}");
        assert_eq!(refused.len(), 1);
        assert!(
            matches!(
                refused[0],
                Err(ExecutionCredentialError::Rejected(
                    "credentials.refresh_reused"
                ))
            ),
            "the loser must see reuse: {refused:?}"
        );
        assert_eq!(issued[0].generation, pair.generation + 1);
        let family = store.credential_family("fam-1").await?.unwrap();
        assert!(
            family.revoked_at_ms.is_some(),
            "reuse must revoke the family"
        );
        for successor in &issued {
            assert!(
                verify_execution_token(
                    &store,
                    &credentials,
                    &directory,
                    &successor.access.secret,
                    successor.access.expires_at_ms - 1,
                )
                .await
                .is_err()
            );
        }
        Ok(())
    })
    .await;
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn lost_refresh_response_requires_launcher() {
    with_factory(async |f| {
        let (store, directory, credentials) = setup(f).await;
        activate(&store, &directory).await;
        let issuer = issuer_record();
        let pair = mint_execution_pair(
            &store,
            &credentials,
            &directory,
            &issuer,
            &exe(),
            &mint_req(),
            "m1",
            [2; 32],
            entropy(1),
        )
        .await
        .unwrap();
        let request = RefreshRequest {
            refresh_token: pair.refresh.secret.clone(),
        };
        // The rotated pair is discarded (lost response); retrying the old
        // token revokes it — there is no credential recovery path.
        let _discarded =
            refresh_execution_pair(&store, &credentials, &directory, &request, entropy(2))
                .await
                .unwrap();
        assert!(matches!(
            refresh_execution_pair(&store, &credentials, &directory, &request, entropy(3)).await,
            Err(ExecutionCredentialError::Rejected(
                "credentials.refresh_reused"
            ))
        ));
        assert!(
            verify_execution_token(
                &store,
                &credentials,
                &directory,
                &_discarded.access.secret,
                _discarded.access.expires_at_ms - 1,
            )
            .await
            .is_err()
        );
        // A viewer-role issuer cannot launcher-mint; the launcher can.
        let mut viewer = issuer_record();
        viewer.id = tok(201);
        viewer.hash = secret_hash("trb_svc_viewer-fixture");
        viewer.principal = runbook_core::credentials::CredentialPrincipal::Service {
            project_id: scope(1).project_id,
            role: runbook_model::ProjectRole::Viewer,
        };
        credentials.insert(viewer.clone()).await.unwrap();
        assert!(
            mint_execution_pair(
                &store,
                &credentials,
                &directory,
                &viewer,
                &exe(),
                &mint_req(),
                "m2",
                [3; 32],
                entropy(4)
            )
            .await
            .is_err()
        );
        let fresh = mint_execution_pair(
            &store,
            &credentials,
            &directory,
            &issuer,
            &exe(),
            &mint_req(),
            "m2",
            [3; 32],
            entropy(4),
        )
        .await
        .unwrap();
        verify_execution_token(
            &store,
            &credentials,
            &directory,
            &fresh.access.secret,
            fresh.access.expires_at_ms - 1,
        )
        .await
        .unwrap();
        Ok(())
    })
    .await;
}

/// F33: a commit's credential writes are bound to its own run and tenant.
/// A run cannot consume, revoke or issue credentials of a sibling run.
#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn credential_writes_are_bound_to_the_commits_run() {
    use runbook_core::{
        ExecutionCommit, StoreError,
        execution_credentials::{CredentialFamily, CredentialWrite, ExecutionCredentialRecord},
    };
    use runbook_store_conformance::catalog::{batch, id};
    with_factory(async |f| {
        let store = f
            .create(runbook_store_conformance::execution::projection())
            .await?;
        store
            .commit(batch(2, 0, 1, ExecutionStatus::Running))
            .await?;
        let family = |run: u8, n: u8| {
            Box::new(CredentialFamily {
                family_id: format!("fam-{n}"),
                execution_id: id(run),
                org_id: scope(1).org_id,
                project_id: scope(1).project_id,
                issuer_token_id: tok(200),
                issuer: issuer_record().principal,
                session_id: "sess-1".into(),
                idempotency_key: format!("mint-{n}"),
                request_hash: [n; 32],
                created_at_ms: 1,
                revoked_at_ms: None,
            })
        };
        let record = |run: u8, n: u8, kind: TokenKind, token: u8| {
            Box::new(ExecutionCredentialRecord {
                token_id: tok(token),
                kind,
                hash: secret_hash(&format!("trb_exr_fixture-{token}")),
                family_id: format!("fam-{n}"),
                execution_id: id(run),
                org_id: scope(1).org_id,
                project_id: scope(1).project_id,
                generation: 1,
                issued_at_ms: 1,
                expires_at_ms: 4_000_000_000_000,
                consumed_at_ms: None,
                revoked_at_ms: None,
            })
        };
        let with = |mut commit: ExecutionCommit, write: CredentialWrite| {
            commit.credentials = vec![write];
            commit
        };
        let rotate = |run: u8, consume: u8, tokens: [u8; 2]| CredentialWrite::Rotate {
            consume_token_id: tok(consume),
            consumed_at_ms: 2,
            access: record(run, 1, TokenKind::Execution, tokens[0]),
            refresh: record(run, 1, TokenKind::Refresh, tokens[1]),
        };
        // Run 2 holds family fam-1 with refresh token 11.
        store
            .commit(with(
                batch(2, 1, 2, ExecutionStatus::Running),
                CredentialWrite::IssueFamily {
                    family: family(2, 1),
                    access: record(2, 1, TokenKind::Execution, 10),
                    refresh: record(2, 1, TokenKind::Refresh, 11),
                },
            ))
            .await?;
        let run_one = || batch(1, 0, 1, ExecutionStatus::Running);
        // Run 1 cannot rotate run 2's token, whichever run its new rows name.
        assert_eq!(
            store.commit(with(run_one(), rotate(1, 11, [12, 13]))).await,
            Err(StoreError::NotFound)
        );
        assert!(
            store
                .commit(with(run_one(), rotate(2, 11, [12, 13])))
                .await
                .is_err()
        );
        // Nor revoke run 2's family, nor issue a family for run 2.
        for execution_id in [id(1), id(2)] {
            assert!(
                store
                    .commit(with(
                        run_one(),
                        CredentialWrite::RevokeFamily {
                            execution_id,
                            family_id: "fam-1".into(),
                            revoked_at_ms: 2,
                        },
                    ))
                    .await
                    .is_err()
            );
        }
        assert!(
            store
                .commit(with(
                    run_one(),
                    CredentialWrite::IssueFamily {
                        family: family(2, 2),
                        access: record(2, 2, TokenKind::Execution, 20),
                        refresh: record(2, 2, TokenKind::Refresh, 21),
                    },
                ))
                .await
                .is_err()
        );
        // Run 2's credentials are untouched and still rotate on run 2.
        let token = store.credential_by_id(&tok(11)).await?.unwrap();
        assert_eq!((token.consumed_at_ms, token.revoked_at_ms), (None, None));
        assert!(
            store
                .credential_family("fam-1")
                .await?
                .unwrap()
                .revoked_at_ms
                .is_none()
        );
        assert!(store.credential_by_id(&tok(12)).await?.is_none());
        assert_eq!(
            store
                .commit(with(
                    batch(2, 2, 3, ExecutionStatus::Running),
                    rotate(2, 11, [14, 15]),
                ))
                .await?,
            runbook_core::CommitResult::Committed(3)
        );
        Ok(())
    })
    .await;
}
