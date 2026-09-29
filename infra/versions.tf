terraform {
  # 1.12+ short-circuits && and ||, which the null-default variable
  # validations and the conditional data-source counts rely on; older
  # releases fail the plan with "argument must not be null" / "Invalid index".
  required_version = ">= 1.12.0"

  required_providers {
    aws = {
      source = "hashicorp/aws"
      # >= 5.61 for aws_chatbot_slack_channel_configuration (alerting.tf);
      # existing checkouts need `terraform -chdir=infra init -upgrade`.
      version = "~> 5.61"
    }
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = ">= 4.0.0"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.6"
    }
  }
}

provider "aws" {
  region = var.aws_region

  default_tags {
    tags = {
      Project   = "guardian"
      ManagedBy = "terraform"
    }
  }
}

# Root-account provider used only for the GitHub OIDC bootstrap role
# (infra/oidc.tf). It reaches the root account either through a named profile
# or by assuming a role from the caller's credentials; with neither set it
# falls back to the caller's own credentials and nothing references it.
provider "aws" {
  alias   = "root_account"
  region  = var.aws_region
  profile = var.github_oidc_root_account_profile != "" ? var.github_oidc_root_account_profile : null

  dynamic "assume_role" {
    for_each = var.github_oidc_root_account_role_arn != "" ? [1] : []

    content {
      role_arn = var.github_oidc_root_account_role_arn
    }
  }

  default_tags {
    tags = {
      Project   = "guardian"
      ManagedBy = "terraform"
    }
  }
}

provider "cloudflare" {
  api_token = var.cloudflare_api_token
}
