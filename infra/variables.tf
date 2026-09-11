variable "aws_region" {
  description = "AWS region for deployment"
  type        = string
  default     = "us-east-1"
}

variable "cpu_architecture" {
  description = "CPU architecture for ECS tasks and the server image (X86_64 or ARM64)"
  type        = string
  default     = "X86_64"

  validation {
    condition     = contains(["X86_64", "ARM64"], var.cpu_architecture)
    error_message = "cpu_architecture must be X86_64 or ARM64."
  }
}

variable "stack_name" {
  description = "Base name for the deployment stack (e.g., guardian or psm)"
  type        = string
  default     = "guardian"
}

variable "deployment_stage" {
  description = "Deployment stage profile (dev or prod)"
  type        = string
  default     = "dev"

  validation {
    condition     = contains(["dev", "prod"], var.deployment_stage)
    error_message = "deployment_stage must be dev or prod."
  }
}

variable "server_image_uri" {
  description = "ECR image URI for guardian-server, including either a tag or an immutable digest"
  type        = string
}

variable "server_network_type" {
  description = "Miden network for the GUARDIAN server runtime (MidenTestnet, MidenDevnet, or MidenLocal)"
  type        = string
  default     = "MidenTestnet"
}

variable "guardian_cors_allowed_origins" {
  description = "Comma-separated explicit HTTP origins allowed by Guardian CORS"
  type        = string
  default     = ""
}

variable "vpc_id" {
  description = "VPC ID. If not specified, uses the default VPC"
  type        = string
  default     = ""
}

variable "subnet_ids" {
  description = "Subnet IDs for ECS tasks and ALB. If not specified, uses all subnets in the VPC"
  type        = list(string)
  default     = []
}

variable "rds_proxy_subnet_ids" {
  description = "Subnet IDs for RDS Proxy. If not specified, uses the shared subnet_ids after filtering region-specific unsupported RDS Proxy AZs"
  type        = list(string)
  default     = []
}

variable "postgres_db" {
  description = "Postgres database name"
  type        = string
  default     = ""
}

variable "postgres_user" {
  description = "Postgres username"
  type        = string
  default     = ""
}

variable "postgres_password" {
  description = "Postgres password"
  type        = string
  default     = ""
  sensitive   = true
}

variable "domain_name" {
  description = "Root domain name for the canonical HTTPS endpoint (e.g., example.com)"
  type        = string
  default     = "openzeppelin.com"
}

variable "subdomain" {
  description = "Subdomain for the canonical service hostname (e.g., guardian -> guardian.example.com). Empty uses the root domain."
  type        = string
  default     = "guardian"
}

variable "acm_certificate_arn" {
  description = "ACM certificate ARN for the canonical service hostname"
  type        = string
  default     = ""
}

variable "alias_subdomain" {
  description = "Migration-only legacy subdomain under domain_name pointing to the same ALB. Terraform manages its DNS record only when a DNS provider is configured; external DNS is supported. Leave empty for normal deployments."
  type        = string
  default     = ""
}

variable "alias_acm_certificate_arn" {
  description = "Migration-only ACM certificate ARN for the legacy hostname. When empty, acm_certificate_arn is reused and must cover both names."
  type        = string
  default     = ""
}

variable "route53_zone_id" {
  description = "Existing Route 53 hosted zone ID for the domain"
  type        = string
  default     = ""
}

variable "cloudflare_api_token" {
  description = "Cloudflare API token used to manage DNS"
  type        = string
  default     = ""
  sensitive   = true
}

variable "cloudflare_zone_id" {
  description = "Cloudflare zone ID for the domain"
  type        = string
  default     = ""
}

variable "cloudflare_proxied" {
  description = "Whether Cloudflare should proxy the DNS record"
  type        = bool
  default     = true
}

variable "alb_ingress_cidrs" {
  description = "CIDR blocks allowed to reach the ALB (used for ports 80/443)"
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "log_retention_days" {
  description = "CloudWatch log retention in days"
  type        = number
  default     = 7
}

variable "server_cpu" {
  description = "Server task CPU units"
  type        = number
  default     = 512
}

variable "server_memory" {
  description = "Server task memory (MB)"
  type        = number
  default     = 1024
}

variable "server_desired_count" {
  description = "Optional override for the ECS service desired task count"
  type        = number
  default     = null
}

variable "server_autoscaling_enabled" {
  description = "Optional override to enable ECS service autoscaling"
  type        = bool
  default     = null
}

variable "server_autoscaling_min_capacity" {
  description = "Optional override for the ECS service autoscaling minimum task count"
  type        = number
  default     = null
}

variable "server_autoscaling_max_capacity" {
  description = "Optional override for the ECS service autoscaling maximum task count"
  type        = number
  default     = null
}

variable "server_autoscaling_cpu_target" {
  description = "Optional override for the ECS service CPU target-tracking percentage"
  type        = number
  default     = null
}

variable "server_autoscaling_memory_target" {
  description = "Optional override for the ECS service memory target-tracking percentage"
  type        = number
  default     = null
}

variable "rds_instance_class" {
  description = "Optional override for the RDS instance class for the managed PostgreSQL database"
  type        = string
  default     = ""
}

variable "rds_allocated_storage" {
  description = "Optional override for allocated RDS storage in GiB"
  type        = number
  default     = null
}

variable "rds_max_allocated_storage" {
  description = "Optional maximum allocated RDS storage in GiB for storage autoscaling"
  type        = number
  default     = null
}

variable "rds_engine_version" {
  description = "Optional PostgreSQL engine version override for RDS"
  type        = string
  default     = ""
}

variable "ca_initializer_image" {
  description = <<-EOT
    Minimal image used by the CA-bundle init container to write the Secrets
    Manager bundle into the shared volume. Defaults to Alpine from the public ECR
    mirror (avoids Docker Hub rate limits), digest-pinned for supply-chain
    consistency with the server Dockerfile. Refresh the digest with:
    `docker manifest inspect public.ecr.aws/docker/library/alpine:3.20`.
  EOT
  type        = string
  default     = "public.ecr.aws/docker/library/alpine:3.20@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc"
}

variable "rds_ca_bundle_secret_arn" {
  description = <<-EOT
    ARN of a Secrets Manager secret whose SecretString is a PEM CA bundle the
    server trusts when connecting to Postgres. When set, an init container writes
    the bundle into a shared in-task volume and the server DATABASE_URL uses
    sslmode=verify-full&sslrootcert=<mounted path> (authenticated TLS); when empty
    the server falls back to sslmode=require (encrypted, unverified).

    The published image ships no CA bundle (it stays provider-neutral); the bundle
    is delivered at deploy time via the init container, so nothing is baked into
    the image and the app never fetches it. For RDS the secret MUST contain BOTH
    the Amazon RDS CA roots AND the Amazon Trust Services roots (concatenated),
    because the RDS Proxy endpoint (prod default) presents an ACM certificate
    chaining to Amazon Trust Services while a direct instance chains to the RDS CA
    roots. Verification fails closed at startup if the bundle is missing or
    malformed.
  EOT
  type        = string
  default     = ""

  validation {
    condition = (
      var.rds_ca_bundle_secret_arn == "" ||
      can(regex("^arn:aws[a-z-]*:secretsmanager:[a-z0-9-]+:[0-9]{12}:secret:.+$", trimspace(var.rds_ca_bundle_secret_arn)))
    )
    error_message = "rds_ca_bundle_secret_arn must be empty or a valid Secrets Manager secret ARN."
  }
}

variable "rds_backup_retention_days" {
  description = "Backup retention in days for RDS"
  type        = number
  default     = 7
}

variable "rds_deletion_protection" {
  description = "Optional override for RDS deletion protection; defaults to true in prod, false otherwise"
  type        = bool
  default     = null
}

variable "rds_skip_final_snapshot" {
  description = "Optional override for skipping the final snapshot when destroying RDS; defaults to false in prod, true otherwise"
  type        = bool
  default     = null
}

variable "rds_multi_az" {
  description = "Whether the RDS instance runs as a Multi-AZ deployment with a standby replica"
  type        = bool
  default     = false
}

variable "rds_publicly_accessible" {
  description = "Whether the RDS instance should be publicly accessible"
  type        = bool
  default     = false
}

variable "rds_proxy_enabled" {
  description = "Optional override to enable RDS Proxy"
  type        = bool
  default     = null
}

variable "rds_proxy_route_database_url" {
  description = "Optional override to route the server DATABASE_URL secret through the RDS Proxy endpoint when the proxy exists"
  type        = bool
  default     = null
}

variable "guardian_rate_burst_per_sec" {
  description = "Optional override for the Guardian burst rate limit (HTTP and gRPC)"
  type        = number
  default     = null
}

variable "guardian_rate_per_min" {
  description = "Optional override for the Guardian sustained rate limit (HTTP and gRPC)"
  type        = number
  default     = null
}

variable "guardian_dashboard_commitment_rate_burst_per_sec" {
  description = "Optional override for the fleet-wide dashboard per-commitment burst rate limit"
  type        = number
  default     = null

  validation {
    condition = (
      var.guardian_dashboard_commitment_rate_burst_per_sec == null ||
      (
        var.guardian_dashboard_commitment_rate_burst_per_sec >= 1 &&
        var.guardian_dashboard_commitment_rate_burst_per_sec <= 4294967295 &&
        floor(var.guardian_dashboard_commitment_rate_burst_per_sec) == var.guardian_dashboard_commitment_rate_burst_per_sec
      )
    )
    error_message = "guardian_dashboard_commitment_rate_burst_per_sec must be an integer between 1 and 4294967295 when set."
  }
}

variable "guardian_dashboard_commitment_rate_per_min" {
  description = "Optional override for the fleet-wide dashboard per-commitment sustained rate limit"
  type        = number
  default     = null

  validation {
    condition = (
      var.guardian_dashboard_commitment_rate_per_min == null ||
      (
        var.guardian_dashboard_commitment_rate_per_min >= 1 &&
        var.guardian_dashboard_commitment_rate_per_min <= 4294967295 &&
        floor(var.guardian_dashboard_commitment_rate_per_min) == var.guardian_dashboard_commitment_rate_per_min
      )
    )
    error_message = "guardian_dashboard_commitment_rate_per_min must be an integer between 1 and 4294967295 when set."
  }
}

variable "guardian_max_replicas" {
  description = <<-EOT
    Optional override for GUARDIAN_MAX_REPLICAS, the maximum replica capacity the
    server divides rate limits by. Defaults to the greater of desired count and
    autoscaling maximum when autoscaling is enabled, or the desired count
    otherwise. Drives rate-limit partitioning only (coordination mode is
    backend-derived). An explicit override is clamped up to that steady-state
    capacity. Rolling deployments may temporarily allow up to
    deployment_maximum_percent / 100 times the configured fleet-wide limit.
  EOT
  type        = number
  default     = null

  validation {
    condition = (
      var.guardian_max_replicas == null ||
      (var.guardian_max_replicas >= 1 && floor(var.guardian_max_replicas) == var.guardian_max_replicas)
    )
    error_message = "guardian_max_replicas must be an integer >= 1 when set."
  }
}

variable "server_deployment_maximum_percent" {
  description = <<-EOT
    ECS deployment_maximum_percent for the server service: the ceiling, as a
    percentage of desired count, on tasks that may run concurrently during a
    rolling deploy. Because GUARDIAN_MAX_REPLICAS uses steady-state capacity,
    the fleet-wide rate allowance may temporarily scale by this percentage
    during a rollout. Must exceed 100 and, after ECS rounds the resulting task
    count down, allow at least one task above the minimum positive desired
    capacity (including autoscaling minimum capacity). The ECS service
    precondition enforces this because deployment_minimum_healthy_percent is
    100.
  EOT
  type        = number
  default     = 200

  validation {
    condition = (
      var.server_deployment_maximum_percent > 100 &&
      var.server_deployment_maximum_percent <= 200 &&
      floor(var.server_deployment_maximum_percent) == var.server_deployment_maximum_percent
    )
    error_message = "server_deployment_maximum_percent must be an integer in (100, 200]."
  }
}

variable "guardian_rate_limit_enabled" {
  description = "Optional override to enable or disable Guardian rate limiting (HTTP and gRPC)"
  type        = bool
  default     = null
}

variable "guardian_operator_public_keys_secret_arn" {
  description = "Secrets Manager secret ARN containing a JSON array of serialized Falcon public keys allowed to authenticate as dashboard operators"
  type        = string
  default     = ""
}

variable "guardian_evm_allowed_chain_ids_secret_arn" {
  description = "Secrets Manager secret ARN containing comma-separated EVM chain IDs allowed by the server"
  type        = string
  default     = ""
}

variable "guardian_ack_ecdsa_kms_key_arn" {
  description = "KMS key ARN for the hosted ECDSA ACK signer backend. When set, the ECS task is granted kms:Sign and kms:GetPublicKey on this key, and the ECDSA ACK secret in Secrets Manager is no longer required."
  type        = string
  default     = ""
}

variable "guardian_evm_allowed_chain_ids" {
  description = "Comma-separated EVM chain IDs allowed by the server; when set, Terraform creates a Secrets Manager secret containing this value"
  type        = string
  default     = ""
  sensitive   = true

  validation {
    condition     = var.guardian_evm_allowed_chain_ids == "" || can(regex("^\\s*[0-9]+(\\s*,\\s*[0-9]+)*\\s*$", var.guardian_evm_allowed_chain_ids))
    error_message = "guardian_evm_allowed_chain_ids must be a comma-separated list of numeric chain IDs."
  }
}

variable "guardian_evm_rpc_urls_secret_arn" {
  description = "Secrets Manager secret ARN containing comma-separated chain_id=url EVM RPC entries"
  type        = string
  default     = ""
}

variable "guardian_ack_falcon_secret_name" {
  description = "Secrets Manager secret name holding the Falcon ACK signing key (prod only). Defaults to $${stack_name}/server/ack-falcon-secret-key when empty; override to pin a stack at a pre-existing legacy secret name."
  type        = string
  default     = ""
}

variable "guardian_ack_ecdsa_secret_name" {
  description = "Secrets Manager secret name holding the ECDSA ACK signing key (prod only). Defaults to $${stack_name}/server/ack-ecdsa-secret-key when empty; override to pin a stack at a pre-existing legacy secret name."
  type        = string
  default     = ""
}

variable "guardian_storage_encryption_secret_name" {
  description = "Secrets Manager secret name holding the storage encryption key document ({active, keys}). When set (prod only), the ECS task is granted secretsmanager:GetSecretValue on it and GUARDIAN_STORAGE_ENCRYPTION_KEY_SECRET_ID is injected, enabling encryption at rest. Empty leaves storage in plaintext at rest."
  type        = string
  default     = ""
}

variable "guardian_dashboard_cursor_secret_name" {
  description = "Secrets Manager secret name holding the 64-hex dashboard pagination cursor secret (prod only). Defaults to $${stack_name}/server/dashboard-cursor-secret when empty. The secret must exist before planning or deploying prod."
  type        = string
  default     = ""
}

variable "guardian_evm_rpc_urls" {
  description = "Comma-separated chain_id=url EVM RPC entries; when set, Terraform creates a Secrets Manager secret containing this value"
  type        = string
  default     = ""
  sensitive   = true
}

variable "guardian_evm_entrypoint_address" {
  description = "Shared EVM EntryPoint address used for every configured EVM chain"
  type        = string
  default     = ""

  validation {
    condition     = var.guardian_evm_entrypoint_address == "" || can(regex("^0x[0-9a-fA-F]{40}$", var.guardian_evm_entrypoint_address))
    error_message = "guardian_evm_entrypoint_address must be a 20-byte 0x-prefixed hex address."
  }
}

variable "guardian_operator_public_keys" {
  description = "Serialized Falcon public keys allowed to authenticate as dashboard operators; when set, Terraform creates a Secrets Manager secret containing this JSON array"
  type        = list(string)
  default     = []
}

variable "guardian_db_pool_max_size" {
  description = "Optional override for the Guardian storage DB pool maximum size"
  type        = number
  default     = null
}

variable "guardian_metadata_db_pool_max_size" {
  description = "Optional override for the Guardian metadata DB pool maximum size"
  type        = number
  default     = null
}

variable "guardian_canonicalization_max_concurrent_accounts" {
  description = <<-EOT
    Optional override for GUARDIAN_CANONICALIZATION_MAX_CONCURRENT_ACCOUNTS, how many
    accounts one canonicalization pass processes in parallel (1 = fully sequential).
    Defaults to 50 in prod (which runs behind RDS Proxy), 10 otherwise. Most
    of each account's wall clock is a chain RPC that holds no DB connection,
    so this may exceed guardian_db_pool_max_size; write bursts queue briefly
    at the pool instead of failing.
  EOT
  type        = number
  default     = null
  validation {
    condition = var.guardian_canonicalization_max_concurrent_accounts == null ? true : (
      var.guardian_canonicalization_max_concurrent_accounts >= 1 &&
      floor(var.guardian_canonicalization_max_concurrent_accounts) ==
      var.guardian_canonicalization_max_concurrent_accounts
    )
    error_message = "guardian_canonicalization_max_concurrent_accounts must be a positive integer when provided."
  }
}

variable "guardian_canonicalization_fast_promotion_enabled" {
  description = "Whether ECS enables the recent-candidate fast promotion pass"
  type        = bool
  default     = true
}

variable "guardian_log_format" {
  description = "Log output format for GUARDIAN_LOG_FORMAT (text, json, compact). json enables flattened JSON for CloudWatch Logs Insights"
  type        = string
  default     = "json"

  validation {
    condition     = contains(["text", "json", "compact"], lower(trimspace(var.guardian_log_format)))
    error_message = "guardian_log_format must be text, json, or compact."
  }
}

variable "guardian_metrics_enabled" {
  description = <<-EOT
    Whether the Guardian server exposes its Prometheus metrics endpoint inside
    the ECS task. The endpoint binds loopback inside the task's network
    namespace and is never reachable via the ALB or security groups; an
    externally scraped setup would additionally require an explicit bind
    address, restricted security-group access, and a bearer token — none of
    which this module configures. Disabling also disables the CloudWatch
    export pipeline (there is nothing to scrape).
  EOT
  type        = bool
  default     = true
}

variable "cloudwatch_metrics_enabled" {
  description = <<-EOT
    Whether the deployment ships the metrics endpoint's data to CloudWatch:
    runs the ADOT Collector sidecar that scrapes it and exports EMF metrics,
    and creates the EMF log group, IAM policy, CloudWatch dashboard, and
    alarms. Effective only while guardian_metrics_enabled is true — the
    export pipeline cascades off with the endpoint, so disabling the
    endpoint alone turns everything off. Disable just this flag to keep the
    (loopback-only) endpoint without publishing CloudWatch custom metrics —
    useful only for an alternative in-task collector unless the module is
    customized with a routable bind address, security-group access, and a
    bearer token.
  EOT
  type        = bool
  default     = true
}

variable "adot_image" {
  description = <<-EOT
    AWS Distro for OpenTelemetry Collector image for the metrics sidecar,
    digest-pinned for supply-chain consistency with the server Dockerfile.
    Refresh the digest with:
    `docker manifest inspect public.ecr.aws/aws-observability/aws-otel-collector:<tag>`.
    When bumping, verify metric-name normalization stays off (the
    receiver config pins `trim_metric_suffixes = false`; check the gate
    has not been renamed or force-enabled): with normalization on,
    counters lose their `_total` suffix and stop matching the awsemf
    declarations, silently blanking counter widgets and starving every
    notBreaching alarm — while `guardian_build_info` (a gauge, no
    suffix) keeps the metrics-missing heartbeat green. Verify with
    `aws cloudwatch list-metrics` after any image bump; metrics-missing
    does NOT catch partial selection drift.
  EOT
  type        = string
  default     = "public.ecr.aws/aws-observability/aws-otel-collector:v0.49.0@sha256:d2bdfff2c377c3d71d78bd5d9ce9862fd535b12134a5739d87a07801297cf9fd"
}

variable "metrics_namespace" {
  description = "CloudWatch namespace for Guardian application metrics. Defaults to <Title(stack_name)>/Server (e.g. Guardian/Server), keeping stacks in the same account separate."
  type        = string
  default     = ""
}

variable "alarm_actions" {
  description = "ARNs (e.g. SNS topics) notified when a Guardian CloudWatch alarm transitions to ALARM or back to OK. Empty leaves alarms visible in the console only."
  type        = list(string)
  default     = []
}

variable "alarm_error_rate_threshold_percent" {
  description = "Error-rate percentage above which the HTTP 5xx and gRPC error alarms fire"
  type        = number
  default     = 5

  validation {
    condition     = var.alarm_error_rate_threshold_percent > 0 && var.alarm_error_rate_threshold_percent <= 100
    error_message = "alarm_error_rate_threshold_percent must be in (0, 100]."
  }
}

variable "alarm_latency_threshold_seconds" {
  description = "Average HTTP request latency in seconds above which the latency alarm fires"
  type        = number
  default     = 1

  validation {
    condition     = var.alarm_latency_threshold_seconds > 0
    error_message = "alarm_latency_threshold_seconds must be positive."
  }
}

variable "alarm_cpu_threshold_percent" {
  description = "ECS service average CPU utilization percentage above which the saturation alarm fires. Keep above the autoscaling CPU target so scaling reacts first."
  type        = number
  default     = 85

  validation {
    condition     = var.alarm_cpu_threshold_percent > 0 && var.alarm_cpu_threshold_percent <= 100
    error_message = "alarm_cpu_threshold_percent must be in (0, 100]."
  }
}

variable "alarm_memory_threshold_percent" {
  description = "ECS service average memory utilization percentage above which the saturation alarm fires. Keep above the autoscaling memory target so scaling reacts first."
  type        = number
  default     = 90

  validation {
    condition     = var.alarm_memory_threshold_percent > 0 && var.alarm_memory_threshold_percent <= 100
    error_message = "alarm_memory_threshold_percent must be in (0, 100]."
  }
}

# Resource naming
variable "cluster_name" {
  description = "ECS cluster name"
  type        = string
  default     = ""
}

variable "server_service_name" {
  description = "Server ECS service name"
  type        = string
  default     = ""
}

variable "alb_name" {
  description = "ALB name"
  type        = string
  default     = ""
}

variable "target_group_name" {
  description = "ALB target group name for the server"
  type        = string
  default     = ""
}

variable "alb_security_group_name" {
  description = "Security group name for the ALB"
  type        = string
  default     = ""
}

variable "server_security_group_name" {
  description = "Security group name for the server service"
  type        = string
  default     = ""
}

variable "postgres_security_group_name" {
  description = "Security group name for the managed PostgreSQL database"
  type        = string
  default     = ""
}

variable "task_execution_role_name" {
  description = "IAM role name for ECS task execution"
  type        = string
  default     = ""
}

variable "task_role_name" {
  description = "IAM role name for ECS task runtime"
  type        = string
  default     = ""
}

variable "server_task_family" {
  description = "Task definition family name for the server"
  type        = string
  default     = ""
}

variable "server_container_name" {
  description = "Container name for the server task definition"
  type        = string
  default     = ""
}

variable "server_log_group_name" {
  description = "CloudWatch log group name for the server"
  type        = string
  default     = ""
}
