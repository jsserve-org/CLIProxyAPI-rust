use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use axum::{
    Json,
    body::Bytes,
    extract::{Multipart, Query, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::{AppState, auth::public_entry, copilot, error::AppError};

pub async fn config(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config();
    Json(json!({
        "host": config.host, "port": config.port,
        "admin-host": config.admin_host, "admin-port": config.admin_port,
        "auth-dir": config.auth_dir, "api-keys": config.api_keys.iter().map(|_| "***").collect::<Vec<_>>(),
        "usage-statistics-enabled": state.usage.usage_statistics_enabled(),
        "redis-usage-queue-retention-seconds": config.redis_usage_queue_retention_seconds,
        "proxy-url": config.proxy_url,
        "request-retry": config.request_retry,
        "max-concurrency": config.max_concurrency,
        "routing": config.routing,
        "debug": config.debug,
        "logging-to-file": config.logging_to_file,
        "logs-max-total-size-mb": config.logs_max_total_size_mb,
        "error-logs-max-files": config.error_logs_max_files,
        "max-retry-credentials": config.max_retry_credentials,
        "max-retry-interval": config.max_retry_interval,
        "force-model-prefix": config.force_model_prefix,
        "disable-cooling": config.disable_cooling,
        "transient-error-cooldown-seconds": config.transient_error_cooldown_seconds,
        "quota-exceeded": config.quota_exceeded,
        "model-fallback": config.model_fallback,
    }))
}

#[derive(Deserialize)]
pub struct AuthQuery {
    name: Option<String>,
    auth_index: Option<String>,
}

pub async fn auth_files(
    State(state): State<AppState>,
    Query(query): Query<AuthQuery>,
) -> impl IntoResponse {
    let items = state.auth.list().await;
    let files: Vec<_> = items
        .iter()
        .filter(|item| query.name.as_ref().is_none_or(|name| name == &item.name))
        .filter(|item| {
            query
                .auth_index
                .as_ref()
                .is_none_or(|index| index == &item.auth_index)
        })
        .map(|item| public_entry(item))
        .collect();
    Json(json!({"files": files}))
}

pub async fn download(
    State(state): State<AppState>,
    Query(query): Query<AuthQuery>,
) -> Result<Response, AppError> {
    let name = query
        .name
        .ok_or_else(|| AppError::bad_request("name is required"))?;
    let bytes = state
        .auth
        .download(&name)
        .await?
        .ok_or_else(|| AppError::not_found("auth file not found"))?;
    Ok((
        [
            ("content-type", "application/json"),
            ("content-disposition", "attachment"),
        ],
        bytes,
    )
        .into_response())
}

pub async fn upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, AppError> {
    let field = multipart
        .next_field()
        .await
        .map_err(|_| AppError::bad_request("invalid multipart data"))?
        .ok_or_else(|| AppError::bad_request("file is required"))?;
    let name = field
        .file_name()
        .map(str::to_owned)
        .ok_or_else(|| AppError::bad_request("filename is required"))?;
    let bytes = field
        .bytes()
        .await
        .map_err(|_| AppError::bad_request("invalid file"))?;
    state.auth.upload(&name, &bytes).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"status":"ok", "name":name})),
    ))
}

pub async fn delete(
    State(state): State<AppState>,
    Query(query): Query<AuthQuery>,
) -> Result<impl IntoResponse, AppError> {
    let name = query
        .name
        .ok_or_else(|| AppError::bad_request("name is required"))?;
    if !state
        .auth
        .delete(&name, query.auth_index.as_deref())
        .await?
    {
        return Err(AppError::not_found("auth file not found"));
    }
    Ok(Json(json!({"status":"ok"})))
}

#[derive(Deserialize)]
pub struct StatusMutation {
    name: String,
    auth_index: Option<String>,
    disabled: Option<bool>,
}

pub async fn patch_status(
    State(state): State<AppState>,
    Json(body): Json<StatusMutation>,
) -> Result<impl IntoResponse, AppError> {
    let disabled = body
        .disabled
        .ok_or_else(|| AppError::bad_request("disabled is required"))?;
    if !state
        .auth
        .set_disabled(&body.name, body.auth_index.as_deref(), disabled)
        .await?
    {
        return Err(AppError::not_found("auth file not found"));
    }
    Ok(Json(json!({"status":"ok", "disabled":disabled})))
}

pub async fn refresh(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    state.auth.reload().await?;
    Ok(Json(json!({"status":"ok"})))
}

#[derive(Deserialize)]
pub struct UsageQueueQuery {
    count: Option<String>,
}

pub async fn usage_queue(
    State(state): State<AppState>,
    Query(query): Query<UsageQueueQuery>,
) -> Result<impl IntoResponse, AppError> {
    let count = match query.count.as_deref().map(str::trim) {
        None | Some("") => 1,
        Some(value) => value
            .parse::<usize>()
            .ok()
            .filter(|count| *count > 0)
            .ok_or_else(|| AppError::bad_request("count must be a positive integer"))?,
    };
    Ok(Json(state.usage.pop_oldest(count)))
}

pub async fn get_usage_statistics_enabled(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"usage-statistics-enabled": state.usage.usage_statistics_enabled()}))
}

pub async fn put_usage_statistics_enabled(
    State(state): State<AppState>,
    Json(body): Json<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = body
        .get("value")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| AppError::bad_request("invalid body"))?;
    state.usage.set_usage_statistics_enabled(value);
    Ok(Json(json!({"status":"ok"})))
}

pub async fn api_key_usage() -> impl IntoResponse {
    Json(json!({}))
}

#[derive(Deserialize)]
pub struct ApiCall {
    #[serde(alias = "authIndex", alias = "AuthIndex")]
    auth_index: Option<String>,
    method: String,
    url: String,
    #[serde(default)]
    header: HashMap<String, String>,
    #[serde(default)]
    data: String,
}

pub async fn api_call(
    State(state): State<AppState>,
    Json(body): Json<ApiCall>,
) -> Result<impl IntoResponse, AppError> {
    let url = Url::parse(&body.url).map_err(|_| AppError::bad_request("invalid url"))?;
    validate_management_url(&url, &state.config().management_allowed_hosts)?;
    let method = Method::from_bytes(body.method.trim().to_uppercase().as_bytes())
        .map_err(|_| AppError::bad_request("invalid method"))?;
    let credential = if let Some(index) = &body.auth_index {
        state.auth.find_by_index(index).await
    } else {
        None
    };
    let mut request = state.client.request(method, url).body(body.data);
    for (name, mut value) in body.header {
        if value.contains("$TOKEN$") {
            let token = credential
                .as_ref()
                .ok_or_else(|| AppError::bad_request("auth token not found"))?
                .access_token
                .as_str();
            value = value.replace("$TOKEN$", token);
        }
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| AppError::bad_request("invalid header"))?;
        let value =
            HeaderValue::from_str(&value).map_err(|_| AppError::bad_request("invalid header"))?;
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .map_err(|_| AppError::bad_gateway("request failed"))?;
    let status_code = response.status().as_u16();
    let headers = response.headers().iter().fold(
        HashMap::<String, Vec<String>>::new(),
        |mut acc, (name, value)| {
            acc.entry(name.to_string())
                .or_default()
                .push(value.to_str().unwrap_or_default().to_owned());
            acc
        },
    );
    let bytes = response
        .bytes()
        .await
        .map_err(|_| AppError::bad_gateway("failed to read response"))?;
    if bytes.len() > state.config().max_body_bytes {
        return Err(AppError::bad_gateway("response exceeds limit"));
    }
    Ok(Json(
        json!({"status_code":status_code, "header":headers, "body":String::from_utf8_lossy(&bytes)}),
    ))
}

fn validate_management_url(url: &Url, allowed: &[String]) -> Result<(), AppError> {
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(AppError::bad_request(
            "only HTTPS URLs without userinfo or custom ports are allowed",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::bad_request("URL host is required"))?
        .to_ascii_lowercase();
    if let Ok(ip) = host.parse::<IpAddr>()
        && !is_global(ip)
    {
        return Err(AppError::bad_request(
            "private or local destinations are forbidden",
        ));
    }
    let permitted = allowed.contains(&host);
    if !permitted {
        return Err(AppError::bad_request(
            "destination is not in management-allowed-hosts",
        ));
    }
    Ok(())
}

fn is_global(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            !(v.is_private()
                || v.is_loopback()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_unspecified()
                || v == Ipv4Addr::new(169, 254, 169, 254))
        }
        IpAddr::V6(v) => {
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_unique_local()
                || v.is_unicast_link_local()
                || v == Ipv6Addr::LOCALHOST)
        }
    }
}

pub async fn get_debug(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"debug": state.config().debug}))
}

pub async fn put_debug(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = bool_value(&body)?;
    state.config.write().unwrap().debug = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_logging_to_file(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"logging-to-file": state.config().logging_to_file}))
}

pub async fn put_logging_to_file(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = bool_value(&body)?;
    state.config.write().unwrap().logging_to_file = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_logs_max_total_size_mb(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"logs-max-total-size-mb": state.config().logs_max_total_size_mb}))
}

pub async fn put_logs_max_total_size_mb(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = int_value(&body)?.max(0) as usize;
    state.config.write().unwrap().logs_max_total_size_mb = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_error_logs_max_files(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"error-logs-max-files": state.config().error_logs_max_files}))
}

pub async fn put_error_logs_max_files(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = int_value(&body)?;
    let value = if value < 0 { 10 } else { value as usize };
    state.config.write().unwrap().error_logs_max_files = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_request_retry(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"request-retry": state.config().request_retry}))
}

pub async fn put_request_retry(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = int_value(&body)?.max(0) as usize;
    state.config.write().unwrap().request_retry = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_max_retry_credentials(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"max-retry-credentials": state.config().max_retry_credentials}))
}

pub async fn put_max_retry_credentials(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = int_value(&body)?.max(0) as usize;
    state.config.write().unwrap().max_retry_credentials = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_max_retry_interval(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"max-retry-interval": state.config().max_retry_interval}))
}

pub async fn put_max_retry_interval(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = int_value(&body)?.max(0) as u64;
    state.config.write().unwrap().max_retry_interval = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_force_model_prefix(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"force-model-prefix": state.config().force_model_prefix}))
}

pub async fn put_force_model_prefix(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = bool_value(&body)?;
    state.config.write().unwrap().force_model_prefix = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_switch_project(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"switch-project": state.config().quota_exceeded.switch_project}))
}

pub async fn put_switch_project(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = bool_value(&body)?;
    state.config.write().unwrap().quota_exceeded.switch_project = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn get_switch_preview_model(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"switch-preview-model": state.config().quota_exceeded.switch_preview_model}))
}

pub async fn put_switch_preview_model(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = bool_value(&body)?;
    state
        .config
        .write()
        .unwrap()
        .quota_exceeded
        .switch_preview_model = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn reset_quota(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let auth_index = body
        .get("auth_index")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request("auth_index is required"))?;
    if state.auth.find_by_index(auth_index).await.is_none() {
        return Err(AppError::not_found("auth not found"));
    }
    state.routing.reset_quota(auth_index);
    Ok(Json(json!({
        "status": "ok",
        "auth_index": auth_index,
        "models": [],
    })))
}

pub async fn get_proxy_url(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({"proxy-url": state.config().proxy_url}))
}

pub async fn put_proxy_url(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let value = string_value(&body)?;
    state.config.write().unwrap().proxy_url = value;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn delete_proxy_url(State(state): State<AppState>) -> impl IntoResponse {
    state.config.write().unwrap().proxy_url.clear();
    Json(json!({"status":"ok"}))
}

pub async fn get_routing_strategy(State(state): State<AppState>) -> impl IntoResponse {
    let configured = state.config().routing.strategy.clone();
    let strategy = normalize_routing_strategy(&configured)
        .map(str::to_owned)
        .unwrap_or_else(|| configured.trim().to_owned());
    Json(json!({"strategy": strategy}))
}

pub async fn put_routing_strategy(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let requested = string_value(&body)?;
    let normalized = normalize_routing_strategy(&requested)
        .ok_or_else(|| AppError::bad_request("invalid strategy"))?;
    state.config.write().unwrap().routing.strategy = normalized.to_owned();
    Ok(Json(json!({"status":"ok"})))
}

fn normalize_routing_strategy(strategy: &str) -> Option<&'static str> {
    match strategy.trim().to_ascii_lowercase().as_str() {
        "" | "round-robin" | "roundrobin" | "rr" => Some("round-robin"),
        "weighted-round-robin" | "weightedroundrobin" | "wrr" => Some("weighted-round-robin"),
        "fill-first" | "fillfirst" | "ff" => Some("fill-first"),
        _ => None,
    }
}

pub async fn get_api_keys(State(state): State<AppState>) -> impl IntoResponse {
    let keys = state.config().api_keys.clone();
    Json(json!({"api-keys": keys}))
}

pub async fn put_api_keys(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<impl IntoResponse, AppError> {
    let keys = parse_string_list(&body)?;
    state.config.write().unwrap().api_keys = keys;
    Ok(Json(json!({"status":"ok"})))
}

pub async fn patch_api_keys(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    let index = body.get("index").and_then(Value::as_i64);
    let value = body.get("value").and_then(Value::as_str);
    if let (Some(index), Some(value)) = (index, value) {
        let mut config = state.config.write().unwrap();
        if index >= 0 && (index as usize) < config.api_keys.len() {
            config.api_keys[index as usize] = value.to_owned();
            return Ok(Json(json!({"status":"ok"})));
        }
    }
    let old = body.get("old").and_then(Value::as_str);
    let new = body.get("new").and_then(Value::as_str);
    if let (Some(old), Some(new)) = (old, new) {
        let mut config = state.config.write().unwrap();
        if let Some(entry) = config.api_keys.iter_mut().find(|key| key.as_str() == old) {
            *entry = new.to_owned();
        } else {
            config.api_keys.push(new.to_owned());
        }
        return Ok(Json(json!({"status":"ok"})));
    }
    Err(AppError::bad_request("missing fields"))
}

#[derive(Deserialize)]
pub struct ListDeleteQuery {
    index: Option<String>,
    value: Option<String>,
}

pub async fn delete_api_keys(
    State(state): State<AppState>,
    Query(query): Query<ListDeleteQuery>,
) -> Result<impl IntoResponse, AppError> {
    if let Some(index) = query
        .index
        .as_deref()
        .and_then(|value| value.parse::<usize>().ok())
    {
        let mut config = state.config.write().unwrap();
        if index < config.api_keys.len() {
            config.api_keys.remove(index);
            return Ok(Json(json!({"status":"ok"})));
        }
    }
    if let Some(value) = query
        .value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let mut config = state.config.write().unwrap();
        config.api_keys.retain(|key| key.trim() != value);
        return Ok(Json(json!({"status":"ok"})));
    }
    Err(AppError::bad_request("missing index or value"))
}

fn parse_string_list(body: &[u8]) -> Result<Vec<String>, AppError> {
    if let Ok(items) = serde_json::from_slice::<Vec<String>>(body) {
        return Ok(items);
    }
    if let Ok(wrapper) = serde_json::from_slice::<StringListItems>(body)
        && !wrapper.items.is_empty()
    {
        return Ok(wrapper.items);
    }
    Err(AppError::bad_request("invalid body"))
}

#[derive(Deserialize)]
struct StringListItems {
    items: Vec<String>,
}

fn bool_value(body: &Value) -> Result<bool, AppError> {
    body.get("value")
        .and_then(Value::as_bool)
        .ok_or_else(|| AppError::bad_request("invalid body"))
}

fn int_value(body: &Value) -> Result<i64, AppError> {
    body.get("value")
        .and_then(Value::as_i64)
        .ok_or_else(|| AppError::bad_request("invalid body"))
}

fn string_value(body: &Value) -> Result<String, AppError> {
    body.get("value")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| AppError::bad_request("invalid body"))
}

pub async fn copilot_status(State(state): State<AppState>) -> impl IntoResponse {
    let files: Vec<Value> = state
        .copilot
        .list()
        .await
        .iter()
        .map(|credential| copilot::public_entry(credential))
        .collect();
    Json(json!({"files": files}))
}

pub async fn copilot_device_code(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    if !state.config().copilot.enabled {
        return Err(AppError::not_found("copilot is disabled"));
    }
    let (client_id, scope) = {
        let config = state.config();
        (
            config.copilot.client_id.clone(),
            config.copilot.scope.clone(),
        )
    };
    if client_id.trim().is_empty() {
        return Err(AppError::bad_request("copilot.client-id is empty"));
    }
    let device = copilot::start_device_flow(&state.client, &client_id, &scope).await?;
    Ok(Json(json!({
        "device_code": device.device_code,
        "user_code": device.user_code,
        "verification_uri": device.verification_uri,
        "verification_uri_complete": device.verification_uri_complete,
        "expires_in": device.expires_in,
        "interval": device.interval,
    })))
}

pub async fn copilot_device_token(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    if !state.config().copilot.enabled {
        return Err(AppError::not_found("copilot is disabled"));
    }
    let device_code = body
        .get("device_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request("device_code is required"))?;
    let client_id = state.config().copilot.client_id.clone();
    match copilot::poll_device_token(&state.client, &client_id, device_code).await? {
        copilot::DevicePoll::Authorized { access_token } => {
            let login = copilot::fetch_login(&state.client, &access_token).await;
            let saved = state
                .copilot
                .save_github_token(&access_token, login.as_deref())
                .await
                .map_err(|error| {
                    AppError::bad_gateway(format!("save Copilot credential: {error}"))
                })?;
            Ok((
                StatusCode::OK,
                Json(json!({"status":"ok", "name":saved.name, "login":saved.login})),
            ))
        }
        copilot::DevicePoll::Pending | copilot::DevicePoll::SlowDown => {
            Ok((StatusCode::ACCEPTED, Json(json!({"status":"pending"}))))
        }
        copilot::DevicePoll::Denied => Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"status":"error", "error":"access_denied"})),
        )),
        copilot::DevicePoll::Expired => Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({"status":"error", "error":"expired_token"})),
        )),
    }
}

pub async fn copilot_refresh(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Result<impl IntoResponse, AppError> {
    if !state.config().copilot.enabled {
        return Err(AppError::not_found("copilot is disabled"));
    }
    let auth_index = body
        .get("auth_index")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request("auth_index is required"))?;
    let credential = state
        .copilot
        .find_by_index(auth_index)
        .await
        .ok_or_else(|| AppError::not_found("Copilot credential not found"))?;
    state
        .copilot
        .ensure_copilot_token(&state.client, &credential)
        .await?;
    Ok(Json(json!({"status":"ok"})))
}

#[derive(Deserialize)]
pub struct CopilotQuery {
    auth_index: Option<String>,
}

pub async fn copilot_delete(
    State(state): State<AppState>,
    Query(query): Query<CopilotQuery>,
) -> Result<impl IntoResponse, AppError> {
    let auth_index = query
        .auth_index
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AppError::bad_request("auth_index is required"))?;
    if !state
        .copilot
        .delete(auth_index)
        .await
        .map_err(|error| AppError::bad_gateway(format!("delete Copilot credential: {error}")))?
    {
        return Err(AppError::not_found("Copilot credential not found"));
    }
    Ok(Json(json!({"status":"ok"})))
}

pub async fn not_implemented(request: Request) -> AppError {
    AppError::new(
        StatusCode::NOT_IMPLEMENTED,
        format!(
            "{} {} is not implemented in the Rust core",
            request.method(),
            request.uri().path()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::validate_management_url;
    use url::Url;
    #[test]
    fn api_call_has_ssrf_allowlist() {
        let allowed = vec!["api.openai.com".to_owned()];
        assert!(
            validate_management_url(
                &Url::parse("https://api.openai.com/v1/models").unwrap(),
                &allowed
            )
            .is_ok()
        );
        assert!(
            validate_management_url(&Url::parse("http://127.0.0.1/admin").unwrap(), &allowed)
                .is_err()
        );
        assert!(
            validate_management_url(
                &Url::parse("https://api.openai.com.evil.test/").unwrap(),
                &allowed
            )
            .is_err()
        );
    }
}
