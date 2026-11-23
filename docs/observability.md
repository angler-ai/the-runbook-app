# Runbook observability

The native core and runner write Cloud Logging JSON to stdout. GKE's workload
logging collects those lines. Set `RUNBOOK_LOG_FORMAT=json` on the core. Logs
carry severity, time, component, and the organization, project, run, step run
and process labels when known. An active span also adds the Cloud Logging
trace, span ID, and sampled fields. [Cloud Logging structured fields](https://docs.cloud.google.com/logging/docs/structured-logging)

The core and runner use OpenTelemetry's parent-based sampler. The default root
ratio is `0.1`. Public core requests start a fresh trace. The core honors an
inbound W3C `traceparent` after authorizing a registered runner on its route.
Sampled parents stay sampled and unsampled parents stay unsampled. Set
`RUNBOOK_TRACE_SAMPLE_RATIO` consistently across the deployment. The native core
advertises its root ratio on `/v1/meta`. A sampled runner claim records its trace
ID in the step details. The authenticated run document supplies the Cloud Trace
link's project when the core has `GOOGLE_CLOUD_PROJECT` configured. The
Cloudflare core runs without the native SDK.

Trace headers propagate between Runbook services. Calls to external providers
record local client spans. Each sandbox listener records up to 100 egress
requests and their client spans, then counts additional requests on its
`egress.step` span. Routine runner polling, heartbeats, and wakeups skip server
spans.

Span names and attributes use allowlisted metadata. HTTP instrumentation omits
headers, query strings, and bodies. Model spans record the configured model,
token counts and cost. Tool spans record the pinned server and tool names.
Credential release spans record the credential ID. Application logging uses
the existing redactor and aggregate counts for repeat warnings and errors.

## GKE deployment

Use these Terraform inputs for production.

```hcl
trace_export_enabled   = true
trace_sample_ratio     = 0.1
log_exclusions_enabled = true
```

Pass the inputs from `angler-runbooks` to the Runbook module. The module puts
the sampling and export settings into the core ConfigMap and the runner
process environments. Its observability resources enable the APIs, configure
IAM and the additional runner egress, and add the project exclusion.

The processes send OTLP HTTP/protobuf directly to
`https://telemetry.googleapis.com/v1/traces`. The core, API and vault use their
existing Google service accounts through Workload Identity. Worker and
sandbox-ctl use their individual Kubernetes Workload Identity principals. The
gateway exports no spans: it parses untrusted sandbox traffic, so it holds no
Google credential and no route to the metadata server. It still samples,
propagates trace context between runner services and writes trace IDs into its
logs. No trace context is sent to hosts outside the deployment.
`roles/cloudtrace.agent` includes `telemetry.traces.write`. The sink writes to
the deployment project. No collector is needed. [Google's direct OTLP exporter guide](https://docs.cloud.google.com/stackdriver/docs/instrumentation/migrate-to-otlp-endpoints) · [Cloud Trace IAM permissions](https://docs.cloud.google.com/trace/docs/iam) · [Restricted VIP supported services](https://docs.cloud.google.com/vpc-service-controls/docs/restricted-vip-services)

The exclusion drops debug logs, and probe logs below warning severity, from the
core and runner namespaces before storage. Workload logs use the project's existing `_Default` bucket
and its 30-day retention.

## Docker

Export is off until you set `OTEL_EXPORTER_OTLP_ENDPOINT` or
`OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` in the container environment. The generic
endpoint is an HTTP/protobuf base URL, such as
`http://collector:4318`. The traces-specific endpoint includes the path.
Use Docker Compose service environment overrides for the core and runner
processes. Use `RUNBOOK_LOG_LEVEL=debug` on the runner only when investigating
an issue. Routine probe requests are omitted at every log level.

## Volume and cost

The loopback integration workload ran a three-step job through the runner
claim and report logic with one credential release per step. It counted
newline-terminated application JSON at info level with a Cloud project
configured. The six records occupied **3,558 bytes per run** or **1,186 bytes
per step**. The forced-sampled trace contained **27 spans**. The workload uses
mock core and vault peers and excludes GKE metadata, idle logs, sidecars,
provider-specific activity and failure bursts.

```sh
cd runner
RUNBOOK_TELEMETRY_MEASURE=1 npx vitest run --maxWorkers=2 packages/api/test/telemetry.test.ts
```

At 100,000 similar runs per month, the measured application log payload
projects to 0.331 GiB. At a 10% root sampling ratio the span estimate is
270,000. Both fit the free tiers if other workloads have not used them.
At fully consumed tiers, these volumes alone would add about **$0.22 per
month**. Use the project's billable logging bytes and billing account's
ingested spans to check the estimate after deployment.

Cloud Logging includes the first **50 GiB per project per month**, then
charges **$0.50/GiB** for storage through 30 days. Retention beyond 30 days
costs **$0.01/GiB per month**. Cloud Trace includes the first **2.5 million
spans per billing account per month**, then charges **$0.20 per million
spans**. Lower the root ratio to reduce span volume and leave the exclusion
enabled to keep debug and probe logs out of storage. Callers with a sampled
parent can increase the observed ratio above the root setting. [Google Cloud Observability pricing](https://cloud.google.com/products/observability/pricing)
