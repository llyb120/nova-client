use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::UsageBucket;

const REFRESH_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Prices {
    fetched_at: i64,
    models: BTreeMap<String, Value>,
}

impl Prices {
    pub(super) fn load(dir: &Path) -> Self {
        std::fs::read(Self::path(dir)).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    fn path(dir: &Path) -> PathBuf { dir.join("model-prices.json") }
    pub(super) fn ready(&self) -> bool { !self.models.is_empty() }
    pub(super) fn stale(&self) -> bool { !self.ready() || chrono::Utc::now().timestamp_millis().saturating_sub(self.fetched_at) >= REFRESH_MS }

    pub(super) fn from_catalog(catalog: &Value, fetched_at: i64) -> Result<Self, String> {
        let providers = catalog.as_object().ok_or("价格目录不是对象")?;
        let mut models = BTreeMap::new();
        for (provider, data) in providers {
            if let Some(entries) = data["models"].as_object() {
                for (model, entry) in entries {
                    if entry["cost"].is_object() {
                        models.insert(format!("{provider}/{model}"), entry["cost"].clone());
                    }
                }
            }
        }
        if models.is_empty() { return Err("价格目录没有模型价格".into()); }
        Ok(Self { fetched_at, models })
    }

    pub(super) fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = Self::path(dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(|e| e.to_string())?)
            .and_then(|_| std::fs::rename(tmp, path)).map_err(|e| e.to_string())
    }

    /// Missing rates are unknown, not free. Historical aggregates cannot recover per-request tiers.
    pub(super) fn cost(&self, model: &str, usage: &UsageBucket, historical: bool, reasoning: Option<u64>) -> Option<f64> {
        let model = model.split("/variant/").next().unwrap_or(model);
        let mut cost = self.models.get(model)?;
        let context = usage.input.saturating_add(usage.cache_read).saturating_add(usage.cache_write);
        if let Some(tiers) = cost["tiers"].as_array().filter(|tiers| !tiers.is_empty()) {
            let mut selected = 0;
            for tier in tiers {
                if tier["tier"]["type"] != "context" { return None; }
                let size = tier["tier"]["size"].as_u64()?;
                if historical && context > size { return None; }
                if context > size && size >= selected { cost = tier; selected = size; }
            }
        } else if cost.get("context_over_200k").is_some() {
            if historical && context > 200_000 { return None; }
            if context > 200_000 { cost = &cost["context_over_200k"]; }
        }
        let rate = |key: &str, tokens: u64| -> Option<f64> {
            if tokens == 0 { return Some(0.0); }
            let rate = cost[key].as_f64().filter(|rate| rate.is_finite() && *rate >= 0.0)?;
            Some(rate * tokens as f64 / 1_000_000.0)
        };
        let reasoning_tokens = match (cost["reasoning"].as_f64(), cost["output"].as_f64()) {
            (Some(reasoning_rate), Some(output_rate)) if reasoning_rate != output_rate && usage.output > 0 => {
                if historical { return None; }
                let count = reasoning?;
                if count > usage.output { return None; }
                count
            }
            _ => 0,
        };
        let total = rate("input", usage.input)? + rate("output", usage.output - reasoning_tokens)?
            + rate("reasoning", reasoning_tokens)? + rate("cache_read", usage.cache_read)? + rate("cache_write", usage.cache_write)?;
        total.is_finite().then_some(total)
    }
}

pub(super) async fn fetch() -> Result<Prices, String> {
    let mut response = crate::lyra::provider::http_client().get("https://models.dev/api.json")
        .timeout(Duration::from_secs(30)).send().await.map_err(|e| e.to_string())?
        .error_for_status().map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES { return Err("价格目录超过 16 MiB".into()); }
        bytes.extend_from_slice(&chunk);
    }
    let catalog: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    Prices::from_catalog(&catalog, chrono::Utc::now().timestamp_millis())
}
