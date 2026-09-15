# Log-level monitoring: CloudWatch Logs metric filters on the server log
# group plus the alarm built on them. Unlike the alarms in observability.tf
# these read the server container's own log lines (no metrics endpoint or
# ADOT sidecar involved), so the ERROR filter and its alarm keep working
# with guardian_metrics_enabled = false. They give the rate-based 5xx /
# gRPC alarms an absolute complement: on a low-traffic stack a handful of
# failures never moves a percentage that ALB health checks dominate, but
# every one is an ERROR line. What the alarm does and does not cover, and
# how to tune it, is documented in docs/SERVER_AWS_DEPLOY.md#log-level-alarms.
#
# The filters match the JSON `level` field the server emits with
# GUARDIAN_LOG_FORMAT=json (flattened tracing events), hence the
# guardian_log_format precondition below. A metric filter applies to the
# whole group, and this one also carries the `adot` and `ca-init` streams:
# the ADOT Collector writes console-encoded lines (not JSON, lowercase
# level), which a JSON pattern never matches (collector faults surface
# through the metrics-missing alarm), and the one-shot ca-init container
# prints nothing on success. Anything else that is not a JSON object with
# an uppercase `level` (panic text from a crashing task, for instance) is
# invisible to these filters too.
#
# default_value = "0" makes each filter emit 0 for every non-matching
# event, so the series is continuous while log lines are being ingested:
# the dashboard draws zeros instead of gaps and the alarm clears on real
# datapoints. With no lines at all (a stopped task, or a healthy but quiet
# one: ALB health checks log nothing at the default filter) nothing is
# published, the data is missing, and treat_missing_data = notBreaching
# keeps the alarm OK.
#
# Cost per stack: one custom metric per filter ($0.30/month each) plus one
# standard-resolution alarm ($0.10/month); the filters themselves are free.

resource "aws_cloudwatch_log_metric_filter" "server_log_errors" {
  count = var.cloudwatch_log_alarms_enabled ? 1 : 0

  name           = "${var.stack_name}-server-log-errors"
  log_group_name = aws_cloudwatch_log_group.server.name
  pattern        = "{ $.level = \"ERROR\" }"

  metric_transformation {
    name          = local.log_error_metric_name
    namespace     = local.log_metrics_namespace
    value         = "1"
    unit          = "Count"
    default_value = "0"
  }

  lifecycle {
    precondition {
      condition     = local.effective_guardian_log_format == "json"
      error_message = "cloudwatch_log_alarms_enabled requires guardian_log_format = \"json\": the log metric filters match the JSON level field. Set cloudwatch_log_alarms_enabled = false to run text or compact logs without them."
    }
  }
}

# WARN feeds only the dashboard's "Server log lines by level" widget, so it
# is deployed only alongside the dashboard (nothing would read it
# otherwise). No alarm: the server logs WARN for client-caused and
# self-healing conditions.
resource "aws_cloudwatch_log_metric_filter" "server_log_warnings" {
  count = var.cloudwatch_log_alarms_enabled && local.cloudwatch_metrics_enabled ? 1 : 0

  name           = "${var.stack_name}-server-log-warnings"
  log_group_name = aws_cloudwatch_log_group.server.name
  pattern        = "{ $.level = \"WARN\" }"

  metric_transformation {
    name          = local.log_warn_metric_name
    namespace     = local.log_metrics_namespace
    value         = "1"
    unit          = "Count"
    default_value = "0"
  }
}

# Pages when the count exceeds the threshold in each of two consecutive
# 5-minute periods: a persistent fault, however slow, pages within 10
# minutes; one isolated line does not. A burst confined to a single period
# does not page on its own (it is visible on the dashboard and, if large
# enough, through the rate alarms). The server also logs ERROR for some
# client-caused rejections (see the docs section above), so a persistently
# misconfigured client can trip this; alarm_log_error_threshold is the
# tolerance knob.
resource "aws_cloudwatch_metric_alarm" "server_log_errors" {
  count = var.cloudwatch_log_alarms_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-server-log-errors"
  alarm_description   = "Guardian server logged more than ${var.alarm_log_error_threshold} ERROR-level line(s) per 5-minute period in two consecutive periods (absolute count, independent of request volume; query the server log group for level = \"ERROR\")${local.log_alarm_description_links}"
  namespace           = local.log_metrics_namespace
  metric_name         = aws_cloudwatch_log_metric_filter.server_log_errors[0].metric_transformation[0].name
  statistic           = "Sum"
  period              = 300
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.alarm_log_error_threshold
  evaluation_periods  = 2
  datapoints_to_alarm = 2
  treat_missing_data  = "notBreaching"
  alarm_actions       = local.effective_alarm_actions
  ok_actions          = local.effective_alarm_actions
}
