locals {
  runner_google_identities = var.runner_enabled ? toset(["api", "vault", "bootstrap"]) : toset([])
  runner_processes         = var.runner_enabled ? toset(["api", "worker", "sandbox-ctl", "vault", "gateway"]) : toset([])
}

resource "google_kms_key_ring" "runner" {
  count = var.runner_enabled ? 1 : 0

  project  = var.project_id
  location = var.region
  name     = "${var.name_prefix}runbook-runner"

  depends_on = [google_project_service.apis]
}

resource "google_kms_crypto_key" "runner_vault" {
  count = var.runner_enabled && var.kms_prevent_destroy ? 1 : 0

  key_ring = google_kms_key_ring.runner[0].id
  name     = "${var.name_prefix}vault"
  labels   = local.additional_labels
  purpose  = "ENCRYPT_DECRYPT"
  # New wraps use the newest version; older versions stay enabled to unwrap
  # existing data keys (the vault's re-wrap job moves them forward).
  rotation_period = var.runner_kek_rotation_period

  lifecycle {
    prevent_destroy = true
  }
}

# KMS cannot delete keys or rings. Deleting this Terraform resource schedules
# destruction of its key versions. The protected resource keeps its address.
resource "google_kms_crypto_key" "runner_vault_temporary" {
  count = var.runner_enabled && !var.kms_prevent_destroy ? 1 : 0

  key_ring        = google_kms_key_ring.runner[0].id
  name            = "${var.name_prefix}vault"
  labels          = local.additional_labels
  purpose         = "ENCRYPT_DECRYPT"
  rotation_period = var.runner_kek_rotation_period
}

locals {
  runner_vault_key_id = one(concat(google_kms_crypto_key.runner_vault[*].id, google_kms_crypto_key.runner_vault_temporary[*].id))

  service_account_names = {
    for name in concat(["runbook-gsa"], [for role in local.runner_google_identities : "runbook-runner-${role}"]) :
    name => "${var.name_prefix}${name}"
  }
  # Keep the prefix, and hash the full name if it exceeds IAM's 30-character limit.
  service_account_ids = {
    for name, full in local.service_account_names :
    name => length(full) <= 30 ? full : "${substr(full, 0, 21)}-${substr(sha256(full), 0, 8)}"
  }
}

resource "google_service_account" "runner" {
  for_each = local.runner_google_identities

  project      = var.project_id
  account_id   = local.service_account_ids["runbook-runner-${each.key}"]
  display_name = "Runbook Runner ${each.key}"
}

resource "google_kms_crypto_key_iam_member" "runner_vault" {
  count = var.runner_enabled ? 1 : 0

  crypto_key_id = local.runner_vault_key_id
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
  value       = local.runner_vault_key_id
}
