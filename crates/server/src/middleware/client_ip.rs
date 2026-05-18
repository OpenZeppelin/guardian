//! Shared client-IP extraction.
//!
//! Precedence: `X-Forwarded-For` (first parseable) → `X-Real-IP` →
//! axum `ConnectInfo<SocketAddr>` → `None`.

use axum::{extract::ConnectInfo, http::Request};
use std::net::{IpAddr, SocketAddr};

pub(crate) fn extract_client_ip<B>(req: &Request<B>) -> Option<String> {
    if let Some(ip) = extract_forwarded_for_ip(req) {
        return Some(ip);
    }
    if let Some(ip) = extract_real_ip(req) {
        return Some(ip);
    }
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|connect_info| connect_info.0.ip().to_string())
}

fn extract_forwarded_for_ip<B>(req: &Request<B>) -> Option<String> {
    let forwarded = req.headers().get("x-forwarded-for")?;
    let value = forwarded.to_str().ok()?;
    value
        .split(',')
        .map(str::trim)
        .find_map(|entry| entry.parse::<IpAddr>().ok().map(|ip| ip.to_string()))
}

fn extract_real_ip<B>(req: &Request<B>) -> Option<String> {
    let real_ip = req.headers().get("x-real-ip")?;
    let value = real_ip.to_str().ok()?;
    value.parse::<IpAddr>().ok().map(|ip| ip.to_string())
}
