locals {
  route53_zone_id        = var.domain_name != "" ? var.route53_zone_id : ""
  cloudflare_record_name = var.subdomain != "" ? var.subdomain : "@"
}

data "cloudflare_zone" "domain" {
  count   = local.domain_enabled && var.cloudflare_zone_id != "" ? 1 : 0
  zone_id = var.cloudflare_zone_id
}

# Route 53 alias -> ALB (optional)
resource "aws_route53_record" "service_alias" {
  count = local.domain_enabled && local.route53_zone_id != "" ? 1 : 0

  zone_id = local.route53_zone_id
  name    = local.service_fqdn
  type    = "A"

  alias {
    name                   = aws_lb.main.dns_name
    zone_id                = aws_lb.main.zone_id
    evaluate_target_health = true
  }
}

# Cloudflare CNAME -> ALB (optional)
resource "cloudflare_dns_record" "service" {
  count = local.domain_enabled && var.cloudflare_zone_id != "" ? 1 : 0

  zone_id = data.cloudflare_zone.domain[0].zone_id
  comment = "GUARDIAN service domain"
  content = aws_lb.main.dns_name
  name    = local.cloudflare_record_name
  proxied = var.cloudflare_proxied
  ttl     = 1
  type    = "CNAME"
}
