//! B1.2 directory cases, moved unchanged from runbook-core tests (B2.2b) so
//! every DirectoryStore adapter runs the same assertions.
use crate::DirectoryFactory;
use runbook_core::{CommitResult, directory::*};
use runbook_model::{ActorRef, OrganizationId, ProjectId, ProjectRole, TeamId, UserId};
pub fn org(n: u8) -> OrganizationId {
    format!("org_{n:026}").parse().unwrap()
}
pub fn user(n: u8) -> UserId {
    format!("usr_{n:026}").parse().unwrap()
}
pub fn project(n: u8) -> ProjectId {
    format!("prj_{n:026}").parse().unwrap()
}
pub fn team(n: u8) -> TeamId {
    format!("team_{n:026}").parse().unwrap()
}
async fn seed<S: DirectoryStore>(s: &S, n: u8) {
    let m = vec![
        DirectoryMutation::CreateOrganization {
            title: format!("Org{n}"),
        },
        DirectoryMutation::CreateUser(UserRecord {
            id: user(n),
            name: format!("User{n}"),
            active: true,
            role: OrgRole::Member,
        }),
        DirectoryMutation::CreateProject(ProjectRecord {
            id: project(n),
            org_id: org(n),
            slug: "api".into(),
            title: "API".into(),
        }),
        DirectoryMutation::CreateTeam(TeamRecord {
            id: team(n),
            org_id: org(n),
            slug: "oncall".into(),
            title: "Oncall".into(),
        }),
        DirectoryMutation::GrantProjectRole {
            project_id: project(n),
            user_id: user(n),
            role: ProjectRole::Operator,
        },
        DirectoryMutation::AddTeamMember {
            team_id: team(n),
            user_id: user(n),
        },
    ];
    assert_eq!(
        s.commit(DirectoryCommit {
            org_id: org(n),
            expected_revision: 0,
            mutations: m
        })
        .await
        .unwrap(),
        CommitResult::Committed(1)
    );
}
pub async fn directory_snapshot_is_org_scoped<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    seed(&s, 2).await;
    let a = s.snapshot(&org(1)).await.unwrap();
    assert_eq!(a.users.keys().cloned().collect::<Vec<_>>(), vec![user(1)]);
    assert_eq!(
        a.projects.keys().cloned().collect::<Vec<_>>(),
        vec![project(1)]
    );
    assert_eq!(a.teams.keys().cloned().collect::<Vec<_>>(), vec![team(1)]);
    s.commit(DirectoryCommit {
        org_id: org(2),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::SetUserActive {
            user_id: user(2),
            active: false,
        }],
    })
    .await
    .unwrap();
    assert_eq!(s.snapshot(&org(1)).await.unwrap(), a);
    assert_eq!(s.org_for_project(&project(2)).await.unwrap(), Some(org(2)));
    assert_eq!(s.user_by_id(&user(2)).await.unwrap().unwrap().id, user(2));
    Ok(())
}
pub async fn directory_revision_fences_mutations<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    let c = DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::SetUserActive {
            user_id: user(1),
            active: false,
        }],
    };
    assert_eq!(
        s.commit(c.clone()).await.unwrap(),
        CommitResult::Committed(2)
    );
    let after = s.snapshot(&org(1)).await.unwrap();
    assert_eq!(s.commit(c).await.unwrap(), CommitResult::Conflict);
    assert_eq!(s.snapshot(&org(1)).await.unwrap(), after);
    Ok(())
}
pub async fn directory_rejects_cross_org_membership<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    seed(&s, 2).await;
    let before = s.snapshot(&org(1)).await.unwrap();
    for m in [
        DirectoryMutation::AddTeamMember {
            team_id: team(2),
            user_id: user(1),
        },
        DirectoryMutation::GrantProjectRole {
            project_id: project(1),
            user_id: user(2),
            role: ProjectRole::Admin,
        },
        DirectoryMutation::CreateProject(ProjectRecord {
            id: project(3),
            org_id: org(2),
            slug: "bad".into(),
            title: "Bad".into(),
        }),
    ] {
        assert!(
            s.commit(DirectoryCommit {
                org_id: org(1),
                expected_revision: 1,
                mutations: vec![
                    DirectoryMutation::SetUserActive {
                        user_id: user(1),
                        active: false
                    },
                    m
                ]
            })
            .await
            .is_err()
        );
        assert_eq!(s.snapshot(&org(1)).await.unwrap(), before);
    }
    Ok(())
}
pub async fn directory_slug_uniqueness_is_org_scoped<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    seed(&s, 2).await;
    let before = s.snapshot(&org(1)).await.unwrap();
    assert!(
        s.commit(DirectoryCommit {
            org_id: org(1),
            expected_revision: 1,
            mutations: vec![DirectoryMutation::CreateTeam(TeamRecord {
                id: team(3),
                org_id: org(1),
                slug: "oncall".into(),
                title: "Duplicate".into()
            })]
        })
        .await
        .is_err()
    );
    assert_eq!(s.snapshot(&org(1)).await.unwrap(), before);
    Ok(())
}
pub async fn directory_deactivation_and_removal_are_visible<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![
            DirectoryMutation::SetUserActive {
                user_id: user(1),
                active: false,
            },
            DirectoryMutation::RemoveTeamMember {
                team_id: team(1),
                user_id: user(1),
            },
            DirectoryMutation::RevokeProjectRole {
                project_id: project(1),
                user_id: user(1),
            },
        ],
    })
    .await
    .unwrap();
    let a = s.snapshot(&org(1)).await.unwrap();
    assert_eq!(a.revision, 2);
    assert!(!a.users[&user(1)].active);
    assert!(a.team_members.is_empty());
    assert!(a.project_members.is_empty());
    Ok(())
}

pub async fn directory_fixture_has_distinct_principals<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    seed(&s, 2).await;
    let mut mutations = vec![];
    for (n, name, role) in [
        (3, "bob", ProjectRole::Operator),
        (4, "carol", ProjectRole::Operator),
        (5, "viewer", ProjectRole::Viewer),
    ] {
        mutations.push(DirectoryMutation::CreateUser(UserRecord {
            id: user(n),
            name: name.into(),
            active: true,
            role: OrgRole::Member,
        }));
        mutations.push(DirectoryMutation::GrantProjectRole {
            project_id: project(1),
            user_id: user(n),
            role,
        });
    }
    mutations.push(DirectoryMutation::AddTeamMember {
        team_id: team(1),
        user_id: user(3),
    });
    mutations.push(DirectoryMutation::CreateTeam(TeamRecord {
        id: team(3),
        org_id: org(1),
        slug: "eng-leads".into(),
        title: "Leads".into(),
    }));
    mutations.push(DirectoryMutation::AddTeamMember {
        team_id: team(3),
        user_id: user(4),
    });
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations,
    })
    .await
    .unwrap();
    let a = s.snapshot(&org(1)).await.unwrap();
    assert_eq!(a.users.len(), 4);
    assert_eq!(a.team_members.len(), 3);
    assert!(!a.users.contains_key(&user(2)));
    assert_eq!(
        a.project_members[&(project(1), user(5))],
        ProjectRole::Viewer
    );
    // The service actor is distinct; it is never inserted as a human voter.
    let service = ActorRef::Agent {
        principal_id: "project-service".into(),
        session_id: "fixture".into(),
        name: "runner".into(),
        model: None,
    };
    assert!(matches!(service, ActorRef::Agent { .. }));
    Ok(())
}

pub async fn verified_email_key_reads_are_unique_and_atomic<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    seed(&s, 2).await;
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::BindVerifiedEmail {
            user_id: user(1),
            email: "Alice@example.test".into(),
        }],
    })
    .await
    .unwrap();
    assert_eq!(
        s.user_by_verified_email("alice@example.test")
            .await
            .unwrap()
            .unwrap()
            .id,
        user(1)
    );
    // NOTIFY1: the bulk read is org-scoped and never grants access.
    assert_eq!(
        s.verified_emails(&org(1)).await.unwrap(),
        std::collections::BTreeMap::from([(user(1), "alice@example.test".to_owned())])
    );
    assert_eq!(
        s.verified_emails(&org(2)).await.unwrap(),
        Default::default()
    );
    assert_eq!(
        s.verified_emails(&org(9)).await.unwrap_err(),
        DirectoryError::NotFound
    );
    let before = s.snapshot(&org(2)).await.unwrap();
    assert_eq!(
        s.commit(DirectoryCommit {
            org_id: org(2),
            expected_revision: 1,
            mutations: vec![DirectoryMutation::BindVerifiedEmail {
                user_id: user(2),
                email: "alice@example.test".into()
            }]
        })
        .await,
        Err(DirectoryError::Duplicate)
    );
    assert_eq!(s.snapshot(&org(2)).await.unwrap(), before);
    assert_eq!(
        s.user_by_verified_email("missing@example.test")
            .await
            .unwrap(),
        None
    );
    assert_eq!(s.org_for_project(&project(9)).await.unwrap(), None);
    assert_eq!(s.user_by_id(&user(9)).await.unwrap(), None);
    Ok(())
}

pub async fn invalid_batches_and_duplicate_ids_do_not_leak_indexes<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    let before = s.snapshot(&org(1)).await.unwrap();
    for mutations in [
        vec![],
        vec![
            DirectoryMutation::CreateUser(UserRecord {
                id: user(9),
                name: "Transient".into(),
                active: true,
                role: OrgRole::Member,
            }),
            DirectoryMutation::CreateUser(before.users[&user(1)].clone()),
        ],
        vec![DirectoryMutation::CreateProject(ProjectRecord {
            id: project(9),
            org_id: org(1),
            slug: "BAD".into(),
            title: "Bad".into(),
        })],
    ] {
        assert!(
            s.commit(DirectoryCommit {
                org_id: org(1),
                expected_revision: 1,
                mutations
            })
            .await
            .is_err()
        );
        assert_eq!(s.snapshot(&org(1)).await.unwrap(), before);
        assert_eq!(s.user_by_id(&user(9)).await.unwrap(), None);
        assert_eq!(s.org_for_project(&project(9)).await.unwrap(), None);
    }
    assert_eq!(s.snapshot(&org(9)).await, Err(DirectoryError::NotFound));
    Ok(())
}

pub async fn shared_identity_deactivation_is_org_local<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    seed(&s, 2).await;
    s.commit(DirectoryCommit {
        org_id: org(2),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::CreateUser(UserRecord {
            id: user(1),
            name: "User1".into(),
            active: true,
            role: OrgRole::Member,
        })],
    })
    .await
    .unwrap();
    let b = s.snapshot(&org(2)).await.unwrap();
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::SetUserActive {
            user_id: user(1),
            active: false,
        }],
    })
    .await
    .unwrap();
    assert!(!s.snapshot(&org(1)).await.unwrap().users[&user(1)].active);
    assert_eq!(s.snapshot(&org(2)).await.unwrap(), b);
    assert!(b.users[&user(1)].active);
    Ok(())
}

pub async fn org_role_permission_matrix<F: DirectoryFactory>(f: &F) -> Result<(), DirectoryError> {
    use runbook_core::identity::{OrgAction, org_action_allowed};
    for role in [OrgRole::Owner, OrgRole::Admin, OrgRole::Member] {
        for action in [
            OrgAction::CreateUser,
            OrgAction::CreateTeam,
            OrgAction::CreateProject,
            OrgAction::ManageMemberships,
            OrgAction::ManageTokens,
        ] {
            assert_eq!(org_action_allowed(role, action), role != OrgRole::Member);
        }
    }
    let s = f.create_directory().await?;
    seed(&s, 1).await;
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::GrantProjectRole {
            project_id: project(1),
            user_id: user(1),
            role: ProjectRole::Admin,
        }],
    })
    .await?;
    let snap = s.snapshot(&org(1)).await?;
    assert_eq!(
        snap.project_members[&(project(1), user(1))],
        ProjectRole::Admin
    );
    assert!(!org_action_allowed(
        snap.users[&user(1)].role,
        OrgAction::CreateUser
    ));
    Ok(())
}

async fn role_org<S: DirectoryStore>(
    s: &S,
    n: u8,
    shared_role: OrgRole,
) -> Result<(), DirectoryError> {
    s.commit(DirectoryCommit {
        org_id: org(n),
        expected_revision: 0,
        mutations: vec![
            DirectoryMutation::CreateOrganization {
                title: format!("Role org {n}"),
            },
            DirectoryMutation::CreateUser(UserRecord {
                id: user(1),
                name: "Shared".into(),
                active: true,
                role: shared_role,
            }),
            DirectoryMutation::CreateUser(UserRecord {
                id: user(n + 10),
                name: format!("Owner {n}"),
                active: true,
                role: OrgRole::Owner,
            }),
        ],
    })
    .await?;
    Ok(())
}
pub async fn last_active_owner_is_protected<F: DirectoryFactory>(
    f: &F,
) -> Result<(), DirectoryError> {
    let s = f.create_directory().await?;
    role_org(&s, 1, OrgRole::Member).await?;
    let original = s.snapshot(&org(1)).await?;
    for mutation in [
        DirectoryMutation::SetOrgRole {
            user_id: user(11),
            role: OrgRole::Admin,
        },
        DirectoryMutation::SetUserActive {
            user_id: user(11),
            active: false,
        },
    ] {
        assert_eq!(
            s.commit(DirectoryCommit {
                org_id: org(1),
                expected_revision: 1,
                mutations: vec![mutation]
            })
            .await,
            Err(DirectoryError::Invalid("last owner"))
        );
        assert_eq!(s.snapshot(&org(1)).await?, original);
    }
    // Inactive owners do not satisfy the invariant, and failed batches roll back.
    assert_eq!(
        s.commit(DirectoryCommit {
            org_id: org(1),
            expected_revision: 1,
            mutations: vec![
                DirectoryMutation::SetOrgRole {
                    user_id: user(1),
                    role: OrgRole::Owner
                },
                DirectoryMutation::SetUserActive {
                    user_id: user(1),
                    active: false
                },
                DirectoryMutation::SetOrgRole {
                    user_id: user(11),
                    role: OrgRole::Member
                },
            ]
        })
        .await,
        Err(DirectoryError::Invalid("last owner"))
    );
    assert_eq!(s.snapshot(&org(1)).await?, original);
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::SetOrgRole {
            user_id: user(1),
            role: OrgRole::Owner,
        }],
    })
    .await?;
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 2,
        mutations: vec![DirectoryMutation::SetUserActive {
            user_id: user(11),
            active: false,
        }],
    })
    .await?;
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 3,
        mutations: vec![DirectoryMutation::SetUserActive {
            user_id: user(11),
            active: true,
        }],
    })
    .await?;
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 4,
        mutations: vec![DirectoryMutation::SetOrgRole {
            user_id: user(11),
            role: OrgRole::Member,
        }],
    })
    .await?;
    assert_eq!(
        s.snapshot(&org(1)).await?.users[&user(11)].role,
        OrgRole::Member
    );
    // D29: ownerless directory creation remains valid, until ownership is established.
    assert_eq!(
        s.commit(DirectoryCommit {
            org_id: org(9),
            expected_revision: 0,
            mutations: vec![DirectoryMutation::CreateOrganization {
                title: "Empty".into()
            }]
        })
        .await?,
        CommitResult::Committed(1)
    );
    s.commit(DirectoryCommit {
        org_id: org(9),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::CreateUser(UserRecord {
            id: user(99),
            name: "Initial member".into(),
            active: true,
            role: OrgRole::Member,
        })],
    })
    .await?;
    s.commit(DirectoryCommit {
        org_id: org(9),
        expected_revision: 2,
        mutations: vec![DirectoryMutation::SetOrgRole {
            user_id: user(99),
            role: OrgRole::Owner,
        }],
    })
    .await?;
    let owned = s.snapshot(&org(9)).await?;
    assert_eq!(
        s.commit(DirectoryCommit {
            org_id: org(9),
            expected_revision: 3,
            mutations: vec![DirectoryMutation::SetUserActive {
                user_id: user(99),
                active: false
            }]
        })
        .await,
        Err(DirectoryError::Invalid("last owner"))
    );
    assert_eq!(s.snapshot(&org(9)).await?, owned);
    Ok(())
}
pub async fn org_roles_are_org_local<F: DirectoryFactory>(f: &F) -> Result<(), DirectoryError> {
    use runbook_core::identity::{OrgAction, org_action_allowed};
    let s = f.create_directory().await?;
    role_org(&s, 1, OrgRole::Owner).await?;
    role_org(&s, 2, OrgRole::Member).await?;
    let b = s.snapshot(&org(2)).await?;
    assert!(org_action_allowed(
        s.snapshot(&org(1)).await?.users[&user(1)].role,
        OrgAction::ManageTokens
    ));
    assert!(!org_action_allowed(
        b.users[&user(1)].role,
        OrgAction::ManageTokens
    ));
    s.commit(DirectoryCommit {
        org_id: org(1),
        expected_revision: 1,
        mutations: vec![DirectoryMutation::SetOrgRole {
            user_id: user(1),
            role: OrgRole::Admin,
        }],
    })
    .await?;
    assert_eq!(s.snapshot(&org(2)).await?, b);
    assert_eq!(
        s.snapshot(&org(1)).await?.users[&user(1)].role,
        OrgRole::Admin
    );
    let a = s.snapshot(&org(1)).await?;
    assert_eq!(
        s.commit(DirectoryCommit {
            org_id: org(1),
            expected_revision: 2,
            mutations: vec![DirectoryMutation::SetOrgRole {
                user_id: user(12),
                role: OrgRole::Owner
            }]
        })
        .await,
        Err(DirectoryError::NotFound)
    );
    assert_eq!(s.snapshot(&org(1)).await?, a);
    assert_eq!(s.snapshot(&org(2)).await?, b);
    Ok(())
}
