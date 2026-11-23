resource "google_dns_managed_zone" "runner_googleapis" {
  count       = var.runner_enabled && var.runner_google_api_dns == "vpc" ? 1 : 0
  project     = var.project_id
  name        = "${var.name_prefix}runbook-googleapis"
  labels      = local.additional_labels
  dns_name    = "googleapis.com."
  description = "Use Google's restricted API VIP from the Runbook VPC"
  visibility  = "private"
  private_visibility_config {
    networks {
      network_url = var.network
    }
  }
  depends_on = [google_project_service.apis]
}

resource "google_dns_record_set" "runner_googleapis_vip" {
  count        = var.runner_enabled && var.runner_google_api_dns == "vpc" ? 1 : 0
  project      = var.project_id
  managed_zone = google_dns_managed_zone.runner_googleapis[0].name
  name         = "restricted.googleapis.com."
  type         = "A"
  ttl          = 300
  rrdatas      = local.runner_restricted_vip
}

resource "google_dns_record_set" "runner_googleapis_alias" {
  count        = var.runner_enabled && var.runner_google_api_dns == "vpc" ? 1 : 0
  project      = var.project_id
  managed_zone = google_dns_managed_zone.runner_googleapis[0].name
  name         = "*.googleapis.com."
  type         = "CNAME"
  ttl          = 300
  rrdatas      = [google_dns_record_set.runner_googleapis_vip[0].name]
}

# Dataplane V2 checks the translated destination. Discover the API
# endpoints rather than matching the kubernetes Service's ClusterIP.
data "kubernetes_endpoints_v1" "runner_kubernetes" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    name      = "kubernetes"
    namespace = "default"
  }
}

locals {
  runner_master_version       = var.create_cluster ? google_container_cluster.autopilot[0].master_version : data.google_container_cluster.existing[0].master_version
  runner_master_version_parts = split(".", local.runner_master_version)
  runner_supports_end_port = try(
    tonumber(local.runner_master_version_parts[0]) > 1 ||
    tonumber(local.runner_master_version_parts[0]) == 1 && tonumber(local.runner_master_version_parts[1]) >= 32, false
  )
  runner_restricted_vip = ["199.36.153.4", "199.36.153.5", "199.36.153.6", "199.36.153.7"]
  # In "pods" mode, only runner pods resolve these names to the restricted VIP.
  runner_google_api_hosts = sort(distinct(concat([
    for service in ["aiplatform", "cloudkms", "iamcredentials", "logging", "oauth2", "secretmanager", "sqladmin", "storage", "sts", "telemetry", "www"] :
    "${service}.googleapis.com"
    ], flatten([
      for locations in values(var.runner_vertex_targets) : [
        for location in locations : "${location}-aiplatform.googleapis.com"
        if location != "global"
      ]
  ]))))
  runner_google_api_host_aliases = var.runner_google_api_dns == "pods" ? {
    hostAliases = [for ip in local.runner_restricted_vip : { ip = ip, hostnames = local.runner_google_api_hosts }]
  } : {}
  runner_google_cidrs = {
    "199.36.153.4/30"    = 443
    "169.254.169.254/32" = 80
    "169.254.169.252/32" = 988
  }
  runner_gateway_internet_exclusions = [
    "0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16",
    "172.16.0.0/12", "192.0.0.0/24", "192.0.2.0/24", "192.168.0.0/16", "198.18.0.0/15", "198.51.100.0/24",
    "203.0.113.0/24", "224.0.0.0/3"
  ]
}

resource "kubernetes_network_policy_v1" "runner_default_deny" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}default-deny"
    namespace = local.runner_namespace
  }
  spec {
    pod_selector {}
    policy_types = ["Ingress", "Egress"]
  }
}

resource "kubernetes_network_policy_v1" "runner" {
  for_each = setunion(local.runner_processes, var.runner_enabled ? toset(["bootstrap"]) : toset([]))
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}${each.key}"
    namespace = local.runner_namespace
  }
  spec {
    pod_selector {
      match_labels = { "runbook.dev/process" = each.key }
    }
    policy_types = ["Ingress", "Egress"]

    dynamic "ingress" {
      for_each = toset(lookup(local.runner_callers, each.key, []))
      content {
        from {
          namespace_selector {
            match_labels = { "kubernetes.io/metadata.name" = local.runner_namespace }
          }
          pod_selector {
            match_labels = { "runbook.dev/process" = ingress.value }
          }
        }
        ports {
          protocol = "TCP"
          port     = tostring(local.runner_services[each.key].peer)
        }
      }
    }

    dynamic "ingress" {
      for_each = each.key == "api" ? [1] : []
      content {
        from {
          namespace_selector {
            match_labels = { "kubernetes.io/metadata.name" = kubernetes_namespace.runbook.metadata[0].name }
          }
          pod_selector {
            match_labels = { "app.kubernetes.io/name" = "runbook" }
          }
        }
        ports {
          protocol = "TCP"
          port     = "8090"
        }
      }
    }

    dynamic "ingress" {
      for_each = each.key == "gateway" ? [1] : []
      content {
        from {
          namespace_selector {
            match_labels = { "kubernetes.io/metadata.name" = local.sandbox_namespace }
          }
          pod_selector {
            match_labels = { "runbook.dev/sandbox" = "true" }
          }
        }
        ports {
          protocol = "TCP"
          port     = "20000"
          end_port = 29999
        }
      }
    }

    dynamic "egress" {
      for_each = toset(lookup(local.runner_callees, each.key, []))
      content {
        to {
          namespace_selector {
            match_labels = { "kubernetes.io/metadata.name" = local.runner_namespace }
          }
          pod_selector {
            match_labels = { "runbook.dev/process" = egress.value }
          }
        }
        ports {
          protocol = "TCP"
          port     = tostring(local.runner_services[egress.value].peer)
        }
      }
    }

    egress {
      to {
        ip_block {
          cidr = "${var.runner_dns_ip}/32"
        }
      }
      ports {
        protocol = "UDP"
        port     = "53"
      }
      ports {
        protocol = "TCP"
        port     = "53"
      }
    }

    # Keep the public core origin for issuer verification and the core's
    # canonical routing. Limit access to its module-owned HTTPS ingress IP.
    dynamic "egress" {
      for_each = var.ingress_enabled && contains(["api", "worker", "sandbox-ctl"], each.key) ? [1] : []
      content {
        to {
          ip_block {
            cidr = "${google_compute_global_address.ingress_ip[0].address}/32"
          }
        }
        ports {
          protocol = "TCP"
          port     = "443"
        }
      }
    }

    dynamic "egress" {
      for_each = each.key == "vault" || (!var.ingress_enabled && contains(["api", "worker", "sandbox-ctl"], each.key)) ? [1] : []
      content {
        to {
          namespace_selector {
            match_labels = { "kubernetes.io/metadata.name" = kubernetes_namespace.runbook.metadata[0].name }
          }
          pod_selector {
            match_labels = { "app.kubernetes.io/name" = "runbook" }
          }
        }
        ports {
          protocol = "TCP"
          port     = "8080"
        }
      }
    }

    dynamic "egress" {
      for_each = contains(local.runner_google_identities, each.key) ? merge(local.runner_google_cidrs, {
        ("${google_sql_database_instance.postgres.private_ip_address}/32") = 3307
      }) : {}
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

    dynamic "egress" {
      for_each = each.key == "sandbox-ctl" ? [
        for subset in data.kubernetes_endpoints_v1.runner_kubernetes[0].subset : subset
        if length(subset.address) > 0 && length(subset.port) > 0
      ] : []
      content {
        dynamic "to" {
          for_each = egress.value.address
          content {
            ip_block {
              cidr = "${to.value.ip}/32"
            }
          }
        }
        dynamic "ports" {
          for_each = egress.value.port
          content {
            protocol = "TCP"
            port     = tostring(ports.value.port)
          }
        }
      }
    }

    dynamic "egress" {
      for_each = each.key == "gateway" ? [1] : []
      content {
        to {
          ip_block {
            cidr   = "0.0.0.0/0"
            except = local.runner_gateway_internet_exclusions
          }
        }
        ports {
          protocol = "TCP"
          port     = "443"
        }
      }
    }
  }
}

resource "kubernetes_network_policy_v1" "sandboxes" {
  count = var.runner_enabled ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}sandboxes"
    namespace = local.sandbox_namespace
  }
  lifecycle {
    precondition {
      condition     = local.runner_supports_end_port
      error_message = "The runner requires GKE 1.32 or later for Dataplane V2 listener port ranges."
    }
  }
  spec {
    pod_selector {}
    policy_types = ["Ingress", "Egress"]
    egress {
      to {
        namespace_selector {
          match_labels = { "kubernetes.io/metadata.name" = local.runner_namespace }
        }
        pod_selector {
          match_labels = { "runbook.dev/process" = "gateway" }
        }
      }
      ports {
        protocol = "TCP"
        port     = "20000"
        end_port = 29999
      }
    }
  }
}

# Add this peer to the existing restricted core egress rules. Leave clusters
# with unrestricted core egress on their existing policy.
resource "kubernetes_network_policy_v1" "core_runner" {
  count = var.runner_enabled && var.network_policies && var.restrict_egress ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}core-to-runner-api"
    namespace = kubernetes_namespace.runbook.metadata[0].name
  }
  spec {
    pod_selector {
      match_labels = { "app.kubernetes.io/name" = "runbook" }
    }
    policy_types = ["Egress"]
    egress {
      to {
        namespace_selector {
          match_labels = { "kubernetes.io/metadata.name" = local.runner_namespace }
        }
        pod_selector {
          match_labels = { "runbook.dev/process" = "api" }
        }
      }
      ports {
        protocol = "TCP"
        port     = "8090"
      }
    }
  }
}


resource "kubernetes_network_policy_v1" "vault_core" {
  count = var.runner_enabled && var.network_policies ? 1 : 0
  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}vault-to-core"
    namespace = kubernetes_namespace.runbook.metadata[0].name
  }
  spec {
    pod_selector {
      match_labels = { "app.kubernetes.io/name" = "runbook" }
    }
    policy_types = ["Ingress"]
    ingress {
      from {
        namespace_selector {
          match_labels = { "kubernetes.io/metadata.name" = local.runner_namespace }
        }
        pod_selector {
          match_labels = var.ingress_enabled ? { "runbook.dev/process" = "vault" } : null
          dynamic "match_expressions" {
            for_each = var.ingress_enabled ? [] : [1]
            content {
              key      = "runbook.dev/process"
              operator = "In"
              values   = ["api", "worker", "sandbox-ctl", "vault"]
            }
          }
        }
      }
      ports {
        protocol = "TCP"
        port     = "8080"
      }
    }
  }
}
