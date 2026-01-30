# Cloud Map namespace for service discovery
resource "aws_service_discovery_private_dns_namespace" "main" {
  name        = var.sd_namespace_name
  vpc         = local.vpc_id
  description = "PSM service discovery namespace"
}

# Cloud Map service for Postgres
resource "aws_service_discovery_service" "postgres" {
  name = var.postgres_service_name

  dns_config {
    namespace_id = aws_service_discovery_private_dns_namespace.main.id

    dns_records {
      type = "A"
      ttl  = 10
    }

    routing_policy = "MULTIVALUE"
  }

  health_check_custom_config {
    failure_threshold = 1
  }
}
