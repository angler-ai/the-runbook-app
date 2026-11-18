# Generated secrets
resource "random_id" "webhook_key" {
  byte_length = 32
}

resource "random_id" "auth_secret" {
  byte_length = 32
}

resource "random_id" "upload_signing_key" {
  byte_length = 32
}

# Secret Manager Secrets
resource "google_secret_manager_secret" "database_url" {
  secret_id = "runbook-database-url"
  project   = var.project_id

  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

resource "google_secret_manager_secret_version" "database_url" {
  secret      = google_secret_manager_secret.database_url.id
  secret_data = "postgres://${google_sql_user.runbook_app.name}:${random_password.db_password.result}@127.0.0.1:5432/${google_sql_database.runbook.name}?sslmode=disable"
}

resource "google_secret_manager_secret" "webhook_key" {
  secret_id = "runbook-webhook-key"
  project   = var.project_id

  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

resource "google_secret_manager_secret_version" "webhook_key" {
  secret      = google_secret_manager_secret.webhook_key.id
  secret_data = random_id.webhook_key.b64_std
}

resource "google_secret_manager_secret" "auth_secret" {
  secret_id = "runbook-auth-secret"
  project   = var.project_id

  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

resource "google_secret_manager_secret_version" "auth_secret" {
  secret      = google_secret_manager_secret.auth_secret.id
  secret_data = random_id.auth_secret.hex
}

resource "google_secret_manager_secret" "upload_signing_key" {
  count     = var.usercontent_domain != "" ? 1 : 0
  secret_id = "runbook-upload-signing-key"
  project   = var.project_id

  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

resource "google_secret_manager_secret_version" "upload_signing_key" {
  count       = var.usercontent_domain != "" ? 1 : 0
  secret      = google_secret_manager_secret.upload_signing_key[0].id
  secret_data = random_id.upload_signing_key.b64_std
}

resource "google_secret_manager_secret" "smtp_url" {
  count     = var.smtp_url != "" ? 1 : 0
  secret_id = "runbook-smtp-url"
  project   = var.project_id

  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

resource "google_secret_manager_secret_version" "smtp_url" {
  count       = var.smtp_url != "" ? 1 : 0
  secret      = google_secret_manager_secret.smtp_url[0].id
  secret_data = var.smtp_url
}

resource "google_secret_manager_secret" "google_client_secret" {
  count     = var.google_client_secret != "" ? 1 : 0
  secret_id = "runbook-google-client-secret"
  project   = var.project_id

  replication {
    auto {}
  }

  depends_on = [google_project_service.apis]
}

resource "google_secret_manager_secret_version" "google_client_secret" {
  count       = var.google_client_secret != "" ? 1 : 0
  secret      = google_secret_manager_secret.google_client_secret[0].id
  secret_data = var.google_client_secret
}

# Kubernetes Secret populated for the Deployment
resource "kubernetes_secret" "runbook_secrets" {
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
      DATABASE_URL               = "postgres://${google_sql_user.runbook_app.name}:${random_password.db_password.result}@127.0.0.1:5432/${google_sql_database.runbook.name}?sslmode=disable"
      RUNBOOK_WEBHOOK_SECRET_KEY = random_id.webhook_key.b64_std
      RUNBOOK_AUTH_SECRET        = random_id.auth_secret.hex
    },
    var.usercontent_domain != "" ? {
      RUNBOOK_UPLOAD_SIGNING_KEY = random_id.upload_signing_key.b64_std
    } : {},
    var.smtp_url != "" ? {
      RUNBOOK_SMTP_URL = var.smtp_url
    } : {},
    var.google_client_secret != "" ? {
      RUNBOOK_GOOGLE_CLIENT_SECRET = var.google_client_secret
    } : {}
  )

  type = "Opaque"
}
