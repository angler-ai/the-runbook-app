# Private Service Access peering for Cloud SQL in VPC
resource "google_compute_global_address" "private_ip_address" {
  count = var.create_private_service_access ? 1 : 0

  name          = "${var.sql_instance_name}-private-ip"
  purpose       = "VPC_PEERING"
  address_type  = "INTERNAL"
  prefix_length = 16
  network       = var.network
  project       = var.project_id

  depends_on = [google_project_service.apis]
}

resource "google_service_networking_connection" "private_vpc_connection" {
  count = var.create_private_service_access ? 1 : 0

  network                 = var.network
  service                 = "servicenetworking.googleapis.com"
  reserved_peering_ranges = [google_compute_global_address.private_ip_address[0].name]
}

# Cloud SQL PostgreSQL 16 Instance
resource "google_sql_database_instance" "postgres" {
  name             = var.sql_instance_name
  database_version = "POSTGRES_16"
  region           = var.region
  project          = var.project_id

  deletion_protection = var.sql_deletion_protection

  depends_on = [
    google_project_service.apis,
    google_service_networking_connection.private_vpc_connection
  ]

  settings {
    tier              = var.sql_tier
    edition           = var.sql_edition
    availability_type = var.sql_availability_type

    ip_configuration {
      ipv4_enabled    = false
      private_network = var.network
      ssl_mode        = "ENCRYPTED_ONLY"
    }

    backup_configuration {
      enabled                        = true
      point_in_time_recovery_enabled = true
      start_time                     = var.sql_backup_start_time
      transaction_log_retention_days = 7
    }

    maintenance_window {
      day          = var.sql_maintenance_window_day
      hour         = var.sql_maintenance_window_hour
      update_track = var.sql_maintenance_window_update_track
    }
  }
}

resource "google_sql_database" "runbook" {
  name     = "runbook"
  instance = google_sql_database_instance.postgres.name
  project  = var.project_id
}

resource "random_password" "db_password" {
  length  = 32
  special = false
}

resource "google_sql_user" "runbook_app" {
  name     = "runbook_app"
  instance = google_sql_database_instance.postgres.name
  password = random_password.db_password.result
  project  = var.project_id
}
