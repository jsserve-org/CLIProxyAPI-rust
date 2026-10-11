use std::{collections::HashMap, sync::Arc, time::SystemTime};

use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{AppState, auth::write_private_json, error::AppError};

const CODEX_AUTHORIZE: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CLAUDE_AUTHORIZE: &str = "https://claude.ai/oauth/authorize";
const CLAUDE_TOKEN: &str = "https://platform.claude.com/v1/oauth/token";
const CLAUDE_CLIENT: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const CLAUDE_REDIRECT: &str = "http://localhost:54545/callback";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Codex,
    Anthropic,
}

#[derive(Clone, Debug)]
struct Session {
    provider: Provider,
    verifier: String,
    expires: std::time::Instant,
    exchanging: bool,
    completed: bool,
    error: Option<String>,
}

#[derive(Clone)]
pub struct OAuthSessions {
    inner: Arc<Mutex<HashMap<String, Session>>>,
    listener: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    codex_token_url: String,
}

impl Default for OAuthSessions {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            listener: Arc::default(),
            codex_token_url: CODEX_TOKEN.to_owned(),
        }
    }
}

fn purge(sessions: &mut HashMap<String, Session>) {
    sessions.retain(|_, session| session.expires > std::time::Instant::now());
}

impl OAuthSessions {
    pub async fn begin(&self, provider: Provider) -> Result<(String, String), AppError> {
        let verifier = random_string(48)?;
        let state = random_string(32)?;
        let challenge = b64url(&Sha256::digest(verifier.as_bytes()));
        let (base, client, redirect, scope) = match provider {
            Provider::Codex => (
                CODEX_AUTHORIZE,
                CODEX_CLIENT,
                "http://localhost:1455/auth/callback",
                "openid email profile offline_access",
            ),
            Provider::Anthropic => (
                CLAUDE_AUTHORIZE,
                CLAUDE_CLIENT,
                CLAUDE_REDIRECT,
                "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload",
            ),
        };
        let mut url = url::Url::parse(base).expect("constant OAuth URL");
        url.query_pairs_mut()
            .append_pair("client_id", client)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect)
            .append_pair("scope", scope)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256");
        if provider == Provider::Codex {
            url.query_pairs_mut()
                .append_pair("prompt", "login")
                .append_pair("id_token_add_organizations", "true")
                .append_pair("codex_cli_simplified_flow", "true");
        } else {
            url.query_pairs_mut().append_pair("code", "true");
        }
        let mut sessions = self.inner.lock().await;
        purge(&mut sessions);
        if sessions.len() >= 256 {
            return Err(AppError::new(
                StatusCode::TOO_MANY_REQUESTS,
                "too many OAuth sessions",
            ));
        }
        sessions.insert(
            state.clone(),
            Session {
                provider,
                verifier,
                expires: std::time::Instant::now() + std::time::Duration::from_secs(1800),
                exchanging: false,
                completed: false,
                error: None,
            },
        );
        Ok((state, url.to_string()))
    }

    async fn take(&self, state: &str, provider: Provider) -> Result<Session, AppError> {
        if !validate_state(state) {
            return Err(AppError::bad_request("invalid state"));
        }
        let mut sessions = self.inner.lock().await;
        purge(&mut sessions);
        let session = sessions
            .get_mut(state)
            .ok_or_else(|| AppError::not_found("unknown or expired state"))?;
        if session.provider != provider {
            return Err(AppError::bad_request("provider does not match state"));
        }
        if session.completed || session.exchanging || session.error.is_some() {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "oauth flow is not pending",
            ));
        }
        session.exchanging = true;
        Ok(session.clone())
    }

    pub async fn fail(&self, state: &str, message: String) {
        let mut sessions = self.inner.lock().await;
        purge(&mut sessions);
        if let Some(session) = sessions.get_mut(state)
            && !session.completed
        {
            session.error = Some(message);
            session.verifier.clear();
        }
    }

    pub async fn provider(&self, state: &str) -> Option<Provider> {
        let mut sessions = self.inner.lock().await;
        purge(&mut sessions);
        sessions.get(state).map(|session| session.provider)
    }

    pub async fn cancel(&self, state: Option<&str>) -> bool {
        let Some(state) = state else {
            return false;
        };
        let mut sessions = self.inner.lock().await;
        purge(&mut sessions);
        if sessions.get(state).is_some_and(|s| s.completed) {
            return false;
        }
        sessions.remove(state).is_some()
    }

    pub async fn status(&self, state: Option<&str>) -> Value {
        let mut sessions = self.inner.lock().await;
        purge(&mut sessions);
        let Some(state) = state.filter(|s| !s.trim().is_empty()) else {
            return json!({"status":"ok"});
        };
        match sessions.get(state.trim()) {
            Some(s) if s.completed => json!({"status":"ok"}),
            Some(s) if s.error.is_some() => json!({"status":"error", "error":s.error}),
            Some(_) => json!({"status":"wait"}),
            None => json!({"status":"error", "error":"unknown or expired state"}),
        }
    }

    async fn ensure_listener(
        &self,
        state: AppState,
        address: std::net::SocketAddr,
    ) -> Result<(), AppError> {
        let mut task = self.listener.lock().await;
        if task.as_ref().is_some_and(|task| !task.is_finished()) {
            return Ok(());
        }
        let listener = tokio::net::TcpListener::bind(address).await
            .map_err(|_| AppError::new(StatusCode::CONFLICT,
                "Codex callback port is unavailable; stop the conflicting login listener or use manual callback mode"))?;
        *task = Some(tokio::spawn(async move {
            let router = browser_router(state);
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(%error, "Codex callback listener stopped");
            }
        }));
        Ok(())
    }
}

#[derive(Deserialize)]
struct BrowserCallback {
    state: String,
    code: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

fn browser_router(state: AppState) -> axum::Router {
    use axum::{
        extract::{Query, State},
        response::{Html, IntoResponse},
    };
    axum::Router::new()
        .route(
            "/auth/callback",
            axum::routing::get(
                |State(state): State<AppState>, Query(query): Query<BrowserCallback>| async move {
                    let result = if query.error.is_some() || query.error_description.is_some() {
                        reject_callback(&state, Provider::Codex, &query.state).await
                    } else {
                        callback(
                            &state,
                            Provider::Codex,
                            query.state,
                            query.code.unwrap_or_default(),
                        )
                        .await
                    };
                    let (status, message) = match result {
                        Ok(_) => (StatusCode::OK, "Login finished. You can close this window."),
                        Err(error) => (
                            error.status(),
                            "Login failed. Check the OAuth status in your management application.",
                        ),
                    };
                    (
                        status,
                        [
                            ("cache-control", "no-store"),
                            ("referrer-policy", "no-referrer"),
                        ],
                        Html(message),
                    )
                        .into_response()
                },
            ),
        )
        .with_state(state)
}

pub async fn reject_callback(
    state: &AppState,
    provider: Provider,
    state_id: &str,
) -> Result<Value, AppError> {
    state.oauth.take(state_id, provider).await?;
    state
        .oauth
        .fail(state_id, "Authorization denied by provider".into())
        .await;
    Ok(json!({"status":"ok"}))
}

pub async fn auth_url(
    state: axum::extract::State<AppState>,
    provider: Provider,
    manual: bool,
) -> Result<axum::Json<Value>, AppError> {
    if provider == Provider::Codex && !manual {
        state
            .oauth
            .ensure_listener(state.0.clone(), ([127, 0, 0, 1], 1455).into())
            .await?;
    }
    let (state_id, url) = state.oauth.begin(provider).await?;
    Ok(axum::Json(
        json!({"status":"ok", "state":state_id, "url":url}),
    ))
}

pub async fn callback(
    state: &AppState,
    provider: Provider,
    state_id: String,
    code: String,
) -> Result<Value, AppError> {
    if code.trim().is_empty() {
        return Err(AppError::bad_request("OAuth code is required"));
    }
    let session = state.oauth.take(&state_id, provider).await?;
    // A browser closing its connection must not strand an exchanging session.
    let state = state.clone();
    tokio::spawn(async move {
        let result = finish_callback(&state, provider, &state_id, &code, &session.verifier).await;
        if result.is_err() {
            state
                .oauth
                .fail(
                    &state_id,
                    "Failed to exchange or save authentication tokens".into(),
                )
                .await;
        }
        result
    })
    .await
    .map_err(|_| AppError::bad_gateway("OAuth login task failed"))?
}

async fn finish_callback(
    state: &AppState,
    provider: Provider,
    state_id: &str,
    code: &str,
    verifier: &str,
) -> Result<Value, AppError> {
    let token = exchange(
        &state.client,
        &state.oauth.codex_token_url,
        provider,
        state_id,
        code,
        verifier,
    )
    .await?;
    let filename = format!("{}-oauth-{}.json", provider_name(provider), state_id);
    let timestamp = humantime::format_rfc3339_seconds(SystemTime::now()).to_string();
    let expiry = SystemTime::now()
        .checked_add(std::time::Duration::from_secs(
            token.expires_in.unwrap_or(0),
        ))
        .ok_or_else(|| AppError::bad_gateway("invalid token expiry"))?;
    let claims = token
        .id_token
        .split('.')
        .nth(1)
        .and_then(|value| URL_SAFE_NO_PAD.decode(value).ok())
        .and_then(|value| serde_json::from_slice::<Value>(&value).ok())
        .unwrap_or(Value::Null);
    // Metadata only: the JWT is supplied by the fixed HTTPS token endpoint.
    // These unverified claims must never authorize local management access.
    let raw = match provider {
        Provider::Codex => json!({
            "type":"codex", "access_token":token.access_token,
            "refresh_token":token.refresh_token, "id_token":token.id_token,
            "account_id":claims.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id").and_then(Value::as_str).unwrap_or(""),
            "email":claims.get("email").and_then(Value::as_str).unwrap_or(""),
            "last_refresh":timestamp, "expired":humantime::format_rfc3339_seconds(expiry).to_string()
        }),
        Provider::Anthropic => {
            json!({"type":"claude", "access_token":token.access_token, "refresh_token":token.refresh_token, "expires_in":token.expires_in, "last_refresh":timestamp})
        }
    };
    let path = state.auth.auth_dir().join(&filename);
    {
        let sessions = state.oauth.inner.lock().await;
        if !sessions.contains_key(state_id) {
            return Err(AppError::new(
                StatusCode::CONFLICT,
                "oauth flow was cancelled or expired",
            ));
        }
    }
    write_private_json(&path, &raw).await?;
    state.auth.reload().await?;
    let mut sessions = state.oauth.inner.lock().await;
    purge(&mut sessions);
    let session = sessions.get_mut(state_id).ok_or_else(|| {
        AppError::new(StatusCode::CONFLICT, "oauth flow was cancelled or expired")
    })?;
    session.completed = true;
    session.verifier.clear();
    session.expires = std::time::Instant::now() + std::time::Duration::from_secs(60);
    Ok(json!({"status":"ok"}))
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

async fn exchange(
    client: &Client,
    codex_token_url: &str,
    provider: Provider,
    state: &str,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, AppError> {
    if provider == Provider::Anthropic {
        let mut code_parts = code.splitn(2, '#');
        let code = code_parts.next().unwrap_or_default();
        let callback_state = code_parts.next().unwrap_or(state);
        let response = client
            .post(CLAUDE_TOKEN)
            .json(&json!({"grant_type":"authorization_code", "code":code, "redirect_uri":CLAUDE_REDIRECT, "client_id":CLAUDE_CLIENT, "code_verifier":verifier, "state":callback_state}))
            .send()
            .await
            .map_err(|e| AppError::bad_gateway(format!("OAuth token exchange failed: {}", if e.is_timeout() { "timeout" } else { "transport error" })))?;
        if !response.status().is_success() {
            return Err(AppError::bad_gateway(format!(
                "OAuth token exchange returned HTTP {}",
                response.status()
            )));
        }
        let token: TokenResponse = response
            .json()
            .await
            .map_err(|e| AppError::bad_gateway(format!("invalid OAuth token response: {e}")))?;
        if token.access_token.is_empty() {
            return Err(AppError::bad_gateway(
                "OAuth token response omitted access_token",
            ));
        }
        return Ok(token);
    }
    let (url, form) = match provider {
        Provider::Codex => (
            codex_token_url,
            vec![
                ("grant_type", "authorization_code"),
                ("client_id", CODEX_CLIENT),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", "http://localhost:1455/auth/callback"),
            ],
        ),
        Provider::Anthropic => (
            CLAUDE_TOKEN,
            vec![
                ("grant_type", "authorization_code"),
                ("client_id", CLAUDE_CLIENT),
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", CLAUDE_REDIRECT),
            ],
        ),
    };
    let response = client
        .post(url)
        .timeout(std::time::Duration::from_secs(30))
        .header("accept", "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| {
            AppError::bad_gateway(format!(
                "OAuth token exchange failed: {}",
                if e.is_timeout() {
                    "timeout"
                } else {
                    "transport error"
                }
            ))
        })?;
    if !response.status().is_success() {
        return Err(AppError::bad_gateway(format!(
            "OAuth token exchange returned HTTP {}",
            response.status()
        )));
    }
    let token: TokenResponse = response
        .json()
        .await
        .map_err(|e| AppError::bad_gateway(format!("invalid OAuth token response: {e}")))?;
    if token.access_token.is_empty() {
        return Err(AppError::bad_gateway(
            "OAuth token response omitted access_token",
        ));
    }
    Ok(token)
}

pub(crate) fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Anthropic => "anthropic",
    }
}

pub(crate) fn validate_state(state: &str) -> bool {
    !state.is_empty()
        && state.len() <= 128
        && !state.contains('/')
        && !state.contains('\\')
        && !state.contains("..")
        && state
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
fn random_string(length: usize) -> Result<String, AppError> {
    let mut bytes = vec![0u8; length];
    getrandom::fill(&mut bytes).map_err(|_| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "secure random source unavailable",
        )
    })?;
    Ok(b64url(&bytes))
}
fn b64url(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let a = bytes[i] as usize;
        let b = bytes.get(i + 1).copied().unwrap_or(0) as usize;
        let c = bytes.get(i + 2).copied().unwrap_or(0) as usize;
        out.push(A[a >> 2] as char);
        out.push(A[((a & 3) << 4) | (b >> 4)] as char);
        if i + 1 < bytes.len() {
            out.push(A[((b & 15) << 2) | (c >> 6)] as char);
        }
        if i + 2 < bytes.len() {
            out.push(A[c & 63] as char);
        }
        i += 3;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sessions_are_provider_scoped_and_cancellable() {
        let sessions = OAuthSessions::default();
        let (state, url) = sessions.begin(Provider::Anthropic).await.unwrap();
        assert!(url.contains("code_challenge_method=S256"));
        assert_eq!(sessions.provider(&state).await, Some(Provider::Anthropic));
        assert!(sessions.cancel(Some(&state)).await);
        assert_eq!(sessions.provider(&state).await, None);
    }

    #[test]
    fn base64url_has_no_padding() {
        assert_eq!(b64url(b"foo"), "Zm9v");
        assert!(!b64url(&[0, 1, 2, 253]).contains('='));
    }

    #[tokio::test]
    async fn codex_exchange_accepts_mock_token_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let length = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..length]);
            assert!(request.contains("grant_type=authorization_code"));
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nconnection: close\r\n\r\n{\"access_token\":\"access\",\"refresh_token\":\"refresh\",\"id_token\":\"id\"}",
                )
                .await
                .unwrap();
        });
        let client = Client::new();
        let token = exchange(
            &client,
            &format!("http://{address}"),
            Provider::Codex,
            "state",
            "code",
            "verifier",
        )
        .await
        .unwrap();
        assert_eq!(token.access_token, "access");
        assert_eq!(token.refresh_token, "refresh");
        server.await.unwrap();
    }
}
