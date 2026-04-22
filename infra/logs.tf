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
