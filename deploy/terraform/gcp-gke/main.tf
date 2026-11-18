provider "google" {
  project = var.project_id
  region  = var.region
}

provider "google-beta" {
  project = var.project_id
  region  = var.region
}

data "google_client_config" "default" {}

locals {
  cluster_location = var.cluster_location != "" ? var.cluster_location : var.region
  cluster_endpoint = var.create_cluster ? google_container_cluster.autopilot[0].endpoint : data.google_container_cluster.existing[0].endpoint
  cluster_ca_cert  = base64decode(var.create_cluster ? google_container_cluster.autopilot[0].master_auth[0].cluster_ca_certificate : data.google_container_cluster.existing[0].master_auth[0].cluster_ca_certificate)
}

provider "kubernetes" {
  host                   = "https://${local.cluster_endpoint}"
  token                  = data.google_client_config.default.access_token
  cluster_ca_certificate = local.cluster_ca_cert
}
