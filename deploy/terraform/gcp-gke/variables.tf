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
  description = "Provision the runner and its gVisor sandbox namespace, with per-process identity and networking."
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
  description = "Create a dedicated Vertex inference service account that the runner vault can impersonate."
  type        = bool
  default     = false
}

variable "runner_google_api_dns" {
  type        = string
  default     = "vpc"
  description = "How runner processes reach Google APIs through the restricted VIP. \"vpc\" adds a private googleapis.com zone to the whole VPC. \"pods\" maps the Google API hostnames the runner uses to the VIP inside runner pods only, for a VPC shared with other workloads."
  validation {
    condition     = contains(["vpc", "pods"], var.runner_google_api_dns)
    error_message = "runner_google_api_dns must be \"vpc\" or \"pods\"."
  }
}

variable "runner_vertex_targets" {
  description = "Map of Vertex project IDs to allowed locations (global or regional). Enable Vertex AI and billing in each target project."
  type        = map(set(string))
  default     = {}

  validation {
    condition = alltrue([
      for project, locations in var.runner_vertex_targets :
      can(regex("^[a-z][a-z0-9-]{4,28}[a-z0-9]$", project)) && length(locations) > 0 &&
      alltrue([for location in locations : length(location) <= 40 && can(regex("^(global|[a-z]+-[a-z]+[1-9][0-9]*)$", location))])
    ])
    error_message = "runner_vertex_targets must map valid project IDs to non-empty sets of valid Vertex locations."
  }
}

variable "runner_image" {
  type        = string
  default     = ""
  description = "Digest reference for ghcr.io/angler-ai/runbook-runner, or a mirror of it by the same digest."
  validation {
    condition     = !var.runner_enabled || can(regex("^[a-z0-9][a-z0-9._:/-]*/(runbook-runner|${var.name_prefix}runbook-runner)@sha256:[0-9a-f]{64}$", var.runner_image))
    error_message = "When enabled, pin runbook-runner by digest."
  }
}

variable "runner_sandbox_image" {
  type        = string
  default     = ""
  description = "Digest reference for ghcr.io/angler-ai/runbook-sandbox, or a mirror of it by the same digest."
  validation {
    condition     = !var.runner_enabled || can(regex("^[a-z0-9][a-z0-9._:/-]*/(runbook-sandbox|${var.name_prefix}runbook-sandbox)@sha256:[0-9a-f]{64}$", var.runner_sandbox_image))
    error_message = "When enabled, pin runbook-sandbox by digest."
  }
}

variable "runner_secret_versions" {
  type        = map(string)
  default     = {}
  description = "Per-process Secret Manager references, in projects/PROJECT/secrets/SECRET/versions/VERSION form."
  validation {
    condition = !var.runner_enabled || (!var.runner_database_bootstrap_ready && length(var.runner_secret_versions) == 0) || (
      toset(keys(var.runner_secret_versions)) == toset(["api", "vault", "gateway", "worker", "sandbox-ctl"]) &&
      alltrue([for value in values(var.runner_secret_versions) : can(regex("^projects/[^/]+/secrets/[^/]+/versions/[1-9][0-9]*$", value))]) &&
      length(distinct([for value in values(var.runner_secret_versions) : split("/versions/", value)[0]])) == 5
    )
    error_message = "Provide a distinct Secret Manager secret with a numeric version for each process."
  }
}

variable "runner_install" {
  type = object({
    id          = string
    org_id      = string
    group_id    = string
    runner_id   = string
    role_prefix = string
  })
  default     = null
  description = "IDs from one-time runner registration and a unique PostgreSQL role prefix for the installation."
  validation {
    condition = !var.runner_enabled || (!var.runner_database_bootstrap_ready && var.runner_install == null) || try(
      alltrue([for id in [var.runner_install.id, var.runner_install.org_id, var.runner_install.group_id, var.runner_install.runner_id] : can(regex("^[A-Za-z0-9_-]+$", id))]) &&
      can(regex("^[a-z][a-z0-9_]{2,29}$", var.runner_install.role_prefix)), false
    )
    error_message = "Provide runner registration IDs and a PostgreSQL role prefix of 3 to 30 lowercase letters, digits or underscores."
  }
}

variable "runner_max_sandbox_pods" {
  type        = number
  default     = 20
  description = "Sandbox namespace pod quota."
  validation {
    condition     = floor(var.runner_max_sandbox_pods) == var.runner_max_sandbox_pods && var.runner_max_sandbox_pods >= 1 && var.runner_max_sandbox_pods <= 1000
    error_message = "Set an integer quota from 1 to 1000."
  }
}

variable "runner_worker_capacity" {
  type        = number
  default     = 1
  description = "Concurrent claims for the runner-worker Deployment."
  validation {
    condition     = floor(var.runner_worker_capacity) == var.runner_worker_capacity && var.runner_worker_capacity >= 1 && var.runner_worker_capacity <= var.runner_max_sandbox_pods
    error_message = "Set an integer capacity from 1 to runner_max_sandbox_pods."
  }
}

variable "runner_sql_max_connections" {
  type        = number
  default     = null
  description = "Override Cloud SQL max_connections when the runner is enabled. By default the budget uses the tier limit of 25 on db-f1-micro, 50 on db-g1-small or 100 on db-custom-1-3840. Set it explicitly for other tiers."
  validation {
    condition     = var.runner_sql_max_connections == null ? true : floor(var.runner_sql_max_connections) == var.runner_sql_max_connections && var.runner_sql_max_connections >= 14
    error_message = "Set an integer max_connections of at least 14."
  }
}

variable "runner_database_pool_size" {
  type        = number
  default     = 4
  description = "Upper bound per runner pool (API, scheduler, vault). The shared instance budget may require a smaller pool."
  validation {
    condition     = floor(var.runner_database_pool_size) == var.runner_database_pool_size && var.runner_database_pool_size >= 1 && var.runner_database_pool_size <= 10
    error_message = "Set an integer runner pool size from 1 to 10."
  }
}

variable "runner_database_bootstrap_ready" {
  type        = bool
  default     = false
  description = "Enable the runner migration Job and Deployments after database ownership and core CONNECT grants are prepared."
}

variable "runner_dns_ip" {
  type        = string
  default     = "169.254.20.10"
  description = "NodeLocal DNS IPv4 address on the target GKE cluster."
  validation {
    condition     = can(cidrnetmask("${var.runner_dns_ip}/32"))
    error_message = "Use an IPv4 address."
  }
}

variable "name_prefix" {
  type        = string
  default     = ""
  description = "Prefix for names created by this installation. Leave empty for existing installations."

  validation {
    condition     = var.name_prefix == "" || can(regex("^[a-z][a-z0-9-]{0,19}-$", var.name_prefix))
    error_message = "Use an empty prefix or up to 21 lowercase letters, digits and hyphens, starting with a letter and ending with a hyphen."
  }
}

variable "labels" {
  type        = map(string)
  default     = {}
  description = "Additional labels on supported resources. Built-in Kubernetes labels take precedence."
}

variable "release_channel" {
  type        = string
  default     = null
  description = "Release channel for a new Autopilot cluster. Null leaves channel selection to GKE (normally REGULAR)."

  validation {
    condition     = var.release_channel == null ? true : contains(["RAPID", "REGULAR", "STABLE", "EXTENDED"], var.release_channel)
    error_message = "Use RAPID, REGULAR, STABLE, EXTENDED, or null."
  }
}

variable "deletion_protection" {
  type        = bool
  default     = true
  description = "Protect a module-created GKE cluster against Terraform deletion."
}

variable "sql_final_backup" {
  type        = bool
  default     = true
  description = "Take a final Cloud SQL backup on instance deletion."
}

variable "sql_final_backup_retention_days" {
  type        = number
  default     = 30
  description = "Retention period for the final Cloud SQL backup."

  validation {
    condition     = var.sql_final_backup_retention_days >= 1 && var.sql_final_backup_retention_days <= 365 && floor(var.sql_final_backup_retention_days) == var.sql_final_backup_retention_days
    error_message = "Use a whole number of days from 1 through 365."
  }
}

variable "sql_retain_backups_on_delete" {
  type        = bool
  default     = true
  description = "Retain existing Cloud SQL backups after deletion."
}

variable "kms_prevent_destroy" {
  type        = bool
  default     = true
  description = "Keep the vault key protected. Set false only when creating a new, prefixed, disposable installation."

  validation {
    condition     = var.kms_prevent_destroy || var.name_prefix != ""
    error_message = "Disabling KMS protection requires a nonempty name_prefix."
  }
}

variable "ingress_enabled" {
  type        = bool
  default     = true
  description = "Create the public GKE Ingress, reserved external IP, and managed certificate."
}

variable "runner_sandbox_images" {
  description = "Additional approved sandbox images as full registry/repository@sha256 digest references. The installation sandbox image is always included."
  type        = list(string)
  default     = []

  validation {
    condition = length(var.runner_sandbox_images) <= 64 && alltrue([
      for image in var.runner_sandbox_images :
      can(regex("^([a-z0-9.-]+)(:[0-9]+)?/[a-z0-9][a-z0-9._/-]*@sha256:[0-9a-f]{64}$", image))
    ])
    error_message = "Specify at most 64 full sandbox image references pinned by SHA-256 digest."
  }
}

variable "runner_registry_https_cidrs" {
  description = "Optional IPv4 CIDRs for runner-api to resolve image tags over HTTPS, including any anonymous token endpoint. Fully pinned images need no registry lookup."
  type        = set(string)
  default     = []

  validation {
    condition     = alltrue([for cidr in var.runner_registry_https_cidrs : can(cidrnetmask(cidr)) && !endswith(cidr, "/0")])
    error_message = "Use specific IPv4 CIDRs for the registry HTTPS endpoints."
  }
}

variable "runner_secret_manager_secret_ids" {
  description = "Secret Manager release policy entries for secrets in this project, with their allowed credential types, exact destinations, and optional version."
  type = list(object({
    resource     = string
    version      = optional(string)
    types        = set(string)
    destinations = set(string)
  }))
  default = []

  validation {
    condition = alltrue([
      for entry in var.runner_secret_manager_secret_ids :
      startswith(entry.resource, "projects/${var.project_id}/secrets/") &&
      can(regex("^projects/[^/]+/secrets/[A-Za-z0-9_-]{1,255}$", entry.resource)) &&
      (entry.version == null || can(regex("^(latest|[1-9][0-9]{0,18})$", entry.version))) &&
      length(entry.types) > 0 &&
      length(setsubtract(entry.types, toset(["api-key", "mcp-bearer", "model-provider"]))) == 0 &&
      length(entry.destinations) > 0 &&
      alltrue([for destination in entry.destinations : try(trimspace(destination) != "", false)])
    ])
    error_message = "Specify each secret's full resource name in the deployment project, an optional version (latest or a positive integer), and nonempty supported types and destinations."
  }

  validation {
    # The startup resource can identify the same project by number or by ID.
    condition = length(setintersection(
      toset([for entry in var.runner_secret_manager_secret_ids : basename(entry.resource)]),
      toset([for version in values(var.runner_secret_versions) : try(split("/", version)[3], "")])
    )) == 0
    error_message = "Runner startup secrets cannot appear in runner_secret_manager_secret_ids."
  }
}

variable "manage_secret_manager_audit_config" {
  description = "Opt in to replacing this project's Secret Manager service audit configuration with Data Read and Data Write logging. Leave false to manage audit policy separately."
  type        = bool
  default     = false
}

variable "retention_live_days" {
  type        = string
  default     = "off"
  description = "Core finished-run detail live window in days, or off. Sets RUNBOOK_RETENTION_LIVE_DAYS."
  validation {
    condition     = var.retention_live_days == "off" || try(can(regex("^[0-9]{1,5}$", var.retention_live_days)) && tonumber(var.retention_live_days) >= 1 && tonumber(var.retention_live_days) <= 36500, false)
    error_message = "retention_live_days must be a whole number from 1 to 36500, or off."
  }
}

variable "trace_export_enabled" {
  description = "Enable direct OTLP trace export IAM and networking for the core and enabled runner workloads."
  type        = bool
  default     = false
}

variable "trace_sample_ratio" {
  description = "Fraction of new traces sampled by Runbook. Requests with a parent follow its sampling decision."
  type        = number
  default     = 0.1

  validation {
    condition     = var.trace_sample_ratio >= 0 && var.trace_sample_ratio <= 1
    error_message = "trace_sample_ratio must be between 0 and 1."
  }
}

variable "log_exclusions_enabled" {
  description = "Exclude Runbook container debug and health-check logs from the project's default log bucket."
  type        = bool
  default     = false
}
