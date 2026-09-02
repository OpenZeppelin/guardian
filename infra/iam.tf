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
    Statement = concat(
      [
        {
          Effect = "Allow"
          Action = [
            "secretsmanager:GetSecretValue"
          ]
          Resource = concat(
            [
              aws_secretsmanager_secret.database_url.arn
            ],
            local.ca_bundle_enabled ? [
              var.rds_ca_bundle_secret_arn
            ] : [],
            local.evm_allowed_chain_ids_secret_arn != "" ? [
              local.evm_allowed_chain_ids_secret_arn
            ] : [],
            local.evm_rpc_urls_secret_arn != "" ? [
              local.evm_rpc_urls_secret_arn
            ] : [],
            local.is_prod ? [
              data.aws_secretsmanager_secret.dashboard_cursor[0].arn
            ] : []
          )
        }
      ],
      local.is_prod && data.aws_secretsmanager_secret.dashboard_cursor[0].kms_key_id != "" ? [
        {
          Effect = "Allow"
          Action = [
            "kms:Decrypt"
          ]
          Resource = [
            data.aws_kms_key.dashboard_cursor[0].arn
          ]
        }
      ] : []
    )
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
        Resource = concat(
          [data.aws_secretsmanager_secret.ack_falcon[0].arn],
          var.guardian_ack_ecdsa_kms_key_arn == "" ? [data.aws_secretsmanager_secret.ack_ecdsa[0].arn] : []
        )
      }
    ]
  })
}

resource "aws_iam_role_policy" "ecs_task_ack_ecdsa_kms" {
  count = var.guardian_ack_ecdsa_kms_key_arn != "" ? 1 : 0

  name = "${var.stack_name}-ecs-task-ack-ecdsa-kms"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "kms:GetPublicKey",
          "kms:Sign"
        ]
        Resource = [
          var.guardian_ack_ecdsa_kms_key_arn
        ]
      }
    ]
  })
}

resource "aws_iam_role_policy" "ecs_task_storage_encryption_secret" {
  count = local.managed_storage_encryption_enabled ? 1 : 0

  name = "${var.stack_name}-ecs-task-storage-encryption-secret"
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
          data.aws_secretsmanager_secret.storage_encryption[0].arn
        ]
      }
    ]
  })
}

resource "aws_iam_role_policy" "ecs_task_operator_public_keys_secret" {
  count = var.guardian_operator_public_keys_secret_arn != "" || local.managed_operator_public_keys_secret_enabled ? 1 : 0

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
          local.operator_public_keys_secret_arn
        ]
      }
    ]
  })
}

# The ADOT sidecar publishes metrics as EMF log events using the task
# role, so it needs only stream-level write access on the
# Terraform-managed EMF log group. Deliberately no CreateLogGroup or
# PutRetentionPolicy: Terraform stays authoritative for the group and
# its retention, and a group deleted out of band surfaces via awsemf
# export errors in the sidecar log plus the metrics-missing alarm.
resource "aws_iam_role_policy" "ecs_task_adot_metrics" {
  count = local.cloudwatch_metrics_enabled ? 1 : 0

  name = "${var.stack_name}-ecs-task-adot-metrics"
  role = aws_iam_role.ecs_task.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "logs:CreateLogStream",
          "logs:DescribeLogStreams",
          "logs:PutLogEvents"
        ]
        Resource = [
          aws_cloudwatch_log_group.emf[0].arn,
          "${aws_cloudwatch_log_group.emf[0].arn}:*"
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
