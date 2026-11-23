resource "google_service_account" "runbook_gsa" {
  account_id   = local.service_account_ids["runbook-gsa"]
  display_name = "Runbook GKE Service Account"
  project      = var.project_id

  depends_on = [google_project_service.apis]
}

# Cloud SQL client, so the Auth Proxy sidecar can connect: only to this
# module's instance, since the project may hold other teams' databases.
resource "google_project_iam_member" "cloudsql_client" {
  project = var.project_id
  role    = "roles/cloudsql.client"
  member  = "serviceAccount:${google_service_account.runbook_gsa.email}"

  condition {
    title       = "runbook-instance-only"
    description = "Only The Runbook's own Cloud SQL instance"
    expression  = "resource.name == \"projects/${var.project_id}/instances/${google_sql_database_instance.postgres.name}\""
  }

  # A new condition replaces the binding; grant the new one first so the
  # running proxy never loses access.
  lifecycle {
    create_before_destroy = true
  }
}

# Bind GSA to KSA via Workload Identity
resource "google_service_account_iam_member" "workload_identity_user" {
  service_account_id = google_service_account.runbook_gsa.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "serviceAccount:${var.project_id}.svc.id.goog[${local.core_namespace_name}/${local.core_ksa_name}]"
}

resource "google_project_iam_member" "cloudsql_instance_user" {
  count   = var.database_auth == "iam" ? 1 : 0
  project = var.project_id
  role    = "roles/cloudsql.instanceUser"
  member  = "serviceAccount:${google_service_account.runbook_gsa.email}"

  condition {
    title       = "runbook-instance-only"
    description = "Only The Runbook's own Cloud SQL instance"
    expression  = "resource.name == \"projects/${var.project_id}/instances/${google_sql_database_instance.postgres.name}\""
  }
}
