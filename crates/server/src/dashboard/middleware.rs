use axum::extract::{Request, State};
use axum::http::{HeaderMap, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::{GuardianError, Result};
use crate::state::AppState;

pub async fn require_dashboard_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response> {
    let token =
        extract_cookie(request.headers(), state.dashboard.cookie_name()).ok_or_else(|| {
            GuardianError::AuthenticationFailed("Invalid operator session".to_string())
        })?;
    let operator = state
        .dashboard
        .authenticate_session(&token, state.clock.now())
        .await?;
    request.extensions_mut().insert(operator);
    Ok(next.run(request).await)
}

pub fn extract_cookie(headers: &HeaderMap, cookie_name: &str) -> Option<String> {
    let raw_cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    raw_cookie.split(';').find_map(|item| {
        let (name, value) = item.trim().split_once('=')?;
        (name == cookie_name).then(|| value.to_string())
    })
}
