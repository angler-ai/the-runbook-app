output "ingress_ip" {
  description = "The reserved external IPv4 address assigned to the GKE Ingress load balancer. Point DNS A records for app_domain (and usercontent_domain) to this IP."
  value       = one(google_compute_global_address.ingress_ip[*].address)
}

output "app_hostname" {
  description = "The primary application hostname."
  value       = var.app_domain
}

output "usercontent_hostname" {
  description = "The user-content uploads hostname (empty if not enabled)."
  value       = var.usercontent_domain
}

output "cloud_sql_connection_name" {
  description = "The Cloud SQL instance connection name (PROJECT:REGION:INSTANCE)."
  value       = google_sql_database_instance.postgres.connection_name
}

output "service_account_email" {
  description = "The Google Service Account email used by Workload Identity."
  value       = google_service_account.runbook_gsa.email
}

output "database_name" {
  description = "The PostgreSQL database name for The Runbook."
  value       = google_sql_database.runbook.name
}

output "migration_job_name" {
  description = "The one-shot Kubernetes Job that migrates the database for the configured image before the Deployment rolls."
  value       = local.migration_job_name
}

output "database_user" {
  description = "The PostgreSQL username used by The Runbook."
  value       = local.database_user
}

output "cluster_name" {
  description = "The GKE cluster name."
  value       = local.cluster_name
}

output "cluster_location" {
  description = "The GKE cluster location (region or zone)."
  value       = local.cluster_location
}

output "blob_bucket_name" {
  description = "The GCS bucket name used for blob storage (empty if blob_store is 'postgres')."
  value       = var.blob_store == "gcs" ? google_storage_bucket.blobs[0].name : ""
}

