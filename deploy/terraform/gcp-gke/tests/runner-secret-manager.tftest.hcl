mock_provider "google" {}
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
  runner_secret_manager_secret_ids = [{
    resource     = "projects/runbook-test-project/secrets/upstream"
    types        = ["api-key"]
    destinations = ["api.example.com"]
  }]
}

run "startup_secrets_cannot_be_released" {
  command = plan

  plan_options {
    target = [google_secret_manager_secret_iam_member.runner_vault_access]
  }

  variables {
    runner_secret_versions = {
      api           = "projects/123456789/secrets/api-startup/versions/1"
      vault         = "projects/123456789/secrets/vault-startup/versions/1"
      gateway       = "projects/123456789/secrets/gateway-startup/versions/1"
      worker        = "projects/123456789/secrets/worker-startup/versions/1"
      "sandbox-ctl" = "projects/123456789/secrets/sandbox-startup/versions/1"
    }
    runner_secret_manager_secret_ids = [{
      resource     = "projects/runbook-test-project/secrets/vault-startup"
      types        = ["api-key"]
      destinations = ["api.example.com"]
    }]
  }

  expect_failures = [var.runner_secret_manager_secret_ids]
}

run "audit_management_is_opt_in" {
  command = plan

  plan_options {
    target = [google_project_iam_audit_config.runner_secret_manager_opt_in, google_secret_manager_secret_iam_member.runner_vault_access]
  }

  assert {
    condition     = length(google_project_iam_audit_config.runner_secret_manager_opt_in) == 0
    error_message = "A deployment must explicitly opt in to managing the project's audit configuration."
  }

  assert {
    condition     = google_secret_manager_secret_iam_member.runner_vault_access["upstream"].secret_id == "upstream"
    error_message = "The vault's IAM grant must be scoped to the referenced secret."
  }
}

run "opted_in_audit_management_logs_reads_and_writes" {
  command = plan

  plan_options {
    target = [google_project_iam_audit_config.runner_secret_manager_opt_in]
  }

  variables {
    manage_secret_manager_audit_config = true
  }

  assert {
    condition     = toset(google_project_iam_audit_config.runner_secret_manager_opt_in[0].audit_log_config[*].log_type) == toset(["DATA_READ", "DATA_WRITE"])
    error_message = "The opt-in audit configuration must cover version reads and writes."
  }
}
