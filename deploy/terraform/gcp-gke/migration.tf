locals {
  # A Job's pod template is immutable. Naming each image's Job after that
  # image makes an upgrade create the new migration Job instead of trying
  # to rewrite the old one; finished Jobs are retained for a day of logs.
  migration_job_name = "runbook-migrate-${substr(sha256(var.image), 0, 12)}"
}

# UPD1: migrate once, before the Deployment rolls. The Cloud SQL proxy is
# a native sidecar (an init container with restartPolicy: Always), so
# Kubernetes starts it before `runbook-server migrate` and stops it when
# that one-shot process exits. Terraform waits for the Job to succeed.
resource "kubernetes_manifest" "migration_job" {
  computed_fields = [
    "metadata.annotations",
    "metadata.labels",
    "spec.template.metadata.annotations",
    "spec.template.metadata.labels",
    "status",
  ]

  wait {
    fields = {
      "status.succeeded" = "1"
    }
  }

  timeouts {
    create = "15m"
    update = "15m"
    delete = "5m"
  }

  manifest = {
    apiVersion = "batch/v1"
    kind       = "Job"
    metadata = {
      name      = local.migration_job_name
      namespace = kubernetes_namespace.runbook.metadata[0].name
      labels = {
        "app.kubernetes.io/name"       = "runbook"
        "app.kubernetes.io/component"  = "migration"
        "app.kubernetes.io/managed-by" = "terraform"
      }
    }
    spec = {
      backoffLimit            = 2
      activeDeadlineSeconds   = 900
      ttlSecondsAfterFinished = 86400
      template = {
        metadata = {
          labels = {
            "app.kubernetes.io/name"      = "runbook"
            "app.kubernetes.io/component" = "migration"
          }
        }
        spec = {
          serviceAccountName = kubernetes_service_account.runbook_ksa.metadata[0].name
          restartPolicy      = "Never"
          securityContext = {
            runAsNonRoot = true
            runAsUser    = 65532
            runAsGroup   = 65532
            fsGroup      = 65532
            seccompProfile = {
              type = "RuntimeDefault"
            }
          }
          initContainers = [
            {
              name          = "cloud-sql-proxy"
              image         = "gcr.io/cloud-sql-connectors/cloud-sql-proxy:2.14.2"
              restartPolicy = "Always"
              args = [
                "--port=5432",
                "--health-check",
                "--http-port=9801",
                "--exit-zero-on-sigterm",
                google_sql_database_instance.postgres.connection_name,
              ]
              ports = [
                {
                  name          = "postgres"
                  containerPort = 5432
                  protocol      = "TCP"
                },
              ]
              startupProbe = {
                tcpSocket = {
                  port = 5432
                }
                periodSeconds    = 1
                timeoutSeconds   = 1
                failureThreshold = 60
              }
              resources = {
                requests = {
                  cpu    = "100m"
                  memory = "128Mi"
                }
                limits = {
                  cpu    = "500m"
                  memory = "512Mi"
                }
              }
              securityContext = {
                runAsNonRoot             = true
                allowPrivilegeEscalation = false
                readOnlyRootFilesystem   = true
                capabilities = {
                  drop = ["ALL"]
                }
                seccompProfile = {
                  type = "RuntimeDefault"
                }
              }
            },
          ]
          containers = [
            {
              name            = "runbook-migrate"
              image           = var.image
              imagePullPolicy = "IfNotPresent"
              args            = ["migrate"]
              env = [
                {
                  name  = "RUNBOOK_ALLOW_NEWER_SCHEMA"
                  value = tostring(var.allow_newer_schema)
                },
                {
                  name = "DATABASE_URL"
                  valueFrom = {
                    secretKeyRef = {
                      name = kubernetes_secret.runbook_secrets.metadata[0].name
                      key  = "DATABASE_URL"
                    }
                  }
                },
              ]
              resources = {
                requests = {
                  cpu    = "100m"
                  memory = "128Mi"
                }
                limits = {
                  cpu    = "500m"
                  memory = "512Mi"
                }
              }
              securityContext = {
                runAsNonRoot             = true
                runAsUser                = 65532
                runAsGroup               = 65532
                allowPrivilegeEscalation = false
                readOnlyRootFilesystem   = true
                capabilities = {
                  drop = ["ALL"]
                }
                seccompProfile = {
                  type = "RuntimeDefault"
                }
              }
            },
          ]
        }
      }
    }
  }

  depends_on = [
    google_service_account_iam_member.workload_identity_user,
    google_project_iam_member.cloudsql_client,
    kubernetes_secret.runbook_secrets,
  ]
}
