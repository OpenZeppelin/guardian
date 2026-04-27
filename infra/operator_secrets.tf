resource "aws_secretsmanager_secret" "operator_public_keys" {
  count = local.managed_operator_public_keys_secret_enabled ? 1 : 0

  name                    = local.operator_public_keys_secret_name
  recovery_window_in_days = 0
}

resource "aws_secretsmanager_secret_version" "operator_public_keys" {
  count = local.managed_operator_public_keys_secret_enabled ? 1 : 0

  secret_id     = aws_secretsmanager_secret.operator_public_keys[0].id
  secret_string = jsonencode(var.guardian_operator_public_keys)
}
