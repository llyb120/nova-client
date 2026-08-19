use crate::settings::{ExperienceExpertConfig, Settings};
use crate::threads::{now_ms, AgentKind, Item, Thread};
use crate::{AppState, SCRATCH_MARK};
use futures_util::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
static PROJECT_IDENTITIES: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();
static TRAINING: AtomicBool = AtomicBool::new(false);

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExperienceEntry {
    pub id: String,
    pub expert_id: String,
    /// 产生该知识的项目标识。Git 项目按主仓库根归一，因此子目录和 worktree 共享同一标识。
    #[serde(default)]
    pub project_id: String,
    /// 产生该知识的 Git 仓库根路径，用于展示与审计。
    #[serde(default)]
    pub project_root: String,
    /// universal = 可跨项目使用；project = 仅当前项目使用。旧数据默认 project，避免意外外泄。
    #[serde(default = "default_knowledge_scope")]
    pub knowledge_scope: String,
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

fn default_confidence() -> f64 {
    0.4
}
fn default_entry_kind() -> String {
    "experience".into()
}
fn default_knowledge_scope() -> String {
    "project".into()
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct ExperienceStore {
    #[serde(skip)]
    path: PathBuf,
    /// 真正可跨项目复用的知识；项目独有知识仍只存于 projects。
    #[serde(default)]
    universal_experiences: Vec<ExperienceEntry>,
    #[serde(default)]
    projects: HashMap<String, ProjectExperienceStore>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct ProjectExperienceStore {
    #[serde(default)]
    project_root: String,
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

impl ExperienceStore {
    fn project(&self, key: &str) -> ProjectExperienceStore {
        self.projects.get(key).cloned().unwrap_or_default()
    }

    fn project_mut(&mut self, key: &str, root: &str) -> &mut ProjectExperienceStore {
        let project = self.projects.entry(key.to_string()).or_default();
        if project.project_root.is_empty() {
            project.project_root = root.to_string();
        }
        project
    }
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
    #[serde(default = "default_knowledge_scope")]
    knowledge_scope: String,
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
        let raw = fs::read_to_string(&path).ok();
        let mut store = raw
            .as_deref()
            .and_then(|value| serde_json::from_str::<Self>(value).ok())
            .unwrap_or_default();
        // 旧版是全局混合库，无法可靠判断每条知识属于哪个项目；解析不到新结构时
        // 保持 projects 为空，防止升级后继续跨项目召回。新训练会按项目重新沉淀。
        // 项目属性属于每一条知识，而不是 UI 筛选状态。旧的项目分区数据在加载时补齐属性；
        // key 已由 Git 主仓库根归一，同一仓库的子目录与 worktree 因而得到相同 project_id。
        for (project_id, project) in &mut store.projects {
            for entry in &mut project.experiences {
                if entry.project_id.is_empty() {
                    entry.project_id = project_id.clone();
                }
                if entry.project_root.is_empty() {
                    entry.project_root = project.project_root.clone();
                }
            }
        }
        store.path = path;
        store
    }

    fn save(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
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

fn project_identity(cwd: &str) -> Result<(String, String), String> {
    let cwd = cwd.trim();
    if cwd.is_empty() || cwd.contains(SCRATCH_MARK) {
        return Err("请选择一个项目后再使用大熊座".into());
    }
    let path = PathBuf::from(cwd);
    if !path.is_dir() {
        return Err("项目目录不存在".into());
    }
    let cache = PROJECT_IDENTITIES.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(identity) = cache
        .lock()
        .map_err(|_| "项目身份缓存已损坏".to_string())?
        .get(cwd)
        .cloned()
    {
        return Ok(identity);
    }
    let root = crate::gitwt::project_root(cwd).unwrap_or_else(|_| {
        std::fs::canonicalize(&path)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    });
    let mut normalized = root.replace('\\', "/");
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    #[cfg(windows)]
    {
        normalized = normalized.to_lowercase();
    }
    let identity = (normalized, root);
    cache
        .lock()
        .map_err(|_| "项目身份缓存已损坏".to_string())?
        .insert(cwd.to_string(), identity.clone());
    Ok(identity)
}

fn project_snapshot(cwd: &str) -> Result<(String, String, ProjectExperienceStore), String> {
    let (key, root) = project_identity(cwd)?;
    let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    Ok((key.clone(), root, guard.project(&key)))
}

fn is_training_source_thread(thread: &Thread) -> bool {
    // 训练与世代演进自身产生的会话都标记为 experience_thread。
    // 这些会话只用于审计训练过程，禁止再次作为训练语料，避免自我回灌。
    !thread.experience_thread
}

fn project_threads(state: &AppState, project_key: &str) -> Vec<Thread> {
    let threads = state.store.lock().unwrap().threads.clone();
    threads
        .into_iter()
        .filter(is_training_source_thread)
        .filter(|thread| project_identity(&thread.cwd).is_ok_and(|(key, _)| key == project_key))
        .collect()
}

fn terms(text: &str) -> HashSet<String> {
    let lower = text.to_lowercase();
    let mut out: HashSet<String> = lower
        .split(|c: char| !c.is_alphanumeric() && !('\u{4e00}'..='\u{9fff}').contains(&c))
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect();
    // 中文通常没有空格；加入 CJK 双字片段，避免整句必须完全相同才命中。
    let cjk: Vec<char> = lower
        .chars()
        .filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c))
        .collect();
    for pair in cjk.windows(2) {
        out.insert(pair.iter().collect());
    }
    out
}

fn relevance(query: &HashSet<String>, entry: &ExperienceEntry) -> f64 {
    if query.is_empty() {
        return 0.0;
    }
    let hay = terms(&format!(
        "{} {} {} {}",
        entry.trigger,
        entry.action,
        entry.avoid,
        entry.scope.join(" ")
    ));
    query.intersection(&hay).count() as f64 / query.len().max(1) as f64
}

/// 词面相似度（Jaccard），用于捕获演进产生的近似重复条目。
/// 中英文都走 terms() 的双字片段，阈值 0.65 视为语义重复。
fn text_similarity(a: &str, b: &str) -> f64 {
    if a.trim().is_empty() || b.trim().is_empty() {
        return 0.0;
    }
    let ta = terms(a);
    let tb = terms(b);
    let intersection = ta.intersection(&tb).count();
    let union = ta.union(&tb).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

/// 两条经验是否语义重复（kind + action 近似相同）。
fn entries_duplicate(a: &ExperienceEntry, b: &ExperienceEntry) -> bool {
    if a.id == b.id || a.kind != b.kind || a.knowledge_scope != b.knowledge_scope {
        return false;
    }
    text_similarity(&a.action, &b.action) >= 0.65
        && (a.trigger.is_empty()
            || b.trigger.is_empty()
            || text_similarity(&a.trigger, &b.trigger) >= 0.45)
}

pub fn load_trained_memory(cwd: &str, query: &str, limit: usize) -> Result<Value, String> {
    let (project_key, project_root) = project_identity(cwd)?;
    let (mut universal, mut project) = {
        let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
        let universal: Vec<ExperienceEntry> = guard
            .universal_experiences
            .iter()
            .filter(|entry| entry.knowledge_scope == "universal")
            .cloned()
            .collect();
        let mut project = guard.project(&project_key);
        project
            .experiences
            .retain(|entry| entry.knowledge_scope != "universal");
        (universal, project)
    };
    let universal_len = universal.len();
    let mut entries = Vec::with_capacity(universal_len + project.experiences.len());
    entries.append(&mut universal);
    entries.append(&mut project.experiences);
    let query_terms = terms(query);
    let mut by_expert: HashMap<String, Vec<(usize, f64)>> = HashMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.status == "quarantined" {
            continue;
        }
        let rel = relevance(&query_terms, entry);
        if rel <= 0.0 {
            continue;
        }
        let score = 0.55 * rel + 0.25 * entry.utility + 0.20 * entry.confidence;
        by_expert
            .entry(entry.expert_id.clone())
            .or_default()
            .push((index, score));
    }
    for entries in by_expert.values_mut() {
        entries.sort_by(|a, b| b.1.total_cmp(&a.1));
    }
    let nonce = now_ms();
    let mut experts: Vec<(String, f64)> = by_expert
        .iter()
        .map(|(id, rows)| {
            let preview =
                rows.iter().take(3).map(|x| x.1).sum::<f64>() / rows.len().min(3).max(1) as f64;
            let activations = *project.expert_activations.get(id).unwrap_or(&0) as f64;
            // 相关性为主，叠加有界随机扰动与低激活奖励；无关岛不会进入候选。
            let jitter = deterministic_roll(&format!("recall:{nonce}:{id}")) * 0.16;
            (
                id.clone(),
                preview + 0.12 / (1.0 + activations).sqrt() + jitter,
            )
        })
        .collect();
    experts.sort_by(|a, b| b.1.total_cmp(&a.1));
    let selected: Vec<String> = experts.into_iter().take(3).map(|x| x.0).collect();
    let mut candidates = Vec::new();
    for id in &selected {
        if let Some(rows) = by_expert.get(id) {
            for &(index, score) in rows.iter().take(4) {
                candidates.push((index, score));
            }
        }
    }
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut chosen = Vec::new();
    let mut per_expert: HashMap<String, usize> = HashMap::new();
    for (index, _) in candidates {
        if chosen.len() >= limit.clamp(1, 12) {
            break;
        }
        let id = entries[index].expert_id.clone();
        if *per_expert.get(&id).unwrap_or(&0) >= 3 {
            continue;
        }
        if chosen.iter().any(|&old: &usize| {
            let a = terms(&entries[old].action);
            let b = terms(&entries[index].action);
            !a.is_empty()
                && a.intersection(&b).count() as f64 / a.len().min(b.len()).max(1) as f64 > 0.8
        }) {
            continue;
        }
        *per_expert.entry(id).or_default() += 1;
        chosen.push(index);
    }
    let now = now_ms();
    for id in &selected {
        *project.expert_activations.entry(id.clone()).or_default() += 1;
    }
    let result: Vec<Value> = chosen.into_iter().map(|index| {
        let entry = &mut entries[index];
        entry.hit_count = entry.hit_count.saturating_add(1);
        entry.last_used_at = now;
        json!({
            "id": entry.id, "expertId": entry.expert_id, "kind": entry.kind,
            "projectId": entry.project_id, "projectRoot": entry.project_root,
            "knowledgeScope": entry.knowledge_scope,
            "trigger": entry.trigger, "action": entry.action, "avoid": entry.avoid, "scope": entry.scope,
            "confidence": entry.confidence, "utility": entry.utility
        })
    }).collect();
    let project_entries = entries.split_off(universal_len);
    let universal_entries = entries;
    project.experiences = project_entries;
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    guard.universal_experiences = universal_entries;
    guard.projects.insert(project_key, project);
    // 命中次数与专家激活是召回遥测，不应让每次 polaris 都同步重写完整经验库。
    // 保留内存态，后续训练、反馈或删除等真实写操作会一并持久化。
    Ok(
        json!({ "activatedExperts": selected, "experiences": result, "projectRoot": project_root,
        "instruction": "knowledgeScope=universal 的知识可跨项目使用；knowledgeScope=project 的知识只适用于当前 projectRoot。memory 是可参考事实，rule 是强约束，experience 仅在触发条件匹配时参考；当前会话事实始终优先。" }),
    )
}

pub fn list_memory(cwd: &str, configs: &[ExperienceExpertConfig]) -> Result<Value, String> {
    let (_, project_root, project) = project_snapshot(cwd)?;
    let universal = store()?
        .lock()
        .map_err(|_| "经验库锁已损坏".to_string())?
        .universal_experiences
        .iter()
        .filter(|entry| entry.knowledge_scope == "universal")
        .cloned()
        .collect::<Vec<_>>();
    let experts: Vec<Value> = configs
        .iter()
        .map(|c| json!({ "id": c.id, "name": c.name }))
        .collect();
    let mut experiences = universal;
    experiences.extend(project.experiences);
    Ok(json!({
        "projectRoot": project_root,
        "experiences": experiences,
        "experts": experts,
        "lastTrainAt": project.last_train_at,
        "trainingCycles": project.training_cycles,
        "evolutionGeneration": project.evolution_generation,
        "training": TRAINING.load(Ordering::SeqCst)
    }))
}

pub fn delete_memory(cwd: &str, id: &str) -> Result<Value, String> {
    let (project_key, project_root) = project_identity(cwd)?;
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let universal_before = guard.universal_experiences.len();
    guard.universal_experiences.retain(|entry| entry.id != id);
    let universal_deleted = universal_before - guard.universal_experiences.len();
    let project = guard.project_mut(&project_key, &project_root);
    let project_before = project.experiences.len();
    project.experiences.retain(|entry| entry.id != id);
    let project_deleted = project_before - project.experiences.len();
    let deleted = universal_deleted + project_deleted;
    if deleted > 0 {
        guard.save();
    }
    Ok(json!({ "deleted": deleted }))
}

pub fn feedback_memory(
    cwd: &str,
    ids: &[String],
    reward: f64,
    note: &str,
    configs: &[ExperienceExpertConfig],
) -> Result<Value, String> {
    let reward = reward.clamp(-1.0, 1.0);
    // load 为空或本轮未采用任何条目时，允许 reward=0 的空反馈完成闭环。
    if ids.is_empty() {
        return if reward == 0.0 {
            Ok(json!({ "updated": 0, "reward": 0, "settled": true }))
        } else {
            Err("feedback_memory 非零反馈必须提供 experienceIds".into())
        };
    }
    let (project_key, project_root) = project_identity(cwd)?;
    let rates: HashMap<&str, f64> = configs
        .iter()
        .map(|x| (x.id.as_str(), x.value_learning_rate.clamp(0.01, 1.0)))
        .collect();
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let mut updated = 0;
    for entry in guard
        .universal_experiences
        .iter_mut()
        .filter(|x| ids.contains(&x.id))
    {
        apply_feedback(entry, reward, note, &rates);
        updated += 1;
    }
    let project = guard.project_mut(&project_key, &project_root);
    for entry in project
        .experiences
        .iter_mut()
        .filter(|x| ids.contains(&x.id))
    {
        apply_feedback(entry, reward, note, &rates);
        updated += 1;
    }
    guard.save();
    Ok(json!({ "updated": updated, "reward": reward }))
}

fn apply_feedback(
    entry: &mut ExperienceEntry,
    reward: f64,
    note: &str,
    rates: &HashMap<&str, f64>,
) {
    let rate = *rates.get(entry.expert_id.as_str()).unwrap_or(&0.2);
    entry.utility = ((1.0 - rate) * entry.utility + rate * reward).clamp(-1.0, 1.0);
    if reward > 0.0 {
        entry.positive_count = entry.positive_count.saturating_add(1);
    }
    if reward < 0.0 {
        entry.negative_count = entry.negative_count.saturating_add(1);
    }
    entry.confidence = (entry.confidence + rate * reward * 0.25).clamp(0.0, 1.0);
    if entry.negative_count >= 3 && entry.negative_count > entry.positive_count.saturating_mul(2) {
        entry.status = "quarantined".into();
    }
    entry.updated_at = now_ms();
    if !note.trim().is_empty() && reward < 0.0 {
        entry.avoid = format!("{}；反馈：{}", entry.avoid.trim(), note.trim())
            .trim_matches('；')
            .to_string();
    }
}

/// 用户卡片评价只保留一个当前状态：再次点同按钮取消，点另一按钮切换。
pub fn set_user_feedback(
    cwd: &str,
    id: &str,
    requested: i8,
    configs: &[ExperienceExpertConfig],
) -> Result<Value, String> {
    let (project_key, project_root) = project_identity(cwd)?;
    let rates: HashMap<&str, f64> = configs
        .iter()
        .map(|x| (x.id.as_str(), x.value_learning_rate.clamp(0.01, 1.0)))
        .collect();
    let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
    let entry = if let Some(entry) = guard
        .universal_experiences
        .iter_mut()
        .find(|entry| entry.id == id)
    {
        entry
    } else {
        guard
            .project_mut(&project_key, &project_root)
            .experiences
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or("知识不存在")?
    };
    let old = entry.user_feedback;
    let next = if old == requested { 0 } else { requested };
    if old == 1 {
        entry.positive_count = entry.positive_count.saturating_sub(1);
    }
    if old == -1 {
        entry.negative_count = entry.negative_count.saturating_sub(1);
    }
    if next == 1 {
        entry.positive_count = entry.positive_count.saturating_add(1);
    }
    if next == -1 {
        entry.negative_count = entry.negative_count.saturating_add(1);
    }
    let rate = *rates.get(entry.expert_id.as_str()).unwrap_or(&0.2);
    entry.utility = (entry.utility + rate * (next - old) as f64).clamp(-1.0, 1.0);
    entry.confidence = (entry.confidence + rate * (next - old) as f64 * 0.25).clamp(0.0, 1.0);
    entry.user_feedback = next;
    entry.updated_at = now_ms();
    guard.save();
    Ok(json!({ "feedback": next }))
}

pub async fn evolve_memory(app: &AppHandle, cwd: &str) -> Result<Value, String> {
    let (project_key, project_root) = project_identity(cwd)?;
    if TRAINING.swap(true, Ordering::SeqCst) {
        return Err("已有一次经验训练或演进正在进行".into());
    }
    let result = async {
        let mut settings = {
            let state = app.state::<AppState>();
            let x = state.settings.lock().unwrap().clone();
            x
        };
        if settings.experience_experts.is_empty() { return Err("没有可演进的专家配置".into()); }
        // 旧配置可能尚未持久化训练模型；演进必须复用最近一次真实训练所用的后端与模型，而不是再次要求选择。
        if settings.experience_training_model.trim().is_empty() {
            let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            let project = guard.project(&project_key);
            if let Some(session) = project
                .training_sessions
                .iter()
                .rev()
                .find(|session| !session.model.trim().is_empty())
            {
                settings.experience_training_agent = session.agent_kind.clone();
                settings.experience_training_model = session.model.clone();
            }
        }
        if settings.experience_training_model.trim().is_empty() { return Err("尚无可复用的经验训练模型，请先完成一次经验训练".into()); }

        let (mut evolved, original_ids) = {
            let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            let project = guard.project(&project_key);
            let original_ids = project.experiences.iter().map(|entry| entry.id.clone()).collect::<HashSet<_>>();
            (project, original_ids)
        };
        let stats = evolve_generation(&mut evolved, &settings.experience_experts);
        let candidates = evolved.experiences.iter().filter(|entry| !original_ids.contains(&entry.id)).cloned().collect::<Vec<_>>();
        if candidates.is_empty() {
            let quarantined_ids = evolved.experiences.iter().filter(|entry| entry.status == "quarantined").map(|entry| entry.id.clone()).collect::<HashSet<_>>();
            let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            let mut project = guard.project(&project_key);
            evolve(&mut project, &settings.experience_experts);
            evolve_universal(&mut guard.universal_experiences, &settings.experience_experts);
            for entry in &mut project.experiences {
                if quarantined_ids.contains(&entry.id) { entry.status = "quarantined".into(); }
            }
            project.evolution_generation = evolved.evolution_generation;
            project.last_evolution_at = now_ms();
            if project.project_root.is_empty() { project.project_root = project_root.clone(); }
            guard.projects.insert(project_key.clone(), project);
            guard.save();
            return Ok(stats);
        }
        // 后代直接继承父代的文字，父代及同类高词汇重叠项已足以完成语义审核；限制条数避免经验库增长后 prompt 无界膨胀。
        let parent_ids = candidates.iter().flat_map(|entry| entry.parent_ids.iter().cloned()).collect::<HashSet<_>>();
        let mut existing_review = evolved.experiences.iter().filter(|entry| original_ids.contains(&entry.id) && parent_ids.contains(&entry.id))
            .map(|entry| json!({ "id": entry.id, "expertId": entry.expert_id, "knowledgeScope": entry.knowledge_scope, "kind": entry.kind, "trigger": entry.trigger, "action": entry.action, "avoid": entry.avoid }))
            .collect::<Vec<_>>();
        for entry in evolved.experiences.iter().filter(|entry| original_ids.contains(&entry.id) && entry.status != "quarantined") {
            if existing_review.len() >= 120 { break; }
            let entry_terms = terms(&entry.action);
            if candidates.iter().any(|candidate| candidate.kind == entry.kind && !entry_terms.is_disjoint(&terms(&candidate.action)))
                && !existing_review.iter().any(|row| row["id"] == entry.id) {
                existing_review.push(json!({ "id": entry.id, "expertId": entry.expert_id, "knowledgeScope": entry.knowledge_scope, "kind": entry.kind, "trigger": entry.trigger, "action": entry.action, "avoid": entry.avoid }));
            }
        }

        let prompt = format!(r#"你是经验库世代演进的终审员。候选只是遗传算法选出的亲本组合，不是可直接入库的最终文字；你必须先完成演进，再审核去重。
对每条候选：结合 parentIds 对应知识，提炼出比任一亲本更准确、可执行的 trigger/action/avoid；可以收紧边界、融合互补步骤、补充关键反例或抽象共同规律。仅复制、改写或换专家不算演进，必须丢弃。
最终结果还必须与现有知识及本批结果语义去重：核心条件和行动已被覆盖的不要输出。确有实质增量时输出完整的新经验，candidateId 必须取自本代候选；kind 只能是 experience、memory、rule；knowledgeScope 只能是 universal、project，且不得把项目独有亲本演进成 universal，除非新文本已完全去除所有项目依赖并形成跨项目稳定规律。
返回纯 JSON，不要解释：{{"experiences":[{{"candidateId":"候选ID","knowledgeScope":"universal|project","kind":"experience","trigger":"演进后的适用条件","action":"演进后的行动结论","avoid":"应避免什么","scope":["领域"],"confidence":0.7}}]}}。

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
                project_root.clone(), agent.clone(),
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
            entry.knowledge_scope = if candidate.knowledge_scope == "universal" { "universal".into() } else { "project".into() };
            entry.trigger = candidate.trigger.trim().to_string();
            entry.action = candidate.action.trim().to_string();
            entry.avoid = candidate.avoid.trim().to_string();
            entry.scope = candidate.scope.into_iter().map(|scope| scope.trim().to_string()).filter(|scope| !scope.is_empty()).collect();
            entry.confidence = candidate.confidence.clamp(0.1, 0.95);
            // 用相似度去重替换精确 terms 相等：演进出的新条目往往只是换了说法，
            // 仅靠词集合完全相同根本拦不住，导致知识库越来越臃肿。
            let duplicate = evolved.experiences.iter().any(|old| original_ids.contains(&old.id) && old.status != "quarantined"
                && entries_duplicate(old, &entry));
            if !duplicate { accepted_entries.push(entry); }
        }
        let created = accepted_entries.len();
        let rejected = candidate_ids.len().saturating_sub(created);
        let (mut accepted_universal, accepted_project): (Vec<_>, Vec<_>) = accepted_entries
            .into_iter()
            .partition(|entry| entry.knowledge_scope == "universal");
        for entry in &mut accepted_universal {
            // 泛用知识不得携带项目会话或项目亲本标识，避免跨项目暴露项目内部来源。
            entry.source_thread_ids.clear();
            entry.parent_ids.clear();
        }

        let quarantined_ids = evolved.experiences.iter().filter(|entry| entry.status == "quarantined").map(|entry| entry.id.clone()).collect::<HashSet<_>>();
        let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
        let mut project = guard.project(&project_key);
        evolve(&mut project, &settings.experience_experts);
        evolve_universal(&mut guard.universal_experiences, &settings.experience_experts);
        for entry in &mut project.experiences {
            if quarantined_ids.contains(&entry.id) { entry.status = "quarantined".into(); }
        }
        project.evolution_generation = evolved.evolution_generation;
        project.last_evolution_at = now_ms();
        project.experiences.extend(accepted_project);
        guard.universal_experiences.extend(accepted_universal);
        if project.project_root.is_empty() { project.project_root = project_root.clone(); }
        guard.projects.insert(project_key.clone(), project);
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

fn training_expert_count(seed: i64, available: usize) -> usize {
    let requested = if deterministic_roll(&format!("expert-count:{seed}")) < 0.5 {
        2
    } else {
        3
    };
    requested.min(available)
}

fn render_thread(thread: &crate::threads::Thread) -> String {
    let mut out = format!("会话 {}：{}\n", thread.id, thread.title);
    for item in thread.items.iter().rev().take(16).rev() {
        match item {
            Item::User { text, .. } if !text.trim().is_empty() => {
                out.push_str(&format!("用户：{}\n", text.trim()))
            }
            Item::Assistant { text, .. } if !text.trim().is_empty() => {
                out.push_str(&format!("助手：{}\n", text.trim()))
            }
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
    format!(
        r#"把下面内容修复为严格 JSON。只输出 JSON object，不要 Markdown 代码块、解释或注释。
结构必须是：
{{"experts":[{{"expertId":"专家ID","experiences":[{{"knowledgeScope":"universal|project","kind":"experience|memory|rule","trigger":"适用条件或上下文","action":"结论、事实或强约束","avoid":"应避免什么","scope":["领域"],"confidence":0.6}}]}}]}}
knowledgeScope 缺失或无法确定时填 project；只有不依赖当前项目名称、目录、文件、API、配置、架构和用户偏好的稳定规律才能填 universal。
保留原内容的类型与表达；无法提取时返回 {{"experts":[]}}。

待修复内容：
{}"#,
        raw.chars().take(24_000).collect::<String>()
    )
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
    format!(
        r#"你是独立知识训练专家「{name}」（稳定标识：{id}），本次只代表该专家判断。
从会话中训练三种知识，kind 必须为以下之一：
- experience：从结果归纳的条件性经验，必须有 trigger 和 action，可反馈和淘汰。
- memory：有长期复用价值的客观事实或稳定背景；action 写事实，trigger 写适用项目或上下文。
- rule：用户明确要求、反复纠正或证据充分支持的强约束；action 写必须遵守的动作。普通建议不得升级为守则。
每条知识还必须标记 knowledgeScope：
- universal：真正泛用，可跨项目使用；不得依赖当前项目名称、目录、文件、函数、API、技术栈特有配置、架构决定或当前用户在本项目的偏好。
- project：项目独有；只要依赖当前项目事实、实现、命名、配置、约束、偏好，或无法证明跨项目稳定，就必须标 project。
禁止为了“更有价值”把项目经验拔高为 universal；证据不足时一律 project。
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
{{"experts":[{{"expertId":"{id}","experiences":[{{"knowledgeScope":"universal|project","kind":"experience|memory|rule","trigger":"适用条件或上下文","action":"结论、事实或强约束","avoid":"应避免什么","scope":["领域"],"confidence":0.6}}]}}]}}

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
    let agent: AgentKind = serde_json::from_value(json!(settings.experience_training_agent.trim()))
        .map_err(|_| format!("不支持的训练后端：{}", settings.experience_training_agent))?;
    if matches!(agent, AgentKind::OpenCode | AgentKind::OpenCodePlus) {
        return Err(format!("{} 暂不支持经验训练", agent.label()));
    }
    Ok(agent)
}

/// 在猎户座 Thread 上运行一个真实 agent turn，而不是旁路 complete_once。
/// 这样会复用常规会话运行态、流式事件、取消机制和 ChatView，用户能看到实际训练过程。
async fn run_training_turn(
    app: &AppHandle,
    agent: &AgentKind,
    thread_id: &str,
    prompt: String,
) -> Result<String, String> {
    let before = {
        let state = app.state::<AppState>();
        let value = state
            .store
            .lock()
            .unwrap()
            .get(thread_id)
            .map(|thread| thread.items.len())
            .ok_or("训练会话不存在")?;
        value
    };
    {
        let state = app.state::<AppState>();
        match agent {
            AgentKind::Alkaid => {
                state
                    .alkaid
                    .clone()
                    .run_prompt(thread_id.into(), prompt, Vec::new())
                    .await
            }
            AgentKind::Lyra => {
                state
                    .lyra
                    .clone()
                    .run_prompt(thread_id.into(), prompt, Vec::new())
                    .await
            }
            AgentKind::Codex | AgentKind::CodexPlus => {
                state
                    .codexplus
                    .clone()
                    .run_prompt(thread_id.into(), prompt, Vec::new())
                    .await
            }
            AgentKind::CodeBuddy | AgentKind::CodeBuddyPlus => {
                state
                    .codebuddyplus
                    .clone()
                    .run_prompt(thread_id.into(), prompt, Vec::new())
                    .await
            }
            AgentKind::ClaudeCode => {
                state
                    .claudeplus
                    .clone()
                    .run_prompt(thread_id.into(), prompt, Vec::new())
                    .await
            }
            AgentKind::Cursor => {
                state
                    .cursorplus
                    .clone()
                    .run_prompt(thread_id.into(), prompt, Vec::new())
                    .await
            }
            AgentKind::Devin => {
                state
                    .acp
                    .clone()
                    .run_prompt(thread_id.into(), prompt, Vec::new())
                    .await
            }
            AgentKind::OpenCode | AgentKind::OpenCodePlus => {
                return Err(format!(
                    "{} 暂不支持经验训练，请选择 Vega、Lyra、Devin、Codex、CodeBuddy、Claude 或 Cursor",
                    agent.label()
                ))
            }
        }
    }
    let state = app.state::<AppState>();
    let thread_store = state.store.lock().unwrap();
    let thread = thread_store.get(thread_id).ok_or("训练会话不存在")?;
    if let Some(text) = thread.items[before..]
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::Assistant { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
    {
        return Ok(text);
    }
    let error = thread.items[before..]
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::System { text, .. } if !text.trim().is_empty() => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "训练会话结束，但模型没有返回内容".into());
    Err(error)
}

pub async fn train(app: &AppHandle, cwd: &str, force: bool) -> Result<Value, String> {
    train_source_thread(app, cwd, force, None).await
}

async fn train_source_thread(
    app: &AppHandle,
    cwd: &str,
    force: bool,
    source_thread_id: Option<&str>,
) -> Result<Value, String> {
    let (project_key, project_root) = project_identity(cwd)?;
    if TRAINING.swap(true, Ordering::SeqCst) {
        return Err("已有一次经验训练正在进行".into());
    }
    let mut attempted = false;
    let result = async {
        let (settings, active_configs, threads) = {
            let state = app.state::<AppState>();
            let settings = state.settings.lock().unwrap().clone();
            if settings.experience_training_model.trim().is_empty() { return Err("请先选择经验训练模型".into()); }
            let guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            let project = guard.project(&project_key);
            if !force && !settings.experience_training_enabled { return Err("经验训练未启用".into()); }
            let last_schedule_at = project.last_train_at.max(project.last_attempt_at);
            if !force && now_ms() - last_schedule_at < settings.experience_training_interval_minutes.max(5) as i64 * 60_000 {
                return Ok(json!({ "trained": false, "reason": "notDue" }));
            }
            let seed = now_ms() / 1_000 + project.training_cycles as i64;
            let mut configs = settings.experience_experts.clone();
            configs.sort_by(|a, b| {
                project.last_trained_experts.contains(&a.id).cmp(&project.last_trained_experts.contains(&b.id))
                    .then_with(|| deterministic_roll(&format!("activate:{seed}:{}", a.id)).total_cmp(&deterministic_roll(&format!("activate:{seed}:{}", b.id))))
            });
            let active_count = training_expert_count(seed, configs.len());
            configs.truncate(active_count);
            let mut threads = project_threads(&state, &project_key);
            if let Some(source_thread_id) = source_thread_id {
                // 自动训练直接选择会话；项目只由会话 cwd 决定并作为知识标签，
                // 不再把同项目的其他会话捆成一个调度单元。
                threads.retain(|thread| thread.id == source_thread_id);
            }
            threads.retain(|thread| thread.items.iter().any(|item| matches!(item, Item::User { .. }))
                && configs.iter().any(|config| project.expert_processed_threads.get(&config.id)
                    .and_then(|seen| seen.get(&thread.id)).copied().unwrap_or(0) < thread.updated_at));
            threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));
            (settings, configs, threads.into_iter().take(MAX_TRAIN_THREADS).collect::<Vec<_>>())
        };
        if threads.is_empty() {
            let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            guard.project_mut(&project_key, &project_root).last_attempt_at = now_ms();
            guard.save();
            return Ok(json!({ "trained": false, "reason": "noNewSessions" }));
        }
        attempted = true;
        {
            let mut guard = store()?.lock().map_err(|_| "经验库锁已损坏".to_string())?;
            guard.project_mut(&project_key, &project_root).last_attempt_at = now_ms();
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
                let mut rows = guard.universal_experiences.iter()
                    .filter(|entry| entry.expert_id == config.id && entry.status != "quarantined")
                    .take(20)
                    .map(|entry| format!("- [泛用/{}] 当 {} => {}（避免：{}；用户评价={}；正反馈={}；负反馈={}；效用={:.2}）", entry.kind, entry.trigger, entry.action, entry.avoid, entry.user_feedback, entry.positive_count, entry.negative_count, entry.utility))
                    .collect::<Vec<_>>();
                rows.extend(guard.project(&project_key).experiences.iter()
                    .filter(|entry| entry.expert_id == config.id && entry.status != "quarantined")
                    .take(20)
                    .map(|entry| format!("- [项目独有/{}] 当 {} => {}（避免：{}；用户评价={}；正反馈={}；负反馈={}；效用={:.2}）", entry.kind, entry.trigger, entry.action, entry.avoid, entry.user_feedback, entry.positive_count, entry.negative_count, entry.utility)));
                if rows.is_empty() { "（该专家暂无知识）".into() } else { rows.join("\n") }
            };
            let prompt = training_prompt(config, &existing, &conversations);
            let session_id = {
                let state = app.state::<AppState>();
                let mut thread = Thread::new(
                    project_root.clone(), agent.clone(),
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
            let project = guard.project_mut(&project_key, &project_root);
            project.training_sessions.push(ExperienceTrainingSession {
                id: session_id, created_at: now_ms(), agent_kind: settings.experience_training_agent.clone(),
                model: settings.experience_training_model.clone(), expert_id,
                source_thread_ids: source_thread_ids.clone(), conversation: conversations.clone(),
                output, status: status.into(), error,
            });
            if project.training_sessions.len() > 100 {
                let excess = project.training_sessions.len() - 100;
                project.training_sessions.drain(..excess);
            }
            guard.save();
        }

        if combined.experts.is_empty() {
            return Err(failures.join("；"));
        }
        let activated_experts = active_configs.iter().map(|config| config.id.clone()).collect::<Vec<_>>();
        let learned = apply_training_output(&project_key, &project_root, &settings, &threads, combined);
        if let Ok(mut guard) = store().and_then(|value| value.lock().map_err(|_| "lock".into())) {
            guard.project_mut(&project_key, &project_root).last_trained_experts = activated_experts.clone();
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
    }
    .await;
    if attempted {
        // 自动调度的间隔从本轮结束时开始计算。失败或耗时较长的训练也必须进入冷却，
        // 否则若运行时间已接近间隔，下一次每分钟 tick 会在结束后立即再开一轮。
        if let Ok(mut guard) = store().and_then(|value| value.lock().map_err(|_| "lock".into())) {
            guard
                .project_mut(&project_key, &project_root)
                .last_attempt_at = now_ms();
            guard.save();
        }
    }
    TRAINING.store(false, Ordering::SeqCst);
    result
}

pub fn tick(app: &AppHandle) {
    consume_feedback_inbox(app);
    if TRAINING.load(Ordering::SeqCst) {
        return;
    }
    let action = {
        let state = app.state::<AppState>();
        let settings = state.settings.lock().unwrap().clone();
        if !settings.experience_training_enabled {
            None
        } else {
            let source_threads = state
                .store
                .lock()
                .unwrap()
                .threads
                .iter()
                .filter(|thread| is_training_source_thread(thread))
                .filter(|thread| {
                    thread
                        .items
                        .iter()
                        .any(|item| matches!(item, Item::User { .. }))
                })
                .cloned()
                .collect::<Vec<_>>();
            let guard = store()
                .and_then(|value| value.lock().map_err(|_| "lock".into()))
                .ok();
            guard.and_then(|guard| {
                let now = now_ms();
                let evolution_interval =
                    settings.experience_evolution_interval_minutes.max(10) as i64 * 60_000;
                let training_interval =
                    settings.experience_training_interval_minutes.max(5) as i64 * 60_000;
                let global_last_schedule_at = guard
                    .projects
                    .values()
                    .map(|project| project.last_train_at.max(project.last_attempt_at))
                    .max()
                    .unwrap_or(0);

                // 世代演进仍作用于知识所属项目；训练调度则不按项目轮转，而是直接从
                // 全部来源会话里选最新的未消费会话，再由该会话自然带出项目标签。
                let evolution = guard.projects.iter().find_map(|(_, project)| {
                    (!project.experiences.is_empty()
                        && now - project.last_evolution_at >= evolution_interval)
                        .then(|| project.project_root.clone())
                });
                if evolution.is_some() {
                    return evolution.map(|root| (true, root, None));
                }
                if now - global_last_schedule_at < training_interval {
                    return None;
                }

                source_threads
                    .into_iter()
                    .filter_map(|thread| {
                        let (project_key, project_root) = project_identity(&thread.cwd).ok()?;
                        let project = guard.project(&project_key);
                        let pending = settings.experience_experts.iter().any(|config| {
                            project
                                .expert_processed_threads
                                .get(&config.id)
                                .and_then(|seen| seen.get(&thread.id))
                                .copied()
                                .unwrap_or(0)
                                < thread.updated_at
                        });
                        pending.then_some((thread.updated_at, project_root, thread.id))
                    })
                    .max_by_key(|(updated_at, _, _)| *updated_at)
                    .map(|(_, root, thread_id)| (false, root, Some(thread_id)))
            })
        }
    };
    let Some((evolve_due, cwd, source_thread_id)) = action else {
        return;
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if evolve_due {
            let _ = evolve_memory(&app, &cwd).await;
        } else {
            let _ = train_source_thread(&app, &cwd, false, source_thread_id.as_deref()).await;
        }
    });
}

fn consume_feedback_inbox(app: &AppHandle) {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct InboxFeedback {
        #[serde(default)]
        cwd: String,
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
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten().take(64) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let parsed = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<InboxFeedback>(&raw).ok());
        if let Some(feedback) = parsed {
            if feedback_memory(
                &feedback.cwd,
                &feedback.experience_ids,
                feedback.reward,
                &feedback.note,
                &configs,
            )
            .is_ok()
            {
                let _ = fs::remove_file(path);
            }
        } else {
            let _ = fs::rename(&path, path.with_extension("invalid"));
        }
    }
}

fn apply_training_output(
    project_key: &str,
    project_root: &str,
    settings: &Settings,
    threads: &[crate::threads::Thread],
    output: TrainingOutput,
) -> usize {
    let Ok(mut guard) = store().and_then(|s| s.lock().map_err(|_| "lock".into())) else {
        return 0;
    };
    let mut project = guard.project(project_key);
    let now = now_ms();
    let source_ids: Vec<String> = threads.iter().map(|t| t.id.clone()).collect();
    let configs: HashMap<&str, &ExperienceExpertConfig> = settings
        .experience_experts
        .iter()
        .map(|x| (x.id.as_str(), x))
        .collect();
    let mut learned = 0;
    let mut activated_experts = Vec::new();
    for expert in output.experts {
        activated_experts.push(expert.expert_id.clone());
        let Some(config) = configs.get(expert.expert_id.as_str()) else {
            continue;
        };
        let max_candidates =
            ((config.write_rate.clamp(0.0, 1.0) * 3.0).ceil() as usize).clamp(1, 3);
        for candidate in expert.experiences.into_iter().take(max_candidates) {
            if candidate.action.trim().is_empty()
                || (candidate.kind == "experience" && candidate.trigger.trim().is_empty())
            {
                continue;
            }
            let knowledge_scope = if candidate.knowledge_scope == "universal" {
                "universal".to_string()
            } else {
                "project".to_string()
            };
            let duplicate = if knowledge_scope == "universal" {
                guard.universal_experiences.iter().any(|e| {
                    e.expert_id == expert.expert_id
                        && e.kind == candidate.kind
                        && e.status != "quarantined"
                        && terms(&e.action) == terms(&candidate.action)
                })
            } else {
                project.experiences.iter().any(|e| {
                    e.expert_id == expert.expert_id
                        && e.kind == candidate.kind
                        && e.status != "quarantined"
                        && terms(&e.action) == terms(&candidate.action)
                })
            };
            if duplicate {
                continue;
            }
            let generation = project.training_cycles as u32;
            let kind = match candidate.kind.as_str() {
                "memory" | "rule" => candidate.kind,
                _ => "experience".into(),
            };
            let mut entry = ExperienceEntry {
                id: uuid::Uuid::new_v4().to_string(),
                expert_id: expert.expert_id.clone(),
                project_id: project_key.to_string(),
                project_root: project_root.to_string(),
                knowledge_scope: knowledge_scope.clone(),
                kind,
                trigger: candidate.trigger,
                action: candidate.action,
                avoid: candidate.avoid,
                scope: candidate.scope,
                source_thread_ids: source_ids.clone(),
                parent_ids: Vec::new(),
                generation,
                confidence: candidate.confidence.clamp(0.1, 0.9),
                utility: 0.0,
                positive_count: 0,
                negative_count: 0,
                user_feedback: 0,
                hit_count: 0,
                created_at: now,
                updated_at: now,
                last_used_at: 0,
                status: "candidate".into(),
            };
            if knowledge_scope == "universal" {
                // 泛用知识正文可跨项目，来源会话 id 仍是项目内部数据，不进入全局分区。
                entry.source_thread_ids.clear();
                guard.universal_experiences.push(entry);
            } else {
                project.experiences.push(entry);
            }
            learned += 1;
        }
    }
    for expert_id in activated_experts {
        let seen = project
            .expert_processed_threads
            .entry(expert_id)
            .or_default();
        for thread in threads {
            seen.insert(thread.id.clone(), thread.updated_at);
        }
    }
    project.last_train_at = now;
    project.training_cycles = project.training_cycles.saturating_add(1);
    evolve(&mut project, &settings.experience_experts);
    evolve_universal(
        &mut guard.universal_experiences,
        &settings.experience_experts,
    );
    if project.project_root.is_empty() {
        project.project_root = project_root.to_string();
    }
    guard.projects.insert(project_key.to_string(), project);
    guard.save();
    learned
}

fn fitness(entry: &ExperienceEntry) -> f64 {
    entry.utility * 2.0
        + entry.confidence
        + (entry.positive_count as f64 - entry.negative_count as f64 * 1.25) * 0.15
        + (entry.hit_count as f64 + 1.0).ln() * 0.08
}

/// 日常维护：遗忘率按未使用天数衰减效用，并限制各岛容量。
fn evolve(store: &mut ProjectExperienceStore, configs: &[ExperienceExpertConfig]) {
    let now = now_ms();
    for config in configs {
        for entry in store
            .experiences
            .iter_mut()
            .filter(|e| e.expert_id == config.id && e.status != "quarantined")
        {
            let age_days =
                (now - entry.last_used_at.max(entry.created_at)).max(0) as f64 / 86_400_000.0;
            entry.utility *= (-config.forget_rate.clamp(0.0, 1.0) * age_days).exp();
        }
        let mut indices: Vec<usize> = store
            .experiences
            .iter()
            .enumerate()
            .filter(|(_, e)| e.expert_id == config.id && e.status != "quarantined")
            .map(|(i, _)| i)
            .collect();
        indices.sort_by(|&a, &b| {
            fitness(&store.experiences[b]).total_cmp(&fitness(&store.experiences[a]))
        });
        for &index in indices.iter().skip(MAX_EXPERIENCES_PER_EXPERT) {
            store.experiences[index].status = "quarantined".into();
        }
    }
    dedupe_entries(&mut store.experiences);
    purge_quarantined(&mut store.experiences);
}

/// 隔离超过 7 天的条目直接删除，防止经验库文件无限膨胀。
/// 7 天足够发现误隔离并手动回滚；正常演进路径不会把近期条目长时间留在 quarantined。
fn purge_quarantined(experiences: &mut Vec<ExperienceEntry>) {
    let cutoff = now_ms() - 7 * 86_400_000;
    experiences.retain(|entry| {
        !(entry.status == "quarantined" && entry.updated_at < cutoff)
    });
}

/// 用词面相似度把同一 expert 内的近似重复条目隔离，保留适应度最高的一条。
fn dedupe_entries(experiences: &mut Vec<ExperienceEntry>) {
    // 按 (expert, kind, scope) 分组，在组内做 O(n²) 查重；每组实际几十条，开销可控。
    let mut groups: HashMap<(String, String, String), Vec<usize>> = HashMap::new();
    for (index, entry) in experiences.iter().enumerate() {
        if entry.status == "quarantined" || entry.status == "candidate" {
            continue;
        }
        groups
            .entry((
                entry.expert_id.clone(),
                entry.kind.clone(),
                entry.knowledge_scope.clone(),
            ))
            .or_default()
            .push(index);
    }
    for (_, indices) in groups {
        // 按适应度降序，保留排在前面的“最优代表”。
        let mut sorted = indices.clone();
        sorted.sort_by(|&a, &b| fitness(&experiences[b]).total_cmp(&fitness(&experiences[a])));
        let mut kept: Vec<usize> = Vec::new();
        for &index in &sorted {
            let entry = &experiences[index];
            if kept
                .iter()
                .any(|&k| entries_duplicate(&experiences[k], entry))
            {
                experiences[index].status = "quarantined".into();
            } else {
                kept.push(index);
            }
        }
    }
}

fn evolve_universal(store: &mut Vec<ExperienceEntry>, configs: &[ExperienceExpertConfig]) {
    let now = now_ms();
    for config in configs {
        for entry in store
            .iter_mut()
            .filter(|entry| entry.expert_id == config.id && entry.status != "quarantined")
        {
            let age_days =
                (now - entry.last_used_at.max(entry.created_at)).max(0) as f64 / 86_400_000.0;
            entry.utility *= (-config.forget_rate.clamp(0.0, 1.0) * age_days).exp();
        }
        let mut indices: Vec<usize> = store
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.expert_id == config.id && entry.status != "quarantined")
            .map(|(index, _)| index)
            .collect();
        indices.sort_by(|&left, &right| fitness(&store[right]).total_cmp(&fitness(&store[left])));
        for &index in indices.iter().skip(MAX_EXPERIENCES_PER_EXPERT) {
            store[index].status = "quarantined".into();
        }
    }
    dedupe_entries(store);
    purge_quarantined(store);
}

/// 完整群岛遗传世代：岛内选择、双亲继承、参数化变异、环形迁移。
fn evolve_generation(
    store: &mut ProjectExperienceStore,
    configs: &[ExperienceExpertConfig],
) -> Value {
    evolve(store, configs);
    store.evolution_generation = store.evolution_generation.saturating_add(1);
    let generation = store.evolution_generation;
    let now = now_ms();
    let mut offspring = Vec::new();
    let (mut crossed, mut mutated, mut migrated, mut quarantined) =
        (0usize, 0usize, 0usize, 0usize);
    for config in configs {
        let mut island = store
            .experiences
            .iter()
            .filter(|e| e.expert_id == config.id && e.status != "quarantined")
            .cloned()
            .collect::<Vec<_>>();
        island.sort_by(|a, b| fitness(b).total_cmp(&fitness(a)));
        if island.len() >= 8 {
            let survivors = (island.len() * 4 / 5).max(2);
            let discarded = island
                .iter()
                .skip(survivors)
                .map(|e| e.id.clone())
                .collect::<HashSet<_>>();
            for entry in store
                .experiences
                .iter_mut()
                .filter(|e| discarded.contains(&e.id))
            {
                entry.status = "quarantined".into();
                quarantined += 1;
            }
            island.truncate(survivors);
        }
        if island.len() < 2
            || deterministic_roll(&format!("cross:{}:{generation}", config.id))
                > config.write_rate.clamp(0.0, 1.0)
        {
            continue;
        }
        let (parent_a, parent_b) = (&island[0], &island[1]);
        // 双亲内容高度相似时不再生成候选：子代大概率只是换说法，审核成本高且入库价值低。
        if text_similarity(&parent_a.action, &parent_b.action) >= 0.55 {
            continue;
        }
        let mut child = parent_a.clone();
        child.id = uuid::Uuid::new_v4().to_string();
        child.parent_ids = vec![parent_a.id.clone(), parent_b.id.clone()];
        for scope in &parent_b.scope {
            if !child.scope.contains(scope) {
                child.scope.push(scope.clone());
            }
        }
        child.generation = parent_a
            .generation
            .max(parent_b.generation)
            .saturating_add(1);
        child.confidence = ((parent_a.confidence + parent_b.confidence) / 2.0).clamp(0.1, 0.95);
        child.utility = ((parent_a.utility + parent_b.utility) * 0.25).clamp(-1.0, 1.0);
        child.positive_count = 0;
        child.negative_count = 0;
        child.hit_count = 0;
        child.created_at = now;
        child.updated_at = now;
        child.last_used_at = 0;
        child.status = "candidate".into();
        if deterministic_roll(&format!("mutation:{}:{generation}", config.id))
            <= config.mutation_rate.clamp(0.0, 1.0)
        {
            let delta = (deterministic_roll(&format!("mutation-delta:{}:{generation}", config.id))
                - 0.5)
                * 0.3;
            child.confidence = (child.confidence + delta).clamp(0.1, 0.95);
            child.utility = (child.utility + delta).clamp(-1.0, 1.0);
            mutated += 1;
        }
        offspring.push(child);
        crossed += 1;
    }
    if configs.len() > 1 {
        for (i, source) in configs.iter().enumerate() {
            let target = &configs[(i + 1) % configs.len()];
            if deterministic_roll(&format!(
                "migration:{}:{}:{generation}",
                source.id, target.id
            )) > target.migration_rate.clamp(0.0, 1.0)
            {
                continue;
            }
            if let Some(parent) = store
                .experiences
                .iter()
                .filter(|e| e.expert_id == source.id && e.status != "quarantined")
                .max_by(|a, b| fitness(a).total_cmp(&fitness(b)))
                .cloned()
            {
                // 目标岛已有高度相似的条目时跳过，避免同一条知识在不同专家之间复制。
                if store.experiences.iter().any(|e| {
                    e.expert_id == target.id
                        && e.status != "quarantined"
                        && e.kind == parent.kind
                        && text_similarity(&e.action, &parent.action) >= 0.65
                }) {
                    continue;
                }
                let mut child = parent.clone();
                child.id = uuid::Uuid::new_v4().to_string();
                child.expert_id = target.id.clone();
                child.parent_ids = vec![parent.id];
                child.generation = child.generation.saturating_add(1);
                child.confidence = (child.confidence * 0.8).max(0.2);
                child.utility = 0.0;
                child.positive_count = 0;
                child.negative_count = 0;
                child.hit_count = 0;
                child.created_at = now;
                child.updated_at = now;
                child.last_used_at = 0;
                child.status = "candidate".into();
                offspring.push(child);
                migrated += 1;
            }
        }
    }
    let created = offspring.len();
    store.experiences.extend(offspring);
    json!({ "generation": generation, "created": created, "crossed": crossed, "mutated": mutated, "migrated": migrated, "quarantined": quarantined })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn training_randomly_activates_two_or_three_experts() {
        let counts = (0..100)
            .map(|seed| training_expert_count(seed, 7))
            .collect::<HashSet<_>>();
        assert_eq!(counts, HashSet::from([2, 3]));
        assert_eq!(training_expert_count(0, 1), 1);
    }

    #[test]
    fn training_agent_accepts_devin_and_rejects_opencode() {
        let mut settings = Settings::default();
        settings.experience_training_agent = "devin".into();
        assert_eq!(training_agent(&settings).unwrap(), AgentKind::Devin);

        settings.experience_training_agent = "opencode".into();
        assert!(training_agent(&settings)
            .unwrap_err()
            .contains("暂不支持经验训练"));
    }

    #[test]
    fn training_and_evolution_sessions_are_not_training_sources() {
        let mut ordinary = Thread::new(String::new(), AgentKind::Lyra, None, None, None, false);
        assert!(is_training_source_thread(&ordinary));

        ordinary.experience_thread = true;
        ordinary.title = "经验训练 · 1 个来源会话 · fast".into();
        assert!(!is_training_source_thread(&ordinary));

        ordinary.title = "世代演进审核 · 第 2 代".into();
        assert!(!is_training_source_thread(&ordinary));
    }
}
