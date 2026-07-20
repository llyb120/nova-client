use serde_json::Value;
use std::time::Duration;

pub const AUTO_VALUE_MODEL: &str = "__nova_auto_value__";
pub const AUTO_IQ_MODEL: &str = "__nova_auto_iq__";
pub const AUTO_COMMUNITY_MODEL: &str = "__nova_auto_community__";

const RADAR_URL: &str = "https://codexradar.com/";
const RADAR_JSON_URL: &str = "https://codexradar.com/current.json";
const RADAR_RATINGS_URL: &str = "https://codexradar.com/api/model-ratings";

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
    matches!(
        model,
        AUTO_VALUE_MODEL | AUTO_IQ_MODEL | AUTO_COMMUNITY_MODEL
    )
}

pub fn selection_label(model: &str) -> &'static str {
    match model {
        AUTO_IQ_MODEL => "按智商",
        AUTO_COMMUNITY_MODEL => "按社区评分",
        _ => "按性价比",
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
    let winner = if selected == AUTO_COMMUNITY_MODEL {
        let ratings = fetch_text(&client, RADAR_RATINGS_URL).await?;
        let ratings: Value = serde_json::from_str(&ratings)
            .map_err(|e| format!("解析 Codex 雷达社区评分失败：{e}"))?;
        community_rating_winner(&ratings)?
    } else {
        let summary = fetch_text(&client, RADAR_JSON_URL).await?;
        let summary: Value =
            serde_json::from_str(&summary).map_err(|e| format!("解析 Codex 雷达状态失败：{e}"))?;
        let entries = radar_models(&summary)?;
        if selected == AUTO_IQ_MODEL {
            latest_iq_winner(&entries)?
        } else {
            let homepage = fetch_text(&client, RADAR_URL).await?;
            latest_value_winner(&entries, &homepage)?
        }
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
        let Some(rest) = tooltip.strip_prefix("custom_value|") else {
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

fn community_rating_winner(ratings: &Value) -> Result<RadarModel, String> {
    ratings
        .get("models")
        .and_then(Value::as_array)
        .ok_or("Codex 雷达社区评分响应缺少 models")?
        .iter()
        .filter_map(|rating| {
            let id = rating.get("id")?.as_str()?;
            let model = rating.get("group")?.as_str()?;
            let average = rating.get("average")?.as_f64()?;
            let count = rating.get("count")?.as_u64()?;
            if count == 0 {
                return None;
            }
            let model_slug = model
                .split_whitespace()
                .map(str::to_ascii_lowercase)
                .collect::<Vec<_>>()
                .join("-");
            let model_key = normalize_key(&model_slug);
            let id_key = normalize_key(id);
            let effort = id_key.strip_prefix(&format!("{model_key}_"))?;
            if effort == "ultra" {
                return None;
            }
            Some((
                RadarModel {
                    key: id.to_string(),
                    model: model_slug,
                    effort: effort.to_string(),
                    date: ratings
                        .get("updated_at")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    iq: average,
                },
                count,
            ))
        })
        .max_by(|(a, a_count), (b, b_count)| {
            a.iq.total_cmp(&b.iq).then_with(|| a_count.cmp(b_count))
        })
        .map(|(winner, _)| winner)
        .ok_or_else(|| "Codex 雷达没有可用的社区体感评分（已排除 ultra）".into())
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
    if let Some(effort) = effort {
        return same_model(model, &winner.model) && effort == winner.effort;
    }

    // Some OpenCode providers expose Codex models as one flat id instead of a
    // model plus `/variant/<effort>`, for example:
    //   Codex:    gpt-5.6-sol + max
    //   OpenCode: windsurf/gpt-5-6-sol-max
    // Normalize punctuation, peel off the effort suffix, then compare the model
    // part so both representations resolve to the same locally available model.
    let model = normalize_key(model);
    let effort_suffix = format!("_{}", normalize_key(&winner.effort));
    model
        .strip_suffix(&effort_suffix)
        .is_some_and(|model| same_model(model, &winner.model))
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
        let html = r#"<circle data-model-key="smart" data-model-iq-tooltip-key="custom_value|2026-07-18T08:00:00+08:00|90.0"><circle data-model-key="value" data-model-iq-tooltip-key="custom_value|2026-07-18T08:00:00+08:00|120.0">"#;
        assert_eq!(latest_value_winner(&entries, html).unwrap().key, "value");
        assert_eq!(
            compact_key("gpt_5_6_sol_max"),
            compact_key("gpt_56_sol_max")
        );
    }

    #[test]
    fn picks_highest_community_rating_and_excludes_ultra() {
        let ratings = json!({
            "updated_at": "2026-07-19T02:46:38.504Z",
            "models": [
                {"id":"gpt-5.6-sol-ultra","group":"GPT-5.6 Sol","average":9.9,"count":100},
                {"id":"gpt-5.6-sol-max","group":"GPT-5.6 Sol","average":8.5,"count":10},
                {"id":"gpt-5.6-terra-max","group":"GPT-5.6 Terra","average":8.5,"count":20},
                {"id":"gpt-5.6-luna-high","group":"GPT-5.6 Luna","average":null,"count":0}
            ]
        });
        let winner = community_rating_winner(&ratings).unwrap();
        assert_eq!(winner.model, "gpt-5.6-terra");
        assert_eq!(winner.effort, "max");
        assert_eq!(selection_label(AUTO_COMMUNITY_MODEL), "按社区评分");
        assert!(is_auto_model(AUTO_COMMUNITY_MODEL));
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

    #[test]
    fn does_not_map_radar_max_to_opencode_xhigh_variant() {
        let winner = RadarModel {
            key: "x".into(),
            model: "gpt-5.6-terra".into(),
            effort: "max".into(),
            date: "x".into(),
            iq: 1.0,
        };
        let opencode = json!({"configOptions":[{"id":"model","options":[
            {"value":"codex/gpt-5.6-terra/variant/xhigh"}
        ]}]});
        assert_eq!(match_available_model(&opencode, &winner, true), None);

        let opencode = json!({"configOptions":[{"id":"model","options":[
            {"value":"codex/gpt-5.6-terra/variant/xhigh"},
            {"value":"codex/gpt-5.6-terra/variant/max"}
        ]}]});
        assert_eq!(
            match_available_model(&opencode, &winner, true).as_deref(),
            Some("codex/gpt-5.6-terra/variant/max")
        );
    }

    #[test]
    fn matches_opencode_flat_model_ids_with_embedded_effort() {
        let winner = RadarModel {
            key: "x".into(),
            model: "gpt-5.6-sol".into(),
            effort: "max".into(),
            date: "x".into(),
            iq: 1.0,
        };
        let opencode = json!({"configOptions":[{"id":"model","options":[
            {"value":"windsurf/gpt-5-6-sol-max"},
            {"value":"windsurf/gpt-5-6-sol-max-priority"}
        ]}]});
        assert_eq!(
            match_available_model(&opencode, &winner, true).as_deref(),
            Some("windsurf/gpt-5-6-sol-max")
        );
    }
}
