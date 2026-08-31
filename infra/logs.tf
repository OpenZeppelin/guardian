# CloudWatch log group for ECS execute command
resource "aws_cloudwatch_log_group" "cluster" {
  name              = local.cluster_log_group_name
  retention_in_days = var.log_retention_days
}

# CloudWatch log groups for ECS tasks

resource "aws_cloudwatch_log_group" "server" {
  name              = local.server_log_group_name
  retention_in_days = var.log_retention_days
}

# Log group the ADOT sidecar's awsemf exporter writes EMF metric events
# into. Pre-created so retention is Terraform-managed rather than the
# exporter's default (never expire).
resource "aws_cloudwatch_log_group" "emf" {
  count = var.metrics_enabled ? 1 : 0

  name              = local.emf_log_group_name
  retention_in_days = var.log_retention_days
}
