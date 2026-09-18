use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::Credential;

pub const ACCOUNTING_VERSION: i64 = 1;
pub const DEFAULT_RETENTION_SECONDS: i64 = 60;
pub const MAX_RETENTION_SECONDS: i64 = 3600;

struct QueueItem {
    enqueued_at: SystemTime,
    payload: Value,
}

/// In-memory usage queue mirroring upstream `internal/redisqueue` semantics:
/// bounded retention, enable toggle, and usage-statistics toggle.
pub struct UsageQueue {
    items: Mutex<VecDeque<QueueItem>>,
    enabled: AtomicBool,
    stats_enabled: AtomicBool,
    retention_seconds: AtomicI64,
}

impl UsageQueue {
    pub fn new(stats_enabled: bool, retention_seconds: i64) -> Self {
        let queue = Self {
            items: Mutex::new(VecDeque::new()),
            enabled: AtomicBool::new(true),
            stats_enabled: AtomicBool::new(stats_enabled),
            retention_seconds: AtomicI64::new(DEFAULT_RETENTION_SECONDS),
        };
        queue.set_retention_seconds(retention_seconds);
        queue
    }

    pub fn set_enabled(&self, value: bool) {
        self.enabled.store(value, Ordering::SeqCst);
        if !value {
            self.items.lock().unwrap().clear();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub fn set_usage_statistics_enabled(&self, value: bool) {
        self.stats_enabled.store(value, Ordering::SeqCst);
    }

    pub fn usage_statistics_enabled(&self) -> bool {
        self.stats_enabled.load(Ordering::SeqCst)
    }

    pub fn set_retention_seconds(&self, value: i64) {
        let normalized = if value <= 0 {
            DEFAULT_RETENTION_SECONDS
        } else if value > MAX_RETENTION_SECONDS {
            MAX_RETENTION_SECONDS
        } else {
            value
        };
        self.retention_seconds.store(normalized, Ordering::SeqCst);
    }

    pub fn retention_seconds(&self) -> i64 {
        self.retention_seconds.load(Ordering::SeqCst)
    }

    pub fn recording(&self) -> bool {
        self.enabled() && self.usage_statistics_enabled()
    }

    pub fn enqueue(&self, payload: Value) {
        if !self.enabled() {
            return;
        }
        let now = SystemTime::now();
        let mut items = self.items.lock().unwrap();
        prune(
            &mut items,
            now,
            self.retention_seconds.load(Ordering::SeqCst),
        );
        items.push_back(QueueItem {
            enqueued_at: now,
            payload,
        });
    }

    pub fn pop_oldest(&self, count: usize) -> Vec<Value> {
        if !self.enabled() || count == 0 {
            return Vec::new();
        }
        let now = SystemTime::now();
        let mut items = self.items.lock().unwrap();
        prune(
            &mut items,
            now,
            self.retention_seconds.load(Ordering::SeqCst),
        );
        let take = count.min(items.len());
        items.drain(..take).map(|item| item.payload).collect()
    }
}

fn prune(items: &mut VecDeque<QueueItem>, now: SystemTime, retention_seconds: i64) {
    let window = retention_seconds.max(1) as u64;
    let cutoff = now
        .checked_sub(std::time::Duration::from_secs(window))
        .unwrap_or(UNIX_EPOCH);
    while items.front().is_some_and(|item| item.enqueued_at < cutoff) {
        items.pop_front();
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TokenStats {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cached_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_read_tokens_present: bool,
    pub cache_creation_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FailDetail {
    pub status_code: i64,
    pub body: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    pub timestamp: String,
    pub latency_ms: i64,
    pub ttft_ms: i64,
    pub source: String,
    pub auth_index: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub access_token_sha256: String,
    pub client_ip: String,
    pub x_forwarded_for: String,
    pub user_agent: String,
    pub tokens: TokenStats,
    pub failed: bool,
    pub generate: bool,
    pub stream: bool,
    pub fail: FailDetail,
    pub accounting_version: i64,
    pub provider: String,
    pub executor_type: String,
    pub model: String,
    pub alias: String,
    pub endpoint: String,
    pub auth_type: String,
    pub api_key: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent_session_id: String,
    pub reasoning_effort: String,
    pub service_tier: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub response_service_tier: String,
}

impl UsageRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: String,
        stream: bool,
        reasoning_effort: String,
        endpoint: String,
        source: String,
        credential: Option<&Credential>,
        request_id: String,
        client_ip: String,
        x_forwarded_for: String,
        user_agent: String,
    ) -> Self {
        let alias = model.clone();
        let (auth_index, api_key) = match credential {
            Some(credential) => (credential.name.clone(), String::new()),
            None => (String::new(), String::new()),
        };
        Self {
            timestamp: now_rfc3339(),
            latency_ms: 0,
            ttft_ms: 0,
            source,
            auth_index,
            access_token_sha256: String::new(),
            client_ip,
            x_forwarded_for,
            user_agent,
            tokens: TokenStats::default(),
            failed: false,
            generate: false,
            stream,
            fail: FailDetail::default(),
            accounting_version: ACCOUNTING_VERSION,
            provider: "codex".to_owned(),
            executor_type: "codex".to_owned(),
            model,
            alias,
            endpoint,
            auth_type: "oauth".to_owned(),
            api_key,
            request_id,
            session_id: String::new(),
            parent_session_id: String::new(),
            reasoning_effort,
            service_tier: String::new(),
            response_service_tier: String::new(),
        }
    }

    pub fn finish_success(&mut self, latency_ms: i64) {
        self.latency_ms = latency_ms;
        self.failed = false;
        self.fail = FailDetail {
            status_code: 200,
            body: String::new(),
        };
    }

    pub fn finish_failure(&mut self, latency_ms: i64, status_code: i64, body: String) {
        self.latency_ms = latency_ms;
        self.failed = true;
        self.fail = FailDetail {
            status_code: if status_code <= 0 { 500 } else { status_code },
            body,
        };
    }

    pub fn apply_completed_event(&mut self, event: &Value) {
        let response = event.get("response").unwrap_or(event);
        if let Some(model) = response.get("model").and_then(Value::as_str)
            && !model.is_empty()
        {
            self.model = model.to_owned();
            self.alias = model.to_owned();
        }
        if let Some(tier) = response.get("service_tier").and_then(Value::as_str) {
            let tier = tier.trim();
            if !tier.is_empty() {
                self.response_service_tier = tier.to_owned();
            }
        }
        if let Some(usage) = response.get("usage") {
            self.tokens = token_stats_from_usage(usage);
        }
    }
}

fn token_stats_from_usage(usage: &Value) -> TokenStats {
    let input = int_field(usage, "input_tokens");
    let output = int_field(usage, "output_tokens");
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(input.saturating_add(output));
    TokenStats {
        input_tokens: input,
        output_tokens: output,
        reasoning_tokens: pointer_int(usage, "/output_tokens_details/reasoning_tokens"),
        cached_tokens: pointer_int(usage, "/input_tokens_details/cached_tokens"),
        cache_read_tokens: 0,
        cache_read_tokens_present: false,
        cache_creation_tokens: pointer_int(usage, "/input_tokens_details/cache_write_tokens"),
        total_tokens: total,
    }
}

fn int_field(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn pointer_int(value: &Value, pointer: &str) -> i64 {
    value.pointer(pointer).and_then(Value::as_i64).unwrap_or(0)
}

pub fn now_rfc3339() -> String {
    humantime::format_rfc3339(SystemTime::now()).to_string()
}

#[cfg(test)]
mod tests {
    use super::{UsageQueue, UsageRecord};
    use serde_json::json;

    #[test]
    fn pop_oldest_returns_in_fifo_order_and_removes() {
        let queue = UsageQueue::new(true, 60);
        queue.enqueue(json!({"id": 1}));
        queue.enqueue(json!({"id": 2}));
        queue.enqueue(json!({"id": 3}));
        let popped = queue.pop_oldest(2);
        assert_eq!(popped, vec![json!({"id": 1}), json!({"id": 2})]);
        assert_eq!(queue.pop_oldest(10), vec![json!({"id": 3})]);
    }

    #[test]
    fn disabled_queue_discards_and_returns_nothing() {
        let queue = UsageQueue::new(true, 60);
        queue.enqueue(json!({"id": 1}));
        queue.set_enabled(false);
        assert!(queue.pop_oldest(10).is_empty());
        queue.enqueue(json!({"id": 2}));
        assert!(queue.pop_oldest(10).is_empty());
    }

    #[test]
    fn usage_statistics_toggle_gates_recording() {
        let queue = UsageQueue::new(false, 60);
        assert!(!queue.recording());
        queue.set_usage_statistics_enabled(true);
        assert!(queue.recording());
    }

    #[test]
    fn retention_is_clamped() {
        let queue = UsageQueue::new(true, 999_999);
        assert_eq!(queue.retention_seconds(), super::MAX_RETENTION_SECONDS);
        queue.set_retention_seconds(0);
        assert_eq!(queue.retention_seconds(), super::DEFAULT_RETENTION_SECONDS);
    }

    #[test]
    fn completed_event_fills_tokens() {
        let mut record = UsageRecord::new(
            "gpt-5.6-sol".into(),
            true,
            "medium".into(),
            "responses".into(),
            "responses".into(),
            None,
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        );
        record.apply_completed_event(&json!({
            "response": {
                "model": "gpt-5.6-sol",
                "service_tier": "default",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 20,
                    "total_tokens": 120,
                    "input_tokens_details": {"cached_tokens": 30, "cache_write_tokens": 40},
                    "output_tokens_details": {"reasoning_tokens": 5}
                }
            }
        }));
        assert_eq!(record.tokens.input_tokens, 100);
        assert_eq!(record.tokens.output_tokens, 20);
        assert_eq!(record.tokens.total_tokens, 120);
        assert_eq!(record.tokens.cached_tokens, 30);
        assert_eq!(record.tokens.cache_creation_tokens, 40);
        assert_eq!(record.tokens.reasoning_tokens, 5);
        assert_eq!(record.response_service_tier, "default");
    }
}
