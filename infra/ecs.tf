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

      environment = [
        {
          name  = "RUST_LOG"
          value = "info"
        },
        {
          name  = "DATABASE_URL"
          value = local.database_url
        },
        {
          name  = "GUARDIAN_NETWORK_TYPE"
          value = var.server_network_type
        }
      ]

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

# Postgres task definition
resource "aws_ecs_task_definition" "postgres" {
  family                   = local.postgres_task_family
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = var.postgres_cpu
  memory                   = var.postgres_memory
  execution_role_arn       = aws_iam_role.ecs_task_execution.arn
  task_role_arn            = aws_iam_role.ecs_task.arn

  runtime_platform {
    cpu_architecture        = var.cpu_architecture
    operating_system_family = "LINUX"
  }

  container_definitions = jsonencode([
    {
      name      = local.postgres_service_name
      image     = "postgres:16-alpine"
      essential = true

      portMappings = [
        {
          containerPort = 5432
          protocol      = "tcp"
        }
      ]

      environment = [
        {
          name  = "POSTGRES_USER"
          value = local.postgres_user
        },
        {
          name  = "POSTGRES_PASSWORD"
          value = local.postgres_password
        },
        {
          name  = "POSTGRES_DB"
          value = local.postgres_db
        }
      ]

      logConfiguration = {
        logDriver = "awslogs"
        options = {
          "awslogs-group"         = aws_cloudwatch_log_group.postgres.name
          "awslogs-region"        = var.aws_region
          "awslogs-stream-prefix" = "ecs"
        }
      }
    }
  ])
}

# Server ECS service
resource "aws_ecs_service" "server" {
  name                   = local.server_service_name
  cluster                = aws_ecs_cluster.main.id
  task_definition        = aws_ecs_task_definition.server.arn
  desired_count          = 1
  launch_type            = "FARGATE"
  platform_version       = "LATEST"
  enable_execute_command = true

  health_check_grace_period_seconds = 30

  network_configuration {
    subnets          = [local.primary_subnet_id]
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
    aws_ecs_service.postgres
  ]
}

# Postgres ECS service
resource "aws_ecs_service" "postgres" {
  name                   = local.postgres_service_name
  cluster                = aws_ecs_cluster.main.id
  task_definition        = aws_ecs_task_definition.postgres.arn
  desired_count          = 1
  launch_type            = "FARGATE"
  platform_version       = "LATEST"
  enable_execute_command = true

  network_configuration {
    subnets          = [local.primary_subnet_id]
    security_groups  = [aws_security_group.postgres.id]
    assign_public_ip = true
  }

  service_registries {
    registry_arn = aws_service_discovery_service.postgres.arn
  }

  depends_on = [aws_service_discovery_service.postgres]
}
