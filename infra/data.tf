# Get current AWS account ID
data "aws_caller_identity" "current" {}

data "aws_secretsmanager_secret" "ack_falcon" {
  count = var.deployment_stage == "prod" ? 1 : 0
  name  = local.ack_falcon_secret_name
}

data "aws_secretsmanager_secret" "ack_ecdsa" {
  count = var.deployment_stage == "prod" ? 1 : 0
  name  = local.ack_ecdsa_secret_name
}

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
  vpc_cidr   = data.aws_vpc.selected.cidr_block
  is_prod    = var.deployment_stage == "prod"
  stage_name = var.deployment_stage

  cluster_name                                 = var.cluster_name != "" ? var.cluster_name : "${var.stack_name}-cluster"
  server_service_name                          = var.server_service_name != "" ? var.server_service_name : "${var.stack_name}-server"
  alb_name                                     = var.alb_name != "" ? var.alb_name : "${var.stack_name}-alb"
  target_group_name                            = var.target_group_name != "" ? var.target_group_name : "${var.stack_name}-server-tg"
  grpc_target_group_name                       = "${var.stack_name}-grpc-tg"
  alb_security_group_name                      = var.alb_security_group_name != "" ? var.alb_security_group_name : "${var.stack_name}-alb-sg"
  server_security_group_name                   = var.server_security_group_name != "" ? var.server_security_group_name : "${var.stack_name}-server-sg"
  postgres_security_group_name                 = var.postgres_security_group_name != "" ? var.postgres_security_group_name : "${var.stack_name}-postgres-sg"
  task_execution_role_name                     = var.task_execution_role_name != "" ? var.task_execution_role_name : "${var.stack_name}-ecs-task-execution"
  task_role_name                               = var.task_role_name != "" ? var.task_role_name : "${var.stack_name}-ecs-task"
  server_task_family                           = var.server_task_family != "" ? var.server_task_family : "${var.stack_name}-server"
  server_container_name                        = var.server_container_name != "" ? var.server_container_name : "${var.stack_name}-server"
  server_log_group_name                        = var.server_log_group_name != "" ? var.server_log_group_name : "/ecs/${local.server_service_name}"
  cluster_log_group_name                       = "/aws/ecs/${local.cluster_name}/cluster"
  postgres_identifier_seed                     = lower(replace(var.stack_name, "/[^0-9A-Za-z]/", ""))
  postgres_identifier_base                     = local.postgres_identifier_seed != "" ? local.postgres_identifier_seed : "guardian"
  postgres_identifier_default                  = substr(can(regex("^[a-z]", local.postgres_identifier_base)) ? local.postgres_identifier_base : "g${local.postgres_identifier_base}", 0, 63)
  postgres_db                                  = var.postgres_db != "" ? var.postgres_db : local.postgres_identifier_default
  postgres_user                                = var.postgres_user != "" ? var.postgres_user : local.postgres_identifier_default
  postgres_password                            = var.postgres_password != "" ? var.postgres_password : "${var.stack_name}_dev_password"
  postgres_port                                = 5432
  rds_instance_identifier                      = "${var.stack_name}-postgres"
  rds_subnet_group_name                        = "${var.stack_name}-postgres-subnets"
  database_secret_name                         = "${var.stack_name}/server/database-url"
  database_credentials_secret_name             = "${var.stack_name}/server/database-credentials"
  ack_falcon_secret_name                       = "guardian-prod/server/ack-falcon-secret-key"
  ack_ecdsa_secret_name                        = "guardian-prod/server/ack-ecdsa-secret-key"
  rds_proxy_name                               = "${var.stack_name}-postgres-proxy"
  rds_proxy_role_name                          = "${var.stack_name}-rds-proxy"
  rds_proxy_security_group_name                = "${var.stack_name}-rds-proxy-sg"
  rds_master_password                          = var.postgres_password != "" ? var.postgres_password : random_password.postgres[0].result
  effective_rds_instance_class                 = var.rds_instance_class != "" ? var.rds_instance_class : (local.is_prod ? "db.t3.medium" : "db.t3.micro")
  effective_rds_allocated_storage              = var.rds_allocated_storage != null ? var.rds_allocated_storage : (local.is_prod ? 50 : 20)
  effective_server_desired_count               = var.server_desired_count != null ? var.server_desired_count : (local.is_prod ? 2 : 1)
  effective_server_autoscaling_enabled         = var.server_autoscaling_enabled != null ? var.server_autoscaling_enabled : local.is_prod
  effective_server_autoscaling_min_capacity    = var.server_autoscaling_min_capacity != null ? var.server_autoscaling_min_capacity : local.effective_server_desired_count
  effective_server_autoscaling_max_capacity    = var.server_autoscaling_max_capacity != null ? var.server_autoscaling_max_capacity : (local.is_prod ? max(local.effective_server_desired_count, 6) : local.effective_server_desired_count)
  effective_server_autoscaling_cpu_target      = var.server_autoscaling_cpu_target != null ? var.server_autoscaling_cpu_target : 65
  effective_server_autoscaling_memory_target   = var.server_autoscaling_memory_target != null ? var.server_autoscaling_memory_target : 75
  effective_rds_proxy_enabled                  = var.rds_proxy_enabled != null ? var.rds_proxy_enabled : local.is_prod
  effective_rds_max_allocated_storage          = var.rds_max_allocated_storage != null ? var.rds_max_allocated_storage : (local.is_prod ? max(local.effective_rds_allocated_storage, 200) : null)
  effective_guardian_rate_burst_per_sec        = var.guardian_rate_burst_per_sec != null ? var.guardian_rate_burst_per_sec : (local.is_prod ? 200 : 10)
  effective_guardian_rate_per_min              = var.guardian_rate_per_min != null ? var.guardian_rate_per_min : (local.is_prod ? 5000 : 60)
  effective_guardian_db_pool_max_size          = var.guardian_db_pool_max_size != null ? var.guardian_db_pool_max_size : (local.is_prod ? 32 : 16)
  effective_guardian_metadata_db_pool_max_size = var.guardian_metadata_db_pool_max_size != null ? var.guardian_metadata_db_pool_max_size : local.effective_guardian_db_pool_max_size

  direct_database_endpoint = aws_db_instance.postgres.address
  database_proxy_endpoint  = local.effective_rds_proxy_enabled ? aws_db_proxy.postgres[0].endpoint : ""
  database_endpoint        = local.effective_rds_proxy_enabled ? local.database_proxy_endpoint : local.direct_database_endpoint

  database_url = "postgres://${urlencode(local.postgres_user)}:${urlencode(local.rds_master_password)}@${local.database_endpoint}:${local.postgres_port}/${local.postgres_db}?sslmode=require"

  # Custom domain configuration
  domain_enabled      = var.domain_name != ""
  service_fqdn        = var.domain_name == "" ? "" : (var.subdomain != "" ? "${var.subdomain}.${var.domain_name}" : var.domain_name)
  acm_certificate_arn = local.domain_enabled ? var.acm_certificate_arn : ""
}
