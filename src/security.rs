use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use crate::{AppState, error::AppError};

pub async fn require_api_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let supplied = bearer(request.headers()).unwrap_or_default();
    let valid = state
        .config
        .api_keys
        .iter()
        .any(|key| constant_eq(key, supplied));
    if !valid {
        return Err(AppError::unauthorized("invalid API key"));
    }
    Ok(next.run(request).await)
}

pub async fn require_management_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let supplied = bearer(request.headers())
        .or_else(|| {
            request
                .headers()
                .get("x-management-key")
                .and_then(|v| v.to_str().ok())
        })
        .unwrap_or_default();
    let configured = &state.config.remote_management.secret_key;
    let valid = if configured.starts_with("$2") {
        bcrypt::verify(supplied, configured).unwrap_or(false)
    } else {
        constant_eq(configured, supplied)
    };
    if !valid {
        return Err(AppError::unauthorized("invalid management key"));
    }
    Ok(next.run(request).await)
}

fn bearer(headers: &http::HeaderMap) -> Option<&str> {
    headers
        .get(http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn constant_eq(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}
