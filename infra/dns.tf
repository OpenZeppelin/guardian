locals {
  route53_zone_id = var.route53_zone_id != "" ? var.route53_zone_id : data.aws_route53_zone.existing[0].zone_id
}

data "aws_route53_zone" "existing" {
  count = var.domain_name != "" && var.route53_zone_id == "" ? 1 : 0

  name         = var.domain_name
  private_zone = false
}

# ACM certificate for the custom domain (DNS validated)
resource "aws_acm_certificate" "main" {
  count = local.domain_enabled ? 1 : 0

  domain_name       = local.service_fqdn
  validation_method = "DNS"
}

resource "aws_route53_record" "acm_validation" {
  for_each = local.domain_enabled ? {
    for dvo in aws_acm_certificate.main[0].domain_validation_options : dvo.domain_name => {
      name   = dvo.resource_record_name
      type   = dvo.resource_record_type
      record = dvo.resource_record_value
    }
  } : {}

  zone_id = local.route53_zone_id
  name    = each.value.name
  type    = each.value.type
  ttl     = 300
  records = [each.value.record]
}

resource "aws_acm_certificate_validation" "main" {
  count = local.domain_enabled ? 1 : 0

  certificate_arn         = aws_acm_certificate.main[0].arn
  validation_record_fqdns = [for record in aws_route53_record.acm_validation : record.fqdn]
}

# Alias record for ALB
resource "aws_route53_record" "alb_alias" {
  count = local.domain_enabled ? 1 : 0

  zone_id = local.route53_zone_id
  name    = local.service_fqdn
  type    = "A"

  alias {
    name                   = aws_lb.main.dns_name
    zone_id                = aws_lb.main.zone_id
    evaluate_target_health = true
  }
}
