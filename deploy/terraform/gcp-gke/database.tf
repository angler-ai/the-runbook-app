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
    tier                        = var.sql_tier
    edition                     = var.sql_edition
    availability_type           = var.sql_availability_type
    deletion_protection_enabled = var.sql_api_deletion_protection
    retain_backups_on_delete    = true
    connector_enforcement       = "REQUIRED"

    final_backup_config {
      enabled        = true
      retention_days = 30
    }

    dynamic "database_flags" {
      for_each = merge(
        { log_connections = "on", log_disconnections = "on" },
        var.database_auth == "iam" ? { "cloudsql.iam_authentication" = "on" } : {},
        var.sql_enable_pgaudit ? { "cloudsql.enable_pgaudit" = "on" } : {}
      )
      content {
        name  = database_flags.key
        value = database_flags.value
      }
    }

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
  count   = local.manage_db_password ? 1 : 0
  length  = 32
  special = false
}

resource "google_sql_user" "runbook_app" {
  count    = local.manage_db_password ? 1 : 0
  name     = "runbook_app"
  instance = google_sql_database_instance.postgres.name
  password = random_password.db_password[0].result
  project  = var.project_id
}

locals {
  manage_db_password = var.manage_secret_values && var.database_auth == "password"
  database_user      = var.database_auth == "iam" ? trimsuffix(google_service_account.runbook_gsa.email, ".gserviceaccount.com") : "runbook_app"
  proxy_auth_args    = var.database_auth == "iam" ? ["--auto-iam-authn"] : []
}

resource "google_sql_user" "runbook_iam" {
  count    = var.database_auth == "iam" ? 1 : 0
  name     = local.database_user
  type     = "CLOUD_IAM_SERVICE_ACCOUNT"
  instance = google_sql_database_instance.postgres.name
  project  = var.project_id
}

moved {
  from = random_password.db_password
  to   = random_password.db_password[0]
}

moved {
  from = google_sql_user.runbook_app
  to   = google_sql_user.runbook_app[0]
}
