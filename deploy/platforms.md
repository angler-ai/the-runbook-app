# Running the container on a platform

The Runbook's self-hosted edition is one container image and one PostgreSQL
database. Evidence, archives and every other blob live in PostgreSQL, so the
container needs no disk: anything that runs a container and reaches
PostgreSQL can host it. Google Cloud Run is the reference platform; Fly.io,
Render, Kubernetes, ECS or a plain VM work the same way.

> **Status.** Everything below the "What any platform needs" section was
> written from the platforms' documentation and **has not been run against a
> real cloud account**. The behaviour it relies on (one port, `$PORT`, several
> replicas on one database, SIGTERM, TLS and Unix-socket database URLs) is
> tested locally: `crates/runbook-server/tests/replicas.rs` runs two replicas
> behind a proxy on one database, and `deploy/smoke/platform.sh` runs the
> image the way a platform does. Treat each `gcloud`, `fly` and `kubectl`
> command as a starting point and check it against your account.

## What any platform needs

| Need | What to give it |
| --- | --- |
| Image | The repository's `Dockerfile` (`docker build -t runbook:<version> .`), pushed to a registry your platform pulls from. Releases will also be published to ghcr.io (package UPD1). |
| Port | One HTTP port. The image sets `RUNBOOK_LISTEN_PORT_FROM_ENV=1` and `PORT=8080`, so with `RUNBOOK_LISTEN_ADDR` unset it listens on `0.0.0.0:$PORT` and a platform that sets `PORT` itself is followed. (Outside the image, `PORT` alone is ignored, so a stray variable in a shell never binds every interface.) Terminate TLS in front (every platform here does). |
| Database | `DATABASE_URL`, PostgreSQL 14 or newer. The server creates and migrates the `runbook` schema, and the sign-in service its `auth` schema, at start. See [Database URLs](#database-urls). |
| Secrets | `DATABASE_URL`, `RUNBOOK_WEBHOOK_SECRET_KEY`, and when used `RUNBOOK_AUTH_SECRET`, `RUNBOOK_UPLOAD_SIGNING_KEY`, `RUNBOOK_SMTP_URL`, `RUNBOOK_GOOGLE_CLIENT_SECRET`, from the platform's secret store as environment variables. Nothing is baked into the image. |
| Public URL | `RUNBOOK_ORIGIN`, the `https://` URL users reach. |
| User-content host | For evidence uploads, a second hostname routed to the **same** service, set as `RUNBOOK_USERCONTENT_ORIGIN`: a different **host name**, not the app's name on another port (ports make separate origins, but cookies ignore ports, so the app's cookies would reach user content; the server warns at start), and ideally another registrable domain, such as `files.example-usercontent.com`. Leave `RUNBOOK_USERCONTENT_LISTEN_ADDR` unset: the server then routes requests whose `Host` is that name to the user-content handler on the same port. The proxy in front must pass the `Host` header through unchanged. Without a second hostname, leave uploads off. |
| Probes | Liveness `GET /health/live` (never touches the database); readiness and startup `GET /health/ready` (the database answers and the instance is not shutting down). Both answer `{"status":...}` and nothing else. |
| Stop | SIGTERM (or SIGINT). The server stops within `RUNBOOK_SHUTDOWN_TIMEOUT_SECS` (default 8, inside Cloud Run's 10 s). Give it at least that long before SIGKILL. |
| Logs | `RUNBOOK_LOG_FORMAT=json` writes one JSON object per line on stdout (`severity`, `message`, `time`, `component`), which Cloud Logging, Fly, Render and most collectors parse as is. |

### Environment variables

The full list is in `deploy/.env.example` and `deploy/README.md`. The ones a
platform deployment sets:

| Variable | Notes |
| --- | --- |
| `DATABASE_URL` | Secret. See below. |
| `DATABASE_URL_DIRECT` | Only behind a transaction-mode pooler (PgBouncer, Supavisor, Neon's pooler): a direct URL for the one `LISTEN` connection per instance. |
| `RUNBOOK_ORIGIN` | `https://runbooks.example.com`. |
| `RUNBOOK_WEBHOOK_SECRET_KEY` | Secret, `openssl rand -base64 32`. Keep it stable. |
| `RUNBOOK_USERCONTENT_ORIGIN`, `RUNBOOK_UPLOAD_SIGNING_KEY` | Uploads: the second hostname and a secret (`openssl rand -base64 32`). |
| `RUNBOOK_AUTH_SECRET`, `RUNBOOK_AUTH_METHODS`, `RUNBOOK_SMTP_URL`, `RUNBOOK_SMTP_FROM`, `RUNBOOK_GOOGLE_CLIENT_ID`, `RUNBOOK_GOOGLE_CLIENT_SECRET`, `RUNBOOK_AUTH_ALLOWED_DOMAINS` | Sign-in (`docs/install.md`). Google's redirect URI is `{RUNBOOK_ORIGIN}/api/auth/callback/google`. |
| `RUNBOOK_TRUSTED_PROXIES` | The address ranges your platform's proxy connects from, so per-IP sign-in limits see the client (the right-most `X-Forwarded-For` hop that is not a trusted proxy). Empty trusts none; every client then shares the proxy's addresses in those limits. |
| `RUNBOOK_LOG_FORMAT` | `json` on a platform; `text` (the default) for a terminal. |
| `RUNBOOK_DATABASE_POOL_SIZE` | The server's pooled connections per instance (default 16). |
| `RUNBOOK_AUTH_DATABASE_POOL_SIZE` | The sign-in service's connections per instance (default 5). |
| `RUNBOOK_BLOB_STORE` | Blob backend: `postgres` (default), `gcs`, or `s3`. |
| `RUNBOOK_BLOB_BUCKET` | Bucket name (required when `RUNBOOK_BLOB_STORE` is `gcs` or `s3`). |
| `RUNBOOK_BLOB_PREFIX` | Key prefix for objects in bucket (default `runbook/`). |
| `RUNBOOK_S3_ENDPOINT` | Custom S3 endpoint URL (for MinIO or local testing). |
| `RUNBOOK_S3_REGION` | S3 region (e.g. `us-east-1`). |
| `RUNBOOK_S3_FORCE_PATH_STYLE` | `true` for path-style S3 requests (MinIO/LocalStack). |
| `RUNBOOK_SHUTDOWN_TIMEOUT_SECS` | Longest stop after SIGTERM (default 8, 1 to 300). |
| `PORT` | Set by most platforms; otherwise 8080. Followed only with `RUNBOOK_LISTEN_PORT_FROM_ENV=1` (the image sets it); `RUNBOOK_LISTEN_ADDR`, when set, wins over both. |

### Database URLs

Both the server and the sign-in service read `DATABASE_URL` with libpq's
meaning for these keys:

| `sslmode` | Meaning |
| --- | --- |
| `disable` | No TLS. |
| `allow`, `prefer` (default) | TLS when the server offers it; the certificate is not checked. |
| `require` | TLS or no connection; the certificate is not checked unless `sslrootcert` is set (then as `verify-ca`). Not recommended: an attacker on the path can impersonate the database. The server warns at start, for `DATABASE_URL` and `DATABASE_URL_DIRECT` alike. |
| `verify-ca` | TLS; the certificate must chain to `sslrootcert` (or the system's roots). The host name is not checked. |
| `verify-full` | TLS; the chain and the host name are checked. **Use this for a database reached over a network you do not control.** |

- `sslrootcert=/path/ca.pem` names a PEM file of CA certificates that
  replaces the system's roots (mount it from a secret; `sslrootcert=system`
  means the system's roots). Use it when your provider's CA is not a public
  one (Amazon RDS, Cloud SQL's server CA, Azure).
- **Unix sockets** (Cloud SQL on Cloud Run, or a local socket):
  `postgres://USER:PASSWORD@/DB?host=/cloudsql/PROJECT:REGION:INSTANCE`.
  TLS never applies to a socket, so `sslmode` is ignored there.
- Client certificates (`sslcert`, `sslkey`) are not supported.
- URL-encode the password (or use `openssl rand -hex 24`, which needs none).

**Connection budget.** Each instance holds at most
`RUNBOOK_DATABASE_POOL_SIZE` pooled connections, one `LISTEN` connection for
live event streams, and `RUNBOOK_AUTH_DATABASE_POOL_SIZE` for the sign-in
service; at start, one more briefly runs migrations. So for `N` instances:

    N × (RUNBOOK_DATABASE_POOL_SIZE + RUNBOOK_AUTH_DATABASE_POOL_SIZE + 2)

must stay under the database's connection limit, leaving room for the
provider's own connections and for an instance being replaced during a
deploy (count `N + 1`). Small managed tiers allow only a few dozen
connections; with `RUNBOOK_DATABASE_POOL_SIZE=5` and
`RUNBOOK_AUTH_DATABASE_POOL_SIZE=2`, two instances use at most 18, and the
replica test runs with a pool of 3. Requests beyond the pool wait for a
connection (up to 30 s), so a smaller pool slows peaks rather than failing
them.

### Blob storage: Postgres, GCS or S3

By default (`RUNBOOK_BLOB_STORE=postgres`), all blobs (archives, evidence uploads, image thumbnails, snapshots) live directly in PostgreSQL chunks (`runbook.blob_chunk`).

To offload blob bytes to an object bucket (Google Cloud Storage or Amazon S3 / MinIO):
- Set `RUNBOOK_BLOB_STORE=gcs` or `RUNBOOK_BLOB_STORE=s3`.
- Set `RUNBOOK_BLOB_BUCKET` to the bucket name (required).
- Optionally set `RUNBOOK_BLOB_PREFIX` (default: `runbook/`).
- For S3-compatible stores (such as MinIO or LocalStack), set `RUNBOOK_S3_ENDPOINT`, `RUNBOOK_S3_REGION`, and optionally `RUNBOOK_S3_FORCE_PATH_STYLE=true`.

**IAM permissions:**
- **Google Cloud Storage (GCS):** The instance identity (Workload Identity or service account) needs `roles/storage.objectAdmin` on **that bucket only**, with no HMAC keys.
- **Amazon S3:** The instance IAM role requires `s3:GetObject`, `s3:PutObject`, `s3:DeleteObject`, and `s3:ListBucket` scoped to the bucket and prefix.

**Architecture and mixed instances:**
- PostgreSQL remains the authority for ownership, tenancy, and immutability. Each blob has a metadata row in `runbook.blob` with `location` (`'postgres'` or `'object'`), `size`, and `sha256`.
- For existing instances switching from PostgreSQL to GCS or S3, existing blobs stored in PostgreSQL continue to be read from `runbook.blob_chunk` (`location = 'postgres'`), while new blobs are written directly to object storage (`location = 'object'`). Copying historical blobs between stores is out of scope.
- **Orphan sweeper:** An hourly background sweeper running in the drain worker uses an advisory lock (`0x72625f7377656570`) to scan the bucket prefix and clean up orphaned objects older than 1 hour (from cascaded run deletions or interrupted writes) that have no matching row in `runbook.blob`.
- For GKE deployments using the Terraform module (`deploy/terraform/gcp-gke/`), set `blob_store = "gcs"` to automatically provision a private bucket with uniform bucket-level access, public access prevention, and Workload Identity IAM bindings.

### Several instances

Any number of instances can share one database:

- Timers, run activations, claim expiry and webhook notifications are
  claimed under generation-fenced leases in PostgreSQL, so each fires once
  whichever instance claims it; a stopping instance releases the leases it
  has not processed.
- Webhook deliveries are leased per delivery and recorded once per event ID;
  they are at least once (a delivery whose instance dies mid-POST is sent
  again with the same event ID after its 60 s lease).
- Project events, archive writes and retention deletes are arbitrated by row
  locks and event IDs; two instances running the archive job at once do the
  same work once, the second finding it done.
- Rate limits (device-login codes per client address, approval misses) are
  counted in PostgreSQL (`runbook.rate_window`), and the sign-in service
  keeps its limits and sessions in its own schema, so limits hold across
  instances.
- Migrations take a database advisory lock: instances starting together
  apply each migration once; the others wait, then serve.
- Live event streams listen for PostgreSQL notifications, so a run changed
  through one instance updates streams open on another.
- Each instance caches a resolved sign-in session for up to 30 s (D44). A
  sign-out through one instance takes effect at once there and within 30 s
  on the others.

No sticky sessions are needed.

### CPU between requests, and scaling to zero

The background workers (timers, webhook deliveries, archives) run inside the
server process. On a platform that pauses or throttles CPU between requests
(Cloud Run's default "CPU only during requests", or an instance scaled to
zero), that work waits: nothing is lost, because it is all in PostgreSQL and
due work is claimed by due time, but a timer fires, and a webhook is sent,
only once some instance has CPU again. A delivery cut off mid-POST is sent
again after its lease expires. To keep background work on time, keep CPU
allocated and at least one instance running (Cloud Run:
`--no-cpu-throttling --min-instances=1`).

### Updating

Deploy the new image. By default each instance migrates the database at
start (under the advisory lock) before it listens. On Kubernetes, prefer one
`runbook-server migrate` Job from the new image and set
`RUNBOOK_MIGRATE_ON_START=false` on the Deployment: the Job must succeed
first, and each new pod then checks the schema before it can become ready.
Either way, a rolling update is safe as long as each release's migrations
keep the previous release working while both run (UPD1 owns that rule and
the release process).

## Cloud Run walkthrough (untested)

Assumes the `gcloud` CLI, a project with billing, and Docker. Replace the
values in the first block.

```sh
PROJECT=my-project
REGION=us-central1
APP_HOST=runbooks.example.com                 # RUNBOOK_ORIGIN's host
FILES_HOST=files.example-usercontent.com      # the user-content host
VERSION=0.1.0
IMAGE=$REGION-docker.pkg.dev/$PROJECT/runbook/runbook:$VERSION
SA=runbook-run@$PROJECT.iam.gserviceaccount.com
INSTANCE=$PROJECT:$REGION:runbook-db

gcloud config set project "$PROJECT"
gcloud services enable run.googleapis.com sqladmin.googleapis.com \
  secretmanager.googleapis.com artifactregistry.googleapis.com
```

**1. Image.** Cloud Run pulls from Artifact Registry. Build from the
repository (or `docker pull` the release image from ghcr.io and retag it):

```sh
gcloud artifacts repositories create runbook --repository-format=docker --location="$REGION"
gcloud auth configure-docker "$REGION-docker.pkg.dev"
docker build --platform linux/amd64 -t "$IMAGE" .
docker push "$IMAGE"
```

**2. Database.** Cloud SQL for PostgreSQL 16, reached over its Unix socket.
Pick a tier whose connection limit fits the budget above.

```sh
gcloud sql instances create runbook-db --database-version=POSTGRES_16 \
  --edition=ENTERPRISE --tier=db-custom-1-3840 --region="$REGION"
gcloud sql databases create runbook --instance=runbook-db
DB_PASSWORD=$(openssl rand -hex 24)
gcloud sql users create runbook_app --instance=runbook-db --password="$DB_PASSWORD"
```

The server needs a role that can create the `runbook` and `auth` schemas in
that database (Cloud SQL's built-in users can; `deploy/postgres-init/` shows
the narrower setup the compose file uses).

**3. Secrets.**

```sh
printf '%s' "postgres://runbook_app:$DB_PASSWORD@/runbook?host=/cloudsql/$INSTANCE" |
  gcloud secrets create runbook-database-url --data-file=-
unset DB_PASSWORD
openssl rand -base64 32 | tr -d '\n' | gcloud secrets create runbook-webhook-key --data-file=-
openssl rand -base64 32 | tr -d '\n' | gcloud secrets create runbook-upload-key --data-file=-
openssl rand -hex 32 | tr -d '\n' | gcloud secrets create runbook-auth-secret --data-file=-
printf '%s' 'smtps://login-user:CHANGE_ME@smtp.example.com:465' |
  gcloud secrets create runbook-smtp-url --data-file=-
```

**4. Service account.**

```sh
gcloud iam service-accounts create runbook-run
gcloud projects add-iam-policy-binding "$PROJECT" \
  --member="serviceAccount:$SA" --role=roles/cloudsql.client
for secret in runbook-database-url runbook-webhook-key runbook-upload-key \
  runbook-auth-secret runbook-smtp-url; do
  gcloud secrets add-iam-policy-binding "$secret" \
    --member="serviceAccount:$SA" --role=roles/secretmanager.secretAccessor
done
```

**5. Deploy.** CPU always allocated and one warm instance keep timers and
webhooks on time; `--timeout=3600` lets live event streams stay open their
full hour (clients reconnect either way).

```sh
gcloud run deploy runbook --image="$IMAGE" --region="$REGION" \
  --service-account="$SA" --add-cloudsql-instances="$INSTANCE" \
  --port=8080 --cpu=1 --memory=1Gi --concurrency=80 --timeout=3600 \
  --no-cpu-throttling --min-instances=1 --max-instances=3 \
  --set-env-vars="RUNBOOK_ORIGIN=https://$APP_HOST,RUNBOOK_USERCONTENT_ORIGIN=https://$FILES_HOST,RUNBOOK_LOG_FORMAT=json,RUNBOOK_DATABASE_POOL_SIZE=5,RUNBOOK_AUTH_DATABASE_POOL_SIZE=2,RUNBOOK_AUTH_METHODS=email,RUNBOOK_SMTP_FROM=The Runbook <login@example.com>" \
  --set-secrets="DATABASE_URL=runbook-database-url:latest,RUNBOOK_WEBHOOK_SECRET_KEY=runbook-webhook-key:latest,RUNBOOK_UPLOAD_SIGNING_KEY=runbook-upload-key:latest,RUNBOOK_AUTH_SECRET=runbook-auth-secret:latest,RUNBOOK_SMTP_URL=runbook-smtp-url:latest" \
  --startup-probe=httpGet.path=/health/ready,httpGet.port=8080,periodSeconds=2,timeoutSeconds=3,failureThreshold=60 \
  --liveness-probe=httpGet.path=/health/live,httpGet.port=8080,periodSeconds=10,timeoutSeconds=3,failureThreshold=3 \
  --allow-unauthenticated
```

`--allow-unauthenticated` makes the service public at the network level;
The Runbook does its own sign-in. Cloud Run sends SIGTERM and allows 10 s
before SIGKILL; the default 8 s shutdown fits.

**6. Hostnames.** Map both hostnames to the same service, either with
domain mappings (available in some regions) or a global external
Application Load Balancer with a serverless network endpoint group; both
pass `Host` through.

```sh
gcloud beta run domain-mappings create --service=runbook --region="$REGION" --domain="$APP_HOST"
gcloud beta run domain-mappings create --service=runbook --region="$REGION" --domain="$FILES_HOST"
```

Add the DNS records each command prints. To try the service on its
`run.app` URL first, deploy with that URL as `RUNBOOK_ORIGIN` and no
`RUNBOOK_USERCONTENT_ORIGIN` (uploads off).

**7. First owner.** Run `bootstrap` once as a Cloud Run job with the same
image, database and secrets. With `--owner-email` it prints no token, so
nothing secret reaches the job's logs; the owner then signs in by email.

```sh
gcloud run jobs create runbook-bootstrap --image="$IMAGE" --region="$REGION" \
  --service-account="$SA" --set-cloudsql-instances="$INSTANCE" \
  --set-env-vars="RUNBOOK_ORIGIN=https://$APP_HOST" \
  --set-secrets="DATABASE_URL=runbook-database-url:latest,RUNBOOK_WEBHOOK_SECRET_KEY=runbook-webhook-key:latest" \
  --args=bootstrap,--owner-email,you@example.com
gcloud run jobs execute runbook-bootstrap --region="$REGION" --wait
```

**Client addresses.** Cloud Run's front end appends the client to
`X-Forwarded-For`, but the address range it connects to the container from
is not documented as fixed. Until a real deployment confirms it, leave
`RUNBOOK_TRUSTED_PROXIES` empty: the per-IP sign-in limits (10 code requests
an hour per address) then count the front end's addresses, which many
clients share, so a busy install can see `429` on code requests sooner.
(Open item for the first real deploy.)

**Cost note.** `--no-cpu-throttling --min-instances=1` bills one instance
all the time. Without them the service still works, but timers and webhook
deliveries wait for the next request (see above).

## Fly.io (untested)

```toml
# fly.toml
app = "my-runbook"
primary_region = "iad"
kill_signal = "SIGTERM"   # Fly's default SIGINT works too
kill_timeout = 10

[build]
  image = "registry.example.com/runbook:0.1.0"

[env]
  RUNBOOK_ORIGIN = "https://runbooks.example.com"
  RUNBOOK_USERCONTENT_ORIGIN = "https://files.example-usercontent.com"
  RUNBOOK_LOG_FORMAT = "json"
  RUNBOOK_DATABASE_POOL_SIZE = "5"
  RUNBOOK_AUTH_DATABASE_POOL_SIZE = "2"

[http_service]
  internal_port = 8080
  force_https = true
  auto_stop_machines = "off"   # keep background work running
  min_machines_running = 1

  [[http_service.checks]]
    method = "GET"
    path = "/health/ready"
    interval = "10s"
    timeout = "3s"
    grace_period = "30s"
```

Set secrets with `fly secrets set DATABASE_URL=... RUNBOOK_WEBHOOK_SECRET_KEY=...`
and both hostnames with `fly certs add`. Fly's managed Postgres is reached
on its private network; an external provider wants `sslmode=verify-full`.
For Fly's private IPv6 network set `RUNBOOK_LISTEN_ADDR=[::]:8080`.

## Render (untested)

Create a **Web Service** from the image (or the repository's Dockerfile).
Render sets `PORT`; the server follows it. Set the health check path to
`/health/ready`, add the environment variables and secrets above in the
dashboard (or a `render.yaml` with `envVars`), add both custom domains, and
use a paid instance type, since free instances sleep (timers then wait for a
request). Render's managed PostgreSQL gives an internal URL for services in
the same region; on its external URL use `sslmode=verify-full` (with
`sslrootcert=<CA file>` if its certificate does not chain to a public CA).
Render sends SIGTERM and waits 30 s.

## GKE and Cloud SQL with Terraform

For running on Google Cloud using Kubernetes Engine (GKE) and Cloud SQL for
PostgreSQL 16, a production-grade Terraform module is available in
[`deploy/terraform/gcp-gke/`](terraform/gcp-gke/README.md).

It provisions:
- An Autopilot GKE cluster (or deploys to an existing cluster) with Workload Identity;
- Cloud SQL for PostgreSQL 16 on private IP with automated backups, PITR and maintenance windows;
- Secret Manager secrets and Kubernetes Secrets;
- A Deployment with pod disruption budget, probes, security context, and the **Cloud SQL Auth Proxy sidecar**;
- External GKE Ingress with Google-managed SSL certificates (`ManagedCertificate`), HTTP-to-HTTPS redirect (`FrontendConfig`), and health check configuration (`BackendConfig`).

See the [module README](terraform/gcp-gke/README.md) for quick start, variables, DNS setup, and the one-off owner bootstrap job.

## Kubernetes (generic snippet)

For custom or non-GCP Kubernetes clusters, a Deployment with two or more
replicas, a Service, and an Ingress with both hostnames pointing at the same
Service (the ingress controller keeps `Host`):

```yaml
spec:
  replicas: 2
  template:
    spec:
      terminationGracePeriodSeconds: 20
      containers:
        - name: runbook
          image: registry.example.com/runbook:0.1.0
          ports: [{containerPort: 8080}]
          envFrom: [{secretRef: {name: runbook}}, {configMapRef: {name: runbook}}]
          readinessProbe: {httpGet: {path: /health/ready, port: 8080}, periodSeconds: 5}
          livenessProbe: {httpGet: {path: /health/live, port: 8080}, periodSeconds: 10}
          startupProbe: {httpGet: {path: /health/ready, port: 8080}, periodSeconds: 2, failureThreshold: 60}
          # Endpoints update a moment after SIGTERM; keep serving meanwhile.
          lifecycle: {preStop: {exec: {command: ["sleep", "5"]}}}
          securityContext: {readOnlyRootFilesystem: true, runAsNonRoot: true, allowPrivilegeEscalation: false}
          volumeMounts: [{name: tmp, mountPath: /tmp}, {name: home, mountPath: /home/runbook}]
      volumes: [{name: tmp, emptyDir: {}}, {name: home, emptyDir: {}}]
```

Keep `RUNBOOK_SHUTDOWN_TIMEOUT_SECS` plus the `preStop` sleep under
`terminationGracePeriodSeconds`. Set `RUNBOOK_TRUSTED_PROXIES` to the
ingress controller's pod network range.

## ECS, Nomad or a VM

The same contract: one port (the image's `PORT`, or `RUNBOOK_LISTEN_ADDR`), the
environment above, `/health/ready` for the load balancer's target health,
`/health/live` for restarts, and a stop timeout of at least 10 s
(`stopTimeout` on ECS). On a VM, `deploy/docker-compose.external-db.yml` or a
systemd unit running the image is enough.
