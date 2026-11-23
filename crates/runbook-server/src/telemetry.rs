//! Native HTTP tracing. The portable core and Workers do not depend on an SDK.
use std::{
    future::Future,
    io::Read,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime},
};

use axum::{
    body::Body,
    http::{Request, Response},
    middleware::Next,
};
use opentelemetry::{
    Context, KeyValue, global,
    propagation::TextMapPropagator,
    trace::{FutureExt, SpanKind, Status, TraceContextExt, Tracer},
};
use opentelemetry_http::{HeaderExtractor, HttpClient, HttpError};
use opentelemetry_otlp::{SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    Resource,
    propagation::TraceContextPropagator,
    trace::{Sampler, SdkTracerProvider},
};

const GOOGLE_ENDPOINT: &str = "https://telemetry.googleapis.com/v1/traces";
const EXPORT_TIMEOUT: Duration = Duration::from_secs(3);
static PROJECT: OnceLock<Option<String>> = OnceLock::new();

fn sampling_ratio(value: Option<&str>) -> f64 {
    value
        .and_then(|value| value.trim().parse().ok())
        .filter(|value| (0.0..=1.0).contains(value))
        .unwrap_or(0.1)
}

pub fn configured_ratio() -> f64 {
    sampling_ratio(std::env::var("RUNBOOK_TRACE_SAMPLE_RATIO").ok().as_deref())
}

fn otlp_endpoint() -> Option<String> {
    std::env::var("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
        .ok()
        .filter(|url| !url.is_empty())
        .or_else(|| {
            std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
                .ok()
                .filter(|url| !url.is_empty())
                .map(|url| format!("{}/v1/traces", url.trim_end_matches('/')))
        })
}

pub fn advertised_ratio() -> Option<String> {
    (otlp_endpoint().is_some() || std::env::var_os("RUNBOOK_TRACE_SAMPLE_RATIO").is_some())
        .then(|| configured_ratio().to_string())
}

pub fn init() -> SdkTracerProvider {
    let project = project();
    let mut resource = Resource::builder_empty().with_service_name("runbook-core");
    if let Some(project) = project {
        resource = resource.with_attribute(KeyValue::new("gcp.project_id", project));
    }
    let mut provider = SdkTracerProvider::builder()
        .with_resource(resource.build())
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            configured_ratio(),
        ))));
    if let Some(endpoint) = otlp_endpoint() {
        let google = endpoint == GOOGLE_ENDPOINT;
        match SpanExporter::builder()
            .with_http()
            .with_endpoint(endpoint)
            .with_timeout(EXPORT_TIMEOUT)
            .with_http_client(OtlpClient::new(google))
            .build()
        {
            Ok(exporter) => provider = provider.with_batch_exporter(exporter),
            Err(_) => crate::log::warning("telemetry", "trace exporter configuration invalid"),
        }
    }
    let provider = provider.build();
    global::set_tracer_provider(provider.clone());
    provider
}

struct GkeToken {
    // Avoid printing this struct or the downstream HTTP error.
    authorization: http::HeaderValue,
    refresh_after: Instant,
}

struct OtlpClient {
    agent: ureq::Agent,
    token: Option<Mutex<Option<GkeToken>>>,
}

impl std::fmt::Debug for OtlpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OtlpClient")
    }
}

impl OtlpClient {
    fn new(google: bool) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout(EXPORT_TIMEOUT)
                .build(),
            token: google.then(|| Mutex::new(None)),
        }
    }

    fn authorization(&self) -> Result<Option<http::HeaderValue>, HttpError> {
        let Some(cache) = &self.token else {
            return Ok(None);
        };
        let mut cache = cache
            .lock()
            .map_err(|_| "metadata token cache unavailable")?;
        if let Some(token) = &*cache
            && token.refresh_after > Instant::now()
        {
            return Ok(Some(token.authorization.clone()));
        }
        #[derive(serde::Deserialize)]
        struct MetadataToken {
            access_token: String,
            expires_in: u64,
        }
        let response = self
            .agent
            .get(
                "http://169.254.169.254/computeMetadata/v1/instance/service-accounts/default/token",
            )
            .set("Metadata-Flavor", "Google")
            .call()
            .map_err(|_| "metadata token request failed")?;
        let token: MetadataToken = serde_json::from_reader(response.into_reader().take(8192))
            .map_err(|_| "metadata token response invalid")?;
        let mut authorization =
            http::HeaderValue::from_str(&format!("Bearer {}", token.access_token))
                .map_err(|_| "metadata token response invalid")?;
        authorization.set_sensitive(true);
        *cache = Some(GkeToken {
            authorization: authorization.clone(),
            refresh_after: Instant::now()
                + Duration::from_secs(token.expires_in.clamp(60, 3600) - 60),
        });
        Ok(Some(authorization))
    }
}

// The SDK's batch processor performs export on a dedicated thread.
#[async_trait::async_trait]
impl HttpClient for OtlpClient {
    async fn send_bytes(
        &self,
        request: http::Request<bytes::Bytes>,
    ) -> Result<http::Response<bytes::Bytes>, HttpError> {
        let mut call = self
            .agent
            .request(request.method().as_str(), &request.uri().to_string());
        for (key, value) in request.headers() {
            call = call.set(key.as_str(), value.to_str()?);
        }
        if let Some(auth) = self.authorization()? {
            call = call.set("authorization", auth.to_str()?);
        }
        let response = match call.send_bytes(request.body()) {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(_) => return Err("OTLP transport failed".into()),
        };
        let mut builder = http::Response::builder().status(response.status());
        for name in response.headers_names() {
            if let Some(value) = response.header(&name) {
                builder = builder.header(name, value);
            }
        }
        let mut body = Vec::new();
        response
            .into_reader()
            .take(256 * 1024 + 1)
            .read_to_end(&mut body)?;
        if body.len() > 256 * 1024 {
            return Err("OTLP response too large".into());
        }
        Ok(builder.body(body.into())?)
    }
}

// A core-created root has no runner work attached to it.
struct InboundTrace;

/// A runner-shaped bearer is only a hint to defer sampling. The portable
/// router supplies the authenticated context after checking the credential.
struct PendingRunnerTrace {
    parent: Context,
    started: SystemTime,
    authenticated: OnceLock<Context>,
}

#[derive(Default)]
struct RequestLabels {
    org: Option<String>,
    project: Option<String>,
    run: Option<String>,
    step_run: Option<String>,
}

fn label(value: &str, prefix: &str) -> Option<String> {
    let id = value.strip_prefix(prefix)?;
    (!id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-'))
    .then(|| value.to_owned())
}

pub async fn http_server(request: Request<Body>, next: Next) -> Response<Body> {
    let path = request.uri().path();
    if !path.starts_with("/v1/") && path != "/mcp" {
        return next.run(request).await;
    }
    let mut parts = path.split('/').skip(2);
    let kind = parts.next();
    let id = parts.next().unwrap_or_default();
    let labels = RequestLabels {
        org: None,
        project: None,
        run: (kind == Some("executions"))
            .then(|| label(id, "exe_"))
            .flatten(),
        step_run: (kind == Some("step-runs"))
            .then(|| label(id, "sr_"))
            .flatten(),
    };
    let started = SystemTime::now();
    let base = Context::new().with_value(Arc::new(Mutex::new(labels)));
    if let Some(parent) = candidate_runner_parent(request.headers()) {
        let pending = PendingRunnerTrace {
            parent,
            started,
            authenticated: OnceLock::new(),
        };
        let cx = base.with_value(pending);
        let response = next.run(request).with_context(cx.clone()).await;
        let pending = cx.get::<PendingRunnerTrace>().expect("request trace scope");
        let status = Some(response.status().as_u16());
        if let Some(authenticated) = pending.authenticated.get() {
            finish(authenticated, status, true);
        } else {
            finish(&server_context(&cx, &Context::new(), started), status, true);
        }
        return response;
    }
    let cx = server_context(&base, &Context::new(), started);
    let response = next.run(request).with_context(cx.clone()).await;
    finish(&cx, Some(response.status().as_u16()), true);
    response
}

fn candidate_runner_parent(headers: &http::HeaderMap) -> Option<Context> {
    use runbook_core::runners::{RunnerSecretKind, runner_secret_kind};
    let traceparent = headers.get("traceparent")?;
    let bearer = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    if runner_secret_kind(bearer)? == RunnerSecretKind::Bootstrap {
        return None;
    }
    let mut carrier = http::HeaderMap::new();
    carrier.insert("traceparent", traceparent.clone());
    let context = TraceContextPropagator::new()
        .extract_with_context(&Context::new(), &HeaderExtractor(&carrier));
    context
        .span()
        .span_context()
        .is_remote()
        .then(|| context.clone())
}

fn server_context(base: &Context, parent: &Context, started: SystemTime) -> Context {
    let tracer = global::tracer("runbook-core");
    let span = tracer
        .span_builder("http.server")
        .with_kind(SpanKind::Server)
        .with_start_time(started)
        .start_with_context(&tracer, parent);
    base.with_span(span)
}

pub async fn authenticated_runner<T>(work: impl Future<Output = T>) -> T {
    let cx = Context::current();
    let Some(pending) = cx.get::<PendingRunnerTrace>() else {
        return work.await;
    };
    // Build from the remote context to keep the stored context from retaining
    // the pending state that owns it. Share the request labels across threads.
    let mut base = Context::new().with_value(InboundTrace);
    if let Some(labels) = cx.get::<Arc<Mutex<RequestLabels>>>() {
        base = base.with_value(labels.clone());
    }
    let active = pending
        .authenticated
        .get_or_init(|| server_context(&base, &pending.parent, pending.started));
    work.with_context(active.clone()).await
}

pub fn client_context() -> Context {
    let parent = Context::current();
    let tracer = global::tracer("runbook-core");
    let span = tracer
        .span_builder("runner.http.client")
        .with_kind(SpanKind::Client)
        .start_with_context(&tracer, &parent);
    parent.with_span(span)
}

pub fn inject(cx: &Context, headers: &mut Vec<(String, String)>) {
    let mut carrier = std::collections::HashMap::new();
    TraceContextPropagator::new().inject_context(cx, &mut carrier);
    if let Some(value) = carrier.remove("traceparent") {
        headers.push(("traceparent".into(), value));
    }
}

pub fn finish(cx: &Context, status: Option<u16>, server: bool) {
    let span = cx.span();
    if let Some(status) = status {
        span.set_attribute(KeyValue::new(
            "http.response.status_code",
            i64::from(status),
        ));
    }
    if status.is_none_or(|code| code >= if server { 500 } else { 400 }) {
        span.set_status(Status::error("http request failed"));
    }
    span.end();
}

pub fn log_fields(value: &mut serde_json::Value, cx: &Context, project: Option<&str>) {
    let mut labels = serde_json::Map::new();
    labels.insert("runner_process".into(), "core".into());
    if let Some(locked) = cx.get::<Arc<Mutex<RequestLabels>>>()
        && let Ok(scoped) = locked.lock()
    {
        if let Some(org) = &scoped.org {
            labels.insert("org".into(), org.clone().into());
        }
        if let Some(project) = &scoped.project {
            labels.insert("project".into(), project.clone().into());
        }
        if let Some(run) = &scoped.run {
            labels.insert("run".into(), run.clone().into());
        }
        if let Some(step) = &scoped.step_run {
            labels.insert("step_run".into(), step.clone().into());
        }
    }
    value["logging.googleapis.com/labels"] = labels.into();
    let span = cx.span();
    let span = span.span_context();
    if span.is_valid() {
        let trace_id = span.trace_id();
        value["logging.googleapis.com/trace"] = project
            .map_or_else(
                || trace_id.to_string(),
                |project| format!("projects/{project}/traces/{trace_id}"),
            )
            .into();
        value["logging.googleapis.com/spanId"] = span.span_id().to_string().into();
        value["logging.googleapis.com/trace_sampled"] = span.is_sampled().into();
    }
}

pub fn project() -> Option<&'static str> {
    PROJECT
        .get_or_init(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok())
        .as_deref()
}

pub fn label_execution(
    scope: &runbook_core::catalog::TenantScope,
    run: &runbook_model::ExecutionId,
    step: &runbook_model::StepRunId,
) {
    if let Some(locked) = Context::current().get::<Arc<Mutex<RequestLabels>>>()
        && let Ok(mut labels) = locked.lock()
    {
        labels.org = Some(scope.org_id.to_string());
        labels.project = Some(scope.project_id.to_string());
        labels.run = Some(run.to_string());
        labels.step_run = Some(step.to_string());
    }
}

pub fn active_trace_id() -> Option<String> {
    let cx = Context::current();
    cx.get::<InboundTrace>()?;
    let span = cx.span();
    let span = span.span_context();
    (span.is_valid() && span.is_sampled()).then(|| span.trace_id().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_defaults_and_bounds() {
        assert_eq!(sampling_ratio(None), 0.1);
        for (input, expected) in [
            ("0", 0.0),
            ("1", 1.0),
            (" 0.25 ", 0.25),
            ("2", 0.1),
            ("-1", 0.1),
            ("", 0.1),
            ("NaN", 0.1),
        ] {
            assert_eq!(sampling_ratio(Some(input)), expected);
        }
    }
}
