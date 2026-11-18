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
  description = "Container image to deploy, pinned to an X.Y.Z tag and optionally its digest (e.g. ghcr.io/angler-ai/runbook:0.1.0)."

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
  description = "Google OAuth web client secret, with google_client_id. Kept in Secret Manager and the workload's Secret."

  validation {
    condition     = (var.google_client_secret == "") == (var.google_client_id == "")
    error_message = "Set google_client_id and google_client_secret together, or neither."
  }
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

