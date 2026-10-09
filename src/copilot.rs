use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::{
    auth::{sanitize_name, write_private_json},
    error::AppError,
};

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const GITHUB_USER_URL: &str = "https://api.github.com/user";
const TOKEN_EXPIRY_SKEW_SECONDS: i64 = 60;

#[derive(Clone, Debug)]
pub struct CopilotCredential {
    pub name: String,
    pub path: PathBuf,
    pub auth_index: String,
    pub github_token: String,
    pub login: Option<String>,
    pub disabled: bool,
    pub raw: Map<String, Value>,
}

#[derive(Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub expires_in: i64,
    #[serde(default)]
    pub interval: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DevicePoll {
    Authorized { access_token: String },
    Pending,
    SlowDown,
    Denied,
    Expired,
}

#[derive(Debug)]
pub struct CopilotToken {
    pub token: String,
    pub expires_at: Option<i64>,
}

/// Stores GitHub Copilot credential files (`type: "copilot"`) alongside the
/// Codex files. Codex loading ignores these, and vice versa.
pub struct CopilotStore {
    auth_dir: PathBuf,
    credentials: RwLock<Arc<[Arc<CopilotCredential>]>>,
}

impl CopilotStore {
    pub async fn new(auth_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&auth_dir)
            .await
            .with_context(|| format!("create auth directory {}", auth_dir.display()))?;
        let auth_dir = tokio::fs::canonicalize(auth_dir).await?;
        let store = Self {
            auth_dir,
            credentials: RwLock::new(Arc::from([])),
        };
        store.reload().await?;
        Ok(store)
    }

    pub async fn reload(&self) -> Result<()> {
        let mut dir = tokio::fs::read_dir(&self.auth_dir).await?;
        let mut loaded = Vec::new();
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if path
                .extension()
                .and_then(|value| value.to_str())
                .is_none_or(|value| !value.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            match load_one(&path).await {
                Ok(Some(credential)) => loaded.push(Arc::new(credential)),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignored invalid Copilot auth file")
                }
            }
        }
        loaded.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        *self.credentials.write().await = loaded.into();
        Ok(())
    }

    pub async fn list(&self) -> Arc<[Arc<CopilotCredential>]> {
        self.credentials.read().await.clone()
    }

    pub async fn find_by_index(&self, index: &str) -> Option<Arc<CopilotCredential>> {
        self.list()
            .await
            .iter()
            .find(|item| item.auth_index == index)
            .cloned()
    }

    pub async fn delete(&self, auth_index: &str) -> Result<bool> {
        let Some(item) = self.find_by_index(auth_index).await else {
            return Ok(false);
        };
        tokio::fs::remove_file(&item.path).await?;
        self.reload().await?;
        Ok(true)
    }

    pub async fn save_github_token(
        &self,
        github_token: &str,
        login: Option<&str>,
    ) -> Result<CopilotCredential> {
        let stem = login
            .map(sanitize_stem)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| short_hash(github_token));
        let name = format!("copilot-{stem}.json");
        sanitize_name(&name)?;
        let path = self.auth_dir.join(&name);
        let mut raw = Map::new();
        raw.insert("type".into(), Value::String("copilot".into()));
        raw.insert(
            "github_token".into(),
            Value::String(github_token.to_owned()),
        );
        if let Some(login) = login {
            raw.insert("login".into(), Value::String(login.to_owned()));
        }
        raw.insert(
            "created_at".into(),
            Value::String(unix_seconds().to_string()),
        );
        raw.insert("disabled".into(), Value::Bool(false));
        write_private_json(&path, &Value::Object(raw)).await?;
        self.reload().await?;
        self.find_by_index(&auth_index_for(&path))
            .await
            .map(|item| item.as_ref().clone())
            .context("reload saved Copilot credential")
    }

    /// Return a valid Copilot session token, exchanging the GitHub token when
    /// the cached one is missing or close to expiry.
    pub async fn ensure_copilot_token(
        &self,
        client: &reqwest::Client,
        credential: &CopilotCredential,
    ) -> Result<String, AppError> {
        let now = unix_seconds() as i64;
        let cached = credential
            .raw
            .get("copilot_token")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let expires_at = credential
            .raw
            .get("copilot_expires_at")
            .and_then(Value::as_i64);
        if token_is_fresh(cached, expires_at, now) {
            return Ok(cached.to_owned());
        }
        let exchanged = exchange_copilot_token(client, &credential.github_token).await?;
        self.persist_copilot_token(credential, exchanged.token, exchanged.expires_at)
            .await
    }

    /// Force a GitHub→Copilot token exchange even when a cached token has not
    /// yet expired (used after an upstream 401).
    pub async fn force_refresh_copilot_token(
        &self,
        client: &reqwest::Client,
        credential: &CopilotCredential,
    ) -> Result<String, AppError> {
        let exchanged = exchange_copilot_token(client, &credential.github_token).await?;
        self.persist_copilot_token(credential, exchanged.token, exchanged.expires_at)
            .await
    }

    async fn persist_copilot_token(
        &self,
        credential: &CopilotCredential,
        token: String,
        expires_at: Option<i64>,
    ) -> Result<String, AppError> {
        let mut raw = credential.raw.clone();
        raw.insert("copilot_token".into(), Value::String(token.clone()));
        if let Some(expires_at) = expires_at {
            raw.insert(
                "copilot_expires_at".into(),
                Value::Number(expires_at.into()),
            );
        }
        raw.insert(
            "last_refresh".into(),
            Value::String(unix_seconds().to_string()),
        );
        write_private_json(&credential.path, &Value::Object(raw))
            .await
            .map_err(|error| AppError::bad_gateway(format!("persist Copilot token: {error}")))?;
        self.reload().await.map_err(|error| {
            AppError::bad_gateway(format!("reload Copilot credentials: {error}"))
        })?;
        Ok(token)
    }
}

pub fn public_entry(item: &CopilotCredential) -> Value {
    json!({
        "id": item.name,
        "name": item.name,
        "auth_index": item.auth_index,
        "type": "copilot",
        "provider": "copilot",
        "email": item.login.clone().unwrap_or_default(),
        "disabled": item.disabled,
        "status": if item.disabled { "disabled" } else { "active" },
    })
}

pub async fn start_device_flow(
    client: &reqwest::Client,
    client_id: &str,
    scope: &str,
) -> Result<DeviceCode, AppError> {
    let response = client
        .post(DEVICE_CODE_URL)
        .header("accept", "application/json")
        .header("user-agent", "cliproxyapi-rs")
        .form(&[("client_id", client_id), ("scope", scope)])
        .send()
        .await
        .map_err(|_| AppError::bad_gateway("GitHub device-code request failed"))?;
    if !response.status().is_success() {
        return Err(AppError::bad_gateway(format!(
            "GitHub device-code returned HTTP {}",
            response.status()
        )));
    }
    let body: DeviceCode = response
        .json()
        .await
        .map_err(|_| AppError::bad_gateway("invalid GitHub device-code response"))?;
    if body.device_code.is_empty() || body.user_code.is_empty() {
        return Err(AppError::bad_gateway(
            "GitHub device-code response omitted required fields",
        ));
    }
    Ok(body)
}

pub async fn poll_device_token(
    client: &reqwest::Client,
    client_id: &str,
    device_code: &str,
) -> Result<DevicePoll, AppError> {
    let response = client
        .post(ACCESS_TOKEN_URL)
        .header("accept", "application/json")
        .header("user-agent", "cliproxyapi-rs")
        .form(&[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ])
        .send()
        .await
        .map_err(|_| AppError::bad_gateway("GitHub device-token request failed"))?;
    let value: Value = response
        .json()
        .await
        .map_err(|_| AppError::bad_gateway("invalid GitHub device-token response"))?;
    Ok(classify_poll(&value))
}

pub fn classify_poll(value: &Value) -> DevicePoll {
    if let Some(token) = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
    {
        return DevicePoll::Authorized {
            access_token: token.to_owned(),
        };
    }
    match value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "authorization_pending" => DevicePoll::Pending,
        "slow_down" => DevicePoll::SlowDown,
        "access_denied" => DevicePoll::Denied,
        "expired_token" => DevicePoll::Expired,
        _ => DevicePoll::Pending,
    }
}

pub async fn fetch_login(client: &reqwest::Client, github_token: &str) -> Option<String> {
    let response = client
        .get(GITHUB_USER_URL)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "cliproxyapi-rs")
        .header("authorization", format!("token {github_token}"))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let value: Value = response.json().await.ok()?;
    value
        .get("login")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub async fn exchange_copilot_token(
    client: &reqwest::Client,
    github_token: &str,
) -> Result<CopilotToken, AppError> {
    let response = client
        .get(COPILOT_TOKEN_URL)
        .header("accept", "application/json")
        .header("user-agent", "cliproxyapi-rs")
        .header("editor-version", "vscode/1.99.0")
        .header("editor-plugin-version", "copilot-chat/0.26.7")
        .header("authorization", format!("token {github_token}"))
        .send()
        .await
        .map_err(|_| AppError::bad_gateway("Copilot token request failed"))?;
    if !response.status().is_success() {
        return Err(AppError::bad_gateway(format!(
            "Copilot token endpoint returned HTTP {}",
            response.status()
        )));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| AppError::bad_gateway("invalid Copilot token response"))?;
    let token = value
        .get("token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| AppError::bad_gateway("Copilot token response omitted token"))?;
    Ok(CopilotToken {
        token: token.to_owned(),
        expires_at: value.get("expires_at").and_then(Value::as_i64),
    })
}

fn token_is_fresh(token: &str, expires_at: Option<i64>, now: i64) -> bool {
    if token.is_empty() {
        return false;
    }
    match expires_at {
        Some(expires_at) => expires_at > now + TOKEN_EXPIRY_SKEW_SECONDS,
        None => true,
    }
}

fn auth_index_for(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(format!("copilot:{}", path.display()).as_bytes());
    hex::encode(&digest.finalize()[..8])
}

fn short_hash(value: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(value.as_bytes());
    hex::encode(&digest.finalize()[..4])
}

fn sanitize_stem(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn load_one(path: &Path) -> Result<Option<CopilotCredential>> {
    let bytes = tokio::fs::read(path).await?;
    let raw: Map<String, Value> = serde_json::from_slice(&bytes)?;
    if raw.get("type").and_then(Value::as_str) != Some("copilot") {
        return Ok(None);
    }
    let Some(github_token) = raw
        .get("github_token")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        anyhow::bail!("missing github_token");
    };
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .context("invalid filename")?
        .to_owned();
    Ok(Some(CopilotCredential {
        name,
        path: path.to_owned(),
        auth_index: auth_index_for(path),
        github_token: github_token.to_owned(),
        login: raw.get("login").and_then(Value::as_str).map(str::to_owned),
        disabled: raw
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        raw,
    }))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::{
        CopilotStore, DevicePoll, classify_poll, public_entry, short_hash, token_is_fresh,
    };

    #[test]
    fn classifies_device_poll_states() {
        assert_eq!(
            classify_poll(&json!({"error":"authorization_pending"})),
            DevicePoll::Pending
        );
        assert_eq!(
            classify_poll(&json!({"error":"slow_down"})),
            DevicePoll::SlowDown
        );
        assert_eq!(
            classify_poll(&json!({"error":"access_denied"})),
            DevicePoll::Denied
        );
        assert_eq!(
            classify_poll(&json!({"error":"expired_token"})),
            DevicePoll::Expired
        );
        assert_eq!(
            classify_poll(&json!({"access_token":"gho_x"})),
            DevicePoll::Authorized {
                access_token: "gho_x".into()
            }
        );
    }

    #[test]
    fn token_freshness_honors_skew() {
        assert!(!token_is_fresh("", Some(10_000), 0));
        assert!(token_is_fresh("t", None, 0));
        assert!(token_is_fresh("t", Some(1_000), 0));
        assert!(!token_is_fresh("t", Some(30), 0));
    }

    #[tokio::test]
    async fn saves_lists_and_deletes_copilot_credentials() {
        let directory = tempdir().unwrap();
        let store = CopilotStore::new(directory.path().to_path_buf())
            .await
            .unwrap();
        let saved = store
            .save_github_token("gho_secret", Some("octocat"))
            .await
            .unwrap();
        assert_eq!(saved.login.as_deref(), Some("octocat"));
        assert_eq!(store.list().await.len(), 1);
        let entry = public_entry(&saved);
        assert_eq!(entry["type"], json!("copilot"));
        assert_eq!(entry["provider"], json!("copilot"));

        assert!(store.delete(&saved.auth_index).await.unwrap());
        assert!(store.list().await.is_empty());
    }

    #[tokio::test]
    async fn hashed_filename_when_login_missing() {
        let directory = tempdir().unwrap();
        let store = CopilotStore::new(directory.path().to_path_buf())
            .await
            .unwrap();
        let saved = store.save_github_token("gho_secret", None).await.unwrap();
        assert_eq!(
            saved.name,
            format!("copilot-{}.json", short_hash("gho_secret"))
        );
    }
}
