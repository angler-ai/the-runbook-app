terraform {
  required_version = ">= 1.9.0, < 2.0.0"

  required_providers {
    external = {
      source  = "hashicorp/external"
      version = "~> 2.3"
    }
    google = {
      source  = "hashicorp/google"
      version = ">= 7.20, < 8.0"
    }
    google-beta = {
      source  = "hashicorp/google-beta"
      version = ">= 7.20, < 8.0"
    }
    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = "~> 2.30"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.6"
    }
  }

  # Remote state backend:
  # To use Google Cloud Storage (GCS) for remote state management, uncomment
  # and configure the following block, or provide backend configuration via
  # `terraform init -backend-config="bucket=<bucket-name>"`:
  #
  # backend "gcs" {
  #   bucket = "my-company-terraform-state"
  #   prefix = "runbook/gcp-gke"
  # }
}
