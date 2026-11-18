//! CF3: the real `runbook_api::Router` on the Cloudflare backends natively
//! (D1 and every ExecutionDO over rusqlite, R2 in memory), the way the api
//! Worker and the objects run it: bootstrap an org with a PAT, publish a
//! runbook, start a run, claim and report its step, and find the run
//! summary and inbox in D1. After each request the host settles every
//! object (due work, then derived rows), as an ExecutionDO does.
use runbook_api::{
    Credential, Request,
    backends::NoEntropy,
    router::{Router, RouterConfig},
};
use runbook_core::{StoreError, catalog::TenantScope, webhooks::TargetPolicy};
use runbook_model::ExecutionId;
use runbook_store_d1::native::NativeD1;
use runbook_store_do::DoStore;
use runbook_store_r2::MemoryBucket;
use runbook_worker::{
    cloudflare::{CfBackends, CfHost},
    compose::Objects,
    sealer::AesSealer,
};
use runbook_worker_conformance::native::{NativeSql, run};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

const START_MS: i64 = 1_800_000_000_000;

#[derive(Clone, Default)]
struct NativeObjects {
    map: Rc<RefCell<BTreeMap<ExecutionId, NativeSql>>>,
    /// CF11: the webhook subscriptions' objects.
    webhooks: runbook_worker::webhooks::LocalWebhooks<NativeSql>,
    now: Rc<Cell<i64>>,
    /// The next new object is unreachable once (a start whose object write
    /// was lost after D1 committed).
    lose_next: Rc<Cell<bool>>,
}
impl Objects for NativeObjects {
    type Handle = DoStore<NativeSql>;
    type Quota = runbook_worker::quota::NoQuotas;
    fn quota(&self, _org: &runbook_model::OrganizationId) -> Self::Quota {
        runbook_worker::quota::NoQuotas
    }
    type Webhooks = runbook_worker::webhooks::LocalWebhooks<NativeSql>;
    fn webhooks(&self) -> Self::Webhooks {
        self.webhooks.clone()
    }
    fn object(
        &self,
        id: &ExecutionId,
        scope: &TenantScope,
    ) -> Result<DoStore<NativeSql>, StoreError> {
        if !self.map.borrow().contains_key(id) && self.lose_next.replace(false) {
            return Err(StoreError::Backend("test.object_lost"));
        }
        let sql = self
            .map
            .borrow_mut()
            .entry(id.clone())
            .or_insert_with(|| NativeSql::new(self.now.get()))
            .clone();
        sql.set_time(self.now.get());
        DoStore::open(sql, id, scope.clone())
    }
    fn reachable(&self) -> Vec<ExecutionId> {
        self.map.borrow().keys().cloned().collect()
    }
}

struct TestWebhookHost;
impl runbook_api::webhooks::WebhookHost for TestWebhookHost {
    type Sealer = AesSealer;
    fn sealer(&self) -> Option<&AesSealer> {
        None
    }
    fn random32(&self) -> Option<[u8; 32]> {
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).ok().map(|()| bytes)
    }
    fn target_policy(&self) -> TargetPolicy {
        TargetPolicy {
            allow_private: true,
            allow_link_local: false,
        }
    }
    async fn vet_target(&self, _url: &str) -> Result<(), &'static str> {
        Ok(())
    }
}

#[derive(Clone)]
struct NativeHost {
    /// CF13 review round 1: the run budget in force, when not the default.
    run_budget: Option<u64>,
    /// The per-run step cap in force (`RUNBOOK_LIMIT_STEP_RUNS`).
    step_cap: Option<u64>,
    d1: NativeD1,
    limits: runbook_worker::limits::MemoryLimits,
    objects: NativeObjects,
    blobs: MemoryBucket,
    user_content: MemoryBucket,
}
impl CfHost for NativeHost {
    type Sql = NativeSql;
    type D1 = NativeD1;
    type Bucket = MemoryBucket;
    type Objects = NativeObjects;
    type WebhookHost = TestWebhookHost;
    type Limits = runbook_worker::limits::MemoryLimits;
    fn limits(&self) -> runbook_worker::limits::MemoryLimits {
        self.limits.clone()
    }
    fn shards(&self) -> runbook_worker::shards::D1Shards<NativeD1> {
        runbook_worker::shards::D1Shards::single(self.d1.clone())
    }
    fn buckets(&self) -> (MemoryBucket, MemoryBucket) {
        (self.blobs.clone(), self.user_content.clone())
    }
    fn objects(&self) -> NativeObjects {
        self.objects.clone()
    }
    fn webhook_host(&self) -> TestWebhookHost {
        TestWebhookHost
    }
    fn now_ms(&self) -> i64 {
        self.objects.now.get()
    }
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), NoEntropy> {
        getrandom::fill(bytes).map_err(|_| NoEntropy)
    }
    fn run_budget(&self) -> u64 {
        self.run_budget
            .unwrap_or(runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES)
    }
    fn step_cap(&self) -> Option<u64> {
        self.step_cap
    }
}

fn config() -> RouterConfig {
    runbook_worker::cloudflare::router_config("https://runbook.test", 1 << 20)
}

struct Fixture {
    host: NativeHost,
    router: Router<CfBackends<NativeHost>>,
    counter: Cell<u64>,
}
struct Reply {
    status: u16,
    body: Value,
    etag: Option<String>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_run_budget(None)
    }
    fn with_run_budget(run_budget: Option<u64>) -> Self {
        Self::with_limits(run_budget, None)
    }
    fn with_limits(run_budget: Option<u64>, step_cap: Option<u64>) -> Self {
        let d1 = NativeD1::new(START_MS);
        run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
        let objects = NativeObjects::default();
        objects.now.set(START_MS);
        let host = NativeHost {
            run_budget,
            step_cap,
            d1,
            limits: Default::default(),
            objects,
            blobs: MemoryBucket::default(),
            user_content: MemoryBucket::default(),
        };
        Self {
            router: Router::new(CfBackends::new(host.clone()), config()),
            host,
            counter: Cell::new(0),
        }
    }
    fn nonce(&self) -> String {
        self.counter.set(self.counter.get() + 1);
        let mut random = [0u8; 10];
        random[9] = u8::try_from(self.counter.get() % 256).unwrap();
        random[8] = u8::try_from(self.counter.get() / 256).unwrap();
        runbook_api::backends::encode_id("", self.host.now_ms(), random).unwrap()
    }
    /// One request, then the host settles every object (as a DO does).
    fn call(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Value,
        key: Option<&str>,
        if_match: Option<&str>,
    ) -> Reply {
        self.host
            .objects
            .now
            .set(self.host.objects.now.get() + 1_000);
        self.host.d1.set_time(self.host.objects.now.get());
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let request = Request {
            method: method.into(),
            path: path.into(),
            query: serde_urlencoded::from_str(query).unwrap(),
            body,
            credential: (!token.is_empty()).then(|| Credential::Bearer(token.into())),
            idempotency_key: key.map(str::to_owned),
            request_id: format!("req_{}", self.counter.get()),
            new_id: self.nonce(),
            if_match: if_match.map(str::to_owned),
            client_ip: None,
        };
        let response = run(self.router.handle(request));
        let encoded = runbook_api::http::encode(response);
        for id in self.host.objects.reachable() {
            if let Some(scope) = run(runbook_store_d1::catalog::execution_owner(
                &self.host.d1,
                &id,
            ))
            .unwrap()
            {
                run(runbook_worker::settle::settle(
                    &self.router.backends,
                    &id,
                    &scope,
                ))
                .unwrap();
            }
        }
        Reply {
            status: encoded.status,
            etag: encoded.header_value("etag").map(str::to_owned),
            body: serde_json::from_slice(&encoded.body).unwrap_or(Value::Null),
        }
    }
}

fn source(user: &str) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: verify, title: Verify}}\n\
         spec: {{steps: [{{id: check, title: Check the deploy, assignee: \"user:{user}\"}}]}}\n"
    )
}

#[test]
fn a_run_goes_from_publish_to_its_summary_and_inbox_in_d1() {
    let f = Fixture::new();
    let out = run(runbook_worker::bootstrap::run(
        &f.host,
        runbook_worker::bootstrap::BootstrapRequest {
            organization: "Acme".into(),
            user_name: "Owner".into(),
            project_slug: "ops".into(),
            owner_email: None,
        },
    ))
    .unwrap();
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    // A second bootstrap is refused.
    assert!(
        run(runbook_worker::bootstrap::run(
            &f.host,
            runbook_worker::bootstrap::BootstrapRequest {
                organization: "Other".into(),
                user_name: "Other".into(),
                project_slug: "ops".into(),
                owner_email: None,
            },
        ))
        .is_err()
    );

    let me = f.call(pat, "GET", "/v1/me", Value::Null, None, None);
    assert_eq!(me.status, 200, "{}", me.body);

    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({"source": source(out.user_id.as_str())}),
        Some("create-1"),
        None,
    );
    assert_eq!(created.status, 201, "{}", created.body);
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let draft = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}/draft"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(draft.status, 200, "{}", draft.body);
    let published = f.call(
        pat,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        json!({}),
        Some("publish-1"),
        draft.etag.as_deref(),
    );
    assert_eq!(published.status, 201, "{}", published.body);

    let started = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "verify"}),
        Some("start-1"),
        None,
    );
    assert_eq!(started.status, 201, "{}", started.body);
    let exe = started.body["id"].as_str().unwrap().to_owned();
    // The retried start answers the same run.
    let again = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "verify"}),
        Some("start-1"),
        None,
    );
    assert_eq!(again.body["id"], started.body["id"], "{}", again.body);

    let read = f.call(
        pat,
        "GET",
        &format!("/v1/executions/{exe}"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(read.status, 200, "{}", read.body);
    let sr = read.body["steps"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{}", read.body))
        .to_owned();
    // UI-2: the view carries the run's top-level progress.
    assert_eq!(read.body["progress"]["done"], 0, "{}", read.body);
    assert!(
        read.body["progress"]["total"].as_u64().unwrap_or(0) >= 1,
        "{}",
        read.body
    );
    // The run's summary and its inbox row reached D1 through the applier.
    let tasks = f.call(pat, "GET", "/v1/me/tasks", Value::Null, None, None);
    assert_eq!(tasks.status, 200, "{}", tasks.body);
    assert_eq!(
        tasks.body["items"].as_array().map(Vec::len),
        Some(1),
        "{}",
        tasks.body
    );
    // UI-2: and each task says since when it has been open.
    assert!(
        tasks.body["items"][0]["since_ms"].as_i64().unwrap_or(0) > 0,
        "{}",
        tasks.body
    );
    let search = f.call(
        pat,
        "GET",
        &format!("/v1/projects/{project}/executions"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(search.status, 200, "{}", search.body);
    assert_eq!(search.body["items"][0]["id"], exe, "{}", search.body);

    // CF2-9: a minted execution token is routed to its run in D1 before
    // the object stores it, and the bearer then reaches the run.
    let minted = f.call(
        &out.service_token,
        "POST",
        &format!("/v1/executions/{exe}/tokens"),
        json!({"session_id": "agent-1"}),
        Some("mint-1"),
        None,
    );
    assert_eq!(minted.status, 201, "{}", minted.body);
    let access = minted.body["access_token"].as_str().unwrap().to_owned();
    let route = run(runbook_store_d1::D1Credentials::new(f.host.d1.clone())
        .execution_route(&runbook_core::credentials::secret_hash(&access)))
    .unwrap();
    assert_eq!(route.map(|(_, id)| id.to_string()), Some(exe.clone()));
    let next = f.call(
        &access,
        "GET",
        &format!("/v1/executions/{exe}/next"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(next.status, 200, "{}", next.body);

    let claim = f.call(
        pat,
        "POST",
        &format!("/v1/step-runs/{sr}/claim"),
        json!({"session_id": "web"}),
        Some("claim-1"),
        None,
    );
    assert_eq!(claim.status, 200, "{}", claim.body);
    let reported = f.call(
        pat,
        "POST",
        &format!("/v1/step-runs/{sr}/report"),
        json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded", "verdict": null,
               "outputs": {}, "summary": "done", "evidence": [], "logs": []}),
        Some("report-1"),
        None,
    );
    assert_eq!(reported.status, 200, "{}", reported.body);

    let done = f.call(
        pat,
        "GET",
        &format!("/v1/executions/{exe}"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(done.body["status"], "succeeded", "{}", done.body);
    let search = f.call(
        pat,
        "GET",
        &format!("/v1/projects/{project}/executions"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(
        search.body["items"][0]["status"], "succeeded",
        "{}",
        search.body
    );
    let tasks = f.call(pat, "GET", "/v1/me/tasks", Value::Null, None, None);
    assert_eq!(
        tasks.body["items"].as_array().map(Vec::len),
        Some(0),
        "{}",
        tasks.body
    );
    let report = f.call(
        pat,
        "GET",
        &format!("/v1/executions/{exe}/report"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(report.status, 200, "{}", report.body);
}

/// CF0/CF3: the router's future for one request on the Cloudflare
/// backends stays small (each area is boxed), so deep poll chains fit the
/// Worker's stack. Measured on a 64-bit host; wasm32 pointers are half the
/// width, so the Worker's future is no larger.
#[test]
fn the_router_future_on_cloudflare_backends_is_small() {
    let f = Fixture::new();
    let request = Request {
        method: "GET".into(),
        path: "/v1/me".into(),
        query: Default::default(),
        body: Value::Null,
        credential: None,
        idempotency_key: None,
        request_id: "req_size".into(),
        new_id: f.nonce(),
        if_match: None,
        client_ip: None,
    };
    let size = std::mem::size_of_val(&f.router.handle(request));
    println!("router future on CfBackends: {size} bytes");
    assert!(size <= 1024, "the router future is {size} bytes");
}

/// Review fix 2: D1 reserved a start but its object never initialized. A
/// retry with the same key finishes that run (same ID) instead of
/// answering 503 for the whole replay window.
#[test]
fn a_retry_finishes_a_start_whose_object_write_was_lost() {
    let f = Fixture::new();
    let out = run(runbook_worker::bootstrap::run(
        &f.host,
        runbook_worker::bootstrap::BootstrapRequest {
            organization: "Acme".into(),
            user_name: "Owner".into(),
            project_slug: "ops".into(),
            owner_email: None,
        },
    ))
    .unwrap();
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({"source": source(out.user_id.as_str())}),
        Some("create-1"),
        None,
    );
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let draft = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}/draft"),
        Value::Null,
        None,
        None,
    );
    let published = f.call(
        pat,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        json!({}),
        Some("publish-1"),
        draft.etag.as_deref(),
    );
    assert_eq!(published.status, 201, "{}", published.body);
    f.host.objects.lose_next.set(true);
    let lost = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "verify"}),
        Some("start-lost"),
        None,
    );
    assert!(lost.status >= 500, "{} {}", lost.status, lost.body);
    let saved: String = {
        use runbook_store_d1::{
            D1,
            sql::{Columns, prepare},
        };
        let rows = run(f
            .host
            .d1
            .all(prepare("SELECT execution_id FROM start_replay")))
        .unwrap();
        rows[0].string(0).unwrap()
    };
    let retried = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "verify"}),
        Some("start-lost"),
        None,
    );
    assert_eq!(retried.status, 201, "{}", retried.body);
    assert_eq!(retried.body["id"], saved.as_str());
    let read = f.call(
        pat,
        "GET",
        &format!("/v1/executions/{saved}"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(read.status, 200, "{}", read.body);
}

/// CF5: a rerun on Cloudflare. The request reaches the original run's
/// object; the new run gets its own object (natively every object is
/// reachable; on Workers the object protocol reaches it), carries
/// `rerun_of`, replays under its key, and its summary reaches D1.
#[test]
fn a_rerun_starts_a_new_run_in_its_own_object() {
    let f = Fixture::new();
    let out = run(runbook_worker::bootstrap::run(
        &f.host,
        runbook_worker::bootstrap::BootstrapRequest {
            organization: "Acme".into(),
            user_name: "Owner".into(),
            project_slug: "ops".into(),
            owner_email: None,
        },
    ))
    .unwrap();
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({"source": source(out.user_id.as_str())}),
        Some("create-1"),
        None,
    );
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let draft = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}/draft"),
        Value::Null,
        None,
        None,
    );
    let published = f.call(
        pat,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        json!({}),
        Some("publish-1"),
        draft.etag.as_deref(),
    );
    assert_eq!(published.status, 201, "{}", published.body);
    let started = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "verify", "title": "First"}),
        Some("start-1"),
        None,
    );
    assert_eq!(started.status, 201, "{}", started.body);
    let exe = started.body["id"].as_str().unwrap().to_owned();

    let rerun = f.call(
        pat,
        "POST",
        &format!("/v1/executions/{exe}/rerun"),
        json!({}),
        Some("rerun-1"),
        None,
    );
    assert_eq!(rerun.status, 201, "{}", rerun.body);
    let new = rerun.body["id"].as_str().unwrap().to_owned();
    assert_ne!(new, exe);
    // A retry of the rerun answers the same new run.
    let again = f.call(
        pat,
        "POST",
        &format!("/v1/executions/{exe}/rerun"),
        json!({}),
        Some("rerun-1"),
        None,
    );
    assert_eq!(again.body["id"], rerun.body["id"], "{}", again.body);
    let read = f.call(
        pat,
        "GET",
        &format!("/v1/executions/{new}"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(read.body["rerun_of"], exe, "{}", read.body);
    assert_eq!(read.body["title"], "First", "{}", read.body);
    let search = f.call(
        pat,
        "GET",
        &format!("/v1/projects/{project}/executions"),
        Value::Null,
        None,
        None,
    );
    let ids: Vec<&str> = search.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(
        ids.contains(&new.as_str()) && ids.contains(&exe.as_str()),
        "{}",
        search.body
    );
    // A rerun of an unknown run is 404.
    let missing = f.call(
        pat,
        "POST",
        "/v1/executions/exe_00000000000000000000000099/rerun",
        json!({}),
        Some("rerun-2"),
        None,
    );
    assert_eq!(missing.status, 404, "{}", missing.body);
}

/// CF12 review round 1: a create whose source is at the compiler's 1 MiB
/// ceiling with every byte JSON-escaped (a 2 MB request and response) fits
/// D1's 2,000,000-byte row limit, which the native D1 enforces: its replay
/// row keeps neither whole. A retry answers the recorded response, and the
/// key with another body is refused.
#[test]
fn a_ceiling_source_create_and_its_replay_row_fit_d1s_row_limit() {
    let f = Fixture::new();
    let out = run(runbook_worker::bootstrap::run(
        &f.host,
        runbook_worker::bootstrap::BootstrapRequest {
            organization: "Acme".into(),
            user_name: "Owner".into(),
            project_slug: "ops".into(),
            owner_email: None,
        },
    ))
    .unwrap();
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let head = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: ceiling, title: Ceiling}}\n\
         spec: {{steps: [{{id: check, title: Check, assignee: \"user:{}\"}}]}}\n",
        out.user_id
    );
    // A comment of quotes, each of which JSON escapes to two bytes.
    let ceiling = runbook_spec::authoring::MAX_SOURCE_BYTES;
    let source = format!("{head}#{}\n", "\"".repeat(ceiling - head.len() - 2));
    assert_eq!(source.len(), ceiling);
    let body = json!({ "source": source });
    assert!(body.to_string().len() > 2_000_000);
    let path = format!("/v1/projects/{project}/runbooks");
    let created = f.call(pat, "POST", &path, body.clone(), Some("ceiling"), None);
    assert_eq!(created.status, 201, "{}", created.body["code"]);
    assert_eq!(created.body["source"], source);
    let retried = f.call(pat, "POST", &path, body.clone(), Some("ceiling"), None);
    assert_eq!(retried.status, 201, "{}", retried.body["code"]);
    assert_eq!(retried.body, created.body, "the recorded response");
    assert_eq!(retried.etag, created.etag);
    let mut other = body.clone();
    other["source"] = json!(source.replace("Ceiling", "Others!"));
    let reused = f.call(pat, "POST", &path, other, Some("ceiling"), None);
    assert_eq!(reused.status, 409, "{}", reused.body);
    assert_eq!(reused.body["code"], "request.idempotency_key_reused");
    // The replay row is small: the payload is a digest and the response a
    // reference to the project's blobs.
    let rows = run(runbook_store_d1::D1::all(
        &f.host.d1,
        runbook_store_d1::prepare(
            "SELECT length(CAST(payload AS BLOB)) + length(CAST(response AS BLOB)) \
             FROM directory_request WHERE key = 'ceiling'",
        ),
    ))
    .unwrap();
    assert_eq!(rows.len(), 1);
    let runbook_store_d1::sql::Value::Integer(size) = rows[0][0] else {
        panic!("{:?}", rows[0][0]);
    };
    assert!(size < 1_024, "{size} B");
}

/// CF12b: the replay row's `response` column, and the SHA-256 (hex) of the
/// response JSON kept in R2 that it names.
fn replay_reference(f: &Fixture, key: &str) -> (String, String) {
    let rows = run(runbook_store_d1::D1::all(
        &f.host.d1,
        runbook_store_d1::prepare("SELECT response FROM directory_request WHERE key = ?").bind(key),
    ))
    .unwrap();
    assert_eq!(rows.len(), 1, "{key}");
    let runbook_store_d1::sql::Value::Text(column) = &rows[0][0] else {
        panic!("{:?}", rows[0][0]);
    };
    assert!(!column.starts_with('{'), "kept in R2: {}", &column[..40]);
    // `{tag}:{project}:{sha256 hex}…`
    let digest = column.splitn(3, ':').nth(2).unwrap()[..64].to_owned();
    (column.clone(), digest)
}

/// CF12b: the blob bucket's objects whose bytes hash to `digest`.
fn objects_with(f: &Fixture, digest: &str) -> Vec<String> {
    use runbook_store_r2::ObjectBucket;
    f.host
        .blobs
        .keys()
        .into_iter()
        .filter(|key| {
            let bytes = run(f.host.blobs.get(key)).unwrap().unwrap();
            runbook_core::portable::sha256_hex(&bytes) == digest
        })
        .collect()
}

fn large_source(user: &runbook_model::UserId, id: &str) -> String {
    let head = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: {id}, title: Large}}\n\
         spec: {{steps: [{{id: check, title: Check, assignee: \"user:{user}\"}}]}}\n"
    );
    // Over the 64 KiB a replay row keeps inline, in the response too.
    format!("{head}#{}\n", "x".repeat(100_000))
}

fn bootstrapped(f: &Fixture) -> runbook_worker::bootstrap::BootstrapOutput {
    run(runbook_worker::bootstrap::run(
        &f.host,
        runbook_worker::bootstrap::BootstrapRequest {
            organization: "Acme".into(),
            user_name: "Owner".into(),
            project_slug: "ops".into(),
            owner_email: None,
        },
    ))
    .unwrap()
}

/// CF12b (finding 2): a replay response kept in R2 is not part of the
/// project: an export after large creates carries none of them, and they go
/// with their rows once those expire (swept by the next authoring write).
#[test]
fn replay_responses_are_not_exported_and_go_with_their_rows() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let path = format!("/v1/projects/{}/runbooks", out.project_id);
    let mut digests = Vec::new();
    for (key, id) in [("large-a", "large-a"), ("large-b", "large-b")] {
        let body = json!({ "source": large_source(&out.user_id, id) });
        let created = f.call(pat, "POST", &path, body.clone(), Some(key), None);
        assert_eq!(created.status, 201, "{}", created.body["code"]);
        let (_, digest) = replay_reference(&f, key);
        assert_eq!(objects_with(&f, &digest).len(), 1, "stored once");
        digests.push(digest);
        // It replays.
        let retried = f.call(pat, "POST", &path, body, Some(key), None);
        assert_eq!(retried.body, created.body);
    }
    let scope = TenantScope {
        org_id: out.org_id.clone(),
        project_id: out.project_id.clone(),
    };
    use runbook_api::backends::Backends;
    let store = f.router.backends.execution(scope);
    let (data, _) = run(runbook_worker::portable::export_within(
        &store,
        runbook_worker::portable::ImportBudget::PAID,
    ))
    .unwrap();
    for blob in &data.rows.blobs {
        let hash = runbook_core::portable::sha256_hex(&blob.bytes);
        assert!(
            !digests.contains(&hash),
            "a replay response exported: {:?}",
            blob.key
        );
    }
    // A day and a bit later the rows have expired; the next authoring write
    // in the organization sweeps them, and their objects with them.
    f.host
        .objects
        .now
        .set(f.host.objects.now.get() + 25 * 3_600_000);
    let small = json!({ "source": source(out.user_id.as_str()) });
    let created = f.call(pat, "POST", &path, small, Some("after"), None);
    assert_eq!(created.status, 201, "{}", created.body["code"]);
    for digest in &digests {
        assert_eq!(objects_with(&f, digest), Vec::<String>::new(), "swept");
    }
    let left = run(runbook_store_d1::D1::all(
        &f.host.d1,
        runbook_store_d1::prepare(
            "SELECT key FROM directory_request WHERE key IN ('large-a', 'large-b')",
        ),
    ))
    .unwrap();
    assert!(left.is_empty(), "{left:?}");
}

/// CF12b (finding 2): an organization with no later authoring write still
/// loses its expired replay responses, to the daily cron pass.
#[test]
fn the_daily_pass_sweeps_expired_replay_responses() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let path = format!("/v1/projects/{}/runbooks", out.project_id);
    let body = json!({ "source": large_source(&out.user_id, "large") });
    let created = f.call(pat, "POST", &path, body, Some("large"), None);
    assert_eq!(created.status, 201, "{}", created.body["code"]);
    let (_, digest) = replay_reference(&f, "large");
    // Not yet expired: the pass leaves it.
    let now = f.host.objects.now.get();
    assert_eq!(
        run(runbook_worker::cron::replay_pass(&f.router.backends, now))
            .unwrap()
            .rows,
        0
    );
    assert_eq!(objects_with(&f, &digest).len(), 1);
    let later = now + 25 * 3_600_000;
    f.host.d1.set_time(later);
    assert_eq!(
        run(runbook_worker::cron::replay_pass(&f.router.backends, later))
            .unwrap()
            .rows,
        1
    );
    assert_eq!(objects_with(&f, &digest), Vec::<String>::new());
    let left = run(runbook_store_d1::D1::all(
        &f.host.d1,
        runbook_store_d1::prepare("SELECT key FROM directory_request WHERE key = 'large'"),
    ))
    .unwrap();
    assert!(left.is_empty(), "{left:?}");
}

/// CF12b (finding 2): a row CF12 wrote, naming the response by its digest
/// in the project's shared blobs (`blob:`), keeps replaying until it
/// expires.
#[test]
fn a_replay_row_cf12_wrote_still_replays() {
    use runbook_store_r2::ObjectBucket;
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let path = format!("/v1/projects/{}/runbooks", out.project_id);
    let body = json!({ "source": large_source(&out.user_id, "large") });
    let created = f.call(pat, "POST", &path, body.clone(), Some("large"), None);
    assert_eq!(created.status, 201, "{}", created.body["code"]);
    let (_, digest) = replay_reference(&f, "large");
    let [object] = &objects_with(&f, &digest)[..] else {
        panic!("one object");
    };
    // Rewrite the row and its object the way CF12 kept them.
    let bytes = run(f.host.blobs.get(object)).unwrap().unwrap();
    run(f.host.blobs.delete(object)).unwrap();
    let shared = format!("t/{}/{}/shared/{digest}", out.org_id, out.project_id);
    assert!(run(f.host.blobs.put_new(&shared, bytes)).unwrap());
    run(runbook_store_d1::D1::batch(
        &f.host.d1,
        vec![
            runbook_store_d1::prepare(
                "UPDATE directory_request SET response = ? WHERE key = 'large'",
            )
            .bind(format!("blob:{}:{digest}", out.project_id)),
        ],
    ))
    .unwrap();
    let retried = f.call(pat, "POST", &path, body, Some("large"), None);
    assert_eq!(retried.status, 201, "{}", retried.body);
    assert_eq!(retried.body, created.body);
}

/// CF12b (finding 3): request values a store keeps whole in one D1 or
/// Durable Object row are capped in the shared API, so the Workers build
/// refuses them as natively, with `422 request.value_too_large`, instead of
/// a 503 from D1's 2,000,000-byte row limit that no retry could get past.
/// (The actor routes' caps are tested in `runbook-api`'s `actor_requests`.)
#[test]
fn values_kept_whole_in_one_row_are_capped() {
    use runbook_api::field_caps::{CODE, DATA_BYTES, NAME_BYTES, SKILL_FILES, TEXT_BYTES};
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let org = out.org_id.to_string();
    let project = out.project_id.to_string();
    let s = |n: usize| "a".repeat(n);
    let refused = |reply: Reply, what: &str| {
        assert_eq!(reply.status, 422, "{what}: {}", reply.body);
        assert_eq!(reply.body["code"], CODE, "{what}: {}", reply.body);
    };
    let mut key = 0;
    let mut post = |path: &str, body: Value| {
        key += 1;
        f.call(pat, "POST", path, body, Some(&format!("k{key}")), None)
    };
    // Directory names, at the cap and one byte past it (a value past D1's
    // row limit was a 503 before).
    let projects = format!("/v1/orgs/{org}/projects");
    let ok = post(&projects, json!({"slug": "named", "title": s(NAME_BYTES)}));
    assert_eq!(ok.status, 201, "{}", ok.body);
    refused(
        post(
            &projects,
            json!({"slug": "long", "title": s(NAME_BYTES + 1)}),
        ),
        "project title",
    );
    refused(
        post(&projects, json!({"slug": "huge", "title": s(2_100_000)})),
        "a project title past D1's row limit",
    );
    refused(
        post(
            &format!("/v1/orgs/{org}/users"),
            json!({"name": s(NAME_BYTES + 1)}),
        ),
        "user name",
    );
    refused(
        post(
            &format!("/v1/orgs/{org}/teams"),
            json!({"slug": "t", "title": s(NAME_BYTES + 1)}),
        ),
        "team title",
    );
    refused(
        post(
            &format!("/v1/orgs/{org}/invitations"),
            json!({"email": format!("{}@example.com", s(NAME_BYTES))}),
        ),
        "invited email",
    );
    // A skill bundle's files (PRD: 200) and its reference.
    let file = |n: usize| json!({"path": format!("f{n}.md"), "content": ""});
    let skills = format!("/v1/projects/{project}/skills");
    refused(
        post(
            &skills,
            json!({"files": (0..=SKILL_FILES).map(file).collect::<Vec<_>>()}),
        ),
        "skill files",
    );
    refused(
        post(
            &skills,
            json!({"files": [file(0)], "reference": s(TEXT_BYTES + 1)}),
        ),
        "skill reference",
    );
    // A start's data, and a minted token's session.
    let created = post(
        &format!("/v1/projects/{project}/runbooks"),
        json!({"source": source(out.user_id.as_str())}),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let draft = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}/draft"),
        Value::Null,
        None,
        None,
    );
    let published = f.call(
        pat,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        json!({}),
        Some("publish"),
        draft.etag.as_deref(),
    );
    assert_eq!(published.status, 201, "{}", published.body);
    let executions = format!("/v1/projects/{project}/executions");
    refused(
        post(
            &executions,
            json!({"runbook": "verify", "fields": {"k": s(DATA_BYTES)}}),
        ),
        "start data",
    );
    let started = post(
        &executions,
        json!({"runbook": "verify", "title": s(TEXT_BYTES)}),
    );
    assert_eq!(started.status, 201, "{}", started.body);
    let exe = started.body["id"].as_str().unwrap().to_owned();
    let mint = |session: String, key: &str| {
        f.call(
            &out.service_token,
            "POST",
            &format!("/v1/executions/{exe}/tokens"),
            json!({ "session_id": session }),
            Some(key),
            None,
        )
    };
    refused(mint(s(NAME_BYTES + 1), "mint-long"), "token session");
    assert_eq!(mint(s(NAME_BYTES), "mint-ok").status, 201);
}

/// CF12b review round 1 (finding 2): `n` replay rows of the bootstrapped
/// project, expired by `now`, each naming an object in the blobs bucket.
/// Row `i` expires at `START_MS + i` and its key sorts in the opposite
/// order, so the table's own order is not expiry order. Returns the object
/// names, oldest first.
fn expired_replay_rows(
    f: &Fixture,
    out: &runbook_worker::bootstrap::BootstrapOutput,
    n: usize,
) -> Vec<String> {
    use runbook_store_r2::ObjectBucket;
    let mut names = Vec::new();
    let mut inserts = Vec::new();
    for i in 0..n {
        let name = format!("{i:064x}.{i:032x}");
        let key = format!("t/{}/{}/replay/{name}", out.org_id, out.project_id);
        assert!(run(f.host.blobs.put_new(&key, b"{}".to_vec())).unwrap());
        inserts.push(
            runbook_store_d1::prepare(
                "INSERT INTO directory_request (org_id, principal, operation, key, payload, \
                 response, expires_at) VALUES (?, 'p', 'runbook.create', ?, '{}', ?, ?)",
            )
            .bind(out.org_id.as_str())
            .bind(format!("k{:05}", n - i))
            .bind(format!("replay:{}:{name}", out.project_id))
            .bind(START_MS + i as i64),
        );
        names.push(name);
    }
    run(runbook_store_d1::D1::batch(&f.host.d1, inserts)).unwrap();
    names
}

fn replay_rows_left(f: &Fixture) -> usize {
    let rows = run(runbook_store_d1::D1::all(
        &f.host.d1,
        runbook_store_d1::prepare(
            "SELECT key FROM directory_request WHERE response LIKE 'replay:%'",
        ),
    ))
    .unwrap();
    rows.len()
}

/// CF12b review round 1 (finding 2): one daily pass sweeps more than one
/// batch: it repeats until a batch comes back short (bounded), instead of
/// taking 50 rows a day.
#[test]
fn the_daily_pass_sweeps_every_batch_of_expired_replays() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let names = expired_replay_rows(&f, &out, 120);
    let later = START_MS + 25 * 3_600_000;
    f.host.d1.set_time(later);
    let swept = run(runbook_worker::cron::replay_pass(&f.router.backends, later)).unwrap();
    assert_eq!(swept.rows, 120);
    assert_eq!(swept.failed, 0);
    assert_eq!(replay_rows_left(&f), 0);
    for name in &names {
        assert!(
            !f.host.blobs.keys().iter().any(|k| k.ends_with(name)),
            "{name} swept"
        );
    }
}

/// Replay objects whose delete fails for the names in `failing`, and the
/// order deletes were asked for.
#[derive(Default)]
struct FlakyReplies {
    failing: Vec<String>,
    asked: RefCell<Vec<String>>,
}
impl runbook_store_d1::replay::ReplayBlobs for FlakyReplies {
    fn get(
        &self,
        _: &runbook_model::OrganizationId,
        _: &runbook_model::ProjectId,
        _: [u8; 32],
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(None) })
    }
    fn get_reply(
        &self,
        _: &runbook_model::OrganizationId,
        _: &runbook_model::ProjectId,
        _: &str,
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(None) })
    }
    fn delete_reply(
        &self,
        _: &runbook_model::OrganizationId,
        _: &runbook_model::ProjectId,
        name: &str,
    ) -> runbook_store_d1::sources::SourceFuture<'_, ()> {
        self.asked.borrow_mut().push(name.to_owned());
        let fails = self.failing.iter().any(|f| f == name);
        Box::pin(async move {
            if fails {
                Err(StoreError::Backend("test.r2_down"))
            } else {
                Ok(())
            }
        })
    }
}

/// CF12b review round 1 (finding 2): the sweep takes the oldest rows first,
/// and a row whose object delete keeps failing does not hold a place in
/// later batches of the same sweep: it is skipped, counted, and kept (with
/// its object) for the next sweep.
#[test]
fn a_sweep_goes_oldest_first_and_skips_a_failing_delete() {
    use runbook_store_d1::replay::{SWEEP_BATCH, sweep};
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let n = SWEEP_BATCH as usize + 10;
    let names = expired_replay_rows(&f, &out, n);
    let blobs = FlakyReplies {
        failing: names[..3].to_vec(),
        ..Default::default()
    };
    let later = START_MS + 25 * 3_600_000;
    let swept = run(sweep(&f.host.d1, None, later, &blobs, 5)).unwrap();
    assert_eq!((swept.rows, swept.failed), (n - 3, 3));
    assert_eq!(replay_rows_left(&f), 3, "the failed rows stay");
    // Oldest first, each asked once.
    assert_eq!(*blobs.asked.borrow(), names);
    // An organization's own sweep (before a write) takes one batch.
    let f = Fixture::new();
    let out = bootstrapped(&f);
    expired_replay_rows(&f, &out, n);
    let blobs = FlakyReplies::default();
    let swept = run(sweep(&f.host.d1, Some(&out.org_id), later, &blobs, 1)).unwrap();
    assert_eq!((swept.rows, swept.failed), (SWEEP_BATCH as usize, 0));
    assert_eq!(blobs.asked.borrow()[..], names[..SWEEP_BATCH as usize]);
}

/// CF12b review round 2 (finding 4): a row whose object delete fails backs
/// off (an hour, doubling per failure, up to a week), so later sweeps move
/// past it. Before, the 1,000 oldest failing rows took all 20 of the daily
/// pass's batches every day, and 50 failing rows of one organization took
/// the one batch of every authoring write's sweep.
#[test]
fn failing_rows_back_off_so_later_sweeps_move_past_them() {
    use runbook_store_d1::replay::{CRON_SWEEP_PASSES, SWEEP_BATCH, sweep};
    const HOUR: i64 = 3_600_000;
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let failing = SWEEP_BATCH as usize * CRON_SWEEP_PASSES;
    let names = expired_replay_rows(&f, &out, failing + 10);
    let blobs = FlakyReplies {
        failing: names[..failing].to_vec(),
        ..Default::default()
    };
    let later = START_MS + 25 * HOUR;
    let daily = |now: i64| run(sweep(&f.host.d1, None, now, &blobs, CRON_SWEEP_PASSES)).unwrap();
    let first = daily(later);
    assert_eq!((first.rows, first.failed), (0, failing));
    // The next sweep moves past the failing rows to the ones it can take.
    let second = daily(later + 60_000);
    assert_eq!((second.rows, second.failed), (10, 0));
    assert_eq!(blobs.asked.borrow().len(), failing + 10, "none asked twice");
    // Until its hour has passed, a failing row is not tried again; then
    // it is, and backs off two hours.
    assert_eq!(daily(later + HOUR - 1), Default::default());
    let third = daily(later + HOUR);
    assert_eq!((third.rows, third.failed), (0, failing));
    assert_eq!(daily(later + 2 * HOUR), Default::default());
    assert_eq!(daily(later + 3 * HOUR).failed, failing);

    // An organization's own sweep (one batch, before each authoring write).
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let n = SWEEP_BATCH as usize;
    let names = expired_replay_rows(&f, &out, n + 5);
    let blobs = FlakyReplies {
        failing: names[..n].to_vec(),
        ..Default::default()
    };
    let own = |now: i64| run(sweep(&f.host.d1, Some(&out.org_id), now, &blobs, 1)).unwrap();
    assert_eq!((own(later).rows, own(later + 1).rows), (0, 5));
    assert_eq!(
        replay_rows_left(&f),
        n,
        "the failing rows stay, with their objects"
    );
}

/// Replay objects whose delete fails, and which, while the sweep waits on
/// that delete, have a writer replace the row (`replay::record`, the
/// authoring writer's statement) with a live one under a new response.
struct ReplacedMidSweep<'a> {
    d1: &'a NativeD1,
    org: runbook_model::OrganizationId,
    project: runbook_model::ProjectId,
    key: &'static str,
    now: i64,
}
impl runbook_store_d1::replay::ReplayBlobs for ReplacedMidSweep<'_> {
    fn get(
        &self,
        _: &runbook_model::OrganizationId,
        _: &runbook_model::ProjectId,
        _: [u8; 32],
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(None) })
    }
    fn get_reply(
        &self,
        _: &runbook_model::OrganizationId,
        _: &runbook_model::ProjectId,
        _: &str,
    ) -> runbook_store_d1::sources::SourceFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { Ok(None) })
    }
    fn delete_reply(
        &self,
        _: &runbook_model::OrganizationId,
        _: &runbook_model::ProjectId,
        _: &str,
    ) -> runbook_store_d1::sources::SourceFuture<'_, ()> {
        use runbook_store_d1::replay::{RowKey, record};
        let statement = record(
            RowKey {
                org: &self.org,
                principal: "p",
                operation: "runbook.create",
                key: self.key,
            },
            &json!({}),
            format!(
                "replay:{}:{}.{}",
                self.project,
                "f".repeat(64),
                "e".repeat(32)
            ),
            self.now + 86_400_000,
            self.now,
        );
        Box::pin(async move {
            runbook_store_d1::D1::run(self.d1, statement)
                .await
                .map_err(|_| StoreError::Backend("test.d1"))?;
            Err(StoreError::Backend("test.r2_down"))
        })
    }
}

/// CF12c (CF12b's final review, item 5): a row a writer replaced while
/// the sweep waited on its old object's delete is live again under a new
/// response, so the failed delete's backoff is not its own. Before the
/// fix, the `UPDATE` matched it by `rowid` alone and gave the new row an
/// hour's `sweep_after` and a failure. The `DELETE` already had this guard.
#[test]
fn a_row_replaced_mid_sweep_does_not_take_the_old_rows_backoff() {
    use runbook_store_d1::replay::sweep;
    let f = Fixture::new();
    let out = bootstrapped(&f);
    expired_replay_rows(&f, &out, 1);
    let later = START_MS + 25 * 3_600_000;
    let blobs = ReplacedMidSweep {
        d1: &f.host.d1,
        org: out.org_id.clone(),
        project: out.project_id.clone(),
        key: "k00001",
        now: later,
    };
    let swept = run(sweep(&f.host.d1, None, later, &blobs, 1)).unwrap();
    assert_eq!((swept.rows, swept.failed), (0, 1));
    let rows = run(runbook_store_d1::D1::all(
        &f.host.d1,
        runbook_store_d1::prepare(
            "SELECT expires_at, sweep_after, sweep_failures FROM directory_request \
             WHERE key = 'k00001'",
        ),
    ))
    .unwrap();
    use runbook_store_d1::sql::Columns;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].integer(0).unwrap(), later + 86_400_000, "replaced");
    assert_eq!(
        (
            rows[0].optional_integer(1).unwrap(),
            rows[0].integer(2).unwrap()
        ),
        (None, 0),
        "the new row has no backoff"
    );
}

/// CF12b review round 1 (finding 4): a key reused after its row expired,
/// when the organization's sweep before the write did not reach that row
/// (here, 50 older expired rows fill its one batch). The new response
/// replaces the expired row instead of being dropped by `ON CONFLICT DO
/// NOTHING`, and the old row's object goes.
#[test]
fn a_new_replay_replaces_an_expired_row_the_sweep_missed() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let path = format!("/v1/projects/{}/runbooks", out.project_id);
    let first = json!({ "source": large_source(&out.user_id, "first") });
    let created = f.call(pat, "POST", &path, first, Some("reused"), None);
    assert_eq!(created.status, 201, "{}", created.body["code"]);
    let (_, old_digest) = replay_reference(&f, "reused");
    // Older expired rows: the write's own sweep takes them, not "reused".
    expired_replay_rows(&f, &out, runbook_store_d1::replay::SWEEP_BATCH as usize);
    f.host
        .objects
        .now
        .set(f.host.objects.now.get() + 25 * 3_600_000);
    let second = json!({ "source": large_source(&out.user_id, "second") });
    let created = f.call(pat, "POST", &path, second.clone(), Some("reused"), None);
    assert_eq!(created.status, 201, "{}", created.body["code"]);
    let (_, new_digest) = replay_reference(&f, "reused");
    assert_ne!(new_digest, old_digest, "the row records the new response");
    assert_eq!(objects_with(&f, &old_digest), Vec::<String>::new());
    assert_eq!(objects_with(&f, &new_digest).len(), 1);
    // Its retry replays it.
    let retried = f.call(pat, "POST", &path, second, Some("reused"), None);
    assert_eq!(retried.status, 201, "{}", retried.body);
    assert_eq!(retried.body, created.body);
}

/// CF12b review round 1 (finding 4, residue): the object a replay write
/// stored first goes again when its row does not name it: the request lost
/// a race for its key (another row landed first), or its batch failed.
#[test]
fn a_replay_object_no_row_names_is_deleted() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let path = format!("/v1/projects/{}/runbooks", out.project_id);
    // A concurrent request with the same key whose row lands first: a
    // trigger inserts it just before this request's row.
    f.host
        .d1
        .execute_script(
            "CREATE TRIGGER race BEFORE INSERT ON directory_request \
             WHEN NEW.key = 'raced' AND NOT EXISTS (SELECT 1 FROM directory_request \
             WHERE org_id = NEW.org_id AND key = 'raced') BEGIN \
             INSERT INTO directory_request (org_id, principal, operation, key, payload, \
             response, expires_at) VALUES (NEW.org_id, NEW.principal, NEW.operation, \
             NEW.key, NEW.payload, '{\"status\":201,\"body\":{},\"etag\":null}', \
             NEW.expires_at); END;",
        )
        .unwrap();
    let body = json!({ "source": large_source(&out.user_id, "raced") });
    let created = f.call(pat, "POST", &path, body, Some("raced"), None);
    assert_eq!(created.status, 201, "{}", created.body["code"]);
    let replays = |f: &Fixture| {
        f.host
            .blobs
            .keys()
            .into_iter()
            .filter(|k| k.contains("/replay/"))
            .collect::<Vec<_>>()
    };
    assert_eq!(replays(&f), Vec::<String>::new(), "the loser's object");
    // A batch that fails (here, a trigger refuses the row).
    f.host
        .d1
        .execute_script(
            "CREATE TRIGGER refuse BEFORE INSERT ON directory_request \
             WHEN NEW.key = 'refused' BEGIN SELECT RAISE(ABORT, 'refused'); END;",
        )
        .unwrap();
    let body = json!({ "source": large_source(&out.user_id, "refused") });
    let failed = f.call(pat, "POST", &path, body, Some("refused"), None);
    assert_eq!(failed.status, 503, "{}", failed.body);
    assert_eq!(
        replays(&f),
        Vec::<String>::new(),
        "the failed batch's object"
    );
}

/// UI-1c: `GET /v1/projects/{p}/runbooks/{slug}` answers exactly what
/// `GET /v1/runbooks/{rb}` answers (run fields gated on `executions:read`
/// alike), with the project named by ID or slug; 404 for a slug no runbook
/// of the project has, or one no runbook can have; 403 without
/// `runbooks:read`.
#[test]
fn a_runbook_is_read_by_its_slug_as_by_its_id() {
    let f = Fixture::new();
    let out = run(runbook_worker::bootstrap::run(
        &f.host,
        runbook_worker::bootstrap::BootstrapRequest {
            organization: "Acme".into(),
            user_name: "Owner".into(),
            project_slug: "ops".into(),
            owner_email: None,
        },
    ))
    .unwrap();
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let mut ids = vec![];
    for slug in ["deploy", "deploy-api", "notes"] {
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {{id: {slug}, title: T {slug}, description: D, tags: [ops]}}\n\
             spec: {{steps: [{{id: check, title: Check, assignee: \"user:{}\"}}]}}\n",
            out.user_id
        );
        let created = f.call(
            pat,
            "POST",
            &path,
            json!({ "source": source }),
            Some(slug),
            None,
        );
        assert_eq!(created.status, 201, "{}", created.body);
        ids.push(created.body["runbook_id"].as_str().unwrap().to_owned());
        if slug == "deploy" {
            let published = f.call(
                pat,
                "POST",
                &format!("/v1/runbooks/{}/versions", ids[0]),
                json!({}),
                Some("publish"),
                created.etag.as_deref(),
            );
            assert_eq!(published.status, 201, "{}", published.body);
        }
    }
    let started = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "deploy"}),
        Some("start"),
        None,
    );
    assert_eq!(started.status, 201, "{}", started.body);
    let token = |name: &str, scopes: Value| {
        let made = f.call(
            pat,
            "POST",
            "/v1/tokens",
            json!({"name": name, "scopes": scopes, "projects": [project]}),
            Some(name),
            None,
        );
        assert_eq!(made.status, 201, "{}", made.body);
        made.body["secret"].as_str().unwrap().to_owned()
    };
    let narrow = token("lister", json!(["runbooks:read"]));
    let runs_only = token("runs", json!(["executions:read"]));
    for credential in [pat, narrow.as_str()] {
        for (slug, id) in ["deploy", "deploy-api", "notes"].iter().zip(&ids) {
            let by_id = f.call(
                credential,
                "GET",
                &format!("/v1/runbooks/{id}"),
                Value::Null,
                None,
                None,
            );
            assert_eq!(by_id.status, 200, "{}", by_id.body);
            for p in [project.as_str(), "ops"] {
                let uri = format!("/v1/projects/{p}/runbooks/{slug}");
                let by_slug = f.call(credential, "GET", &uri, Value::Null, None, None);
                assert_eq!(by_slug.status, 200, "{uri}: {}", by_slug.body);
                assert_eq!(by_slug.body, by_id.body, "{uri}");
            }
        }
    }
    let full = f.call(
        pat,
        "GET",
        &format!("{path}/deploy"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(full.body["last_run"]["id"], started.body["id"]);
    assert_eq!(full.body["latest_version"], 1);
    assert_eq!(full.body["active_runs"], 1);
    let withheld = f.call(
        &narrow,
        "GET",
        &format!("{path}/deploy"),
        Value::Null,
        None,
        None,
    );
    assert!(withheld.body.get("last_run").is_none(), "{}", withheld.body);
    assert!(
        withheld.body.get("active_runs").is_none(),
        "{}",
        withheld.body
    );
    for missing in [
        "deplo",
        "deploy-",
        "DEPLOY",
        "Not%20A%20Slug",
        "a%00b",
        "deploy@1",
        "rb_x",
        ids[0].as_str(),
    ] {
        let uri = format!("{path}/{missing}");
        let read = f.call(pat, "GET", &uri, Value::Null, None, None);
        assert_eq!(read.status, 404, "{uri}: {}", read.body);
        assert_eq!(read.body["code"], "resource.not_found", "{uri}");
    }
    let refused = f.call(
        &runs_only,
        "GET",
        &format!("{path}/deploy"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(refused.status, 403, "{}", refused.body);
    let other = f.call(
        pat,
        "GET",
        "/v1/projects/nope/runbooks/deploy",
        Value::Null,
        None,
        None,
    );
    assert_eq!(other.status, 404, "{}", other.body);
}

/// UI-1 review, Workers edition: the runbook list and one runbook's read
/// omit `last_run` and `active_runs` for a credential without
/// `executions:read`, and one list page costs a fixed number of D1
/// statements however many runbooks the project and the page hold (the
/// drafts, the page's latest versions and its runs are one query each).
#[test]
fn the_runbook_list_withholds_runs_and_keeps_its_d1_budget() {
    let f = Fixture::new();
    let out = run(runbook_worker::bootstrap::run(
        &f.host,
        runbook_worker::bootstrap::BootstrapRequest {
            organization: "Acme".into(),
            user_name: "Owner".into(),
            project_slug: "ops".into(),
            owner_email: None,
        },
    ))
    .unwrap();
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let mut newest = String::new();
    for i in 0..30 {
        let slug = format!("check-{i:02}");
        let source = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {{id: {slug}, title: Check {i}, tags: [ops]}}\n\
             spec: {{steps: [{{id: check, title: Check, assignee: \"user:{}\"}}]}}\n",
            out.user_id
        );
        let created = f.call(
            pat,
            "POST",
            &path,
            json!({ "source": source }),
            Some(&slug),
            None,
        );
        assert_eq!(created.status, 201, "{}", created.body);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let published = f.call(
            pat,
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            json!({}),
            Some(&format!("publish-{slug}")),
            created.etag.as_deref(),
        );
        assert_eq!(published.status, 201, "{}", published.body);
        newest = rb;
    }
    let started = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "check-29"}),
        Some("start"),
        None,
    );
    assert_eq!(started.status, 201, "{}", started.body);
    let narrow = f.call(
        pat,
        "POST",
        "/v1/tokens",
        json!({"name": "lister", "scopes": ["runbooks:read"], "projects": [project]}),
        Some("narrow"),
        None,
    );
    assert_eq!(narrow.status, 201, "{}", narrow.body);
    let narrow = narrow.body["secret"].as_str().unwrap().to_owned();

    // The owner's PAT holds executions:read: run fields on every row.
    let full = f.call(
        pat,
        "GET",
        &format!("{path}?limit=25"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(full.status, 200, "{}", full.body);
    let items = full.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 25);
    assert_eq!(items[0]["runbook_id"], newest.as_str());
    assert_eq!(items[0]["last_run"]["id"], started.body["id"]);
    assert_eq!(items[0]["latest_version"], 1);
    assert_eq!(items[0]["draft_ahead"], false);
    assert!(items.iter().all(|i| i["active_runs"].is_u64()), "{items:?}");
    // runbooks:read alone: the same rows without run fields.
    for uri in [format!("{path}?limit=25"), format!("/v1/runbooks/{newest}")] {
        let read = f.call(&narrow, "GET", &uri, Value::Null, None, None);
        assert_eq!(read.status, 200, "{uri}: {}", read.body);
        let rows = read.body["items"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![read.body.clone()]);
        assert!(!rows.is_empty());
        for row in rows {
            assert!(row.get("last_run").is_none(), "{uri}: {row}");
            assert!(row.get("active_runs").is_none(), "{uri}: {row}");
            assert_eq!(row["latest_version"], 1, "{uri}: {row}");
        }
    }
    // The run reads themselves stay refused to it.
    let runs = f.call(
        &narrow,
        "GET",
        &format!("/v1/projects/{project}/executions"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(runs.status, 403, "{}", runs.body);

    // D1 budget: a page of 25 (and of 5) out of 30 runbooks, the request
    // alone (no settling afterwards): the same fixed count.
    let statements = |limit: &str| {
        let request = Request {
            method: "GET".into(),
            path: path.clone(),
            query: [("limit".to_owned(), limit.to_owned())].into(),
            body: Value::Null,
            credential: Some(Credential::Bearer(pat.into())),
            idempotency_key: None,
            request_id: format!("req_budget_{limit}"),
            new_id: f.nonce(),
            if_match: None,
            client_ip: None,
        };
        let before = f.host.d1.statements_run();
        let response = runbook_api::http::encode(run(f.router.handle(request)));
        assert_eq!(response.status, 200);
        f.host.d1.statements_run() - before
    };
    let (page, small) = (statements("25"), statements("5"));
    println!("D1 statements for one runbook list page: {page} (25 rows), {small} (5 rows)");
    assert_eq!(page, small);
    assert!(page <= 12, "{page} D1 statements for one page");
}

/// UI-1b: one list request, alone (no settling), and what it cost: D1
/// statements and R2 operations (each a subrequest on Workers).
fn list_cost(f: &Fixture, token: &str, path: &str, query: &[(&str, &str)]) -> (Value, u64, u64) {
    let request = Request {
        method: "GET".into(),
        path: path.into(),
        query: query
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect(),
        body: Value::Null,
        credential: Some(Credential::Bearer(token.into())),
        idempotency_key: None,
        request_id: format!("req_list_{}", f.counter.get()),
        new_id: f.nonce(),
        if_match: None,
        client_ip: None,
    };
    let (d1, r2) = (f.host.d1.statements_run(), f.host.blobs.operations());
    let response = runbook_api::http::encode(run(f.router.handle(request)));
    let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
    assert_eq!(response.status, 200, "{body}");
    (
        body,
        f.host.d1.statements_run() - d1,
        f.host.blobs.operations() - r2,
    )
}

fn listed_source(slug: &str, title: &str, user: &runbook_model::UserId) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: {slug}, title: {title}, description: About {title}, tags: [ops, '{slug}']}}\n\
         spec: {{steps: [{{id: check, title: Check, assignee: \"user:{user}\"}}]}}\n"
    )
}

/// Create a runbook through the API and return its ID and draft ETag.
fn create_listed(
    f: &Fixture,
    pat: &str,
    project: &str,
    slug: &str,
    title: &str,
    user: &runbook_model::UserId,
) -> (String, String) {
    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({ "source": listed_source(slug, title, user) }),
        Some(&format!("create-{slug}")),
        None,
    );
    assert_eq!(created.status, 201, "{}", created.body);
    (
        created.body["runbook_id"].as_str().unwrap().to_owned(),
        created.etag.unwrap(),
    )
}

fn publish_listed(f: &Fixture, pat: &str, rb: &str, etag: &str, key: &str) {
    let published = f.call(
        pat,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        json!({}),
        Some(key),
        Some(etag),
    );
    assert_eq!(published.status, 201, "{}", published.body);
}

fn save_listed(f: &Fixture, pat: &str, rb: &str, etag: &str, source: String) -> String {
    let saved = f.call(
        pat,
        "PATCH",
        &format!("/v1/runbooks/{rb}/draft"),
        json!({ "source": source }),
        None,
        Some(etag),
    );
    assert_eq!(saved.status, 200, "{}", saved.body);
    saved.etag.unwrap()
}

/// UI-1b review finding 1: a list page reads rows only. Every source is in
/// R2 (CF11), and before this each runbook in the project cost an R2 read
/// per request (and a page row's latest version another). Now a page makes
/// no R2 operation at all and the same number of D1 statements for a
/// project of 5 runbooks as for one of 500, with or without a search.
#[test]
fn a_runbook_list_page_reads_no_source_for_5_or_500_runbooks() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let mut ids = vec![];
    let mut thirty = (0, 0);
    for i in 0..500 {
        let slug = format!("check-{i:03}");
        let (rb, etag) = create_listed(
            &f,
            pat,
            &project,
            &slug,
            &format!("Check {i}"),
            &out.user_id,
        );
        if i % 50 == 0 {
            publish_listed(&f, pat, &rb, &etag, &format!("publish-{slug}"));
        }
        if i == 450 {
            // Published, then saved ahead of it.
            let next = listed_source(&slug, "Check 450 edited", &out.user_id);
            let etag = f
                .call(
                    pat,
                    "GET",
                    &format!("/v1/runbooks/{rb}/draft"),
                    Value::Null,
                    None,
                    None,
                )
                .etag
                .unwrap();
            save_listed(&f, pat, &rb, &etag, next);
        }
        ids.push(rb);
        if i == 29 {
            // What one page of 25 out of 30 runbooks returns from D1.
            let before = f.host.d1.returned();
            list_cost(&f, pat, &path, &[("limit", "25")]);
            let after = f.host.d1.returned();
            thirty = (after.0 - before.0, after.1 - before.1);
        }
        if i == 4 {
            let (body, d1, r2) = list_cost(&f, pat, &path, &[("limit", "5")]);
            assert_eq!(body["items"].as_array().unwrap().len(), 5);
            println!("5 runbooks: {d1} D1 statements, {r2} R2 operations");
            assert_eq!(r2, 0, "a page of 5 runbooks read R2");
            assert!(d1 <= 12, "{d1} D1 statements");
        }
    }
    let mut costs = vec![];
    for query in [
        vec![("limit", "25")],
        vec![("limit", "5")],
        vec![("limit", "25"), ("q", "check 45")],
        vec![("limit", "25"), ("tag", "ops")],
    ] {
        let (body, d1, r2) = list_cost(&f, pat, &path, &query);
        println!("500 runbooks, {query:?}: {d1} D1 statements, {r2} R2 operations");
        assert_eq!(r2, 0, "{query:?} read R2");
        assert!(d1 <= 12, "{query:?}: {d1} D1 statements");
        costs.push(d1);
        let items = body["items"].as_array().unwrap();
        assert!(!items.is_empty());
        if query.len() == 1 {
            assert_eq!(items[0]["runbook_id"], ids[499].as_str());
            assert_eq!(items[0]["title"], "Check 499");
            assert_eq!(items[0]["description"], "About Check 499");
            assert_eq!(items[0]["tags"], json!(["ops", "check-499"]));
        }
    }
    assert!(costs.windows(2).all(|w| w[0] == w[1]), "{costs:?}");
    // Review finding 2: memory stays flat. The page holds its own rows
    // only (D1 returns the same rows for 500 runbooks as for 30, and about
    // the same bytes: IDs and titles are as long), and no source.
    let before = f.host.d1.returned();
    list_cost(&f, pat, &path, &[("limit", "25")]);
    let after = f.host.d1.returned();
    let five_hundred = (after.0 - before.0, after.1 - before.1);
    println!(
        "D1 returned for a page of 25: {thirty:?} of 30 runbooks, {five_hundred:?} of 500 (rows, bytes)"
    );
    assert_eq!(five_hundred.0, thirty.0, "rows");
    assert!(
        five_hundred.1 <= thirty.1 + 1_024,
        "{five_hundred:?} vs {thirty:?}"
    );
    // The search found the right rows: "check 45" is in the titles of
    // 45 and 450–459 (the edited title of 450 too), newest first.
    let (body, _, _) = list_cost(&f, pat, &path, &[("q", "check 45")]);
    let slugs: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["slug"].as_str().unwrap())
        .collect();
    let mut expected: Vec<String> = (450..460).rev().map(|i| format!("check-{i:03}")).collect();
    expected.push("check-045".into());
    assert_eq!(slugs, expected);
    // The published one is level with its draft; the edited one is ahead.
    let (body, _, _) = list_cost(&f, pat, &path, &[("q", "check-45")]);
    let row = |slug: &str| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["slug"] == slug)
            .cloned()
            .unwrap()
    };
    assert_eq!(row("check-450")["draft_ahead"], true);
    assert_eq!(row("check-450")["title"], "Check 450 edited");
    assert_eq!(row("check-450")["latest_version"], 1);
    assert_eq!(row("check-451")["draft_ahead"], false);
    assert!(
        row("check-451")
            .get("latest_version")
            .is_none_or(Value::is_null)
    );
}

/// UI-1b: `draft_ahead` compares digests, so it must follow every save and
/// publication: no version, level, ahead, back level by saving the
/// published text again, and level after the next publication.
#[test]
fn draft_ahead_follows_saves_and_publications() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let ahead = |f: &Fixture| {
        let (body, _, r2) = list_cost(f, pat, &path, &[]);
        assert_eq!(r2, 0);
        let row = body["items"][0].clone();
        (
            row["draft_ahead"].as_bool().unwrap(),
            row["latest_version"].as_u64(),
        )
    };
    let first = listed_source("deploy", "Deploy", &out.user_id);
    let (rb, etag) = create_listed(&f, pat, &project, "deploy", "Deploy", &out.user_id);
    assert_eq!(ahead(&f), (false, None), "nothing published");
    publish_listed(&f, pat, &rb, &etag, "publish-1");
    assert_eq!(ahead(&f), (false, Some(1)));
    let etag = save_listed(
        &f,
        pat,
        &rb,
        &etag,
        listed_source("deploy", "Deploy v2", &out.user_id),
    );
    assert_eq!(ahead(&f), (true, Some(1)));
    let etag = save_listed(&f, pat, &rb, &etag, first);
    assert_eq!(ahead(&f), (false, Some(1)), "the published text again");
    let etag = save_listed(
        &f,
        pat,
        &rb,
        &etag,
        listed_source("deploy", "Deploy v3", &out.user_id),
    );
    assert_eq!(ahead(&f), (true, Some(1)));
    publish_listed(&f, pat, &rb, &etag, "publish-2");
    assert_eq!(ahead(&f), (false, Some(2)));
    // One runbook's read agrees.
    let one = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(one.body["draft_ahead"], false);
    assert_eq!(one.body["title"], "Deploy v3");
}

/// UI-1b: rows whose stored listing is missing (written before D1 1018) or
/// stale (a draft saved by a release before it, after a rollback) are
/// listed correctly; the first list reads each such draft once (R2) and
/// stores its listing, and the next list reads none.
#[test]
fn listings_written_before_1018_are_backfilled_on_the_next_list() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let mut ids = vec![];
    for i in 0..3 {
        let slug = format!("old-{i}");
        let (rb, _) = create_listed(&f, pat, &project, &slug, &format!("Old {i}"), &out.user_id);
        ids.push(rb);
    }
    let (expected, _, _) = list_cost(&f, pat, &path, &[]);
    // As 1018 leaves rows written before it: no listing.
    f.host
        .d1
        .execute_script(
            "UPDATE runbook SET listing = NULL, listing_search = NULL, listing_digest = NULL, \
             listing_revision = NULL, listing_format = NULL",
        )
        .unwrap();
    let (body, _, r2) = list_cost(&f, pat, &path, &[]);
    assert_eq!(body, expected);
    assert_eq!(r2, 3, "one read per draft without a listing");
    let (body, _, r2) = list_cost(&f, pat, &path, &[("q", "old 1")]);
    assert_eq!(r2, 0, "backfilled");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["runbook_id"], ids[1].as_str());
    // An older release saved a draft (inline text, the next revision) and
    // left the listing alone: it is stale and recomputed.
    let renamed = listed_source("old-2", "Renamed", &out.user_id);
    use runbook_store_d1::D1 as _;
    run(f.host.d1.run(
        runbook_store_d1::prepare(
            "UPDATE runbook SET draft_source = ?, draft_digest = NULL, draft_size = NULL, \
             draft_revision = draft_revision + 1 WHERE runbook_id = ?",
        )
        .bind(renamed.as_bytes())
        .bind(ids[2].as_str()),
    ))
    .unwrap();
    let (body, _, r2) = list_cost(&f, pat, &path, &[("q", "renamed")]);
    assert_eq!(r2, 0, "an inline draft needs no blob read");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["title"], "Renamed");
    let (_, _, r2) = list_cost(&f, pat, &path, &[]);
    assert_eq!(r2, 0);
    // Review round 1: a long inline draft (over 16 KiB) is not returned
    // with the page but read alone.
    let long = listed_source("old-1", "Long inline", &out.user_id)
        + &format!("# {}\n", "pad ".repeat(5_000));
    assert!(long.len() > 16_384);
    run(f.host.d1.run(
        runbook_store_d1::prepare(
            "UPDATE runbook SET draft_source = ?, draft_digest = NULL, draft_size = NULL, \
             draft_revision = draft_revision + 1 WHERE runbook_id = ?",
        )
        .bind(long.as_bytes())
        .bind(ids[1].as_str()),
    ))
    .unwrap();
    let before = f.host.d1.returned();
    let (body, d1, r2) = list_cost(&f, pat, &path, &[("q", "long inline")]);
    let after = f.host.d1.returned();
    assert_eq!(r2, 0);
    assert_eq!(body["items"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!(body["items"][0]["title"], "Long inline");
    assert!(d1 > 0 && after.1 - before.1 > 20_000, "read alone");
    let (body, _, _) = list_cost(&f, pat, &path, &[("q", "long inline")]);
    assert_eq!(body["items"][0]["runbook_id"], ids[1].as_str(), "stored");
}

/// UI-1b: the listing conformance case on the Workers store (D1 with every
/// source in R2), and again with every stored listing removed before each
/// query (the backfill path must answer the same, filters included).
#[test]
fn the_listing_conformance_case_holds_on_d1_with_and_without_stored_listings() {
    use runbook_api::backends::Backends as _;
    use runbook_core::authoring::{AuthoringFactory as _, AuthoringStore as _};
    use runbook_store_conformance::listing;
    let f = Fixture::new();
    let store = f
        .router
        .backends
        .authoring()
        .scoped(runbook_store_conformance::scope(1));
    let published = run(listing::seed(&store)).unwrap();
    run(listing::runbook_listing_pages(&store, &published)).unwrap();
    for query in listing::queries() {
        let fresh = run(store.runbook_listing_page(&query)).unwrap();
        f.host
            .d1
            .execute_script(
                "UPDATE runbook SET listing = NULL, listing_search = NULL, \
                 listing_digest = NULL, listing_revision = NULL, listing_format = NULL",
            )
            .unwrap();
        let before = f.host.blobs.operations();
        let backfilled = run(store.runbook_listing_page(&query)).unwrap();
        assert_eq!(backfilled, fresh, "{query:?}");
        // Every row after the cursor is stale, so the page reads a draft
        // unless no row is left (after the oldest runbook).
        let rows_after = query.after.as_deref() != Some("rb_00000000000000000000000001");
        assert_eq!(
            f.host.blobs.operations() > before,
            rows_after,
            "{query:?}: read the drafts it reached"
        );
        let before = f.host.blobs.operations();
        assert_eq!(run(store.runbook_listing_page(&query)).unwrap(), fresh);
        assert_eq!(f.host.blobs.operations(), before, "{query:?}: stored again");
    }
}

/// UI-1c: `runbook_by_slug` on the Workers store: one D1 point read of the
/// unique slug index, no R2 read, scoped to the project.
#[test]
fn a_runbook_is_found_by_slug_on_d1_in_one_point_read() {
    use runbook_api::backends::Backends as _;
    use runbook_core::authoring::{AuthoringFactory as _, AuthoringStore as _};
    use runbook_store_conformance::{listing, scope};
    let f = Fixture::new();
    let factory = f.router.backends.authoring();
    let store = factory.scoped(scope(1));
    let other = factory.scoped(runbook_core::catalog::TenantScope {
        org_id: scope(1).org_id,
        project_id: scope(2).project_id,
    });
    run(listing::seed(&store)).unwrap();
    run(listing::runbook_by_slug(&store, &other)).unwrap();
    let (statements, blobs) = (f.host.d1.statements_run(), f.host.blobs.operations());
    let (rows, _) = f.host.d1.returned();
    assert!(run(store.runbook_by_slug("delta")).unwrap().is_some());
    assert_eq!(f.host.d1.statements_run() - statements, 1, "one statement");
    assert_eq!(
        f.host.d1.returned().0 - rows,
        1,
        "one row, not the project's"
    );
    assert_eq!(f.host.blobs.operations(), blobs, "no source read");
}

/// How many runbook rows keep a current listing.
fn listed_rows(f: &Fixture) -> i64 {
    use runbook_store_d1::{
        D1,
        sql::{Columns, prepare},
    };
    run(f.host.d1.all(prepare(
        "SELECT count(*) FROM runbook WHERE listing IS NOT NULL \
         AND listing_revision IS draft_revision",
    )))
    .unwrap()[0]
        .integer(0)
        .unwrap()
}

/// UI-1c (items 3 and 5): a request writes the listings it derives as it
/// goes, a batch of `LISTING_FLUSH` (10) at a time, and writes what it
/// derived before a draft read fails, before failing. So a request that
/// dies part-way (on Workers Free, at the 50-subrequest limit: each draft
/// is an R2 read) still spares the next one the drafts it read. Before: one
/// batch per round, written after the round, so a failed read discarded
/// every listing the round had derived, and a `limit=100` request over
/// unlisted rows on Free never stored any.
#[test]
fn derived_listings_are_stored_as_they_go_and_before_a_failed_read() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    for i in 0..30 {
        create_listed(
            &f,
            pat,
            &project,
            &format!("flush-{i:02}"),
            &format!("Flush {i}"),
            &out.user_id,
        );
    }
    let search = [("q", "no such runbook"), ("limit", "100")];
    // 30 stale rows in one request: written in 3 batches of 10.
    unlist_all(&f);
    let queries = f.host.d1.queries();
    let (_, _, r2) = list_cost(&f, pat, &path, &search);
    let listing_queries = f.host.d1.queries() - queries;
    assert_eq!(r2, 30);
    assert_eq!(listed_rows(&f), 30);
    let (_, _, r2) = list_cost(&f, pat, &path, &search);
    assert_eq!(r2, 0, "all stored");
    let steady = {
        let queries = f.host.d1.queries();
        list_cost(&f, pat, &path, &search);
        f.host.d1.queries() - queries
    };
    println!("D1 queries: {steady} listed, {listing_queries} deriving 30");
    assert_eq!(
        listing_queries - steady,
        3,
        "one batch per 10 derived listings"
    );

    // R2 fails from its 25th read on, as every subrequest past the Workers
    // limit does: the request fails as before (no guessing), and the 24
    // listings derived before the failure are stored, the last 4 by the
    // write made before failing.
    unlist_all(&f);
    f.host.blobs.fail_from(Some(f.host.blobs.operations() + 24));
    let failed = f.call(
        pat,
        "GET",
        &format!("{path}?q=no+such+runbook&limit=100"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(failed.status, 503, "{}", failed.body);
    assert_eq!(listed_rows(&f), 24);
    f.host.blobs.fail_from(None);
    let (body, _, r2) = list_cost(&f, pat, &path, &search);
    assert_eq!(r2, 6, "only the drafts not yet listed");
    assert_eq!(body["items"], json!([]));
    assert_eq!(listed_rows(&f), 30);
}

/// Remove every stored listing, as D1 1018 leaves the rows written before it.
fn unlist_all(f: &Fixture) {
    f.host
        .d1
        .execute_script(
            "UPDATE runbook SET listing = NULL, listing_search = NULL, listing_digest = NULL, \
             listing_revision = NULL, listing_format = NULL",
        )
        .unwrap();
}

/// Every page of one list, following `next_cursor` as the web does: the
/// slugs in order and each request's cost (D1 statements, R2 operations).
fn walk_list(
    f: &Fixture,
    token: &str,
    path: &str,
    query: &[(&str, &str)],
) -> (Vec<String>, Vec<(u64, u64)>) {
    let (mut slugs, mut costs, mut cursor) = (vec![], vec![], None::<String>);
    loop {
        let mut q = query.to_vec();
        if let Some(c) = &cursor {
            q.push(("cursor", c.as_str()));
        }
        let (body, d1, r2) = list_cost(f, token, path, &q);
        costs.push((d1, r2));
        slugs.extend(
            body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["slug"].as_str().unwrap().to_owned()),
        );
        match body["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => return (slugs, costs),
        }
        assert!(costs.len() < 200, "the walk does not end");
    }
}

/// D1 statements one list request may make: the page's own reads (12, as
/// measured above), at most 2 more selects and one guarded update for
/// each listing derived (in at most 3 batches; `NativeD1` counts each
/// statement of a batch).
const LIST_STATEMENTS: u64 = 14 + STALE_PER_REQUEST;
/// Listings one request derives at most (`runbook_store_d1::listing::
/// STALE_PER_REQUEST`): each may be an R2 read.
const STALE_PER_REQUEST: u64 = 100;

/// UI-1b review round 1, finding 1: a search over rows not yet listed
/// (every row, right after 1018) derives at most 100 listings a request;
/// past that it answers a short page with a cursor that continues after
/// the last row it examined, so no match is skipped and the list does not
/// end early. Before, one request derived every row of the project.
#[test]
fn a_search_over_unlisted_rows_is_bounded_per_request_and_pages_correctly() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let slug = |i: usize| format!("check-{i:03}");
    for i in 0..500 {
        create_listed(
            &f,
            pat,
            &project,
            &slug(i),
            &format!("Check {i}"),
            &out.user_id,
        );
    }
    let within = |costs: &[(u64, u64)]| {
        for &(d1, r2) in costs {
            assert!(r2 <= STALE_PER_REQUEST, "{r2} R2 operations: {costs:?}");
            assert!(d1 <= LIST_STATEMENTS, "{d1} D1 statements: {costs:?}");
        }
    };
    unlist_all(&f);
    // Memory: one request holds a bounded set of rows (no draft: these are
    // in R2, read one at a time), whatever the project's size.
    let before = f.host.d1.returned();
    list_cost(&f, pat, &path, &[("q", "no such runbook"), ("limit", "25")]);
    let after = f.host.d1.returned();
    println!(
        "one request over unlisted rows: {:?} (rows, bytes) from D1",
        (after.0 - before.0, after.1 - before.1)
    );
    assert!(after.1 - before.1 < 128 * 1024, "{before:?} {after:?}");
    unlist_all(&f);
    // A search that matches nothing: 5 requests of at most 100 drafts each.
    let (found, costs) = walk_list(&f, pat, &path, &[("q", "no such runbook"), ("limit", "25")]);
    println!("no match over 500 unlisted rows: {costs:?}");
    assert!(found.is_empty());
    within(&costs);
    assert_eq!(costs.len(), 5, "{costs:?}");
    assert_eq!(
        costs.iter().map(|c| c.1).sum::<u64>(),
        500,
        "each draft read once"
    );
    // Now every row is listed: one request, no R2.
    let (found, costs) = walk_list(&f, pat, &path, &[("q", "no such runbook"), ("limit", "25")]);
    assert!(found.is_empty());
    assert_eq!(costs.len(), 1);
    assert_eq!(costs[0].1, 0);
    // Paging a search while every row is unlisted: every match, in order.
    unlist_all(&f);
    let expected: Vec<String> = (0..500)
        .rev()
        .filter(|i| format!("check {i}").contains("check 4"))
        .map(slug)
        .collect();
    let (found, costs) = walk_list(&f, pat, &path, &[("q", "check 4"), ("limit", "10")]);
    within(&costs);
    assert_eq!(found, expected);
    // A tag that every row has, unlisted again: the whole list, 25 a page.
    unlist_all(&f);
    let (found, costs) = walk_list(&f, pat, &path, &[("tag", "ops"), ("limit", "25")]);
    within(&costs);
    assert_eq!(found, (0..500).rev().map(slug).collect::<Vec<_>>());
    assert_eq!(costs.len(), 20, "full pages: {costs:?}");
    // A tag only one row has, unlisted again: found, after the rows before
    // it are examined 100 a request.
    unlist_all(&f);
    let (found, costs) = walk_list(&f, pat, &path, &[("tag", "check-007"), ("limit", "25")]);
    within(&costs);
    assert_eq!(found, [slug(7)]);
}

/// UI-1b review round 1, finding 4: a request whose listing writes all
/// lose (a concurrent save moved each draft on) still answers from the
/// listings it derived, and keeps the cursor when it did not reach the end.
/// Before, such a request answered the matches among one SQL page and no
/// cursor, ending the list early.
#[test]
fn a_list_whose_listing_writes_all_lose_still_pages_to_the_end() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let slug = |i: usize| format!("check-{i:03}");
    for i in 0..30 {
        create_listed(
            &f,
            pat,
            &project,
            &slug(i),
            &format!("Check {i}"),
            &out.user_id,
        );
    }
    unlist_all(&f);
    // Every listing write is dropped, as one guarded by a draft revision a
    // concurrent save has moved on.
    f.host
        .d1
        .execute_script(
            "CREATE TRIGGER lose_listing BEFORE UPDATE OF listing ON runbook \
             WHEN NEW.listing IS NOT NULL AND NEW.draft_revision = OLD.draft_revision \
             BEGIN SELECT RAISE(IGNORE); END",
        )
        .unwrap();
    let expected: Vec<String> = (0..30)
        .rev()
        .filter(|i| format!("check {i}").contains("check 1"))
        .map(slug)
        .collect();
    for limit in ["2", "5", "100"] {
        let (found, _) = walk_list(&f, pat, &path, &[("q", "check 1"), ("limit", limit)]);
        assert_eq!(found, expected, "limit {limit}");
    }
    let (found, _) = walk_list(&f, pat, &path, &[("limit", "7")]);
    assert_eq!(found, (0..30).rev().map(slug).collect::<Vec<_>>());
}

/// UI-1b review round 1: one row whose listing cannot be written (here a
/// trigger refuses it) neither fails the page nor keeps the other rows'
/// listings from being stored.
#[test]
fn one_rows_failed_listing_write_fails_neither_the_page_nor_the_others() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let mut ids = vec![];
    for i in 0..5 {
        let (rb, _) = create_listed(
            &f,
            pat,
            &project,
            &format!("row-{i}"),
            &format!("Row {i}"),
            &out.user_id,
        );
        ids.push(rb);
    }
    let (expected, _, _) = list_cost(&f, pat, &path, &[]);
    unlist_all(&f);
    f.host
        .d1
        .execute_script(&format!(
            "CREATE TRIGGER refuse_one BEFORE UPDATE OF listing ON runbook \
             WHEN OLD.runbook_id = '{}' AND NEW.listing IS NOT NULL \
             BEGIN SELECT RAISE(ABORT, 'string or blob too big'); END",
            ids[2]
        ))
        .unwrap();
    let (body, _, r2) = list_cost(&f, pat, &path, &[]);
    assert_eq!(body, expected);
    assert_eq!(r2, 5);
    let (body, _, r2) = list_cost(&f, pat, &path, &[]);
    assert_eq!(body, expected);
    assert_eq!(r2, 1, "only the refused row is read again");
}

/// UI-1b review round 1: a draft whose listing would not fit beside it in
/// a D1 row (at most 2,000,000 bytes; here about 116,000 distinct tags in
/// a source just under 1 MiB) is saved without a stored listing and listed
/// from its source, instead of failing the save.
#[test]
fn a_draft_whose_listing_would_overflow_its_row_is_saved_and_listed() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let path = format!("/v1/projects/{project}/runbooks");
    let mut tags = String::new();
    let mut n = 0;
    while tags.len() < 1_000_000 {
        tags.push_str(&format!("t{n:06}, "));
        n += 1;
    }
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: wide, title: Wide, tags: [{tags}]}}\n\
         spec: {{steps: [{{id: check, title: Check}}]}}\n"
    );
    assert!(source.len() < 1 << 20);
    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({ "source": source }),
        Some("create-wide"),
        None,
    );
    assert_eq!(created.status, 201, "{}", created.body);
    create_listed(&f, pat, &project, "small", "Small", &out.user_id);
    for query in [vec![], vec![("tag", "t000007")], vec![("q", "wide")]] {
        let (body, _, r2) = list_cost(&f, pat, &path, &query);
        let row = body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["slug"] == "wide")
            .cloned()
            .unwrap_or_else(|| panic!("{query:?}: {body}"));
        assert_eq!(row["title"], "Wide");
        assert_eq!(row["tags"].as_array().unwrap().len(), n);
        assert_eq!(r2, 1, "{query:?}: its source is read");
    }
}

// ---- CF13: a live run never outgrows a Durable Object row ----

/// Publish `source` (whose runbook ID is `slug`) and start a run of it.
fn publish_and_start(
    f: &Fixture,
    pat: &str,
    project: &str,
    source: &str,
    slug: &str,
    inputs: Value,
) -> String {
    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({ "source": source }),
        Some(&format!("create-{slug}")),
        None,
    );
    assert_eq!(created.status, 201, "{}", created.body);
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let draft = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}/draft"),
        Value::Null,
        None,
        None,
    );
    let published = f.call(
        pat,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        json!({}),
        Some(&format!("publish-{slug}")),
        draft.etag.as_deref(),
    );
    assert_eq!(published.status, 201, "{}", published.body);
    let started = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({ "runbook": slug, "inputs": inputs }),
        Some(&format!("start-{slug}")),
        None,
    );
    assert_eq!(started.status, 201, "{}", started.body["code"]);
    started.body["id"].as_str().unwrap().to_owned()
}

fn execution_view(f: &Fixture, pat: &str, exe: &str) -> Value {
    let read = f.call(
        pat,
        "GET",
        &format!("/v1/executions/{exe}"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(read.status, 200, "{}", read.body["code"]);
    read.body
}

/// Claim and report every ready step run of `exe` until none is ready,
/// each with `summary`; the replies of the reports, in order.
fn report_every_step(f: &Fixture, pat: &str, exe: &str, summary: &str) -> Vec<Reply> {
    let mut replies = vec![];
    loop {
        let view = execution_view(f, pat, exe);
        let ready: Vec<String> = view["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["status"] == "ready" && s["kind"] == "task")
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect();
        if ready.is_empty() {
            return replies;
        }
        for sr in ready {
            let n = replies.len();
            let claim = f.call(
                pat,
                "POST",
                &format!("/v1/step-runs/{sr}/claim"),
                json!({"session_id": "web"}),
                Some(&format!("claim-{n}")),
                None,
            );
            assert_eq!(claim.status, 200, "claim {n}: {}", claim.body);
            let reply = f.call(
                pat,
                "POST",
                &format!("/v1/step-runs/{sr}/report"),
                json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded",
                       "verdict": null, "outputs": {}, "summary": summary, "evidence": [],
                       "logs": []}),
                Some(&format!("report-{n}")),
                None,
            );
            let stop = reply.status != 200;
            replies.push(reply);
            if stop {
                return replies;
            }
        }
    }
}

/// The longest value any row of `exe`'s object holds, by table.
fn longest_values(f: &Fixture, exe: &str) -> BTreeMap<String, i64> {
    use runbook_store_do::DoSql;
    let sql = f.host.objects.map.borrow()[&exe.parse::<ExecutionId>().unwrap()].clone();
    let mut out = BTreeMap::new();
    for (table, column) in [
        ("meta", "projection"),
        ("step_run", "body"),
        ("derived_outbox", "entry"),
        ("derived_assignment", "body"),
        ("event", "envelope"),
        ("outbox", "intent"),
        ("claim", "body"),
    ] {
        let rows = sql
            .query(
                &format!("SELECT coalesce(max(length(CAST({column} AS BLOB))), 0) FROM {table}"),
                &[],
            )
            .unwrap();
        let runbook_store_do::Value::Integer(n) = rows[0][0] else {
            panic!("{table}: {:?}", rows[0][0]);
        };
        out.insert(table.to_owned(), n);
    }
    out
}

/// CF13 (the bug CF12c's review found): a run's head row kept every step
/// run's outputs and summary, so report 31 of a 40-step run with
/// 65,000-byte summaries passed Durable Object SQLite's 2,000,000-byte row
/// and answered `503 storage.unavailable`, which clients retry forever.
/// Each step run now has its own row: every report answers 200 and the
/// run completes, its head small.
#[test]
fn a_long_run_with_large_summaries_never_outgrows_a_row() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let steps: Vec<String> = (1..=40)
        .map(|n| {
            format!(
                "{{id: s{n}, title: Step {n}, assignee: \"user:{}\"}}",
                out.user_id
            )
        })
        .collect();
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: long, title: Long}}\n\
         spec: {{steps: [{}]}}\n",
        steps.join(", ")
    );
    let exe = publish_and_start(&f, pat, &project, &source, "long", json!({}));
    let summary = "s".repeat(65_000);
    let replies = report_every_step(&f, pat, &exe, &summary);
    for (n, reply) in replies.iter().enumerate() {
        assert_eq!(reply.status, 200, "report {}: {}", n + 1, reply.body);
    }
    assert_eq!(replies.len(), 40);
    let done = execution_view(&f, pat, &exe);
    assert_eq!(done["status"], "succeeded", "{}", done["status"]);
    let steps = done["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 40);
    for step in steps {
        assert_eq!(step["summary"].as_str().map(str::len), Some(65_000));
    }
    let report = f.call(
        pat,
        "GET",
        &format!("/v1/executions/{exe}/report"),
        Value::Null,
        None,
        None,
    );
    assert_eq!(report.status, 200, "{}", report.body["code"]);
    let longest = longest_values(&f, &exe);
    assert!(
        longest["meta"] < 65_536,
        "the head keeps no step data: {longest:?}"
    );
    assert!(longest["step_run"] < 200_000, "{longest:?}");
}

/// The source of a `foreach` over `inputs.items` (up to 100, `concurrency`
/// of them open at once), each iteration one task titled with `inputs.note`.
fn wide_source(id: &str, user: &runbook_model::UserId, concurrency: u32) -> String {
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: {id}, title: Wide}}\n\
         spec:\n  inputs:\n    items: {{type: list, items: {{type: integer}}}}\n    \
         note: {{type: string}}\n  steps:\n    - id: each\n      \
         foreach: {{items: '${{{{ inputs.items }}}}', as: item, max_items: 100, concurrency: {concurrency}, \
         steps: [{{id: check, title: 'Check ${{{{ item }}}} ${{{{ inputs.note }}}}', assignee: \"user:{user}\"}}]}}\n"
    )
}

fn open_tasks(view: &Value) -> usize {
    view["steps"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["status"] == "ready" && s["kind"] == "task")
        .count()
}

/// CF13: many small step runs reach the limit too. A `foreach` of 100
/// iterations, each reported with a 25,000-byte summary: 2.5 MB of
/// summaries, which the head kept whole. (One open at a time: a hundred
/// open at once costs this debug build minutes per run, storage aside;
/// the next test opens them all.)
#[test]
fn a_wide_foreach_never_outgrows_a_row() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let items: Vec<u32> = (0..100).collect();
    let source = wide_source("wide", &out.user_id, 1);
    let inputs = json!({"items": items, "note": "now"});
    let exe = publish_and_start(&f, pat, &project, &source, "wide", inputs);
    let replies = report_every_step(&f, pat, &exe, &"s".repeat(25_000));
    for (n, reply) in replies.iter().enumerate() {
        assert_eq!(reply.status, 200, "report {}: {}", n + 1, reply.body);
    }
    assert_eq!(replies.len(), 100);
    let done = execution_view(&f, pat, &exe);
    assert_eq!(done["status"], "succeeded", "{}", done["status"]);
    let longest = longest_values(&f, &exe);
    assert!(longest["meta"] < 65_536, "{longest:?}");
    assert!(longest["step_run"] < 200_000, "{longest:?}");
}

/// CF13: each derived row kept every open assignment, so 100 open
/// iterations titled with 25,000 bytes each (2.5 MB) could not even be
/// activated. Each assignment is now its own row: the run starts, the
/// inbox lists them with their titles, and they can be worked.
#[test]
fn many_open_assignments_never_outgrow_a_row() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let items: Vec<u32> = (0..100).collect();
    let source = wide_source("titled", &out.user_id, 100);
    let inputs = json!({"items": items, "note": "n".repeat(25_000)});
    let exe = publish_and_start(&f, pat, &project, &source, "titled", inputs);
    let view = execution_view(&f, pat, &exe);
    assert_eq!(open_tasks(&view), 100, "every iteration is open at once");
    let tasks = f.call(pat, "GET", "/v1/me/tasks", Value::Null, None, None);
    assert_eq!(tasks.status, 200, "{}", tasks.body["code"]);
    let items = tasks.body["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for task in items {
        assert!(task["title"].as_str().is_some_and(|t| t.len() > 25_000));
    }
    // One of them is worked; the next derived row keeps the other 99.
    let sr = view["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "task")
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .unwrap();
    let claim = f.call(
        pat,
        "POST",
        &format!("/v1/step-runs/{sr}/claim"),
        json!({"session_id": "web"}),
        Some("claim-titled"),
        None,
    );
    assert_eq!(claim.status, 200, "{}", claim.body);
    let reported = f.call(
        pat,
        "POST",
        &format!("/v1/step-runs/{sr}/report"),
        json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded",
               "verdict": null, "outputs": {}, "summary": "done", "evidence": [],
               "logs": []}),
        Some("report-titled"),
        None,
    );
    assert_eq!(reported.status, 200, "{}", reported.body);
    assert_eq!(open_tasks(&execution_view(&f, pat, &exe)), 99);
    let longest = longest_values(&f, &exe);
    assert!(longest["derived_outbox"] < 65_536, "{longest:?}");
    assert!(longest["derived_assignment"] < 65_536, "{longest:?}");
}

/// The source of a runbook whose step `work` returns `blob` and whose
/// `count` outputs each copy it; `then` follows `work`.
fn wide_outputs_source(id: &str, user: &runbook_model::UserId, count: u32, then: &str) -> String {
    let outputs: Vec<String> = (1..=count)
        .map(|n| format!("o{n:02}: '${{{{ steps.work.outputs.blob }}}}'"))
        .collect();
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: {id}, title: Wide outputs}}\n\
         spec:\n  steps:\n    - id: work\n      title: Work\n      assignee: \"user:{user}\"\n      \
         outputs: {{blob: {{type: string}}}}\n{then}  outputs: {{{}}}\n",
        outputs.join(", ")
    )
}

/// Claim and report the ready task `path` of `exe` with `outputs`.
fn report_step(f: &Fixture, pat: &str, exe: &str, path: &str, outputs: Value) -> Reply {
    let view = execution_view(f, pat, exe);
    let sr = view["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["path"] == path)
        .and_then(|s| s["id"].as_str())
        .unwrap()
        .to_owned();
    let claim = f.call(
        pat,
        "POST",
        &format!("/v1/step-runs/{sr}/claim"),
        json!({"session_id": "web"}),
        Some(&format!("claim-{exe}-{path}")),
        None,
    );
    assert_eq!(claim.status, 200, "{}", claim.body);
    f.call(
        pat,
        "POST",
        &format!("/v1/step-runs/{sr}/report"),
        json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded",
               "verdict": null, "outputs": outputs, "summary": "done",
               "evidence": [], "logs": []}),
        Some(&format!("report-{exe}-{path}")),
        None,
    )
}

/// The event bodies of `exe` of type `kind`, in order.
fn events_of(f: &Fixture, exe: &str, kind: &str) -> Vec<Value> {
    use runbook_store_do::DoSql;
    let sql = f.host.objects.map.borrow()[&exe.parse::<ExecutionId>().unwrap()].clone();
    sql.query(
        "SELECT envelope FROM event WHERE type = ? ORDER BY seq",
        &[runbook_store_do::Value::Text(kind.into())],
    )
    .unwrap()
    .iter()
    .map(|row| {
        let runbook_store_do::Value::Text(text) = &row[0] else {
            panic!("{:?}", row[0]);
        };
        serde_json::from_str::<Value>(text).unwrap()["event"].clone()
    })
    .collect()
}

/// Every row of `exe` stays within what one Durable Object row holds.
fn assert_rows_fit(f: &Fixture, exe: &str) {
    let longest = longest_values(f, exe);
    for (table, bytes) in &longest {
        assert!(
            *bytes < 2_000_000 - 4_096,
            "{table} holds {bytes} bytes: {longest:?}"
        );
    }
}

/// The outputs `exe` completed with: how many keep a value, and how many
/// are null.
fn completed_outputs(f: &Fixture, exe: &str) -> (usize, usize) {
    let completed = events_of(f, exe, "ExecutionCompleted");
    assert_eq!(completed.len(), 1, "the run completed once");
    let values = completed[0]["outputs"]["values"]
        .as_object()
        .unwrap()
        .clone();
    let nulls = values.values().filter(|v| **v == json!("Null")).count();
    (values.len() - nulls, nulls)
}

/// CF13 review round 1, item 1: a runbook's outputs together were not
/// bounded, so 40 outputs that each copy a step's 60,000-byte output
/// passed one row: the report that completed the run answered `422
/// execution.row_too_large`, and so did a cancel, so the run could never
/// end on Workers. The engine (version 5) now holds them to
/// `MAX_OUTPUTS_BYTES`: the report answers 200 and the run fails with
/// `execution.outputs_too_large`, its outputs past the budget null.
#[test]
fn outputs_past_the_budget_fail_the_run_instead_of_refusing_its_report() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let source = wide_outputs_source("wideout", &out.user_id, 40, "");
    let exe = publish_and_start(&f, pat, &project, &source, "wideout", json!({}));
    let reported = report_step(&f, pat, &exe, "work", json!({"blob": "b".repeat(60_000)}));
    assert_eq!(reported.status, 200, "{}", reported.body);
    let view = execution_view(&f, pat, &exe);
    assert_eq!(view["status"], "failed", "{}", view["status"]);
    let started = events_of(&f, &exe, "CleanupStarted");
    assert_eq!(started[0]["reason"], runbook_engine::OUTPUTS_TOO_LARGE);
    let (kept, nulls) = completed_outputs(&f, &exe);
    assert!(kept > 0 && nulls > 0, "{kept} kept, {nulls} null");
    assert_rows_fit(&f, &exe);
}

/// The same run cancelled before it completes: the cancel answers 200 and
/// the run closes with its outputs past the budget null.
#[test]
fn a_run_past_the_outputs_budget_can_be_cancelled() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let then = format!(
        "    - {{id: next, title: Next, assignee: \"user:{}\"}}\n",
        out.user_id
    );
    let source = wide_outputs_source("cancelout", &out.user_id, 40, &then);
    let exe = publish_and_start(&f, pat, &project, &source, "cancelout", json!({}));
    let reported = report_step(&f, pat, &exe, "work", json!({"blob": "b".repeat(60_000)}));
    assert_eq!(reported.status, 200, "{}", reported.body);
    let cancelled = f.call(
        pat,
        "PATCH",
        &format!("/v1/executions/{exe}"),
        json!({"action": "cancel"}),
        Some("cancel-wide"),
        None,
    );
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    let view = execution_view(&f, pat, &exe);
    assert_eq!(view["status"], "cancelled", "{}", view["status"]);
    let (kept, nulls) = completed_outputs(&f, &exe);
    assert!(kept > 0 && nulls > 0, "{kept} kept, {nulls} null");
    assert_rows_fit(&f, &exe);
}

/// A run whose close is driven by a timer (its last step a wait) ends when
/// the timer fires: before, the timer's commit was refused and retried at
/// every lease lapse.
#[test]
fn a_timer_driven_close_past_the_outputs_budget_completes() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let source = wide_outputs_source(
        "timedout",
        &out.user_id,
        40,
        "    - {id: pause, wait: {for: 1m}}\n",
    );
    let exe = publish_and_start(&f, pat, &project, &source, "timedout", json!({}));
    let reported = report_step(&f, pat, &exe, "work", json!({"blob": "b".repeat(60_000)}));
    assert_eq!(reported.status, 200, "{}", reported.body);
    assert_eq!(execution_view(&f, pat, &exe)["status"], "waiting");
    // Two minutes on; the next request settles the object's due timer.
    f.host.objects.now.set(f.host.objects.now.get() + 120_000);
    let view = execution_view(&f, pat, &exe);
    let view = if view["status"] == "failed" {
        view
    } else {
        execution_view(&f, pat, &exe)
    };
    assert_eq!(view["status"], "failed", "{}", view["status"]);
    let started = events_of(&f, &exe, "CleanupStarted");
    assert_eq!(started[0]["reason"], runbook_engine::OUTPUTS_TOO_LARGE);
    assert_rows_fit(&f, &exe);
}

/// Suspicion B (CF13 review round 1): a parent's call step keeps its
/// child's outputs, in its row and in its `CallCompleted`. A child whose
/// outputs are just under the budget succeeds, and its parent's rows fit;
/// one past it fails with the fault, and its parent records the outputs
/// that fit (the rest null).
#[test]
fn a_call_result_near_the_outputs_budget_fits_its_parents_rows() {
    let f = Fixture::new();
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    for (slug, count, status) in [("near", 15, "succeeded"), ("past", 40, "failed")] {
        let child = wide_outputs_source(&format!("{slug}child"), &out.user_id, count, "");
        let declared: Vec<String> = (1..=count)
            .map(|n| format!("o{n:02}: {{type: string}}"))
            .collect();
        let parent = format!(
            "apiVersion: therunbook.app/v1\nkind: Runbook\n\
             metadata: {{id: {slug}parent, title: Parent}}\n\
             spec:\n  steps:\n    - id: db\n      \
             call: {{runbook: {slug}child, outputs: {{{}}}}}\n",
            declared.join(", ")
        );
        let created = f.call(
            pat,
            "POST",
            &format!("/v1/projects/{project}/runbooks"),
            json!({ "source": child }),
            Some(&format!("create-{slug}child")),
            None,
        );
        assert_eq!(created.status, 201, "{}", created.body);
        let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
        let draft = f.call(
            pat,
            "GET",
            &format!("/v1/runbooks/{rb}/draft"),
            Value::Null,
            None,
            None,
        );
        let published = f.call(
            pat,
            "POST",
            &format!("/v1/runbooks/{rb}/versions"),
            json!({}),
            Some(&format!("publish-{slug}child")),
            draft.etag.as_deref(),
        );
        assert_eq!(published.status, 201, "{}", published.body);
        let parent_exe = publish_and_start(
            &f,
            pat,
            &project,
            &parent,
            &format!("{slug}parent"),
            json!({}),
        );
        let view = execution_view(&f, pat, &parent_exe);
        let child_exe = view["steps"][0]["child_execution_id"]
            .as_str()
            .or_else(|| view["steps"][0]["call"]["child_execution_id"].as_str())
            .unwrap_or_else(|| panic!("{}", view["steps"][0]))
            .to_owned();
        // The child starts as the host settles the parent's call.
        for _ in 0..10 {
            if execution_view(&f, pat, &child_exe)["status"] != "queued" {
                break;
            }
        }
        let reported = report_step(
            &f,
            pat,
            &child_exe,
            "work",
            json!({"blob": "b".repeat(60_000)}),
        );
        assert_eq!(reported.status, 200, "{}", reported.body);
        assert_eq!(execution_view(&f, pat, &child_exe)["status"], status);
        let mut parent_view = execution_view(&f, pat, &parent_exe);
        if parent_view["status"] != status {
            parent_view = execution_view(&f, pat, &parent_exe);
        }
        assert_eq!(
            parent_view["status"], status,
            "{slug}: {}",
            parent_view["status"]
        );
        let outcome = &events_of(&f, &parent_exe, "CallCompleted")[0]["outcome"];
        let values = outcome["outputs"].as_object().unwrap();
        assert_eq!(values.len(), count as usize);
        let kept = values.values().filter(|v| **v != json!("Null")).count();
        if status == "succeeded" {
            assert_eq!(kept, count as usize);
        } else {
            assert!(kept > 0 && kept < count as usize, "{kept}");
        }
        assert_rows_fit(&f, &parent_exe);
        assert_rows_fit(&f, &child_exe);
    }
}

/// Claim and report `path` of `exe` with `logs` and `summary`; the reply.
fn report_logged(f: &Fixture, pat: &str, exe: &str, path: &str, logs: Value, key: &str) -> Reply {
    let view = execution_view(f, pat, exe);
    let step = view["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["path"] == path)
        .cloned()
        .unwrap();
    let sr = step["id"].as_str().unwrap().to_owned();
    let claim = if step["status"] == "claimed" {
        step["claim"].clone()
    } else {
        let claim = f.call(
            pat,
            "POST",
            &format!("/v1/step-runs/{sr}/claim"),
            json!({"session_id": "web"}),
            Some(&format!("claim-{key}")),
            None,
        );
        assert_eq!(claim.status, 200, "{}", claim.body);
        claim.body
    };
    f.call(
        pat,
        "POST",
        &format!("/v1/step-runs/{sr}/report"),
        json!({"step_run_id": sr, "claim": claim, "status": "succeeded",
               "verdict": null, "outputs": {}, "summary": "done", "evidence": [],
               "logs": logs}),
        Some(&format!("report-{key}")),
        None,
    )
}

/// CF13 review round 1, item 2: nothing bounded a run's total size, and
/// every request loads all of it (the old 2 MB head capped it by
/// accident). A run's event log is now held to a budget
/// (`RUNBOOK_LIMIT_RUN_BYTES`, here lowered to 400 KB): a request whose own
/// data would take the run past it is refused with a non-retryable `422
/// execution.too_large` and writes nothing, while small writes still pass,
/// so a person can finish the run or cancel it.
#[test]
fn a_run_at_its_stored_data_budget_refuses_large_writes_but_can_be_finished_or_cancelled() {
    let f = Fixture::with_run_budget(Some(400_000));
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let steps: Vec<String> = (1..=4)
        .map(|n| {
            format!(
                "{{id: s{n}, title: Step {n}, assignee: \"user:{}\"}}",
                out.user_id
            )
        })
        .collect();
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: budget, title: Budget}}\n\
         spec: {{steps: [{}]}}\n",
        steps.join(", ")
    );
    let logs = |n: usize| json!(vec!["l".repeat(60_000); n]);
    let exe = publish_and_start(&f, pat, &project, &source, "budget", json!({}));
    // 300 KB of logs fit.
    let first = report_logged(&f, pat, &exe, "s1", logs(5), "s1");
    assert_eq!(first.status, 200, "{}", first.body);
    // Another 180 KB would pass the budget: refused, twice, nothing written.
    let refused = report_logged(&f, pat, &exe, "s2", logs(3), "s2");
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert_eq!(refused.body["code"], "execution.too_large");
    let before = execution_view(&f, pat, &exe);
    assert_eq!(before["steps"][1]["status"], "claimed");
    assert_eq!(before["steps"][1]["summary"], Value::Null);
    let again = report_logged(&f, pat, &exe, "s2", logs(3), "s2");
    assert_eq!(again.status, 422, "{}", again.body);
    assert_eq!(execution_view(&f, pat, &exe)["steps"], before["steps"]);
    // A person can still finish the step with a short report...
    let small = report_logged(&f, pat, &exe, "s2", json!(["ok"]), "s2-small");
    assert_eq!(small.status, 200, "{}", small.body);
    // ...and cancel the run.
    let cancelled = f.call(
        pat,
        "PATCH",
        &format!("/v1/executions/{exe}"),
        json!({"action": "cancel"}),
        Some("cancel-budget"),
        None,
    );
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    assert_eq!(execution_view(&f, pat, &exe)["status"], "cancelled");
}

/// The step cap (`RUNBOOK_LIMIT_STEP_RUNS`, here 2) on Workers, as
/// natively: a run that would open a third step fails with
/// `limit.step_runs_exceeded`, keeping the two it did, and says why.
#[test]
fn a_run_stops_at_the_step_cap() {
    let f = Fixture::with_limits(None, Some(2));
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let steps: Vec<String> = (1..=3)
        .map(|n| {
            format!(
                "{{id: s{n}, title: Step {n}, assignee: \"user:{}\"}}",
                out.user_id
            )
        })
        .collect();
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: capped, title: Capped}}\n\
         spec: {{steps: [{}]}}\n",
        steps.join(", ")
    );
    let exe = publish_and_start(&f, pat, &project, &source, "capped", json!({}));
    for step in ["s1", "s2"] {
        let done = report_logged(&f, pat, &exe, step, json!(["ok"]), step);
        assert_eq!(done.status, 200, "{}", done.body);
    }
    let view = execution_view(&f, pat, &exe);
    assert_eq!(view["status"], "failed", "{view}");
    assert_eq!(
        view["terminal_reason"], "limit.step_runs_exceeded",
        "{view}"
    );
    assert_eq!(view["steps"].as_array().unwrap().len(), 2, "{view}");
}

/// The default budget (measured: a report on a run at 8 MiB peaks near
/// 61 MB) leaves the CF13 runs alone: the 40-step run with 65,000-byte
/// summaries is about 5.3 MB of log, and the tests above complete it.
#[test]
fn the_default_run_budget_is_8_mib() {
    assert_eq!(
        runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES,
        8 * 1024 * 1024
    );
    assert_eq!(
        Fixture::new().host.run_budget(),
        runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES
    );
}

/// The bytes of `exe`'s stored event log (each envelope as its row keeps it).
fn log_bytes(f: &Fixture, exe: &str) -> i64 {
    use runbook_store_do::DoSql;
    let sql = f.host.objects.map.borrow()[&exe.parse::<ExecutionId>().unwrap()].clone();
    let rows = sql
        .query(
            "SELECT coalesce(sum(length(CAST(envelope AS BLOB))), 0) FROM event",
            &[],
        )
        .unwrap();
    let runbook_store_do::Value::Integer(n) = rows[0][0] else {
        panic!("{:?}", rows[0][0]);
    };
    n
}

/// Publish `source` (a runbook other runbooks call), without starting it.
fn publish_only(f: &Fixture, pat: &str, project: &str, source: &str, slug: &str) {
    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({ "source": source }),
        Some(&format!("create-{slug}")),
        None,
    );
    assert_eq!(created.status, 201, "{}", created.body);
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let draft = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}/draft"),
        Value::Null,
        None,
        None,
    );
    let published = f.call(
        pat,
        "POST",
        &format!("/v1/runbooks/{rb}/versions"),
        json!({}),
        Some(&format!("publish-{slug}")),
        draft.etag.as_deref(),
    );
    assert_eq!(published.status, 201, "{}", published.body);
}

/// A parent of `calls` call steps in sequence, each calling `child` (which
/// returns 15 outputs) and keeping all 15.
fn calling_source(id: &str, child: &str, calls: u32) -> String {
    let declared: Vec<String> = (1..=15)
        .map(|n| format!("o{n:02}: {{type: string}}"))
        .collect();
    let steps: Vec<String> = (1..=calls)
        .map(|n| {
            format!(
                "    - id: c{n}\n      call: {{runbook: {child}, outputs: {{{}}}}}\n",
                declared.join(", ")
            )
        })
        .collect();
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: {id}, title: Calls}}\nspec:\n  steps:\n{}",
        steps.concat()
    )
}

/// Run `parent_exe`'s call step `path` through: its child starts, its work
/// is reported with a 60,000-byte blob (so the child returns about
/// 900 KB), and the parent receives the outcome.
fn complete_call(f: &Fixture, pat: &str, parent_exe: &str, path: &str) {
    let child_exe = (0..10)
        .find_map(|_| {
            let view = execution_view(f, pat, parent_exe);
            let step = view["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["path"] == path)?
                .clone();
            step["child_execution_id"]
                .as_str()
                .or_else(|| step["call"]["child_execution_id"].as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("{path} has no child"));
    for _ in 0..10 {
        if execution_view(f, pat, &child_exe)["status"] != "queued" {
            break;
        }
    }
    let reported = report_step(
        f,
        pat,
        &child_exe,
        "work",
        json!({"blob": "b".repeat(60_000)}),
    );
    assert_eq!(reported.status, 200, "{}", reported.body);
    assert_eq!(execution_view(f, pat, &child_exe)["status"], "succeeded");
    // The parent receives the outcome as the host settles it.
    for _ in 0..10 {
        let view = execution_view(f, pat, parent_exe);
        let step = view["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == path)
            .cloned()
            .unwrap();
        if step["status"] != "waiting" {
            break;
        }
    }
}

/// CF13b, item 1: the run budget refuses only requests' large events, and
/// the engine's own are never counted: each `call` step's `CallCompleted`
/// carries its child's outputs (up to 900 KB), so a definition alone grew
/// a run's log without bound, and on Workers every request loads all of it
/// (about 130 MB at 18 MiB). The engine (version 6) now holds the log to a
/// hard ceiling of one and a half budgets (here a 2,000,000-byte budget, a
/// 3,000,000-byte ceiling): the call result that passes it faults the run,
/// which closes failed in the same commit with `limit.run_size_exceeded`,
/// and its later calls never start. Before, all five calls ran and the run
/// succeeded with about 4.5 MB of log.
#[test]
fn a_run_whose_call_results_pass_the_ceiling_closes_itself() {
    let f = Fixture::with_run_budget(Some(2_000_000));
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let child = wide_outputs_source("bigchild", &out.user_id, 15, "");
    publish_only(&f, pat, &project, &child, "bigchild");
    let parent = calling_source("heavy", "bigchild", 5);
    let exe = publish_and_start(&f, pat, &project, &parent, "heavy", json!({}));
    let mut completed = 0;
    for n in 1..=5 {
        if execution_view(&f, pat, &exe)["status"] == "failed" {
            break;
        }
        complete_call(&f, pat, &exe, &format!("c{n}"));
        completed = n;
    }
    let view = execution_view(&f, pat, &exe);
    assert_eq!(view["status"], "failed", "{}", view["status"]);
    let faults = events_of(&f, &exe, "ExecutionFaulted");
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(
        faults[0]["fault"]["code"],
        runbook_engine::RUN_SIZE_EXCEEDED
    );
    assert_eq!(events_of(&f, &exe, "ExecutionCompleted").len(), 1);
    // The fourth result passed 3,000,000 bytes; the fifth call never ran.
    assert_eq!(completed, 4, "calls completed");
    assert_eq!(events_of(&f, &exe, "CallCompleted").len(), 4);
    assert!(
        view["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "c5")
            .is_none_or(|s| s["status"] != "succeeded")
    );
    // At most one result past the ceiling, and the close.
    let logged = log_bytes(&f, &exe);
    assert!(
        (3_000_000..3_000_000 + 1_000_000).contains(&logged),
        "{logged} bytes of log"
    );
    assert_rows_fit(&f, &exe);
    // The closed run still answers: its reads, and a cancel (a no-op on a
    // closed run, as for any other).
    let cancelled = f.call(
        pat,
        "PATCH",
        &format!("/v1/executions/{exe}"),
        json!({"action": "cancel"}),
        Some("cancel-heavy"),
        None,
    );
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    assert_eq!(execution_view(&f, pat, &exe)["status"], "failed");
}

/// A run past its budget but under its ceiling (three call results, about
/// 2.7 MB) is not faulted, and a cancel still closes it.
#[test]
fn a_run_between_its_budget_and_its_ceiling_can_be_cancelled() {
    let f = Fixture::with_run_budget(Some(2_000_000));
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let child = wide_outputs_source("bigchild", &out.user_id, 15, "");
    publish_only(&f, pat, &project, &child, "bigchild");
    let parent = calling_source("heavy", "bigchild", 5);
    let exe = publish_and_start(&f, pat, &project, &parent, "heavy", json!({}));
    for n in 1..=3 {
        complete_call(&f, pat, &exe, &format!("c{n}"));
    }
    let logged = log_bytes(&f, &exe);
    assert!(
        (2_000_000..3_000_000).contains(&logged),
        "{logged} bytes of log"
    );
    assert_eq!(execution_view(&f, pat, &exe)["status"], "running");
    let cancelled = f.call(
        pat,
        "PATCH",
        &format!("/v1/executions/{exe}"),
        json!({"action": "cancel"}),
        Some("cancel-heavy"),
        None,
    );
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    let mut view = execution_view(&f, pat, &exe);
    if view["status"] != "cancelled" {
        view = execution_view(&f, pat, &exe);
    }
    assert_eq!(view["status"], "cancelled", "{}", view["status"]);
    assert!(events_of(&f, &exe, "ExecutionFaulted").is_empty());
    assert_rows_fit(&f, &exe);
}

/// A runbook of `sets` `set` steps, each assigning `vars` variables the
/// whole of a string input, then a person's step that holds the run open.
fn sets_then_hold(user: &impl std::fmt::Display, sets: u32, vars: u32) -> String {
    let names: Vec<String> = (1..=vars).map(|n| format!("a{n:02}")).collect();
    let declared: Vec<String> = names.iter().map(|n| format!("{n}: ''")).collect();
    let assigned: Vec<String> = names
        .iter()
        .map(|n| format!("{n}: \"${{{{ inputs.blob }}}}\""))
        .collect();
    let steps: String = (1..=sets)
        .map(|n| {
            format!(
                "    - {{id: s{n}, set: {{vars: {{{}}}}}}}\n",
                assigned.join(", ")
            )
        })
        .collect();
    format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: sets, title: Sets}}\nspec:\n  \
         inputs: {{blob: {{type: string, required: true}}}}\n  vars: {{{}}}\n  steps:\n\
         {steps}    - {{id: hold, title: Hold, assignee: \"user:{user}\"}}\n",
        declared.join(", ")
    )
}

/// Start `source` with a 60,000-byte input and let it activate; the view
/// once it is no longer queued.
fn start_sets(f: &Fixture, pat: &str, project: &str, source: &str) -> (String, Value) {
    let exe = publish_and_start(
        f,
        pat,
        project,
        source,
        "sets",
        json!({"blob": "b".repeat(60_000)}),
    );
    let mut view = execution_view(f, pat, &exe);
    for _ in 0..10 {
        if view["status"] != "queued" {
            break;
        }
        view = execution_view(f, pat, &exe);
    }
    (exe, view)
}

/// The run was faulted for its size and closed failed while it activated:
/// one fault, one close, fewer `DataChanged` than `set` steps, the person's
/// step never opened, the log at most about one settle round past
/// `ceiling`, and every row within a Durable Object row.
fn assert_closed_at_activation(f: &Fixture, exe: &str, view: &Value, sets: usize, ceiling: i64) {
    assert_eq!(view["status"], "failed", "{}", view["status"]);
    let faults = events_of(f, exe, "ExecutionFaulted");
    assert_eq!(faults.len(), 1, "{faults:?}");
    assert_eq!(
        faults[0]["fault"]["code"],
        runbook_engine::RUN_SIZE_EXCEEDED
    );
    assert_eq!(events_of(f, exe, "ExecutionCompleted").len(), 1);
    let changed = events_of(f, exe, "DataChanged").len();
    assert!(changed < sets, "{changed} of {sets} set steps ran");
    assert!(
        view["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["path"] == "hold")
            .is_none_or(|s| !matches!(s["status"].as_str(), Some("ready" | "claimed"))),
        "{}",
        view["steps"]
    );
    let logged = log_bytes(f, exe);
    assert!(
        (ceiling..ceiling + 2_000_000).contains(&logged),
        "{logged} bytes of log, ceiling {ceiling}"
    );
    assert_rows_fit(f, exe);
}

/// CF13b review round 1, finding 1: activation (`start::activate`) settled
/// on a bare runtime with no ceiling, so a definition of `set` steps alone
/// left a run open far past it (budget 400,000 bytes, ceiling 600,000:
/// thirty `set` steps of a 60,000-byte input left it `running` with more
/// than 1,800,000 bytes of log). Activation is now held to the ceiling:
/// the run is faulted and closes failed in its activation commit.
#[test]
fn an_activation_that_passes_the_ceiling_closes_the_run() {
    let f = Fixture::with_run_budget(Some(400_000));
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let source = sets_then_hold(&out.user_id, 30, 1);
    let (exe, view) = start_sets(&f, pat, &project, &source);
    assert_closed_at_activation(&f, &exe, &view, 30, 600_000);
}

/// The same at the default budget (8 MiB, ceiling 12 MiB): twenty `set`
/// steps each setting sixteen variables to a 60,000-byte input (about
/// 960 KB a step, every row under 2 MB). Before, the run was left
/// `running` at about 19.3 MB, which no later request on Workers could
/// load.
#[test]
fn an_activation_at_the_default_budget_closes_the_run_at_its_ceiling() {
    let f = Fixture::with_run_budget(None);
    let out = bootstrapped(&f);
    let pat = out.personal_token.as_str();
    let project = out.project_id.to_string();
    let source = sets_then_hold(&out.user_id, 20, 16);
    let (exe, view) = start_sets(&f, pat, &project, &source);
    assert_closed_at_activation(&f, &exe, &view, 20, 12 * 1024 * 1024);
}
