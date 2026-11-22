locals {
  runner_processes = var.runner_enabled ? toset(["api", "worker", "sandbox-ctl", "vault", "gateway"]) : toset([])
}

resource "google_kms_key_ring" "runner" {
  count = var.runner_enabled ? 1 : 0

  project  = var.project_id
  location = var.region
  name     = "runbook-runner"

  depends_on = [google_project_service.apis]
}

resource "google_kms_crypto_key" "runner_vault" {
  count = var.runner_enabled ? 1 : 0

  key_ring = google_kms_key_ring.runner[0].id
  name     = "vault"
  purpose  = "ENCRYPT_DECRYPT"
  # New wraps use the newest version; older versions stay enabled to unwrap
  # existing data keys (the vault's re-wrap job moves them forward).
  rotation_period = var.runner_kek_rotation_period

  lifecycle {
    prevent_destroy = true
  }
}

resource "google_service_account" "runner" {
  for_each = local.runner_processes

  project      = var.project_id
  account_id   = "runbook-runner-${each.key}"
  display_name = "Runbook Runner ${each.key}"
}

resource "google_kms_crypto_key_iam_member" "runner_vault" {
  count = var.runner_enabled ? 1 : 0

  crypto_key_id = google_kms_crypto_key.runner_vault[0].id
  role          = "roles/cloudkms.cryptoKeyEncrypterDecrypter"
  member        = "serviceAccount:${google_service_account.runner["vault"].email}"
}

# Off until attached-identity model use exists in the runner (review X19):
# a project-wide Vertex role the vault doesn't use is only exposure.
resource "google_project_iam_member" "runner_vault_vertex" {
  count = var.runner_enabled && var.runner_vault_vertex_enabled ? 1 : 0

  project = var.project_id
  role    = "roles/aiplatform.user"
  member  = "serviceAccount:${google_service_account.runner["vault"].email}"

  depends_on = [google_project_service.apis]
}

output "runner_vault_kms_key" {
  description = "Fully qualified KEK name for RUNNER_KEK_GCP_KEY, when runner IAM and KMS are enabled."
  value       = one(google_kms_crypto_key.runner_vault[*].id)
}

# TODO(R2): Create runner workloads and Workload Identity bindings after the
# Kubernetes sandbox backend is available. No runner Pods or Docker sockets
# are installed into GKE by this switch.
