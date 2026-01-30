variable "aws_region" {
  description = "AWS region for deployment"
  type        = string
  default     = "us-east-1"
}

variable "server_image_uri" {
  description = "ECR image URI for psm-server (e.g., 123456789012.dkr.ecr.us-east-1.amazonaws.com/psm-server:latest)"
  type        = string
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
  default     = "psm"
}

variable "postgres_user" {
  description = "Postgres username"
  type        = string
  default     = "psm"
}

variable "postgres_password" {
  description = "Postgres password"
  type        = string
  default     = "psm_dev_password"
  sensitive   = true
}

variable "domain_name" {
  description = "Root domain name for the HTTPS endpoint (e.g., openzeppelin.com)"
  type        = string
  default     = "openzeppelin.com"
}

variable "subdomain" {
  description = "Subdomain for the service (e.g., psm -> psm.openzeppelin.com). Empty uses the root domain."
  type        = string
  default     = "psm"
}

variable "acm_certificate_arn" {
  description = "ACM certificate ARN for the service domain (e.g., psm-stg.openzeppelin.com)"
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

variable "postgres_cpu" {
  description = "Postgres task CPU units"
  type        = number
  default     = 512
}

variable "postgres_memory" {
  description = "Postgres task memory (MB)"
  type        = number
  default     = 1024
}

# Resource naming
variable "cluster_name" {
  description = "ECS cluster name"
  type        = string
  default     = "psm-cluster"
}

variable "server_service_name" {
  description = "Server ECS service name"
  type        = string
  default     = "psm-server"
}

variable "postgres_service_name" {
  description = "Postgres ECS service name"
  type        = string
  default     = "psm-postgres"
}

variable "alb_name" {
  description = "ALB name"
  type        = string
  default     = "psm-alb"
}

variable "sd_namespace_name" {
  description = "Cloud Map namespace name for service discovery"
  type        = string
  default     = "psm.local"
}
