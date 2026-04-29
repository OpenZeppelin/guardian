# Application Load Balancer
resource "aws_lb" "main" {
  name               = local.alb_name
  internal           = false
  load_balancer_type = "application"
  security_groups    = [aws_security_group.alb.id]
  subnets            = local.load_balancer_subnet_ids

  enable_deletion_protection = false

  lifecycle {
    precondition {
      condition     = length(local.load_balancer_subnet_ids) >= 2
      error_message = "Application Load Balancer requires at least two subnets in distinct Availability Zones. Configure subnet_ids with subnets from at least two AZs."
    }
  }
}

# Target group for server
resource "aws_lb_target_group" "server" {
  name        = local.target_group_name
  port        = 3000
  protocol    = "HTTP"
  vpc_id      = local.vpc_id
  target_type = "ip"

  health_check {
    enabled             = true
    healthy_threshold   = 5
    unhealthy_threshold = 2
    timeout             = 5
    interval            = 30
    path                = "/"
    protocol            = "HTTP"
    matcher             = "200"
  }
}

resource "aws_lb_target_group" "server_grpc" {
  count = local.acm_certificate_arn != "" ? 1 : 0

  name             = local.grpc_target_group_name
  port             = 50051
  protocol         = "HTTP"
  protocol_version = "GRPC"
  vpc_id           = local.vpc_id
  target_type      = "ip"

  health_check {
    enabled             = true
    healthy_threshold   = 5
    unhealthy_threshold = 2
    timeout             = 5
    interval            = 30
    path                = "/guardian.Guardian/GetPubkey"
    matcher             = "0"
  }
}

# HTTP listener (always created)
resource "aws_lb_listener" "http" {
  load_balancer_arn = aws_lb.main.arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = local.acm_certificate_arn != "" ? "redirect" : "forward"

    # Forward to target group when no HTTPS
    dynamic "forward" {
      for_each = local.acm_certificate_arn == "" ? [1] : []
      content {
        target_group {
          arn = aws_lb_target_group.server.arn
        }
      }
    }

    # Redirect to HTTPS when certificate is provided
    dynamic "redirect" {
      for_each = local.acm_certificate_arn != "" ? [1] : []
      content {
        host        = "#{host}"
        path        = "/#{path}"
        query       = "#{query}"
        port        = "443"
        protocol    = "HTTPS"
        status_code = "HTTP_301"
      }
    }
  }
}

# HTTPS listener (only when certificate is provided)
resource "aws_lb_listener" "https" {
  count = local.acm_certificate_arn != "" ? 1 : 0

  load_balancer_arn = aws_lb.main.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = local.acm_certificate_arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.server.arn
  }
}

resource "aws_lb_listener_rule" "https_grpc" {
  count = local.acm_certificate_arn != "" ? 1 : 0

  listener_arn = aws_lb_listener.https[0].arn
  priority     = 10

  action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.server_grpc[0].arn
  }

  condition {
    path_pattern {
      values = ["/guardian.Guardian/*"]
    }
  }
}
