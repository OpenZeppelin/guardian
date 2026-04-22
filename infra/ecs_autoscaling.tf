resource "aws_appautoscaling_target" "server" {
  count = local.effective_server_autoscaling_enabled ? 1 : 0

  max_capacity       = local.effective_server_autoscaling_max_capacity
  min_capacity       = local.effective_server_autoscaling_min_capacity
  resource_id        = "service/${aws_ecs_cluster.main.name}/${aws_ecs_service.server.name}"
  scalable_dimension = "ecs:service:DesiredCount"
  service_namespace  = "ecs"
}

resource "aws_appautoscaling_policy" "server_cpu" {
  count = local.effective_server_autoscaling_enabled ? 1 : 0

  name               = "${local.server_service_name}-cpu-target"
  policy_type        = "TargetTrackingScaling"
  resource_id        = aws_appautoscaling_target.server[0].resource_id
  scalable_dimension = aws_appautoscaling_target.server[0].scalable_dimension
  service_namespace  = aws_appautoscaling_target.server[0].service_namespace

  target_tracking_scaling_policy_configuration {
    predefined_metric_specification {
      predefined_metric_type = "ECSServiceAverageCPUUtilization"
    }

    target_value = local.effective_server_autoscaling_cpu_target
  }
}

resource "aws_appautoscaling_policy" "server_memory" {
  count = local.effective_server_autoscaling_enabled ? 1 : 0

  name               = "${local.server_service_name}-memory-target"
  policy_type        = "TargetTrackingScaling"
  resource_id        = aws_appautoscaling_target.server[0].resource_id
  scalable_dimension = aws_appautoscaling_target.server[0].scalable_dimension
  service_namespace  = aws_appautoscaling_target.server[0].service_namespace

  target_tracking_scaling_policy_configuration {
    predefined_metric_specification {
      predefined_metric_type = "ECSServiceAverageMemoryUtilization"
    }

    target_value = local.effective_server_autoscaling_memory_target
  }
}
