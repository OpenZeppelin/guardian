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

output "postgres_service_arn" {
  description = "Postgres ECS service ARN"
  value       = aws_ecs_service.postgres.id
}

output "postgres_service_name" {
  description = "Postgres ECS service name"
  value       = aws_ecs_service.postgres.name
}

output "server_log_group" {
  description = "CloudWatch log group for server"
  value       = aws_cloudwatch_log_group.server.name
}

output "cluster_log_group" {
  description = "CloudWatch log group for ECS execute command"
  value       = aws_cloudwatch_log_group.cluster.name
}

output "postgres_log_group" {
  description = "CloudWatch log group for postgres"
  value       = aws_cloudwatch_log_group.postgres.name
}
