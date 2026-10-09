use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use axum::http::HeaderMap;
use lru::LruCache;
use serde_json::Value;

use crate::{auth::Credential, config::Config};

const MAX_SESSION_ENTRIES: usize = 65_536;
const MAX_EXPLICIT_ID_BYTES: usize = 256;
const AUTH_SCOPE: &str = "*";
const DEFAULT_TRANSIENT_COOLDOWN_SECS: u64 = 60;
const MIN_QUOTA_COOLDOWN_SECS: u64 = 10;
const QUOTA_BACKOFF_MAX_SECS: u64 = 30 * 60;

#[derive(Clone)]
struct Binding {
    auth_index: String,
    expires_at: Instant,
}

#[derive(Clone, Copy)]
struct Cooldown {
    until: Instant,
    backoff_level: u32,
}

pub struct RoutingState {
    strategy: Strategy,
    session_affinity: bool,
    session_affinity_subagents: bool,
    session_ttl: Duration,
    cursor: AtomicUsize,
    sessions: Mutex<LruCache<String, Binding>>,
    weighted_current: Mutex<HashMap<String, i64>>,
    cooldowns: Mutex<HashMap<String, Cooldown>>,
    disable_cooling: bool,
    transient_cooldown_seconds: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    RoundRobin,
    WeightedRoundRobin,
    FillFirst,
}

pub struct CandidateOrder {
    pub credentials: Vec<std::sync::Arc<Credential>>,
    cache_key: Option<String>,
}

impl RoutingState {
    pub fn new(config: &Config) -> Result<Self> {
        let strategy = match config.routing.strategy.trim().to_ascii_lowercase().as_str() {
            "fill-first" | "fillfirst" | "ff" => Strategy::FillFirst,
            "weighted-round-robin" | "weighted" | "wrr" => Strategy::WeightedRoundRobin,
            _ => Strategy::RoundRobin,
        };
        let session_ttl = humantime::parse_duration(&config.routing.session_affinity_ttl)
            .context("parse routing.session-affinity-ttl")?;
        Ok(Self {
            strategy,
            session_affinity: config.routing.session_affinity,
            session_affinity_subagents: config.routing.session_affinity_subagents.unwrap_or(true),
            session_ttl,
            cursor: AtomicUsize::new(0),
            sessions: Mutex::new(LruCache::new(
                NonZeroUsize::new(MAX_SESSION_ENTRIES).unwrap(),
            )),
            weighted_current: Mutex::new(HashMap::new()),
            cooldowns: Mutex::new(HashMap::new()),
            disable_cooling: config.disable_cooling,
            transient_cooldown_seconds: config.transient_error_cooldown_seconds,
        })
    }

    /// Record the outcome of a credential/model attempt and update cooldowns.
    /// A failure only extends a still-live deadline; it never shortens one.
    pub fn record_result(
        &self,
        credential: &Credential,
        model: &str,
        success: bool,
        status: Option<u16>,
        retry_after: Option<Duration>,
    ) {
        if self.disable_cooling {
            return;
        }
        let now = Instant::now();
        let mut map = self
            .cooldowns
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if success {
            map.remove(&cooldown_key(&credential.auth_index, model));
            map.remove(&cooldown_key(&credential.auth_index, AUTH_SCOPE));
            return;
        }
        let status = status.unwrap_or(502);
        let key = cooldown_key(
            &credential.auth_index,
            match status {
                401..=403 => AUTH_SCOPE,
                _ => model,
            },
        );
        let existing = map.get(&key).copied();
        let duration = match status {
            401..=403 => Some(Duration::from_secs(30 * 60)),
            404 => Some(Duration::from_secs(12 * 60 * 60)),
            429 => {
                if let Some(retry_after) = retry_after {
                    Some(retry_after.max(Duration::from_secs(MIN_QUOTA_COOLDOWN_SECS)))
                } else {
                    let level = existing.map(|entry| entry.backoff_level).unwrap_or(0);
                    Some(quota_backoff(level))
                }
            }
            _ => self.transient_duration(),
        };
        let Some(duration) = duration else {
            return;
        };
        let until = now + duration;
        let entry = map.entry(key).or_insert(Cooldown {
            until,
            backoff_level: 0,
        });
        if entry.until < until {
            entry.until = until;
        }
        if status == 429 {
            entry.backoff_level = existing.map(|entry| entry.backoff_level + 1).unwrap_or(1);
        }
    }

    /// Clear all cooldown state for one credential (management `reset-quota`).
    pub fn reset_quota(&self, auth_index: &str) -> usize {
        let prefix = format!("{auth_index}|");
        let mut map = self
            .cooldowns
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let before = map.len();
        map.retain(|key, _| !key.starts_with(&prefix));
        before - map.len()
    }

    fn transient_duration(&self) -> Option<Duration> {
        match self.transient_cooldown_seconds {
            seconds if seconds < 0 => None,
            0 => Some(Duration::from_secs(DEFAULT_TRANSIENT_COOLDOWN_SECS)),
            seconds => Some(Duration::from_secs(seconds as u64)),
        }
    }

    pub fn candidates(
        &self,
        values: &[std::sync::Arc<Credential>],
        headers: &HeaderMap,
        body: &[u8],
    ) -> CandidateOrder {
        let mut credentials: Vec<_> = values
            .iter()
            .filter(|credential| !credential.disabled)
            .cloned()
            .collect();
        let model = request_model(body);
        let now = Instant::now();
        {
            let cooldowns = self
                .cooldowns
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            credentials
                .retain(|credential| !cooling_in(&cooldowns, &credential.auth_index, &model, now));
        }
        if credentials.is_empty() {
            return CandidateOrder {
                credentials,
                cache_key: None,
            };
        }
        match self.strategy {
            Strategy::FillFirst => {}
            Strategy::RoundRobin => {
                let offset = self.cursor.fetch_add(1, Ordering::Relaxed) % credentials.len();
                credentials.rotate_left(offset);
            }
            Strategy::WeightedRoundRobin => self.weighted_order(&mut credentials),
        }

        let cache_key = self
            .session_affinity
            .then(|| session_cache_key(headers, body, self.session_affinity_subagents))
            .flatten()
            .map(|session| format!("codex::{session}::{}", request_model(body)));
        if let Some(key) = &cache_key
            && let Some(bound) = self.bound_auth(key)
            && let Some(position) = credentials
                .iter()
                .position(|credential| credential.auth_index == bound)
        {
            credentials.rotate_left(position);
        }
        CandidateOrder {
            credentials,
            cache_key,
        }
    }

    pub fn bind(&self, order: &CandidateOrder, credential: &Credential) {
        let Some(key) = &order.cache_key else { return };
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        sessions.put(
            key.clone(),
            Binding {
                auth_index: credential.auth_index.clone(),
                expires_at: Instant::now() + self.session_ttl,
            },
        );
    }

    pub fn release(&self, order: &CandidateOrder, credential: &Credential) {
        let Some(key) = &order.cache_key else { return };
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if sessions
            .peek(key)
            .is_some_and(|binding| binding.auth_index == credential.auth_index)
        {
            sessions.pop(key);
        }
    }

    fn bound_auth(&self, key: &str) -> Option<String> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        if sessions
            .peek(key)
            .is_some_and(|binding| binding.expires_at <= now)
        {
            sessions.pop(key);
            return None;
        }
        let binding = sessions.get_mut(key)?;
        binding.expires_at = now + self.session_ttl;
        Some(binding.auth_index.clone())
    }

    fn weighted_order(&self, credentials: &mut Vec<std::sync::Arc<Credential>>) {
        credentials.retain(|credential| credential_weight(credential) > 0);
        if credentials.is_empty() {
            return;
        }
        let mut current = self
            .weighted_current
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        current.retain(|index, _| {
            credentials
                .iter()
                .any(|credential| &credential.auth_index == index)
        });
        let mut total = 0_i64;
        let mut picked = 0;
        let mut highest = i64::MIN;
        for (position, credential) in credentials.iter().enumerate() {
            let weight = credential_weight(credential);
            total = total.saturating_add(weight);
            let score = current.entry(credential.auth_index.clone()).or_default();
            *score = score.saturating_add(weight);
            if *score > highest {
                highest = *score;
                picked = position;
            }
        }
        if let Some(score) = current.get_mut(&credentials[picked].auth_index) {
            *score = score.saturating_sub(total);
        }
        credentials.rotate_left(picked);
    }
}

fn cooldown_key(auth_index: &str, scope: &str) -> String {
    format!("{auth_index}|{scope}")
}

fn cooling_in(
    cooldowns: &HashMap<String, Cooldown>,
    auth_index: &str,
    model: &str,
    now: Instant,
) -> bool {
    let model_active = cooldowns
        .get(&cooldown_key(auth_index, model))
        .is_some_and(|entry| entry.until > now);
    let auth_active = cooldowns
        .get(&cooldown_key(auth_index, AUTH_SCOPE))
        .is_some_and(|entry| entry.until > now);
    model_active || auth_active
}

fn quota_backoff(level: u32) -> Duration {
    let shift = level.min(20);
    let seconds = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
    Duration::from_secs(seconds.min(QUOTA_BACKOFF_MAX_SECS))
}

fn request_model(body: &[u8]) -> String {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|payload| {
            payload
                .get("model")?
                .as_str()
                .map(str::trim)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

fn credential_weight(credential: &Credential) -> i64 {
    let value = credential
        .raw
        .get("weight")
        .or_else(|| credential.raw.get("metadata")?.get("weight"));
    match value {
        Some(Value::Number(number)) => number.as_i64().unwrap_or(0),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
        Some(_) => 0,
        None => 1,
    }
}

fn session_cache_key(
    headers: &HeaderMap,
    body: &[u8],
    session_affinity_subagents: bool,
) -> Option<String> {
    let payload = serde_json::from_slice::<Value>(body).ok();
    if let Some(session) = header(headers, "x-claude-code-session-id") {
        let agent = header(headers, "x-claude-code-agent-id");
        if !session_affinity_subagents && agent.as_deref().is_some_and(|agent| agent != "main") {
            return None;
        }
        return Some(match agent.as_deref() {
            Some(agent) if agent != "main" => format!("claude:{session}:agent:{agent}"),
            _ => format!("claude:{session}"),
        });
    }
    if let Some(payload) = &payload
        && let Some(user_id) = payload.pointer("/metadata/user_id").and_then(Value::as_str)
        && let Ok(metadata) = serde_json::from_str::<Value>(user_id)
        && let Some(session) = value_id(metadata.get("session_id"))
    {
        let agent =
            value_id(metadata.get("agent_id")).or_else(|| value_id(metadata.get("subagent_id")));
        if !session_affinity_subagents && agent.as_deref().is_some_and(|agent| agent != "main") {
            return None;
        }
        return Some(match agent.as_deref() {
            Some(agent) if agent != "main" => format!("claude:{session}:agent:{agent}"),
            _ => format!("claude:{session}"),
        });
    }
    let session = header(headers, "session-id").or_else(|| header(headers, "session_id"));
    let thread = header(headers, "thread-id").or_else(|| header(headers, "thread_id"));
    if session.is_some() || thread.is_some() {
        let id = thread.or(session)?;
        return Some(format!("codex:{id}"));
    }
    for (name, prefix) in [
        ("x-http-session-id", "agy:"),
        ("x-session-id", "header:"),
        ("x-session-affinity", "affinity:"),
        ("x-slot-session-id", "slot:"),
        ("x-conversation-id", "conv:"),
        ("x-thread-id", "thread:"),
        ("x-client-request-id", "clientreq:"),
    ] {
        if let Some(value) = header(headers, name) {
            return Some(format!("{prefix}{value}"));
        }
    }
    let payload = payload.as_ref()?;
    for pointer in ["/thread_id", "/session_id", "/sessionId"] {
        if let Some(value) = value_id(payload.pointer(pointer)) {
            return Some(format!("session:{value}"));
        }
    }
    if let Some(value) = value_id(payload.get("prompt_cache_key")) {
        return Some(format!("pck:{value}"));
    }
    if let Some(value) = value_id(payload.pointer("/conversation/id")) {
        return Some(format!("conv:{value}"));
    }
    if let Some(value) = value_id(payload.pointer("/metadata/user_id")) {
        return Some(format!("user:{value}"));
    }
    if let Some(value) =
        value_id(payload.get("conversation_id")).or_else(|| value_id(payload.get("chat_id")))
    {
        return Some(format!("conv:{value}"));
    }
    message_hash(payload)
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    normalize_id(headers.get(name)?.to_str().ok()?)
}

fn value_id(value: Option<&Value>) -> Option<String> {
    normalize_id(value?.as_str()?)
}

fn normalize_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > MAX_EXPLICIT_ID_BYTES
        || value.chars().any(char::is_control)
    {
        return None;
    }
    Some(value.to_owned())
}

fn message_hash(payload: &Value) -> Option<String> {
    let mut system = payload
        .get("system")
        .and_then(text_content)
        .unwrap_or_default();
    let mut user = String::new();
    let mut assistant = String::new();
    for message in payload
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let text = message
            .get("content")
            .and_then(text_content)
            .unwrap_or_default();
        match message.get("role").and_then(Value::as_str) {
            Some("system") if system.is_empty() => system = text,
            Some("user") if user.is_empty() => user = text,
            Some("assistant") if assistant.is_empty() => assistant = text,
            _ => {}
        }
    }
    if user.is_empty() {
        return None;
    }
    let mut bytes = Vec::with_capacity(320);
    append_hash_part(&mut bytes, b"sys:", &system);
    append_hash_part(&mut bytes, b"usr:", &user);
    append_hash_part(&mut bytes, b"ast:", &assistant);
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Some(format!("msg:{hash:016x}"))
}

fn append_hash_part(output: &mut Vec<u8>, prefix: &[u8], value: &str) {
    if value.is_empty() {
        return;
    }
    output.extend_from_slice(prefix);
    output.extend_from_slice(&value.as_bytes()[..value.len().min(100)]);
    output.push(b'\n');
}

fn text_content(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Array(parts) => {
            let texts: Vec<_> = parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .collect();
            (!texts.is_empty()).then(|| texts.join(" "))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::Arc,
        time::{Duration, Instant},
    };

    use axum::http::HeaderMap;
    use serde_json::{Map, json};

    use super::{RoutingState, normalize_id, session_cache_key};
    use crate::{auth::Credential, config::Config};

    fn credential(index: &str) -> Arc<Credential> {
        Arc::new(Credential {
            name: format!("{index}.json"),
            path: PathBuf::from(format!("{index}.json")),
            auth_index: index.into(),
            access_token: "token".into(),
            account_id: None,
            disabled: false,
            raw: Map::new(),
        })
    }

    fn weighted_credential(index: &str, weight: i64) -> Arc<Credential> {
        let mut credential = credential(index).as_ref().clone();
        credential.raw.insert("weight".into(), weight.into());
        Arc::new(credential)
    }

    #[test]
    fn explicit_session_headers_outrank_body() {
        let mut headers = HeaderMap::new();
        headers.insert("session-id", "codex-session".parse().unwrap());
        let body = serde_json::to_vec(&json!({"prompt_cache_key":"body-session"})).unwrap();
        assert_eq!(
            session_cache_key(&headers, &body, true).as_deref(),
            Some("codex:codex-session")
        );
    }

    #[test]
    fn message_fallback_is_stable_and_ignores_later_turns() {
        let headers = HeaderMap::new();
        let first = serde_json::to_vec(
            &json!({"system":"rules","messages":[{"role":"user","content":"hello"}]}),
        )
        .unwrap();
        let later = serde_json::to_vec(&json!({"system":"rules","messages":[{"role":"user","content":"hello"},{"role":"user","content":"later"}]})).unwrap();
        assert_eq!(
            session_cache_key(&headers, &first, true),
            session_cache_key(&headers, &later, true)
        );
    }

    #[test]
    fn rejects_control_and_oversized_ids() {
        assert!(normalize_id("bad\nvalue").is_none());
        assert!(normalize_id(&"x".repeat(257)).is_none());
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({"session_id":"x".repeat(257)})).unwrap();
        assert!(session_cache_key(&headers, &body, true).is_none());
    }

    #[test]
    fn subagent_affinity_can_be_disabled() {
        let mut headers = HeaderMap::new();
        headers.insert("x-claude-code-session-id", "parent".parse().unwrap());
        headers.insert("x-claude-code-agent-id", "worker".parse().unwrap());

        assert_eq!(
            session_cache_key(&headers, b"{}", true).as_deref(),
            Some("claude:parent:agent:worker")
        );
        assert!(session_cache_key(&headers, b"{}", false).is_none());
    }

    #[test]
    fn binding_overrides_round_robin_and_release_removes_it() {
        let mut config = Config::default();
        config.routing.session_affinity = true;
        let routing = RoutingState::new(&config).unwrap();
        let credentials = vec![credential("one"), credential("two")];
        let mut headers = HeaderMap::new();
        headers.insert("session-id", "session-a".parse().unwrap());
        let body = serde_json::to_vec(&json!({"model":"gpt-test"})).unwrap();

        let first = routing.candidates(&credentials, &headers, &body);
        routing.bind(&first, &credentials[1]);
        let bound = routing.candidates(&credentials, &headers, &body);
        assert_eq!(bound.credentials[0].auth_index, "two");
        routing.release(&bound, &credentials[1]);
        assert!(
            routing
                .bound_auth("codex::codex:session-a::gpt-test")
                .is_none()
        );
    }

    #[test]
    fn quota_failure_cools_model_and_reset_restores() {
        let config = Config::default();
        let routing = RoutingState::new(&config).unwrap();
        let credentials = vec![credential("one"), credential("two")];
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({"model":"gpt-test"})).unwrap();
        let first = routing.candidates(&credentials, &headers, &body);
        for selected in &first.credentials {
            routing.record_result(selected, "gpt-test", false, Some(429), None);
        }
        assert!(
            routing
                .candidates(&credentials, &headers, &body)
                .credentials
                .is_empty()
        );
        routing.reset_quota("one");
        let remaining = routing.candidates(&credentials, &headers, &body);
        assert_eq!(remaining.credentials.len(), 1);
        assert_eq!(remaining.credentials[0].auth_index, "one");
        routing.record_result(&credentials[1], "gpt-test", true, Some(200), None);
        assert_eq!(
            routing
                .candidates(&credentials, &headers, &body)
                .credentials
                .len(),
            2
        );
    }

    #[test]
    fn auth_scope_failure_blocks_every_model() {
        let config = Config::default();
        let routing = RoutingState::new(&config).unwrap();
        let credentials = vec![credential("one"), credential("two")];
        let headers = HeaderMap::new();
        routing.record_result(&credentials[0], "gpt-test", false, Some(401), None);
        for model in ["gpt-test", "gpt-other"] {
            let body = serde_json::to_vec(&json!({"model": model})).unwrap();
            let order = routing.candidates(&credentials, &headers, &body);
            assert_eq!(order.credentials.len(), 1);
            assert_eq!(order.credentials[0].auth_index, "two");
        }
    }

    #[test]
    fn disable_cooling_keeps_credentials_available() {
        let config = Config {
            disable_cooling: true,
            ..Config::default()
        };
        let routing = RoutingState::new(&config).unwrap();
        let credentials = vec![credential("one")];
        let headers = HeaderMap::new();
        let body = serde_json::to_vec(&json!({"model": "gpt-test"})).unwrap();
        routing.record_result(&credentials[0], "gpt-test", false, Some(429), None);
        assert_eq!(
            routing
                .candidates(&credentials, &headers, &body)
                .credentials
                .len(),
            1
        );
    }

    #[test]
    fn quota_retry_after_is_respected_with_floor() {
        let config = Config::default();
        let routing = RoutingState::new(&config).unwrap();
        let credential = credential("one");
        routing.record_result(
            &credential,
            "gpt-test",
            false,
            Some(429),
            Some(Duration::from_secs(2)),
        );
        let cooldowns = routing.cooldowns.lock().unwrap();
        let entry = cooldowns
            .get(&super::cooldown_key("one", "gpt-test"))
            .unwrap();
        let remaining = entry.until.saturating_duration_since(Instant::now());
        assert!(remaining >= Duration::from_secs(9));
    }

    #[test]
    fn smooth_weighted_round_robin_honors_weights_and_excludes_zero() {
        let mut config = Config::default();
        config.routing.strategy = "weighted-round-robin".into();
        let routing = RoutingState::new(&config).unwrap();
        let credentials = vec![
            weighted_credential("heavy", 5),
            weighted_credential("light", 1),
            weighted_credential("off", 0),
        ];
        let headers = HeaderMap::new();
        let picks: Vec<_> = (0..6)
            .map(|_| {
                routing
                    .candidates(&credentials, &headers, b"{}")
                    .credentials[0]
                    .auth_index
                    .clone()
            })
            .collect();
        assert_eq!(
            picks.iter().filter(|pick| pick.as_str() == "heavy").count(),
            5
        );
        assert_eq!(
            picks.iter().filter(|pick| pick.as_str() == "light").count(),
            1
        );
        assert!(!picks.iter().any(|pick| pick == "off"));
    }
}
