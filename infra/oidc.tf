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
# on exactly one stack per account with github_oidc_enabled = true and list
# every stack the workflow may roll out in github_deploy_stack_names. Setup,
# adoption of pre-existing roles, and reuse from a fork are documented in
# docs/runbooks/github-oidc-deploy-roles.md.

locals {
  # Account that hosts the OIDC provider and the bootstrap role. Deriving the
  # bootstrap role ARN from it (rather than from the resource) keeps the deploy
  # role's trust policy computable at plan time even when the bootstrap role
  # has pending changes.
  github_oidc_root_account_id = var.github_oidc_enabled ? split(":", var.github_oidc_provider_arn)[4] : ""
  github_oidc_role_arn        = "arn:aws:iam::${local.github_oidc_root_account_id}:role/${var.github_oidc_role_name}"

  # Resources the deploy role may touch, derived from the default stack naming
  # in data.tf for each stack in github_deploy_stack_names.
  github_deploy_account_id = data.aws_caller_identity.current.account_id
  github_deploy_ecr_repository_arns = [
    for stack in var.github_deploy_stack_names :
    "arn:aws:ecr:${var.aws_region}:${local.github_deploy_account_id}:repository/${stack}-server"
  ]
  github_deploy_ecs_service_arns = [
    for stack in var.github_deploy_stack_names :
    "arn:aws:ecs:${var.aws_region}:${local.github_deploy_account_id}:service/${stack}-cluster/${stack}-server"
  ]
  github_deploy_task_definition_arns = [
    for stack in var.github_deploy_stack_names :
    "arn:aws:ecs:${var.aws_region}:${local.github_deploy_account_id}:task-definition/${stack}-server:*"
  ]
  github_deploy_task_role_arns = flatten([
    for stack in var.github_deploy_stack_names : [
      "arn:aws:iam::${local.github_deploy_account_id}:role/${stack}-ecs-task-execution",
      "arn:aws:iam::${local.github_deploy_account_id}:role/${stack}-ecs-task",
    ]
  ])
}

# Guards against root-account credentials that belong to a different account
# than the one encoded in github_oidc_provider_arn; the bootstrap role would
# otherwise be created in one account while the deploy role trusts a role ARN
# in another.
data "aws_caller_identity" "github_oidc_root" {
  count    = var.github_oidc_enabled ? 1 : 0
  provider = aws.root_account
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

    # Exact match: the subjects are full environment claims, never patterns.
    condition {
      test     = "StringEquals"
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

    precondition {
      condition     = data.aws_caller_identity.github_oidc_root[0].account_id == local.github_oidc_root_account_id
      error_message = "The aws.root_account provider resolves to account ${data.aws_caller_identity.github_oidc_root[0].account_id}, but github_oidc_provider_arn belongs to account ${local.github_oidc_root_account_id}."
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

# Least-privilege policy for what the AWS Deploy workflow actually does: log in
# to ECR, mirror an image into the stack repository, read the service and its
# task definition, register a new revision, and update the service.
data "aws_iam_policy_document" "github_deploy" {
  count = var.github_oidc_enabled ? 1 : 0

  statement {
    sid       = "EcrLogin"
    actions   = ["ecr:GetAuthorizationToken"]
    resources = ["*"]
  }

  statement {
    sid = "EcrMirrorImage"
    actions = [
      "ecr:DescribeRepositories",
      "ecr:DescribeImages",
      "ecr:ListImages",
      "ecr:BatchGetImage",
      "ecr:GetDownloadUrlForLayer",
      "ecr:BatchCheckLayerAvailability",
      "ecr:InitiateLayerUpload",
      "ecr:UploadLayerPart",
      "ecr:CompleteLayerUpload",
      "ecr:PutImage",
    ]
    resources = local.github_deploy_ecr_repository_arns
  }

  statement {
    sid = "EcsService"
    actions = [
      "ecs:DescribeServices",
      "ecs:UpdateService",
    ]
    resources = local.github_deploy_ecs_service_arns
  }

  # Describe/RegisterTaskDefinition do not support resource-level permissions.
  statement {
    sid = "EcsTaskDefinition"
    actions = [
      "ecs:DescribeTaskDefinition",
      "ecs:RegisterTaskDefinition",
    ]
    resources = ["*"]
  }

  statement {
    sid       = "EcsTagTaskDefinition"
    actions   = ["ecs:TagResource"]
    resources = local.github_deploy_task_definition_arns
  }

  statement {
    sid       = "PassTaskRoles"
    actions   = ["iam:PassRole"]
    resources = local.github_deploy_task_role_arns

    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["ecs-tasks.amazonaws.com"]
    }
  }
}

resource "aws_iam_role_policy" "github_deploy" {
  count = var.github_oidc_enabled ? 1 : 0

  name   = "guardian-aws-deploy"
  role   = aws_iam_role.github_deploy[0].id
  policy = data.aws_iam_policy_document.github_deploy[0].json
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
