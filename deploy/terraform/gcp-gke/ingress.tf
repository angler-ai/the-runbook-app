# Reserved global static IPv4 address for GKE Ingress
resource "google_compute_global_address" "ingress_ip" {
  name        = "runbook-ingress-ip"
  description = "Static external IP address for The Runbook GKE Ingress"
  project     = var.project_id

  depends_on = [google_project_service.apis]
}

# GKE BackendConfig for health check and optional Cloud Armor
resource "kubernetes_manifest" "backend_config" {
  manifest = {
    apiVersion = "cloud.google.com/v1"
    kind       = "BackendConfig"
    metadata = {
      name      = "runbook-backendconfig"
      namespace = kubernetes_namespace.runbook.metadata[0].name
      labels = {
        "app.kubernetes.io/name"       = "runbook"
        "app.kubernetes.io/managed-by" = "terraform"
      }
    }
    spec = merge(
      {
        healthCheck = {
          checkIntervalSec   = 10
          timeoutSec         = 3
          healthyThreshold   = 1
          unhealthyThreshold = 3
          type               = "HTTP"
          requestPath        = "/health/ready"
          port               = 8080
        }
      },
      var.cloud_armor_policy != "" ? {
        securityPolicy = {
          name = var.cloud_armor_policy
        }
      } : {}
    )
  }
}

# GKE FrontendConfig for HTTP to HTTPS redirect
resource "kubernetes_manifest" "frontend_config" {
  manifest = {
    apiVersion = "networking.gke.io/v1beta1"
    kind       = "FrontendConfig"
    metadata = {
      name      = "runbook-frontendconfig"
      namespace = kubernetes_namespace.runbook.metadata[0].name
      labels = {
        "app.kubernetes.io/name"       = "runbook"
        "app.kubernetes.io/managed-by" = "terraform"
      }
    }
    spec = merge(
      {
        redirectToHttps = {
          enabled          = true
          responseCodeName = "MOVED_PERMANENTLY_DEFAULT"
        }
      },
      var.ssl_policy != "" ? { sslPolicy = var.ssl_policy } : {}
    )
  }
}

# Google-managed TLS certificate for app and user-content hostnames
resource "kubernetes_manifest" "managed_certificate" {
  manifest = {
    apiVersion = "networking.gke.io/v1"
    kind       = "ManagedCertificate"
    metadata = {
      name      = "runbook-managed-cert"
      namespace = kubernetes_namespace.runbook.metadata[0].name
      labels = {
        "app.kubernetes.io/name"       = "runbook"
        "app.kubernetes.io/managed-by" = "terraform"
      }
    }
    spec = {
      domains = concat([var.app_domain], var.usercontent_domain != "" ? [var.usercontent_domain] : [])
    }
  }
}

# GKE Ingress routing to the Runbook Service
resource "kubernetes_ingress_v1" "runbook_ingress" {
  metadata {
    name      = "runbook-ingress"
    namespace = kubernetes_namespace.runbook.metadata[0].name
    labels = {
      "app.kubernetes.io/name"       = "runbook"
      "app.kubernetes.io/managed-by" = "terraform"
    }
    annotations = {
      "kubernetes.io/ingress.class"                 = "gce"
      "kubernetes.io/ingress.global-static-ip-name" = google_compute_global_address.ingress_ip.name
      "networking.gke.io/managed-certificates"      = "runbook-managed-cert"
      "networking.gke.io/v1beta1.FrontendConfig"    = "runbook-frontendconfig"
    }
  }

  spec {
    rule {
      host = var.app_domain
      http {
        path {
          path      = "/*"
          path_type = "ImplementationSpecific"
          backend {
            service {
              name = kubernetes_service_v1.runbook_service.metadata[0].name
              port {
                number = 8080
              }
            }
          }
        }
      }
    }

    dynamic "rule" {
      for_each = var.usercontent_domain != "" ? [var.usercontent_domain] : []
      content {
        host = rule.value
        http {
          path {
            path      = "/*"
            path_type = "ImplementationSpecific"
            backend {
              service {
                name = kubernetes_service_v1.runbook_service.metadata[0].name
                port {
                  number = 8080
                }
              }
            }
          }
        }
      }
    }
  }

  depends_on = [
    kubernetes_manifest.backend_config,
    kubernetes_manifest.frontend_config,
    kubernetes_manifest.managed_certificate
  ]
}
