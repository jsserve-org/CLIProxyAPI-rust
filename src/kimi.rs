//! Kimi Code provider: device-code sign-in, token refresh, and chat
//! completions against `api.kimi.com/coding`. Requests identify themselves as
//! this proxy, the way upstream CLIProxyAPI does.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use axum::http::StatusCode;
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::{AppState, auth::write_private_bytes, error::AppError, proxy};

/// Kimi Code's public OAuth client ID.
const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const REFRESH_BEFORE_EXPIRY: Duration = Duration::from_secs(300);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(900);

/// Client-facing model IDs, as upstream's registry lists them.
pub const MODELS: [&str; 8] = [
    "kimi-k2",
    "kimi-k2-thinking",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "kimi-k2.7-code-highspeed",
    "kimi-k3",
    "kimi-k3-256k",
];

/// The name Kimi's API expects for a client-facing model ID.
fn upstream_model(model: &str) -> &str {
    match model {
        "kimi-k2.7-code" => "kimi-for-coding",
        "kimi-k2.7-code-highspeed" => "kimi-for-coding-highspeed",
        other => other.strip_prefix("kimi-").unwrap_or(other),
    }
}

#[derive(Clone, Debug)]
pub struct KimiCredential {
    pub name: String,
    pub path: PathBuf,
    pub auth_index: String,
    pub access_token: String,
    pub device_id: String,
    pub disabled: bool,
    pub raw: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    #[serde(default)]
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: String,
    #[serde(default)]
    pub interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    error: String,
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_in: f64,
}

/// Kimi credential files (`type: "kimi"`) in the shared auth directory, plus
/// the sign-ins still waiting for the user to approve the device.
pub struct KimiStore {
    auth_dir: PathBuf,
    credentials: RwLock<Arc<[Arc<KimiCredential>]>>,
    refresh_lock: tokio::sync::Mutex<()>,
    /// Pending sign-ins by state; the value is the failure, if it failed.
    logins: Mutex<HashMap<String, Option<String>>>,
}

impl KimiStore {
    pub async fn new(auth_dir: PathBuf) -> Result<Self> {
        let store = Self {
            auth_dir: tokio::fs::canonicalize(auth_dir).await?,
            credentials: RwLock::new(Arc::from([])),
            refresh_lock: tokio::sync::Mutex::new(()),
            logins: Mutex::default(),
        };
        store.reload().await?;
        Ok(store)
    }

    pub async fn reload(&self) -> Result<()> {
        let mut dir = tokio::fs::read_dir(&self.auth_dir).await?;
        let mut loaded = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match load_one(&path).await {
                Ok(Some(credential)) => loaded.push(Arc::new(credential)),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignored invalid Kimi auth file")
                }
            }
        }
        loaded.sort_by(|a, b| a.name.cmp(&b.name));
        *self.credentials.write().await = loaded.into();
        Ok(())
    }

    pub async fn list(&self) -> Arc<[Arc<KimiCredential>]> {
        self.credentials.read().await.clone()
    }

    pub async fn delete(&self, auth_index: &str) -> Result<bool> {
        let list = self.list().await;
        let Some(item) = list.iter().find(|item| item.auth_index == auth_index) else {
            return Ok(false);
        };
        tokio::fs::remove_file(&item.path).await?;
        self.reload().await?;
        Ok(true)
    }

    /// `None` while pending, `Some(None)` once finished, `Some(Some(error))` on failure.
    pub fn login_status(&self, state: &str) -> Option<Option<String>> {
        match self.logins.lock().unwrap().get(state) {
            Some(None) => None,
            Some(Some(error)) => Some(Some(error.clone())),
            None => Some(None),
        }
    }

    /// Whether `state` belongs to a Kimi sign-in that has not succeeded yet.
    pub fn knows_login(&self, state: &str) -> bool {
        self.logins.lock().unwrap().contains_key(state)
    }

    async fn save(&self, token: TokenResponse, device_id: &str, path: Option<&Path>) -> Result<()> {
        let expires = SystemTime::now() + Duration::from_secs_f64(token.expires_in.max(0.0));
        let file = json!({
            "type": "kimi",
            "access_token": token.access_token,
            "refresh_token": token.refresh_token,
            "token_type": "Bearer",
            "device_id": device_id,
            "expired": humantime::format_rfc3339_seconds(expires).to_string(),
        });
        let default = self.auth_dir.join(format!("kimi-{}.json", &device_id[..8]));
        write_private_bytes(path.unwrap_or(&default), file.to_string().as_bytes()).await?;
        self.reload().await
    }

    /// A usable access token, refreshed first when it is about to expire.
    async fn access_token(
        &self,
        state: &AppState,
        credential: &KimiCredential,
        force: bool,
    ) -> Result<String, AppError> {
        let _guard = self.refresh_lock.lock().await;
        // Another request may have refreshed while this one waited for the lock.
        let list = self.list().await;
        let current = list
            .iter()
            .find(|item| item.auth_index == credential.auth_index)
            .map_or(credential, |item| item.as_ref());
        let replaced = current.access_token != credential.access_token;
        if replaced || (!force && !expiring(&current.raw)) {
            return Ok(current.access_token.clone());
        }
        let refresh_token = current
            .raw
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AppError::unauthorized("Kimi credential has no refresh token"))?;
        let token = token_request(
            state,
            &current.device_id,
            &[
                ("client_id", CLIENT_ID),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
            ],
        )
        .await?;
        if token.access_token.is_empty() {
            return Err(AppError::unauthorized("Kimi refused to refresh the token"));
        }
        let access_token = token.access_token.clone();
        self.save(token, &current.device_id, Some(&current.path))
            .await?;
        Ok(access_token)
    }
}

fn expiring(raw: &Map<String, Value>) -> bool {
    raw.get("expired")
        .and_then(Value::as_str)
        .and_then(|value| humantime::parse_rfc3339_weak(value).ok())
        .is_some_and(|expires| SystemTime::now() + REFRESH_BEFORE_EXPIRY >= expires)
}

async fn load_one(path: &Path) -> Result<Option<KimiCredential>> {
    let raw: Map<String, Value> = serde_json::from_slice(&tokio::fs::read(path).await?)?;
    if raw.get("type").and_then(Value::as_str) != Some("kimi") {
        return Ok(None);
    }
    let text = |key: &str| raw.get(key).and_then(Value::as_str).unwrap_or_default();
    let access_token = text("access_token").to_owned();
    if access_token.is_empty() {
        anyhow::bail!("missing access_token");
    }
    let mut digest = Sha256::new();
    digest.update(format!("kimi:{}", path.display()).as_bytes());
    Ok(Some(KimiCredential {
        name: path
            .file_name()
            .and_then(|value| value.to_str())
            .context("invalid filename")?
            .to_owned(),
        path: path.to_owned(),
        auth_index: hex::encode(&digest.finalize()[..8]),
        access_token,
        device_id: text("device_id").to_owned(),
        disabled: raw.get("disabled").and_then(Value::as_bool) == Some(true),
        raw,
    }))
}

pub fn public_entry(item: &KimiCredential) -> Value {
    json!({
        "id": item.name,
        "name": item.name,
        "auth_index": item.auth_index,
        "type": "kimi",
        "provider": "kimi",
        "email": "",
        "disabled": item.disabled,
        "status": if item.disabled { "disabled" } else { "active" },
        "expired": item.raw.get("expired"),
    })
}

/// Headers Kimi expects on every request; they name this proxy as the client.
fn identify(request: reqwest::RequestBuilder, device_id: &str) -> reqwest::RequestBuilder {
    request
        .header("x-msh-platform", "CLIProxyAPI")
        .header("x-msh-version", env!("CARGO_PKG_VERSION"))
        .header("x-msh-device-name", "cliproxyapi-rs")
        .header("x-msh-device-model", std::env::consts::OS)
        .header("x-msh-device-id", device_id)
}

async fn token_request(
    state: &AppState,
    device_id: &str,
    form: &[(&str, &str)],
) -> Result<TokenResponse, AppError> {
    let url = format!("{}/api/oauth/token", auth_url(state));
    identify(state.client.post(url), device_id)
        .header("accept", "application/json")
        .form(form)
        .send()
        .await
        .map_err(|_| AppError::bad_gateway("Kimi token request failed"))?
        .json()
        .await
        .map_err(|_| AppError::bad_gateway("invalid Kimi token response"))
}

fn auth_url(state: &AppState) -> String {
    state
        .config()
        .kimi
        .auth_url
        .trim_end_matches('/')
        .to_owned()
}

/// Start a device-code sign-in. Returns the page to open and the state to poll
/// with `/get-auth-status`; a background task waits for the approval.
pub async fn start_login(state: &AppState) -> Result<(DeviceCode, String), AppError> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|_| AppError::bad_gateway("system randomness is unavailable"))?;
    let device_id = hex::encode(random);
    let url = format!("{}/api/oauth/device_authorization", auth_url(state));
    let response = identify(state.client.post(url), &device_id)
        .header("accept", "application/json")
        .form(&[("client_id", CLIENT_ID)])
        .send()
        .await
        .map_err(|_| AppError::bad_gateway("Kimi device-code request failed"))?;
    if !response.status().is_success() {
        return Err(AppError::bad_gateway(format!(
            "Kimi device-code returned HTTP {}",
            response.status()
        )));
    }
    let device: DeviceCode = response
        .json()
        .await
        .map_err(|_| AppError::bad_gateway("invalid Kimi device-code response"))?;

    let login = device_id.clone();
    state
        .kimi
        .logins
        .lock()
        .unwrap()
        .insert(login.clone(), None);
    let (state, device_code) = (state.clone(), device.device_code.clone());
    let interval = Duration::from_secs(device.interval.max(5));
    let poll_state = login.clone();
    tokio::spawn(async move {
        let result = tokio::time::timeout(LOGIN_TIMEOUT, async {
            loop {
                tokio::time::sleep(interval).await;
                let form = [
                    ("client_id", CLIENT_ID),
                    ("device_code", device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ];
                let token = token_request(&state, &device_id, &form).await?;
                match token.error.as_str() {
                    "authorization_pending" | "slow_down" => continue,
                    "" if !token.access_token.is_empty() => {
                        return state
                            .kimi
                            .save(token, &device_id, None)
                            .await
                            .map_err(AppError::from);
                    }
                    error => return Err(AppError::bad_request(error.to_owned())),
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err(AppError::bad_request("sign-in timed out")));
        let mut logins = state.kimi.logins.lock().unwrap();
        match result {
            Ok(()) => {
                logins.remove(&poll_state);
            }
            Err(error) => {
                logins.insert(poll_state, Some(error.message().to_owned()));
            }
        }
    });
    Ok((device, login))
}

/// Whether a chat request for `model` should go to Kimi.
pub async fn serves(state: &AppState, model: &str) -> bool {
    MODELS.contains(&model) && state.kimi.list().await.iter().any(|item| !item.disabled)
}

/// Forward a chat completion to Kimi, moving to the next account when one fails.
pub async fn execute(
    state: &AppState,
    mut payload: Value,
) -> Result<proxy::UpstreamResponse, AppError> {
    let model = payload["model"].as_str().unwrap_or_default().to_owned();
    payload["model"] = Value::String(upstream_model(&model).to_owned());
    let body = Bytes::from(
        serde_json::to_vec(&payload).map_err(|_| AppError::bad_request("invalid request"))?,
    );
    let url = format!(
        "{}/v1/chat/completions",
        state.config().kimi.upstream_url.trim_end_matches('/')
    );
    let mut last_status = StatusCode::SERVICE_UNAVAILABLE;
    for credential in state.kimi.list().await.iter().filter(|item| !item.disabled) {
        // A rejected token gets one forced refresh before the account is given up on.
        for force in [false, true] {
            let token = match state.kimi.access_token(state, credential, force).await {
                Ok(token) => token,
                Err(error) => {
                    tracing::warn!(credential = %credential.name, %error, "Kimi token unavailable");
                    last_status = error.status();
                    break;
                }
            };
            let request = identify(state.client.post(&url), &credential.device_id)
                .header("content-type", "application/json")
                .header(
                    "user-agent",
                    concat!("CLIProxyAPI/", env!("CARGO_PKG_VERSION")),
                )
                .bearer_auth(token)
                .body(body.clone());
            let Ok(response) = request.send().await else {
                last_status = StatusCode::BAD_GATEWAY;
                break;
            };
            last_status = response.status();
            if last_status == StatusCode::UNAUTHORIZED && !force {
                continue;
            }
            if proxy::should_retry(last_status) {
                break;
            }
            state.usage.observe(!last_status.is_success(), 0);
            return Ok(proxy::passthrough_response(response));
        }
    }
    state.usage.observe(true, 0);
    Err(AppError::new(last_status, "every Kimi account failed"))
}

#[cfg(test)]
mod tests {
    use super::upstream_model;

    #[test]
    fn maps_client_models_to_kimi_names() {
        assert_eq!(upstream_model("kimi-k2.7-code"), "kimi-for-coding");
        assert_eq!(upstream_model("kimi-k3-256k"), "k3-256k");
        assert_eq!(upstream_model("kimi-k2-thinking"), "k2-thinking");
    }
}
