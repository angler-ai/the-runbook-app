//! CF13 review round 1, item 2: how much memory one request of a large run
//! takes, measured natively with the real router over the object protocol
//! (every object request and reply goes through JSON, as between the api
//! Worker and its ExecutionDO), counting the Rust heap with a peak-tracking
//! allocator. SQLite's own pages are not counted (in Workers they are not
//! the isolate's heap either).
//!
//! The run: a `foreach` of 100 items, 5 tasks each, every task reported
//! with a 64,000-byte summary. Ignored by default (minutes in a debug
//! build): `cargo test -p runbook-worker --test run_memory -- --ignored
//! --nocapture` prints the peak heap of a report request as the run grows.
//! `RB_MEMORY_BUDGET` (bytes) raises the run budget to measure past 8 MiB.
use runbook_api::{
    Credential, Request,
    backends::NoEntropy,
    router::{Router, RouterConfig},
};
use runbook_core::webhooks::TargetPolicy;
use runbook_model::ExecutionId;
use runbook_store_d1::native::NativeD1;
use runbook_store_r2::MemoryBucket;
use runbook_worker::{
    cloudflare::{CfBackends, CfHost},
    compose::Objects,
    protocol::RemoteObjects,
    sealer::AesSealer,
};
use runbook_worker_conformance::{
    cross::{CrossHost, InProcess, NativeCross},
    native::run,
};
use serde_json::{Value, json};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Peak;
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Peak {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(ptr, layout, new_size) };
        if !out.is_null() {
            if new_size > layout.size() {
                let grow = new_size - layout.size();
                let now = CURRENT.fetch_add(grow, Ordering::Relaxed) + grow;
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        out
    }
}
#[global_allocator]
static ALLOCATOR: Peak = Peak;

const START_MS: i64 = 1_800_000_000_000;

/// The most heap any object request held above what was allocated when it
/// began (the object's side and the wire), since the last reset.
static OBJECT_PEAK: AtomicUsize = AtomicUsize::new(0);

/// The native transport, measuring each object request on its own.
#[derive(Clone)]
struct Probe(InProcess);
impl runbook_worker::protocol::Transport for Probe {
    async fn call(
        &self,
        request: runbook_worker::protocol::Request,
    ) -> runbook_worker::protocol::Reply {
        use runbook_worker::protocol::Op;
        let label = match &request.op {
            Op::Begin => "begin",
            Op::Load => "load",
            Op::CommitFencedRef { .. } => "commit_fenced_ref",
            Op::CommitFenced { .. } => "commit_fenced",
            Op::Commit { .. } => "commit",
            _ => "other",
        };
        let outer = PEAK.load(Ordering::Relaxed);
        let base = CURRENT.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        let reply = self.0.call(request).await;
        let inner = PEAK.load(Ordering::Relaxed);
        if std::env::var_os("RB_MEMORY_OPS").is_some() {
            eprintln!(
                "  {label}: {} held before, {} more",
                base,
                inner.saturating_sub(base)
            );
        }
        OBJECT_PEAK.fetch_max(inner.saturating_sub(base), Ordering::Relaxed);
        PEAK.store(outer.max(inner), Ordering::Relaxed);
        reply
    }
    async fn call_org(
        &self,
        request: runbook_worker::quota::QuotaRequest,
    ) -> runbook_worker::protocol::Reply {
        self.0.call_org(request).await
    }
    async fn call_webhook(
        &self,
        request: runbook_worker::webhooks::WebhookRequest,
    ) -> runbook_worker::protocol::Reply {
        self.0.call_webhook(request).await
    }
    fn clock(&self) -> Option<i64> {
        self.0.clock()
    }
}

struct TestWebhookHost;
impl runbook_api::webhooks::WebhookHost for TestWebhookHost {
    type Sealer = AesSealer;
    fn sealer(&self) -> Option<&AesSealer> {
        None
    }
    fn random32(&self) -> Option<[u8; 32]> {
        Some([7; 32])
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
struct Host {
    d1: NativeD1,
    limits: runbook_worker::limits::MemoryLimits,
    objects: RemoteObjects<Probe>,
    clock: Rc<Cell<i64>>,
    blobs: MemoryBucket,
    user_content: MemoryBucket,
}
impl CfHost for Host {
    type Sql = runbook_worker_conformance::native::NativeSql;
    type D1 = NativeD1;
    type Bucket = MemoryBucket;
    type Objects = RemoteObjects<Probe>;
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
    fn objects(&self) -> RemoteObjects<Probe> {
        self.objects.clone()
    }
    fn webhook_host(&self) -> TestWebhookHost {
        TestWebhookHost
    }
    fn now_ms(&self) -> i64 {
        self.clock.get()
    }
    fn fill_random(&self, bytes: &mut [u8]) -> Result<(), NoEntropy> {
        getrandom::fill(bytes).map_err(|_| NoEntropy)
    }
    /// `RB_MEMORY_BUDGET` raises the run budget (and the ceiling with it,
    /// CF13b) to measure a run past the default.
    fn run_budget(&self) -> u64 {
        std::env::var("RB_MEMORY_BUDGET")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(runbook_core::run_budget::DEFAULT_MAX_RUN_BYTES)
    }
}

struct Fixture {
    host: Host,
    router: Router<CfBackends<Host>>,
    counter: Cell<u64>,
}
struct Reply {
    status: u16,
    body: Value,
    etag: Option<String>,
    /// The most heap the request (and the settle after it) held above
    /// what was allocated before it.
    peak: usize,
    /// The most of that any one object request held (its side and the
    /// wire; the rest is the api Worker's).
    object_peak: usize,
}
impl Fixture {
    fn new() -> Self {
        let d1 = NativeD1::new(START_MS);
        run(runbook_store_d1::migrate::migrate(&d1)).unwrap();
        let clock = Rc::new(Cell::new(START_MS));
        let host = Host {
            d1,
            limits: Default::default(),
            objects: RemoteObjects::tracked(Probe(NativeCross.transport(clock.clone()))),
            clock,
            blobs: MemoryBucket::default(),
            user_content: MemoryBucket::default(),
        };
        let config: RouterConfig =
            runbook_worker::cloudflare::router_config("https://runbook.test", 1 << 20);
        Self {
            router: Router::new(CfBackends::new(host.clone()), config),
            host,
            counter: Cell::new(0),
        }
    }
    fn call(&self, token: &str, method: &str, path: &str, body: Value, key: Option<&str>) -> Reply {
        self.host.clock.set(self.host.clock.get() + 1_000);
        self.host.d1.set_time(self.host.clock.get());
        self.counter.set(self.counter.get() + 1);
        let mut random = [0u8; 10];
        random[8..].copy_from_slice(&u16::try_from(self.counter.get()).unwrap().to_be_bytes());
        let request = Request {
            method: method.into(),
            path: path.into(),
            query: Default::default(),
            body,
            credential: (!token.is_empty()).then(|| Credential::Bearer(token.into())),
            idempotency_key: key.map(str::to_owned),
            request_id: format!("req_{}", self.counter.get()),
            new_id: runbook_api::backends::encode_id("", self.host.clock.get(), random).unwrap(),
            if_match: None,
            client_ip: None,
        };
        let base = CURRENT.load(Ordering::Relaxed);
        PEAK.store(base, Ordering::Relaxed);
        OBJECT_PEAK.store(0, Ordering::Relaxed);
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
        let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
        let object_peak = OBJECT_PEAK.load(Ordering::Relaxed);
        Reply {
            object_peak,
            status: encoded.status,
            etag: encoded.header_value("etag").map(str::to_owned),
            body: serde_json::from_slice(&encoded.body).unwrap_or(Value::Null),
            peak,
        }
    }
}

/// The bytes of `exe`'s event log as stored (each envelope's JSON).
fn log_bytes(f: &Fixture, scope: &runbook_core::catalog::TenantScope, exe: &str) -> usize {
    use runbook_worker::object::ObjectHandle;
    let id: ExecutionId = exe.parse().unwrap();
    let object = f.host.objects.object(&id, scope).unwrap();
    let stored = run(object.load(&id)).unwrap();
    stored
        .events
        .iter()
        .map(|e| serde_json::to_vec(e).unwrap().len())
        .sum()
}

#[test]
#[ignore = "a measurement: minutes in a debug build"]
fn a_large_runs_report_requests_peak_heap() {
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
    let scope = runbook_core::catalog::TenantScope {
        org_id: out.org_id.clone(),
        project_id: out.project_id.clone(),
    };
    let user = out.user_id.to_string();
    let tasks: Vec<String> = (1..=5)
        .map(|n| {
            format!("{{id: t{n}, title: 'Task {n} ${{{{ item }}}}', assignee: \"user:{user}\"}}")
        })
        .collect();
    let source = format!(
        "apiVersion: therunbook.app/v1\nkind: Runbook\n\
         metadata: {{id: big, title: Big}}\n\
         spec:\n  inputs:\n    items: {{type: list, items: {{type: integer}}}}\n  steps:\n    - id: each\n      \
         foreach: {{items: '${{{{ inputs.items }}}}', as: item, max_items: 100, concurrency: 1, \
         steps: [{}]}}\n",
        tasks.join(", ")
    );
    let created = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/runbooks"),
        json!({ "source": source }),
        Some("create"),
    );
    assert_eq!(created.status, 201, "{}", created.body);
    let rb = created.body["runbook_id"].as_str().unwrap().to_owned();
    let draft = f.call(
        pat,
        "GET",
        &format!("/v1/runbooks/{rb}/draft"),
        Value::Null,
        None,
    );
    let publish = Request {
        method: "POST".into(),
        path: format!("/v1/runbooks/{rb}/versions"),
        query: Default::default(),
        body: json!({}),
        credential: Some(Credential::Bearer(pat.into())),
        idempotency_key: Some("publish".into()),
        request_id: "req_publish".into(),
        new_id: runbook_api::backends::encode_id("", f.host.clock.get(), [1; 10]).unwrap(),
        if_match: draft.etag.clone(),
        client_ip: None,
    };
    let published = runbook_api::http::encode(run(f.router.handle(publish)));
    assert_eq!(published.status, 201);
    let items: Vec<u32> = (0..100).collect();
    let started = f.call(
        pat,
        "POST",
        &format!("/v1/projects/{project}/executions"),
        json!({"runbook": "big", "inputs": {"items": items}}),
        Some("start"),
    );
    assert_eq!(started.status, 201, "{}", started.body);
    let exe = started.body["id"].as_str().unwrap().to_owned();
    let summary = "s".repeat(64_000);
    let mut reports = 0usize;
    let mut worst = 0usize;
    // `RB_MEMORY_REPORTS` stops early (a quicker measurement).
    let stop: usize = std::env::var("RB_MEMORY_REPORTS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);
    println!(
        "reports | event log bytes | report peak heap | of it one object request | worst so far"
    );
    loop {
        let view = f.call(
            pat,
            "GET",
            &format!("/v1/executions/{exe}"),
            Value::Null,
            None,
        );
        assert_eq!(view.status, 200);
        let ready: Vec<String> = view.body["steps"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["status"] == "ready" && s["kind"] == "task")
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect();
        if ready.is_empty() {
            println!("run {} after {reports} reports", view.body["status"]);
            break;
        }
        for sr in ready {
            let claim = f.call(
                pat,
                "POST",
                &format!("/v1/step-runs/{sr}/claim"),
                json!({"session_id": "web"}),
                Some(&format!("claim-{reports}")),
            );
            assert_eq!(claim.status, 200, "{}", claim.body);
            let reply = f.call(
                pat,
                "POST",
                &format!("/v1/step-runs/{sr}/report"),
                json!({"step_run_id": sr, "claim": claim.body, "status": "succeeded",
                       "verdict": null, "outputs": {}, "summary": summary, "evidence": [],
                       "logs": []}),
                Some(&format!("report-{reports}")),
            );
            reports += 1;
            worst = worst.max(reply.peak).max(view.peak).max(claim.peak);
            if reply.status != 200 {
                println!(
                    "report {reports} answered {} {} at {} log bytes",
                    reply.status,
                    reply.body["code"],
                    log_bytes(&f, &scope, &exe)
                );
                println!("worst peak heap: {worst}");
                return;
            }
            if reports >= stop {
                println!(
                    "{reports} | {} | {} | {} | {worst} (stopped)",
                    log_bytes(&f, &scope, &exe),
                    reply.peak,
                    reply.object_peak
                );
                return;
            }
            if reports.is_multiple_of(50) {
                println!(
                    "{reports} | {} | {} | {} | {worst}",
                    log_bytes(&f, &scope, &exe),
                    reply.peak,
                    reply.object_peak
                );
            }
        }
    }
    println!("worst peak heap: {worst}");
}
