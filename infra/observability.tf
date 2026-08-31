# Application metrics pipeline: an ADOT Collector sidecar scrapes the
# Guardian Prometheus endpoint over the task-local loopback and exports
# selected metrics to CloudWatch as EMF log events, which CloudWatch
# materializes as custom metrics under local.metrics_namespace. The
# dashboard and alarms below are built on those metrics plus the AWS/ECS
# service metrics.
#
# Cardinality/cost note: every metric name + dimension-set combination
# below becomes one CloudWatch custom metric. Dimension sets are chosen
# from Guardian's closed label sets (statuses, outcomes, small enums) to
# keep the count bounded; high-cardinality labels (route, method,
# operation) are deliberately rolled up, and zero-dimension rollups are
# declared only where a widget or alarm actually consumes them.

locals {
  # HTTP statuses and gRPC codes counted as server faults by the error
  # rate dashboard widget and alarms. One list each, consumed by both,
  # so the alarm and the dashboard can never disagree about what
  # "error rate" means. gRPC `aborted` and `resource_exhausted` are
  # deliberately excluded: they signal client-retryable conflicts and
  # rate-limit pressure, not server faults.
  http_error_statuses = ["500", "501", "502", "503", "504"]
  grpc_error_codes    = ["internal", "unavailable", "unknown", "data_loss", "deadline_exceeded"]

  http_error_rate_expression = "100 * (${join(" + ", [for s in local.http_error_statuses : "FILL(h${s}, 0)"])}) / FILL(hall, 1)"
  grpc_error_rate_expression = "100 * (${join(" + ", [for c in local.grpc_error_codes : "FILL(g${replace(c, "_", "")}, 0)"])}) / FILL(gall, 1)"

  # A pass that errors outright reports outcome=error; a pass in which
  # individual accounts failed still completes and reports
  # outcome=partial. Both mean canonicalization work is failing.
  canonicalization_run_metrics = {
    runs      = "guardian_canonicalization_runs_total"
    fast      = "guardian_canonicalization_fast_runs_total"
    reconcile = "guardian_canonicalization_reconcile_runs_total"
  }
  canonicalization_failure_outcomes = ["error", "partial"]
  canonicalization_failure_queries = {
    for pair in setproduct(keys(local.canonicalization_run_metrics), local.canonicalization_failure_outcomes) :
    "${pair[0]}${title(pair[1])}" => {
      metric  = local.canonicalization_run_metrics[pair[0]]
      outcome = pair[1]
    }
  }
  canonicalization_failures_expression = join(" + ", [for id in sort(keys(local.canonicalization_failure_queries)) : "FILL(${id}, 0)"])

  # Injected into the sidecar via AOT_CONFIG_CONTENT, so no config file,
  # SSM parameter, or custom image is needed.
  #
  # The metric_name_selectors below re-list part of the metric inventory
  # that crates/server/src/metrics/names.rs owns as REGISTRY (the Grafana
  # counterpart lives in docs/guides/observability/grafana/dashboards/
  # guardian.json). A metric added to REGISTRY is NOT exported to
  # CloudWatch until it is declared here.
  adot_config = yamlencode({
    extensions = {
      health_check = {}
    }
    receivers = {
      prometheus = {
        config = {
          scrape_configs = [
            {
              job_name        = "guardian-server"
              scrape_interval = "60s"
              metrics_path    = local.metrics_path
              static_configs = [
                { targets = [local.metrics_bind_addr] }
              ]
            }
          ]
        }
      }
    }
    processors = {
      # Resolves the {TaskId} log-stream placeholder so concurrent tasks
      # write distinct EMF streams. Resource attributes never become
      # metric dimensions (resource_to_telemetry_conversion stays off).
      resourcedetection = {
        detectors = ["ecs"]
        timeout   = "5s"
      }
      batch = {}
    }
    exporters = {
      awsemf = {
        namespace               = local.metrics_namespace
        log_group_name          = local.emf_log_group_name
        log_stream_name         = "emf/{TaskId}"
        dimension_rollup_option = "NoDimensionRollup"
        # Without this, the exporter's cumulative-to-delta conversion
        # swallows the first datapoint of any newly-appearing series.
        # Rare-event counters (canonicalization outcome=error, refresh
        # failures) are born at their first failure, so dropping that
        # initial value would hide exactly the events the alarms exist
        # for. The cost is a small spike on counter series at task start.
        retain_initial_value_of_delta_metric = true
        # Only metrics matching a declaration are exported; everything
        # else the scrape returns (process_*, etc.) is dropped here.
        metric_declarations = [
          {
            # Constant-1 gauge emitted from startup; the traffic-
            # independent heartbeat behind the metrics-missing alarm.
            metric_name_selectors = ["^guardian_build_info$"]
            dimensions            = [[]]
          },
          {
            metric_name_selectors = ["^guardian_http_requests_total$"]
            dimensions            = [[], ["status"]]
          },
          {
            metric_name_selectors = ["^guardian_grpc_requests_total$"]
            dimensions            = [[], ["code"]]
          },
          {
            # Prometheus histograms carry only sum/count through the
            # EMF path, so Average is the one meaningful statistic;
            # Min/Max read 0 and percentiles are unavailable.
            metric_name_selectors = [
              "^guardian_http_request_duration_seconds$",
              "^guardian_grpc_request_duration_seconds$",
              "^guardian_http_requests_in_flight$",
              "^guardian_grpc_requests_in_flight$",
            ]
            dimensions = [[]]
          },
          {
            metric_name_selectors = [
              "^guardian_miden_rpc_requests_total$",
              "^guardian_storage_operations_total$",
            ]
            dimensions = [["outcome"]]
          },
          {
            metric_name_selectors = [
              "^guardian_miden_rpc_duration_seconds$",
              "^guardian_miden_rpc_retries_total$",
              "^guardian_storage_operation_duration_seconds$",
            ]
            dimensions = [[]]
          },
          {
            metric_name_selectors = ["^guardian_db_pool_"]
            dimensions            = [["pool"]]
          },
          {
            metric_name_selectors = [
              "^guardian_canonicalization_runs_total$",
              "^guardian_canonicalization_fast_runs_total$",
              "^guardian_canonicalization_reconcile_runs_total$",
              "^guardian_canonicalization_candidates_total$",
            ]
            dimensions = [["outcome"]]
          },
          {
            metric_name_selectors = [
              "^guardian_canonicalization_run_duration_seconds$",
              "^guardian_canonicalization_fast_run_duration_seconds$",
              "^guardian_canonicalization_reconcile_run_duration_seconds$",
              "^guardian_canonicalization_candidate_age_seconds$",
              "^guardian_canonicalization_retries_total$",
              "^guardian_canonicalization_commitment_mismatches_total$",
              "^guardian_canonicalization_deltas_fetched_total$",
              "^guardian_canonicalization_pass_accounts$",
            ]
            dimensions = [[]]
          },
          {
            metric_name_selectors = [
              "^guardian_deltas_submitted_total$",
              "^guardian_accounts_created_total$",
            ]
            dimensions = [["kind"]]
          },
          {
            metric_name_selectors = ["^guardian_proposals_total$"]
            dimensions            = [["event"]]
          },
          {
            metric_name_selectors = ["^guardian_deltas$"]
            dimensions            = [["status"]]
          },
          {
            metric_name_selectors = [
              "^guardian_proposals_in_flight$",
              "^guardian_accounts$",
              "^guardian_operator_sessions_started_total$",
              "^guardian_metrics_refresh_failures_total$",
              "^guardian_metrics_refresh_timestamp_seconds$",
            ]
            dimensions = [[]]
          },
          {
            metric_name_selectors = [
              "^guardian_operator_auth_challenges_total$",
              "^guardian_operator_auth_verifications_total$",
            ]
            dimensions = [["outcome"]]
          },
          {
            metric_name_selectors = ["^guardian_rate_limit_rejections_total$"]
            dimensions            = [["transport"]]
          },
        ]
      }
    }
    service = {
      extensions = ["health_check"]
      pipelines = {
        metrics = {
          receivers  = ["prometheus"]
          processors = ["resourcedetection", "batch"]
          exporters  = ["awsemf"]
        }
      }
    }
  })
}

# --- Dashboard -------------------------------------------------------------
# SEARCH schema terms quote the namespace: hyphenated stack names (e.g.
# guardian-prod -> Guardian-Prod/Server) are invalid as unquoted tokens.

resource "aws_cloudwatch_dashboard" "server" {
  count = var.metrics_enabled ? 1 : 0

  dashboard_name = local.dashboard_name

  dashboard_body = jsonencode({
    widgets = [
      # --- Row 0: request volume, errors, latency --------------------------
      {
        type = "metric", x = 0, y = 0, width = 8, height = 6
        properties = {
          title  = "HTTP requests by status"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",status} MetricName=\"guardian_http_requests_total\"', 'Sum', 300)", id = "e1" }],
            ["${local.metrics_namespace}", "guardian_http_requests_total", { stat = "Sum", label = "total" }],
          ]
        }
      },
      {
        type = "metric", x = 8, y = 0, width = 8, height = 6
        properties = {
          title  = "Error rate (%)"
          region = var.aws_region, view = "timeSeries", period = 300
          yAxis  = { left = { min = 0 } }
          metrics = concat(
            [
              [{ expression = local.http_error_rate_expression, label = "HTTP 5xx %", id = "httpErr" }],
              [{ expression = local.grpc_error_rate_expression, label = "gRPC error %", id = "grpcErr" }],
            ],
            [for s in local.http_error_statuses : ["${local.metrics_namespace}", "guardian_http_requests_total", "status", s, { id = "h${s}", stat = "Sum", visible = false }]],
            [["${local.metrics_namespace}", "guardian_http_requests_total", { id = "hall", stat = "Sum", visible = false }]],
            [for c in local.grpc_error_codes : ["${local.metrics_namespace}", "guardian_grpc_requests_total", "code", c, { id = "g${replace(c, "_", "")}", stat = "Sum", visible = false }]],
            [["${local.metrics_namespace}", "guardian_grpc_requests_total", { id = "gall", stat = "Sum", visible = false }]],
          )
        }
      },
      {
        type = "metric", x = 16, y = 0, width = 8, height = 6
        properties = {
          title  = "Request latency (avg seconds)"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            ["${local.metrics_namespace}", "guardian_http_request_duration_seconds", { stat = "Average", label = "HTTP avg" }],
            ["${local.metrics_namespace}", "guardian_grpc_request_duration_seconds", { stat = "Average", label = "gRPC avg" }],
          ]
        }
      },
      # --- Row 1: gRPC detail, in-flight, guards ----------------------------
      {
        type = "metric", x = 0, y = 6, width = 8, height = 6
        properties = {
          title  = "gRPC requests by code"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",code} MetricName=\"guardian_grpc_requests_total\"', 'Sum', 300)", id = "e1" }],
          ]
        }
      },
      {
        type = "metric", x = 8, y = 6, width = 8, height = 6
        properties = {
          title  = "In-flight requests"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            ["${local.metrics_namespace}", "guardian_http_requests_in_flight", { stat = "Maximum", label = "HTTP" }],
            ["${local.metrics_namespace}", "guardian_grpc_requests_in_flight", { stat = "Maximum", label = "gRPC" }],
          ]
        }
      },
      {
        type = "metric", x = 16, y = 6, width = 8, height = 6
        properties = {
          title  = "Rate limiting & metrics refresher"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",transport} MetricName=\"guardian_rate_limit_rejections_total\"', 'Sum', 300)", id = "e1", label = "rejections" }],
            ["${local.metrics_namespace}", "guardian_metrics_refresh_failures_total", { stat = "Sum", label = "refresh failures" }],
          ]
        }
      },
      # --- Row 2: proposal / delta lifecycle --------------------------------
      {
        type = "metric", x = 0, y = 12, width = 8, height = 6
        properties = {
          title  = "Proposal lifecycle events"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",event} MetricName=\"guardian_proposals_total\"', 'Sum', 300)", id = "e1" }],
          ]
        }
      },
      {
        type = "metric", x = 8, y = 12, width = 8, height = 6
        properties = {
          title  = "Proposals in flight & accounts"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            ["${local.metrics_namespace}", "guardian_proposals_in_flight", { stat = "Maximum", label = "proposals in flight" }],
            ["${local.metrics_namespace}", "guardian_accounts", { stat = "Maximum", label = "accounts" }],
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",kind} MetricName=\"guardian_accounts_created_total\"', 'Sum', 300)", id = "e1", label = "created" }],
          ]
        }
      },
      {
        type = "metric", x = 16, y = 12, width = 8, height = 6
        properties = {
          title  = "Deltas"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",status} MetricName=\"guardian_deltas\"', 'Maximum', 300)", id = "e1" }],
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",kind} MetricName=\"guardian_deltas_submitted_total\"', 'Sum', 300)", id = "e2", label = "submitted" }],
          ]
        }
      },
      # --- Row 3: canonicalization health -----------------------------------
      {
        type = "metric", x = 0, y = 18, width = 8, height = 6
        properties = {
          title  = "Canonicalization runs by outcome"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_canonicalization_runs_total\"', 'Sum', 300)", id = "e1", label = "full" }],
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_canonicalization_fast_runs_total\"', 'Sum', 300)", id = "e2", label = "fast" }],
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_canonicalization_reconcile_runs_total\"', 'Sum', 300)", id = "e3", label = "reconcile" }],
          ]
        }
      },
      {
        type = "metric", x = 8, y = 18, width = 8, height = 6
        properties = {
          title  = "Canonicalization candidates"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_canonicalization_candidates_total\"', 'Sum', 300)", id = "e1" }],
            ["${local.metrics_namespace}", "guardian_canonicalization_deltas_fetched_total", { stat = "Sum", label = "deltas fetched" }],
            ["${local.metrics_namespace}", "guardian_canonicalization_commitment_mismatches_total", { stat = "Sum", label = "commitment mismatches" }],
            ["${local.metrics_namespace}", "guardian_canonicalization_retries_total", { stat = "Sum", label = "retries" }],
          ]
        }
      },
      {
        type = "metric", x = 16, y = 18, width = 8, height = 6
        properties = {
          title  = "Canonicalization durations & candidate age (avg s)"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            ["${local.metrics_namespace}", "guardian_canonicalization_run_duration_seconds", { stat = "Average", label = "full pass avg" }],
            ["${local.metrics_namespace}", "guardian_canonicalization_fast_run_duration_seconds", { stat = "Average", label = "fast pass avg" }],
            ["${local.metrics_namespace}", "guardian_canonicalization_reconcile_run_duration_seconds", { stat = "Average", label = "reconcile pass avg" }],
            ["${local.metrics_namespace}", "guardian_canonicalization_candidate_age_seconds", { stat = "Average", label = "candidate age avg" }],
          ]
        }
      },
      # --- Row 4: storage health --------------------------------------------
      {
        type = "metric", x = 0, y = 24, width = 8, height = 6
        properties = {
          title  = "Storage operations by outcome"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_storage_operations_total\"', 'Sum', 300)", id = "e1" }],
          ]
        }
      },
      {
        type = "metric", x = 8, y = 24, width = 8, height = 6
        properties = {
          title  = "Storage operation latency (avg s)"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            ["${local.metrics_namespace}", "guardian_storage_operation_duration_seconds", { stat = "Average", label = "avg" }],
          ]
        }
      },
      {
        type = "metric", x = 16, y = 24, width = 8, height = 6
        properties = {
          title  = "DB pools"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",pool} MetricName=\"guardian_db_pool_connections\"', 'Maximum', 300)", id = "e1", label = "connections" }],
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",pool} MetricName=\"guardian_db_pool_pending_acquires\"', 'Maximum', 300)", id = "e2", label = "pending acquires" }],
          ]
        }
      },
      # --- Row 5: upstream chain node & ECS ---------------------------------
      {
        type = "metric", x = 0, y = 30, width = 8, height = 6
        properties = {
          title  = "Miden RPC by outcome"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_miden_rpc_requests_total\"', 'Sum', 300)", id = "e1" }],
            ["${local.metrics_namespace}", "guardian_miden_rpc_retries_total", { stat = "Sum", label = "retries" }],
          ]
        }
      },
      {
        type = "metric", x = 8, y = 30, width = 8, height = 6
        properties = {
          title  = "Miden RPC latency (avg s)"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            ["${local.metrics_namespace}", "guardian_miden_rpc_duration_seconds", { stat = "Average", label = "avg" }],
          ]
        }
      },
      {
        type = "metric", x = 16, y = 30, width = 8, height = 6
        properties = {
          title  = "ECS CPU & memory (%)"
          region = var.aws_region, view = "timeSeries", period = 300
          yAxis  = { left = { min = 0, max = 100 } }
          metrics = [
            ["AWS/ECS", "CPUUtilization", "ClusterName", local.cluster_name, "ServiceName", local.server_service_name, { stat = "Average", label = "CPU avg" }],
            ["AWS/ECS", "MemoryUtilization", "ClusterName", local.cluster_name, "ServiceName", local.server_service_name, { stat = "Average", label = "memory avg" }],
          ]
        }
      },
      # --- Row 6: fleet size & operator auth ---------------------------------
      {
        type = "metric", x = 0, y = 36, width = 8, height = 6
        properties = {
          title  = "ECS tasks"
          region = var.aws_region, view = "timeSeries", period = 300
          metrics = [
            ["ECS/ContainerInsights", "RunningTaskCount", "ClusterName", local.cluster_name, "ServiceName", local.server_service_name, { stat = "Average", label = "running" }],
            ["ECS/ContainerInsights", "DesiredTaskCount", "ClusterName", local.cluster_name, "ServiceName", local.server_service_name, { stat = "Average", label = "desired" }],
          ]
        }
      },
      {
        type = "metric", x = 8, y = 36, width = 8, height = 6
        properties = {
          title  = "Operator auth"
          region = var.aws_region, view = "timeSeries", stat = "Sum", period = 300
          metrics = [
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_operator_auth_challenges_total\"', 'Sum', 300)", id = "e1", label = "challenges" }],
            [{ expression = "SEARCH('{\"${local.metrics_namespace}\",outcome} MetricName=\"guardian_operator_auth_verifications_total\"', 'Sum', 300)", id = "e2", label = "verifications" }],
            ["${local.metrics_namespace}", "guardian_operator_sessions_started_total", { stat = "Sum", label = "sessions started" }],
          ]
        }
      },
    ]
  })
}

# --- Alarms ----------------------------------------------------------------
# All application-metric alarms treat missing data as notBreaching except
# the metrics-missing alarm, whose entire job is to catch the pipeline
# going dark (server metrics disabled, sidecar dead, or scrape failing).

resource "aws_cloudwatch_metric_alarm" "http_error_rate" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-http-5xx-rate"
  alarm_description   = "Guardian HTTP 5xx responses exceed ${var.alarm_error_rate_threshold_percent}% of requests"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.alarm_error_rate_threshold_percent
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions

  metric_query {
    id          = "errorRate"
    expression  = local.http_error_rate_expression
    label       = "HTTP 5xx %"
    return_data = true
  }

  dynamic "metric_query" {
    for_each = toset(local.http_error_statuses)
    content {
      id = "h${metric_query.value}"
      metric {
        namespace   = local.metrics_namespace
        metric_name = "guardian_http_requests_total"
        dimensions  = { status = metric_query.value }
        stat        = "Sum"
        period      = 300
      }
    }
  }

  metric_query {
    id = "hall"
    metric {
      namespace   = local.metrics_namespace
      metric_name = "guardian_http_requests_total"
      stat        = "Sum"
      period      = 300
    }
  }
}

resource "aws_cloudwatch_metric_alarm" "grpc_error_rate" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-grpc-error-rate"
  alarm_description   = "Guardian gRPC server-fault responses (${join(", ", local.grpc_error_codes)}) exceed ${var.alarm_error_rate_threshold_percent}% of requests"
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.alarm_error_rate_threshold_percent
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions

  metric_query {
    id          = "errorRate"
    expression  = local.grpc_error_rate_expression
    label       = "gRPC error %"
    return_data = true
  }

  dynamic "metric_query" {
    for_each = toset(local.grpc_error_codes)
    content {
      id = "g${replace(metric_query.value, "_", "")}"
      metric {
        namespace   = local.metrics_namespace
        metric_name = "guardian_grpc_requests_total"
        dimensions  = { code = metric_query.value }
        stat        = "Sum"
        period      = 300
      }
    }
  }

  metric_query {
    id = "gall"
    metric {
      namespace   = local.metrics_namespace
      metric_name = "guardian_grpc_requests_total"
      stat        = "Sum"
      period      = 300
    }
  }
}

# Fleet-wide average with route/method rolled up, so continuous ALB
# health-check probes dilute it on low-traffic stacks — treat this as a
# sustained-degradation signal, not a per-request SLO. Per-route latency
# needs the route dimension exported (deliberately not, for cost).
resource "aws_cloudwatch_metric_alarm" "http_latency" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-http-latency"
  alarm_description   = "Guardian average HTTP request latency exceeds ${var.alarm_latency_threshold_seconds}s (fleet average across all routes, including ALB health checks)"
  namespace           = local.metrics_namespace
  metric_name         = "guardian_http_request_duration_seconds"
  statistic           = "Average"
  period              = 300
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.alarm_latency_threshold_seconds
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions
}

resource "aws_cloudwatch_metric_alarm" "canonicalization_failures" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-canonicalization-failures"
  alarm_description   = "Guardian canonicalization passes (full, fast, or reconcile) are erroring or completing with failed accounts"
  comparison_operator = "GreaterThanThreshold"
  threshold           = 0
  evaluation_periods  = 2
  datapoints_to_alarm = 2
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions

  metric_query {
    id          = "failures"
    expression  = local.canonicalization_failures_expression
    label       = "failed canonicalization passes"
    return_data = true
  }

  dynamic "metric_query" {
    for_each = local.canonicalization_failure_queries
    content {
      id = metric_query.key
      metric {
        namespace   = local.metrics_namespace
        metric_name = metric_query.value.metric
        dimensions  = { outcome = metric_query.value.outcome }
        stat        = "Sum"
        period      = 300
      }
    }
  }
}

# guardian_build_info is a constant-1 gauge emitted from metrics-listener
# startup, so it produces a datapoint on every scrape regardless of
# traffic; it disappearing means the pipeline is down. Maximum is never
# negative, so LessThanThreshold(0) only fires via treat_missing_data =
# breaching. Fleet-level canary: it catches the pipeline going fully
# dark; one dead sidecar in a multi-task fleet only lowers the metric's
# SampleCount and is not detected here.
resource "aws_cloudwatch_metric_alarm" "metrics_missing" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-metrics-missing"
  alarm_description   = "Guardian application metrics stopped arriving in CloudWatch (metrics endpoint down, ADOT sidecar dead, or scrape failing — check the adot log stream)"
  namespace           = local.metrics_namespace
  metric_name         = "guardian_build_info"
  statistic           = "Maximum"
  period              = 300
  comparison_operator = "LessThanThreshold"
  threshold           = 0
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "breaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions
}

resource "aws_cloudwatch_metric_alarm" "metrics_refresh_failures" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-metrics-refresh-failures"
  alarm_description   = "Guardian slow-aggregate metrics refresher attempts are failing; delta/proposal/account gauges are stale"
  namespace           = local.metrics_namespace
  metric_name         = "guardian_metrics_refresh_failures_total"
  statistic           = "Sum"
  period              = 300
  comparison_operator = "GreaterThanThreshold"
  threshold           = 0
  evaluation_periods  = 2
  datapoints_to_alarm = 2
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions
}

# Complements the failures alarm: a refresher that hangs (or dies) makes
# no attempts, so the failures counter never moves — but the last-success
# timestamp stops advancing. A healthy refresher (30s cadence) advances
# the per-period Maximum by ~300 each period; DIFF <= 0 for two periods
# means no successful refresh for >= 10 minutes.
resource "aws_cloudwatch_metric_alarm" "metrics_refresh_stale" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name          = "${var.stack_name}-metrics-refresh-stale"
  alarm_description   = "Guardian slow-aggregate refresh timestamp stopped advancing; delta/proposal/account gauges are stale (hung or dead refresher)"
  comparison_operator = "LessThanOrEqualToThreshold"
  threshold           = 0
  evaluation_periods  = 2
  datapoints_to_alarm = 2
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions

  metric_query {
    id          = "staleness"
    expression  = "DIFF(ts)"
    label       = "refresh timestamp advance per period (s)"
    return_data = true
  }

  metric_query {
    id = "ts"
    metric {
      namespace   = local.metrics_namespace
      metric_name = "guardian_metrics_refresh_timestamp_seconds"
      stat        = "Maximum"
      period      = 300
    }
  }
}

resource "aws_cloudwatch_metric_alarm" "ecs_cpu_high" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name        = "${var.stack_name}-ecs-cpu-high"
  alarm_description = "Guardian ECS service average CPU utilization exceeds ${var.alarm_cpu_threshold_percent}%"
  namespace         = "AWS/ECS"
  metric_name       = "CPUUtilization"
  dimensions = {
    ClusterName = local.cluster_name
    ServiceName = local.server_service_name
  }
  statistic           = "Average"
  period              = 300
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.alarm_cpu_threshold_percent
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions

  lifecycle {
    precondition {
      condition = (
        !local.effective_server_autoscaling_enabled ||
        var.alarm_cpu_threshold_percent > local.effective_server_autoscaling_cpu_target
      )
      error_message = "alarm_cpu_threshold_percent must exceed the autoscaling CPU target so scaling reacts before the alarm fires."
    }
  }
}

resource "aws_cloudwatch_metric_alarm" "ecs_memory_high" {
  count = var.metrics_enabled ? 1 : 0

  alarm_name        = "${var.stack_name}-ecs-memory-high"
  alarm_description = "Guardian ECS service average memory utilization exceeds ${var.alarm_memory_threshold_percent}%"
  namespace         = "AWS/ECS"
  metric_name       = "MemoryUtilization"
  dimensions = {
    ClusterName = local.cluster_name
    ServiceName = local.server_service_name
  }
  statistic           = "Average"
  period              = 300
  comparison_operator = "GreaterThanThreshold"
  threshold           = var.alarm_memory_threshold_percent
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "notBreaching"
  alarm_actions       = var.alarm_actions
  ok_actions          = var.alarm_actions

  lifecycle {
    precondition {
      condition = (
        !local.effective_server_autoscaling_enabled ||
        var.alarm_memory_threshold_percent > local.effective_server_autoscaling_memory_target
      )
      error_message = "alarm_memory_threshold_percent must exceed the autoscaling memory target so scaling reacts before the alarm fires."
    }
  }
}
