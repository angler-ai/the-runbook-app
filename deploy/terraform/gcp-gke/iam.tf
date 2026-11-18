resource "google_service_account" "runbook_gsa" {
  account_id   = "runbook-gsa"
  display_name = "Runbook GKE Service Account"
  project      = var.project_id

  depends_on = [google_project_service.apis]
}

# Grant Cloud SQL client permission so the Cloud SQL Auth Proxy sidecar can connect
resource "google_project_iam_member" "cloudsql_client" {
  project = var.project_id
  role    = "roles/cloudsql.client"
  member  = "serviceAccount:${google_service_account.runbook_gsa.email}"
}

# Bind GSA to KSA via Workload Identity
resource "google_service_account_iam_member" "workload_identity_user" {
  service_account_id = google_service_account.runbook_gsa.name
  role               = "roles/iam.workloadIdentityUser"
  member             = "serviceAccount:${var.project_id}.svc.id.goog[runbook/runbook-ksa]"
}
