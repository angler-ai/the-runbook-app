mock_provider "google" {
  mock_data "google_project" {
    defaults = { number = "123456789" }
  }
}
mock_provider "google-beta" {}
mock_provider "kubernetes" {}
mock_provider "random" {}
mock_provider "external" {}

variables {
  project_id             = "runbook-test-project"
  region                 = "us-central1"
  network                = "projects/runbook-test-project/global/networks/fixture"
  subnetwork             = "projects/runbook-test-project/regions/us-central1/subnetworks/fixture"
  app_domain             = "runbook.example"
  image                  = "example.com/runbook@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  runner_image           = "ghcr.io/angler-ai/runbook-runner@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
  runner_sandbox_image   = "ghcr.io/angler-ai/runbook-sandbox@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
  cloud_sql_private_cidr = "10.42.0.0/24"
  runner_enabled         = true
}

run "exclusions_without_trace_export" {
  command = plan
  plan_options {
    target = [
      google_project_service.observability,
      google_project_iam_member.core_trace,
      google_project_iam_member.runner_trace,
      google_logging_project_exclusion.runbook_noise,
    ]
  }
  variables {
    log_exclusions_enabled = true
  }
  assert {
    condition     = length(google_project_iam_member.core_trace) == 0 && length(google_project_iam_member.runner_trace) == 0
    error_message = "Trace exporter IAM is opt-in."
  }
  assert {
    condition = (
      toset(keys(google_project_service.observability)) == toset(["logging.googleapis.com"]) &&
      strcontains(google_logging_project_exclusion.runbook_noise[0].filter, "severity=DEBUG") &&
      strcontains(google_logging_project_exclusion.runbook_noise[0].filter, "health/(live|ready|decision)") &&
      strcontains(google_logging_project_exclusion.runbook_noise[0].filter, "runbook-runner")
    )
    error_message = "Debug and probes in the Runbook namespaces are excluded when enabled."
  }
}

run "trace_export_iam" {
  command = plan
  plan_options {
    target = [
      google_project_service.observability,
      google_project_iam_member.core_trace,
      google_project_iam_member.runner_trace,
      google_logging_project_exclusion.runbook_noise,
    ]
  }
  variables {
    trace_export_enabled   = true
    log_exclusions_enabled = false
    trace_sample_ratio     = 0.02
  }
  assert {
    condition = (
      google_project_iam_member.core_trace[0].role == "roles/cloudtrace.agent" &&
      toset(keys(google_project_iam_member.runner_trace)) == toset(["api", "worker", "gateway", "vault", "sandbox-ctl"]) &&
      alltrue([for member in google_project_iam_member.runner_trace : member.role == "roles/cloudtrace.agent"])
    )
    error_message = "Only the core and five runtime runner identities receive the trace role."
  }
  assert {
    condition = (
      toset(keys(google_project_service.observability)) == toset(["cloudtrace.googleapis.com", "telemetry.googleapis.com"]) &&
      length(google_logging_project_exclusion.runbook_noise) == 0
    )
    error_message = "Enable both trace APIs and honor the exclusion toggle."
  }
}
