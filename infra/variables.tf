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
  description = "ECR image URI for guardian-server (e.g., 123456789012.dkr.ecr.us-east-1.amazonaws.com/guardian-server:latest)"
  type        = string
}

variable "server_network_type" {
  description = "Miden network for the GUARDIAN server runtime (MidenTestnet, MidenDevnet, or MidenLocal)"
  type        = string
  default     = "MidenTestnet"
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
  description = "Root domain name for the HTTPS endpoint (e.g., openzeppelin.com)"
  type        = string
  default     = "openzeppelin.com"
}

variable "subdomain" {
  description = "Subdomain for the service (e.g., guardian -> guardian.openzeppelin.com). Empty uses the root domain."
  type        = string
  default     = "guardian"
}

variable "acm_certificate_arn" {
  description = "ACM certificate ARN for the service domain (e.g., guardian-stg.openzeppelin.com)"
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
  description = "RDS instance class for the managed PostgreSQL database"
  type        = string
  default     = "db.t3.micro"
}

variable "rds_allocated_storage" {
  description = "Allocated RDS storage in GiB"
  type        = number
  default     = 20
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

variable "rds_backup_retention_days" {
  description = "Backup retention in days for RDS"
  type        = number
  default     = 7
}

variable "rds_deletion_protection" {
  description = "Whether to enable deletion protection for RDS"
  type        = bool
  default     = false
}

variable "rds_skip_final_snapshot" {
  description = "Whether to skip the final snapshot when destroying RDS"
  type        = bool
  default     = true
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

variable "guardian_rate_burst_per_sec" {
  description = "Optional override for the Guardian HTTP burst rate limit"
  type        = number
  default     = null
}

variable "guardian_rate_per_min" {
  description = "Optional override for the Guardian HTTP sustained rate limit"
  type        = number
  default     = null
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
