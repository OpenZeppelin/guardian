# Security group for ALB
resource "aws_security_group" "alb" {
  name        = "guardian-alb-sg"
  description = "GUARDIAN ALB security group"
  vpc_id      = local.vpc_id

  # HTTP ingress
  ingress {
    description = "HTTP from anywhere"
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = var.alb_ingress_cidrs
  }

  # HTTPS ingress
  ingress {
    description = "HTTPS from anywhere"
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = var.alb_ingress_cidrs
  }

  egress {
    description = "All outbound traffic"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = [local.vpc_cidr]
  }
}

# Security group for server
resource "aws_security_group" "server" {
  name        = "guardian-server-sg"
  description = "GUARDIAN server security group"
  vpc_id      = local.vpc_id

  # HTTP from ALB
  ingress {
    description     = "HTTP from ALB"
    from_port       = 3000
    to_port         = 3000
    protocol        = "tcp"
    security_groups = [aws_security_group.alb.id]
  }

  # gRPC from anywhere (public)
  ingress {
    description = "gRPC from anywhere"
    from_port   = 50051
    to_port     = 50051
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    description = "All outbound traffic"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

# Security group for Postgres
resource "aws_security_group" "postgres" {
  name        = "guardian-postgres-sg"
  description = "GUARDIAN Postgres security group"
  vpc_id      = local.vpc_id

  # Postgres from server
  ingress {
    description     = "Postgres from server"
    from_port       = 5432
    to_port         = 5432
    protocol        = "tcp"
    security_groups = [aws_security_group.server.id]
  }

  egress {
    description = "All outbound traffic"
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}
