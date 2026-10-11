//! API-key providers: OpenAI-compatible upstreams and Anthropic keys, plus the
//! Codex model alias/exclusion rules. Config shapes mirror upstream's
//! `openai-compatibility`, `claude-api-key`, `oauth-model-alias` and
//! `oauth-excluded-models`.

use axum::http::{HeaderMap, StatusCode};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{AppState, config::Config, error::AppError, proxy};

const ANTHROPIC_URL: &str = "https://api.anthropic.com";

/// An upstream model, optionally exposed to clients under another name.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ModelAlias {
    pub name: String,
    pub alias: String,
}

impl ModelAlias {
    /// The name clients request.
    pub fn client_name(&self) -> &str {
        let alias = self.alias.trim();
        if alias.is_empty() {
            self.name.trim()
        } else {
            alias
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ApiKeyEntry {
    pub api_key: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct OpenAiCompat {
    pub name: String,
    pub base_url: String,
    pub api_key_entries: Vec<ApiKeyEntry>,
    pub models: Vec<ModelAlias>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ClaudeKey {
    pub api_key: String,
    pub base_url: String,
    pub models: Vec<ModelAlias>,
}

/// Where a request for a provider-served model goes.
pub struct Target {
    attempts: Vec<Attempt>,
    anthropic: bool,
}

struct Attempt {
    url: String,
    key: String,
    model: String,
}

fn upstream_name(models: &[ModelAlias], requested: &str) -> Option<String> {
    models
        .iter()
        .find(|model| !requested.is_empty() && model.client_name() == requested)
        .map(|model| model.name.trim().to_owned())
}

/// The OpenAI-compatible provider that serves `model` on chat/completions.
pub fn openai_target(config: &Config, model: &str) -> Option<Target> {
    config.openai_compatibility.iter().find_map(|provider| {
        let upstream = upstream_name(&provider.models, model.trim())?;
        Some(Target {
            attempts: provider
                .api_key_entries
                .iter()
                .map(|entry| Attempt {
                    url: format!(
                        "{}/chat/completions",
                        provider.base_url.trim().trim_end_matches('/')
                    ),
                    key: entry.api_key.clone(),
                    model: upstream.clone(),
                })
                .collect(),
            anthropic: false,
        })
    })
}

/// The Anthropic keys that serve `model` on /v1/messages. Entries listing the
/// same model share the work: later keys are tried when earlier ones fail.
pub fn claude_target(config: &Config, model: &str) -> Option<Target> {
    let attempts = config
        .claude_api_key
        .iter()
        .filter_map(|entry| {
            let model = upstream_name(&entry.models, model.trim())?;
            let base = entry.base_url.trim().trim_end_matches('/');
            let base = if base.is_empty() { ANTHROPIC_URL } else { base };
            Some(Attempt {
                url: format!("{base}/v1/messages"),
                key: entry.api_key.clone(),
                model,
            })
        })
        .collect::<Vec<_>>();
    (!attempts.is_empty()).then_some(Target { attempts, anthropic: true })
}

/// Forward a request as-is (with the upstream model name) and stream the
/// answer back, moving to the next key on a retryable status.
pub async fn execute(
    state: &AppState,
    target: Target,
    headers: &HeaderMap,
    mut payload: Value,
) -> Result<proxy::UpstreamResponse, AppError> {
    let mut last_status = StatusCode::SERVICE_UNAVAILABLE;
    for attempt in &target.attempts {
        payload["model"] = Value::String(attempt.model.clone());
        let body = Bytes::from(
            serde_json::to_vec(&payload).map_err(|_| AppError::bad_request("invalid request"))?,
        );
        let mut request = state
            .client
            .post(&attempt.url)
            .header("content-type", "application/json")
            .body(body.clone());
        request = if target.anthropic {
            let mut request = request.header("x-api-key", &attempt.key).header(
                "anthropic-version",
                headers
                    .get("anthropic-version")
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("2023-06-01"),
            );
            if let Some(beta) = headers.get("anthropic-beta") {
                request = request.header("anthropic-beta", beta);
            }
            request
        } else {
            request.bearer_auth(&attempt.key)
        };
        let response = match request.send().await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(url = %attempt.url, %error, "provider request failed");
                last_status = StatusCode::BAD_GATEWAY;
                continue;
            }
        };
        last_status = response.status();
        if proxy::should_retry(last_status) {
            tracing::warn!(url = %attempt.url, status = %last_status, "provider key failed");
            continue;
        }
        state.usage.observe(!last_status.is_success(), 0);
        return Ok(proxy::passthrough_response(response));
    }
    state.usage.observe(true, 0);
    Err(AppError::new(
        last_status,
        "every API key for this provider failed",
    ))
}

/// Apply the Codex alias and exclusion rules to a request body: an alias is
/// rewritten to its real model, and an excluded model is refused.
pub fn apply_codex_rules(config: &Config, bytes: Bytes) -> Result<Bytes, AppError> {
    let aliases = config.oauth_model_alias.get("codex");
    let excluded = config.oauth_excluded_models.get("codex");
    if aliases.is_none_or(Vec::is_empty) && excluded.is_none_or(Vec::is_empty) {
        return Ok(bytes);
    }
    let Ok(mut payload) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(bytes);
    };
    let Some(requested) = payload.get("model").and_then(Value::as_str) else {
        return Ok(bytes);
    };
    let requested = requested.trim().to_owned();
    let model = aliases
        .and_then(|aliases| {
            aliases
                .iter()
                .find(|alias| !alias.alias.trim().is_empty() && alias.alias.trim() == requested)
        })
        .map_or(requested.clone(), |alias| alias.name.trim().to_owned());
    if is_excluded(excluded, &model) {
        return Err(AppError::not_found(format!(
            "model {requested} is not available"
        )));
    }
    if model == requested {
        return Ok(bytes);
    }
    payload["model"] = Value::String(model);
    Ok(serde_json::to_vec(&payload).map_or(bytes, Bytes::from))
}

pub fn is_excluded(patterns: Option<&Vec<String>>, model: &str) -> bool {
    patterns.is_some_and(|patterns| patterns.iter().any(|pattern| glob(pattern.trim(), model)))
}

/// Case-insensitive match where `*` stands for any run of characters.
fn glob(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.to_ascii_lowercase(), text.to_ascii_lowercase());
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let mut parts = parts.peekable();
    if parts.peek().is_none() {
        return rest.is_empty();
    }
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(index) => rest = &rest[index + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::{ApiKeyEntry, ClaudeKey, ModelAlias, OpenAiCompat, apply_codex_rules, claude_target, glob, openai_target};
    use crate::config::Config;

    #[test]
    fn glob_matches_wildcards() {
        assert!(glob("gpt-5*", "gpt-5.6-sol"));
        assert!(glob("*-spark", "gpt-5.3-codex-spark"));
        assert!(glob("gpt-*-sol", "GPT-5.6-sol"));
        assert!(glob("gpt-5.5", "gpt-5.5"));
        assert!(!glob("gpt-5.5", "gpt-5.5-mini"));
        assert!(!glob("*-spark", "gpt-5.6-sol"));
    }

    #[test]
    fn codex_rules_rewrite_aliases_and_refuse_excluded_models() {
        let mut config = Config::default();
        config.oauth_model_alias.insert(
            "codex".into(),
            vec![ModelAlias {
                name: "gpt-5.6-sol".into(),
                alias: "best".into(),
            }],
        );
        config
            .oauth_excluded_models
            .insert("codex".into(), vec!["*-spark".into()]);
        let rewritten = apply_codex_rules(&config, Bytes::from(r#"{"model":"best"}"#)).unwrap();
        assert_eq!(rewritten, r#"{"model":"gpt-5.6-sol"}"#);
        assert!(
            apply_codex_rules(&config, Bytes::from(r#"{"model":"gpt-5.3-codex-spark"}"#)).is_err()
        );
    }

    #[test]
    fn providers_resolve_client_names_to_upstream_models() {
        let mut config = Config::default();
        config.openai_compatibility.push(OpenAiCompat {
            name: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1/".into(),
            api_key_entries: vec![ApiKeyEntry { api_key: "test".into() }],
            models: vec![ModelAlias {
                name: "moonshotai/kimi-k2".into(),
                alias: "kimi".into(),
            }],
        });
        let target = openai_target(&config, "kimi").unwrap();
        assert_eq!(target.attempts[0].url, "https://openrouter.ai/api/v1/chat/completions");
        assert_eq!(target.attempts[0].model, "moonshotai/kimi-k2");
        assert!(openai_target(&config, "moonshotai/kimi-k2").is_none());
        assert!(claude_target(&config, "kimi").is_none());
    }

    #[test]
    fn claude_failover_preserves_each_entry_url_and_model() {
        let mut config = Config::default();
        config.claude_api_key = vec![
            ClaudeKey {
                api_key: "first".into(),
                base_url: "https://first.example".into(),
                models: vec![ModelAlias { name: "upstream-one".into(), alias: "shared".into() }],
            },
            ClaudeKey {
                api_key: "second".into(),
                base_url: "https://second.example".into(),
                models: vec![ModelAlias { name: "upstream-two".into(), alias: "shared".into() }],
            },
        ];
        let target = claude_target(&config, "shared").unwrap();
        assert_eq!(target.attempts[0].url, "https://first.example/v1/messages");
        assert_eq!(target.attempts[0].model, "upstream-one");
        assert_eq!(target.attempts[1].url, "https://second.example/v1/messages");
        assert_eq!(target.attempts[1].model, "upstream-two");
    }
}
