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
    /// 旧缓存没有别名索引；为真才按官方价查，否则仍用完整键。
    #[serde(default)]
    indexed: bool,
}

impl Prices {
    pub(super) fn load(dir: &Path) -> Self {
        std::fs::read(Self::path(dir)).ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
    }

    fn path(dir: &Path) -> PathBuf { dir.join("model-prices.json") }
    pub(super) fn ready(&self) -> bool { !self.models.is_empty() }
    pub(super) fn stale(&self) -> bool {
        !self.ready() || !self.indexed
            || chrono::Utc::now().timestamp_millis().saturating_sub(self.fetched_at) >= REFRESH_MS
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
                if !entry["cost"].is_object() { continue; }
                let key = format!("{provider}/{model}");
                let canonical = entry["canonical_model_id"].as_str().unwrap_or("").trim().to_string();
                models.insert(key.clone(), entry["cost"].clone());
                rows.push(Row { key, provider: provider.clone(), model: model.clone(), canonical });
            }
        }
        if models.is_empty() { return Err("价格目录没有模型价格".into()); }

        let mut by_canon: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, row) in rows.iter().enumerate() {
            if !row.canonical.is_empty() {
                by_canon.entry(row.canonical.to_ascii_lowercase()).or_default().push(i);
            }
        }
        let mut aliases: BTreeMap<String, (u8, String)> = BTreeMap::new();
        for (canon, idxs) in &by_canon {
            let Some((owner, id)) = canon.split_once('/') else { continue };
            let owner_idxs: Vec<usize> = idxs.iter().copied()
                .filter(|i| rows[*i].provider.eq_ignore_ascii_case(owner)).collect();
            if owner_idxs.is_empty() { continue; }
            let agreed = owner_idxs.iter().all(|i| models[&rows[*i].key] == models[&rows[owner_idxs[0]].key]);
            let exact = owner_idxs.iter().copied().find(|i| rows[*i].model.eq_ignore_ascii_case(id));
            if !agreed {
                for i in &owner_idxs {
                    bind_alias(&mut aliases, &rows[*i].model, &rows[*i].key, 3);
                    bind_alias(&mut aliases, &rows[*i].key, &rows[*i].key, 3);
                }
                if let Some(i) = exact {
                    let key = rows[i].key.clone();
                    bind_alias(&mut aliases, canon, &key, 3);
                    bind_alias(&mut aliases, id, &key, 3);
                }
                for i in idxs {
                    if rows[*i].provider.eq_ignore_ascii_case(owner) { continue; }
                    let Some(matched) = owner_idxs.iter().copied()
                        .find(|o| rows[*o].model.eq_ignore_ascii_case(&rows[*i].model)) else { continue };
                    bind_alias(&mut aliases, &rows[*i].model, &rows[matched].key, 1);
                    bind_alias(&mut aliases, &rows[*i].key, &rows[matched].key, 1);
                }
                continue;
            }
            let key = rows[exact.unwrap_or(owner_idxs[0])].key.clone();
            bind_alias(&mut aliases, canon, &key, 3);
            bind_alias(&mut aliases, id, &key, 3);
            for i in idxs {
                let rank = if rows[*i].provider.eq_ignore_ascii_case(owner) { 3 } else { 1 };
                bind_alias(&mut aliases, &rows[*i].model, &key, rank);
                bind_alias(&mut aliases, &rows[*i].key, &key, rank);
            }
        }
        for row in rows.iter().filter(|row| row.canonical.is_empty()) {
            bind_alias(&mut aliases, &row.model, &row.key, 0);
            bind_alias(&mut aliases, &row.key, &row.key, 0);
        }
        let official = aliases.into_iter().map(|(alias, (_, key))| (alias, key)).collect();
        Ok(Self { fetched_at, models, official, indexed: true })
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
        let mut cost = self.official_cost(model)?;
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

    fn official_cost(&self, model: &str) -> Option<&Value> {
        let model = model.split("/variant/").next().filter(|id| !id.is_empty()).unwrap_or(model);
        if !self.indexed { return self.models.get(model); }
        let id = model.split_once('/').map(|(_, rest)| rest).filter(|id| !id.is_empty()).unwrap_or(model);
        for candidate in price_aliases(id) {
            match self.official.get(&candidate.to_ascii_lowercase()).map(String::as_str) {
                Some("") => return None,
                Some(key) => return self.models.get(key),
                None => {}
            }
        }
        None
    }
}

fn bind_alias(map: &mut BTreeMap<String, (u8, String)>, alias: &str, key: &str, rank: u8) {
    let alias = alias.trim().to_ascii_lowercase();
    if alias.is_empty() || key.is_empty() { return; }
    let Some((rank0, key0)) = map.get_mut(&alias) else {
        map.insert(alias, (rank, key.to_string()));
        return;
    };
    if key0.is_empty() {
        if rank > *rank0 { *rank0 = rank; *key0 = key.to_string(); }
        return;
    }
    if key0.eq_ignore_ascii_case(key) {
        if rank > *rank0 { *rank0 = rank; }
        return;
    }
    if rank > *rank0 {
        *rank0 = rank;
        *key0 = key.to_string();
        return;
    }
    if rank == *rank0 { key0.clear(); }
}

/// 与 lyra::presets::variant_base 同一组计费后缀。日期快照（`-1017`）是官方 id 的一部分，不在这里剔掉。
fn strip_variant(id: &str) -> Option<&str> {
    ["[1m]", "-1m", "-fast", "-paid", ":free"].iter().find_map(|suffix| id.strip_suffix(suffix))
}

fn price_aliases(id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_chain = |mut cur: String| {
        for _ in 0..4 {
            if cur.is_empty() || out.iter().any(|seen: &String| seen.eq_ignore_ascii_case(&cur)) { break; }
            let next = strip_variant(&cur).unwrap_or("").to_string();
            out.push(cur);
            if next.is_empty() { break; }
            cur = next;
        }
    };
    push_chain(id.to_string());
    if let Some((_, last)) = id.rsplit_once('/') {
        push_chain(last.to_string());
    }
    out
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
