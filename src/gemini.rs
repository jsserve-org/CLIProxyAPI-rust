use crate::{AppState, error::AppError};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::StreamExt;

const DEFAULT_MODEL_LIST: &str = "v1beta/models";

pub async fn models(State(state): State<AppState>) -> Result<Response, AppError> {
    forward(state, "GET", DEFAULT_MODEL_LIST, Bytes::new(), true).await
}

pub async fn action(
    Path(action): Path<String>,
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Result<Response, AppError> {
    if !valid_action(&action) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let path = match uri.query() {
        Some(query) => format!("v1beta/models/{action}?{query}"),
        None => format!("v1beta/models/{action}"),
    };
    forward(state, method.as_str(), &path, body, false).await
}

pub async fn interactions(
    State(state): State<AppState>,
    method: Method,
    body: Bytes,
) -> Result<Response, AppError> {
    forward_with_keys(
        state,
        method.as_str(),
        "v1beta/interactions",
        body,
        false,
        true,
    )
    .await
}

async fn forward(
    state: AppState,
    method: &str,
    path: &str,
    body: Bytes,
    list_models: bool,
) -> Result<Response, AppError> {
    forward_with_keys(state, method, path, body, list_models, false).await
}

async fn forward_with_keys(
    state: AppState,
    method: &str,
    path: &str,
    body: Bytes,
    _list_models: bool,
    interactions: bool,
) -> Result<Response, AppError> {
    let config = state.config().clone();
    let configured = if interactions {
        &config.interactions_api_key
    } else {
        &config.gemini_api_key
    };
    let keys: Vec<_> = configured
        .iter()
        .filter(|key| !key.disabled && (!key.api_key.is_empty() || !key.base_url.is_empty()))
        .cloned()
        .collect();
    if keys.is_empty() {
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "no enabled Gemini credentials",
        ));
    }
    let attempts = config.request_retry.saturating_add(1).min(keys.len());
    let mut last_status = StatusCode::BAD_GATEWAY;
    for key in keys.into_iter().take(attempts) {
        let base = key.base_url.trim_end_matches('/').to_owned();
        let base = if base.is_empty() {
            config.gemini_upstream_url.trim_end_matches('/').to_owned()
        } else {
            base
        };
        let url = format!("{base}/{path}");
        let mut request = state.client.request(
            method
                .parse()
                .map_err(|_| AppError::bad_request("invalid method"))?,
            url,
        );
        if !key.api_key.is_empty() {
            request = request.query(&[("key", key.api_key.as_str())]);
        }
        let response = request
            .header("content-type", "application/json")
            .body(body.clone())
            .send()
            .await
            .map_err(|_| AppError::bad_gateway("Gemini upstream request failed"))?;
        last_status = response.status();
        if matches!(
            last_status.as_u16(),
            401 | 403 | 408 | 429 | 500 | 502 | 503 | 504
        ) {
            continue;
        }
        return passthrough(response).await;
    }
    Err(AppError::new(last_status, "all Gemini credentials failed"))
}

async fn passthrough(response: reqwest::Response) -> Result<Response, AppError> {
    let status = response.status();
    let headers = response.headers().clone();
    let stream = response
        .bytes_stream()
        .map(|chunk| chunk.map_err(|error| std::io::Error::other(error.to_string())));
    let mut output = Response::new(Body::from_stream(stream));
    *output.status_mut() = status;
    copy_headers(output.headers_mut(), &headers);
    Ok(output)
}

fn copy_headers(output: &mut HeaderMap, input: &HeaderMap) {
    for (name, value) in input {
        if name == "content-length" || name == "transfer-encoding" || name == "connection" {
            continue;
        }
        output.append(name, value.clone());
    }
}

fn valid_action(action: &str) -> bool {
    !action.is_empty()
        && action.len() <= 512
        && !action.starts_with('/')
        && !action.contains("..")
        && action.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_' | b':' | b'?' | b'=' | b'&' | b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::valid_action;

    #[test]
    fn validates_model_actions() {
        assert!(valid_action("gemini-2.5-flash:generateContent"));
        assert!(valid_action(
            "gemini-2.5-flash:streamGenerateContent?alt=sse"
        ));
        assert!(!valid_action("../admin"));
        assert!(!valid_action("gemini/model:generateContent"));
    }
}
