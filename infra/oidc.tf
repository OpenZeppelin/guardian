# GitHub Actions OIDC roles for the AWS Deploy workflow
# ======================================================
# Two roles back .github/workflows/aws-deploy.yml: a bootstrap role in the
# organization's root account that GitHub's OIDC provider is trusted to assume,
# and a deploy role in this stack's account that the bootstrap role chains
# into. A workflow job only obtains the identity when it runs under one of the
# GitHub environments listed in github_oidc_subjects; those environments
# restrict deployments to main.
#
# The roles are shared by every Guardian stack in the account, so enable them
# on exactly one stack (guardian-prod) with github_oidc_enabled = true. Roles
# that already exist are adopted with terraform import; see infra/README.md.

locals {
  # Account that hosts the OIDC provider and the bootstrap role. Deriving the
  # bootstrap role ARN from it (rather than from the resource) keeps the deploy
  # role's trust policy computable at plan time even when the bootstrap role
  # has pending changes.
  github_oidc_root_account_id = var.github_oidc_enabled ? split(":", var.github_oidc_provider_arn)[4] : ""
  github_oidc_role_arn        = "arn:aws:iam::${local.github_oidc_root_account_id}:role/${var.github_oidc_role_name}"
}

data "aws_iam_policy_document" "github_oidc_trust" {
  count = var.github_oidc_enabled ? 1 : 0

  statement {
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [var.github_oidc_provider_arn]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }

    condition {
      test     = "StringLike"
      variable = "token.actions.githubusercontent.com:sub"
      values   = var.github_oidc_subjects
    }
  }
}

resource "aws_iam_role" "github_oidc" {
  count    = var.github_oidc_enabled ? 1 : 0
  provider = aws.root_account

  name                 = var.github_oidc_role_name
  assume_role_policy   = data.aws_iam_policy_document.github_oidc_trust[0].json
  max_session_duration = 14400

  tags = {
    Environment  = local.stage_name
    Component    = "Generic"
    Subcomponent = "OIDC ROLE"
  }

  lifecycle {
    precondition {
      condition     = var.github_oidc_provider_arn != "" && (var.github_oidc_root_account_role_arn != "" || var.github_oidc_root_account_profile != "")
      error_message = "github_oidc_enabled requires github_oidc_provider_arn and one of github_oidc_root_account_role_arn or github_oidc_root_account_profile."
    }
  }
}

data "aws_iam_policy_document" "github_deploy_trust" {
  count = var.github_oidc_enabled ? 1 : 0

  statement {
    actions = ["sts:AssumeRole", "sts:TagSession"]

    principals {
      type        = "AWS"
      identifiers = [local.github_oidc_role_arn]
    }
  }
}

resource "aws_iam_role" "github_deploy" {
  count = var.github_oidc_enabled ? 1 : 0

  name                 = var.github_deploy_role_name
  assume_role_policy   = data.aws_iam_policy_document.github_deploy_trust[0].json
  max_session_duration = 14400

  tags = {
    Component    = "Generic"
    Subcomponent = "Github Actions Role"
  }

  # IAM rejects a trust policy naming a principal role that does not exist yet.
  depends_on = [aws_iam_role.github_oidc]
}

# The deploy role currently carries AdministratorAccess, inherited from the
# original definition. The workflow only needs ECR push/pull, ECS describe/
# register/update, and iam:PassRole on the stack task roles (see
# docs/SERVER_AWS_DEPLOY.md); narrowing it is a separate change.
resource "aws_iam_role_policy_attachment" "github_deploy_admin" {
  count = var.github_oidc_enabled ? 1 : 0

  role       = aws_iam_role.github_deploy[0].name
  policy_arn = "arn:aws:iam::aws:policy/AdministratorAccess"
}

resource "aws_iam_role_policy" "github_oidc_assume_deploy" {
  count    = var.github_oidc_enabled ? 1 : 0
  provider = aws.root_account

  name = "github-actions-solutions-guardian-assume-role-policy"
  role = aws_iam_role.github_oidc[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect   = "Allow"
        Action   = ["sts:AssumeRole", "sts:TagSession"]
        Resource = aws_iam_role.github_deploy[0].arn
      }
    ]
  })
}
