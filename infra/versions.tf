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

provider "cloudflare" {
  api_token = var.cloudflare_api_token
}
