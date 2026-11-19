# The Runbook on Google Cloud (GKE & Cloud SQL)

Terraform module to provision and run [The Runbook](https://github.com/angler-ai/the-runbook-app) on Google Cloud using:
- **Google Kubernetes Engine (GKE):** Either an Autopilot cluster with private nodes or an existing GKE cluster.
- **Cloud SQL for PostgreSQL 16:** Private IP only, automated backups with point-in-time recovery (PITR), and maintenance windows.
- **Cloud SQL Auth Proxy Sidecar:** Runs in the pod with `--private-ip`, `--structured-logs`, `--health-check` (probed via `/startup`), and `--max-sigterm-delay=30s`, bound via Workload Identity (least privilege, no service account keys). Note: While Kubernetes 1.28+ supports native sidecars (`initContainers` with `restartPolicy: Always`), the `hashicorp/kubernetes` Terraform provider does not yet expose `restart_policy` in `init_container` schema (hashicorp/terraform-provider-kubernetes#2371). The proxy therefore runs as a standard sidecar with startup probes and `--max-sigterm-delay=30s` to outlast the application on shutdown while `terminationGracePeriodSeconds` is set to 45s.
- **Google Secret Manager & Kubernetes Secrets:** Secret storage for database connection, webhook signing, auth secret, and upload signing keys.
- **GKE Ingress:** External Application Load Balancer with a reserved global static IP, Google-managed SSL certificates (`ManagedCertificate`), HTTP-to-HTTPS redirect (`FrontendConfig`), and `/health/ready` readiness checks (`BackendConfig`).

- **Blob Storage Options:** Blobs (evidence, archives) can live either directly in PostgreSQL (`blob_store = "postgres"`, the default) or in a dedicated private Google Cloud Storage bucket (`blob_store = "gcs"`).

---

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

1. **Terraform:** Version 1.5.0 or newer.
2. **Google Cloud SDK (`gcloud`):** Authenticated with permissions to manage GKE, Cloud SQL, Compute, IAM, and Secret Manager.
3. **Google Cloud APIs:** Managed automatically by the module when `enable_apis = true` (default):
   - `container.googleapis.com` (GKE)
   - `sqladmin.googleapis.com` (Cloud SQL)
   - `secretmanager.googleapis.com` (Secret Manager)
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

image              = "ghcr.io/angler-ai/runbook:0.1.0"
app_domain         = "runbooks.example.com"
usercontent_domain = "files.example-usercontent.com" # Optional; leave empty "" if uploads are disabled

auth_methods       = "email"
auth_allowed_domains = "example.com,getangler.ai"
smtp_from          = "The Runbook <login@example.com>"
smtp_url           = "smtps://login-user:PASSWORD@smtp.example.com:465"
```

### 2. Configure Remote State (Recommended)

To store Terraform state securely in a Google Cloud Storage bucket, uncomment the `backend "gcs"` block in `versions.tf` or initialize with backend configuration:

```sh
terraform init -backend-config="bucket=my-terraform-state-bucket" -backend-config="prefix=runbook/gcp-gke"
```

> **Security Note:** Generated secrets (such as the database password, auth secret, and webhook key) reside in the Terraform state file. Ensure your state bucket is private, encrypted with Customer-Managed or Google-Managed Encryption Keys, and restricted to authorized CI/CD or admin roles.

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
          image: ghcr.io/angler-ai/runbook:0.1.0
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
IMAGE="$REGION-docker.pkg.dev/$PROJECT/$REPO/server:0.1.0"

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
- **Google sign-in (optional):** create an OAuth web client in the Google Cloud console with the redirect URI `https://<app_domain>/api/auth/callback/google`, then set `google_client_id` and `google_client_secret` and add `google` to `auth_methods` (`"google"` alone turns email codes off). The secret goes to Secret Manager and the workload's Secret; the ID goes to the ConfigMap. Pass the secret from the environment (`TF_VAR_google_client_secret`), not a tfvars file.

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
