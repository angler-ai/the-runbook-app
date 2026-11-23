# The Runbook on Google Cloud (GKE & Cloud SQL)

Terraform module to provision and run [The Runbook](https://github.com/angler-ai/the-runbook-app) on Google Cloud using:
- **Google Kubernetes Engine (GKE):** Either an Autopilot cluster with private nodes or an existing GKE cluster.
- **Cloud SQL for PostgreSQL 16:** Private IP only, automated backups with point-in-time recovery (PITR), and maintenance windows.
- **Cloud SQL Auth Proxy Sidecar:** Runs in the pod with `--private-ip`, `--structured-logs`, `--health-check` (probed via `/startup`), and `--max-sigterm-delay=30s`, bound via Workload Identity (least privilege, no service account keys). Note: While Kubernetes 1.28+ supports native sidecars (`initContainers` with `restartPolicy: Always`), the `hashicorp/kubernetes` Terraform provider does not yet expose `restart_policy` in `init_container` schema (hashicorp/terraform-provider-kubernetes#2371). The proxy therefore runs as a standard sidecar with startup probes and `--max-sigterm-delay=30s` to outlast the application on shutdown while `terminationGracePeriodSeconds` is set to 45s.
- **Kubernetes Secrets:** Credentials for the application and the migration Job.
- **GKE Ingress:** External Application Load Balancer with a reserved global static IP, Google-managed SSL certificates (`ManagedCertificate`), HTTP-to-HTTPS redirect (`FrontendConfig`), and `/health/ready` readiness checks (`BackendConfig`).

- **Blob Storage Options:** Blobs (evidence, archives) can live either directly in PostgreSQL (`blob_store = "postgres"`, the default) or in a dedicated private Google Cloud Storage bucket (`blob_store = "gcs"`).

---

## Terraform state and credentials

**With `manage_secret_values = true` (default), state and saved plans contain
secret values**, including the database password and URL, the auth, webhook
and upload keys, and any configured OAuth, SMTP or Slack credentials. Use a
dedicated state bucket readable only by the deployer and explicitly named
administrators. Enable uniform bucket-level access and public access
prevention. Inherited project roles can still grant access. Use a dedicated
restricted project or an IAM deny policy with explicit exceptions where
needed. Treat plan files, old object generations, and state backups as
credentials. Pass managed input secrets through protected CI environment
variables.

Set `manage_secret_values = false` to keep these values out of new Terraform
state and plans. Create the namespace first (target `kubernetes_namespace.runbook`
after the cluster exists), then create an Opaque Secret named by
`existing_secret_name` (default `runbook-secrets`) in namespace `runbook`
outside Terraform. Both serving and migration pods consume it. Supply these keys:

| Key | Value |
|---|---|
| `DATABASE_URL` | Required. `postgres://runbook_app:<url-encoded-password>@127.0.0.1:5432/runbook?sslmode=disable` for password auth. Provision the application role and password outside Terraform and grant schema access for migrations. For IAM, use `postgres://runbook-gsa%40<project-id>@127.0.0.1:5432/runbook?sslmode=disable`. The proxy handles remote TLS. |
| `RUNBOOK_AUTH_SECRET` | Required. 32 random bytes encoded as hex. |
| `RUNBOOK_WEBHOOK_SECRET_KEY` | 32 random bytes encoded as standard base64. Needed for webhooks and server signing keys. |
| `RUNBOOK_UPLOAD_SIGNING_KEY` | 32 random bytes encoded as standard base64 when `usercontent_domain` is set. |
| `RUNBOOK_GOOGLE_CLIENT_SECRET` | Required for Google sign-in. Pass `google_client_id` separately to Terraform. |
| `RUNBOOK_SMTP_URL` | Required for email sign-in and for SMTP notifications. Include URL-encoded relay credentials and TLS settings. |
| `RUNBOOK_SLACK_BOT_TOKEN`, `RUNBOOK_SLACK_SIGNING_SECRET` | Both required when Slack is enabled. |
| `RUNBOOK_WEBHOOK_PREVIOUS_SECRET_KEYS` | Optional comma-separated standard-base64 keys for opening existing sealed values on versions that support a previous-key list. |

Leave `google_client_secret`, `smtp_url`, `slack_bot_token`, and
`slack_signing_secret` empty in Terraform in external mode. Use protected
secret provisioning rather than putting literals in shell history. Restart
the serving Deployment after every external Secret update, then verify
rollout and readiness. Environment variables are read at pod start.

### Upgrade and handoff

For a normal upgrade in managed password mode, state moves preserve the
existing managed Secret, DB user and generated secrets. The refreshed proxy
and pod settings roll the Deployment and create a new migration Job. The Job
name tracks immutable template inputs, including the proxy image, database
auth mode and Secret name.

The module forgets the old Secret Manager secrets and their versions with
`removed` blocks and `destroy = false`. Review the plan for state-only
removals, then have an authorized operator deliberately delete the unused
copies after checking for any external consumers. Previous state versions
still contain their values.

For external mode, first move to the restricted backend and pause deployments.
With the old configuration still in effect, keep the existing Kubernetes
Secret and SQL role in place. Hand ownership of the Secret and, in password
mode, the SQL user to the operator with `terraform state rm` for
`kubernetes_secret.runbook_secrets` and `google_sql_user.runbook_app` in
the old state. Use the `[0]` addresses if already upgraded, and prepend the
module address (such as `module.runbook.`) as applicable. If using a new
Secret name, pre-create it with the current values. Then set
`manage_secret_values = false` and the selected name, remove secret inputs
and inspect the plan. Reject any deletion of a live Secret or SQL role.
The random resources can drop out of state. Rotate credentials after the
handoff and restrict or retire historical state copies.

### Rotate credentials

Use a maintenance window for changes that cannot overlap across replicas.
Take a database backup and secure any recovery keys beforehand.

- **Database password.** In managed mode, plan with
  `-replace='random_password.db_password[0]'` (prefix the module address if
  needed). In external mode change the SQL role password and update
  `DATABASE_URL` together. Old pods may fail on new connections until they
  roll. IAM database auth uses short-lived tokens.
- **Auth secret.** Replace `random_id.auth_secret[0]` in managed mode or
  generate a new `RUNBOOK_AUTH_SECRET` in external mode. Expect active
  sessions to require fresh sign-in.
- **Webhook sealing key.** Move the current key to
  `RUNBOOK_WEBHOOK_PREVIOUS_SECRET_KEYS` in external mode before rotating,
  and deploy a server version that supports it. Switch every replica to the
  new primary key. Rotate each subscription's signing secret and update
  receivers. Where server signing keys use the same sealer, rotate those
  keys too and let retired keys expire. Remove the previous key after all
  still-needed sealed records and delivery overlap have aged out or been
  rotated. If the old key is exposed, rotate the signing secrets themselves.
  Older servers without a previous-key list require coordinated recreation
  of affected subscriptions and a signing-key recovery plan before replacing
  the sealer.
- **Upload key.** Replace `random_id.upload_signing_key[0]` or generate a
  new `RUNBOOK_UPLOAD_SIGNING_KEY`. Existing signed links expire on
  rotation. Issue fresh links as needed.
- **Google OAuth.** Create a new client secret, set
  `TF_VAR_google_client_secret` or the external Secret key, verify a Google
  sign-in through the new pods, then revoke the old secret.
- **SMTP.** Issue new relay credentials. Set `TF_VAR_smtp_url` or update
  `RUNBOOK_SMTP_URL`, verify end-to-end mail and revoke the old credentials.
- **Slack.** Regenerate the signing secret or reissue the bot token in
  Slack, update the corresponding Terraform input or external key, and test
  delivery and approval callbacks. Revoke superseded credentials.

## Cloud SQL and network upgrades

SQL API deletion protection defaults to on, independently of Terraform's
`sql_deletion_protection`. Backups are retained on deletion and a final
backup is kept for 30 days. Connector enforcement rejects direct database
connections. Convert administrative tools and every other client to the
Auth Proxy or a supported connector before applying. Connection and
disconnection logs are on. `sql_enable_pgaudit` defaults to false. When
enabled, configure the extension and audit classes separately.

Expect these instance settings to update in place. Flags can restart
PostgreSQL and interrupt connections. Before a live apply, take and verify a
named backup, review the maintenance window, and reject any plan that
replaces the instance or database.

### IAM database authentication

Keep `database_auth = "password"` for the existing credentials. For IAM
mode, use a maintenance window and stage the new login before the Job or
serving pods are updated:

1. Back up the database, keep the current pods and the built-in SQL role,
   and verify administrative access via a connector. For a password-to-IAM
   migration, remove only the built-in SQL user address from Terraform
   state as described above. Preserve that PostgreSQL role and its
   ownership. Discard any full plan produced before this handoff.
2. Set `database_auth = "iam"` and apply only the IAM SQL user
   (`google_sql_user.runbook_iam[0]`) and its instance login grant
   (`google_project_iam_member.cloudsql_instance_user[0]`), with any module
   prefix. This creates the IAM user and updates the instance flags through
   dependencies. For a fresh IAM install, also target
   `google_sql_database.runbook` and provision the cluster in its usual
   first stage.
3. In PostgreSQL, grant the IAM identity `CONNECT` and the privileges
   needed to run migrations. For an existing install, preserve object
   ownership and grant membership in the existing owner role, for example
   `GRANT runbook_app TO "runbook-gsa@my-project";`. For a fresh install,
   grant `CREATE` on the `runbook` database so the IAM user can
   create the `runbook` and `auth` schemas. Verify any pre-created schemas
   also grant it `USAGE, CREATE`. Test from the serving identity via an IAM-enabled proxy
   before rolling it out.
4. For external Secret mode, stage the IAM URL in a **new** Secret and set
   `existing_secret_name` to it so old password-mode pods can still start
   during the rollout. Plan and apply the full change. Verify the
   migration Job, both hostnames and application database operations.
   After all old pods drain, disable the built-in role's login and
   retire its password. Keep the role if it owns objects or supplies
   inherited permissions. Rollback requires a working password login and
   accounting for any objects created by new migrations.

### NetworkPolicy and the proxy image

`network_policies` defaults to true. On a cluster enforcing NetworkPolicy,
it isolates the namespace from inbound pod and VPC traffic. Global external
GKE Ingress uses network endpoint groups and the two Google Front End and
health-check IPv4 ranges on TCP 8080. No proxy-only subnet is used. The
migration Job has no incoming peers. Kubelet probes and pod-local proxy
connections still work. Move internal clients to the public HTTPS hostname
or add a scoped allow policy before applying. Check both hostnames and pod
readiness after applying. The load balancer sends HSTS with
`hsts_max_age = 31536000` and `hsts_include_subdomains = true` by default.

`restrict_egress` defaults to false and applies to serving and migration
pods when `network_policies` is enabled. To restrict egress, set `cloud_sql_private_cidr` to the SQL private IP as a
`/32` or its private-services CIDR (TCP 3307). The policy permits cluster
DNS and GKE metadata endpoints and public IPv4 destinations, including
Google APIs (HTTPS), OAuth, Slack, public webhooks and public SMTP relays.
For private endpoints, egress gateways, custom DNS or IPv6, add scoped
egress rules for the resolver or destination and port before enabling it.
In particular, allow any private Google API endpoints and any explicitly
configured private webhook relay. Test DNS, proxy startup, login, mail,
uploads and webhooks. The policy does not impose egress restrictions on
the runner namespace.

`cloud_sql_proxy_image` defaults to the **Cloud SQL Auth Proxy 2.28.0**
multi-platform digest. To keep both workload images in your private
registry, mirror the proxy by digest with `crane copy`, verify the mirror
digest, and set `cloud_sql_proxy_image` to the mirrored
`registry/repository/cloud-sql-proxy@sha256:…` reference.

## Architecture & Layout

```
                        [ Internet Traffic ]
                                 │
                   [ Reserved Global Static IP ]
                                 │
                   [ GKE Ingress (External ALB) ]
                    Managed SSL (App & Files Host)
                                 │
                    [ GKE Service (ClusterIP) ]
                                 │
                ┌────────────────┴────────────────┐
                │        Pod (Replicas >= 2)      │
                │                                 │
                │  ┌───────────────────────────┐  │
                │  │ App Container             │  │
                │  │  - Port 8080              │  │
                │  │  - Probes on /health/*    │  │
                │  └─────────────┬─────────────┘  │
                │                │ 127.0.0.1:5432 │
                │  ┌─────────────┴─────────────┐  │
                │  │ Cloud SQL Auth Proxy      │  │
                │  │  - Workload Identity (GSA)│  │
                │  └─────────────┬─────────────┘  │
                └────────────────┼────────────────┘
                                 │ (Private VPC Peering)
                                 ▼
                     [ Cloud SQL PostgreSQL 16 ]
```

---

## Prerequisites

1. **Terraform:** Version 1.9 or newer. The Google and Google Beta providers require 7.20 or newer in the 7.x series.
2. **Google Cloud SDK (`gcloud`):** Authenticated with permissions to manage GKE, Cloud SQL, Compute, and IAM.
3. **Google Cloud APIs:** Managed automatically by the module when `enable_apis = true` (default):
   - `container.googleapis.com` (GKE)
   - `sqladmin.googleapis.com` (Cloud SQL)
   - `servicenetworking.googleapis.com` (VPC Peering for Private IP)
   - `compute.googleapis.com` (Networking and Load Balancing)
4. **VPC Network:** A VPC network and subnetwork. If `create_private_service_access = true` (default), the module will allocate an internal IP range and configure Private Service Access peering for Cloud SQL.

---

## Quick Start

### 1. Configure Variables

Copy the example variables file:

```sh
cp terraform.tfvars.example terraform.tfvars
```

Edit `terraform.tfvars` with your settings:

```hcl
project_id         = "my-gcp-project"
region             = "us-central1"
network            = "default"
subnetwork         = "default"

# Set create_cluster = true to create a new GKE Autopilot cluster,
# or false to deploy into an existing cluster:
create_cluster     = false
cluster_name       = "my-existing-cluster"

image              = "ghcr.io/angler-ai/runbook:0.1.1"
app_domain         = "runbooks.example.com"
usercontent_domain = "files.example-usercontent.com" # Optional; leave empty "" if uploads are disabled

auth_methods       = "email"
auth_allowed_domains = "example.com,getangler.ai"
smtp_from          = "The Runbook <login@example.com>"

# Pass TF_VAR_smtp_url separately in managed mode.
```

### 2. Configure Remote State (Recommended)

To store Terraform state securely in a Google Cloud Storage bucket, uncomment the `backend "gcs"` block in `versions.tf` or initialize with backend configuration:

```sh
terraform init -backend-config="bucket=my-terraform-state-bucket" -backend-config="prefix=runbook/gcp-gke"
```

See [Terraform state and credentials](#terraform-state-and-credentials) before the first plan.

### 3. Deploy

```sh
terraform init
terraform plan -out=tfplan
terraform apply tfplan
```

**When this module creates the cluster (`create_cluster = true`), apply in two stages the first time.** The GKE BackendConfig, FrontendConfig and ManagedCertificate are `kubernetes_manifest` resources, and Terraform needs a reachable cluster to plan them. So a single first apply fails before the cluster exists. Create the cluster first, then apply everything:

```sh
terraform apply -target=google_container_cluster.autopilot
terraform apply
```

With an existing cluster (`create_cluster = false`, the default), one apply is enough.

### 4. Configure DNS

After `apply` completes, retrieve the external IP address:

```sh
terraform output ingress_ip
```

Create DNS `A` records with your DNS provider:
- `app_domain` (e.g. `runbooks.example.com`) -> `<ingress_ip>`
- `usercontent_domain` (e.g. `files.example-usercontent.com`) -> `<ingress_ip>`

Google-managed SSL certificates (`ManagedCertificate`) will provision automatically once DNS points to the ingress IP (typically 10–30 minutes).

---

## First-Run Bootstrap (Initial Owner)

The Runbook requires an initial owner account. Run the `bootstrap` command as a one-off Kubernetes Job.

Create `bootstrap-job.yaml`:

```yaml
apiVersion: batch/v1
kind: Job
metadata:
  name: runbook-bootstrap
  namespace: runbook
spec:
  template:
    spec:
      serviceAccountName: runbook-ksa
      restartPolicy: Never
      containers:
        - name: bootstrap
          image: ghcr.io/angler-ai/runbook:0.1.1
          args: ["bootstrap", "--owner-email", "admin@example.com"]
          envFrom:
            - secretRef:
                name: runbook-secrets
            - configMapRef:
                name: runbook-config
        - name: cloud-sql-proxy
          image: gcr.io/cloud-sql-connectors/cloud-sql-proxy:2.14.2
          args:
            - "--private-ip"
            - "--structured-logs"
            - "--port=5432"
            - "PROJECT_ID:REGION:runbook-db"
```

Apply and inspect the output:

```sh
kubectl apply -f bootstrap-job.yaml
kubectl wait --for=condition=complete job/runbook-bootstrap -n runbook --timeout=120s
kubectl logs job/runbook-bootstrap -n runbook -c bootstrap
kubectl delete -f bootstrap-job.yaml
```

Alternatively, run `exec` in a running pod:

```sh
POD=$(kubectl get pod -n runbook -l app.kubernetes.io/name=runbook -o jsonpath="{.items[0].metadata.name}")
kubectl exec -it "$POD" -n runbook -c runbook -- runbook-server bootstrap --owner-email admin@example.com
```

With `--owner-email`, no secret token is printed in the logs; the owner signs in directly via email code.

---

## Building and Pushing the Image

Build the container image using the root `Dockerfile` and push to Google Artifact Registry:

```sh
REGION="us-central1"
PROJECT="my-gcp-project"
REPO="runbook"
IMAGE="$REGION-docker.pkg.dev/$PROJECT/$REPO/server:0.1.1"

# Create Artifact Registry repository if not exists:
gcloud artifacts repositories create "$REPO" \
  --repository-format=docker \
  --location="$REGION" \
  --project="$PROJECT"

# Configure docker authentication:
gcloud auth configure-docker "$REGION-docker.pkg.dev"

# Build and push multi-platform or linux/amd64:
docker build --platform linux/amd64 -t "$IMAGE" .
docker push "$IMAGE"
```

---

## Authentication Configuration

The sign-in service (`auth/`) is built into the image and runs automatically as a supervised loopback child process inside the container.

- **Email codes (SMTP):** Configured via `smtp_url` and `smtp_from`. Users enter their email and receive a one-time code.
- **Allowed Domains:** Use `auth_allowed_domains` (e.g. `example.com,getangler.ai`) to restrict registrations and sign-ins to verified corporate domains.
- **Google sign-in (optional):** create an OAuth web client in the Google Cloud console with the redirect URI `https://<app_domain>/api/auth/callback/google`, then set `google_client_id` and `google_client_secret` and add `google` to `auth_methods` (`"google"` alone turns email codes off). The secret goes to the workload's Secret and the ID goes to the ConfigMap. In external mode, supply only the ID to Terraform. Pass the secret from the environment (`TF_VAR_google_client_secret`), not a tfvars file.

---

## Slack

`slack_bot_token` and `slack_signing_secret` (both sensitive; pass them as `TF_VAR_…`) turn on The Runbook's Slack app: approval requests in a project's channel with a button per option. `slack_default_channel` is optional. Setup: [`docs/slack.md`](../../../docs/slack.md).

---

## TLS policy and Cloud Armor

- `ssl_policy`: the name of a Compute Engine SSL policy (say TLS 1.2 minimum with the `MODERN` profile) that the load balancer uses, attached through the FrontendConfig.
- `cloud_armor_policy`: the name of a Cloud Armor security policy attached to the backend.

Both are created outside this module, so one policy can serve several apps.

---

## Blob Storage: PostgreSQL vs. Google Cloud Storage

The module supports two modes for storing blob objects (evidence attachments and historical daily run archives):

### 1. PostgreSQL (`blob_store = "postgres"`, Default)
- All evidence uploads and archives are stored directly within PostgreSQL.
- Ideal for simpler operational architecture: no object storage bucket to manage, and backups are unified in Cloud SQL.
- Sets `RUNBOOK_BLOB_STORE=postgres` in the application ConfigMap.

### 2. Google Cloud Storage (`blob_store = "gcs"`)
- Provisions a dedicated, private GCS bucket configured with:
  - Uniform bucket-level access enabled;
  - Public access prevention enforced;
  - Object versioning enabled;
  - Optional noncurrent version lifecycle retention (`gcs_noncurrent_version_retention_days`).
- Workload Identity grants `roles/storage.objectAdmin` to the Runbook Google Service Account **on that bucket only** (least privilege, no keys).
- Sets `RUNBOOK_BLOB_STORE=gcs`, `RUNBOOK_BLOB_BUCKET=<bucket-name>`, and `RUNBOOK_BLOB_PREFIX=runbook/` in the application ConfigMap.

---

## Upgrades and rollback

The Runbook uses forward-only, checksummed, idempotent database migrations.
In this module the serving pods do not migrate on start
(`RUNBOOK_MIGRATE_ON_START=false`). Instead, each pinned image gets a
`runbook-migrate-<hash>` Job that runs `runbook-server migrate`; Terraform
waits for that Job to succeed before it updates the Deployment, and the new
pods check the schema before they can become ready.

Pin `image` to an exact release, such as
`ghcr.io/angler-ai/runbook:0.2.0`, optionally with its `@sha256:` digest.
Terraform refuses `latest` and other moving tags.

Before an upgrade:

1. Read the target release notes. Check its rollback floor with the new
   image: `docker run --rm <new-image> --version`. After this upgrade, code
   older than that floor is not safe on the migrated database.
2. For a multi-replica rolling upgrade, first set
   `allow_newer_schema = true` while `image` still names the current
   release and run `terraform apply`. This lets an old pod restart safely
   after the new Job has expanded the schema.
3. Take and await an explicit pre-upgrade backup. The argument is a label
   (stored as the backup's description); Cloud SQL assigns the numeric id,
   which the script prints once the backup has succeeded:

   ```sh
   BACKUP_ID=$(./pre-upgrade-backup.sh pre-upgrade-v0-2-0)
   echo "$BACKUP_ID"
   ```

   The operator running the script needs permission to create and read
   Cloud SQL backup runs. Automated daily backups and PITR remain enabled;
   this named backup is the exact boundary for this upgrade.

Then set the new `image`, set `allow_newer_schema = false`, review a plan
that creates the new migration Job before changing the Deployment, and apply:

```sh
terraform plan -out=tfplan
terraform apply tfplan
kubectl logs "job/$(terraform output -raw migration_job_name)" -n runbook -c runbook-migrate
kubectl rollout status deployment/runbook -n runbook
```

If the Job exhausts its retries, the Deployment does not roll. Fix the cause,
delete that failed Job by the name above, and apply again; migrations are
idempotent, so rerunning the Job is safe.

To roll back to a release at or above the rollback floor, set that release's
exact image and `allow_newer_schema = true`, then apply. Its migration Job
verifies and accepts the newer additive schema, and its pods do the same.
Take another named backup first if you need a restore point for the rollback
itself.

Below the rollback floor, changing the image is not enough and
`allow_newer_schema` must not be used to force it. Stop writes, restore the
pre-upgrade on-demand backup (or a PITR point before migration), accepting
that data written after that point is lost, then set the old image with
`allow_newer_schema = false` and apply. Keep the pre-upgrade backup until the
upgrade is accepted.

---

## Database Permissions

The built-in user `runbook_app` is created by Terraform with a randomized password. On first boot, the server creates the `runbook` and `auth` schemas. Cloud SQL built-in users possess `CONNECT` and `CREATE` privileges on the instance databases by default.

If using a pre-existing database with restricted default permissions, ensure `runbook_app` has schema creation permissions:

```sql
GRANT CONNECT, TEMPORARY, CREATE ON DATABASE runbook TO runbook_app;
```

---

## Backups and Restoration

- **Automated Backups:** Enabled daily at `sql_backup_start_time` (default `03:00` UTC).
- **Point-in-Time Recovery (PITR):** Transaction logs are retained for 7 days, allowing restoration to any specific second.
- **Restoration:** Performed via `gcloud sql backups restore` or the Google Cloud Console.

---

## Teardown

To protect against catastrophic data loss, `sql_deletion_protection` is set to `true` by default.

To destroy the deployment:
1. In `terraform.tfvars`, set `sql_deletion_protection = false`.
2. Run `terraform apply` to disable deletion protection on the database.
3. Run `terraform destroy`.

## Runner infrastructure

See [Run the runner on GKE](https://docs.therunbook.app/runner/gke/) for
runner image digests, one-time registration and database preparation,
process secrets, and vault identity enrollment. The guide's shared database
connection budget covers the 25-connection test tier and the production
`db-custom-1-3840` tier.

## Isolated temporary installs

Use separate state and a dedicated network and subnet for each install. Set
`name_prefix` when creating the install. This prefixes module-owned names,
including the SQL databases, service accounts, and Kubernetes namespaces.
Names provided for existing resources, standard DNS record names, container
names, and port names keep their existing meaning. IAM account IDs longer
than 30 characters keep the prefix and receive a deterministic hash suffix.
`labels` adds labels wherever the resource supports them. Built-in Kubernetes
labels retain precedence so selectors and admission still work. KMS key
rings, Google service accounts, Cloud SQL databases and users, and DNS
records have no label field.

For a disposable instance, example overrides are:

```hcl
create_cluster               = true
name_prefix                  = "runbook-test-"
labels                       = { runbook-test = "true" }
release_channel              = "REGULAR"
deletion_protection          = false
sql_deletion_protection      = false
sql_api_deletion_protection  = false
sql_final_backup              = false
sql_retain_backups_on_delete = false
kms_prevent_destroy          = false
ingress_enabled              = false
```

Leaving `release_channel` unset preserves GKE's automatic channel selection,
normally Regular for Autopilot. Both SQL deletion protections and cluster
deletion protection default to enabled. Cloud SQL retains backups on
deletion and takes a final backup with `sql_final_backup_retention_days = 30`
by default.

With `ingress_enabled = false`, the module omits the public static IP,
Ingress, BackendConfig, FrontendConfig, and managed certificate. Private
Service Access can still reserve an internal address range for Cloud SQL.
Use the core's ClusterIP Service address from inside the cluster or
`kubectl -n runbook-test-runbook port-forward svc/runbook-test-runbook 8080:8080`.
In this mode the core and runner use the internal HTTP Service origin.
Existing ingress state addresses are moved automatically by Terraform.

OAuth MCP discovery and browser callbacks require HTTPS. The module warns
when a runner install uses the internal HTTP origin. The production default
uses managed-certificate ingress and sets the core origin to
`https://${app_domain}`. Wait until the certificate is active before connecting
OAuth credentials.

For a private test, add a TLS reverse-proxy sidecar to the core pod and
expose its HTTPS port through the existing ClusterIP Service. Use a private
test CA and a hostname on the certificate that is resolvable by the browser
and the runner pods. Route that hostname to the sidecar from inside the
cluster and use a local port forward or other private access path for the
browser. Install the test CA in the browser and mount it in runner pods
that connect over TLS (set `NODE_EXTRA_CA_CERTS` to its PEM file). Set
`RUNBOOK_ORIGIN` on core and `CORE_URL` on the runner processes to that HTTPS
origin so the assertion issuer, discovery and callback URLs agree. The vault
can keep `RUNNER_VAULT_CORE_URL` on the internal HTTP Service for grant calls.
Retain the core's private NetworkPolicy allowance for the test TLS port and
configure the mock OAuth provider for the HTTPS callback
`/v1/runner/oauth/callback/<credential-id>`.

Terraform requires literal lifecycle values. The protected KMS key stays
at `google_kms_crypto_key.runner_vault[0]`. A new install with
`kms_prevent_destroy = false` uses `runner_vault_temporary[0]` at a separate
address. The protected runner database also keeps its existing address,
while a disposable SQL instance uses `runner_temporary[0]`. Select these
settings before the first apply. Changing them on an existing install
would change the resource address. No manual state migration is required
for default installs. When destroying a temporary key, verify that its
versions are scheduled for destruction and automatic rotation is disabled.
Cloud KMS retains the key and key ring records.

Temporary image repositories can supply digest-pinned images with
`<name_prefix>runbook-runner` and `<name_prefix>runbook-sandbox` basenames.
Use a distinct prefixed Secret Manager secret for each process and pass its
numeric version. The module supplies the database names to the migration Job and the
sandbox account, pod name prefix and labels to the controller. Use the
prefixed database names in the one-time SQL preparation.

For a prefixed install with both SQL deletion protections disabled, Terraform
abandons the child databases and users during destroy. The SQL instance
deletion removes them together with their grants and ownership. If you
disable only the runner inside a disposable install, its database and users
stay on the instance. Google can retain the Private Service Access
connection and its underlying network and allocation for four days after
Cloud SQL deletion.
