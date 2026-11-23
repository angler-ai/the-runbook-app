variable "runner_retention_live_days" {
  type        = string
  default     = "30"
  description = "Runner step detail live window in days, or off to keep live detail."
  validation {
    condition     = var.runner_retention_live_days == "off" || try(can(regex("^[0-9]{1,5}$", var.runner_retention_live_days)) && tonumber(var.runner_retention_live_days) >= 1 && tonumber(var.runner_retention_live_days) <= 36500, false)
    error_message = "runner_retention_live_days must be a whole number from 1 to 36500, or off."
  }
}

variable "runner_retention_archive_days" {
  type        = string
  default     = "off"
  description = "Runner archive window in days, or off to keep archives indefinitely."
  validation {
    condition     = var.runner_retention_archive_days == "off" || try(can(regex("^[0-9]{1,5}$", var.runner_retention_archive_days)) && tonumber(var.runner_retention_archive_days) >= tonumber(var.runner_retention_live_days) && tonumber(var.runner_retention_archive_days) <= 36500, false)
    error_message = "runner_retention_archive_days must be off or a whole number from 1 to 36500 at least as large as runner_retention_live_days (which must be enabled)."
  }
}

resource "google_storage_bucket" "runner_archives" {
  count    = var.runner_enabled ? 1 : 0
  project  = var.project_id
  name     = "${var.name_prefix}${var.project_id}-rb-archives"
  location = local.gcs_bucket_location
  labels   = local.additional_labels

  uniform_bucket_level_access = true
  public_access_prevention    = "enforced"

  versioning {
    enabled = false
  }

  soft_delete_policy {
    retention_duration_seconds = 0
  }

  dynamic "lifecycle_rule" {
    for_each = var.runner_retention_archive_days == "off" ? [] : [tonumber(var.runner_retention_archive_days)]
    content {
      action {
        type = "Delete"
      }
      condition {
        age = lifecycle_rule.value
      }
    }
  }

  depends_on = [google_project_service.apis]
}

resource "google_storage_bucket_iam_member" "runner_archive_api" {
  count  = var.runner_enabled ? 1 : 0
  bucket = google_storage_bucket.runner_archives[0].name
  role   = "roles/storage.objectUser"
  member = "serviceAccount:${google_service_account.runner["api"].account_id}@${var.project_id}.iam.gserviceaccount.com"
}
