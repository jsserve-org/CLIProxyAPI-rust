use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Static model metadata mirroring the pinned upstream Codex tiers
/// (`codex-free`/`codex-team`/`codex-plus`/`codex-pro`), deduplicated by ID.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub owned_by: String,
    #[serde(rename = "type", default)]
    pub model_type: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_length: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ModelInfo {
    /// The four fields the upstream OpenAI `/v1/models` handler exposes.
    pub fn public_entry(&self) -> Value {
        json!({
            "id": self.id,
            "object": self.object,
            "created": self.created,
            "owned_by": self.owned_by,
        })
    }
}

static CODEX_MODELS: OnceLock<Vec<ModelInfo>> = OnceLock::new();

fn load() -> &'static [ModelInfo] {
    CODEX_MODELS.get_or_init(|| {
        let parsed: Vec<ModelInfo> =
            serde_json::from_str(include_str!("models.json")).unwrap_or_default();
        let mut models = parsed;
        models.sort_by(|left, right| left.id.cmp(&right.id));
        models
    })
}

/// Models currently available for a handler/provider identifier. Only the
/// Codex provider is implemented in the Rust core.
pub fn available_models(provider: &str) -> &'static [ModelInfo] {
    match provider.trim().to_ascii_lowercase().as_str() {
        "" | "openai" | "codex" => load(),
        _ => &[],
    }
}

pub fn lookup(model_id: &str) -> Option<&'static ModelInfo> {
    load().iter().find(|model| model.id == model_id)
}

#[cfg(test)]
mod tests {
    use super::{available_models, lookup};

    #[test]
    fn loads_codex_tier_union_sorted_by_id() {
        let models = available_models("openai");
        assert!(!models.is_empty());
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert!(ids.contains(&"gpt-5.6-sol"));
        assert!(ids.contains(&"gpt-5.6-terra"));
        assert!(ids.contains(&"gpt-5.6-luna"));
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn unknown_provider_has_no_models() {
        assert!(available_models("gemini").is_empty());
    }

    #[test]
    fn public_entry_keeps_four_fields() {
        let model = lookup("gpt-5.6-sol").unwrap();
        let entry = model.public_entry();
        assert_eq!(entry["id"], serde_json::json!("gpt-5.6-sol"));
        assert_eq!(entry["object"], serde_json::json!("model"));
        assert_eq!(entry["owned_by"], serde_json::json!("openai"));
        assert!(entry.get("created").is_some());
    }
}
