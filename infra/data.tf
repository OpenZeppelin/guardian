# Get current AWS account ID
data "aws_caller_identity" "current" {}

# Get default VPC if vpc_id is not specified
data "aws_vpc" "default" {
  count   = var.vpc_id == "" ? 1 : 0
  default = true
}

# Get subnets in the VPC if subnet_ids is not specified
data "aws_subnets" "default" {
  count = length(var.subnet_ids) == 0 ? 1 : 0

  filter {
    name   = "vpc-id"
    values = [local.vpc_id]
  }
}

data "aws_vpc" "selected" {
  id = var.vpc_id != "" ? var.vpc_id : data.aws_vpc.default[0].id
}

locals {
  vpc_id     = var.vpc_id != "" ? var.vpc_id : data.aws_vpc.default[0].id
  subnet_ids = length(var.subnet_ids) > 0 ? var.subnet_ids : data.aws_subnets.default[0].ids
  # Use the first subnet for single-task services (postgres)
  primary_subnet_id = local.subnet_ids[0]
  vpc_cidr          = data.aws_vpc.selected.cidr_block

  # Service discovery DNS
  postgres_dns = "${var.postgres_service_name}.${var.sd_namespace_name}"

  # Database URL
  database_url = "postgres://${var.postgres_user}:${var.postgres_password}@${local.postgres_dns}:5432/${var.postgres_db}"

  # Custom domain configuration
  domain_enabled      = var.domain_name != ""
  service_fqdn        = var.domain_name == "" ? "" : (var.subdomain != "" ? "${var.subdomain}.${var.domain_name}" : var.domain_name)
  acm_certificate_arn = local.domain_enabled ? var.acm_certificate_arn : ""
}
