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
    /// 小写别名 → `models` 的官方价键。空字符串表示有多个官方价，不能猜。
    #[serde(default)]
    official: BTreeMap<String, String>,
    /// 旧缓存的索引包含渠道名，不能用于按模型 ID 迁移历史。
    #[serde(default)]
    model_id_index: bool,
}

impl Prices {
    pub(super) fn load(dir: &Path) -> Self {
        std::fs::read(Self::path(dir)).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    fn path(dir: &Path) -> PathBuf { dir.join("model-prices.json") }
    pub(super) fn ready(&self) -> bool { self.model_id_index && !self.models.is_empty() }
    pub(super) fn stale(&self) -> bool {
        !self.ready() || chrono::Utc::now().timestamp_millis().saturating_sub(self.fetched_at) >= REFRESH_MS
    }

    /// 不计接入厂商，只按 canonical 所属厂商的官方价。接入名、大小写和计费后缀
    /// （`-fast` 等）都回到这个价；`deepseek-v4.1-flash-1017` 这种日期快照名本身就是官方 id。
    pub(super) fn from_catalog(catalog: &Value, fetched_at: i64) -> Result<Self, String> {
        let providers = catalog.as_object().ok_or("价格目录不是对象")?;
        struct Row { key: String, provider: String, model: String, canonical: String }
        let mut models = BTreeMap::new();
        let mut rows = Vec::new();
        for (provider, data) in providers {
            let Some(entries) = data["models"].as_object() else { continue };
            for (model, entry) in entries {
                let key = format!("{provider}/{model}");
                let canonical = entry["canonical_model_id"].as_str().unwrap_or("").trim().to_ascii_lowercase();
                if entry["cost"].is_object() {
                    models.insert(key.clone(), entry["cost"].clone());
                }
                rows.push(Row { key, provider: provider.clone(), model: model.clone(), canonical });
            }
        }
        if models.is_empty() { return Err("价格目录没有模型价格".into()); }

        let mut by_canon: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
        let mut aliases = BTreeMap::new();
        for row in &rows {
            if !row.canonical.is_empty() {
                by_canon.entry(&row.canonical).or_default().push(row);
            }
            if models.contains_key(&row.key) {
                bind_alias(&mut aliases, &models, &row.model, &row.key, 0);
            }
        }
        for (canon, rows) in by_canon {
            let Some((owner, id)) = canon.split_once('/') else { continue };
            let owner_rows: Vec<_> = rows.iter().copied()
                .filter(|row| row.provider.eq_ignore_ascii_case(owner) && models.contains_key(&row.key)).collect();
            for row in &owner_rows {
                bind_alias(&mut aliases, &models, &row.model, &row.key, 2);
            }
            // Prefer the model owner's rate; where it has multiple editions, an alias must
            // identify one unambiguously instead of borrowing another edition's price.
            let exact = models.get_key_value(canon).map(|(key, _)| key.as_str())
                .or_else(|| owner_rows.iter().find(|row| row.model.eq_ignore_ascii_case(id)).map(|row| row.key.as_str()))
                .or_else(|| {
                    // DeepSeek publishes V4.1 Flash under `deepseek-flash`, not the dated API name.
                    if canon != "deepseek/deepseek-v4.1-flash-1017" { return None; }
                    owner_rows.iter().find(|row| row.model == "deepseek-flash").map(|row| row.key.as_str())
                });
            let agreed = owner_rows.first().filter(|first| owner_rows.iter().all(|row| models[&row.key] == models[&first.key]));
            if let Some(key) = exact.or_else(|| agreed.map(|row| row.key.as_str())) {
                bind_alias(&mut aliases, &models, id, key, 2);
                for row in rows {
                    bind_alias(&mut aliases, &models, &row.model, key, 1);
                }
            } else if owner_rows.is_empty() {
                // No owner price: a single (or agreeing) catalog quote still works, regardless of channel.
                for row in rows.into_iter().filter(|row| models.contains_key(&row.key)) {
                    bind_alias(&mut aliases, &models, id, &row.key, 0);
                }
            }
        }
        let official = aliases.into_iter().map(|(alias, (_, key))| (alias, key)).collect();
        Ok(Self { fetched_at, models, official, model_id_index: true })
    }

    pub(super) fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = Self::path(dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(self).map_err(|e| e.to_string())?)
            .and_then(|_| std::fs::rename(tmp, path)).map_err(|e| e.to_string())
    }

    /// Historical totals lack request lengths; estimate them at the model's base rate.
    pub(super) fn cost(&self, model: &str, usage: &UsageBucket, historical: bool, reasoning: Option<u64>) -> Option<f64> {
        let mut cost = self.official_cost(model)?;
        if !historical {
            let context = usage.input.saturating_add(usage.cache_read).saturating_add(usage.cache_write);
            if let Some(tiers) = cost["tiers"].as_array().filter(|tiers| !tiers.is_empty()) {
                let mut selected = 0;
                for tier in tiers {
                    if tier["tier"]["type"] != "context" { return None; }
                    let size = tier["tier"]["size"].as_u64()?;
                    if context > size && size >= selected { cost = tier; selected = size; }
                }
            } else if context > 200_000 && cost.get("context_over_200k").is_some() {
                cost = &cost["context_over_200k"];
            }
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

    fn official_cost(&self, model: &str) -> Option<&Value> {
        if !self.ready() { return None; }
        let model = model.rsplit_once("/variant/").filter(|(_, effort)| !effort.contains('/')).map_or(model, |(id, _)| id);
        let id = model_id(model);
        let mut candidate = id.as_str();
        loop {
            match self.official.get(candidate).map(String::as_str) {
                Some("") => return None,
                Some(key) => return self.models.get(key),
                None => candidate = strip_variant(candidate)?,
            }
        }
    }
}

fn model_id(selector: &str) -> String {
    selector.rsplit('/').next().unwrap_or_default().trim().to_ascii_lowercase()
}

fn bind_alias(map: &mut BTreeMap<String, (u8, String)>, models: &BTreeMap<String, Value>, alias: &str, key: &str, rank: u8) {
    let alias = model_id(alias);
    if alias.is_empty() { return; }
    let Some((rank0, key0)) = map.get_mut(&alias) else {
        map.insert(alias, (rank, key.to_string()));
        return;
    };
    if rank > *rank0 {
        *rank0 = rank;
        *key0 = key.to_string();
    } else if rank == *rank0 && !key0.is_empty() && models[&*key0] != models[key] {
        key0.clear();
    }
}

/// 日期快照（`-1017`）是模型 ID 的一部分；只回退已知的渠道变体后缀。
fn strip_variant(id: &str) -> Option<&str> {
    ["[1m]", "-1m", "-fast", "-paid", ":free"].iter().find_map(|suffix| id.strip_suffix(suffix))
        .or_else(|| (id == "deepseek-v4.1-flash-eco").then_some("deepseek-v4.1-flash"))
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
