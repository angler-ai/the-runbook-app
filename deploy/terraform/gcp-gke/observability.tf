locals {
  trace_endpoint = "https://telemetry.googleapis.com/v1/traces"
  # Not the gateway: it parses untrusted sandbox traffic and stays without a
  # Google credential or a route to the metadata server. It still propagates
  # trace context and writes trace IDs into its logs.
  trace_runner_roles = var.trace_export_enabled && var.runner_enabled ? toset([
    "api", "worker", "vault", "sandbox-ctl",
  ]) : toset([])
  trace_sampling_environment = {
    RUNBOOK_TRACE_SAMPLE_RATIO = tostring(var.trace_sample_ratio)
    RUNBOOK_LOG_LEVEL          = "info"
  }
  trace_environment = merge(local.trace_sampling_environment, var.trace_export_enabled ? {
    GOOGLE_CLOUD_PROJECT               = var.project_id
    OTEL_EXPORTER_OTLP_TRACES_ENDPOINT = local.trace_endpoint
  } : {})
}

resource "google_project_service" "observability" {
  for_each = toset(concat(
    var.trace_export_enabled ? ["cloudtrace.googleapis.com", "telemetry.googleapis.com"] : [],
    var.log_exclusions_enabled ? ["logging.googleapis.com"] : [],
  ))
  project            = var.project_id
  service            = each.key
  disable_on_destroy = false
}

resource "google_project_iam_member" "core_trace" {
  count   = var.trace_export_enabled ? 1 : 0
  project = var.project_id
  role    = "roles/cloudtrace.agent"
  member  = "serviceAccount:${google_service_account.runbook_gsa.email}"

  depends_on = [google_project_service.observability]
}

resource "google_project_iam_member" "runner_trace" {
  for_each = local.trace_runner_roles
  project  = var.project_id
  role     = "roles/cloudtrace.agent"
  member = contains(local.runner_google_identities, each.key) ? (
    "serviceAccount:${google_service_account.runner[each.key].email}"
    ) : (
    "principal://iam.googleapis.com/projects/${data.google_project.runner[0].number}/locations/global/workloadIdentityPools/${var.project_id}.svc.id.goog/subject/ns/${local.runner_namespace_name}/sa/${var.name_prefix}${each.key}"
  )

  depends_on = [google_project_service.observability]
}

# API and vault already reach these addresses. Give only the additional
# exporting processes the same path through the restricted VIP.
resource "kubernetes_network_policy_v1" "runner_trace_egress" {
  # The runner's default-deny policy exists whenever the runner is enabled.
  count = length(local.trace_runner_roles) != 0 ? 1 : 0

  metadata {
    name      = "${var.name_prefix}trace-export"
    namespace = local.runner_namespace
    labels    = local.additional_labels
  }

  spec {
    pod_selector {
      match_expressions {
        key      = "runbook.dev/process"
        operator = "In"
        values   = sort(tolist(setsubtract(local.trace_runner_roles, local.runner_google_identities)))
      }
    }

    policy_types = ["Egress"]

    dynamic "egress" {
      for_each = local.runner_google_cidrs
      content {
        to {
          ip_block {
            cidr = egress.key
          }
        }
        ports {
          protocol = "TCP"
          port     = tostring(egress.value)
        }
      }
    }
  }
}

resource "google_logging_project_exclusion" "runbook_noise" {
  count       = var.log_exclusions_enabled ? 1 : 0
  project     = var.project_id
  name        = "${var.name_prefix}runbook-noise"
  description = "Exclude debug and health-check traffic from Runbook container logs"
  filter      = <<-FILTER
    resource.type="k8s_container"
    (resource.labels.namespace_name="${kubernetes_namespace.runbook.metadata[0].name}" OR resource.labels.namespace_name="${local.runner_namespace_name}")
    (severity=DEBUG OR (severity<WARNING AND (httpRequest.requestUrl =~ "^(https?://[^/?]+)?/(health/(live|ready|decision)|healthz|readyz)([?]|$)" OR jsonPayload.path =~ "^/(health/(live|ready|decision)|healthz|readyz)$")))
  FILTER

  depends_on = [google_project_service.observability]
}

output "observability_environment" {
  description = "Nonsecret environment values for the core ConfigMap and runner process containers. Set RUNBOOK_LOG_FORMAT=json on core."
  value       = local.trace_environment
}
