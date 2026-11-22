variable "project_id" {
  type        = string
  description = "The Google Cloud project ID to deploy resources into."
}

variable "region" {
  type        = string
  description = "The Google Cloud region for regional resources (e.g. us-central1)."
}

variable "enable_apis" {
  type        = bool
  default     = true
  description = "Whether to manage enabling required Google Cloud APIs via Terraform."
}

# --- Network & Cluster Variables ---

variable "network" {
  type        = string
  description = "The VPC network name or self-link for GKE and Cloud SQL private IP."
}

variable "subnetwork" {
  type        = string
  description = "The subnetwork name or self-link for GKE nodes."
}

variable "create_cluster" {
  type        = bool
  default     = false
  description = "Whether to create a new GKE Autopilot cluster (true) or use an existing cluster (false)."
}

variable "cluster_name" {
  type        = string
  default     = "runbook-cluster"
  description = "Name of the GKE cluster (new or existing)."
}

variable "cluster_location" {
  type        = string
  default     = ""
  description = "Location (region or zone) of the GKE cluster. Defaults to var.region if unset."
}

variable "master_ipv4_cidr_block" {
  type        = string
  default     = "172.16.0.0/28"
  description = "The /28 IP range for GKE master control plane if create_cluster is true."
}

variable "create_private_service_access" {
  type        = bool
  default     = true
  description = "Whether to create the Google Service Networking connection for Cloud SQL private IP in var.network. Set to false if the VPC already has private service access configured."
}

# --- Cloud SQL Variables ---

variable "sql_instance_name" {
  type        = string
  default     = "runbook-db"
  description = "Name of the Cloud SQL PostgreSQL instance."
}

variable "sql_tier" {
  type        = string
  default     = "db-custom-1-3840"
  description = "The machine type / tier for the Cloud SQL instance."
}

variable "sql_edition" {
  type        = string
  default     = "ENTERPRISE"
  description = "The edition of the instance (ENTERPRISE or ENTERPRISE_PLUS)."
}

variable "sql_availability_type" {
  type        = string
  default     = "ZONAL"
  description = "The availability type of the Cloud SQL instance: ZONAL or REGIONAL (high availability)."
}

variable "sql_deletion_protection" {
  type        = bool
  default     = true
  description = "Whether deletion protection is enabled on the Cloud SQL instance."
}

variable "sql_backup_start_time" {
  type        = string
  default     = "03:00"
  description = "HH:MM format time (UTC) for automated backups to begin."
}

variable "sql_maintenance_window_day" {
  type        = number
  default     = 7
  description = "Day of week for maintenance window (1-7, 1=Monday, 7=Sunday)."
}

variable "sql_maintenance_window_hour" {
  type        = number
  default     = 3
  description = "Hour of day (0-23 UTC) for maintenance window."
}

variable "sql_maintenance_window_update_track" {
  type        = string
  default     = "stable"
  description = "Update track of maintenance window: stable or canary."
}

# --- Workload & Application Variables ---

variable "image" {
  type        = string
  description = "Container image to deploy, pinned to an X.Y.Z tag and optionally its digest (e.g. ghcr.io/angler-ai/runbook:0.1.1)."

  validation {
    condition     = can(regex("(:(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)(@sha256:[0-9a-f]{64})?|@sha256:[0-9a-f]{64})$", var.image))
    error_message = "image must be pinned to an X.Y.Z tag (optionally with a sha256 digest) or to a sha256 digest; do not use latest or a moving tag."
  }
}

variable "allow_newer_schema" {
  type        = bool
  default     = false
  description = "Allow the deployed release to run on a Postgres schema migrated by a newer release. Set true only for a rollback to a release at or above the newer binary's rollback floor."
}

variable "replicas" {
  type        = number
  default     = 2
  description = "Number of pod replicas in the Deployment."
}

variable "app_domain" {
  type        = string
  description = "The primary public domain name for The Runbook (e.g. runbooks.example.com)."
}

variable "usercontent_domain" {
  type        = string
  default     = ""
  description = "Optional secondary domain for user-content uploads (e.g. files.example-usercontent.com)."
}

variable "auth_methods" {
  type        = string
  default     = "email"
  description = "Comma-separated list of enabled auth methods (e.g. 'email' or 'email,google')."
}

variable "auth_allowed_domains" {
  type        = string
  default     = ""
  description = "Comma-separated list of email domains allowed to sign in (e.g. 'example.com,getangler.ai'). Empty allows any domain."
}

variable "smtp_url" {
  type        = string
  default     = ""
  sensitive   = true
  description = "Optional SMTP connection URL (e.g. smtps://user:pass@smtp.example.com:465) for email sign-in codes."

  validation {
    condition     = var.manage_secret_values || var.smtp_url == ""
    error_message = "Supply the SMTP URL only in the external Secret when manage_secret_values is false."
  }
}

variable "google_client_id" {
  type        = string
  default     = ""
  description = "Google OAuth web client ID for Google sign-in (auth_methods including 'google'). Its redirect URI is https://<app_domain>/api/auth/callback/google."
}

variable "google_client_secret" {
  type        = string
  default     = ""
  sensitive   = true
  description = "Google OAuth web client secret, with google_client_id. Stored in the Terraform-managed Kubernetes Secret."

  validation {
    condition     = var.manage_secret_values ? (var.google_client_secret == "") == (var.google_client_id == "") : var.google_client_secret == ""
    error_message = "Set google_client_id and google_client_secret together in managed mode. In external mode, pass the client ID only."
  }
}

variable "slack_bot_token" {
  type        = string
  default     = ""
  sensitive   = true
  description = "Bot token (xoxb-...) of The Runbook's Slack app (deploy/slack/manifest.yaml, docs/slack.md): approval buttons in Slack. With slack_signing_secret."
}

variable "slack_signing_secret" {
  type        = string
  default     = ""
  sensitive   = true
  description = "The Slack app's signing secret, with slack_bot_token."

  validation {
    condition     = var.manage_secret_values ? (var.slack_signing_secret == "") == (var.slack_bot_token == "") : var.slack_signing_secret == "" && var.slack_bot_token == ""
    error_message = "Set both Slack credentials in managed mode, or supply them only in the external Secret."
  }
}

variable "slack_default_channel" {
  type        = string
  default     = ""
  description = "Optional Slack channel ID for projects that name none."
}

variable "smtp_from" {
  type        = string
  default     = "The Runbook <login@example.com>"
  description = "Sender address for email sign-in codes."
}

variable "step_runs_limit" {
  type        = string
  default     = "off"
  description = "Most step runs one run may open (RUNBOOK_LIMIT_STEP_RUNS): a positive number, or 'off' for no cap."
}

variable "ssl_policy" {
  type        = string
  default     = ""
  description = "Optional name of a Compute Engine SSL policy (for example TLS 1.2 minimum, MODERN profile) for the HTTPS load balancer, set through the FrontendConfig."
}

variable "cloud_armor_policy" {
  type        = string
  default     = ""
  description = "Optional name of a Google Cloud Armor security policy to attach via BackendConfig."
}

variable "database_pool_size" {
  type        = number
  default     = 5
  description = "Number of pooled database connections per container replica."
}

variable "auth_database_pool_size" {
  type        = number
  default     = 2
  description = "Number of pooled database connections for the auth service per container replica."
}

variable "shutdown_timeout_secs" {
  type        = number
  default     = 8
  description = "Seconds allowed for graceful server shutdown after SIGTERM."
}

variable "resources" {
  type = object({
    requests = object({
      cpu    = string
      memory = string
    })
    limits = object({
      cpu    = string
      memory = string
    })
  })
  default = {
    requests = {
      cpu    = "500m"
      memory = "512Mi"
    }
    limits = {
      cpu    = "1000m"
      memory = "1Gi"
    }
  }
  description = "Compute resource requests and limits for the runbook application container."
}

# --- Blob Storage (GCS) Variables ---

variable "blob_store" {
  type        = string
  default     = "postgres"
  description = "Blob storage engine: 'postgres' (default, all blobs in PostgreSQL) or 'gcs' (Google Cloud Storage bucket)."
  validation {
    condition     = contains(["postgres", "gcs"], var.blob_store)
    error_message = "blob_store must be either 'postgres' or 'gcs'."
  }
}

variable "gcs_bucket_name" {
  type        = string
  default     = ""
  description = "Name for the GCS blob bucket (when blob_store is 'gcs'). If empty, defaults to '<project_id>-runbook-blobs'."
}

variable "gcs_bucket_location" {
  type        = string
  default     = ""
  description = "Location for the GCS bucket (e.g. US, us-central1). Defaults to var.region if unset."
}

variable "gcs_noncurrent_version_retention_days" {
  type        = number
  default     = 0
  description = "Optional lifecycle rule: delete noncurrent versions after N days (0 disables this rule)."
}

variable "blob_prefix" {
  type        = string
  default     = "runbook/"
  description = "Object key prefix for blobs stored in GCS."
}


variable "runner_enabled" {
  type        = bool
  default     = false
  description = "Provision runner IAM identities and its vault key. Runner workloads wait for the R2 Kubernetes sandbox backend."
}

variable "sql_api_deletion_protection" {
  type        = bool
  default     = true
  description = "Protect the Cloud SQL instance against deletion through the Cloud SQL API."
}

variable "database_auth" {
  type        = string
  default     = "password"
  description = "PostgreSQL authentication method: password or automatic IAM authentication through the proxy."

  validation {
    condition     = contains(["password", "iam"], var.database_auth)
    error_message = "database_auth must be password or iam."
  }
}

variable "sql_enable_pgaudit" {
  type        = bool
  default     = false
  description = "Enable the Cloud SQL pgAudit flag. Configure the extension and audit classes separately."
}

variable "manage_secret_values" {
  type        = bool
  default     = true
  description = "Generate and store credentials in Terraform state and manage the Kubernetes Secret. Set false to use an out-of-band Secret."
}

variable "existing_secret_name" {
  type        = string
  default     = "runbook-secrets"
  description = "Name of the out-of-band Secret in the runbook namespace when manage_secret_values is false."
}

variable "cloud_sql_proxy_image" {
  type = string
  # Cloud SQL Auth Proxy 2.28.0, multi-platform manifest.
  default     = "gcr.io/cloud-sql-connectors/cloud-sql-proxy@sha256:e0cbea7b63455c1635d354ef456696d019dd02a5db47bf1994e849823e71ae89"
  description = "Digest-pinned Cloud SQL Auth Proxy image for serving and migration pods."

  validation {
    condition     = can(regex("@sha256:[0-9a-f]{64}$", var.cloud_sql_proxy_image))
    error_message = "cloud_sql_proxy_image must end in a SHA-256 image digest."
  }
}

variable "network_policies" {
  type        = bool
  default     = true
  description = "Manage network policies. Deny namespace ingress except load balancer traffic to the serving port. Requires NetworkPolicy enforcement."
}

variable "restrict_egress" {
  type        = bool
  default     = false
  description = "Restrict serving and migration pod egress to DNS, metadata, private Cloud SQL and public IPv4 destinations."
}

variable "cloud_sql_private_cidr" {
  type        = string
  default     = null
  description = "Private Cloud SQL IPv4 range, preferably the instance IP as /32. Required when restrict_egress is true."

  validation {
    condition     = !(var.network_policies && var.restrict_egress) || can(cidrnetmask(var.cloud_sql_private_cidr))
    error_message = "Set cloud_sql_private_cidr to the Cloud SQL IPv4 CIDR when restrict_egress is true."
  }
}

variable "hsts_max_age" {
  type        = number
  default     = 31536000
  description = "Max-age in seconds for the load balancer's HSTS response header."

  validation {
    condition     = var.hsts_max_age >= 0 && floor(var.hsts_max_age) == var.hsts_max_age
    error_message = "hsts_max_age must be a nonnegative integer."
  }
}

variable "hsts_include_subdomains" {
  type        = bool
  default     = true
  description = "Add includeSubDomains to the load balancer's HSTS header."
}

variable "runner_kek_rotation_period" {
  description = "Rotation period for the runner vault's KMS key (KEK). New key versions wrap new data keys; old versions stay enabled for unwrapping."
  type        = string
  default     = "7776000s"
}

variable "runner_vault_vertex_enabled" {
  description = "Grant the runner vault's service account roles/aiplatform.user. Leave off until attached-identity model use is enabled in the runner."
  type        = bool
  default     = false
}
