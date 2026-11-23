//! B9a storage: amending a completed run does not reopen it.
mod support;
use runbook_core::{
    ExecutionStore, StoreError,
    actor::{ActorPrincipal, begin_actor},
    amendments::override_step,
    catalog::{CatalogStore, RunbookOrigin, RunbookRecord},
    claims::{claim_step, report_step},
    credentials::{
        CredentialPrincipal, CredentialRecord, CredentialStore, Scope, TokenKind, secret_hash,
    },
    directory::{DirectoryCommit, DirectoryMutation, DirectoryStore, ProjectRecord, UserRecord},
    execution_credentials::{
        ExecutionCredentialStore, MintEntropy, mint_execution_pair, verify_execution_token,
    },
    publish::{prepare, publish_draft},
    start::{StartCommand, StartStore, activate, prepare_start_commit},
    work_queue::{WorkKind, WorkQueue},
};
use runbook_model::{
    ActorRef, ClaimRequest, Data, ExecutionStatus, MintTokenRequest, ProjectRole, ReportRequest,
    ReportStatus, StepRecord,
};
use runbook_store_conformance::scope;
use runbook_store_postgres::PostgresCredentials;
use support::factory::with_factory;

const SPEC: &str = "apiVersion: therunbook.app/v1\nkind: Runbook\n\
    metadata: {id: amend-check, title: Amend check}\n\
    spec: {steps: [{id: only, title: Only, outputs: {count: {type: integer}}}]}";
fn usr(n: u8) -> runbook_model::UserId {
    format!("usr_{n:026}").parse().unwrap()
}
fn human(n: u8) -> ActorPrincipal {
    ActorPrincipal {
        principal: format!("user:{}", usr(n)),
        actor: ActorRef::Human { user_id: usr(n) },
        credential_ref: "pat".into(),
    }
}
fn exe() -> runbook_model::ExecutionId {
    "exe_00000000000000000000000001".parse().unwrap()
}
fn rb() -> runbook_model::RunbookId {
    "rb_00000000000000000000000001".parse().unwrap()
}

#[tokio::test]
#[ignore = "requires DATABASE_URL; creates and drops only test-owned databases"]
async fn completed_amendment_does_not_reopen() {
    with_factory(async |f| {
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
        ];
        for (n, role) in [(1, ProjectRole::Operator), (3, ProjectRole::Admin)] {
            mutations.push(DirectoryMutation::CreateUser(UserRecord {
                id: usr(n),
                name: format!("user{n}"),
                active: true,
                role: runbook_core::directory::OrgRole::Member,
            }));
            mutations.push(DirectoryMutation::GrantProjectRole {
                project_id: scope(1).project_id,
                user_id: usr(n),
                role,
            });
        }
        directory
            .commit(DirectoryCommit {
                org_id: scope(1).org_id,
                expected_revision: 0,
                mutations,
            })
            .await
            .unwrap();
        let credentials = PostgresCredentials::new(f.config.clone());
        let issuer = CredentialRecord {
            id: "tok_00000000000000000000000200".parse().unwrap(),
            kind: TokenKind::Service,
            hash: secret_hash("trb_svc_launcher"),
            principal: CredentialPrincipal::Service {
                project_id: scope(1).project_id,
                role: ProjectRole::Operator,
            },
            org_id: scope(1).org_id,
            scopes: [Scope::ExecutionsAct].into(),
            projects: None,
            expires_at: None,
            revoked_at: None,
            last_used_at: None,
        };
        credentials.insert(issuer.clone()).await.unwrap();
        store
            .create_runbook(
                RunbookRecord {
                    runbook_id: rb(),
                    slug: "amend-check".into(),
                    origin: RunbookOrigin::Ui,
                    archived: false,
                },
                SPEC.into(),
            )
            .await?;
        let prepared = prepare(
            "amend-check",
            SPEC,
            &runbook_spec::CompileContext::default(),
            "rbv_00000000000000000000000001".parse().unwrap(),
        )
        .unwrap();
        publish_draft(&store, &store, &rb(), 1, prepared).await?;
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
        store.commit_start(commit).await?;
        let claims = store.claim_due(10, 60_000).await?;
        let activation = claims
            .iter()
            .find(|lease| lease.id.kind == WorkKind::Activation)
            .unwrap();
        activate(&store, &directory, scope(1), activation).await?;
        // Mint before completion so closure must revoke the family.
        let pair = mint_execution_pair(
            &store,
            &credentials,
            &directory,
            &issuer,
            &exe(),
            &MintTokenRequest {
                session_id: "s1".into(),
            },
            "m1",
            [2; 32],
            MintEntropy {
                family_id: "fam-1".into(),
                access_token_id: "tok_00000000000000000000000002".parse().unwrap(),
                refresh_token_id: "tok_00000000000000000000000003".parse().unwrap(),
                access_random: [7; 32],
                refresh_random: [8; 32],
            },
        )
        .await
        .unwrap();
        // Claim and complete the only step.
        let w = begin_actor(&store, &directory, scope(1), &exe())
            .await
            .unwrap();
        let only = w
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "only")
            .and_then(|s| s.id.clone())
            .unwrap();
        let claim = claim_step(
            w,
            &human(1),
            &only,
            &ClaimRequest {
                take_over: false,
                session_id: "web".into(),
                actor: None,
            },
            "clm_00000000000000000000000001".parse().unwrap(),
            None,
        )
        .await
        .unwrap();
        let w = begin_actor(&store, &directory, scope(1), &exe())
            .await
            .unwrap();
        report_step(
            w,
            &human(1),
            &ReportRequest {
                step_run_id: only.clone(),
                claim,
                status: ReportStatus::Succeeded,
                verdict: None,
                outputs: Data::from([("count".to_string(), serde_json::json!(1))]),
                summary: Some("done".into()),
                evidence: vec![],
                logs: vec![],
                cost: None,
            },
            None,
        )
        .await
        .unwrap();
        let closed = store.load(&exe()).await?;
        assert_eq!(closed.projection.status, ExecutionStatus::Succeeded);
        let completed_at = closed.projection.completed_at_ms.unwrap();
        // Non-admin cannot amend; nothing changes.
        let to = StepRecord {
            status: runbook_model::StepStatus::Succeeded,
            verdict: None,
            outputs: Data::from([("count".to_string(), serde_json::json!(2))]),
            summary: Some("corrected".into()),
        };
        let w = begin_actor(&store, &directory, scope(1), &exe())
            .await
            .unwrap();
        let revision_before = w.runtime.state.sequence;
        assert!(
            override_step(
                w,
                &human(1),
                &only,
                &runbook_model::OverrideRequest {
                    to: to.clone(),
                    reason: "wrong count".into(),
                },
                None,
            )
            .await
            .is_err()
        );
        // Admin amends at a later session time; closure facts stay frozen.
        let w = begin_actor(&store, &directory, scope(1), &exe())
            .await
            .unwrap();
        assert!(w.at_ms() > completed_at);
        let amendment = override_step(
            w,
            &human(3),
            &only,
            &runbook_model::OverrideRequest {
                to,
                reason: "wrong count".into(),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(amendment.from.outputs["count"], serde_json::json!(1));
        let after = store.load(&exe()).await?;
        assert!(after.revision > revision_before);
        assert_eq!(after.projection.status, ExecutionStatus::Succeeded);
        assert_eq!(after.projection.completed_at_ms, Some(completed_at));
        assert_eq!(after.projection.result, closed.projection.result);
        // Amended markers and the original record are in rebuilt state.
        let w = begin_actor(&store, &directory, scope(1), &exe())
            .await
            .unwrap();
        let step = w
            .runtime
            .state
            .steps
            .iter()
            .find(|s| s.step_id == "only")
            .unwrap();
        assert!(step.amended);
        assert_eq!(step.outputs["count"], serde_json::json!(2));
        assert_eq!(
            step.original.as_ref().unwrap().outputs["count"],
            serde_json::json!(1)
        );
        drop(w);
        // No slot release, activation, timer or credential resurrection.
        assert!(
            store.claim_due(10, 60_000).await?.iter().all(
                |lease| lease.id.execution_id != exe() || lease.id.kind != WorkKind::Activation
            )
        );
        let family = store.credential_family("fam-1").await?.unwrap();
        assert!(family.revoked_at_ms.is_some());
        assert!(
            verify_execution_token(
                &store,
                &credentials,
                &directory,
                &pair.access.secret,
                pair.access.expires_at_ms - 1,
            )
            .await
            .is_err()
        );
        let _ = StoreError::NotFound;
        Ok(())
    })
    .await;
}
