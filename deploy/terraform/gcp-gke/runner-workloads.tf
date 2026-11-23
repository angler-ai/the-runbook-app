resource "kubernetes_namespace_v1" "runner" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    name = local.runner_namespace_name
    labels = merge(var.labels, {
      "app.kubernetes.io/managed-by"       = "terraform"
      "pod-security.kubernetes.io/enforce" = "restricted"
    })
  }
}

locals {
  runner_namespace_name = "${var.name_prefix}runbook-runner"
  runner_namespace      = try(kubernetes_namespace_v1.runner[0].metadata[0].name, null)
  runner_services = {
    api         = { public = 8090, peer = 8094 }
    worker      = { peer = 8094 }
    sandbox-ctl = { peer = 8093 }
    vault       = { peer = 8091 }
    gateway     = { peer = 8092 }
  }
  runner_callers = {
    api         = ["worker", "gateway", "sandbox-ctl"]
    worker      = ["api"]
    sandbox-ctl = ["worker"]
    vault       = ["api", "worker", "sandbox-ctl"]
    gateway     = ["vault", "worker", "sandbox-ctl"]
  }
  runner_callees = { for role in keys(local.runner_services) :
    role => [for target, callers in local.runner_callers : target if contains(callers, role)]
  }
  runner_url_names = {
    api = "API_URL", worker = "WORKER_URL", sandbox-ctl = "SANDBOX_CTL_URL", vault = "VAULT_URL", gateway = "GATEWAY_URL"
  }
  runner_api_url        = "http://${var.name_prefix}runner-api.${local.runner_namespace_name}.svc.cluster.local:8090"
  runner_vault_core_url = local.core_service_url
  runner_peer_urls = { for role, ports in local.runner_services :
    role => "http://${var.name_prefix}runner-${role}.${local.runner_namespace_name}.svc.cluster.local:${ports.peer}"
  }
  runner_core_env = var.ingress_enabled ? {} : { RUNNER_INSECURE_CORE_HTTP = "true" }
  runner_common_env = {
    CORE_URL                      = local.core_origin
    RUNNER_INSECURE_INTERNAL_HTTP = "true"
    RUNNER_SANDBOX_BACKEND        = "kubernetes"
    RUNNER_INSTALL_ID             = try(var.runner_install.id, "")
    RUNNER_ALLOWED_RUNNER_IDS     = try(var.runner_install.runner_id, "")
    RUNNER_ORG_ID                 = try(var.runner_install.org_id, "")
  }
  runner_database_urls = { for role, user in local.runner_db_users :
    role => "postgresql://${urlencode(user)}@127.0.0.1:5432/${local.runner_database_name}?sslmode=disable"
  }
  runner_role_env = {
    api = {
      RUNNER_INTERNAL_LISTEN         = "0.0.0.0:8094"
      RUNNER_GKE_WORKER_URL          = local.runner_peer_urls.worker
      RUNNER_INSTALL_WORKER_CAPACITY = tostring(var.runner_worker_capacity)
      RUNNER_DATABASE_URL            = try(local.runner_database_urls.api, "")
      RUNNER_SCHEDULER_DATABASE_URL  = try(local.runner_database_urls.api, "")
      RUNNER_DB_POOL_SIZE            = tostring(local.runner_db_pool_size)
      RUNNER_DB_ROLE_PREFIX          = try(var.runner_install.role_prefix, "")
      RUNNER_SANDBOX_IMAGE           = var.runner_sandbox_image
      RUNNER_SANDBOX_APPROVED_IMAGES = join(",", local.sandbox_images)
    }
    worker = {
      RUNNER_ID            = try(var.runner_install.runner_id, "")
      RUNNER_GROUP_ID      = try(var.runner_install.group_id, "")
      RUNNER_CAPACITY      = tostring(var.runner_worker_capacity)
      RUNNER_SANDBOX_IMAGE = var.runner_sandbox_image
    }
    sandbox-ctl = merge({
      RUNNER_SANDBOX_IMAGE     = var.runner_sandbox_image
      RUNNER_SANDBOX_NAMESPACE = local.sandbox_namespace
      }, var.name_prefix == "" ? {} : {
      RUNNER_SANDBOX_NAME_PREFIX     = var.name_prefix
      RUNNER_SANDBOX_SERVICE_ACCOUNT = local.sandbox_account_name
    }, length(var.labels) == 0 ? {} : { RUNNER_SANDBOX_LABELS = jsonencode(var.labels) })
    vault = {
      RUNNER_VAULT_CORE_URL = local.runner_vault_core_url
      RUNNER_DATABASE_URL   = try(local.runner_database_urls.vault, "")
      RUNNER_DB_POOL_SIZE   = tostring(local.runner_db_pool_size)
      RUNNER_DB_ROLE_PREFIX = try(var.runner_install.role_prefix, "")
      RUNNER_KEK_PROVIDER   = "gcp-kms"
      RUNNER_KEK_GCP_KEY    = local.runner_vault_key_id == null ? "" : local.runner_vault_key_id
    }
    gateway = {}
  }
  runner_env = { for role in keys(local.runner_services) :
    role => merge(local.runner_common_env, local.runner_role_env[role], local.runner_core_env, {
      RUNNER_ROLE            = role
      RUNNER_SECRET_ENV_FILE = "/runbook/secrets/process.json"
      }, {
      for target in local.runner_callees[role] : local.runner_url_names[target] => local.runner_peer_urls[target]
    })
  }
  runner_security = {
    runAsNonRoot = true, allowPrivilegeEscalation = false, readOnlyRootFilesystem = true,
    capabilities = { drop = ["ALL"] }
  }
  runner_proxy = {
    name  = "cloud-sql-proxy"
    image = var.cloud_sql_proxy_image
    args = [
      "--private-ip", "--auto-iam-authn", "--structured-logs", "--port=5432",
      "--health-check", "--http-address=0.0.0.0", google_sql_database_instance.postgres.connection_name
    ]
    securityContext = local.runner_security
    startupProbe = {
      httpGet = { path = "/startup", port = 9090 }, periodSeconds = 2, failureThreshold = 40
    }
    resources = {
      requests = { cpu = "100m", memory = "128Mi", "ephemeral-storage" = "1Gi" }
      limits   = { cpu = "200m", memory = "256Mi", "ephemeral-storage" = "1Gi" }
    }
  }
}

resource "kubernetes_service_account_v1" "runner" {
  for_each = setunion(local.runner_processes, var.runner_enabled ? toset(["bootstrap"]) : toset([]))
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}${each.key}"
    namespace = local.runner_namespace
    annotations = contains(local.runner_google_identities, each.key) ? {
      "iam.gke.io/gcp-service-account" = google_service_account.runner[each.key].email
    } : {}
  }
  automount_service_account_token = each.key == "sandbox-ctl"
}

resource "google_service_account_iam_member" "runner_workload_identity" {
  for_each           = local.runner_google_identities
  service_account_id = google_service_account.runner[each.key].name
  role               = "roles/iam.workloadIdentityUser"
  member             = "serviceAccount:${var.project_id}.svc.id.goog[${local.runner_namespace}/${var.name_prefix}${each.key}]"
}

data "google_project" "runner" {
  count      = var.runner_enabled ? 1 : 0
  project_id = var.project_id
}

resource "google_secret_manager_secret_iam_member" "runner" {
  for_each  = var.runner_enabled ? var.runner_secret_versions : {}
  secret_id = split("/versions/", each.value)[0]
  role      = "roles/secretmanager.secretAccessor"
  # The CSI provider impersonates the GSA on annotated KSAs. Unannotated
  # KSAs use their federated identity only for the mounted secret.
  member = contains(local.runner_google_identities, each.key) ? "serviceAccount:${google_service_account.runner[each.key].email}" : "principal://iam.googleapis.com/projects/${data.google_project.runner[0].number}/locations/global/workloadIdentityPools/${var.project_id}.svc.id.goog/subject/ns/${local.runner_namespace}/sa/${var.name_prefix}${each.key}"
}

resource "kubernetes_manifest" "runner_secrets" {
  for_each = var.runner_enabled ? var.runner_secret_versions : {}
  manifest = {
    apiVersion = "secrets-store.csi.x-k8s.io/v1"
    kind       = "SecretProviderClass"
    metadata   = merge({ name = "${var.name_prefix}${each.key}", namespace = local.runner_namespace }, length(var.labels) == 0 ? {} : { labels = var.labels })
    spec = {
      provider = "gke"
      parameters = {
        secrets = yamlencode([{ resourceName = var.runner_secret_versions[each.key], path = "process.json" }])
      }
    }
  }
}

resource "kubernetes_manifest" "runner" {
  for_each = var.runner_database_bootstrap_ready ? local.runner_processes : toset([])
  depends_on = [
    kubernetes_manifest.runner_database_migration,
    kubernetes_manifest.sandbox_policy_binding,
    kubernetes_role_binding_v1.sandbox_ctl,
    kubernetes_resource_quota_v1.sandboxes,
    kubernetes_limit_range_v1.sandbox,
    terraform_data.runner_setup,
    kubernetes_network_policy_v1.runner_default_deny,
    kubernetes_network_policy_v1.runner,
    kubernetes_network_policy_v1.sandboxes,
    google_secret_manager_secret_iam_member.runner,
    google_service_account_iam_member.runner_workload_identity,
  ]
  manifest = {
    apiVersion = "apps/v1"
    kind       = "Deployment"
    metadata   = merge({ name = "${var.name_prefix}runner-${each.key}", namespace = local.runner_namespace }, length(var.labels) == 0 ? {} : { labels = var.labels })
    spec = {
      replicas = 1
      # Both the gateway and sandbox-ctl keep in-memory ownership state.
      strategy = { type = "Recreate" }
      selector = { matchLabels = { "runbook.dev/process" = each.key } }
      template = {
        metadata = {
          labels      = merge(var.labels, { "runbook.dev/process" = each.key })
          annotations = { "runbook.dev/secret-version" = var.runner_secret_versions[each.key] }
        }
        spec = {
          serviceAccountName           = kubernetes_service_account_v1.runner[each.key].metadata[0].name
          automountServiceAccountToken = each.key == "sandbox-ctl"
          enableServiceLinks           = false
          securityContext              = { seccompProfile = { type = "RuntimeDefault" }, fsGroup = 10001 }
          containers = concat([{
            name  = each.key
            image = var.runner_image
            env = concat(
              [for name, value in local.runner_env[each.key] : { name = name, value = value }],
              each.key == "gateway" ? [{ name = "GATEWAY_SANDBOX_HOST", valueFrom = { fieldRef = { fieldPath = "status.podIP" } } }] : []
            )
            ports           = [for name, port in local.runner_services[each.key] : { name = name, containerPort = port }]
            readinessProbe  = { tcpSocket = { port = "peer" } }
            securityContext = merge(local.runner_security, { runAsUser = 10001, runAsGroup = 10001 })
            resources = {
              requests = { cpu = "250m", memory = "512Mi", "ephemeral-storage" = "1Gi" }
              limits   = { cpu = "500m", memory = "1Gi", "ephemeral-storage" = "1Gi" }
            }
            volumeMounts = [
              { name = "process-secrets", mountPath = "/runbook/secrets", readOnly = true },
              { name = "tmp", mountPath = "/tmp" }
            ]
          }], contains(["api", "vault"], each.key) ? [local.runner_proxy] : [])
          volumes = [
            { name = "tmp", emptyDir = { sizeLimit = "512Mi" } },
            { name = "process-secrets", csi = {
              driver = "secrets-store-gke.csi.k8s.io", readOnly = true
              volumeAttributes = {
                secretProviderClass = kubernetes_manifest.runner_secrets[each.key].manifest.metadata.name
              }
            } }
          ]
        }
      }
    }
  }
}

resource "kubernetes_service_v1" "runner" {
  for_each = local.runner_processes
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}runner-${each.key}"
    namespace = local.runner_namespace
  }
  spec {
    selector = { "runbook.dev/process" = each.key }
    dynamic "port" {
      for_each = local.runner_services[each.key]
      content {
        name        = port.key
        port        = port.value
        target_port = port.value
      }
    }
  }
}

output "runner_api_url" {
  description = "Exact Service URL to use for one-time core registration."
  value       = var.runner_enabled ? local.runner_api_url : null
}


check "runner_oauth_https" {
  assert {
    condition     = !var.runner_enabled || var.ingress_enabled
    error_message = "OAuth MCP discovery and callbacks require an HTTPS core origin. This runner install uses HTTP because ingress_enabled is false. Enable managed-certificate ingress or configure a private HTTPS origin for OAuth testing as documented in the module README."
  }
}

resource "kubernetes_network_policy_v1" "runner_registry_resolution" {
  count = var.runner_enabled && var.network_policies && length(var.runner_registry_https_cidrs) > 0 ? 1 : 0
  metadata {
    name      = "${var.name_prefix}runner-registry-resolution"
    namespace = local.runner_namespace
    labels    = local.additional_labels
  }
  spec {
    pod_selector {
      match_labels = { "runbook.dev/process" = "api" }
    }
    policy_types = ["Egress"]
    egress {
      dynamic "to" {
        for_each = var.runner_registry_https_cidrs
        content {
          ip_block {
            cidr = to.value
          }
        }
      }
      ports {
        protocol = "TCP"
        port     = "443"
      }
    }
  }
}
