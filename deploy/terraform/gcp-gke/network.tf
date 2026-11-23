resource "kubernetes_network_policy_v1" "allow_load_balancer" {
  count = var.network_policies && var.ingress_enabled ? 1 : 0

  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}allow-load-balancer"
    namespace = kubernetes_namespace.runbook.metadata[0].name
  }

  spec {
    pod_selector {
      match_labels = { "app.kubernetes.io/name" = "runbook" }
    }
    policy_types = ["Ingress"]

    ingress {
      ports {
        port     = "8080"
        protocol = "TCP"
      }
      dynamic "from" {
        # Global external GKE Ingress (gce) uses GFEs, without a regional proxy-only subnet.
        for_each = ["35.191.0.0/16", "130.211.0.0/16"]
        content {
          ip_block {
            cidr = from.value
          }
        }
      }
    }
  }
}

resource "kubernetes_network_policy_v1" "deny_ingress" {
  count = var.network_policies ? 1 : 0

  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}default-deny-ingress"
    namespace = kubernetes_namespace.runbook.metadata[0].name
  }

  spec {
    pod_selector {}
    policy_types = ["Ingress"]
  }

  depends_on = [kubernetes_network_policy_v1.allow_load_balancer]
}

resource "kubernetes_network_policy_v1" "runbook_egress" {
  count = var.network_policies && var.restrict_egress ? 1 : 0

  metadata {
    labels    = local.additional_labels
    name      = "${var.name_prefix}runbook-egress"
    namespace = kubernetes_namespace.runbook.metadata[0].name
  }

  spec {
    pod_selector {
      match_expressions {
        key      = "app.kubernetes.io/name"
        operator = "In"
        values   = ["runbook", "runbook-migrate"]
      }
    }
    policy_types = ["Egress"]

    egress {
      to {
        namespace_selector {
          match_labels = { "kubernetes.io/metadata.name" = "kube-system" }
        }
        pod_selector {
          match_labels = { "k8s-app" = "kube-dns" }
        }
      }
      dynamic "to" {
        # Cloud DNS and NodeLocal DNSCache endpoints.
        for_each = ["169.254.169.254/32", "169.254.20.10/32"]
        content {
          ip_block {
            cidr = to.value
          }
        }
      }
      dynamic "ports" {
        for_each = ["UDP", "TCP"]
        content {
          port     = "53"
          protocol = ports.value
        }
      }
    }

    dynamic "egress" {
      for_each = {
        "169.254.169.252/32" = ["987", "988"]
        "169.254.169.254/32" = ["80", "8080"]
      }
      content {
        to {
          ip_block {
            cidr = egress.key
          }
        }
        dynamic "ports" {
          for_each = egress.value
          content {
            port     = ports.value
            protocol = "TCP"
          }
        }
      }
    }

    egress {
      to {
        ip_block {
          cidr = var.cloud_sql_private_cidr
        }
      }
      ports {
        port     = "3307"
        protocol = "TCP"
      }
    }

    egress {
      to {
        ip_block {
          cidr = "0.0.0.0/0"
          except = [
            "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16",
            "100.64.0.0/10", "169.254.0.0/16", "127.0.0.0/8",
          ]
        }
      }
    }
  }
}
