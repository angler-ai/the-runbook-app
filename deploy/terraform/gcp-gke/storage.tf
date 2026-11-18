locals {
  gcs_bucket_name     = var.gcs_bucket_name != "" ? var.gcs_bucket_name : "${var.project_id}-runbook-blobs"
  gcs_bucket_location = var.gcs_bucket_location != "" ? var.gcs_bucket_location : var.region
}

# Optional GCS bucket for blob storage (when blob_store is "gcs")
resource "google_storage_bucket" "blobs" {
  count = var.blob_store == "gcs" ? 1 : 0

  name     = local.gcs_bucket_name
  location = local.gcs_bucket_location
  project  = var.project_id

  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"

  versioning {
    enabled = true
  }

  dynamic "lifecycle_rule" {
    for_each = var.gcs_noncurrent_version_retention_days > 0 ? [1] : []
    content {
      action {
        type = "Delete"
      }
      condition {
        days_since_noncurrent_time = var.gcs_noncurrent_version_retention_days
      }
    }
  }

  depends_on = [google_project_service.apis]
}

# Grant the Runbook GSA objectAdmin on this bucket only
resource "google_storage_bucket_iam_member" "gsa_bucket_admin" {
  count = var.blob_store == "gcs" ? 1 : 0

  bucket = google_storage_bucket.blobs[0].name
  role   = "roles/storage.objectAdmin"
  member = "serviceAccount:${google_service_account.runbook_gsa.email}"
}
