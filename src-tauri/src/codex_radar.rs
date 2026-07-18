use serde_json::Value;
use std::time::Duration;

pub const AUTO_VALUE_MODEL: &str = "__nova_auto_value__";
pub const AUTO_IQ_MODEL: &str = "__nova_auto_iq__";

const RADAR_URL: &str = "https://codexradar.com/";
const RADAR_JSON_URL: &str = "https://codexradar.com/current.json";

#[derive(Clone, Debug, PartialEq)]
struct RadarModel {
    key: String,
    model: String,
    effort: String,
    date: String,
    iq: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAutoModel {
    pub value: String,
    pub label: String,
}

pub fn is_auto_model(model: &str) -> bool {
    matches!(model, AUTO_VALUE_MODEL | AUTO_IQ_MODEL)
}

pub fn selection_label(model: &str) -> &'static str {
    if model == AUTO_IQ_MODEL {
        "按智商"
    } else {
        "按性价比"
    }
}

pub async fn resolve_auto_model(
    selected: &str,
    options: &Value,
    open_code: bool,
) -> Result<ResolvedAutoModel, String> {
    if !is_auto_model(selected) {
        return Ok(ResolvedAutoModel {
            value: selected.to_string(),
            label: selected.to_string(),
        });
    }
    if open_code && !has_gpt_model(options) {
        return Err("OpenCode 尚未配置 GPT 模型，Auto 路由未生效".into());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Nova/codex-radar-auto-router")
        .build()
        .map_err(|e| format!("创建 Codex 雷达请求失败：{e}"))?;
    let summary = fetch_text(&client, RADAR_JSON_URL).await?;
    let summary: Value =
        serde_json::from_str(&summary).map_err(|e| format!("解析 Codex 雷达状态失败：{e}"))?;
    let entries = radar_models(&summary)?;
    let winner = if selected == AUTO_IQ_MODEL {
        latest_iq_winner(&entries)?
    } else {
        let homepage = fetch_text(&client, RADAR_URL).await?;
        latest_value_winner(&entries, &homepage)?
    };
    let value = match_available_model(options, &winner, open_code).ok_or_else(|| {
        format!(
            "Codex 雷达当前第一名 {} · {}，但本机没有对应模型/推理档位",
            winner.model, winner.effort
        )
    })?;
    Ok(ResolvedAutoModel {
        value,
        label: format!("{} · {}", winner.model, winner.effort),
    })
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("抓取 Codex 雷达失败：{e}"))?
        .error_for_status()
        .map_err(|e| format!("抓取 Codex 雷达失败：{e}"))?;
    response
        .text()
        .await
        .map_err(|e| format!("读取 Codex 雷达响应失败：{e}"))
}

fn radar_models(summary: &Value) -> Result<Vec<RadarModel>, String> {
    let model_iq = summary
        .get("model_iq")
        .ok_or("Codex 雷达响应缺少 model_iq")?;
    let mut entries = Vec::new();
    if let Some(latest) = model_iq.get("latest") {
        if let Some(entry) = radar_model(latest, None) {
            entries.push(entry);
        }
    }
    if let Some(comparisons) = model_iq.get("comparisons").and_then(Value::as_object) {
        for (key, comparison) in comparisons {
            if let Some(entry) = comparison
                .get("latest")
                .and_then(|latest| radar_model(latest, Some(key)))
            {
                entries.push(entry);
            }
        }
    }
    (!entries.is_empty())
        .then_some(entries)
        .ok_or_else(|| "Codex 雷达没有可用的最新模型排名".into())
}

fn radar_model(latest: &Value, key: Option<&str>) -> Option<RadarModel> {
    let model = latest.get("model")?.as_str()?.to_string();
    let effort = latest.get("reasoning_effort")?.as_str()?.to_string();
    Some(RadarModel {
        key: key
            .map(str::to_string)
            .unwrap_or_else(|| format!("{}_{}", normalize_key(&model), normalize_key(&effort))),
        model,
        effort,
        date: latest.get("date")?.as_str()?.to_string(),
        iq: latest.get("score")?.as_f64()?,
    })
}

fn latest_iq_winner(entries: &[RadarModel]) -> Result<RadarModel, String> {
    let latest = entries
        .iter()
        .map(|entry| entry.date.as_str())
        .max()
        .ok_or("Codex 雷达没有 IQ 排名")?;
    entries
        .iter()
        .filter(|entry| entry.date == latest)
        .max_by(|a, b| a.iq.total_cmp(&b.iq))
        .cloned()
        .ok_or_else(|| "Codex 雷达没有 IQ 第一名".into())
}

fn latest_value_winner(entries: &[RadarModel], html: &str) -> Result<RadarModel, String> {
    let mut points = Vec::new();
    for tag in html
        .split('<')
        .filter_map(|part| part.split_once('>').map(|v| v.0))
    {
        let Some(key) = attr(tag, "data-model-key") else {
            continue;
        };
        let Some(tooltip) = attr(tag, "data-model-iq-tooltip-key") else {
            continue;
        };
        let Some(rest) = tooltip.strip_prefix("value|") else {
            continue;
        };
        let Some((date, score)) = rest.rsplit_once('|') else {
            continue;
        };
        if let Ok(score) = score.parse::<f64>() {
            points.push((date.to_string(), key.to_string(), score));
        }
    }
    let latest = points
        .iter()
        .map(|point| point.0.as_str())
        .max()
        .ok_or("Codex 雷达首页没有性价比排名")?;
    let winner_key = points
        .iter()
        .filter(|point| point.0 == latest)
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .map(|point| point.1.as_str())
        .ok_or("Codex 雷达首页没有性价比第一名")?;
    entries
        .iter()
        .find(|entry| compact_key(&entry.key) == compact_key(winner_key))
        .cloned()
        .ok_or_else(|| format!("Codex 雷达性价比第一名 {winner_key} 缺少模型信息"))
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let marker = format!("{name}=\"");
    let rest = tag.split_once(&marker)?.1;
    rest.split_once('"').map(|value| value.0)
}

fn match_available_model(options: &Value, winner: &RadarModel, open_code: bool) -> Option<String> {
    model_options(options)
        .filter_map(|option| {
            option
                .get("value")
                .and_then(Value::as_str)
                .map(|v| (option, v))
        })
        .filter(|(_, value)| !is_auto_model(value))
        .find_map(|(option, value)| {
            if open_code {
                matches_opencode(value, winner).then(|| value.to_string())
            } else {
                matches_codex(option, value, winner).then(|| value.to_string())
            }
        })
}

fn model_options(options: &Value) -> impl Iterator<Item = &Value> {
    options["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|config| config["id"] == "model")
        .and_then(|config| config["options"].as_array())
        .into_iter()
        .flatten()
}

fn has_gpt_model(options: &Value) -> bool {
    model_options(options).any(|option| {
        option
            .get("value")
            .and_then(Value::as_str)
            .is_some_and(|value| value.to_ascii_lowercase().contains("gpt"))
    })
}

fn matches_codex(option: &Value, value: &str, winner: &RadarModel) -> bool {
    let effort = option
        .pointer("/_meta/codex.ai/effort")
        .and_then(Value::as_str)
        .or_else(|| value.rsplit_once(':').map(|parts| parts.1));
    let model = effort
        .and_then(|effort| value.strip_suffix(&format!(":{effort}")))
        .unwrap_or(value);
    same_model(model, &winner.model) && effort == Some(winner.effort.as_str())
}

fn matches_opencode(value: &str, winner: &RadarModel) -> bool {
    if !value.to_ascii_lowercase().contains("gpt") {
        return false;
    }
    let (base, effort) = value
        .rsplit_once("/variant/")
        .map(|parts| (parts.0, Some(parts.1)))
        .unwrap_or((value, None));
    let model = base.split_once('/').map(|parts| parts.1).unwrap_or(base);
    same_model(model, &winner.model) && effort == Some(winner.effort.as_str())
}

fn same_model(available: &str, radar: &str) -> bool {
    let available = normalize_key(available);
    let radar = normalize_key(radar);
    available == radar || available.ends_with(&format!("_{radar}"))
}

fn normalize_key(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn compact_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn picks_latest_iq_and_value_winners() {
        let entries = vec![
            RadarModel {
                key: "smart".into(),
                model: "gpt-smart".into(),
                effort: "high".into(),
                date: "2026-07-18T08:00:00+08:00".into(),
                iq: 101.0,
            },
            RadarModel {
                key: "value".into(),
                model: "gpt-value".into(),
                effort: "low".into(),
                date: "2026-07-18T08:00:00+08:00".into(),
                iq: 80.0,
            },
        ];
        assert_eq!(latest_iq_winner(&entries).unwrap().key, "smart");
        let html = r#"<circle data-model-key="smart" data-model-iq-tooltip-key="value|2026-07-18T08:00:00+08:00|90.0"><circle data-model-key="value" data-model-iq-tooltip-key="value|2026-07-18T08:00:00+08:00|120.0">"#;
        assert_eq!(latest_value_winner(&entries, html).unwrap().key, "value");
        assert_eq!(
            compact_key("gpt_5_6_sol_max"),
            compact_key("gpt_56_sol_max")
        );
    }

    #[test]
    fn matches_codex_and_opencode_effort_variants() {
        let winner = RadarModel {
            key: "x".into(),
            model: "gpt-5.6-terra".into(),
            effort: "high".into(),
            date: "x".into(),
            iq: 1.0,
        };
        let codex = json!({"configOptions":[{"id":"model","options":[{"value":"gpt-5.6-terra:high","_meta":{"codex.ai/effort":"high"}}]}]});
        let opencode = json!({"configOptions":[{"id":"model","options":[{"value":"openai/gpt-5.6-terra/variant/high"}]}]});
        assert_eq!(
            match_available_model(&codex, &winner, false).as_deref(),
            Some("gpt-5.6-terra:high")
        );
        assert_eq!(
            match_available_model(&opencode, &winner, true).as_deref(),
            Some("openai/gpt-5.6-terra/variant/high")
        );
    }
}
