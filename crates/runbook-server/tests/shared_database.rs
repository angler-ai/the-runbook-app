//! Run against a disposable PostgreSQL 16 instance with max_connections=25
//! (or 100 for the normal workspace suite).
#[path = "../../runbook-store-postgres/tests/support/database.rs"]
#[allow(dead_code)]
mod database;

use axum::{body::Body, http::Request};
use futures_util::{FutureExt, future::join_all};
use runbook_server::{
    config::ServerConfig,
    http::{Worker, main_router},
};
use runbook_store_postgres::{DatabaseConfig, PostgresStore};
use std::{panic::AssertUnwindSafe, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
};
use tower::ServiceExt;

#[tokio::test]
#[ignore = "requires a disposable local TCP DATABASE_URL"]
async fn runner_connection_burst_leaves_core_ready_and_core_recovers_after_outage() {
    let mut core = database::TestDatabase::create_from_env().await.unwrap();
    let mut runner = database::TestDatabase::create_from_env().await.unwrap();
    let admin = PostgresStore::connect(core.config()).await.unwrap();
    let runner_logins =
        ["api", "vault", "bootstrap"].map(|role| format!("{}_{role}", runner.name()));
    let core_login = format!("{}_login", core.name());
    let result = AssertUnwindSafe(async {
        let url = url::Url::parse(&std::env::var("DATABASE_URL").unwrap()).unwrap();
        let upstream = (url.host_str().unwrap().to_owned(), url.port().unwrap_or(5432));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (outage, connection_state) = watch::channel(false);

        // Let all existing TCP sockets remain connected but silent when the
        // upstream is unavailable. New sockets during the outage hang too.
        let proxy = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    socket = listener.accept() => {
                        let (mut downstream, _) = socket.unwrap();
                        let mut state = connection_state.clone();
                        let upstream = upstream.clone();
                        connections.spawn(async move {
                            if !*state.borrow_and_update() {
                                let mut upstream = TcpStream::connect(upstream).await.unwrap();
                                tokio::select! {
                                    _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream) => return,
                                    _ = state.changed() => {}
                                }
                            }
                            std::future::pending::<()>().await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        let pg_config = |database: &str, login: &str, proxy: bool| {
            let mut url = url.clone();
            url.set_username(login).unwrap();
            url.set_password(Some("ephemeral-test-login")).unwrap();
            url.set_path(database);
            url.set_query(Some("sslmode=disable"));
            if proxy {
                url.set_host(Some("127.0.0.1")).unwrap();
                url.set_port(Some(address.port())).unwrap();
            }
            DatabaseConfig::from_values(Some(url.into()), None).unwrap().with_pool_size(5)
        };
        for (login, limit) in std::iter::zip(&runner_logins, [2, 1, 2]) {
            admin.client().batch_execute(&format!(
                "CREATE ROLE {login} LOGIN CONNECTION LIMIT {limit} PASSWORD 'ephemeral-test-login'"
            )).await.unwrap();
        }
        admin.client().batch_execute(&format!(
            "CREATE ROLE {core_login} LOGIN PASSWORD 'ephemeral-test-login'"
        )).await.unwrap();
        let config = ServerConfig::new(pg_config(core.name(), &core_login, true), "http://localhost", "127.0.0.1:0").unwrap();
        let worker = Worker::start(config.clone()).unwrap();
        let app = main_router(worker.client(), &config);
        let ready = async || {
            app.clone().oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
                .await.unwrap().status().as_u16()
        };

        // Repeatedly attempt four simultaneous connections per login. Hold
        // accepted sessions while the next wave reconnects on the same SQL.
        for _ in 0..3 {
            let configs = runner_logins.each_ref().map(|login| pg_config(runner.name(), login, false));
            let attempts = configs.iter().flat_map(|config| (0..4).map(move |_| PostgresStore::connect(config)));
            let (burst, _) = tokio::join!(join_all(attempts), async {
                for _ in 0..4 {
                    assert_eq!(ready().await, 200);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            });
            assert!(burst.iter().any(Result::is_ok));
            assert!(burst.iter().filter(|connection| connection.is_ok()).count() <= 5);
            assert!(configs.iter().all(|config| config.pool_stats().failures.contains_key("connection_limit")));
            tokio::time::sleep(runbook_server::http::READY_CACHE).await;
            assert_eq!(ready().await, 200);
            drop(burst);
        }
        outage.send(true).unwrap();
        tokio::time::sleep(runbook_server::http::READY_CACHE).await;
        assert_eq!(ready().await, 503);
        outage.send(false).unwrap();
        tokio::time::sleep(runbook_server::http::READY_CACHE).await;
        assert_eq!(ready().await, 200);
        proxy.abort();
    })
    .catch_unwind().await;
    for login in runner_logins.iter().chain([&core_login]) {
        admin
            .client()
            .batch_execute(&format!("DROP ROLE IF EXISTS {login}"))
            .await
            .unwrap();
    }
    drop(admin);
    runner.finish(Ok(())).await;
    core.finish(result).await;
}
