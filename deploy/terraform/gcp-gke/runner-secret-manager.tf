# Grant access on the explicitly named secrets, never at the project level.
resource "google_secret_manager_secret_iam_member" "runner_vault_access" {
  for_each = var.runner_enabled ? toset([for entry in var.runner_secret_manager_secret_ids : basename(entry.resource)]) : toset([])

  project   = var.project_id
  secret_id = each.value
  role      = "roles/secretmanager.secretAccessor"
  member    = "serviceAccount:${google_service_account.runner["vault"].email}"

  depends_on = [google_project_service.apis]
}

# Earlier versions automatically managed this service's audit configuration.
# Leave that policy in place when removing the legacy resource from state.
removed {
  from = google_project_iam_audit_config.runner_secret_manager

  lifecycle {
    destroy = false
  }
}

resource "google_project_iam_audit_config" "runner_secret_manager_opt_in" {
  count = var.manage_secret_manager_audit_config ? 1 : 0

  project = var.project_id
  service = "secretmanager.googleapis.com"

  audit_log_config {
    log_type = "DATA_READ"
  }

  audit_log_config {
    log_type = "DATA_WRITE"
  }
}

output "runner_secret_manager_resources" {
  description = "JSON value of RUNNER_SECRET_MANAGER_RESOURCES for the vault process configuration."
  value = jsonencode(var.runner_enabled ? [
    for entry in var.runner_secret_manager_secret_ids : merge(
      { resource = entry.resource, types = sort(tolist(entry.types)), destinations = sort(tolist(entry.destinations)) },
      entry.version == null ? {} : { version = entry.version }
    )
  ] : [])
}
