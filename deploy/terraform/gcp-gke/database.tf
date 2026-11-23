# Private Service Access peering for Cloud SQL in VPC
resource "google_compute_global_address" "private_ip_address" {
  count = var.create_private_service_access ? 1 : 0

  name          = "${local.sql_instance_name}-private-ip"
  labels        = local.additional_labels
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
  name             = local.sql_instance_name
  database_version = "POSTGRES_16"
  region           = var.region
  project          = var.project_id

  deletion_protection = var.sql_deletion_protection

  depends_on = [
    google_project_service.apis,
    google_service_networking_connection.private_vpc_connection
  ]

  lifecycle {
    precondition {
      condition     = !var.runner_enabled || local.runner_db_pool_size >= 1
      error_message = "Runner SQL requires space for the core pools and rollout surge, three runner pools, bootstrap, and maintenance. Set runner_sql_max_connections for an unlisted tier, reduce core replicas or pools, or use a larger SQL tier."
    }
  }

  settings {
    tier                        = var.sql_tier
    edition                     = var.sql_edition
    availability_type           = var.sql_availability_type
    deletion_protection_enabled = var.sql_api_deletion_protection
    retain_backups_on_delete    = var.sql_retain_backups_on_delete
    user_labels                 = local.additional_labels
    connector_enforcement       = "REQUIRED"

    final_backup_config {
      enabled        = var.sql_final_backup
      retention_days = var.sql_final_backup ? var.sql_final_backup_retention_days : null
    }

    dynamic "database_flags" {
      for_each = merge(
        { log_connections = "on", log_disconnections = "on" },
        var.runner_enabled && var.runner_sql_max_connections != null ? { max_connections = tostring(local.runner_sql_max_connections) } : {},
        (var.database_auth == "iam" || var.runner_enabled) ? { "cloudsql.iam_authentication" = "on" } : {},
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
  name            = local.core_database_name
  deletion_policy = local.sql_child_deletion_policy
  instance        = google_sql_database_instance.postgres.name
  project         = var.project_id
}

resource "random_password" "db_password" {
  count   = local.manage_db_password ? 1 : 0
  length  = 32
  special = false
}

resource "google_sql_user" "runbook_app" {
  count           = local.manage_db_password ? 1 : 0
  name            = local.password_database_user
  instance        = google_sql_database_instance.postgres.name
  password        = random_password.db_password[0].result
  deletion_policy = local.sql_child_deletion_policy
  project         = var.project_id
}

locals {
  core_database_name = "${var.name_prefix}runbook"
  # A private disposable instance deletes these children itself. The SQL API
  # cannot delete a child after ownership or privileges have been assigned.
  sql_child_deletion_policy = var.name_prefix != "" && !var.sql_deletion_protection && !var.sql_api_deletion_protection ? "ABANDON" : null

  manage_db_password     = var.manage_secret_values && var.database_auth == "password"
  password_database_user = "${var.name_prefix}runbook_app"
  database_user          = var.database_auth == "iam" ? trimsuffix(google_service_account.runbook_gsa.email, ".gserviceaccount.com") : local.password_database_user
  proxy_auth_args        = var.database_auth == "iam" ? ["--auto-iam-authn"] : []
}

resource "google_sql_user" "runbook_iam" {
  count           = var.database_auth == "iam" ? 1 : 0
  name            = local.database_user
  type            = "CLOUD_IAM_SERVICE_ACCOUNT"
  deletion_policy = local.sql_child_deletion_policy
  instance        = google_sql_database_instance.postgres.name
  project         = var.project_id
}

moved {
  from = random_password.db_password
  to   = random_password.db_password[0]
}

moved {
  from = google_sql_user.runbook_app
  to   = google_sql_user.runbook_app[0]
}
