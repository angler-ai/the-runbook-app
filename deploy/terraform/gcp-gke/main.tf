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
  additional_labels   = length(var.labels) == 0 ? null : var.labels
  cluster_name        = "${var.create_cluster ? var.name_prefix : ""}${var.cluster_name}"
  sql_instance_name   = "${var.name_prefix}${var.sql_instance_name}"
  core_namespace_name = "${var.name_prefix}runbook"
  core_service_name   = "${var.name_prefix}runbook"
  core_ksa_name       = "${var.name_prefix}runbook-ksa"
  core_service_url    = "http://${local.core_service_name}.${local.core_namespace_name}.svc.cluster.local:8080"
  core_origin         = var.ingress_enabled ? "https://${var.app_domain}" : local.core_service_url

  cluster_location = var.cluster_location != "" ? var.cluster_location : var.region
  cluster_endpoint = var.create_cluster ? google_container_cluster.autopilot[0].endpoint : data.google_container_cluster.existing[0].endpoint
  cluster_ca_cert  = base64decode(var.create_cluster ? google_container_cluster.autopilot[0].master_auth[0].cluster_ca_certificate : data.google_container_cluster.existing[0].master_auth[0].cluster_ca_certificate)
}

provider "kubernetes" {
  host                   = "https://${local.cluster_endpoint}"
  token                  = data.google_client_config.default.access_token
  cluster_ca_certificate = local.cluster_ca_cert
}
