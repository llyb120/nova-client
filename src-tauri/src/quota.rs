//! 通过 windsurf 后端的 GetUserStatus 接口查询剩余额度（日/周限额）。
//! devin CLI 的 ACP 模式不提供额度查询，这里直接复用其凭证调用同一后端。

use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Devin CLI 数据目录：Windows `%APPDATA%/devin`；Unix `$XDG_DATA_HOME/devin` 或 `~/.local/share/devin`。
fn devin_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join("devin"))
            .or_else(|| Some(home_dir()?.join("AppData").join("Roaming").join("devin")))
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return Some(PathBuf::from(xdg).join("devin"));
        }
        Some(home_dir()?.join(".local").join("share").join("devin"))
    }
}

/// Devin CLI 缓存目录：Windows `%LOCALAPPDATA%/devin`；Unix 与 data 目录相同。
fn devin_cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("devin"))
    }
    #[cfg(not(windows))]
    {
        devin_data_dir()
    }
}

fn credentials_path() -> Option<PathBuf> {
    Some(devin_data_dir()?.join("credentials.toml"))
}

/// 解析 credentials.toml（仅取 key = "value" 形式的两个字段）
fn read_credentials() -> Result<(String, String), String> {
    let path = credentials_path().ok_or("无法定位 Devin 凭证目录")?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| "未找到 devin 凭证（请先运行 devin 完成登录）".to_string())?;
    let get = |key: &str| -> Option<String> {
        text.lines().find_map(|l| {
            let l = l.trim();
            let rest = l.strip_prefix(key)?.trim_start();
            let rest = rest.strip_prefix('=')?.trim();
            Some(rest.trim_matches('"').to_string())
        })
    };
    let api_key = get("windsurf_api_key").ok_or("凭证中缺少 windsurf_api_key")?;
    let server =
        get("api_server_url").unwrap_or_else(|| "https://server.self-serve.windsurf.com".into());
    Ok((api_key, server))
}

/// 在 JSON 树中递归查找首个指定 key
fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if let Some(hit) = map.get(key) {
                return Some(hit);
            }
            map.values().find_map(|child| find(child, key))
        }
        Value::Array(arr) => arr.iter().find_map(|child| find(child, key)),
        _ => None,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|n| n as f64))
        .or_else(|| v.as_u64().map(|n| n as f64))
}

fn as_i64(v: &Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_u64().map(|n| n as i64))
        .or_else(|| v.as_f64().map(|n| n as i64))
}

/// devin CLI 的版本号（GetCliModelConfigs 按客户端版本过滤可用模型列表）
fn devin_version() -> Option<String> {
    let dir = devin_cache_dir()?;
    let s = std::fs::read_to_string(dir.join("cli").join("cached_version.json")).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    v.get("latest")?.as_str().map(|x| x.to_string())
}

fn model_prices(m: &Value) -> Value {
    let input = find(m, "inputTokenCost")
        .or_else(|| find(m, "inputPrice"))
        .or_else(|| find(m, "promptTokenCost"))
        .and_then(as_f64);
    let cached = find(m, "cachedInputTokenCost")
        .or_else(|| find(m, "cachedPrice"))
        .or_else(|| find(m, "cacheReadTokenCost"))
        .and_then(as_f64);
    let output = find(m, "outputTokenCost")
        .or_else(|| find(m, "outputPrice"))
        .or_else(|| find(m, "completionTokenCost"))
        .and_then(as_f64);
    if input.is_none() && cached.is_none() && output.is_none() {
        return Value::Null;
    }
    json!({
        "input": input,
        "cached": cached,
        "output": output,
    })
}

/// 拉取模型配置（积分倍率/厂商/视觉支持），整理成 { modelUid: ModelCost } 映射。
pub async fn fetch_model_costs() -> Result<Value, String> {
    let (api_key, server) = read_credentials()?;
    let version = devin_version().unwrap_or_else(|| "2026.5.26-8".into());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let body = json!({
        "metadata": {
            "api_key": api_key,
            "ide_name": "windsurf",
            "ide_version": version,
            "extension_name": "windsurf",
            "extension_version": version,
            "locale": "en"
        }
    });
    let resp = client
        .post(format!(
            "{server}/exa.api_server_pb.ApiServerService/GetCliModelConfigs"
        ))
        .header("Connect-Protocol-Version", "1")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("模型配置查询失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("模型配置查询失败：HTTP {}", resp.status()));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("模型配置解析失败：{e}"))?;

    let mut out = serde_json::Map::new();
    if let Some(arr) = v.get("clientModelConfigs").and_then(|x| x.as_array()) {
        for m in arr {
            let uid = m
                .get("modelUid")
                .or_else(|| m.get("model_uid"))
                .and_then(|x| x.as_str())
                .unwrap_or_default();
            if uid.is_empty() {
                continue;
            }
            // protobuf 省略零值：倍率缺失视为 null（前端按 0× 促销免费展示）
            let multiplier = find(m, "creditMultiplier")
                .or_else(|| find(m, "multiplier"))
                .and_then(as_f64);
            let provider = find(m, "provider")
                .or_else(|| find(m, "modelProvider"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let supports_images = find(m, "supportsImages")
                .or_else(|| find(m, "isVision"))
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            let tier = find(m, "tier")
                .or_else(|| find(m, "modelTier"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let pricing = find(m, "pricing")
                .or_else(|| find(m, "pricingType"))
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            out.insert(
                uid.to_string(),
                json!({
                    "multiplier": multiplier,
                    "provider": provider,
                    "supportsImages": supports_images,
                    "tier": tier,
                    "pricing": pricing,
                    "prices": model_prices(m),
                }),
            );
        }
    }
    Ok(Value::Object(out))
}

/// 查询剩余额度，返回前端 Quota 形状。
pub async fn fetch_quota() -> Result<Value, String> {
    let (api_key, server) = read_credentials()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let body = json!({
        "metadata": {
            "api_key": api_key,
            "ide_name": "windsurf",
            "ide_version": "1.0.0",
            "extension_name": "windsurf",
            "extension_version": "1.0.0",
            "locale": "en"
        }
    });
    let resp = client
        .post(format!(
            "{server}/exa.seat_management_pb.SeatManagementService/GetUserStatus"
        ))
        .header("Connect-Protocol-Version", "1")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("额度查询失败：{e}"))?;
    if !resp.status().is_success() {
        return Err(format!("额度查询失败：HTTP {}", resp.status()));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| format!("额度解析失败：{e}"))?;

    let plan = find(&v, "planName")
        .or_else(|| find(&v, "plan"))
        .and_then(|x| x.as_str())
        .map(|s| s.to_string());

    // 优先用后端直接给的剩余百分比；否则用 used/available 推算
    let daily_percent = find(&v, "dailyQuotaRemainingPercent")
        .or_else(|| find(&v, "daily_quota_remaining_percent"))
        .and_then(as_f64)
        .or_else(|| {
            let used = find(&v, "usedPromptCredits").and_then(as_f64)?;
            let avail = find(&v, "availablePromptCredits").and_then(as_f64)?;
            let total = used + avail;
            if total <= 0.0 {
                return Some(100.0);
            }
            Some((avail / total) * 100.0)
        })
        .unwrap_or(100.0);

    let weekly_percent = find(&v, "weeklyQuotaRemainingPercent")
        .or_else(|| find(&v, "weekly_quota_remaining_percent"))
        .and_then(as_f64)
        .unwrap_or(daily_percent);

    let daily_reset = find(&v, "dailyQuotaResetAtUnix")
        .or_else(|| find(&v, "daily_quota_reset_at_unix"))
        .or_else(|| find(&v, "dailyPromptCreditsResetAt"))
        .and_then(as_i64);
    let weekly_reset = find(&v, "weeklyQuotaResetAtUnix")
        .or_else(|| find(&v, "weekly_quota_reset_at_unix"))
        .or_else(|| find(&v, "weeklyPromptCreditsResetAt"))
        .and_then(as_i64);

    // flex / 按量积分：百进制时除以 100；优先 available - used
    let flex_credits = find(&v, "availableFlexCredits")
        .and_then(as_f64)
        .map(|avail| {
            let used = find(&v, "usedFlexCredits").and_then(as_f64).unwrap_or(0.0);
            let raw = (avail - used).max(0.0);
            // 常见存储为百分之一积分
            if raw >= 100.0 {
                raw / 100.0
            } else {
                raw
            }
        })
        .or_else(|| find(&v, "flexCreditBalance").and_then(as_f64))
        .or_else(|| find(&v, "flexCredits").and_then(as_f64))
        .or_else(|| {
            find(&v, "overageBalanceMicros")
                .and_then(as_f64)
                .map(|micros| micros / 1_000_000.0)
        });

    Ok(json!({
        "plan": plan,
        "dailyPercent": daily_percent,
        "weeklyPercent": weekly_percent,
        "dailyResetAt": daily_reset,
        "weeklyResetAt": weekly_reset,
        "flexCredits": flex_credits,
    }))
}
