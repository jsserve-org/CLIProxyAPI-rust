use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

#[derive(Clone, Debug)]
pub struct Credential {
    pub name: String,
    pub path: PathBuf,
    pub auth_index: String,
    pub access_token: String,
    pub account_id: Option<String>,
    pub disabled: bool,
    pub raw: Map<String, Value>,
}

#[derive(Debug)]
pub struct AuthStore {
    auth_dir: PathBuf,
    credentials: RwLock<Arc<[Arc<Credential>]>>,
    cursor: AtomicUsize,
    refresh_lock: Mutex<()>,
}

impl AuthStore {
    pub async fn new(auth_dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&auth_dir)
            .await
            .with_context(|| format!("create auth directory {}", auth_dir.display()))?;
        let auth_dir = tokio::fs::canonicalize(auth_dir).await?;
        let store = Self {
            auth_dir,
            credentials: RwLock::new(Arc::from([])),
            cursor: AtomicUsize::new(0),
            refresh_lock: Mutex::new(()),
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
                .and_then(|v| v.to_str())
                .is_none_or(|v| !v.eq_ignore_ascii_case("json"))
            {
                continue;
            }
            match load_one(&path).await {
                Ok(Some(credential)) => loaded.push(Arc::new(credential)),
                Ok(None) => tracing::debug!(path = %path.display(), "ignored non-Codex auth file"),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignored invalid auth file")
                }
            }
        }
        loaded.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        *self.credentials.write().await = loaded.into();
        Ok(())
    }

    pub async fn list(&self) -> Arc<[Arc<Credential>]> {
        self.credentials.read().await.clone()
    }

    pub async fn candidates(&self) -> Vec<Arc<Credential>> {
        let values = self.list().await;
        let enabled: Vec<_> = values.iter().filter(|v| !v.disabled).cloned().collect();
        if enabled.is_empty() {
            return enabled;
        }
        let offset = self.cursor.fetch_add(1, Ordering::Relaxed) % enabled.len();
        enabled
            .iter()
            .cycle()
            .skip(offset)
            .take(enabled.len())
            .cloned()
            .collect()
    }

    pub async fn find(&self, name: &str, index: Option<&str>) -> Option<Arc<Credential>> {
        self.list()
            .await
            .iter()
            .find(|item| item.name == name && index.is_none_or(|value| value == item.auth_index))
            .cloned()
    }

    pub async fn find_by_index(&self, index: &str) -> Option<Arc<Credential>> {
        self.list()
            .await
            .iter()
            .find(|item| item.auth_index == index)
            .cloned()
    }

    pub async fn set_disabled(
        &self,
        name: &str,
        index: Option<&str>,
        disabled: bool,
    ) -> Result<bool> {
        let Some(item) = self.find(name, index).await else {
            return Ok(false);
        };
        let mut raw = item.raw.clone();
        raw.insert("disabled".into(), Value::Bool(disabled));
        write_private_json(&item.path, &Value::Object(raw)).await?;
        self.reload().await?;
        Ok(true)
    }

    pub async fn delete(&self, name: &str, index: Option<&str>) -> Result<bool> {
        let Some(item) = self.find(name, index).await else {
            return Ok(false);
        };
        tokio::fs::remove_file(&item.path).await?;
        self.reload().await?;
        Ok(true)
    }

    pub async fn download(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let Some(item) = self.find(name, None).await else {
            return Ok(None);
        };
        Ok(Some(tokio::fs::read(&item.path).await?))
    }

    pub async fn upload(&self, name: &str, data: &[u8]) -> Result<()> {
        let safe_name = sanitize_name(name)?;
        let _: Value = serde_json::from_slice(data).context("auth file is not valid JSON")?;
        write_private_bytes(&self.auth_dir.join(safe_name), data).await?;
        self.reload().await
    }

    pub fn auth_dir(&self) -> &Path {
        &self.auth_dir
    }

    pub async fn refresh(
        &self,
        client: &reqwest::Client,
        stale: &Credential,
    ) -> Result<Option<Arc<Credential>>> {
        let _guard = self.refresh_lock.lock().await;
        // Another request may already have refreshed and replaced the stale token.
        if let Some(current) = self.find(&stale.name, Some(&stale.auth_index)).await
            && current.access_token != stale.access_token
        {
            return Ok(Some(current));
        }
        let Some(refresh_token) = stale
            .raw
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        else {
            return Ok(None);
        };
        let response = client
            .post(CODEX_TOKEN_URL)
            .form(&RefreshForm {
                client_id: CODEX_CLIENT_ID,
                grant_type: "refresh_token",
                refresh_token,
                scope: "openid profile email",
            })
            .send()
            .await
            .context("request Codex token refresh")?;
        if !response.status().is_success() {
            anyhow::bail!("Codex token refresh returned HTTP {}", response.status());
        }
        let token: RefreshResponse = response.json().await.context("parse Codex token refresh")?;
        if token.access_token.is_empty() {
            anyhow::bail!("Codex token refresh omitted access_token");
        }
        let mut raw = stale.raw.clone();
        raw.insert("access_token".into(), Value::String(token.access_token));
        if !token.refresh_token.is_empty() {
            raw.insert("refresh_token".into(), Value::String(token.refresh_token));
        }
        if !token.id_token.is_empty() {
            raw.insert("id_token".into(), Value::String(token.id_token));
        }
        raw.insert(
            "last_refresh".into(),
            Value::String(unix_seconds().to_string()),
        );
        write_private_json(&stale.path, &Value::Object(raw)).await?;
        self.reload().await?;
        Ok(self.find(&stale.name, Some(&stale.auth_index)).await)
    }
}

#[derive(Serialize)]
struct RefreshForm<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: &'a str,
    scope: &'a str,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

async fn load_one(path: &Path) -> Result<Option<Credential>> {
    let bytes = tokio::fs::read(path).await?;
    let raw: Map<String, Value> = serde_json::from_slice(&bytes)?;
    if raw.get("type").and_then(Value::as_str) != Some("codex") {
        return Ok(None);
    }
    let Some(access_token) = raw
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    else {
        anyhow::bail!("missing access_token");
    };
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .context("invalid filename")?
        .to_owned();
    let mut digest = Sha256::new();
    // Matches CLIProxyAPI's stableAuthIndex("codex:<absolute auth path>").
    digest.update(format!("codex:{}", path.display()).as_bytes());
    let auth_index = hex::encode(&digest.finalize()[..8]);
    Ok(Some(Credential {
        name,
        path: path.to_owned(),
        auth_index,
        access_token: access_token.to_owned(),
        account_id: raw
            .get("account_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        disabled: raw
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        raw,
    }))
}

pub fn public_entry(item: &Credential) -> Value {
    json!({
        "id": item.name,
        "name": item.name,
        "auth_index": item.auth_index,
        "type": "codex",
        "provider": "codex",
        "email": item.raw.get("email").cloned().unwrap_or(Value::String(String::new())),
        "account_id": item.account_id,
        "disabled": item.disabled,
        "status": if item.disabled { "disabled" } else { "active" },
    })
}

pub(crate) fn sanitize_name(name: &str) -> Result<&str> {
    let path = Path::new(name);
    if name.is_empty()
        || path.file_name().and_then(|v| v.to_str()) != Some(name)
        || !name.to_lowercase().ends_with(".json")
    {
        anyhow::bail!("invalid auth filename");
    }
    Ok(name)
}

pub(crate) async fn write_private_json(path: &Path, value: &Value) -> Result<()> {
    write_private_bytes(path, &serde_json::to_vec(value)?).await
}

async fn write_private_bytes(path: &Path, data: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    let temporary = path.with_extension("json.tmp");
    let mut options = tokio::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).await?;
    file.write_all(data).await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(temporary, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::sanitize_name;
    #[test]
    fn rejects_path_traversal() {
        assert!(sanitize_name("../secret.json").is_err());
        assert!(sanitize_name("token.txt").is_err());
        assert!(sanitize_name("codex.json").is_ok());
    }
}
