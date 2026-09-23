output "alb_dns_name" {
  description = "ALB DNS name for accessing the server"
  value       = aws_lb.main.dns_name
}

output "alb_url" {
  description = "Full URL for accessing the server"
  value       = local.acm_certificate_arn != "" ? "https://${aws_lb.main.dns_name}" : "http://${aws_lb.main.dns_name}"
}

output "custom_domain_url" {
  description = "Canonical service URL: https when a certificate is configured, http when Terraform manages only the DNS record"
  value = !local.domain_enabled ? "" : (
    local.acm_certificate_arn != "" ? "https://${local.service_fqdn}" : (
      local.route53_zone_id != "" || var.cloudflare_zone_id != "" ? "http://${local.service_fqdn}" : ""
    )
  )
}

output "alias_domain_url" {
  description = "Migration-only legacy domain URL"
  value       = local.alias_domain_enabled ? "https://${local.alias_service_fqdn}" : ""
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

output "storage_encryption_secret_name" {
  description = "Secrets Manager name for the storage encryption key (empty when encryption is disabled)"
  value       = local.storage_encryption_secret_name
}

output "dashboard_cursor_secret_name" {
  description = "Secrets Manager name for the shared dashboard pagination cursor secret"
  value       = local.dashboard_cursor_secret_name
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
  description = "Effective Guardian burst rate limit (HTTP and gRPC)"
  value       = local.effective_guardian_rate_burst_per_sec
}

output "guardian_rate_limit_enabled" {
  description = "Whether Guardian rate limiting is enabled (HTTP and gRPC)"
  value       = local.effective_guardian_rate_limit_enabled
}

output "guardian_rate_per_min" {
  description = "Effective Guardian sustained rate limit (HTTP and gRPC)"
  value       = local.effective_guardian_rate_per_min
}

output "guardian_max_replicas" {
  description = "Effective GUARDIAN_MAX_REPLICAS rate-limit divisor after clamping to the steady-state ECS capacity"
  value       = local.effective_guardian_max_replicas
}

output "guardian_dashboard_commitment_rate_burst_per_sec" {
  description = "Effective fleet-wide dashboard per-commitment burst rate limit"
  value       = local.dashboard_rate_burst_per_sec
}

output "guardian_dashboard_commitment_rate_per_min" {
  description = "Effective fleet-wide dashboard per-commitment sustained rate limit"
  value       = local.dashboard_rate_per_min
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

output "guardian_metrics_enabled" {
  description = "Whether the Guardian Prometheus metrics endpoint is enabled in the ECS task"
  value       = var.guardian_metrics_enabled
}

output "cloudwatch_metrics_enabled" {
  description = "Whether the ADOT metrics sidecar, dashboard, and metric-based alarms are deployed (cascades off with the metrics endpoint; the log-based alarm is gated separately)"
  value       = local.cloudwatch_metrics_enabled
}

output "metrics_namespace" {
  description = "CloudWatch namespace receiving Guardian application metrics"
  value       = local.cloudwatch_metrics_enabled ? local.metrics_namespace : ""
}

output "metrics_dashboard_name" {
  description = "CloudWatch dashboard name for the Guardian server"
  value       = local.cloudwatch_metrics_enabled ? aws_cloudwatch_dashboard.server[0].dashboard_name : ""
}

output "metrics_emf_log_group" {
  description = "CloudWatch log group the ADOT sidecar writes EMF metric events into"
  value       = local.cloudwatch_metrics_enabled ? aws_cloudwatch_log_group.emf[0].name : ""
}

output "metrics_missing_alarm_name" {
  description = "Name of the metrics-pipeline heartbeat alarm for this stack"
  value       = local.cloudwatch_metrics_enabled ? aws_cloudwatch_metric_alarm.metrics_missing[0].alarm_name : ""
}

output "alarm_actions" {
  description = "Effective ARNs every Guardian alarm notifies on alarm/ok transitions (operator alarm_actions plus the managed topic when enabled)"
  value       = local.effective_alarm_actions
}

output "alarm_sns_topic_arn" {
  description = "ARN of the Terraform-managed alarm SNS topic, empty when alarm_notifications_enabled is false"
  value       = local.alarm_sns_topic_arn
}

output "alarm_slack_configuration_name" {
  description = "Name of the Amazon Q Developer in chat applications Slack channel configuration (its error log group is /aws/chatbot/<name> in us-east-1), empty when not configured"
  value       = local.alarm_slack_enabled ? local.alarm_slack_configuration_name : ""
}

output "alarm_slack_configuration_arn" {
  description = "ARN of the Amazon Q Developer in chat applications Slack channel configuration for this stack's alarms, empty when not configured"
  value       = local.alarm_slack_enabled ? aws_chatbot_slack_channel_configuration.alarms[0].chat_configuration_arn : ""
}

output "cloudwatch_log_alarms_enabled" {
  description = "Whether the server log group's ERROR metric filter and log-errors alarm are deployed (the WARN filter additionally needs the dashboard)"
  value       = var.cloudwatch_log_alarms_enabled
}

output "log_metrics_namespace" {
  description = "CloudWatch namespace receiving the server log-level metric-filter counts (<metrics_namespace>/Logs)"
  value       = var.cloudwatch_log_alarms_enabled ? local.log_metrics_namespace : ""
}

output "server_log_errors_alarm_name" {
  description = "Name of the alarm on ERROR-level server log lines for this stack"
  value       = var.cloudwatch_log_alarms_enabled ? aws_cloudwatch_metric_alarm.server_log_errors[0].alarm_name : ""
}
