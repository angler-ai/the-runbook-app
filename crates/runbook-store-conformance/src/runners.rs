//! Contract cases shared by runner registry adapters.
use runbook_core::{
    agent_directory::AgentDirectory,
    credentials::secret_hash,
    runners::{NewInstall, NewRunner, RunnerError, RunnerPurpose, RunnerStore},
};
use runbook_model::{RunnerHeartbeatEntry, RunnerHeartbeatRequest, RunnerInstallStatus};

pub async fn registration_and_rejoin(
    store: &(impl RunnerStore + AgentDirectory),
    scope: runbook_core::catalog::TenantScope,
) {
    let org = &scope.org_id;
    let url = "https://runner.example";
    let bootstrap = secret_hash("bootstrap");
    let replaced = secret_hash("previous bootstrap");
    let owner = "usr_00000000000000000000000001".parse().unwrap();
    let install = NewInstall {
        id: "ri_00000000000000000000000001".parse().unwrap(),
        url: url.into(),
        version: "1.0.0".into(),
        contract_version: 2,
        vault_identity_key: Some("initial-key".into()),
        directory: secret_hash("directory"),
        release: secret_hash("release"),
        join: secret_hash("join"),
    };
    assert!(store.install_of(org).await.unwrap().is_none());
    assert!(matches!(
        store
            .set_vault_identity(org, &owner, "unregistered-key", 1)
            .await,
        Err(RunnerError::NotFound)
    ));
    store
        .create_bootstrap(org, url, &replaced, 1)
        .await
        .unwrap();
    store
        .create_bootstrap(org, url, &bootstrap, 2)
        .await
        .unwrap();
    assert!(matches!(
        store.register(&replaced, install.clone(), 3).await,
        Err(RunnerError::InvalidToken)
    ));
    let mut wrong_url = install.clone();
    wrong_url.url = "https://other.example".into();
    assert!(matches!(
        store.register(&bootstrap, wrong_url, 3).await,
        Err(RunnerError::UrlMismatch)
    ));
    let record = store
        .register(&bootstrap, install.clone(), 3)
        .await
        .unwrap();
    assert_eq!(record.vault_identity_key.as_deref(), Some("initial-key"));
    assert_eq!(
        store
            .status(org, 3)
            .await
            .unwrap()
            .vault_identity_key
            .as_deref(),
        Some("initial-key")
    );
    store
        .set_vault_identity(org, &owner, "replacement-key", 4)
        .await
        .unwrap();
    assert_eq!(
        store
            .status(org, 4)
            .await
            .unwrap()
            .vault_identity_key
            .as_deref(),
        Some("replacement-key")
    );
    assert_eq!(
        store
            .install_of(org)
            .await
            .unwrap()
            .unwrap()
            .vault_identity_key
            .as_deref(),
        Some("replacement-key")
    );
    assert_eq!(record.id, install.id);
    assert!(matches!(
        store.register(&bootstrap, install.clone(), 4).await,
        Err(RunnerError::InvalidToken)
    ));
    let directory = store
        .authenticate(&install.directory)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(directory.purpose, RunnerPurpose::Directory);
    assert_eq!(directory.org_id, *org);
    assert_eq!(directory.install_id, install.id);
    assert!(directory.group_id.is_none());
    for (hash, purpose) in [
        (&install.release, RunnerPurpose::Release),
        (&install.join, RunnerPurpose::Join),
    ] {
        let principal = store.authenticate(hash).await.unwrap().unwrap();
        assert_eq!(principal.purpose, purpose);
        assert_eq!(principal.install_id, install.id);
        assert!(principal.group_id.is_none());
    }

    let runner = NewRunner {
        id: "rn_00000000000000000000000001".parse().unwrap(),
        group_id: "rg_00000000000000000000000001".parse().unwrap(),
        group_name: "Group".into(),
        name: "Runner".into(),
        version: "1.0.0".into(),
        claim: secret_hash("claim"),
    };
    assert!(matches!(
        store.set_group_projects(org, &runner.group_id, &[]).await,
        Err(RunnerError::NotFound)
    ));
    assert_eq!(
        store
            .join(&install.id, &install.join, runner.clone(), 5)
            .await
            .unwrap(),
        runner.id
    );
    assert_eq!(
        store.runner_group(&install.id, &runner.id).await.unwrap(),
        Some(runner.group_id.clone())
    );
    let other_install = "ri_00000000000000000000000002".parse().unwrap();
    assert!(
        store
            .runner_group(&other_install, &runner.id)
            .await
            .unwrap()
            .is_none()
    );
    store
        .set_group_projects(
            org,
            &runner.group_id,
            std::slice::from_ref(&scope.project_id),
        )
        .await
        .unwrap();
    let mut rejoin = runner.clone();
    rejoin.id = "rn_00000000000000000000000002".parse().unwrap();
    rejoin.claim = secret_hash("reissued");
    assert_eq!(
        store
            .join(&install.id, &install.join, rejoin.clone(), 6)
            .await
            .unwrap(),
        runner.id
    );
    assert!(store.authenticate(&runner.claim).await.unwrap().is_none());
    let claim = store.authenticate(&rejoin.claim).await.unwrap().unwrap();
    assert_eq!(claim.purpose, RunnerPurpose::Claim);
    assert_eq!(claim.runner_id.as_ref(), Some(&runner.id));
    assert_eq!(claim.group_id.as_ref(), Some(&runner.group_id));
    let status = store.status(org, 6).await.unwrap();
    assert_eq!(
        status.groups[0].projects,
        std::slice::from_ref(&scope.project_id)
    );
    assert_eq!(status.runners.len(), 1);
    assert_eq!(status.runners[0].id, runner.id);

    assert_eq!(status.status, RunnerInstallStatus::Offline);
    let heartbeat_at = 7;
    store
        .heartbeat(
            &install.id,
            &RunnerHeartbeatRequest {
                groups: vec![],
                runners: vec![RunnerHeartbeatEntry {
                    id: runner.id.clone(),
                    status: "ready".into(),
                    running: 1,
                    capacity: 2,
                    decision_contract: None,
                }],
            },
            heartbeat_at,
        )
        .await
        .unwrap();
    let online = store.status(org, heartbeat_at + 89_999).await.unwrap();
    assert_eq!(online.status, RunnerInstallStatus::Online);
    assert_eq!(online.last_heartbeat_at, Some(heartbeat_at));
    assert_eq!(online.runners[0].last_heartbeat_at, Some(heartbeat_at));
    assert_eq!(online.runners[0].status, "ready");
    assert_eq!(online.runners[0].running, 1);
    assert_eq!(online.runners[0].capacity, 2);
    assert_eq!(
        store
            .status(org, heartbeat_at + 90_000)
            .await
            .unwrap()
            .status,
        RunnerInstallStatus::Offline
    );

    let agent = runbook_model::AgentDirectoryEntry {
        agent_id: "agt_1".into(),
        name: "checks".into(),
        project_id: scope.project_id.clone(),
        group_id: runner.group_id.clone(),
        status: runbook_model::AgentStatus::Active,
        version_id: "v1".into(),
        ceiling: runbook_model::AgentCeiling {
            profiles: vec!["fast".into()],
            profile_models: Default::default(),
            max_cost_usd: 0.5,
            max_tokens: 100,
            timeout_s: 60,
            credentials: vec![],
            tools: vec![],
            hosts: vec![],
        },
    };
    store.upsert_agent(&install.id, &agent, 7).await.unwrap();
    let mut second = agent.clone();
    second.agent_id = "agt_2".into();
    assert!(matches!(
        store.upsert_agent(&install.id, &second, 7).await,
        Err(RunnerError::Conflict("directory.name_taken"))
    ));
    let mut renamed = agent.clone();
    renamed.name = "new-checks".into();
    store.upsert_agent(&install.id, &renamed, 8).await.unwrap();
    store.upsert_agent(&install.id, &second, 8).await.unwrap();
    assert_eq!(
        store.project_agents(org, &scope.project_id).await.unwrap(),
        [second.clone(), renamed]
    );
    store
        .set_default_agent(org, &scope.project_id, Some(&second.name))
        .await
        .unwrap();
    assert_eq!(
        store
            .default_agent(org, &scope.project_id)
            .await
            .unwrap()
            .as_deref(),
        Some(second.name.as_str())
    );
    store
        .set_group_projects(org, &runner.group_id, &[])
        .await
        .unwrap();
    assert!(
        store
            .project_agents(org, &scope.project_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .agent_by_id(org, &second.agent_id)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        store.upsert_agent(&install.id, &second, 9).await,
        Err(RunnerError::Forbidden("directory.project_not_allowed"))
    ));
    store
        .remove_agent(&install.id, &second.agent_id)
        .await
        .unwrap();
    store
        .set_group_projects(
            org,
            &runner.group_id,
            std::slice::from_ref(&scope.project_id),
        )
        .await
        .unwrap();
    assert!(
        store
            .agent_by_id(org, &second.agent_id)
            .await
            .unwrap()
            .is_none()
    );
    store
        .set_default_agent(org, &scope.project_id, None)
        .await
        .unwrap();
    assert!(
        store
            .default_agent(org, &scope.project_id)
            .await
            .unwrap()
            .is_none()
    );
}
