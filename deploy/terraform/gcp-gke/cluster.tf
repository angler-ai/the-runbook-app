data "google_container_cluster" "existing" {
  count = var.create_cluster ? 0 : 1

  name     = var.cluster_name
  location = local.cluster_location
  project  = var.project_id
}

resource "google_container_cluster" "autopilot" {
  count = var.create_cluster ? 1 : 0

  name     = var.cluster_name
  location = local.cluster_location
  project  = var.project_id

  enable_autopilot = true

  network    = var.network
  subnetwork = var.subnetwork

  ip_allocation_policy {}

  private_cluster_config {
    enable_private_nodes    = true
    enable_private_endpoint = false
    master_ipv4_cidr_block  = var.master_ipv4_cidr_block
  }

  depends_on = [
    google_project_service.apis
  ]
}
