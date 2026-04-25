# IAM role for ECS task execution
resource "aws_iam_role" "ecs_task_execution" {
  name = local.task_execution_role_name

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Principal = {
          Service = "ecs-tasks.amazonaws.com"
        }
        Action = "sts:AssumeRole"
      }
    ]
  })
}

resource "aws_iam_role_policy_attachment" "ecs_task_execution" {
  role       = aws_iam_role.ecs_task_execution.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}

resource "aws_iam_role_policy" "ecs_task_execution_database_secret" {
  name = "${var.stack_name}-ecs-task-execution-database-secret"
  role = aws_iam_role.ecs_task_execution.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "secretsmanager:GetSecretValue"
        ]
        Resource = [
          aws_secretsmanager_secret.database_url.arn
        ]
      }
    ]
  })
}

# IAM role for ECS tasks (runtime)
resource "aws_iam_role" "ecs_task" {
  name = local.task_role_name

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Principal = {
          Service = "ecs-tasks.amazonaws.com"
        }
        Action = "sts:AssumeRole"
      }
    ]
  })
}

resource "aws_iam_role_policy" "ecs_task_ack_secrets" {
  count = local.is_prod ? 1 : 0

  name = "${var.stack_name}-ecs-task-ack-secrets"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "secretsmanager:GetSecretValue"
        ]
        Resource = [
          data.aws_secretsmanager_secret.ack_falcon[0].arn,
          data.aws_secretsmanager_secret.ack_ecdsa[0].arn
        ]
      }
    ]
  })
}

resource "aws_iam_role_policy" "ecs_task_operator_public_keys_secret" {
  count = var.guardian_operator_public_keys_secret_arn != "" ? 1 : 0

  name = "${var.stack_name}-ecs-task-operator-public-keys-secret"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "secretsmanager:GetSecretValue"
        ]
        Resource = [
          var.guardian_operator_public_keys_secret_arn
        ]
      }
    ]
  })
}

resource "aws_iam_role_policy" "ecs_task_execute_command" {
  name = "${var.stack_name}-ecs-task-execute-command"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "ssmmessages:CreateControlChannel",
          "ssmmessages:CreateDataChannel",
          "ssmmessages:OpenControlChannel",
          "ssmmessages:OpenDataChannel"
        ]
        Resource = "*"
      }
    ]
  })
}

resource "aws_iam_role" "rds_proxy" {
  count = local.effective_rds_proxy_enabled ? 1 : 0

  name = local.rds_proxy_role_name

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Principal = {
          Service = "rds.amazonaws.com"
        }
        Action = "sts:AssumeRole"
      }
    ]
  })
}

resource "aws_iam_role_policy" "rds_proxy_secrets" {
  count = local.effective_rds_proxy_enabled ? 1 : 0

  name = "${var.stack_name}-rds-proxy-secrets"
  role = aws_iam_role.rds_proxy[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "secretsmanager:GetSecretValue"
        ]
        Resource = [
          aws_secretsmanager_secret.database_credentials[0].arn
        ]
      }
    ]
  })
}
