//! Shared client-IP extraction.
//!
//! Precedence (matches the original implementation in
//! `rate_limit.rs::extract_client_ip` that this module replaces):
//! `X-Forwarded-For` (first parseable entry) → `X-Real-IP` → axum
//! `ConnectInfo<SocketAddr>` → `None`.
//!
//! Returning `Option<String>` lets audit callers persist `NULL` for
//! "we don't know" rather than baking in the rate-limit-side
//! `"unknown"` sentinel. The rate-limit module wraps with its own
//! `.unwrap_or_else(|| "unknown".into())` to preserve the keying
//! behavior it depended on.

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
