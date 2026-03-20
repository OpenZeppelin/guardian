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

  cluster_name                 = var.cluster_name != "" ? var.cluster_name : "${var.stack_name}-cluster"
  server_service_name          = var.server_service_name != "" ? var.server_service_name : "${var.stack_name}-server"
  postgres_service_name        = var.postgres_service_name != "" ? var.postgres_service_name : "${var.stack_name}-postgres"
  alb_name                     = var.alb_name != "" ? var.alb_name : "${var.stack_name}-alb"
  sd_namespace_name            = var.sd_namespace_name != "" ? var.sd_namespace_name : "${var.stack_name}.local"
  target_group_name            = var.target_group_name != "" ? var.target_group_name : "${var.stack_name}-server-tg"
  alb_security_group_name      = var.alb_security_group_name != "" ? var.alb_security_group_name : "${var.stack_name}-alb-sg"
  server_security_group_name   = var.server_security_group_name != "" ? var.server_security_group_name : "${var.stack_name}-server-sg"
  postgres_security_group_name = var.postgres_security_group_name != "" ? var.postgres_security_group_name : "${var.stack_name}-postgres-sg"
  task_execution_role_name     = var.task_execution_role_name != "" ? var.task_execution_role_name : "${var.stack_name}-ecs-task-execution"
  task_role_name               = var.task_role_name != "" ? var.task_role_name : "${var.stack_name}-ecs-task"
  server_task_family           = var.server_task_family != "" ? var.server_task_family : "${var.stack_name}-server"
  postgres_task_family         = var.postgres_task_family != "" ? var.postgres_task_family : "${var.stack_name}-postgres"
  server_container_name        = var.server_container_name != "" ? var.server_container_name : "${var.stack_name}-server"
  server_log_group_name        = var.server_log_group_name != "" ? var.server_log_group_name : "/ecs/${local.server_service_name}"
  postgres_log_group_name      = var.postgres_log_group_name != "" ? var.postgres_log_group_name : "/ecs/${local.postgres_service_name}"
  cluster_log_group_name       = "/aws/ecs/${local.cluster_name}/cluster"
  postgres_db                  = var.postgres_db != "" ? var.postgres_db : var.stack_name
  postgres_user                = var.postgres_user != "" ? var.postgres_user : var.stack_name
  postgres_password            = var.postgres_password != "" ? var.postgres_password : "${var.stack_name}_dev_password"

  # Service discovery DNS
  postgres_dns = "${local.postgres_service_name}.${local.sd_namespace_name}"

  # Database URL
  database_url = "postgres://${local.postgres_user}:${local.postgres_password}@${local.postgres_dns}:5432/${local.postgres_db}"

  # Custom domain configuration
  domain_enabled      = var.domain_name != ""
  service_fqdn        = var.domain_name == "" ? "" : (var.subdomain != "" ? "${var.subdomain}.${var.domain_name}" : var.domain_name)
  acm_certificate_arn = local.domain_enabled ? var.acm_certificate_arn : ""
}
