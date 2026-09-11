//! Shared client-IP extraction for rate-limit keying.
//!
//! Precedence: `X-Forwarded-For` (rightmost entry) → `X-Real-IP` →
//! axum `ConnectInfo<SocketAddr>` → tonic `TcpConnectInfo` → `None`.
//!
//! `X-Forwarded-For` is parsed from the trusted end: each proxy appends
//! the address it observed, so the rightmost entry is the one vouched
//! for by the nearest proxy (the production ALB, whose append mode is
//! pinned in `infra/alb.tf`), while any prefix is client-supplied and
//! must not influence keying. An unparseable rightmost entry means the
//! chain did not come from a trusted proxy, and the header is ignored.
//!
//! With no proxy in front (direct exposure, local dev), a single-entry
//! `X-Forwarded-For` or an `X-Real-IP` is attacker-controlled and the
//! derived identity is best-effort; deployed topologies restrict
//! ingress to the load balancer.

use axum::{extract::ConnectInfo, http::Request};
use std::net::{IpAddr, SocketAddr};

pub(crate) fn extract_client_ip<B>(req: &Request<B>) -> Option<String> {
    if let Some(ip) = extract_forwarded_for_ip(req) {
        return Some(ip);
    }
    if let Some(ip) = extract_real_ip(req) {
        return Some(ip);
    }
    if let Some(connect_info) = req.extensions().get::<ConnectInfo<SocketAddr>>() {
        return Some(connect_info.0.ip().to_string());
    }
    req.extensions()
        .get::<tonic::transport::server::TcpConnectInfo>()
        .and_then(|info| info.remote_addr())
        .map(|addr| addr.ip().to_string())
}

fn extract_forwarded_for_ip<B>(req: &Request<B>) -> Option<String> {
    let forwarded = req
        .headers()
        .get_all("x-forwarded-for")
        .iter()
        .next_back()?;
    let value = forwarded.to_str().ok()?;
    value
        .rsplit(',')
        .next()?
        .trim()
        .parse::<IpAddr>()
        .ok()
        .map(|ip| ip.to_string())
}

fn extract_real_ip<B>(req: &Request<B>) -> Option<String> {
    let real_ip = req.headers().get("x-real-ip")?;
    let value = real_ip.to_str().ok()?;
    value.parse::<IpAddr>().ok().map(|ip| ip.to_string())
}
