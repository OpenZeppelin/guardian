resource "random_password" "postgres" {
  count = var.postgres_password == "" ? 1 : 0

  length  = 32
  special = false
}

resource "aws_db_subnet_group" "postgres" {
  name       = local.rds_subnet_group_name
  subnet_ids = local.subnet_ids

  lifecycle {
    precondition {
      condition     = length(local.subnet_ids) >= 2
      error_message = "RDS deployments require at least two subnets."
    }
  }
}

resource "aws_db_instance" "postgres" {
  identifier                 = local.rds_instance_identifier
  engine                     = "postgres"
  engine_version             = var.rds_engine_version != "" ? var.rds_engine_version : null
  instance_class             = var.rds_instance_class
  allocated_storage          = var.rds_allocated_storage
  max_allocated_storage      = local.effective_rds_max_allocated_storage
  db_name                    = local.postgres_db
  username                   = local.postgres_user
  password                   = local.rds_master_password
  port                       = local.postgres_port
  db_subnet_group_name       = aws_db_subnet_group.postgres.name
  vpc_security_group_ids     = [aws_security_group.postgres.id]
  publicly_accessible        = var.rds_publicly_accessible
  backup_retention_period    = var.rds_backup_retention_days
  deletion_protection        = var.rds_deletion_protection
  skip_final_snapshot        = var.rds_skip_final_snapshot
  storage_encrypted          = true
  apply_immediately          = true
  auto_minor_version_upgrade = true
  copy_tags_to_snapshot      = true
}

resource "aws_secretsmanager_secret" "database_url" {
  name                    = local.database_secret_name
  recovery_window_in_days = 0
}

resource "aws_secretsmanager_secret" "database_credentials" {
  count = local.effective_rds_proxy_enabled ? 1 : 0

  name                    = local.database_credentials_secret_name
  recovery_window_in_days = 0
}

resource "aws_secretsmanager_secret_version" "database_url" {
  secret_id     = aws_secretsmanager_secret.database_url.id
  secret_string = local.database_url
}

resource "aws_secretsmanager_secret_version" "database_credentials" {
  count = local.effective_rds_proxy_enabled ? 1 : 0

  secret_id = aws_secretsmanager_secret.database_credentials[0].id
  secret_string = jsonencode({
    username = local.postgres_user
    password = local.rds_master_password
  })
}

resource "aws_db_proxy" "postgres" {
  count = local.effective_rds_proxy_enabled ? 1 : 0

  name                   = local.rds_proxy_name
  debug_logging          = false
  engine_family          = "POSTGRESQL"
  idle_client_timeout    = 1800
  require_tls            = true
  role_arn               = aws_iam_role.rds_proxy[0].arn
  vpc_subnet_ids         = local.subnet_ids
  vpc_security_group_ids = [aws_security_group.rds_proxy[0].id]

  auth {
    auth_scheme = "SECRETS"
    description = "Guardian PostgreSQL credentials"
    iam_auth    = "DISABLED"
    secret_arn  = aws_secretsmanager_secret.database_credentials[0].arn
  }

  depends_on = [
    aws_secretsmanager_secret_version.database_credentials
  ]
}

resource "aws_db_proxy_default_target_group" "postgres" {
  count = local.effective_rds_proxy_enabled ? 1 : 0

  db_proxy_name = aws_db_proxy.postgres[0].name

  connection_pool_config {
    connection_borrow_timeout    = 120
    max_connections_percent      = 80
    max_idle_connections_percent = 50
  }
}

resource "aws_db_proxy_target" "postgres" {
  count = local.effective_rds_proxy_enabled ? 1 : 0

  db_proxy_name          = aws_db_proxy.postgres[0].name
  target_group_name      = "default"
  db_instance_identifier = aws_db_instance.postgres.identifier
}
