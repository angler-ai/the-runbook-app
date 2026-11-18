mod support {
    pub mod cases;
}
#[test]
fn directory_routes_enforce_scope_and_roles() {
    use std::future::Future;
    let mut f = Box::pin(support::cases::directory_routes_enforce_scope_and_roles(
        runbook_core::directory::memory::MemoryDirectory::default(),
        runbook_core::credentials::memory::MemoryCredentials::default(),
    ));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        f.as_mut().poll(&mut cx),
        std::task::Poll::Ready(())
    ));
}
#[test]
fn replay_expires_at_twenty_four_hours() {
    use runbook_api::{Service, discovery::DiscoveryConfig};
    use runbook_core::{credentials::*, directory_operations::*};
    use std::future::Future;
    let mut test = Box::pin(async {
        let d = runbook_core::directory::memory::MemoryDirectory::default();
        let c = runbook_core::credentials::memory::MemoryCredentials::default();
        support::cases::directory_routes_enforce_scope_and_roles(d.clone(), c.clone()).await;
        d.set_time_ms(86_400_000);
        let service = Service::new(
            d.clone(),
            c,
            DiscoveryConfig {
                origin: "https://test.example".into(),
                minimum_cli_version: "0.1.0".into(),
                max_request_bytes: 67_108_864,
                version: runbook_api::VERSION.into(),
                schema: Default::default(),
                theme: None,
                step_runs: None,
            },
        );
        let org = format!("org_{:026}", 1);
        let token = encode_secret(TokenKind::Personal, [1; 32]);
        let r = service
            .handle(support::cases::request(
                "POST",
                &format!("/v1/orgs/{org}/users"),
                serde_json::json!({"name":"Alice"}),
                &token,
                Some("Alice"),
                90,
            ))
            .await;
        assert_eq!(r.status, 201);
        assert_eq!(r.body["id"], format!("usr_{:026}", 90));
        assert_eq!(d.current_time_ms().await.unwrap(), 86_400_000);
    });
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        test.as_mut().poll(&mut cx),
        std::task::Poll::Ready(())
    ));
}
#[test]
fn admins_cannot_deactivate_owners() {
    use std::future::Future;
    let mut f = Box::pin(support::cases::admins_cannot_deactivate_owners(
        runbook_core::directory::memory::MemoryDirectory::default(),
        runbook_core::credentials::memory::MemoryCredentials::default(),
    ));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    assert!(matches!(
        f.as_mut().poll(&mut cx),
        std::task::Poll::Ready(())
    ));
}
