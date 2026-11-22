# Generated secrets
resource "random_id" "webhook_key" {
  count       = var.manage_secret_values ? 1 : 0
  byte_length = 32
}

resource "random_id" "auth_secret" {
  count       = var.manage_secret_values ? 1 : 0
  byte_length = 32
}

resource "random_id" "upload_signing_key" {
  count       = var.manage_secret_values ? 1 : 0
  byte_length = 32
}


locals {
  workload_secret_name = var.manage_secret_values ? kubernetes_secret.runbook_secrets[0].metadata[0].name : var.existing_secret_name
}
# Kubernetes Secret populated for the Deployment
resource "kubernetes_secret" "runbook_secrets" {
  count = var.manage_secret_values ? 1 : 0

  metadata {
    name      = "runbook-secrets"
    namespace = kubernetes_namespace.runbook.metadata[0].name
    labels = {
      "app.kubernetes.io/name"       = "runbook"
      "app.kubernetes.io/managed-by" = "terraform"
    }
  }

  data = merge(
    {
      DATABASE_URL               = "postgres://${urlencode(local.database_user)}${local.manage_db_password ? ":${random_password.db_password[0].result}" : ""}@127.0.0.1:5432/${google_sql_database.runbook.name}?sslmode=disable"
      RUNBOOK_WEBHOOK_SECRET_KEY = random_id.webhook_key[0].b64_std
      RUNBOOK_AUTH_SECRET        = random_id.auth_secret[0].hex
    },
    var.usercontent_domain != "" ? {
      RUNBOOK_UPLOAD_SIGNING_KEY = random_id.upload_signing_key[0].b64_std
    } : {},
    var.smtp_url != "" ? {
      RUNBOOK_SMTP_URL = var.smtp_url
    } : {},
    var.google_client_secret != "" ? {
      RUNBOOK_GOOGLE_CLIENT_SECRET = var.google_client_secret
    } : {},
    var.slack_bot_token != "" ? {
      RUNBOOK_SLACK_BOT_TOKEN      = var.slack_bot_token
      RUNBOOK_SLACK_SIGNING_SECRET = var.slack_signing_secret
    } : {}
  )

  type = "Opaque"
}

removed {
  from = google_secret_manager_secret.database_url

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret.webhook_key

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret.auth_secret

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret.upload_signing_key

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret.smtp_url

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret.google_client_secret

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret.slack

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.database_url

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.webhook_key

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.auth_secret

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.upload_signing_key

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.smtp_url

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.google_client_secret

  lifecycle {
    destroy = false
  }
}

removed {
  from = google_secret_manager_secret_version.slack

  lifecycle {
    destroy = false
  }
}

moved {
  from = random_id.webhook_key
  to   = random_id.webhook_key[0]
}

moved {
  from = random_id.auth_secret
  to   = random_id.auth_secret[0]
}

moved {
  from = random_id.upload_signing_key
  to   = random_id.upload_signing_key[0]
}

moved {
  from = kubernetes_secret.runbook_secrets
  to   = kubernetes_secret.runbook_secrets[0]
}
