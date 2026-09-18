use std::{pin::Pin, sync::Arc, time::Instant};

use async_stream::stream;
use axum::{
    body::Body,
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::{Value, json};

use crate::{
    AppState,
    auth::Credential,
    error::AppError,
    usage::{UsageQueue, UsageRecord},
};

const MAX_SSE_LINE: usize = 4 * 1024 * 1024;

pub async fn health() -> impl IntoResponse {
    axum::Json(json!({"status": "ok"}))
}

pub async fn root() -> impl IntoResponse {
    StatusCode::NOT_FOUND
}

pub async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
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
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    forward(state, request, &path).await
}

async fn forward(
    state: AppState,
    request: Request,
    upstream_path: &str,
) -> Result<Response, AppError> {
    let (parts, body) = request.into_parts();
    let max = state.config().max_body_bytes;
    let bytes = axum::body::to_bytes(body, max)
        .await
        .map_err(|_| AppError::bad_request("request body exceeds limit"))?;
    validate_json(&bytes)?;
    let response =
        execute_codex(&state, &parts.method, &parts.headers, bytes, upstream_path).await?;
    to_axum(response)
}

/// A bounded upstream response whose body stream optionally records usage.
pub struct UpstreamResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
}

impl UpstreamResponse {
    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub fn bytes_stream(self) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
        self.body
    }
}

pub(crate) async fn execute_codex(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    bytes: Bytes,
    upstream_path: &str,
) -> Result<UpstreamResponse, AppError> {
    let started = Instant::now();
    let values = state.auth.list().await;
    let order = state.routing.candidates(&values, headers, &bytes);
    if order.credentials.is_empty() {
        record_failure(
            state,
            headers,
            &bytes,
            upstream_path,
            None,
            started,
            StatusCode::SERVICE_UNAVAILABLE,
            "no enabled Codex credentials",
        );
        return Err(AppError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "no enabled Codex credentials",
        ));
    }
    let attempts = (state.config().request_retry + 1).min(order.credentials.len());
    let mut last_status = StatusCode::BAD_GATEWAY;
    for credential in order.credentials.iter().take(attempts) {
        let mut credential = credential.clone();
        state.routing.bind(&order, &credential);
        let mut response = match send(
            state,
            method,
            headers,
            bytes.clone(),
            upstream_path,
            &credential,
        )
        .await
        {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(credential = %credential.name, ?error, "trying next credential after transport failure");
                state.routing.release(&order, &credential);
                continue;
            }
        };
        if response.status() == StatusCode::UNAUTHORIZED {
            match state.auth.refresh(&state.client, &credential).await {
                Ok(Some(refreshed)) => {
                    credential = refreshed;
                    response = match send(
                        state,
                        method,
                        headers,
                        bytes.clone(),
                        upstream_path,
                        &credential,
                    )
                    .await
                    {
                        Ok(response) => response,
                        Err(error) => {
                            tracing::warn!(credential = %credential.name, ?error, "trying next credential after refreshed-token transport failure");
                            state.routing.release(&order, &credential);
                            continue;
                        }
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(credential = %credential.name, %error, "token refresh failed")
                }
            }
        }
        last_status = response.status();
        if should_retry(last_status) {
            state.routing.release(&order, &credential);
            continue;
        }
        return Ok(wrap_response(
            state,
            headers,
            &bytes,
            upstream_path,
            &credential,
            started,
            response,
        ));
    }
    record_failure(
        state,
        headers,
        &bytes,
        upstream_path,
        None,
        started,
        last_status,
        "all eligible credentials failed",
    );
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
        state.config().upstream_url.trim_end_matches('/'),
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

fn wrap_response(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    upstream_path: &str,
    credential: &Credential,
    started: Instant,
    response: reqwest::Response,
) -> UpstreamResponse {
    let status = response.status();
    let response_headers = response.headers().clone();
    let inner = response.bytes_stream();
    if state.usage.recording() {
        let record = build_usage_record(headers, body, upstream_path, Some(credential));
        let recorder = UsageRecorder {
            queue: state.usage.clone(),
            record: Some(record),
            started,
            buffer: Vec::new(),
            first_byte: false,
        };
        let body = Box::pin(stream! {
            let mut inner = inner;
            let mut recorder = recorder;
            while let Some(chunk) = inner.next().await {
                match chunk {
                    Ok(bytes) => {
                        recorder.observe(&bytes);
                        yield Ok(bytes);
                    }
                    Err(error) => yield Err(std::io::Error::other(error)),
                }
            }
        });
        UpstreamResponse {
            status,
            headers: response_headers,
            body,
        }
    } else {
        UpstreamResponse {
            status,
            headers: response_headers,
            body: Box::pin(inner.map(|result| result.map_err(std::io::Error::other))),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn record_failure(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
    upstream_path: &str,
    credential: Option<&Credential>,
    started: Instant,
    status: StatusCode,
    message: &str,
) {
    if !state.usage.recording() {
        return;
    }
    let mut record = build_usage_record(headers, body, upstream_path, credential);
    record.finish_failure(
        started.elapsed().as_millis() as i64,
        status.as_u16() as i64,
        message.to_owned(),
    );
    if let Ok(value) = serde_json::to_value(&record) {
        state.usage.enqueue(value);
    }
}

fn build_usage_record(
    headers: &HeaderMap,
    body: &Bytes,
    upstream_path: &str,
    credential: Option<&Credential>,
) -> UsageRecord {
    let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let model = payload
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let stream = payload
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let reasoning_effort = payload
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .or_else(|| payload.get("reasoning_effort").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    UsageRecord::new(
        model,
        stream,
        reasoning_effort,
        upstream_path.to_owned(),
        upstream_path.to_owned(),
        credential,
        header_string(headers, "x-client-request-id"),
        header_string(headers, "x-real-ip"),
        header_string(headers, "x-forwarded-for"),
        header_string(headers, "user-agent"),
    )
}

fn header_string(headers: &HeaderMap, name: &str) -> String {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

struct UsageRecorder {
    queue: Arc<UsageQueue>,
    record: Option<UsageRecord>,
    started: Instant,
    buffer: Vec<u8>,
    first_byte: bool,
}

impl UsageRecorder {
    fn observe(&mut self, bytes: &[u8]) {
        if !self.first_byte {
            self.first_byte = true;
            if let Some(record) = self.record.as_mut() {
                record.ttft_ms = self.started.elapsed().as_millis() as i64;
            }
        }
        if let Some(record) = self.record.as_mut() {
            scan_sse_usage(bytes, &mut self.buffer, record);
        }
    }

    fn finalize(&mut self) {
        let Some(mut record) = self.record.take() else {
            return;
        };
        record.latency_ms = self.started.elapsed().as_millis() as i64;
        if let Ok(value) = serde_json::to_value(&record) {
            self.queue.enqueue(value);
        }
    }
}

impl Drop for UsageRecorder {
    fn drop(&mut self) {
        self.finalize();
    }
}

fn scan_sse_usage(chunk: &[u8], buffer: &mut Vec<u8>, record: &mut UsageRecord) {
    if buffer.len() + chunk.len() > MAX_SSE_LINE {
        buffer.clear();
        return;
    }
    buffer.extend_from_slice(chunk);
    while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=position).collect();
        let line = line.strip_suffix(b"\n").unwrap_or(&line);
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(data) = line.strip_prefix(b"data:") else {
            continue;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if data == b"[DONE]" {
            continue;
        }
        if let Ok(event) = serde_json::from_slice::<Value>(data)
            && matches!(
                event.get("type").and_then(Value::as_str),
                Some("response.completed") | Some("response.incomplete")
            )
        {
            record.apply_completed_event(&event);
        }
    }
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

fn to_axum(upstream: UpstreamResponse) -> Result<Response, AppError> {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let stream = upstream.bytes_stream();
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
