# CloudWatch log group for ECS execute command
resource "aws_cloudwatch_log_group" "cluster" {
  name              = "/aws/ecs/${var.cluster_name}/cluster"
  retention_in_days = var.log_retention_days
}

# CloudWatch log groups for ECS tasks

resource "aws_cloudwatch_log_group" "server" {
  name              = "/ecs/psm-server"
  retention_in_days = var.log_retention_days
}

resource "aws_cloudwatch_log_group" "postgres" {
  name              = "/ecs/psm-postgres"
  retention_in_days = var.log_retention_days
}
