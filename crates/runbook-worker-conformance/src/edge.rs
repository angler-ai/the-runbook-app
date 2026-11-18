//! The test Worker: `GET /cases` lists what is registered and what must
//! be; `POST /case/{name}?run={nonce}` runs one case in its own object.
//! A D1 case (CF3) runs there too, over the Worker's local D1 binding,
//! migrated and emptied first.
//!
//! A Durable Object can only use its own storage, and the store is
//! synchronous, so a case cannot get a second object for a second store.
//! Instead each `fresh` empties this case's object under a new epoch, and a
//! store from an earlier epoch fails every call (`test.store_aliased`): two
//! live stores in one case can never silently share rows.
use crate::host::Host;
use runbook_core::StoreError;
use runbook_store_do::{DoObject, DoSql, Row, Value, workers::WorkersSql};
use runbook_store_r2::{ObjectBucket, workers::R2Bucket};
use serde_json::json;
use std::{cell::Cell, rc::Rc};
use worker::{DurableObject, Env, Method, Request, Response, Result, State, durable_object, event};

/// `WorkersSql` with a test clock and an epoch fence.
#[derive(Clone)]
pub struct CaseSql {
    inner: WorkersSql,
    epoch: u64,
    current: Rc<Cell<u64>>,
    clock: Rc<Cell<Option<i64>>>,
}
impl CaseSql {
    fn live(&self) -> std::result::Result<(), StoreError> {
        if self.epoch == self.current.get() {
            Ok(())
        } else {
            Err(StoreError::Backend("test.store_aliased"))
        }
    }
}
impl DoSql for CaseSql {
    fn exec(&self, sql: &str, params: &[Value]) -> std::result::Result<(), StoreError> {
        self.live()?;
        self.inner.exec(sql, params)
    }
    fn query(&self, sql: &str, params: &[Value]) -> std::result::Result<Vec<Row>, StoreError> {
        self.live()?;
        self.inner.query(sql, params)
    }
    fn exec_script(&self, sql: &str) -> std::result::Result<(), StoreError> {
        self.live()?;
        self.inner.exec_script(sql)
    }
    fn transaction<T: 'static>(
        &self,
        body: impl FnOnce() -> std::result::Result<T, StoreError> + 'static,
    ) -> std::result::Result<T, StoreError> {
        self.live()?;
        self.inner.transaction(body)
    }
    fn now_ms(&self) -> i64 {
        self.clock.get().unwrap_or_else(|| self.inner.now_ms())
    }
    fn set_alarm(&self, at_ms: Option<i64>) -> std::result::Result<(), StoreError> {
        self.live()?;
        self.inner.set_alarm(at_ms)
    }
}

thread_local! {
    /// CF11: one fresh namespace of webhook objects per D1 case.
    static WEBHOOK_NAMESPACES: Cell<u64> = const { Cell::new(0) };
}

/// One case's object as a test host.
pub struct WorkersHost {
    sql: WorkersSql,
    /// Miniflare's local R2 buckets (CF4).
    blobs: worker::Bucket,
    user_content: worker::Bucket,
    current: Rc<Cell<u64>>,
    clock: Rc<Cell<Option<i64>>>,
    start_ms: i64,
    env: Env,
}
impl Host for WorkersHost {
    type Sql = CaseSql;
    fn fresh(&self) -> std::result::Result<CaseSql, StoreError> {
        let epoch = self.current.get() + 1;
        self.current.set(epoch);
        self.clock.set(None);
        let sql = CaseSql {
            inner: self.sql.clone(),
            epoch,
            current: self.current.clone(),
            clock: self.clock.clone(),
        };
        // Migrate, then empty every table and the alarm.
        DoObject::new(sql.clone()).reset()?;
        Ok(sql)
    }
    fn start_ms(&self) -> i64 {
        self.start_ms
    }
    fn set_time(&self, _sql: &CaseSql, now_ms: i64) {
        self.clock.set(Some(now_ms));
    }
    async fn alarm(&self, _sql: &CaseSql) -> Option<i64> {
        self.sql.storage().get_alarm().await.ok().flatten()
    }
    type D1 = runbook_store_d1::workers::WorkersD1;
    /// The local D1, migrated and emptied: cases run one at a time and
    /// share it (CF3).
    async fn d1(&self) -> std::result::Result<Self::D1, StoreError> {
        let db = runbook_store_d1::workers::WorkersD1::new(
            self.env
                .d1("DB")
                .map_err(|_| StoreError::Backend("test.d1_binding"))?,
        );
        crate::host::fresh_d1(&db).await?;
        Ok(db)
    }
    type Webhooks = runbook_worker::webhooks::ViaTransport<CaseTransport>;
    /// One fresh namespace of case objects per D1 case (a random prefix).
    fn webhooks(&self) -> Self::Webhooks {
        runbook_worker::webhooks::ViaTransport(CaseTransport {
            namespace: self.env.durable_object("CASES").expect("the CASES binding"),
            prefix: format!(
                "d1-webhooks/{}/{}",
                worker::Date::now().as_millis(),
                WEBHOOK_NAMESPACES.with(|n| {
                    n.set(n.get() + 1);
                    n.get()
                })
            ),
            clock: Rc::new(Cell::new(self.start_ms)),
        })
    }
    type Bucket = R2Bucket;
    /// Empty both buckets: cases run one at a time and share them.
    async fn buckets(&self) -> std::result::Result<(R2Bucket, R2Bucket), StoreError> {
        let buckets = (
            R2Bucket(self.blobs.clone()),
            R2Bucket(self.user_content.clone()),
        );
        for bucket in [&buckets.0, &buckets.1] {
            loop {
                let page = bucket.list("", None).await?;
                if page.keys.is_empty() {
                    break;
                }
                for key in &page.keys {
                    bucket.delete(key).await?;
                }
            }
        }
        Ok(buckets)
    }
}

/// CF5: `WorkersSql` at a request's test clock (the object protocol's
/// `clock`), for an object serving another case object's calls.
#[derive(Clone)]
struct OpSql {
    inner: WorkersSql,
    now: Option<i64>,
}
impl DoSql for OpSql {
    fn exec(&self, sql: &str, params: &[Value]) -> std::result::Result<(), StoreError> {
        self.inner.exec(sql, params)
    }
    fn query(&self, sql: &str, params: &[Value]) -> std::result::Result<Vec<Row>, StoreError> {
        self.inner.query(sql, params)
    }
    fn exec_script(&self, sql: &str) -> std::result::Result<(), StoreError> {
        self.inner.exec_script(sql)
    }
    fn transaction<T: 'static>(
        &self,
        body: impl FnOnce() -> std::result::Result<T, StoreError> + 'static,
    ) -> std::result::Result<T, StoreError> {
        self.inner.transaction(body)
    }
    fn now_ms(&self) -> i64 {
        self.now.unwrap_or_else(|| self.inner.now_ms())
    }
    fn set_alarm(&self, at_ms: Option<i64>) -> std::result::Result<(), StoreError> {
        self.inner.set_alarm(at_ms)
    }
}

/// CF5: the object protocol to the case's run objects: each run is its own
/// `ConformanceDO`, named for the case run, the factory's namespace epoch
/// (one per `create`) and the execution, reached with a stub `fetch`.
#[derive(Clone)]
pub struct CaseTransport {
    namespace: worker::ObjectNamespace,
    prefix: String,
    clock: Rc<Cell<i64>>,
}
impl runbook_worker::protocol::Transport for CaseTransport {
    async fn call(
        &self,
        request: runbook_worker::protocol::Request,
    ) -> runbook_worker::protocol::Reply {
        let failed = |_| StoreError::Backend("object.unreachable");
        let stub = self
            .namespace
            .id_from_name(&format!("{}/{}", self.prefix, request.execution_id))
            .map_err(failed)?
            .get_stub()
            .map_err(failed)?;
        let body =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("object.encode"))?;
        let mut init = worker::RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(wasm_bindgen::JsValue::from_str(&body)));
        let outgoing = Request::new_with_init("https://do/op", &init).map_err(failed)?;
        let mut response = stub.fetch_with_request(outgoing).await.map_err(failed)?;
        let status = response.status_code();
        let text = response.text().await.map_err(failed)?;
        let reply = serde_json::from_str(&text)
            .map_err(|_| StoreError::Backend("object.bad_reply"))
            .and_then(runbook_worker::protocol::decode_reply);
        if matches!(reply, Err(StoreError::Backend("object.bad_reply"))) {
            worker::console_log!(
                "object protocol: bad reply {status} to {}: {}",
                body.chars().take(300).collect::<String>(),
                text.chars().take(300).collect::<String>()
            );
        }
        reply
    }
    async fn call_org(
        &self,
        request: runbook_worker::quota::QuotaRequest,
    ) -> runbook_worker::protocol::Reply {
        let failed = |_| StoreError::Backend("object.unreachable");
        let stub = self
            .namespace
            .id_from_name(&format!("{}/org/{}", self.prefix, request.org))
            .map_err(failed)?
            .get_stub()
            .map_err(failed)?;
        let body =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("quota.encode"))?;
        let mut init = worker::RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(wasm_bindgen::JsValue::from_str(&body)));
        let outgoing = Request::new_with_init("https://do/quota", &init).map_err(failed)?;
        let mut response = stub.fetch_with_request(outgoing).await.map_err(failed)?;
        let text = response.text().await.map_err(failed)?;
        serde_json::from_str(&text)
            .map_err(|_| StoreError::Backend("object.bad_reply"))
            .and_then(runbook_worker::protocol::decode_reply)
    }
    async fn call_webhook(
        &self,
        request: runbook_worker::webhooks::WebhookRequest,
    ) -> runbook_worker::protocol::Reply {
        let failed = |_| StoreError::Backend("object.unreachable");
        let stub = self
            .namespace
            .id_from_name(&format!("{}/webhook/{}", self.prefix, request.subscription))
            .map_err(failed)?
            .get_stub()
            .map_err(failed)?;
        let body =
            serde_json::to_string(&request).map_err(|_| StoreError::Backend("webhooks.encode"))?;
        let mut init = worker::RequestInit::new();
        init.with_method(Method::Post)
            .with_body(Some(wasm_bindgen::JsValue::from_str(&body)));
        let outgoing = Request::new_with_init("https://do/webhook", &init).map_err(failed)?;
        let mut response = stub.fetch_with_request(outgoing).await.map_err(failed)?;
        let text = response.text().await.map_err(failed)?;
        serde_json::from_str(&text)
            .map_err(|_| StoreError::Backend("object.bad_reply"))
            .and_then(runbook_worker::webhooks::decode_webhook_reply)
    }
    fn clock(&self) -> Option<i64> {
        Some(self.clock.get())
    }
}

/// CF5: the cross-store host in workerd: run objects over [`CaseTransport`],
/// the local D1 and both R2 buckets (emptied for each `create`).
struct WorkersCross {
    env: Env,
    run: String,
    epoch: Cell<u64>,
    start_ms: i64,
}
impl crate::cross::CrossHost for WorkersCross {
    type Transport = CaseTransport;
    type D1 = runbook_store_d1::workers::WorkersD1;
    type Bucket = R2Bucket;
    fn transport(&self, clock: Rc<Cell<i64>>) -> CaseTransport {
        self.epoch.set(self.epoch.get() + 1);
        CaseTransport {
            namespace: self.env.durable_object("CASES").expect("the CASES binding"),
            prefix: format!("{}/{}", self.run, self.epoch.get()),
            clock,
        }
    }
    async fn d1(&self) -> std::result::Result<Self::D1, StoreError> {
        Ok(runbook_store_d1::workers::WorkersD1::new(
            self.env
                .d1("DB")
                .map_err(|_| StoreError::Backend("test.d1_binding"))?,
        ))
    }
    async fn buckets(&self) -> std::result::Result<(R2Bucket, R2Bucket), StoreError> {
        let buckets = (
            R2Bucket(
                self.env
                    .bucket("BLOBS")
                    .map_err(|_| StoreError::Backend("test.r2_binding"))?,
            ),
            R2Bucket(
                self.env
                    .bucket("USER_CONTENT")
                    .map_err(|_| StoreError::Backend("test.r2_binding"))?,
            ),
        );
        for bucket in [&buckets.0, &buckets.1] {
            loop {
                let page = bucket.list("", None).await?;
                if page.keys.is_empty() {
                    break;
                }
                for key in &page.keys {
                    bucket.delete(key).await?;
                }
            }
        }
        Ok(buckets)
    }
    fn start_ms(&self) -> i64 {
        self.start_ms
    }
}

fn reply(status: u16, body: serde_json::Value) -> Result<Response> {
    Ok(Response::from_json(&body)?.with_status(status))
}

#[durable_object]
pub struct ConformanceDO {
    state: State,
    env: Env,
}

impl DurableObject for ConformanceDO {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }
    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let name = url.path().trim_start_matches('/').to_owned();
        // CF5: this object is one run of a cross-store case, serving the
        // object protocol (test seams on) at the caller's test clock.
        if name == "op" {
            // `serde_json` on the text, never `Request::json` (JS
            // `JSON.parse`, then `serde_wasm_bindgen`), which loses
            // `serde_json`'s arbitrary-precision numbers inside run data.
            let request: runbook_worker::protocol::Request =
                match serde_json::from_str(&req.text().await?) {
                    Ok(request) => request,
                    Err(_) => return reply(400, json!({ "error": "object.invalid_request" })),
                };
            let sql = OpSql {
                inner: WorkersSql::new(self.state.storage()),
                now: request.clock,
            };
            let outcome = match DoObject::new(sql).store(&request.execution_id, request.scope) {
                Ok(store) => runbook_worker::protocol::serve(&store, request.op, true).await,
                Err(StoreError::Backend("do.object_mismatch")) => Err(StoreError::NotFound),
                Err(error) => Err(error),
            };
            return Response::from_json(&runbook_worker::protocol::into_wire(outcome));
        }
        // CF11: this object is a case's WebhookDO, serving one
        // subscription's delivery log.
        if name == "webhook" {
            let request: runbook_worker::webhooks::WebhookRequest =
                match serde_json::from_str(&req.text().await?) {
                    Ok(request) => request,
                    Err(_) => return reply(400, json!({ "error": "webhooks.invalid_request" })),
                };
            let outcome = runbook_worker::webhooks::serve_webhook(
                WorkersSql::new(self.state.storage()),
                request,
            );
            return Response::from_json(&runbook_worker::webhooks::encode_webhook_reply(&outcome));
        }
        // CF6: this object is a case's OrgDO, serving quota operations.
        if name == "quota" {
            let request: runbook_worker::quota::QuotaRequest =
                match serde_json::from_str(&req.text().await?) {
                    Ok(request) => request,
                    Err(_) => return reply(400, json!({ "error": "quota.invalid_request" })),
                };
            let outcome =
                runbook_store_do::org::OrgStore::open(WorkersSql::new(self.state.storage()))
                    .and_then(|store| runbook_worker::quota::serve_quota(&store, request));
            return Response::from_json(&runbook_worker::protocol::into_wire(outcome));
        }
        let run = url
            .query_pairs()
            .find(|(k, _)| k == "run")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        // An hour ahead, rounded: alarms the cases set stay in the future.
        #[allow(clippy::cast_possible_truncation)]
        let start_ms = (worker::Date::now().as_millis() as i64 / 1_000 + 3_600) * 1_000;
        let host = WorkersHost {
            sql: WorkersSql::new(self.state.storage()),
            blobs: self.env.bucket("BLOBS")?,
            user_content: self.env.bucket("USER_CONTENT")?,
            current: Rc::default(),
            clock: Rc::default(),
            start_ms,
            env: self.env.clone(),
        };
        let outcome = match crate::run_case(&name, &host).await {
            Some(outcome) => Some(outcome.map_err(|error| format!("{error:?}"))),
            None => match crate::run_d1_case(&name, &host).await {
                Some(outcome) => Some(outcome),
                // CF5: a cross-store case, its runs in their own objects.
                None => {
                    crate::cross_registry::run_cross_case(
                        &name,
                        WorkersCross {
                            env: self.env.clone(),
                            run: format!("{name}#{run}"),
                            epoch: Cell::new(0),
                            start_ms,
                        },
                    )
                    .await
                }
            },
        };
        match outcome {
            None => reply(404, json!({ "error": "case.unregistered", "case": name })),
            Some(Ok(())) => reply(200, json!({ "ok": true, "case": name })),
            Some(Err(error)) => reply(500, json!({ "error": error, "case": name })),
        }
    }
    /// The cases leave real alarms behind (timers, outbox retries) and drive
    /// the queue themselves, so a firing alarm has nothing to do. Without
    /// this, workers-rs's default handler panics and resets the instance
    /// under any case still running.
    async fn alarm(&self) -> Result<Response> {
        Response::ok("conformance: alarms are driven by the cases")
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: worker::Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_owned();
    match (req.method(), path.as_str()) {
        (Method::Get, "/cases") => reply(
            200,
            json!({
                "shared": crate::SHARED,
                "blob": crate::BLOB,
                "blob_cases": runbook_store_conformance::execution_scoped::BLOB_CASES,
                "own": crate::DO_CASES,
                "d1": crate::D1,
                "d1_own": crate::D1_OWN,
                "d1_cases": runbook_store_conformance::d1_scoped::D1_CASES,
                "execution_scoped": crate::EXECUTION_SCOPED,
                "cross": crate::cross_registry::CROSS,
                "cross_cases": runbook_store_conformance::cross_scoped::CROSS_CASES,
                "cross_own": crate::cross_registry::CROSS_OWN,
            }),
        ),
        (Method::Post, case) if case.starts_with("/case/") => {
            let name = &case["/case/".len()..];
            let run = url
                .query_pairs()
                .find(|(k, _)| k == "run")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            // One fresh object per case run.
            let stub = env
                .durable_object("CASES")?
                .id_from_name(&format!("{name}#{run}"))?
                .get_stub()?;
            let mut init = worker::RequestInit::new();
            init.with_method(Method::Post);
            stub.fetch_with_request(Request::new_with_init(
                &format!("https://do/{name}?run={run}"),
                &init,
            )?)
            .await
        }
        _ => reply(404, json!({ "error": "route.not_found" })),
    }
}
