use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use axum::{
    Json,
    extract::{Multipart, Query, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use url::Url;

use crate::{AppState, auth::public_entry, error::AppError};

pub async fn config(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "host": state.config.host, "port": state.config.port,
        "admin-host": state.config.admin_host, "admin-port": state.config.admin_port,
        "auth-dir": state.config.auth_dir, "api-keys": state.config.api_keys.iter().map(|_| "***").collect::<Vec<_>>(),
        "usage-statistics-enabled": state.usage.usage_statistics_enabled(),
        "redis-usage-queue-retention-seconds": state.config.redis_usage_queue_retention_seconds,
        "proxy-url": state.config.proxy_url,
        "request-retry": state.config.request_retry,
        "max-concurrency": state.config.max_concurrency,
        "routing": state.config.routing,
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
    validate_management_url(&url, &state.config.management_allowed_hosts)?;
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
    if bytes.len() > state.config.max_body_bytes {
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
