resource "kubernetes_namespace" "runbook" {
  metadata {
    name = "runbook"
    labels = {
      "app.kubernetes.io/name"       = "runbook"
      "app.kubernetes.io/managed-by" = "terraform"
    }
  }
}

resource "kubernetes_service_account" "runbook_ksa" {
  metadata {
    name      = "runbook-ksa"
    namespace = kubernetes_namespace.runbook.metadata[0].name
    annotations = {
      "iam.gke.io/gcp-service-account" = google_service_account.runbook_gsa.email
    }
    labels = {
      "app.kubernetes.io/name"       = "runbook"
      "app.kubernetes.io/managed-by" = "terraform"
    }
  }
}

resource "kubernetes_config_map" "runbook_config" {
  metadata {
    name      = "runbook-config"
    namespace = kubernetes_namespace.runbook.metadata[0].name
    labels = {
      "app.kubernetes.io/name"       = "runbook"
      "app.kubernetes.io/managed-by" = "terraform"
    }
  }

  data = merge(
    {
      RUNBOOK_ORIGIN                  = "https://${var.app_domain}"
      RUNBOOK_USERCONTENT_ORIGIN      = var.usercontent_domain != "" ? "https://${var.usercontent_domain}" : ""
      RUNBOOK_AUTH_METHODS            = var.auth_methods
      RUNBOOK_AUTH_ALLOWED_DOMAINS    = var.auth_allowed_domains
      RUNBOOK_SMTP_FROM               = var.smtp_from
      RUNBOOK_LIMIT_STEP_RUNS         = var.step_runs_limit
      RUNBOOK_LOG_FORMAT              = "json"
      RUNBOOK_MIGRATE_ON_START        = "false"
      RUNBOOK_ALLOW_NEWER_SCHEMA      = tostring(var.allow_newer_schema)
      RUNBOOK_DATABASE_POOL_SIZE      = tostring(var.database_pool_size)
      RUNBOOK_AUTH_DATABASE_POOL_SIZE = tostring(var.auth_database_pool_size)
      RUNBOOK_SHUTDOWN_TIMEOUT_SECS   = tostring(var.shutdown_timeout_secs)
      # Google's front ends, and the load balancer's own address: it appends
      # itself to X-Forwarded-For after the client, so without it every
      # client would look like the load balancer and share its limits.
      RUNBOOK_TRUSTED_PROXIES = "35.191.0.0/16,130.211.0.0/16,${google_compute_global_address.ingress_ip.address}"
      RUNBOOK_BLOB_STORE      = var.blob_store
    },
    var.blob_store == "gcs" ? {
      RUNBOOK_BLOB_BUCKET = google_storage_bucket.blobs[0].name
      RUNBOOK_BLOB_PREFIX = var.blob_prefix
    } : {},
    var.google_client_id != "" ? {
      RUNBOOK_GOOGLE_CLIENT_ID = var.google_client_id
    } : {},
    var.slack_default_channel != "" ? {
      RUNBOOK_SLACK_DEFAULT_CHANNEL = var.slack_default_channel
    } : {}
  )
}

resource "kubernetes_deployment" "runbook" {
  depends_on = [kubernetes_manifest.migration_job]

  metadata {
    name      = "runbook"
    namespace = kubernetes_namespace.runbook.metadata[0].name
    labels = {
      "app.kubernetes.io/name"       = "runbook"
      "app.kubernetes.io/managed-by" = "terraform"
    }
  }

  spec {
    replicas = var.replicas

    selector {
      match_labels = {
        "app.kubernetes.io/name" = "runbook"
      }
    }

    template {
      metadata {
        labels = {
          "app.kubernetes.io/name" = "runbook"
        }
        # Pods read the ConfigMap and the Secret only at start: a change to
        # either rolls the Deployment (a rotated Slack, SMTP or Google secret
        # takes effect on apply). Hashes only; no values.
        annotations = {
          "runbook.dev/config-sha256" = sha256(jsonencode(kubernetes_config_map.runbook_config.data))
          "runbook.dev/secret-sha256" = nonsensitive(sha256(jsonencode(kubernetes_secret.runbook_secrets.data)))
        }
      }

      spec {
        service_account_name             = kubernetes_service_account.runbook_ksa.metadata[0].name
        termination_grace_period_seconds = 45

        # Cloud SQL Auth Proxy sidecar container
        # Note: hashicorp/kubernetes provider does not yet support restart_policy in init_container (native sidecars).
        # Cloud SQL Auth Proxy runs as a standard sidecar with health checks and max-sigterm-delay.
        container {
          name  = "cloud-sql-proxy"
          image = "gcr.io/cloud-sql-connectors/cloud-sql-proxy:2.14.2"
          args = [
            "--private-ip",
            "--structured-logs",
            "--port=5432",
            "--health-check",
            "--http-address=0.0.0.0",
            "--max-sigterm-delay=30s",
            google_sql_database_instance.postgres.connection_name,
          ]

          startup_probe {
            http_get {
              path = "/startup"
              port = 9090
            }
            period_seconds    = 2
            timeout_seconds   = 3
            failure_threshold = 30
          }

          security_context {
            run_as_non_root            = true
            allow_privilege_escalation = false
          }

          resources {
            # ephemeral-storage explicit, as Autopilot would set it.
            requests = {
              cpu                 = "50m"
              memory              = "64Mi"
              "ephemeral-storage" = "1Gi"
            }
            limits = {
              cpu                 = "200m"
              memory              = "256Mi"
              "ephemeral-storage" = "1Gi"
            }
          }
        }

        container {
          name  = "runbook"
          image = var.image

          port {
            name           = "http"
            container_port = 8080
          }

          env_from {
            secret_ref {
              name = kubernetes_secret.runbook_secrets.metadata[0].name
            }
          }

          env_from {
            config_map_ref {
              name = kubernetes_config_map.runbook_config.metadata[0].name
            }
          }

          readiness_probe {
            http_get {
              path = "/health/ready"
              port = 8080
            }
            period_seconds    = 5
            timeout_seconds   = 3
            failure_threshold = 3
          }

          liveness_probe {
            http_get {
              path = "/health/live"
              port = 8080
            }
            period_seconds    = 10
            timeout_seconds   = 3
            failure_threshold = 3
          }

          startup_probe {
            http_get {
              path = "/health/ready"
              port = 8080
            }
            period_seconds    = 2
            timeout_seconds   = 3
            failure_threshold = 60
          }

          lifecycle {
            pre_stop {
              exec {
                command = ["sleep", "5"]
              }
            }
          }

          # The image's runbook user, by number: with only run_as_non_root the
          # kubelet cannot verify a named USER and refuses to start the pod.
          security_context {
            read_only_root_filesystem  = true
            run_as_non_root            = true
            run_as_user                = 10001
            run_as_group               = 10001
            allow_privilege_escalation = false
          }

          volume_mount {
            name       = "tmp"
            mount_path = "/tmp"
          }

          volume_mount {
            name       = "home"
            mount_path = "/home/runbook"
          }

          resources {
            requests = {
              cpu                 = var.resources.requests.cpu
              memory              = var.resources.requests.memory
              "ephemeral-storage" = "1Gi"
            }
            limits = {
              cpu                 = var.resources.limits.cpu
              memory              = var.resources.limits.memory
              "ephemeral-storage" = "1Gi"
            }
          }
        }

        volume {
          name = "tmp"
          empty_dir {}
        }

        volume {
          name = "home"
          empty_dir {}
        }
      }
    }
  }
}

resource "kubernetes_pod_disruption_budget_v1" "runbook_pdb" {
  metadata {
    name      = "runbook-pdb"
    namespace = kubernetes_namespace.runbook.metadata[0].name
  }

  spec {
    min_available = 1

    selector {
      match_labels = {
        "app.kubernetes.io/name" = "runbook"
      }
    }
  }
}

resource "kubernetes_service_v1" "runbook_service" {
  metadata {
    name      = "runbook"
    namespace = kubernetes_namespace.runbook.metadata[0].name
    labels = {
      "app.kubernetes.io/name"       = "runbook"
      "app.kubernetes.io/managed-by" = "terraform"
    }
    annotations = {
      "cloud.google.com/backend-config" = jsonencode({ "default" = "runbook-backendconfig" })
      "cloud.google.com/neg"            = jsonencode({ "ingress" = true })
    }
  }

  spec {
    type = "ClusterIP"

    selector = {
      "app.kubernetes.io/name" = "runbook"
    }

    port {
      name        = "http"
      port        = 8080
      target_port = 8080
      protocol    = "TCP"
    }
  }
}
