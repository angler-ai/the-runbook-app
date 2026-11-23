data "google_container_cluster" "existing" {
  count = var.create_cluster ? 0 : 1

  name     = local.cluster_name
  location = local.cluster_location
  project  = var.project_id
}

resource "google_container_cluster" "autopilot" {
  count = var.create_cluster ? 1 : 0

  name     = local.cluster_name
  location = local.cluster_location
  project  = var.project_id

  deletion_protection = var.deletion_protection
  resource_labels     = local.additional_labels

  dynamic "release_channel" {
    for_each = var.release_channel == null ? [] : [var.release_channel]
    content {
      channel = release_channel.value
    }
  }

  enable_autopilot = true
  # An explicit channel selects its current default. runner_setup checks the
  # resulting version before enabling runner workloads.
  min_master_version = var.runner_enabled && var.release_channel == null ? "1.32" : null

  network    = var.network
  subnetwork = var.subnetwork

  ip_allocation_policy {}

  dynamic "secret_manager_config" {
    for_each = var.runner_enabled ? [1] : []
    content {
      enabled = true
    }
  }

  private_cluster_config {
    enable_private_nodes    = true
    enable_private_endpoint = false
    master_ipv4_cidr_block  = var.master_ipv4_cidr_block
  }

  depends_on = [
    google_project_service.apis
  ]
}
