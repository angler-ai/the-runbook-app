locals {
  required_services = toset(concat([
    "container.googleapis.com",
    "sqladmin.googleapis.com",
    "servicenetworking.googleapis.com",
    "compute.googleapis.com",
    "storage.googleapis.com",
  ], var.runner_enabled ? ["cloudkms.googleapis.com", "aiplatform.googleapis.com"] : []))
}

resource "google_project_service" "apis" {
  for_each = var.enable_apis ? local.required_services : toset([])

  project                    = var.project_id
  service                    = each.key
  disable_on_destroy         = false
  disable_dependent_services = false
}
