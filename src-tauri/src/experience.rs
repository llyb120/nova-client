use crate::settings::{ExperienceExpertConfig, Settings};
use crate::threads::{now_ms, AgentKind, Item, Thread};
use crate::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use futures_util::future::join_all;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tauri::{AppHandle, Emitter, Manager};

const MAX_TRAIN_THREADS: usize = 12;
const MAX_EXPERIENCES_PER_EXPERT: usize = 800;
static STORE: OnceLock<Mutex<ExperienceStore>> = OnceLock::new();
static TRAINING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperienceEntry {
    pub id: String,
    pub expert_id: String,
    #[serde(default = "default_entry_kind")]
    pub kind: String,
    pub trigger: String,
    pub action: String,
    #[serde(default)]
    pub avoid: String,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub source_thread_ids: Vec<String>,
    #[serde(default)]
    pub parent_ids: Vec<String>,
    #[serde(default)]
    pub generation: u32,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    #[serde(default)]
    pub utility: f64,
    #[serde(default)]
    pub positive_count: u32,
    #[serde(default)]
    pub negative_count: u32,
    #[serde(default)]
    pub user_feedback: i8,
    #[serde(default)]
    pub hit_count: u32,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub last_used_at: i64,
    #[serde(default)]
    pub status: String,
}

fn default_confidence() -> f64 { 0.4 }
fn default_entry_kind() -> String { "experience".into() }

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct ExperienceStore {
    #[serde(skip)]
    path: PathBuf,
    #[serde(default)]
    experiences: Vec<ExperienceEntry>,
    #[serde(default)]
    training_sessions: Vec<ExperienceTrainingSession>,
    /// 旧版全局游标，仅保留用于兼容已有经验库。
    #[serde(default)]
    processed_threads: HashMap<String, i64>,
    /// 各专家独立消费来源会话，未激活专家不会错过证据。
    #[serde(default)]
    expert_processed_threads: HashMap<String, HashMap<String, i64>>,
    #[serde(default)]
    last_trained_experts: Vec<String>,
    #[serde(default)]
    last_train_at: i64,
    #[serde(default)]
    last_attempt_at: i64,
    #[serde(default)]
    training_cycles: u64,
    #[serde(default)]
    evolution_generation: u64,
    #[serde(default)]
    last_evolution_at: i64,
    #[serde(default)]
    expert_activations: HashMap<String, u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperienceTrainingSession {
    pub id: String,
    pub created_at: i64,
    pub agent_kind: String,
    pub model: String,
    #[serde(default)]
    pub expert_id: String,
    pub source_thread_ids: Vec<String>,
    pub conversation: String,
    pub output: String,
    pub status: String,
    #[serde(default)]
    pub error: String,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct TrainingOutput {
    #[serde(default)]
    experts: Vec<ExpertOutput>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ExpertOutput {
    expert_id: String,
    #[serde(default)]
    experiences: Vec<CandidateExperience>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct CandidateExperience {
    #[serde(default = "default_entry_kind")]
    kind: String,
    #[serde(default)]
    trigger: String,
    action: String,
    #[serde(default)]
    avoid: String,
    #[serde(default)]
    scope: Vec<String>,
    #[serde(default = "default_confidence")]
    confidence: f64,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct EvolutionReview {
    #[serde(default)]
    experiences: Vec<EvolutionReviewedExperience>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct EvolutionReviewedExperience {
    candidate_id: String,
    #[serde(flatten)]
    experience: CandidateExperience,
}

impl ExperienceStore {
    fn load(root: &Path) -> Self {
        let path = root.join("experience_memory.json");
        let mut store = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Self>(&raw).ok())
            .unwrap_or_default();
        store.path = path;
        store
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() { let _ = fs::create_dir_all(parent); }
        if let Ok(raw) = serde_json::to_string_pretty(self) {
            let tmp = self.path.with_extension("json.tmp");
            if fs::write(&tmp, raw).is_ok() {
                let _ = fs::remove_file(&self.path);
                let _ = fs::rename(tmp, &self.path);
            }
        }
    }
}

pub fn init(root: &Path) {
    let _ = STORE.set(Mutex::new(ExperienceStore::load(root)));
}

fn store() -> Result<&'static Mutex<ExperienceStore>, String> {
    STORE.get().ok_or_else(|| "经验系统尚未初始化".to_string())
}

fn terms(text: &str) -> HashSet<String> {
    let lower = text.to_lowercase();
    let mut out: HashSet<String> = lower
        .split(|c: char| !c.is_alphanumeric() && !('\u{4e00}'..='\u{9fff}').contains(&c))
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect();
    // 中文通常没有空格；加入 CJK 双字片段，避免整句必须完全相同才命中。
    let cjk: Vec<char> = lower.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).collect();
    for pair in cjk.windows(2) { out.insert(pair.iter().collect()); }
    out
}

fn relevance(query: &HashSet<String>, entry: &ExperienceEntry) -> f64 {
    if query.is_empty() { return 0.0; }
    let hay = terms(&format!("{} {} {} {}", entry.trigger, entry.action, entry.avoid, entry.scope.join(" ")));
    query.intersection(&hay).count() as f64 / query.len().max(1) as f64
}

pub fn load_trained_memory(query: &str, limit: usize) -> Result<Value, String> {
    let query_terms = terms(query);
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let mut by_expert: HashMap<String, Vec<(usize, f64)>> = HashMap::new();
    for (index, entry) in guard.experiences.iter().enumerate() {
        if entry.status == "quarantined" { continue; }
        let rel = relevance(&query_terms, entry);
        if rel <= 0.0 { continue; }
        let score = 0.55 * rel + 0.25 * entry.utility + 0.20 * entry.confidence;
        by_expert.entry(entry.expert_id.clone()).or_default().push((index, score));
    }
    for entries in by_expert.values_mut() { entries.sort_by(|a, b| b.1.total_cmp(&a.1)); }
    let nonce = now_ms();
    let mut experts: Vec<(String, f64)> = by_expert.iter().map(|(id, rows)| {
        let preview = rows.iter().take(3).map(|x| x.1).sum::<f64>() / rows.len().min(3).max(1) as f64;
        let activations = *guard.expert_activations.get(id).unwrap_or(&0) as f64;
        // 相关性为主，叠加有界随机扰动与低激活奖励；无关岛不会进入候选。
        let jitter = deterministic_roll(&format!("recall:{nonce}:{id}")) * 0.16;
        (id.clone(), preview + 0.12 / (1.0 + activations).sqrt() + jitter)
    }).collect();
    experts.sort_by(|a, b| b.1.total_cmp(&a.1));
    let selected: Vec<String> = experts.into_iter().take(3).map(|x| x.0).collect();
    let mut candidates = Vec::new();
    for id in &selected {
        if let Some(rows) = by_expert.get(id) {
            for &(index, score) in rows.iter().take(4) { candidates.push((index, score)); }
        }
    }
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut chosen = Vec::new();
    let mut per_expert: HashMap<String, usize> = HashMap::new();
    for (index, _) in candidates {
        if chosen.len() >= limit.clamp(1, 12) { break; }
        let id = guard.experiences[index].expert_id.clone();
        if *per_expert.get(&id).unwrap_or(&0) >= 3 { continue; }
        if chosen.iter().any(|&old: &usize| {
            let a = terms(&guard.experiences[old].action);
            let b = terms(&guard.experiences[index].action);
            !a.is_empty() && a.intersection(&b).count() as f64 / a.len().min(b.len()).max(1) as f64 > 0.8
        }) { continue; }
        *per_expert.entry(id).or_default() += 1;
        chosen.push(index);
    }
    let now = now_ms();
    for id in &selected { *guard.expert_activations.entry(id.clone()).or_default() += 1; }
    let result: Vec<Value> = chosen.into_iter().map(|index| {
        let entry = &mut guard.experiences[index];
        entry.hit_count = entry.hit_count.saturating_add(1);
        entry.last_used_at = now;
        json!({
            "id": entry.id, "expertId": entry.expert_id, "kind": entry.kind,
            "trigger": entry.trigger, "action": entry.action, "avoid": entry.avoid, "scope": entry.scope,
            "confidence": entry.confidence, "utility": entry.utility
        })
    }).collect();
    guard.save();
    Ok(json!({ "activatedExperts": selected, "experiences": result,
        "instruction": "memory 是可参考事实，rule 是强约束，experience 仅在触发条件匹配时参考；当前会话事实始终优先。" }))
}

pub fn list_memory(configs: &[ExperienceExpertConfig]) -> Result<Value, String> {
    let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let experts: Vec<Value> = configs.iter().map(|c| json!({ "id": c.id, "name": c.name })).collect();
    Ok(json!({
        "experiences": guard.experiences,
        "experts": experts,
        "lastTrainAt": guard.last_train_at,
        "trainingCycles": guard.training_cycles,
        "evolutionGeneration": guard.evolution_generation,
        "training": TRAINING.load(Ordering::SeqCst)
    }))
}

pub fn delete_memory(id: &str) -> Result<Value, String> {
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let before = guard.experiences.len();
    guard.experiences.retain(|entry| entry.id != id);
    let deleted = before - guard.experiences.len();
    if deleted > 0 { guard.save(); }
    Ok(json!({ "deleted": deleted }))
}

pub fn feedback_memory(ids: &[String], reward: f64, note: &str, configs: &[ExperienceExpertConfig]) -> Result<Value, String> {
    let reward = reward.clamp(-1.0, 1.0);
    // load 为空或本轮未采用任何条目时，允许 reward=0 的空反馈完成闭环。
    if ids.is_empty() {
        return if reward == 0.0 {
            Ok(json!({ "updated": 0, "reward": 0, "settled": true }))
        } else {
            Err("feedback_memory 非零反馈必须提供 experienceIds".into())
        };
    }
    let rates: HashMap<&str, f64> = configs.iter().map(|x| (x.id.as_str(), x.value_learning_rate.clamp(0.01, 1.0))).collect();
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let mut updated = 0;
    for entry in guard.experiences.iter_mut().filter(|x| ids.contains(&x.id)) {
        let rate = *rates.get(entry.expert_id.as_str()).unwrap_or(&0.2);
        entry.utility = ((1.0 - rate) * entry.utility + rate * reward).clamp(-1.0, 1.0);
        if reward > 0.0 { entry.positive_count = entry.positive_count.saturating_add(1); }
        if reward < 0.0 { entry.negative_count = entry.negative_count.saturating_add(1); }
        entry.confidence = (entry.confidence + rate * reward * 0.25).clamp(0.0, 1.0);
        if entry.negative_count >= 3 && entry.negative_count > entry.positive_count.saturating_mul(2) { entry.status = "quarantined".into(); }
        entry.updated_at = now_ms();
        if !note.trim().is_empty() && reward < 0.0 { entry.avoid = format!("{}；反馈：{}", entry.avoid.trim(), note.trim()).trim_matches('；').to_string(); }
        updated += 1;
    }
    guard.save();
    Ok(json!({ "updated": updated, "reward": reward }))
}

/// 用户卡片评价只保留一个当前状态：再次点同按钮取消，点另一按钮切换。
pub fn set_user_feedback(id: &str, requested: i8, configs: &[ExperienceExpertConfig]) -> Result<Value, String> {
    let rates: HashMap<&str, f64> = configs.iter().map(|x| (x.id.as_str(), x.value_learning_rate.clamp(0.01, 1.0))).collect();
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let entry = guard.experiences.iter_mut().find(|entry| entry.id == id).ok_or("知识不存在")?;
    let old = entry.user_feedback;
    let next = if old == requested { 0 } else { requested };
    if old == 1 { entry.positive_count = entry.positive_count.saturating_sub(1); }
    if old == -1 { entry.negative_count = entry.negative_count.saturating_sub(1); }
    if next == 1 { entry.positive_count = entry.positive_count.saturating_add(1); }
    if next == -1 { entry.negative_count = entry.negative_count.saturating_add(1); }
    let rate = *rates.get(entry.expert_id.as_str()).unwrap_or(&0.2);
    entry.utility = (entry.utility + rate * (next - old) as f64).clamp(-1.0, 1.0);
    entry.confidence = (entry.confidence + rate * (next - old) as f64 * 0.25).clamp(0.0, 1.0);
    entry.user_feedback = next;
    entry.updated_at = now_ms();
    guard.save();
    Ok(json!({ "feedback": next }))
}

pub async fn evolve_memory(app: &AppHandle) -> Result<Value, String> {
    if TRAINING.swap(true, Ordering::SeqCst) { return Err("已有一次经验训练或演进正在进行".into()); }
    let result = async {
        let mut settings = {
            let state = app.state::<AppState>();
            let settings = state.settings.lock().unwrap().clone();
            settings
        };
        if settings.experience_experts.is_empty() { return Err("没有可演进的专家配置".into()); }
        // 旧配置可能尚未持久化训练模型；演进必须复用最近一次真实训练所用的后端与模型，而不是再次要求选择。
        if settings.experience_training_model.trim().is_empty() {
            let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            if let Some(session) = guard.training_sessions.iter().rev().find(|session| !session.model.trim().is_empty()) {
                settings.experience_training_agent = session.agent_kind.clone();
                settings.experience_training_model = session.model.clone();
            }
        }
        if settings.experience_training_model.trim().is_empty() { return Err("尚无可复用的经验训练模型，请先完成一次经验训练".into()); }

        let (mut evolved, original_ids) = {
            let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            let original_ids = guard.experiences.iter().map(|entry| entry.id.clone()).collect::<HashSet<_>>();
            (guard.clone(), original_ids)
        };
        let stats = evolve_generation(&mut evolved, &settings.experience_experts);
        let candidates = evolved.experiences.iter().filter(|entry| !original_ids.contains(&entry.id)).cloned().collect::<Vec<_>>();
        if candidates.is_empty() {
            let quarantined_ids = evolved.experiences.iter().filter(|entry| entry.status == "quarantined").map(|entry| entry.id.clone()).collect::<HashSet<_>>();
            let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            evolve(&mut guard, &settings.experience_experts);
            for entry in &mut guard.experiences {
                if quarantined_ids.contains(&entry.id) { entry.status = "quarantined".into(); }
            }
            guard.evolution_generation = evolved.evolution_generation;
            guard.last_evolution_at = now_ms();
            guard.save();
            return Ok(stats);
        }
        // 后代直接继承父代的文字，父代及同类高词汇重叠项已足以完成语义审核；限制条数避免经验库增长后 prompt 无界膨胀。
        let parent_ids = candidates.iter().flat_map(|entry| entry.parent_ids.iter().cloned()).collect::<HashSet<_>>();
        let mut existing_review = evolved.experiences.iter().filter(|entry| original_ids.contains(&entry.id) && parent_ids.contains(&entry.id))
            .map(|entry| json!({ "id": entry.id, "expertId": entry.expert_id, "kind": entry.kind, "trigger": entry.trigger, "action": entry.action, "avoid": entry.avoid }))
            .collect::<Vec<_>>();
        for entry in evolved.experiences.iter().filter(|entry| original_ids.contains(&entry.id) && entry.status != "quarantined") {
            if existing_review.len() >= 120 { break; }
            let entry_terms = terms(&entry.action);
            if candidates.iter().any(|candidate| candidate.kind == entry.kind && !entry_terms.is_disjoint(&terms(&candidate.action)))
                && !existing_review.iter().any(|row| row["id"] == entry.id) {
                existing_review.push(json!({ "id": entry.id, "expertId": entry.expert_id, "kind": entry.kind, "trigger": entry.trigger, "action": entry.action, "avoid": entry.avoid }));
            }
        }

        let prompt = format!(r#"你是经验库世代演进的终审员。候选只是遗传算法选出的亲本组合，不是可直接入库的最终文字；你必须先完成演进，再审核去重。
对每条候选：结合 parentIds 对应知识，提炼出比任一亲本更准确、可执行的 trigger/action/avoid；可以收紧边界、融合互补步骤、补充关键反例或抽象共同规律。仅复制、改写或换专家不算演进，必须丢弃。
最终结果还必须与现有知识及本批结果语义去重：核心条件和行动已被覆盖的不要输出。确有实质增量时输出完整的新经验，candidateId 必须取自本代候选；kind 只能是 experience、memory、rule。
返回纯 JSON，不要解释：{{"experiences":[{{"candidateId":"候选ID","kind":"experience","trigger":"演进后的适用条件","action":"演进后的行动结论","avoid":"应避免什么","scope":["领域"],"confidence":0.7}}]}}。

现有知识（含候选亲本）：
{}

本代候选：
{}"#,
            serde_json::to_string(&existing_review).unwrap_or_default(),
            serde_json::to_string(&candidates).unwrap_or_default());
        let agent = training_agent(&settings)?;
        let thread_id = {
            let state = app.state::<AppState>();
            let mut thread = Thread::new(
                state.config_dir.to_string_lossy().into_owned(), agent.clone(),
                Some(settings.experience_training_model.clone()), Some("build".into()), None, false,
            );
            thread.title = format!("世代演进审核 · 第 {} 代", evolved.evolution_generation);
            thread.experience_thread = true;
            let id = thread.id.clone();
            let mut thread_store = state.store.lock().unwrap();
            thread_store.threads.push(thread);
            thread_store.save();
            id
        };
        let _ = app.emit(crate::acp::EV_THREADS, json!({}));
        let answer = run_training_turn(app, &agent, &thread_id, prompt).await?;
        let review = extract_balanced_object::<EvolutionReview>(&answer).ok_or("演进审核模型未返回有效 JSON")?;
        let candidate_ids = candidates.iter().map(|entry| entry.id.clone()).collect::<HashSet<_>>();
        let mut accepted_entries = Vec::new();
        let mut accepted_candidate_ids = HashSet::new();
        for reviewed in review.experiences {
            if !candidate_ids.contains(&reviewed.candidate_id) || !accepted_candidate_ids.insert(reviewed.candidate_id.clone()) { continue; }
            let Some(mut entry) = candidates.iter().find(|entry| entry.id == reviewed.candidate_id).cloned() else { continue; };
            let candidate = reviewed.experience;
            if candidate.action.trim().is_empty() || (candidate.kind == "experience" && candidate.trigger.trim().is_empty()) { continue; }
            entry.kind = match candidate.kind.as_str() { "memory" | "rule" => candidate.kind, _ => "experience".into() };
            entry.trigger = candidate.trigger.trim().to_string();
            entry.action = candidate.action.trim().to_string();
            entry.avoid = candidate.avoid.trim().to_string();
            entry.scope = candidate.scope.into_iter().map(|scope| scope.trim().to_string()).filter(|scope| !scope.is_empty()).collect();
            entry.confidence = candidate.confidence.clamp(0.1, 0.95);
            let duplicate = evolved.experiences.iter().any(|old| original_ids.contains(&old.id) && old.status != "quarantined"
                && old.kind == entry.kind && terms(&old.action) == terms(&entry.action));
            if !duplicate { accepted_entries.push(entry); }
        }
        let created = accepted_entries.len();
        let rejected = candidate_ids.len().saturating_sub(created);

        let quarantined_ids = evolved.experiences.iter().filter(|entry| entry.status == "quarantined").map(|entry| entry.id.clone()).collect::<HashSet<_>>();
        let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
        evolve(&mut guard, &settings.experience_experts);
        for entry in &mut guard.experiences {
            if quarantined_ids.contains(&entry.id) { entry.status = "quarantined".into(); }
        }
        guard.evolution_generation = evolved.evolution_generation;
        guard.last_evolution_at = now_ms();
        guard.experiences.extend(accepted_entries);
        guard.save();
        let mut result = stats;
        result["created"] = json!(created);
        result["rejected"] = json!(rejected);
        result["reviewed"] = json!(candidate_ids.len());
        Ok(result)
    }.await;
    TRAINING.store(false, Ordering::SeqCst);
    result
}

fn deterministic_roll(key: &str) -> f64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut h);
    h.finish() as f64 / u64::MAX as f64
}

fn render_thread(thread: &crate::threads::Thread) -> String {
    let mut out = format!("会话 {}：{}\n", thread.id, thread.title);
    for item in thread.items.iter().rev().take(16).rev() {
        match item {
            Item::User { text, .. } if !text.trim().is_empty() => out.push_str(&format!("用户：{}\n", text.trim())),
            Item::Assistant { text, .. } if !text.trim().is_empty() => out.push_str(&format!("助手：{}\n", text.trim())),
            _ => {}
        }
    }
    out.chars().take(12_000).collect()
}

fn extract_balanced_object<T: serde::de::DeserializeOwned>(text: &str) -> Option<T> {
    // 不使用“第一个 { 到最后一个 }”：模型可能在 JSON 前后解释，甚至在解释中带花括号。
    // 逐个扫描完整、平衡的 JSON object，并正确跳过字符串内的括号和转义字符。
    let bytes = text.as_bytes();
    for start in text.match_indices('{').map(|(index, _)| index) {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for (offset, byte) in bytes[start..].iter().enumerate() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if *byte == b'\\' {
                    escaped = true;
                } else if *byte == b'"' {
                    in_string = false;
                }
                continue;
            }
            match *byte {
                b'"' => in_string = true,
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        let end = start + offset + 1;
                        if let Ok(output) = serde_json::from_str::<T>(&text[start..end]) {
                            return Some(output);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn extract_json(text: &str) -> Option<TrainingOutput> {
    extract_balanced_object(text)
}

fn repair_prompt(raw: &str) -> String {
    format!(r#"把下面内容修复为严格 JSON。只输出 JSON object，不要 Markdown 代码块、解释或注释。
结构必须是：
{{"experts":[{{"expertId":"专家ID","experiences":[{{"kind":"experience|memory|rule","trigger":"适用条件或上下文","action":"结论、事实或强约束","avoid":"应避免什么","scope":["领域"],"confidence":0.6}}]}}]}}
保留原内容的类型与表达；无法提取时返回 {{"experts":[]}}。

待修复内容：
{}"#, raw.chars().take(24_000).collect::<String>())
}

fn training_prompt(config: &ExperienceExpertConfig, existing: &str, conversations: &str) -> String {
    let max_candidates = ((config.write_rate.clamp(0.0, 1.0) * 3.0).ceil() as usize).clamp(1, 3);
    let role = match config.id.as_str() {
        "fast" => "天枢·即时行动派：只提炼高频、低歧义、能立刻执行并快速验证的经验，重速度与反馈闭环。",
        "concrete" => "天璇·细节工程派：保留技术栈、对象、操作顺序和验收信号，输出可照做的具体步骤。",
        "balanced" => "天玑·系统权衡派：专看收益、成本、风险和适用边界的平衡，提炼决策条件与取舍，不复制单纯操作步骤。",
        "abstract" => "天权·规律抽象派：跨项目寻找因果结构、稳定模式和可迁移原则，避免被单次实现细节束缚。",
        "negative" => "玉衡·反例审计派：优先研究失败、返工、误判和隐藏前提，输出防错条件、停止信号与规避动作。",
        "novel" => "开阳·探索创新派：主动寻找现有知识未覆盖的替代路径、组合方式和新假设；同义结论宁可不写。",
        "slow" => "摇光·长期守成派：只接受跨会话仍稳定、证据充分的知识，关注长期维护、兼容性和不应轻易改变的约束。",
        _ => "保持独立视角，只产出与其他专家有实质差异的知识。",
    };
    let abstraction = if config.abstraction_level >= 0.7 {
        "跨具体项目抽象规律，trigger 保留必要边界，action 避免绑定单个函数或文件"
    } else if config.abstraction_level <= 0.3 {
        "保留技术栈、对象和操作细节，只归纳可直接执行的具体经验"
    } else {
        "在可复用性和具体可执行性之间保持平衡"
    };
    let novelty = if config.novelty_preference >= 0.7 {
        "强烈偏好现有库未覆盖的新角度；相似结论宁可不写"
    } else if config.novelty_preference <= 0.3 {
        "允许对已有结论做更可靠、更精确的补充，但禁止同义重复"
    } else {
        "优先新结论，同时允许有证据的边界修正"
    };
    let negative = if config.negative_sensitivity >= 1.2 {
        "优先检查失败、纠正、返工和反例，并把规避动作写清楚"
    } else if config.negative_sensitivity <= 0.8 {
        "优先提炼被结果证实的成功做法，不因单个弱反例过度否定"
    } else {
        "均衡处理成功证据与失败反例"
    };
    let mutation = if config.mutation_rate >= 0.25 {
        "可对现有经验主动寻找更好的适用边界或替代动作"
    } else if config.mutation_rate <= 0.06 {
        "保持保守，除非新证据明确改变边界，否则不要改写已有经验"
    } else {
        "发现明确新证据时可修正已有经验的边界"
    };
    format!(r#"你是独立知识训练专家「{name}」（稳定标识：{id}），本次只代表该专家判断。
从会话中训练三种知识，kind 必须为以下之一：
- experience：从结果归纳的条件性经验，必须有 trigger 和 action，可反馈和淘汰。
- memory：有长期复用价值的客观事实或稳定背景；action 写事实，trigger 写适用项目或上下文。
- rule：用户明确要求、反复纠正或证据充分支持的强约束；action 写必须遵守的动作。普通建议不得升级为守则。
禁止把一次性进度、猜测或用户沉默写入任何类型。

你的固定认知角色：{role}
必须从该角色审视证据；若只能得到其他角色也会给出的通用结论，则输出 0 条，不为凑数写入。

该专家参数决定提炼策略：
- writeRate={write:.2}：三种类型合计最多输出 {max_candidates} 条，证据不足可为 0 条。
- abstraction={abstraction_value:.2}：{abstraction}。
- novelty={novelty_value:.2}：{novelty}。
- negativeSensitivity={negative_value:.2}：{negative}。
- mutationRate={mutation_value:.2}：{mutation}。
- valueLearningRate={learning:.2}、forgetRate={forget:.3}、migrationRate={migration:.2} 由系统执行。

现有知识（用于去重、修正和寻找新角度）：
{existing}

返回纯 JSON，不要解释：
{{"experts":[{{"expertId":"{id}","experiences":[{{"kind":"experience|memory|rule","trigger":"适用条件或上下文","action":"结论、事实或强约束","avoid":"应避免什么","scope":["领域"],"confidence":0.6}}]}}]}}

会话证据：
{conversations}"#,
        id = config.id,
        name = config.name,
        role = role,
        write = config.write_rate,
        abstraction_value = config.abstraction_level,
        novelty_value = config.novelty_preference,
        negative_value = config.negative_sensitivity,
        mutation_value = config.mutation_rate,
        learning = config.value_learning_rate,
        forget = config.forget_rate,
        migration = config.migration_rate,
    )
}

fn training_agent(settings: &Settings) -> Result<AgentKind, String> {
    serde_json::from_value(json!(settings.experience_training_agent.trim()))
        .map_err(|_| format!("不支持的训练后端：{}", settings.experience_training_agent))
}

/// 在猎户座 Thread 上运行一个真实 agent turn，而不是旁路 complete_once。
/// 这样会复用常规会话运行态、流式事件、取消机制和 ChatView，用户能看到实际训练过程。
async fn run_training_turn(app: &AppHandle, agent: &AgentKind, thread_id: &str, prompt: String) -> Result<String, String> {
    let before = {
        let state = app.state::<AppState>();
        let value = state.store.lock().unwrap().get(thread_id).map(|thread| thread.items.len()).ok_or("训练会话不存在")?;
        value
    };
    {
        let state = app.state::<AppState>();
        match agent {
            AgentKind::Alkaid => state.alkaid.clone().run_prompt(thread_id.into(), prompt, Vec::new()).await,
            AgentKind::Lyra => state.lyra.clone().run_prompt(thread_id.into(), prompt, Vec::new()).await,
            AgentKind::Codex | AgentKind::CodexPlus => state.codexplus.clone().run_prompt(thread_id.into(), prompt, Vec::new()).await,
            AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => state.codebuddyplus.clone().run_prompt(thread_id.into(), prompt, Vec::new()).await,
            AgentKind::ClaudeCode => state.claudeplus.clone().run_prompt(thread_id.into(), prompt, Vec::new()).await,
            AgentKind::Cursor => state.cursorplus.clone().run_prompt(thread_id.into(), prompt, Vec::new()).await,
            AgentKind::Devin | AgentKind::OpenCode | AgentKind::OpenCodePlus =>
                return Err(format!("{} 暂不支持经验训练，请选择 Vega、Lyra、Codex、CodeBuddy、Claude 或 Cursor", agent.label())),
        }
    }
    let state = app.state::<AppState>();
    let thread_store = state.store.lock().unwrap();
    let thread = thread_store.get(thread_id).ok_or("训练会话不存在")?;
    if let Some(text) = thread.items[before..].iter().rev().find_map(|item| match item {
        Item::Assistant { text, .. } if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    }) {
        return Ok(text);
    }
    let error = thread.items[before..].iter().rev().find_map(|item| match item {
        Item::System { text, .. } if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    }).unwrap_or_else(|| "训练会话结束，但模型没有返回内容".into());
    Err(error)
}

pub async fn train(app: &AppHandle, force: bool) -> Result<Value, String> {
    if TRAINING.swap(true, Ordering::SeqCst) { return Err("已有一次经验训练正在进行".into()); }
    let result = async {
        let (settings, active_configs, threads) = {
            let state = app.state::<AppState>();
            let settings = state.settings.lock().unwrap().clone();
            if settings.experience_training_model.trim().is_empty() { return Err("请先选择经验训练模型".into()); }
            let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            if !force && !settings.experience_training_enabled { return Err("经验训练未启用".into()); }
            let last_schedule_at = guard.last_train_at.max(guard.last_attempt_at);
            if !force && now_ms() - last_schedule_at < settings.experience_training_interval_minutes.max(5) as i64 * 60_000 {
                return Ok(json!({ "trained": false, "reason": "notDue" }));
            }
            let seed = now_ms() / 1_000 + guard.training_cycles as i64;
            let mut configs = settings.experience_experts.clone();
            configs.sort_by(|a, b| {
                guard.last_trained_experts.contains(&a.id).cmp(&guard.last_trained_experts.contains(&b.id))
                    .then_with(|| deterministic_roll(&format!("activate:{seed}:{}", a.id)).total_cmp(&deterministic_roll(&format!("activate:{seed}:{}", b.id))))
            });
            configs.truncate(configs.len().min(2));
            let mut threads = state.store.lock().unwrap().threads.clone();
            threads.retain(|thread| !thread.mind_thread && !thread.experience_thread
                && thread.items.iter().any(|item| matches!(item, Item::User { .. }))
                && configs.iter().any(|config| guard.expert_processed_threads.get(&config.id)
                    .and_then(|seen| seen.get(&thread.id)).copied().unwrap_or(0) < thread.updated_at));
            threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
            (settings, configs, threads.into_iter().take(MAX_TRAIN_THREADS).collect::<Vec<_>>())
        };
        if threads.is_empty() { return Ok(json!({ "trained": false, "reason": "noNewSessions" })); }
        {
            let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            guard.last_attempt_at = now_ms();
            guard.save();
        }
        let agent = training_agent(&settings)?;
        let conversations = threads.iter().map(render_thread).collect::<Vec<_>>().join("\n---\n");
        let source_thread_ids = threads.iter().map(|thread| thread.id.clone()).collect::<Vec<_>>();
        let mut combined = TrainingOutput::default();
        let mut session_ids = Vec::new();
        let mut failures = Vec::new();

        // 先一次性创建本轮全部 Stage，再并行执行；首个专家会话作为事件根，不创建空 Stage。
        let mut event_id = String::new();
        let mut jobs = Vec::new();
        for (expert_index, config) in active_configs.iter().enumerate() {
            let existing = {
                let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
                let rows = guard.experiences.iter()
                    .filter(|entry| entry.expert_id == config.id && entry.status != "quarantined")
                    .take(40)
                    .map(|entry| format!("- [{}] 当 {} => {}（避免：{}；用户评价={}；正反馈={}；负反馈={}；效用={:.2}）", entry.kind, entry.trigger, entry.action, entry.avoid, entry.user_feedback, entry.positive_count, entry.negative_count, entry.utility))
                    .collect::<Vec<_>>();
                if rows.is_empty() { "（该专家暂无知识）".into() } else { rows.join("\n") }
            };
            let prompt = training_prompt(config, &existing, &conversations);
            let session_id = {
                let state = app.state::<AppState>();
                let mut thread = Thread::new(
                    state.config_dir.to_string_lossy().into_owned(), agent.clone(),
                    Some(settings.experience_training_model.clone()), Some("build".into()), None, false,
                );
                thread.title = if expert_index == 0 {
                    format!("经验训练 · {} 个来源会话 · {}", threads.len(), config.id)
                } else {
                    format!("[Stage] {}", config.id)
                };
                thread.experience_thread = true;
                if expert_index > 0 {
                    thread.parent_thread_id = Some(event_id.clone());
                    thread.stage_source_thread_id = Some(event_id.clone());
                }
                let id = thread.id.clone();
                let mut thread_store = state.store.lock().unwrap();
                thread_store.threads.push(thread);
                thread_store.save();
                id
            };
            if expert_index == 0 { event_id = session_id.clone(); }
            session_ids.push(session_id.clone());
            jobs.push((config.id.clone(), session_id, prompt));
        }
        let _ = app.emit(crate::acp::EV_THREADS, json!({}));

        let results = join_all(jobs.into_iter().map(|(expert_id, session_id, prompt)| {
            let app = app.clone();
            let agent = agent.clone();
            async move {
                let answer = run_training_turn(&app, &agent, &session_id, prompt).await;
                let parsed = match answer {
                    Ok(text) => {
                        if let Some(parsed) = extract_json(&text) {
                            Ok((parsed, text))
                        } else {
                            match run_training_turn(&app, &agent, &session_id, repair_prompt(&text)).await {
                                Ok(repaired) => extract_json(&repaired)
                                    .map(|parsed| (parsed, repaired))
                                    .ok_or_else(|| format!("{} 未返回有效 JSON", expert_id)),
                                Err(error) => Err(format!("{} 自动修复失败：{}", expert_id, error)),
                            }
                        }
                    }
                    Err(error) => Err(format!("{}：{}", expert_id, error)),
                };
                (expert_id, session_id, parsed)
            }
        })).await;

        for (expert_id, session_id, parsed_result) in results {
            let (status, output, error, experiences) = match parsed_result {
                Ok((parsed, output)) => {
                    let experiences = parsed.experts.into_iter().flat_map(|item| item.experiences).collect::<Vec<_>>();
                    let status = if experiences.is_empty() { "no_experience" } else { "completed" };
                    (status, output, String::new(), Some(experiences))
                }
                Err(error) => {
                    failures.push(error.clone());
                    ("failed", String::new(), error, None)
                }
            };
            if let Some(experiences) = experiences {
                combined.experts.push(ExpertOutput { expert_id: expert_id.clone(), experiences });
            }
            let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            guard.training_sessions.push(ExperienceTrainingSession {
                id: session_id, created_at: now_ms(), agent_kind: settings.experience_training_agent.clone(),
                model: settings.experience_training_model.clone(), expert_id,
                source_thread_ids: source_thread_ids.clone(), conversation: conversations.clone(),
                output, status: status.into(), error,
            });
            if guard.training_sessions.len() > 100 {
                let excess = guard.training_sessions.len() - 100;
                guard.training_sessions.drain(..excess);
            }
            guard.save();
        }

        if combined.experts.is_empty() {
            return Err(failures.join("；"));
        }
        let activated_experts = active_configs.iter().map(|config| config.id.clone()).collect::<Vec<_>>();
        let learned = apply_training_output(&settings, &threads, combined);
        if let Ok(mut guard) = store().and_then(|value| value.lock().map_err(|_| "lock".into())) {
            guard.last_trained_experts = activated_experts.clone();
            guard.save();
        }
        Ok(json!({
            "trained": true,
            "learned": learned,
            "sessionId": event_id,
            "sessionIds": session_ids,
            "activatedExperts": activated_experts,
            "failedExperts": failures
        }))
    }.await;
    TRAINING.store(false, Ordering::SeqCst);
    result
}

pub fn tick(app: &AppHandle) {
    consume_feedback_inbox(app);
    if TRAINING.load(Ordering::SeqCst) { return; }
    // 检查是否到达世代演进间隔；到点则自动演进一次（与训练互斥，TRAINING 锁保护）。
    let evolution_due = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap().clone();
        if !settings.experience_training_enabled { false } else {
            let interval = settings.experience_evolution_interval_minutes.max(10) as i64 * 60_000;
            let guard = store().and_then(|s| s.lock().map_err(|_| "lock".into()));
            match guard {
                Ok(guard) => now_ms() - guard.last_evolution_at >= interval,
                Err(_) => false,
            }
        }
    };
    if evolution_due {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = evolve_memory(&app).await;
        });
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move { let _ = train(&app, false).await; });
}

fn consume_feedback_inbox(app: &AppHandle) {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct InboxFeedback {
        #[serde(default)]
        experience_ids: Vec<String>,
        #[serde(default)]
        reward: f64,
        #[serde(default)]
        note: String,
    }
    let (dir, configs) = {
        let state = app.state::<AppState>();
        let dir = state.config_dir.join("experience-feedback-inbox");
        let configs = state.settings.lock().unwrap().experience_experts.clone();
        (dir, configs)
    };
    let Ok(entries) = fs::read_dir(&dir) else { return; };
    for entry in entries.flatten().take(64) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") { continue; }
        let parsed = fs::read_to_string(&path).ok()
            .and_then(|raw| serde_json::from_str::<InboxFeedback>(&raw).ok());
        if let Some(feedback) = parsed {
            if feedback_memory(&feedback.experience_ids, feedback.reward, &feedback.note, &configs).is_ok() {
                let _ = fs::remove_file(path);
            }
        } else {
            let _ = fs::rename(&path, path.with_extension("invalid"));
        }
    }
}

fn apply_training_output(settings: &Settings, threads: &[crate::threads::Thread], output: TrainingOutput) -> usize {
    let Ok(mut guard) = store().and_then(|s| s.lock().map_err(|_| "lock".into())) else { return 0; };
    let now = now_ms();
    let source_ids: Vec<String> = threads.iter().map(|t| t.id.clone()).collect();
    let configs: HashMap<&str, &ExperienceExpertConfig> = settings.experience_experts.iter().map(|x| (x.id.as_str(), x)).collect();
    let mut learned = 0;
    let mut activated_experts = Vec::new();
    for expert in output.experts {
        activated_experts.push(expert.expert_id.clone());
        let Some(config) = configs.get(expert.expert_id.as_str()) else { continue; };
        let max_candidates = ((config.write_rate.clamp(0.0, 1.0) * 3.0).ceil() as usize).clamp(1, 3);
        for candidate in expert.experiences.into_iter().take(max_candidates) {
            if candidate.action.trim().is_empty() || (candidate.kind == "experience" && candidate.trigger.trim().is_empty()) { continue; }
            let duplicate = guard.experiences.iter().any(|e| e.expert_id == expert.expert_id && e.kind == candidate.kind && e.status != "quarantined" && terms(&e.action) == terms(&candidate.action));
            if duplicate { continue; }
            let generation = guard.training_cycles as u32;
            let kind = match candidate.kind.as_str() {
                "memory" | "rule" => candidate.kind,
                _ => "experience".into(),
            };
            guard.experiences.push(ExperienceEntry {
                id: uuid::Uuid::new_v4().to_string(), expert_id: expert.expert_id.clone(), kind,
                trigger: candidate.trigger, action: candidate.action, avoid: candidate.avoid,
                scope: candidate.scope, source_thread_ids: source_ids.clone(), parent_ids: Vec::new(),
                generation, confidence: candidate.confidence.clamp(0.1, 0.9),
                utility: 0.0, positive_count: 0, negative_count: 0, user_feedback: 0, hit_count: 0,
                created_at: now, updated_at: now, last_used_at: 0, status: "candidate".into(),
            });
            learned += 1;
        }
    }
    for expert_id in activated_experts {
        let seen = guard.expert_processed_threads.entry(expert_id).or_default();
        for thread in threads { seen.insert(thread.id.clone(), thread.updated_at); }
    }
    guard.last_train_at = now;
    guard.training_cycles = guard.training_cycles.saturating_add(1);
    evolve(&mut guard, &settings.experience_experts);
    guard.save();
    learned
}

fn fitness(entry: &ExperienceEntry) -> f64 {
    entry.utility * 2.0 + entry.confidence
        + (entry.positive_count as f64 - entry.negative_count as f64 * 1.25) * 0.15
        + (entry.hit_count as f64 + 1.0).ln() * 0.08
}

/// 日常维护：遗忘率按未使用天数衰减效用，并限制各岛容量。
fn evolve(store: &mut ExperienceStore, configs: &[ExperienceExpertConfig]) {
    let now = now_ms();
    for config in configs {
        for entry in store.experiences.iter_mut().filter(|e| e.expert_id == config.id && e.status != "quarantined") {
            let age_days = (now - entry.last_used_at.max(entry.created_at)).max(0) as f64 / 86_400_000.0;
            entry.utility *= (-config.forget_rate.clamp(0.0, 1.0) * age_days).exp();
        }
        let mut indices: Vec<usize> = store.experiences.iter().enumerate().filter(|(_, e)| e.expert_id == config.id && e.status != "quarantined").map(|(i, _)| i).collect();
        indices.sort_by(|&a, &b| fitness(&store.experiences[b]).total_cmp(&fitness(&store.experiences[a])));
        for &index in indices.iter().skip(MAX_EXPERIENCES_PER_EXPERT) { store.experiences[index].status = "quarantined".into(); }
    }
}

/// 完整群岛遗传世代：岛内选择、双亲继承、参数化变异、环形迁移。
fn evolve_generation(store: &mut ExperienceStore, configs: &[ExperienceExpertConfig]) -> Value {
    evolve(store, configs);
    store.evolution_generation = store.evolution_generation.saturating_add(1);
    let generation = store.evolution_generation;
    let now = now_ms();
    let mut offspring = Vec::new();
    let (mut crossed, mut mutated, mut migrated, mut quarantined) = (0usize, 0usize, 0usize, 0usize);
    for config in configs {
        let mut island = store.experiences.iter().filter(|e| e.expert_id == config.id && e.status != "quarantined").cloned().collect::<Vec<_>>();
        island.sort_by(|a, b| fitness(b).total_cmp(&fitness(a)));
        if island.len() >= 8 {
            let survivors = (island.len() * 4 / 5).max(2);
            let discarded = island.iter().skip(survivors).map(|e| e.id.clone()).collect::<HashSet<_>>();
            for entry in store.experiences.iter_mut().filter(|e| discarded.contains(&e.id)) { entry.status = "quarantined".into(); quarantined += 1; }
            island.truncate(survivors);
        }
        if island.len() < 2 || deterministic_roll(&format!("cross:{}:{generation}", config.id)) > config.write_rate.clamp(0.0, 1.0) { continue; }
        let (parent_a, parent_b) = (&island[0], &island[1]);
        let mut child = parent_a.clone();
        child.id = uuid::Uuid::new_v4().to_string();
        child.parent_ids = vec![parent_a.id.clone(), parent_b.id.clone()];
        for scope in &parent_b.scope { if !child.scope.contains(scope) { child.scope.push(scope.clone()); } }
        child.generation = parent_a.generation.max(parent_b.generation).saturating_add(1);
        child.confidence = ((parent_a.confidence + parent_b.confidence) / 2.0).clamp(0.1, 0.95);
        child.utility = ((parent_a.utility + parent_b.utility) * 0.25).clamp(-1.0, 1.0);
        child.positive_count = 0; child.negative_count = 0; child.hit_count = 0;
        child.created_at = now; child.updated_at = now; child.last_used_at = 0; child.status = "candidate".into();
        if deterministic_roll(&format!("mutation:{}:{generation}", config.id)) <= config.mutation_rate.clamp(0.0, 1.0) {
            let delta = (deterministic_roll(&format!("mutation-delta:{}:{generation}", config.id)) - 0.5) * 0.3;
            child.confidence = (child.confidence + delta).clamp(0.1, 0.95);
            child.utility = (child.utility + delta).clamp(-1.0, 1.0);
            mutated += 1;
        }
        offspring.push(child); crossed += 1;
    }
    if configs.len() > 1 {
        for (i, source) in configs.iter().enumerate() {
            let target = &configs[(i + 1) % configs.len()];
            if deterministic_roll(&format!("migration:{}:{}:{generation}", source.id, target.id)) > target.migration_rate.clamp(0.0, 1.0) { continue; }
            if let Some(parent) = store.experiences.iter().filter(|e| e.expert_id == source.id && e.status != "quarantined").max_by(|a, b| fitness(a).total_cmp(&fitness(b))).cloned() {
                if store.experiences.iter().any(|e| e.expert_id == target.id && e.kind == parent.kind && terms(&e.action) == terms(&parent.action)) { continue; }
                let mut child = parent.clone();
                child.id = uuid::Uuid::new_v4().to_string(); child.expert_id = target.id.clone();
                child.parent_ids = vec![parent.id]; child.generation = child.generation.saturating_add(1);
                child.confidence = (child.confidence * 0.8).max(0.2); child.utility = 0.0;
                child.positive_count = 0; child.negative_count = 0; child.hit_count = 0;
                child.created_at = now; child.updated_at = now; child.last_used_at = 0; child.status = "candidate".into();
                offspring.push(child); migrated += 1;
            }
        }
    }
    let created = offspring.len();
    store.experiences.extend(offspring);
    json!({ "generation": generation, "created": created, "crossed": crossed, "mutated": mutated, "migrated": migrated, "quarantined": quarantined })
}
