data "kubernetes_resources" "runner_runtime_classes" {
  count       = var.runner_enabled && !var.create_cluster ? 1 : 0
  api_version = "node.k8s.io/v1"
  kind        = "RuntimeClass"
}

data "external" "runner_sandbox_iam" {
  count   = var.runner_enabled && var.runner_database_bootstrap_ready ? 1 : 0
  program = ["python3", "${path.module}/check-sandbox-iam.py"]
  query = {
    project           = var.project_id
    project_number    = data.google_project.runner[0].number
    cluster_name      = local.cluster_name
    sandbox_namespace = local.sandbox_namespace
    sandbox_account   = local.sandbox_account_name
    location          = local.cluster_location
    sandbox_uid       = kubernetes_service_account_v1.sandbox[0].metadata[0].uid
  }
  depends_on = [google_project_service.apis]
}

resource "terraform_data" "runner_setup" {
  count = var.runner_enabled ? 1 : 0

  lifecycle {
    precondition {
      condition     = var.create_cluster ? true : data.google_container_cluster.existing[0].enable_autopilot
      error_message = "The runner requires a GKE Autopilot cluster."
    }
    precondition {
      condition     = var.create_cluster ? true : data.google_container_cluster.existing[0].datapath_provider == "ADVANCED_DATAPATH"
      error_message = "The runner requires GKE Dataplane V2."
    }
    precondition {
      condition     = local.runner_supports_end_port
      error_message = "The runner requires GKE 1.32 or later."
    }
    precondition {
      condition = var.create_cluster ? true : anytrue([
        for runtime in data.kubernetes_resources.runner_runtime_classes[0].objects :
        runtime.metadata.name == "gvisor"
      ])
      error_message = "The runner requires the gvisor RuntimeClass."
    }
    precondition {
      condition     = var.runner_database_bootstrap_ready ? data.external.runner_sandbox_iam[0].result.safe == "true" : true
      error_message = "The sandbox IAM setup check must pass before enabling workloads."
    }
  }
}
