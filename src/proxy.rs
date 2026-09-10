use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::{AppState, auth::Credential, error::AppError};

pub async fn health() -> impl IntoResponse {
    axum::Json(json!({"status": "ok"}))
}

pub async fn root() -> impl IntoResponse {
    axum::Json(
        json!({"message": "CLI Proxy API Server (Rust)", "endpoints": ["POST /v1/responses", "POST /v1/messages", "GET /v1/models"]}),
    )
}

pub async fn models(State(state): State<AppState>) -> impl IntoResponse {
    let count = state
        .auth
        .list()
        .await
        .iter()
        .filter(|item| !item.disabled)
        .count();
    axum::Json(json!({"object":"list", "data":[
        {"id":"gpt-5.6-sol","object":"model","owned_by":"openai","available_credentials":count},
        {"id":"gpt-5.6-terra","object":"model","owned_by":"openai","available_credentials":count},
        {"id":"gpt-5.6-luna","object":"model","owned_by":"openai","available_credentials":count}
    ]}))
}

pub async fn responses(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, AppError> {
    forward(state, request, "responses").await
}

pub async fn responses_compact(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, AppError> {
    forward(state, request, "responses/compact").await
}

pub async fn backend(
    Path(path): Path<String>,
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, AppError> {
    if path != "responses" && path != "responses/compact" && path != "alpha/search" {
        return Err(AppError::not_found("unsupported Codex endpoint"));
    }
    forward(state, request, &path).await
}

async fn forward(
    state: AppState,
    request: Request,
    upstream_path: &str,
) -> Result<Response, AppError> {
    let (parts, body) = request.into_parts();
    let max = state.config.max_body_bytes;
    let bytes = axum::body::to_bytes(body, max)
        .await
        .map_err(|_| AppError::bad_request("request body exceeds limit"))?;
    validate_json(&bytes)?;
    let response =
        execute_codex(&state, &parts.method, &parts.headers, bytes, upstream_path).await?;
    to_axum(response)
}

pub(crate) async fn execute_codex(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    bytes: Bytes,
    upstream_path: &str,
) -> Result<reqwest::Response, AppError> {
    let candidates = state.auth.candidates().await;
    if candidates.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "no enabled Codex credentials",
        ));
    }
    let attempts = (state.config.request_retry + 1).min(candidates.len());
    let mut last_status = StatusCode::BAD_GATEWAY;
    for mut credential in candidates.into_iter().take(attempts) {
        let mut response = send(
            state,
            method,
            headers,
            bytes.clone(),
            upstream_path,
            &credential,
        )
        .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            match state.auth.refresh(&state.client, &credential).await {
                Ok(Some(refreshed)) => {
                    credential = refreshed;
                    response = send(
                        state,
                        method,
                        headers,
                        bytes.clone(),
                        upstream_path,
                        &credential,
                    )
                    .await?;
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(credential = %credential.name, %error, "token refresh failed")
                }
            }
        }
        last_status = response.status();
        if should_retry(last_status) {
            continue;
        }
        return Ok(response);
    }
    Err(AppError::new(
        last_status,
        "all eligible credentials failed",
    ))
}

async fn send(
    state: &AppState,
    method: &Method,
    incoming: &HeaderMap,
    body: Bytes,
    path: &str,
    auth: &Credential,
) -> Result<reqwest::Response, AppError> {
    let url = format!(
        "{}/{}",
        state.config.upstream_url.trim_end_matches('/'),
        path
    );
    let mut request = state
        .client
        .request(method.clone(), url)
        .body(body)
        .bearer_auth(&auth.access_token)
        .header(
            "accept",
            incoming
                .get("accept")
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static("text/event-stream")),
        )
        .header("content-type", "application/json")
        .header("user-agent", "codex_cli_rs/0.111.0")
        .header("originator", "codex_cli_rs");
    if let Some(account_id) = &auth.account_id {
        request = request.header("chatgpt-account-id", account_id);
    }
    for name in [
        "x-codex-beta-features",
        "x-codex-turn-metadata",
        "x-client-request-id",
        "session-id",
        "thread-id",
    ] {
        if let Some(value) = incoming.get(name) {
            request = request.header(name, value);
        }
    }
    request.send().await.map_err(|error| {
        tracing::warn!(credential = %auth.name, %error, "upstream request failed");
        AppError::bad_gateway("upstream request failed")
    })
}

fn validate_json(bytes: &[u8]) -> Result<(), AppError> {
    let payload: Value =
        serde_json::from_slice(bytes).map_err(|_| AppError::bad_request("invalid JSON body"))?;
    if !payload.is_object() {
        return Err(AppError::bad_request("JSON body must be an object"));
    }
    Ok(())
}

fn should_retry(status: StatusCode) -> bool {
    matches!(
        status.as_u16(),
        401 | 403 | 408 | 429 | 500 | 502 | 503 | 504
    )
}

fn to_axum(upstream: reqwest::Response) -> Result<Response, AppError> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    for name in [
        "content-type",
        "cache-control",
        "x-request-id",
        "openai-processing-ms",
        "openai-version",
    ] {
        if let (Ok(header_name), Some(value)) =
            (HeaderName::from_bytes(name.as_bytes()), headers.get(name))
        {
            response.headers_mut().insert(header_name, value.clone());
        }
    }
    Ok(response)
}
