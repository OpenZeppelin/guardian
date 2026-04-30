# ECS Cluster
resource "aws_ecs_cluster" "main" {
  name = local.cluster_name

  setting {
    name  = "containerInsights"
    value = "enabled"
  }

  configuration {
    execute_command_configuration {
      logging = "OVERRIDE"
      log_configuration {
        cloud_watch_log_group_name = aws_cloudwatch_log_group.cluster.name
      }
    }
  }
}

resource "aws_ecs_cluster_capacity_providers" "main" {
  cluster_name = aws_ecs_cluster.main.name

  capacity_providers = ["FARGATE", "FARGATE_SPOT"]

  default_capacity_provider_strategy {
    capacity_provider = "FARGATE"
    weight            = 1
  }
}

# Server task definition
resource "aws_ecs_task_definition" "server" {
  family                   = local.server_task_family
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = var.server_cpu
  memory                   = var.server_memory
  execution_role_arn       = aws_iam_role.ecs_task_execution.arn
  task_role_arn            = aws_iam_role.ecs_task.arn

  runtime_platform {
    cpu_architecture        = var.cpu_architecture
    operating_system_family = "LINUX"
  }

  container_definitions = jsonencode([
    {
      name      = local.server_container_name
      image     = var.server_image_uri
      essential = true

      portMappings = [
        {
          containerPort = 3000
          protocol      = "tcp"
        },
        {
          containerPort = 50051
          protocol      = "tcp"
        }
      ]

      environment = concat([
        {
          name  = "RUST_LOG"
          value = "info"
        },
        {
          name  = "GUARDIAN_NETWORK_TYPE"
          value = var.server_network_type
        },
        {
          name  = "GUARDIAN_ENV"
          value = var.deployment_stage
        },
        {
          name  = "AWS_REGION"
          value = var.aws_region
        },
        {
          name  = "GUARDIAN_RATE_LIMIT_ENABLED"
          value = tostring(local.effective_guardian_rate_limit_enabled)
        },
        {
          name  = "GUARDIAN_RATE_BURST_PER_SEC"
          value = tostring(local.effective_guardian_rate_burst_per_sec)
        },
        {
          name  = "GUARDIAN_RATE_PER_MIN"
          value = tostring(local.effective_guardian_rate_per_min)
        },
        {
          name  = "GUARDIAN_DB_POOL_MAX_SIZE"
          value = tostring(local.effective_guardian_db_pool_max_size)
        },
        {
          name  = "GUARDIAN_METADATA_DB_POOL_MAX_SIZE"
          value = tostring(local.effective_guardian_metadata_db_pool_max_size)
        },
        {
          name  = "GUARDIAN_OPERATOR_PUBLIC_KEYS_SECRET_ID"
          value = local.operator_public_keys_secret_arn
        }
        ],
        var.guardian_cors_allowed_origins != "" ? [
          {
            name  = "GUARDIAN_CORS_ALLOWED_ORIGINS"
            value = var.guardian_cors_allowed_origins
          }
        ] : [],
        var.guardian_cors_allow_credentials ? [
          {
            name  = "GUARDIAN_CORS_ALLOW_CREDENTIALS"
            value = tostring(var.guardian_cors_allow_credentials)
          }
        ] : [],
        var.guardian_evm_entrypoint_address != "" ? [
          {
            name  = "GUARDIAN_EVM_ENTRYPOINT_ADDRESS"
            value = var.guardian_evm_entrypoint_address
          }
        ] : [],
        var.guardian_evm_session_cookie_domain != "" ? [
          {
            name  = "GUARDIAN_EVM_SESSION_COOKIE_DOMAIN"
            value = var.guardian_evm_session_cookie_domain
          }
        ] : [],
        var.guardian_evm_session_cookie_same_site != "" ? [
          {
            name  = "GUARDIAN_EVM_SESSION_COOKIE_SAME_SITE"
            value = var.guardian_evm_session_cookie_same_site
          }
        ] : [],
        var.guardian_evm_session_cookie_secure ? [
          {
            name  = "GUARDIAN_EVM_SESSION_COOKIE_SECURE"
            value = tostring(var.guardian_evm_session_cookie_secure)
          }
        ] : []
      )

      secrets = concat([
        {
          name      = "DATABASE_URL"
          valueFrom = aws_secretsmanager_secret.database_url.arn
        }
        ],
        local.evm_allowed_chain_ids_secret_arn != "" ? [
          {
            name      = "GUARDIAN_EVM_ALLOWED_CHAIN_IDS"
            valueFrom = local.evm_allowed_chain_ids_secret_arn
          }
        ] : [],
        local.evm_rpc_urls_secret_arn != "" ? [
          {
            name      = "GUARDIAN_EVM_RPC_URLS"
            valueFrom = local.evm_rpc_urls_secret_arn
          }
        ] : []
      )

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.server.name
          "awslogs-region"        = var.aws_region
          "awslogs-stream-prefix" = "ecs"
        }
      }
    }
  ])
}

# Server ECS service
resource "aws_ecs_service" "server" {
  name                               = local.server_service_name
  cluster                            = aws_ecs_cluster.main.id
  task_definition                    = aws_ecs_task_definition.server.arn
  desired_count                      = local.effective_server_desired_count
  deployment_maximum_percent         = 200
  deployment_minimum_healthy_percent = 100
  launch_type                        = "FARGATE"
  platform_version                   = "LATEST"
  enable_execute_command             = true

  health_check_grace_period_seconds = 30

  network_configuration {
    subnets          = local.subnet_ids
    security_groups  = [aws_security_group.server.id]
    assign_public_ip = true
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.server.arn
    container_name   = local.server_container_name
    container_port   = 3000
  }

  dynamic "load_balancer" {
    for_each = local.acm_certificate_arn != "" ? [1] : []

    content {
      target_group_arn = aws_lb_target_group.server_grpc[0].arn
      container_name   = local.server_container_name
      container_port   = 50051
    }
  }

  depends_on = [
    aws_lb_listener.http,
    aws_lb_listener.https,
    aws_lb_listener_rule.https_grpc,
    aws_secretsmanager_secret_version.database_url,
    aws_secretsmanager_secret_version.evm_allowed_chain_ids,
    aws_secretsmanager_secret_version.evm_rpc_urls,
    aws_secretsmanager_secret_version.operator_public_keys
  ]
}
