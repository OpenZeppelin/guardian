output "alb_dns_name" {
  description = "ALB DNS name for accessing the server"
  value       = aws_lb.main.dns_name
}

output "alb_url" {
  description = "Full URL for accessing the server"
  value       = local.acm_certificate_arn != "" ? "https://${aws_lb.main.dns_name}" : "http://${aws_lb.main.dns_name}"
}

output "custom_domain_url" {
  description = "Custom domain URL when configured"
  value       = local.domain_enabled ? "https://${local.service_fqdn}" : ""
}

output "grpc_endpoint" {
  description = "Public gRPC endpoint when HTTPS is enabled"
  value = local.acm_certificate_arn != "" ? (
    local.domain_enabled ? "https://${local.service_fqdn}" : "https://${aws_lb.main.dns_name}"
  ) : ""
}

output "database_endpoint" {
  description = "Database endpoint used by the Guardian server"
  value       = local.database_endpoint
}

output "direct_database_endpoint" {
  description = "Direct RDS instance endpoint"
  value       = local.direct_database_endpoint
}

output "rds_proxy_endpoint" {
  description = "RDS Proxy endpoint when enabled"
  value       = local.database_proxy_endpoint
}

output "rds_proxy_enabled" {
  description = "Whether RDS Proxy is enabled"
  value       = local.effective_rds_proxy_enabled
}

output "rds_proxy_route_database_url" {
  description = "Whether the DATABASE_URL secret points at the RDS Proxy endpoint"
  value       = local.effective_rds_proxy_route_database_url
}

output "rds_proxy_subnet_ids" {
  description = "Effective subnet IDs used for RDS Proxy placement"
  value       = local.effective_rds_proxy_subnet_ids
}

output "rds_max_allocated_storage" {
  description = "Configured maximum allocated RDS storage for storage autoscaling"
  value       = local.effective_rds_max_allocated_storage
}

output "rds_instance_class" {
  description = "Effective RDS instance class"
  value       = local.effective_rds_instance_class
}

output "rds_allocated_storage" {
  description = "Effective allocated RDS storage in GiB"
  value       = local.effective_rds_allocated_storage
}

output "database_url_secret_arn" {
  description = "Secrets Manager ARN for the server database URL"
  value       = aws_secretsmanager_secret.database_url.arn
}

output "operator_public_keys_secret_arn" {
  description = "Secrets Manager ARN used by the server for dashboard operator public keys"
  value       = local.operator_public_keys_secret_arn
}

output "operator_public_keys_secret_name" {
  description = "Managed Secrets Manager name for dashboard operator public keys when Terraform creates it"
  value       = local.managed_operator_public_keys_secret_enabled ? local.operator_public_keys_secret_name : ""
}

output "guardian_evm_allowed_chain_ids_secret_arn" {
  description = "Secrets Manager ARN used by the server for EVM allowed chain IDs"
  value       = local.evm_allowed_chain_ids_secret_arn
  sensitive   = true
}

output "guardian_evm_allowed_chain_ids_secret_name" {
  description = "Managed Secrets Manager name for EVM allowed chain IDs when Terraform creates it"
  value       = local.managed_evm_allowed_chain_ids_secret_enabled ? local.evm_allowed_chain_ids_secret_name : ""
  sensitive   = true
}

output "guardian_evm_rpc_urls_secret_arn" {
  description = "Secrets Manager ARN used by the server for EVM RPC URLs"
  value       = local.evm_rpc_urls_secret_arn
  sensitive   = true
}

output "guardian_evm_rpc_urls_secret_name" {
  description = "Managed Secrets Manager name for EVM RPC URLs when Terraform creates it"
  value       = local.managed_evm_rpc_urls_secret_enabled ? local.evm_rpc_urls_secret_name : ""
  sensitive   = true
}

output "guardian_evm_entrypoint_address" {
  description = "Shared EVM EntryPoint address configured for the server"
  value       = var.guardian_evm_entrypoint_address
}

output "guardian_cors_allowed_origins" {
  description = "Explicit CORS origins configured for the server"
  value       = var.guardian_cors_allowed_origins
}

output "ack_falcon_secret_name" {
  description = "Secrets Manager name for the Falcon ack key"
  value       = local.ack_falcon_secret_name
}

output "ack_ecdsa_secret_name" {
  description = "Secrets Manager name for the ECDSA ack key"
  value       = local.ack_ecdsa_secret_name
}

output "deployment_stage" {
  description = "Active deployment stage"
  value       = local.stage_name
}

output "server_desired_count" {
  description = "Configured ECS service desired task count"
  value       = local.effective_server_desired_count
}

output "server_cpu" {
  description = "Configured ECS task CPU units"
  value       = var.server_cpu
}

output "server_memory" {
  description = "Configured ECS task memory in MiB"
  value       = var.server_memory
}

output "server_autoscaling_enabled" {
  description = "Whether ECS service autoscaling is enabled"
  value       = local.effective_server_autoscaling_enabled
}

output "server_autoscaling_min_capacity" {
  description = "Configured ECS service autoscaling minimum task count"
  value       = local.effective_server_autoscaling_min_capacity
}

output "server_autoscaling_max_capacity" {
  description = "Configured ECS service autoscaling maximum task count"
  value       = local.effective_server_autoscaling_max_capacity
}

output "guardian_rate_burst_per_sec" {
  description = "Effective Guardian HTTP burst rate limit"
  value       = local.effective_guardian_rate_burst_per_sec
}

output "guardian_rate_limit_enabled" {
  description = "Whether Guardian HTTP rate limiting is enabled"
  value       = local.effective_guardian_rate_limit_enabled
}

output "guardian_rate_per_min" {
  description = "Effective Guardian HTTP sustained rate limit"
  value       = local.effective_guardian_rate_per_min
}

output "guardian_db_pool_max_size" {
  description = "Effective Guardian storage DB pool maximum size"
  value       = local.effective_guardian_db_pool_max_size
}

output "guardian_metadata_db_pool_max_size" {
  description = "Effective Guardian metadata DB pool maximum size"
  value       = local.effective_guardian_metadata_db_pool_max_size
}

output "ecs_cluster_arn" {
  description = "ECS cluster ARN"
  value       = aws_ecs_cluster.main.arn
}

output "ecs_cluster_name" {
  description = "ECS cluster name"
  value       = aws_ecs_cluster.main.name
}

output "server_service_arn" {
  description = "Server ECS service ARN"
  value       = aws_ecs_service.server.id
}

output "server_service_name" {
  description = "Server ECS service name"
  value       = aws_ecs_service.server.name
}

output "server_log_group" {
  description = "CloudWatch log group for server"
  value       = aws_cloudwatch_log_group.server.name
}

output "cluster_log_group" {
  description = "CloudWatch log group for ECS execute command"
  value       = aws_cloudwatch_log_group.cluster.name
}
