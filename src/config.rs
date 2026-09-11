use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

fn default_public_host() -> IpAddr {
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}
fn default_public_port() -> u16 {
    8317
}
fn default_admin_host() -> IpAddr {
    IpAddr::V4(Ipv4Addr::LOCALHOST)
}
fn default_admin_port() -> u16 {
    8318
}
fn default_auth_dir() -> PathBuf {
    PathBuf::from("~/.cli-proxy-api")
}
fn default_concurrency() -> usize {
    64
}
fn default_body_limit() -> usize {
    16 * 1024 * 1024
}
fn default_timeout() -> u64 {
    600
}
fn default_retry() -> usize {
    2
}
fn default_upstream() -> String {
    "https://chatgpt.com/backend-api/codex".into()
}
fn default_management_hosts() -> Vec<String> {
    [
        "chatgpt.com",
        "api.openai.com",
        "api.anthropic.com",
        "api.x.ai",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    pub host: IpAddr,
    pub port: u16,
    pub admin_host: IpAddr,
    pub admin_port: u16,
    pub auth_dir: PathBuf,
    pub api_keys: Vec<String>,
    pub remote_management: RemoteManagement,
    pub max_concurrency: usize,
    pub max_body_bytes: usize,
    pub upstream_timeout_seconds: u64,
    pub request_retry: usize,
    pub upstream_url: String,
    pub usage_statistics_enabled: bool,
    pub redis_usage_queue_retention_seconds: usize,
    pub proxy_url: String,
    pub management_allowed_hosts: Vec<String>,
    pub routing: RoutingConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_public_host(),
            port: default_public_port(),
            admin_host: default_admin_host(),
            admin_port: default_admin_port(),
            auth_dir: default_auth_dir(),
            api_keys: Vec::new(),
            remote_management: RemoteManagement::default(),
            max_concurrency: default_concurrency(),
            max_body_bytes: default_body_limit(),
            upstream_timeout_seconds: default_timeout(),
            request_retry: default_retry(),
            upstream_url: default_upstream(),
            usage_statistics_enabled: false,
            redis_usage_queue_retention_seconds: 60,
            proxy_url: String::new(),
            management_allowed_hosts: default_management_hosts(),
            routing: RoutingConfig::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct RoutingConfig {
    pub strategy: String,
    pub session_affinity: bool,
    pub session_affinity_ttl: String,
    pub session_affinity_subagents: Option<bool>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            strategy: "round-robin".into(),
            session_affinity: false,
            session_affinity_ttl: "1h".into(),
            session_affinity_subagents: Some(true),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct RemoteManagement {
    pub secret_key: String,
}

impl Config {
    pub async fn load(path: &Path) -> Result<Self> {
        let raw = tokio::fs::read(path)
            .await
            .with_context(|| format!("read config {}", path.display()))?;
        let mut config: Self = serde_yaml::from_slice(&raw)
            .with_context(|| format!("parse config {}", path.display()))?;
        config.auth_dir = expand_home(&config.auth_dir)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.port == self.admin_port && self.host == self.admin_host {
            bail!("public and admin listeners must use different addresses");
        }
        if self.max_concurrency == 0 {
            bail!("max-concurrency must be greater than zero");
        }
        if self.max_body_bytes < 1024 {
            bail!("max-body-bytes must be at least 1024");
        }
        if self.remote_management.secret_key.is_empty() {
            bail!("remote-management.secret-key is required");
        }
        if self.api_keys.is_empty() {
            bail!("at least one api-key is required");
        }
        match self.routing.strategy.trim().to_ascii_lowercase().as_str() {
            "round-robin"
            | "roundrobin"
            | "rr"
            | "weighted-round-robin"
            | "weighted"
            | "wrr"
            | "fill-first"
            | "fillfirst"
            | "ff" => {}
            _ => bail!("unsupported routing.strategy"),
        }
        humantime::parse_duration(&self.routing.session_affinity_ttl)
            .context("invalid routing.session-affinity-ttl")?;
        let parsed = url::Url::parse(&self.upstream_url).context("invalid upstream-url")?;
        if parsed.scheme() != "https" {
            bail!("upstream-url must use HTTPS");
        }
        Ok(())
    }

    pub fn public_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
    pub fn admin_addr(&self) -> SocketAddr {
        SocketAddr::new(self.admin_host, self.admin_port)
    }
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.upstream_timeout_seconds)
    }
}

fn expand_home(path: &Path) -> Result<PathBuf> {
    let text = path.to_string_lossy();
    if text == "~" || text.starts_with("~/") {
        let home = std::env::var_os("HOME").context("HOME is unavailable")?;
        return Ok(PathBuf::from(home).join(text.trim_start_matches("~/")));
    }
    Ok(path.to_path_buf())
}
