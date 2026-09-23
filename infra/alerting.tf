# Alarm notification delivery: an opt-in, Terraform-managed SNS topic that
# every Guardian CloudWatch alarm (observability.tf) notifies on ALARM and
# OK transitions, plus an optional Amazon Q Developer in chat applications
# (formerly AWS Chatbot) Slack channel configuration subscribed to it.
# Enablement flags and names live with the other stack-derived locals in
# data.tf; the input-consistency preconditions sit on aws_lb.main (alb.tf)
# with the rest of the cross-variable rules.
#
# Operators who already run their own notification pipeline keep passing
# topic ARNs via alarm_actions; the managed topic is appended to that
# list, never substituted for it.
#
# The Slack side is deliberately split: Terraform manages the channel
# configuration (channel <-> topic <-> IAM role), but authorizing the Slack
# workspace with the AWS account is a one-time OAuth flow that only the
# console can run. Its outcome, the workspace (team) ID, is what
# alarm_slack_workspace_id carries.

# --- SNS topic ------------------------------------------------------------

# Not KMS-encrypted on purpose: CloudWatch cannot publish to a topic
# encrypted with the AWS-managed SNS key (its key policy is immutable), so
# encryption would require a customer-managed key with a cloudwatch
# grant. Alarm payloads carry only alarm metadata (names, thresholds,
# metric values), nothing secret.
resource "aws_sns_topic" "alarms" {
  count = local.alarm_notifications_enabled ? 1 : 0

  name         = local.alarm_sns_topic_name
  display_name = "${var.stack_name} Guardian alarms"
}

# Confused-deputy guard: only this account's CloudWatch alarms may publish
# (name-prefix scoped to this stack; sibling stacks sharing the prefix are
# the same account's alarms, so this is not a cross-account boundary). The
# first statement keeps the same-account management rights of SNS's
# default topic policy, which replacing the policy would otherwise drop.
resource "aws_sns_topic_policy" "alarms" {
  count = local.alarm_notifications_enabled ? 1 : 0

  arn = aws_sns_topic.alarms[0].arn

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "AccountOwner"
        Effect = "Allow"
        Principal = {
          AWS = "arn:aws:iam::${data.aws_caller_identity.current.account_id}:root"
        }
        Action = [
          "SNS:GetTopicAttributes",
          "SNS:SetTopicAttributes",
          "SNS:AddPermission",
          "SNS:RemovePermission",
          "SNS:DeleteTopic",
          "SNS:Subscribe",
          "SNS:ListSubscriptionsByTopic",
          "SNS:Publish"
        ]
        Resource = aws_sns_topic.alarms[0].arn
      },
      {
        Sid    = "CloudWatchAlarms"
        Effect = "Allow"
        Principal = {
          Service = "cloudwatch.amazonaws.com"
        }
        Action   = "SNS:Publish"
        Resource = aws_sns_topic.alarms[0].arn
        Condition = {
          StringEquals = {
            "aws:SourceAccount" = data.aws_caller_identity.current.account_id
          }
          ArnLike = {
            "aws:SourceArn" = "arn:aws:cloudwatch:${var.aws_region}:${data.aws_caller_identity.current.account_id}:alarm:${var.stack_name}-*"
          }
        }
      }
    ]
  })
}

# --- Slack channel (Amazon Q Developer in chat applications) --------------

# Channel role assumed by Amazon Q to render the metric graph attached to
# each alarm notification. The inline policy mirrors AWS's own
# "notification permissions" template (CloudWatch read only), deliberately
# narrower than the CloudWatchReadOnlyAccess managed policy (which also
# grants logs/SNS/autoscaling reads); that managed policy serves only as
# the channel guardrail, capping whatever the channel could ever do even
# if this role were later widened. The trust policy carries no
# aws:SourceAccount/SourceArn condition because AWS does not document the
# context keys Amazon Q presents on AssumeRole; it matches the roles the
# Amazon Q console creates.
resource "aws_iam_role" "chatbot_alarms" {
  count = local.alarm_slack_enabled ? 1 : 0

  name = local.chatbot_role_name

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Principal = {
          Service = "chatbot.amazonaws.com"
        }
        Action = "sts:AssumeRole"
      }
    ]
  })
}

resource "aws_iam_role_policy" "chatbot_alarms_cloudwatch_read" {
  count = local.alarm_slack_enabled ? 1 : 0

  name = "${local.chatbot_role_name}-cloudwatch-read"
  role = aws_iam_role.chatbot_alarms[0].id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "cloudwatch:Describe*",
          "cloudwatch:Get*",
          "cloudwatch:List*"
        ]
        Resource = "*"
      }
    ]
  })
}

# Amazon Q chat configurations are global (no region in the ARN); the AWS
# provider routes the API calls to a supported endpoint regardless of
# var.aws_region, so no provider alias is needed. Error-level logging is
# where Amazon Q-side delivery failures surface (service-managed log
# group /aws/chatbot/<configuration_name> in us-east-1, created on first
# error, outside this state, no retention policy, tiny volume); failures
# upstream of Amazon Q (a denied CloudWatch -> SNS publish) show in the
# alarm's action history instead.
resource "aws_chatbot_slack_channel_configuration" "alarms" {
  count = local.alarm_slack_enabled ? 1 : 0

  configuration_name          = local.alarm_slack_configuration_name
  iam_role_arn                = aws_iam_role.chatbot_alarms[0].arn
  slack_team_id               = var.alarm_slack_workspace_id
  slack_channel_id            = var.alarm_slack_channel_id
  sns_topic_arns              = [aws_sns_topic.alarms[0].arn]
  guardrail_policy_arns       = ["arn:aws:iam::aws:policy/CloudWatchReadOnlyAccess"]
  logging_level               = "ERROR"
  user_authorization_required = false
}
