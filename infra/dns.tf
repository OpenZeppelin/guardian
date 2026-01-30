locals {
  route53_zone_id        = var.domain_name != "" ? var.route53_zone_id : ""
  cloudflare_record_name = var.subdomain != "" ? var.subdomain : "@"
}

data "cloudflare_zone" "domain" {
  count   = local.domain_enabled && var.cloudflare_zone_id != "" ? 1 : 0
  zone_id = var.cloudflare_zone_id
}

# Route 53 CNAME -> Cloudflare CDN (optional)
resource "aws_route53_record" "cloudflare_cname" {
  count = local.domain_enabled && local.route53_zone_id != "" ? 1 : 0

  zone_id = local.route53_zone_id
  name    = local.service_fqdn
  type    = "CNAME"
  ttl     = 300
  records = ["${local.service_fqdn}.cdn.cloudflare.net"]
}

# Cloudflare CNAME -> ALB (optional)
resource "cloudflare_dns_record" "service" {
  count = local.domain_enabled && var.cloudflare_zone_id != "" ? 1 : 0

  zone_id = data.cloudflare_zone.domain[0].zone_id
  comment = "PSM service domain"
  content = aws_lb.main.dns_name
  name    = local.cloudflare_record_name
  proxied = var.cloudflare_proxied
  ttl     = 1
  type    = "CNAME"
}
