//! 语义检索的「外置引擎」客户端：主程序不编译任何推理依赖（onnxruntime 等），
//! 而是运行时调用一个 OpenAI 兼容的 embedding 服务（本地 Ollama / LM Studio / 云端皆可）。
//!
//! - embedding：POST {endpoint}/v1/embeddings（Ollama 自带 OpenAI 兼容层）；
//! - 模型下载：POST {endpoint}/api/pull（Ollama），即"点按钮手动下载模型"；
//! - 向量落地：VectorStore 持久化每个员工每条记忆的向量，惰性补算、换模型即失效。
//!
//! 这样语义能力是「可选、按需、离线」的：不装/不配就自动回退 BM25，零编译成本。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

/// 去掉结尾的 `/` 和 `/v1`，得到服务 base 地址。
fn base_of(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    let e = e.strip_suffix("/v1").unwrap_or(e);
    e.trim_end_matches('/').to_string()
}

/// 调用 OpenAI 兼容 /v1/embeddings，返回每条文本的向量。
pub async fn embed(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    api_key: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}/v1/embeddings", base_of(endpoint));
    let mut req = client
        .post(&url)
        .json(&json!({ "model": model, "input": texts }))
        .timeout(Duration::from_secs(120));
    if !api_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", api_key.trim()));
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {code} {}", one_line(&body, 200)));
    }
    let v: Value = resp.json().await.map_err(|e| e.to_string())?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or("embedding 响应缺少 data 字段")?;
    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let arr = item
            .get("embedding")
            .and_then(|e| e.as_array())
            .ok_or("embedding 响应缺少 embedding 数组")?;
        out.push(
            arr.iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect(),
        );
    }
    Ok(out)
}

/// 探测服务可达性：embed 一个探针，返回向量维度。
pub async fn probe(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
    api_key: &str,
) -> Result<usize, String> {
    let v = embed(client, endpoint, model, api_key, &["ping".to_string()]).await?;
    Ok(v.into_iter().next().map(|x| x.len()).unwrap_or(0))
}

/// 触发 Ollama 拉取模型（"点按钮手动下载"）。阻塞直到完成（stream=false）。
pub async fn ollama_pull(
    client: &reqwest::Client,
    endpoint: &str,
    model: &str,
) -> Result<(), String> {
    let url = format!("{}/api/pull", base_of(endpoint));
    let resp = client
        .post(&url)
        .json(&json!({ "model": model, "stream": false }))
        .timeout(Duration::from_secs(3600))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let code = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {code} {}", one_line(&body, 200)));
    }
    Ok(())
}

/// 余弦相似度。
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..n {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max {
        flat
    } else {
        chars[..max].iter().collect::<String>() + "…"
    }
}

// ===== 向量存储 =====

#[derive(Serialize, Deserialize, Default)]
struct VectorsFile {
    model: String,
    vecs: HashMap<String, HashMap<String, Vec<f32>>>,
}

/// 每个员工每条记忆（按 ts）的向量缓存；换模型时整体失效重建。
pub struct VectorStore {
    path: PathBuf,
    /// 当前向量所用的 embedding 模型（变更即视为全部失效）
    pub model: String,
    /// employeeId -> ts(字符串) -> 向量
    vecs: HashMap<String, HashMap<String, Vec<f32>>>,
}

impl VectorStore {
    pub fn load(dir: &PathBuf) -> Self {
        let path = dir.join("employee_vectors.json");
        let f = fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<VectorsFile>(&s).ok())
            .unwrap_or_default();
        VectorStore {
            path,
            model: f.model,
            vecs: f.vecs,
        }
    }

    pub fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let f = VectorsFile {
            model: self.model.clone(),
            vecs: self.vecs.clone(),
        };
        if let Ok(json) = serde_json::to_string(&f) {
            let _ = fs::write(&self.path, json);
        }
    }

    pub fn get(&self, employee_id: &str, ts: i64) -> Option<&Vec<f32>> {
        self.vecs
            .get(employee_id)
            .and_then(|m| m.get(&ts.to_string()))
    }

    pub fn put(&mut self, employee_id: &str, ts: i64, v: Vec<f32>) {
        self.vecs
            .entry(employee_id.to_string())
            .or_default()
            .insert(ts.to_string(), v);
    }

    /// 换模型：清空全部向量并记录新模型（下次检索惰性重建）。
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
        self.vecs.clear();
    }

    /// 清空某员工的向量（用于"重建索引"）。
    pub fn clear_employee(&mut self, employee_id: &str) {
        self.vecs.remove(employee_id);
    }
}
