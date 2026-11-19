---
title: Quick start on GKE with Terraform
description: Provision GKE, Cloud SQL and an HTTPS load balancer with the Terraform module.
sidebar:
  order: 3
  badge: Docker/Kubernetes
---

The module in `deploy/terraform/gcp-gke/` runs the container edition on
Google Cloud:

- a GKE Autopilot cluster with private nodes, or your existing cluster;
- Cloud SQL for PostgreSQL 16 on a private IP, with backups and
  point-in-time recovery, reached through the Cloud SQL Auth Proxy sidecar
  with Workload Identity;
- secrets in Secret Manager;
- a GKE Ingress with a reserved static IP, Google-managed certificates and
  an HTTP-to-HTTPS redirect;
- blobs in PostgreSQL (`blob_store = "postgres"`, the default) or in a
  private GCS bucket (`blob_store = "gcs"`).

You need Terraform 1.5.0 or newer, an authenticated `gcloud`, and a VPC
network and subnetwork.

## 1. Build and push the image

```sh
REGION="us-central1"
PROJECT="my-gcp-project"
IMAGE="$REGION-docker.pkg.dev/$PROJECT/runbook/server:0.1.0"
gcloud artifacts repositories create runbook --repository-format=docker \
  --location="$REGION" --project="$PROJECT"
gcloud auth configure-docker "$REGION-docker.pkg.dev"
docker build --platform linux/amd64 -t "$IMAGE" .
docker push "$IMAGE"
```

## 2. Set the variables

```sh
cd deploy/terraform/gcp-gke
cp terraform.tfvars.example terraform.tfvars
```

```hcl
project_id         = "my-gcp-project"
region             = "us-central1"
network            = "default"
subnetwork         = "default"
create_cluster     = false
cluster_name       = "my-existing-cluster"
image              = "us-central1-docker.pkg.dev/my-gcp-project/runbook/server:0.1.0"
app_domain         = "runbooks.example.com"
usercontent_domain = "files.example-usercontent.com" # "" if uploads are off
auth_methods       = "email"
smtp_from          = "The Runbook <login@example.com>"
smtp_url           = "smtps://login-user:PASSWORD@smtp.example.com:465"
```

Generated secrets such as the database password end up in the Terraform
state, so keep the state in a private, encrypted bucket:

```sh
terraform init -backend-config="bucket=my-terraform-state-bucket" \
  -backend-config="prefix=runbook/gcp-gke"
```

## 3. Apply

```sh
terraform plan -out=tfplan
terraform apply tfplan
```

When the module creates the cluster (`create_cluster = true`), apply in two
stages the first time, because the ingress manifests need a reachable
cluster to plan:

```sh
terraform apply -target=google_container_cluster.autopilot
terraform apply
```

## 4. Point DNS at the ingress

```sh
terraform output ingress_ip
```

Create `A` records for `app_domain` and `usercontent_domain` pointing at that
address. The managed certificates provision once DNS resolves, usually in 10
to 30 minutes.

## 5. Create the first owner

```sh
POD=$(kubectl get pod -n runbook -l app.kubernetes.io/name=runbook -o jsonpath="{.items[0].metadata.name}")
kubectl exec -it "$POD" -n runbook -c runbook -- runbook-server bootstrap --owner-email admin@example.com
```

The owner then signs in with an email code. The module's
[README](https://github.com/angler-ai/the-runbook-app/blob/main/deploy/terraform/gcp-gke/README.md)
also shows bootstrap as a one-off Job, upgrades and backups.
