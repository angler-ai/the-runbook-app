resource "google_sql_database" "runner" {
  count    = var.runner_enabled && var.sql_deletion_protection ? 1 : 0
  name     = local.runner_database_name
  project  = var.project_id
  instance = google_sql_database_instance.postgres.name
  lifecycle {
    prevent_destroy = true
  }
}

# Keep the existing protected address for installations that use the defaults.
resource "google_sql_database" "runner_temporary" {
  count           = var.runner_enabled && !var.sql_deletion_protection ? 1 : 0
  deletion_policy = local.sql_child_deletion_policy
  name            = local.runner_database_name
  project         = var.project_id
  instance        = google_sql_database_instance.postgres.name
}

locals {
  runner_database_name = "${var.name_prefix}runbook_runner"
  runner_db_users = { for role, account in google_service_account.runner :
    role => trimsuffix(account.email, ".gserviceaccount.com")
  }
  runner_sql_max_connections = var.runner_sql_max_connections != null ? var.runner_sql_max_connections : lookup({
    db-f1-micro      = 25
    db-g1-small      = 50
    db-custom-1-3840 = 100
  }, var.sql_tier, 0)
  # Each pod also has a dedicated LISTEN socket. Include Kubernetes's
  # default 25% rollout surge (rounded up) and leave space for maintenance.
  runner_core_connections = (var.replicas + ceil(var.replicas / 4)) * (var.database_pool_size + var.auth_database_pool_size + 1)
  runner_sql_headroom     = local.runner_sql_max_connections <= 25 ? 4 - local.runner_archive_connections : 5
  # Archive uploads hold the leader session without occupying the API pool.
  runner_archive_connections = var.runner_retention_live_days == "off" ? 0 : 1
  # The migration's control connection and its advisory-lock connection.
  runner_bootstrap_connections = 2
  # Give the IAM logins a spare connection each on larger tiers (operator
  # probes and the vault enrollment command can overlap the running pools).
  runner_login_spare = local.runner_sql_max_connections > 25 ? 1 : 0
  runner_db_pool_size = min(var.runner_database_pool_size, floor((
    local.runner_sql_max_connections - local.runner_core_connections - local.runner_sql_headroom -
    local.runner_bootstrap_connections - local.runner_archive_connections - 2 * local.runner_login_spare
  ) / 3))
  # PostgreSQL counts CONNECTION LIMIT against the login, even after SET ROLE.
  runner_db_limits = {
    api       = 2 * local.runner_db_pool_size + local.runner_archive_connections + local.runner_login_spare
    vault     = local.runner_db_pool_size + local.runner_login_spare
    bootstrap = local.runner_bootstrap_connections
  }
  runner_sql_grants = { for entry in setproduct(local.runner_google_identities, ["cloudsql.client", "cloudsql.instanceUser"]) :
    join("/", entry) => { identity = entry[0], role = "roles/${entry[1]}" }
  }
}

resource "google_sql_user" "runner" {
  for_each        = local.runner_db_users
  name            = each.value
  type            = "CLOUD_IAM_SERVICE_ACCOUNT"
  deletion_policy = local.sql_child_deletion_policy
  instance        = google_sql_database_instance.postgres.name
  project         = var.project_id
}

resource "google_project_iam_member" "runner_sql" {
  for_each = local.runner_sql_grants
  project  = var.project_id
  role     = each.value.role
  member   = "serviceAccount:${google_service_account.runner[each.value.identity].email}"
  condition {
    title      = "runbook-runner-instance-only"
    expression = "resource.name == \"projects/${var.project_id}/instances/${google_sql_database_instance.postgres.name}\""
  }
}

resource "kubernetes_manifest" "runner_database_migration" {
  # Kubernetes adds the controller UID and Job name to the pod template labels.
  computed_fields = [
    "metadata.annotations",
    "metadata.labels",
    "spec.template.metadata.labels",
  ]

  count = var.runner_enabled && var.runner_database_bootstrap_ready ? 1 : 0
  depends_on = [
    google_sql_database.runner, google_sql_database.runner_temporary, google_sql_user.runner, google_project_iam_member.runner_sql,
    google_service_account_iam_member.runner_workload_identity, kubernetes_network_policy_v1.runner,
    google_dns_record_set.runner_googleapis_alias
  ]
  manifest = {
    apiVersion = "batch/v1"
    kind       = "Job"
    metadata = merge({
      name      = "${var.name_prefix}runner-migration-${substr(sha256(jsonencode(concat([var.runner_image, var.runner_install.role_prefix, local.runner_proxy.image, local.core_database_name, local.runner_database_name, var.runner_install.group_id, var.runner_install.runner_id, var.runner_worker_capacity, local.runner_db_limits, local.runner_sql_max_connections, local.runner_core_connections, local.runner_proxy_flags], length(var.labels) == 0 ? [] : [var.labels], length(local.runner_google_api_host_aliases) == 0 ? [] : [local.runner_google_api_host_aliases]))), 0, 16)}"
      namespace = local.runner_namespace
    }, length(var.labels) == 0 ? {} : { labels = var.labels })
    spec = {
      backoffLimit          = 2
      activeDeadlineSeconds = 300
      template = {
        metadata = { labels = merge(var.labels, { "runbook.dev/process" = "bootstrap" }) }
        spec = merge({
          serviceAccountName           = kubernetes_service_account_v1.runner["bootstrap"].metadata[0].name
          automountServiceAccountToken = false
          restartPolicy                = "Never"
          enableServiceLinks           = false
          securityContext              = { seccompProfile = { type = "RuntimeDefault" } }
          initContainers               = [merge(local.runner_proxy, { restartPolicy = "Always" })]
          containers = [{
            name            = "migrate"
            image           = var.runner_image
            command         = ["node", "/app/bin/gke-migrate.mjs"]
            securityContext = merge(local.runner_security, { runAsUser = 10001, runAsGroup = 10001 })
            resources = {
              requests = { cpu = "250m", memory = "512Mi", "ephemeral-storage" = "1Gi" }
              limits   = { cpu = "500m", memory = "1Gi", "ephemeral-storage" = "1Gi" }
            }
            env = [
              { name = "RUNNER_MIGRATE_DATABASE_URL", value = local.runner_database_urls.bootstrap },
              { name = "RUNNER_DB_CORE_NAME", value = local.core_database_name },
              { name = "RUNNER_DB_NAME", value = local.runner_database_name },
              { name = "RUNNER_DB_ROLE_PREFIX", value = var.runner_install.role_prefix },
              { name = "RUNNER_DB_API_LOGIN", value = local.runner_db_users.api },
              { name = "RUNNER_DB_VAULT_LOGIN", value = local.runner_db_users.vault },
              { name = "RUNNER_DB_CONNECTION_LIMITS", value = jsonencode(local.runner_db_limits) },
              { name = "RUNNER_DB_CORE_CONNECTIONS", value = tostring(local.runner_core_connections) },
              { name = "RUNNER_INSTALL_GROUP_ID", value = var.runner_install.group_id },
              { name = "RUNNER_INSTALL_RUNNER_ID", value = var.runner_install.runner_id },
              { name = "RUNNER_INSTALL_WORKER_CAPACITY", value = tostring(var.runner_worker_capacity) },
            ]
          }]
        }, local.runner_google_api_host_aliases)
      }
    }
  }
  wait {
    condition {
      type   = "Complete"
      status = "True"
    }
  }
}

output "runner_database_logins" {
  description = "IAM login names for the runner database and one-time SQL setup."
  value       = local.runner_db_users
}

output "runner_database_connection_sql" {
  description = "Apply as a SQL administrator before enabling or resizing runner workloads. The migration Job verifies the effective limits."
  value = var.runner_enabled ? join("\n", concat(["BEGIN;"], [
    for role, login in local.runner_db_users :
    format("ALTER ROLE \"%s\" CONNECTION LIMIT %d; ALTER ROLE \"%s\" SET idle_in_transaction_session_timeout = '15s';", login, local.runner_db_limits[role], login)
  ], ["COMMIT;"])) : null
}
