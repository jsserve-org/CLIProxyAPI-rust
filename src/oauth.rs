use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::http::StatusCode;
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
    created: u64,
    completed: bool,
}

#[derive(Clone, Default)]
pub struct OAuthSessions {
    inner: Arc<Mutex<HashMap<String, Session>>>,
}

impl OAuthSessions {
    pub async fn begin(&self, provider: Provider) -> (String, String) {
        let verifier = random_string(48);
        let state = random_string(32);
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
                "org:create_api_key user:profile user:inference",
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
        self.inner.lock().await.insert(
            state.clone(),
            Session {
                provider,
                verifier,
                created: now(),
                completed: false,
            },
        );
        (state, url.to_string())
    }

    async fn take(&self, state: &str, provider: Provider) -> Result<Session, AppError> {
        let mut sessions = self.inner.lock().await;
        let session = sessions
            .get_mut(state)
            .ok_or_else(|| AppError::bad_request("unknown OAuth state"))?;
        if session.provider != provider {
            return Err(AppError::bad_request("OAuth provider does not match state"));
        }
        if session.completed || now().saturating_sub(session.created) > 600 {
            sessions.remove(state);
            return Err(AppError::bad_request("OAuth session expired"));
        }
        session.completed = true;
        Ok(session.clone())
    }

    pub async fn provider(&self, state: &str) -> Option<Provider> {
        self.inner
            .lock()
            .await
            .get(state)
            .map(|session| session.provider)
    }

    pub async fn cancel(&self, state: Option<&str>) -> bool {
        state
            .and_then(|s| self.inner.try_lock().ok()?.remove(s))
            .is_some()
    }
    pub async fn status(&self, state: Option<&str>) -> Value {
        let sessions = self.inner.lock().await;
        match state.and_then(|s| sessions.get(s)) {
            Some(s) => {
                json!({"status":"pending", "provider": provider_name(s.provider), "created_at":s.created})
            }
            None => json!({"status":"idle"}),
        }
    }
}

pub async fn auth_url(
    state: axum::extract::State<AppState>,
    provider: Provider,
) -> impl axum::response::IntoResponse {
    let (state_id, url) = state.oauth.begin(provider).await;
    axum::Json(
        json!({"status":"ok", "provider":provider_name(provider), "state":state_id, "url":url, "auth_url":url}),
    )
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
    let token = exchange(&state.client, provider, &state_id, &code, &session.verifier).await?;
    let filename = format!("{}-oauth-{}.json", provider_name(provider), &state_id[..8]);
    let raw = match provider {
        Provider::Codex => {
            json!({"type":"codex", "access_token":token.access_token, "refresh_token":token.refresh_token, "id_token":token.id_token, "last_refresh":now().to_string()})
        }
        Provider::Anthropic => {
            json!({"type":"anthropic", "access_token":token.access_token, "refresh_token":token.refresh_token, "expires_in":token.expires_in, "last_refresh":now().to_string()})
        }
    };
    write_private_json(&state.auth.auth_dir().join(&filename), &raw)
        .await
        .map_err(|e| {
            AppError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("persist OAuth credentials: {e}"),
            )
        })?;
    state.auth.reload().await.map_err(|e| {
        AppError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("reload credentials: {e}"),
        )
    })?;
    Ok(json!({"status":"ok", "provider":provider_name(provider), "name":filename}))
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
    provider: Provider,
    state: &str,
    code: &str,
    verifier: &str,
) -> Result<TokenResponse, AppError> {
    if provider == Provider::Anthropic {
        let response = client
            .post(CLAUDE_TOKEN)
            .json(&json!({"grant_type":"authorization_code", "code":code, "redirect_uri":CLAUDE_REDIRECT, "client_id":CLAUDE_CLIENT, "code_verifier":verifier, "state":state}))
            .send()
            .await
            .map_err(|e| AppError::bad_gateway(format!("OAuth token exchange failed: {e}")))?;
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
            CODEX_TOKEN,
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
        .form(&form)
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("OAuth token exchange failed: {e}")))?;
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

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Codex => "codex",
        Provider::Anthropic => "anthropic",
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn random_string(length: usize) -> String {
    let mut bytes = vec![0u8; length];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut bytes))
        .is_err()
    {
        let digest = Sha256::digest(format!("{}:{}", now(), std::process::id()).as_bytes());
        bytes
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = digest[i % digest.len()]);
    }
    b64url(&bytes)
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
        let (state, url) = sessions.begin(Provider::Anthropic).await;
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
}
