//! 上下文窗口（Reasonix studio 投影/折叠机制的移植）：规范历史只追加、从不改写，
//! 发给模型的是「投影」——被折叠的前缀换成一条 `<compaction-summary>` 用户消息，
//! 其后接规范历史未覆盖的尾部。投影与账本另存 `<id>.context.json`，校验失败即回退规范历史。

use crate::lyra::history::{now_ms, text_content, History};
use crate::lyra::tokens::{TokenBreakdown, TokenCounter, IMAGE_TOKENS};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::path::{Path, PathBuf};

const COMPACT_RATIO: f64 = 0.85;
const CHECKPOINT_CEILING_RATIO: f64 = 0.50;
const RECENT_TAIL_RATIO: f64 = 0.10;
const MIN_TAIL_TOKENS: u64 = 32_000;
const MAX_TAIL_TOKENS: u64 = 96_000;
pub const SUMMARY_OUTPUT_MAX_TOKENS: u64 = 16_000;
const EXCEPTIONAL_SAVINGS_RATIO: f64 = 0.25;
const MIN_RECENT_KEEP: usize = 2;
const PINNED_FIRST_USER_TOKENS: u64 = 1500;
const PINNED_FIRST_USER_FRAC: f64 = 0.15;
const KEPT_USER_TURNS_FRAC: f64 = 0.05;
const PROTOCOL_RESERVE: u64 = 256;
/// 输出预留最多占窗口的比例：max_output_tokens 接近窗口时仍给输入留出折叠空间。
const MAX_OUTPUT_RESERVE_RATIO: f64 = 0.25;
const MIN_FOLD_TOKENS: u64 = 100;
const MIN_SUMMARY_SPAN: u64 = 4000;
const SUMMARY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const RECALL_RATIO: f64 = 0.10;
const MIN_RECALL_TOKENS: u64 = 2000;
const NOTICE_RUNGS: [f64; 2] = [0.75, 0.92];
const MIN_PRUNE_BYTES: usize = 1024;

const TAG_OPEN: &str = "<compaction-summary>";
const TAG_CLOSE: &str = "</compaction-summary>";
const INDEX_HEADING: &str = "## Folded work index (#n = transcript position)";
const BACKSTOP_HEADING: &str = "## Host-retained fold facts";
const ELISION_MARKER: &str = " lines omitted …";

pub const SUMMARY_SYSTEM_PROMPT: &str = r#"You are compacting the earlier part of a coding agent's conversation to save context.
The agent keeps your summary alongside the user's own turns (kept verbatim) and the recent tail; your job is to fold the assistant/tool work into a briefing it can resume from.
Write under these exact headings, omitting a heading only if it has no content:

## Standing facts & constraints
Everything the user stated that still governs the work — names, paths, IDs, versions, tokens, preferences, and hard "never do X" rules — in their own words. Be exhaustive; this is the durable contract, so prefer over- to under-including.

## Goal
The user's request and intent.

## Decisions & rationale
Key choices made so far and why — so they are not re-litigated or reversed.

## Files & code
Files read or modified, with the specific facts that matter: signatures, line locations, data shapes, and exact edits applied. Be concrete; this is what lets the agent act without re-reading everything.

## Commands & outcomes
Commands run (builds, tests, git) and their relevant results — what passed, what failed, and the error text that matters.

## Errors & fixes
Problems hit and how they were resolved (or not), so the same dead ends are not repeated.

## Pending & next step
What is still in progress or unstarted, and the single most concrete next action to take.

Rules: be terse — bullet points and fragments, not prose. Preserve identifiers, paths, and numbers exactly. Do NOT invent anything not present in the messages; if something is unknown, leave it out rather than guessing."#;

/// 触发原因：Auto 仅在超过阈值时折叠；Overflow（provider 报超长）与 Manual 强制折叠。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    Auto,
    Overflow,
    Manual,
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Projection {
    messages: Vec<Value>,
    covered_count: usize,
    covered_prefix_hash: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Receipt {
    /// applied / noop / rejected
    pub status: String,
    pub trigger: String,
    pub reason: String,
    pub input_hash: String,
    pub source_tokens: u64,
    pub result_tokens: u64,
    pub saved_tokens: u64,
    pub covered_count: usize,
    pub folded_messages: usize,
    pub degraded: bool,
    pub at: u64,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct State {
    version: u32,
    generation: u64,
    projection: Option<Projection>,
    /// provider 实测 prompt tokens / 本地估算，校准后续估算。
    calibration: f64,
    /// 估算口径（计数方法 + 去 reasoning 策略）变化时校准作废；不改变投影版本。
    accounting_key: String,
    last_actual_usage: Option<Value>,
    /// 上次 provider 实测对应的已发送前缀；当前视图仍以它开头时以实测为准，只估算新增部分。
    anchor: Option<Anchor>,
    notice_rung: usize,
    recall_spent: u64,
    last_receipt: Option<Receipt>,
    acp: acp::AcpState,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Anchor {
    count: usize,
    prefix_hash: String,
    prompt_tokens: u64,
    /// 实测时的 system/tools 估算，用于系统提示变化后补差额。
    fixed: u64,
}

const STATE_VERSION: u32 = 1;

mod acp;
pub use acp::GUIDE as ACP_GUIDE;

pub fn context_path(root: &Path, session_id: &str) -> PathBuf {
    root.join(format!("{session_id}.context.json"))
}

pub struct ContextWindow {
    window: u64,
    counter: TokenCounter,
    fixed: TokenBreakdown,
    reserved_output: u64,
    completed_before: usize,
    strip_reasoning: bool,
    accounting_key: String,
    path: Option<PathBuf>,
    state: State,
    /// ACP 模式：模型主动压缩，取代下面的自动摘要折叠（后者仅用于旧会话遗留的投影）。
    acp_enabled: bool,
}

impl ContextWindow {
    /// 读取会话的投影账本；损坏或版本不符时从空账本开始（规范历史不受影响）。
    pub fn load(root: &Path, session_id: &str, window: u64, overhead: u64) -> Self {
        let path = context_path(root, session_id);
        let state = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<State>(&text).ok())
            .filter(|state| state.version == STATE_VERSION)
            .unwrap_or_default();
        Self { path: Some(path), state, ..Self::ephemeral(window, overhead) }
    }

    /// 子 agent 用：只在内存里；旧调用的合计开销仍有效，但无法拆为 system/tools。
    pub fn ephemeral(window: u64, overhead: u64) -> Self {
        let counter = TokenCounter::default();
        let fixed = TokenBreakdown { overhead, ..TokenBreakdown::default() };
        let accounting_key = hash_values(&[counter.metadata(), json!(false)]);
        Self { window, counter, fixed, reserved_output: 0, completed_before: 0, strip_reasoning: false, accounting_key, path: None, state: State::default(), acp_enabled: false }
    }

    /// tools 应传实际工具定义 JSON，而非已编码的 JSON 字符串。替代旧 overhead，不重复累计。
    pub fn configure_tokens(&mut self, counter: TokenCounter, system: &str, tools: &Value, reserved_output: u64) {
        self.fixed = TokenBreakdown {
            system: counter.count_text(system),
            tools: if tools.is_null() || tools.as_array().is_some_and(Vec::is_empty) { 0 } else { counter.count_json(tools) },
            overhead: if system.is_empty() { 3 } else { 7 }, // 回复前缀 + system 消息封装，仍是估算。
            ..TokenBreakdown::default()
        };
        self.accounting_key = hash_values(&[counter.metadata(), json!(self.strip_reasoning)]);
        self.counter = counter;
        self.reserved_output = reserved_output;
    }

    /// 每轮折叠检查前设置；completed_before 是规范历史下标，不是投影后的下标。
    pub fn set_prompt_policy(&mut self, completed_before: usize, strip_reasoning: bool) {
        self.completed_before = completed_before;
        self.strip_reasoning = strip_reasoning;
        self.accounting_key = hash_values(&[self.counter.metadata(), json!(strip_reasoning)]);
    }

    fn normalize_prompt(&self, messages: &[Value], completed_before: usize) -> Vec<Value> {
        coalesce_user_runs(History::for_prompt(messages, completed_before, self.strip_reasoning))
    }

    /// 折叠预算与实际发送共用此视图；record_usage 必须接收该请求的同一份 messages。
    pub fn prompt_view(&self, canonical: &[Value]) -> Vec<Value> {
        if self.acp_enabled {
            let (view, map) = self.acp_view(canonical);
            return self.normalize_prompt(&view, map.get(self.completed_before).copied().unwrap_or(0));
        }
        let completed = self.view_index(canonical, self.completed_before).unwrap_or(0);
        self.normalize_prompt(&self.view(canonical), completed)
    }

    fn save(&mut self) {
        let Some(path) = &self.path else { return };
        self.state.version = STATE_VERSION;
        let Ok(text) = serde_json::to_string(&self.state) else { return };
        let tmp = path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)) {
            eprintln!("lyra: 写上下文账本 {} 失败：{error}", path.display());
        }
    }

    pub fn window(&self) -> u64 {
        self.window
    }

    pub fn set_window(&mut self, window: u64) {
        // ACP 模式下没有声明窗口的模型按 200k 算。
        self.window = if window == 0 && self.acp_enabled { acp::DEFAULT_WINDOW } else { window };
    }

    /// 模型切换保留折叠账本，但旧模型的实测 token 校准不能跨模型沿用。
    pub fn reset_model_usage(&mut self) {
        self.state.anchor = None;
        self.state.calibration = 1.0;
        self.state.notice_rung = 0;
        self.state.last_actual_usage = None;
        self.save();
    }

    pub fn overhead(&self) -> u64 {
        self.fixed.total()
    }

    pub fn generation(&self) -> u64 {
        self.state.generation
    }

    fn calibration(&self) -> f64 {
        if self.state.accounting_key == self.accounting_key && (0.2..5.0).contains(&self.state.calibration) {
            self.state.calibration
        } else { 1.0 }
    }

    fn scaled(&self, fixed: u64) -> u64 {
        (fixed as f64 * self.calibration()).ceil() as u64
    }

    fn text_tokens(&self, text: &str) -> u64 {
        self.scaled(self.counter.count_text(text))
    }

    /// 已规范化请求的原始 BPE 分项 + 图像/协议估算；不含校准或预留输出。
    pub fn token_breakdown(&self, messages: &[Value]) -> TokenBreakdown {
        let mut parts = self.counter.count_messages(messages);
        parts.system += self.fixed.system;
        parts.tools += self.fixed.tools;
        parts.overhead += self.fixed.overhead;
        parts
    }

    /// 视图仍以上次实测请求开头时返回该锚点。
    // ponytail: 每次对已发送前缀做一次 sha256（O(历史大小)）；真成瓶颈再缓存 (len, hash)。
    fn anchor(&self, messages: &[Value]) -> Option<&Anchor> {
        self.state.anchor.as_ref()
            .filter(|a| a.count <= messages.len() && hash_values(&messages[..a.count]) == a.prefix_hash)
    }

    /// 已规范化 messages 作为一次请求的 prompt tokens（含系统提示与工具定义）：
    /// 有实测锚点时 = 实测 + 新增消息估算 + 固定开销差额；否则为校准后的整体估算。
    pub fn prompt_tokens(&self, messages: &[Value]) -> u64 {
        match self.anchor(messages) {
            Some(a) => {
                let fixed_delta = (self.fixed.total() as f64 - a.fixed as f64) * self.calibration();
                (a.prompt_tokens as f64 + fixed_delta).max(0.0).ceil() as u64 + self.messages_tokens(&messages[a.count..])
            }
            None => self.scaled(self.token_breakdown(messages).total()),
        }
    }

    fn messages_tokens(&self, messages: &[Value]) -> u64 {
        self.scaled(self.counter.count_messages(messages).total())
    }

    /// 保留旧接口。这里只收到 prompt 总数，不虚构 output/cache usage。
    pub fn calibrate(&mut self, sent: &[Value], prompt_tokens: u64) {
        if prompt_tokens == 0 { return; }
        let fixed = self.token_breakdown(sent).total();
        let ratio = prompt_tokens as f64 / fixed as f64;
        let accepted = fixed > 0 && (0.2..5.0).contains(&ratio);
        if accepted {
            self.state.calibration = ratio;
            self.state.accounting_key = self.accounting_key.clone();
        }
        self.state.anchor = Some(Anchor { count: sent.len(), prefix_hash: hash_values(sent), prompt_tokens, fixed: self.fixed.total() });
        self.state.last_actual_usage = Some(json!({
            "promptTokens": prompt_tokens, "localUncalibratedPromptTokens": fixed,
            "calibrationAccepted": accepted, "tokenizer": self.counter.metadata(), "at": now_ms()
        }));
        self.save();
    }

    /// 现有标准化 usage：input 不含 cacheRead/cacheWrite；reasoning 是 output 子集。
    /// 空/全零 usage 不覆盖上次实测，不能以本地估算冒充 provider 实测。
    pub fn record_usage(&mut self, sent: &[Value], usage: &Value) {
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        let prompt = n("input").saturating_add(n("cacheRead")).saturating_add(n("cacheWrite"));
        if prompt == 0 { return; }
        self.calibrate(sent, prompt);
        if let Some(last) = self.state.last_actual_usage.as_mut() {
            last["usage"] = usage.clone();
        }
        self.save();
    }

    pub fn trigger_tokens(&self) -> u64 {
        ((self.window as f64 * COMPACT_RATIO) as u64).min(self.hard_ceiling())
    }

    fn output_reserve(&self) -> u64 {
        self.reserved_output.min((self.window as f64 * MAX_OUTPUT_RESERVE_RATIO) as u64)
    }

    fn hard_ceiling(&self) -> u64 {
        self.window.saturating_sub(self.output_reserve()).saturating_sub(PROTOCOL_RESERVE)
    }

    fn input_budget_exhausted(&self) -> bool {
        self.window > 0 && self.hard_ceiling() <= self.scaled(self.overhead())
    }

    fn tail_budget(&self) -> u64 {
        let mut budget = (self.window as f64 * RECENT_TAIL_RATIO) as u64;
        if self.window >= 64_000 {
            budget = budget.max(MIN_TAIL_TOKENS);
        }
        budget.min(MAX_TAIL_TOKENS).min(self.window / 2).min(self.trigger_tokens() / 2)
    }

    // ponytail: 每次取视图都对已覆盖前缀做一次 sha256（O(历史大小)）；真成瓶颈再缓存 (len, hash)。
    fn valid_projection(&self, canonical: &[Value]) -> Option<&Projection> {
        self.state.projection.as_ref().filter(|p| {
            !p.messages.is_empty()
                && p.covered_count > 0
                && p.covered_count <= canonical.len()
                && hash_values(&canonical[..p.covered_count]) == p.covered_prefix_hash
        })
    }

    /// 模型可见视图：投影 + 规范历史未覆盖部分；投影无效时就是规范历史。
    pub fn view(&self, canonical: &[Value]) -> Vec<Value> {
        if self.acp_enabled {
            return self.acp_view(canonical).0;
        }
        match self.valid_projection(canonical) {
            Some(p) => {
                let mut out = p.messages.clone();
                out.extend_from_slice(&canonical[p.covered_count..]);
                out
            }
            None => canonical.to_vec(),
        }
    }

    /// 按 rewrite 改写模型可见视图（如 413 后省略旧图）并写入投影持久化，后续请求沿用；条数不变，无改动返回 false。
    /// 投影只覆盖到最后一条被改写的消息，之后的规范历史照常追加，不改动折叠代数。
    pub fn rewrite_view(&mut self, canonical: &[Value], rewrite: impl FnOnce(&[Value]) -> Option<Vec<Value>>) -> bool {
        if self.acp_enabled {
            // ACP 视图每次渲染时套用同一个图片裁剪（唯一调用方就是 drop_old_tool_images），这里只记开关。
            if self.state.acp.trim_images { return false; }
            let before = self.acp_view(canonical).0;
            self.state.acp.trim_images = true;
            if self.acp_view(canonical).0 == before {
                self.state.acp.trim_images = false;
                return false;
            }
            self.save();
            return true;
        }
        let view = self.view(canonical);
        let Some(rewritten) = rewrite(&view).filter(|r| r.len() == view.len()) else { return false };
        let Some(last) = (0..view.len()).rev().find(|&i| view[i] != rewritten[i]) else { return false };
        let (prior, prior_covered) = self.valid_projection(canonical).map_or((0, 0), |p| (p.messages.len(), p.covered_count));
        let covered = prior_covered + (last + 1).saturating_sub(prior);
        self.state.projection = Some(Projection {
            messages: rewritten[..prior + covered - prior_covered].to_vec(),
            covered_count: covered,
            covered_prefix_hash: hash_values(&canonical[..covered]),
        });
        self.save();
        true
    }

    /// 规范下标 → 视图下标；已被折叠（或保留在投影体里）的返回 None。
    pub fn view_index(&self, canonical: &[Value], index: usize) -> Option<usize> {
        if self.acp_enabled {
            return self.acp_view(canonical).1.get(index).copied();
        }
        match self.valid_projection(canonical) {
            Some(p) if index >= p.covered_count => Some(p.messages.len() + index - p.covered_count),
            Some(_) => None,
            None => Some(index),
        }
    }

    pub fn should_compact(&self, canonical: &[Value]) -> bool {
        if self.acp_enabled {
            return self.window > 0 && !self.input_budget_exhausted()
                && self.prompt_tokens(&self.prompt_view(canonical)) >= self.acp_emergency_tokens();
        }
        self.window > 0 && !self.input_budget_exhausted() && self.prompt_tokens(&self.prompt_view(canonical)) >= self.trigger_tokens()
    }

    /// 快到阈值时提醒模型收拢（75% / 92%，每代每档一次）。
    pub fn budget_notice(&mut self, canonical: &[Value]) -> Option<String> {
        if self.acp_enabled {
            return self.acp_nudge(canonical);
        }
        if self.window == 0 || self.input_budget_exhausted() {
            return None;
        }
        let trigger = self.trigger_tokens();
        let used = self.prompt_tokens(&self.prompt_view(canonical));
        let rung = NOTICE_RUNGS.iter().rposition(|r| used as f64 >= trigger as f64 * r)? + 1;
        if rung <= self.state.notice_rung {
            return None;
        }
        self.state.notice_rung = rung;
        self.save();
        let room = trigger.saturating_sub(used);
        Some(if rung == 1 {
            format!("<context-budget>\nAbout {room} tokens of room remain before this conversation is automatically compacted ({used} used of a {} -token window; the fold triggers at {trigger}).\nCompaction folds earlier assistant and tool messages into a summary. Selected user turns stay verbatim within the retention budget; other turns may be summarized. Anything you are holding only in your own earlier replies — exact paths, line numbers, a half-finished plan — survives only if you restate it or put it in the todo list.\nWork narrower from here: scope searches, read ranges rather than whole files, and do not start work whose output you cannot finish reading. Call context_budget when you need the current figure.\n</context-budget>", self.window)
        } else {
            format!("<context-budget>\nAbout {room} tokens of room remain before this conversation is automatically compacted.\nLand what you know now: state the current result, the exact next step, and any path, identifier, or number the summary would otherwise have to carry for you.\n</context-budget>")
        })
    }

    /// `context_budget` 工具的回答：分项保持原始口径，校准差额单列，不能把总量称为精确。
    pub fn budget_report(&self, canonical: &[Value]) -> String {
        let view = self.prompt_view(canonical);
        let parts = self.token_breakdown(&view);
        let raw = parts.total();
        let used = self.prompt_tokens(&view);
        let trigger = self.trigger_tokens();
        let recall_budget = MIN_RECALL_TOKENS.max((self.window as f64 * RECALL_RATIO) as u64);
        let mut report = json!({
            "tokenizer": self.counter.metadata(),
            "contextWindow": self.window,
            "automaticCompaction": self.window > 0 && !self.input_budget_exhausted(),
            "inputBudgetExhausted": self.input_budget_exhausted(),
            "promptNormalization": { "completedBefore": self.completed_before, "stripReasoning": self.strip_reasoning },
            "foldTrigger": trigger,
            "remainingBeforeCompaction": if self.window > 0 { Some(trigger.saturating_sub(used)) } else { None },
            "breakdown": parts,
            "uncalibratedPromptTokens": raw,
            "calibrationFactor": self.calibration(),
            "calibrationAdjustment": used as i128 - raw as i128,
            "anchoredToActualUsage": self.anchor(&view).is_some(),
            "estimatedPromptTokens": used,
            "reservedOutputTokens": self.output_reserve(),
            "protocolReserveTokens": PROTOCOL_RESERVE,
            "estimatedTotalWithReserves": used.saturating_add(self.output_reserve()).saturating_add(PROTOCOL_RESERVE),
            "remainingContextAfterReserves": if self.window > 0 { Some(self.hard_ceiling().saturating_sub(used)) } else { None },
            "folds": self.state.generation,
            "recall": { "budget": recall_budget, "spent": self.state.recall_spent, "remaining": recall_budget.saturating_sub(self.state.recall_spent) },
            "lastActualUsage": self.state.last_actual_usage,
            "notes": format!("Breakdown fields are uncalibrated and additive (messageCount/imageCount/unknownPartCount are counts). Counts are character-heuristic estimates; images ({IMAGE_TOKENS} each, including candidate screenshot paths), protocol overhead, reasoning replay and unknown content are placeholders. When anchoredToActualUsage is true, estimatedPromptTokens is the last provider-measured prompt plus estimates for messages added since. Legacy constructors leave system/tools unsplit in overhead until configured. Last actual usage belongs to its previous request, not necessarily this projection. No declared window disables automatic compaction; exhausted input budget means the fixed system prompt/tools fill the window."),
        });
        if self.acp_enabled {
            report["automaticCompaction"] = json!(false);
            report["acp"] = self.acp_status_value(canonical);
        }
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string())
    }

    fn receipt(&self, status: &str, trigger: Trigger, reason: impl Into<String>) -> Receipt {
        Receipt {
            status: status.into(),
            trigger: format!("{trigger:?}").to_lowercase(),
            reason: reason.into(),
            at: now_ms(),
            ..Receipt::default()
        }
    }

    /// 折叠一次：计划 → 分区（保留/折叠）→ 摘要 → 验收 → 提交。`active` 为当前轮用户消息的规范下标。
    /// `summarize(system, transcript)` 只调用一次；摘要失败且必须腾空间时退化为机械摘要。
    pub async fn compact<F, Fut>(
        &mut self,
        canonical: &[Value],
        active: Option<usize>,
        trigger: Trigger,
        summarize: F,
    ) -> Result<Receipt, String>
    where
        F: FnOnce(String, String) -> Fut,
        Fut: Future<Output = Result<String, String>>,
    {
        if self.acp_enabled {
            return Ok(self.acp_emergency(canonical, trigger));
        }
        if self.window == 0 {
            return Ok(self.receipt("noop", trigger, "no context window declared"));
        }
        // 溢出恢复不看本地预算：provider 已拒绝，只能尽量折叠。
        if self.input_budget_exhausted() && trigger != Trigger::Overflow {
            return Ok(self.receipt("rejected", trigger, "fixed system prompt/tools exhaust the context window"));
        }
        let (body_len, prior_covered, projected) = match self.valid_projection(canonical) {
            Some(p) => (p.messages.len(), p.covered_count, true),
            None => (0, 0, false),
        };
        let visible = self.view(canonical);
        let input_hash = hash_values(&visible);
        if let Some(last) = &self.state.last_receipt {
            if last.status == "applied" && last.input_hash == input_hash {
                return Ok(self.receipt("noop", trigger, "view unchanged since the last fold"));
            }
        }
        let completed = self.view_index(canonical, self.completed_before).unwrap_or(0);
        let source = self.prompt_tokens(&self.normalize_prompt(&visible, completed));
        if trigger == Trigger::Auto && source < self.trigger_tokens() {
            return Ok(self.receipt("noop", trigger, "below trigger"));
        }
        let must_free = trigger == Trigger::Overflow || source >= self.hard_ceiling();
        let force = trigger != Trigger::Auto;
        let active = active.and_then(|a| self.view_index(canonical, a));
        let Some((head, start)) = self.plan_fold(&visible, active, force) else {
            return Ok(self.receipt("noop", trigger, "nothing foldable outside the recent tail"));
        };
        if projected && !must_free {
            if start <= body_len {
                return Ok(self.receipt("noop", trigger, "fold would only refold the projection"));
            }
            if !force && self.messages_tokens(&visible[body_len..start]) < self.tail_budget() {
                return Ok(self.receipt("noop", trigger, "too little new material to fold"));
            }
        }

        let region = &visible[head..start];
        let keep = self.keep_indexes(region, active.filter(|a| (head..start).contains(a)).map(|a| a - head), &visible[start..]);
        let mut kept = Vec::new();
        let mut fold = Vec::new();
        for (message, keep) in region.iter().zip(&keep) {
            if *keep {
                kept.push(snip_failed_result(message));
            } else {
                fold.push(message.clone());
            }
        }
        if fold.is_empty() || self.messages_tokens(&fold) < MIN_FOLD_TOKENS {
            return Ok(self.receipt("noop", trigger, "fold too small to be worth a summary"));
        }
        let (fold, prior_index) = strip_fold_index(&fold);
        let origin = |i: usize| -> Option<usize> {
            let pos = head + i;
            match projected {
                true if pos < body_len => None,
                true => Some(prior_covered + pos - body_len),
                false => Some(pos),
            }
        };
        let entries = build_fold_index(region, &keep, origin);
        let fixed_prefix = self.prompt_tokens(&self.normalize_prompt(&visible[..head], completed));
        if fixed_prefix >= self.trigger_tokens() {
            return Ok(self.receipt("noop", trigger, "fixed prefix alone exceeds the trigger"));
        }

        let (digest, degraded) = match self.summarize_fold(&fold, summarize).await {
            Ok(text) if !text.trim().is_empty() => (text.trim().to_string(), false),
            outcome if must_free => {
                let cause = outcome.err().unwrap_or_else(|| "empty summary".into());
                eprintln!("lyra: compaction summary unavailable ({cause}); folded mechanically");
                (mechanical_digest(fold.len()), true)
            }
            Ok(_) => return Err("compaction summary was empty".into()),
            Err(error) => return Err(error),
        };
        let coverage = fold_coverage(&fold, &digest);
        if !must_free && !degraded && coverage.lost_every_change() {
            let mut receipt = self.receipt("rejected", trigger, format!("the digest carried none of the fold's changes ({})", coverage.reason()));
            receipt.input_hash = input_hash;
            self.state.last_receipt = Some(receipt.clone());
            self.save();
            return Ok(receipt);
        }
        let digest = attach_fold_index(&backstop(digest, &coverage), &prior_index, &entries, self.index_budget(), |t| self.text_tokens(t));

        let mut body = visible[..head].to_vec();
        body.push(summary_message(&digest));
        body.extend(kept);
        if start < body_len {
            body.extend_from_slice(&visible[start..body_len]);
        }
        let covered = prior_covered + start.saturating_sub(body_len);
        let mut candidate = body.clone();
        candidate.extend_from_slice(&canonical[covered..]);
        let completed = self.completed_before.checked_sub(covered).map_or(0, |i| body.len() + i);
        let result = self.prompt_tokens(&self.normalize_prompt(&candidate, completed));
        if let Err(reason) = self.accept(result, source, fixed_prefix, trigger == Trigger::Manual, must_free) {
            let mut receipt = self.receipt("rejected", trigger, reason);
            receipt.input_hash = input_hash;
            receipt.source_tokens = source;
            receipt.result_tokens = result;
            self.state.last_receipt = Some(receipt.clone());
            self.save();
            return Ok(receipt);
        }

        self.state.projection = Some(Projection {
            messages: body,
            covered_count: covered,
            covered_prefix_hash: hash_values(&canonical[..covered]),
        });
        self.state.generation += 1;
        self.state.notice_rung = 0;
        self.state.recall_spent = 0;
        let mut receipt = self.receipt("applied", trigger, "");
        receipt.input_hash = input_hash;
        receipt.source_tokens = source;
        receipt.result_tokens = result;
        receipt.saved_tokens = source.saturating_sub(result);
        receipt.covered_count = covered;
        receipt.folded_messages = fold.len();
        receipt.degraded = degraded;
        self.state.last_receipt = Some(receipt.clone());
        self.save();
        Ok(receipt)
    }

    /// 计划折叠区间 [head, start)：固定头（小的首条用户消息）之后、最近尾部之前；
    /// 当前轮之内只折到其已闭合的工具调用为止。
    fn plan_fold(&self, messages: &[Value], active: Option<usize>, force: bool) -> Option<(usize, usize)> {
        let head = self.pinned_len(messages);
        let mut budget = self.tail_budget();
        if force {
            budget = budget.min(self.messages_tokens(messages) / 2);
        }
        for min in [2usize, 1] {
            let mut start = self.tail_start(messages, head, budget);
            if let Some(a) = active.filter(|a| (head..start).contains(a)) {
                let closed = closed_prefix_end(&messages[a + 1..]);
                start = start.min(a + 1 + closed);
                if closed == 0 && a == head {
                    return None;
                }
            }
            if start > head && start - head >= min {
                return Some((head, start));
            }
        }
        None
    }

    fn pinned_len(&self, messages: &[Value]) -> usize {
        let limit = if self.window > 0 {
            PINNED_FIRST_USER_TOKENS.min((self.window as f64 * PINNED_FIRST_USER_FRAC) as u64)
        } else {
            PINNED_FIRST_USER_TOKENS
        };
        match messages.first() {
            Some(m) if role(m) == "user" && !is_summary(m) && self.messages_tokens(std::slice::from_ref(m)) <= limit => 1,
            _ => 0,
        }
    }

    /// 从最新往回累计，超出尾部预算即停（至少保留 MIN_RECENT_KEEP 条），起点不落在工具结果上。
    fn tail_start(&self, messages: &[Value], head: usize, budget: u64) -> usize {
        let mut start = messages.len();
        let mut acc = 0;
        while start > head {
            let cost = self.messages_tokens(&messages[start - 1..start]);
            if acc + cost > budget && messages.len() - start >= MIN_RECENT_KEEP {
                break;
            }
            acc += cost;
            start -= 1;
        }
        while start > head && start < messages.len() && role(&messages[start]) == "toolResult" {
            start -= 1;
        }
        start
    }

    /// 区间内必须原样保留的消息：失败的工具结果（连同调用组）、`[[keep]]` 用户消息、
    /// 当前轮用户消息，以及预算内（窗口 5%）从旧到新的用户原话。上一份摘要之前的不再保留。
    fn keep_indexes(&self, region: &[Value], active: Option<usize>, tail: &[Value]) -> Vec<bool> {
        let n = region.len();
        let mut keep = vec![false; n];
        // Keep the latest KEEP_FRAMES screenshots of EACH stream, isolated by toolName + frameStream.
        // Later tail records supersede the same stream only.
        let mut screens: std::collections::HashMap<(&str, &str), usize> = std::collections::HashMap::new();
        for index in (0..n + tail.len()).rev() {
            let message = if index < n { &region[index] } else { &tail[index - n] };
            for image in message.pointer("/details/images").and_then(Value::as_array).into_iter().flatten() {
                if let Some(stream) = super::provider::frame_of(message, image) {
                    let seen = screens.entry(stream).or_default();
                    if *seen < super::provider::KEEP_FRAMES && index < n { keep[index] = true; }
                    *seen += 1;
                }
            }
        }
        let policy_start = region.iter().rposition(is_summary).map_or(0, |i| i + 1);
        for i in policy_start..n {
            let m = &region[i];
            if is_error_result(m) || (role(m) == "user" && has_keep_marker(m)) || Some(i) == active {
                keep[i] = true;
            }
        }
        let budget = if self.window > 0 { (self.window as f64 * KEPT_USER_TURNS_FRAC) as u64 } else { 1024 };
        let mut spent = 0;
        for i in policy_start..n {
            let m = &region[i];
            if keep[i] || role(m) != "user" || is_summary(m) {
                continue;
            }
            let cost = self.messages_tokens(std::slice::from_ref(m));
            if spent + cost <= budget {
                spent += cost;
                keep[i] = true;
            }
        }
        // 保留的工具结果带上整组调用者：assistant 与它的全部结果。
        let kept_results: Vec<&str> = (0..n)
            .filter(|&i| keep[i] && role(&region[i]) == "toolResult")
            .filter_map(|i| region[i].get("toolCallId").and_then(Value::as_str))
            .collect();
        for id in kept_results {
            let Some(caller) = region.iter().position(|m| tool_calls(m).any(|(cid, _, _)| cid == id)) else {
                continue;
            };
            keep[caller] = true;
            let ids: Vec<&str> = tool_calls(&region[caller]).map(|(cid, _, _)| cid).collect();
            for (i, m) in region.iter().enumerate() {
                if m.get("toolCallId").and_then(Value::as_str).is_some_and(|cid| ids.contains(&cid)) {
                    keep[i] = true;
                }
            }
        }
        keep
    }

    fn accept(&self, candidate: u64, source: u64, fixed_prefix: u64, manual: bool, must_free: bool) -> Result<(), String> {
        let trigger = self.trigger_tokens();
        if candidate >= source {
            return Err(format!("candidate ({candidate}) is not smaller than the source ({source})"));
        }
        if manual && candidate < trigger {
            return Ok(());
        }
        let ceiling = (self.window as f64 * CHECKPOINT_CEILING_RATIO) as u64;
        if fixed_prefix > ceiling {
            let needed = (self.window as f64 * EXCEPTIONAL_SAVINGS_RATIO) as u64;
            if source - candidate < needed || candidate >= trigger || candidate >= self.hard_ceiling() {
                return Err(format!("fixed prefix above the checkpoint ceiling and the fold saved only {}", source - candidate));
            }
            return Ok(());
        }
        if candidate > ceiling && !must_free {
            return Err(format!("candidate ({candidate}) is above the checkpoint ceiling ({ceiling})"));
        }
        if candidate >= trigger {
            return Err(format!("candidate ({candidate}) is still above the trigger ({trigger})"));
        }
        Ok(())
    }

    fn index_budget(&self) -> u64 {
        256u64.max(self.window / 100)
    }

    fn summary_input_budget(&self) -> u64 {
        let framing = self.text_tokens(SUMMARY_SYSTEM_PROMPT);
        let budget = self
            .window
            .saturating_sub(SUMMARY_OUTPUT_MAX_TOKENS + framing + PROTOCOL_RESERVE);
        if budget < MIN_SUMMARY_SPAN { 0 } else { budget }
    }

    /// 摘要输入只为这一次请求缩短：长工具结果取头尾 → 仍超则省略中段；原文仍在规范历史里。
    async fn summarize_fold<F, Fut>(&self, fold: &[Value], summarize: F) -> Result<String, String>
    where
        F: FnOnce(String, String) -> Fut,
        Fut: Future<Output = Result<String, String>>,
    {
        let budget = self.summary_input_budget();
        let mut transcript = render_transcript(fold, false);
        if budget > 0 && self.text_tokens(&transcript) > budget {
            let shortened: Vec<Value> = fold.iter().map(sketch_tool_result).collect();
            transcript = render_transcript(&shortened, false);
            if self.text_tokens(&transcript) > budget {
                transcript = render_transcript(&self.omit_middle(&shortened, budget), false);
            }
            let tokens = self.text_tokens(&transcript);
            if tokens > budget {
                return Err(format!("summary input still exceeds single-request budget after shortening ({tokens} > {budget})"));
            }
        }
        tokio::time::timeout(SUMMARY_TIMEOUT, summarize(SUMMARY_SYSTEM_PROMPT.to_string(), transcript))
            .await
            .map_err(|_| "compaction summary timed out".to_string())?
    }

    /// 首尾交替（头部约占 2/3）装入预算，优先留摘要与失败结果，中间换成一行省略说明。
    fn omit_middle(&self, fold: &[Value], budget: u64) -> Vec<Value> {
        let marker_cost = self.text_tokens("[... 000 messages of this part omitted to fit the summarizer ...]");
        let Some(avail) = budget.checked_sub(marker_cost).filter(|a| *a >= MIN_SUMMARY_SPAN / 2) else {
            return fold.to_vec();
        };
        let cost = |m: &Value| self.text_tokens(&render_transcript(std::slice::from_ref(m), false));
        let (mut head, mut tail) = (Vec::new(), Vec::new());
        let (mut acc, mut i, mut j) = (0u64, 0usize, fold.len());
        while i < j {
            let from_head = head.len() <= tail.len() * 2;
            let m = if from_head { &fold[i] } else { &fold[j - 1] };
            let c = cost(m);
            if acc + c > avail {
                if !(is_summary(m) || is_error_result(m)) {
                    if from_head { i += 1 } else { j -= 1 }
                    continue;
                }
                break;
            }
            acc += c;
            if from_head {
                head.push(m.clone());
                i += 1;
            } else {
                tail.insert(0, m.clone());
                j -= 1;
            }
        }
        let dropped = fold.len() - head.len() - tail.len();
        if dropped == 0 || head.len() + tail.len() == 0 {
            return fold.to_vec();
        }
        head.push(json!({ "role": "user", "content": format!("[... {dropped} messages of this part omitted to fit the summarizer; the originals are retained in the canonical transcript ...]") }));
        head.extend(tail);
        head
    }

    /// `recall` 工具：按 #n 读回折叠掉的原文，或在折叠区内搜索；每代共享窗口 10% 的预算，超出整单拒绝。
    pub fn recall(&mut self, canonical: &[Value], positions: &[usize], query: Option<&str>) -> Result<String, String> {
        let query = query.map(str::trim).filter(|q| !q.is_empty());
        match (query.is_some(), positions.is_empty()) {
            (true, false) => return Err("recall: give positions to read or a query to search, not both".into()),
            (false, true) => return Err("recall: no positions given".into()),
            _ => {}
        }
        let covered = self.valid_projection(canonical).map_or(0, |p| p.covered_count);
        if covered == 0 {
            return Err("recall: nothing has been folded in this session yet, so every position is still in your context".into());
        }
        let budget = MIN_RECALL_TOKENS.max((self.window as f64 * RECALL_RATIO) as u64);
        let left = budget.saturating_sub(self.state.recall_spent);
        let mut body = String::new();
        if let Some(query) = query {
            let needle = query.to_lowercase();
            let mut hits = 0;
            for (i, m) in canonical[..covered].iter().enumerate() {
                let rendered = render_transcript(std::slice::from_ref(m), true);
                let lower = rendered.to_lowercase();
                let Some(at) = lower.find(&needle) else { continue };
                let from = floor_char_boundary(&rendered, at.saturating_sub(160));
                let to = floor_char_boundary(&rendered, (at + needle.len() + 160).min(rendered.len()));
                body.push_str(&format!("#{i} …{}…\n", rendered[from..to].trim()));
                hits += 1;
                if hits == 20 {
                    body.push_str("(more matches not shown; narrow the query)\n");
                    break;
                }
            }
            if hits == 0 {
                return Err(format!("recall: no folded message matches {query:?}"));
            }
        } else {
            let mut missing = Vec::new();
            for &pos in positions {
                if pos >= canonical.len() {
                    missing.push(format!("#{pos}"));
                    continue;
                }
                if pos >= covered {
                    return Err(format!("recall: #{pos} is not folded — it is still in your context, so read it there"));
                }
                body.push_str(&format!("#{pos}\n{}\n", render_transcript(&recall_span(canonical, pos), true).trim_end()));
            }
            if body.is_empty() {
                return Err(format!("recall: {} named nothing the transcript still holds", missing.join(", ")));
            }
            if !missing.is_empty() {
                body.push_str(&format!("(missing: {})\n", missing.join(", ")));
            }
        }
        let cost = self.text_tokens(&body);
        if cost > left {
            return Err(format!("recall: {cost} tokens exceeds the {left} left in this generation's recall budget — ask for fewer positions"));
        }
        self.state.recall_spent += cost;
        self.save();
        Ok(format!("{}\n(recall budget left: {} tokens)", body.trim_end(), left - cost))
    }
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn hash_values(values: &[Value]) -> String {
    let mut hasher = Sha256::new();
    for value in values {
        hasher.update(value.to_string().as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn role(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or_default()
}

fn tool_calls(m: &Value) -> impl Iterator<Item = (&str, &str, &Value)> {
    m.get("content")
        .and_then(Value::as_array)
        .filter(|_| role(m) == "assistant")
        .into_iter()
        .flatten()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("toolCall"))
        .map(|p| {
            (
                p.get("id").and_then(Value::as_str).unwrap_or_default(),
                p.get("name").and_then(Value::as_str).unwrap_or_default(),
                p.get("arguments").unwrap_or(&Value::Null),
            )
        })
}

fn is_summary(m: &Value) -> bool {
    role(m) == "user" && text_content(&m["content"]).starts_with(TAG_OPEN)
}

fn is_error_result(m: &Value) -> bool {
    if role(m) != "toolResult" {
        return false;
    }
    if m.get("isError").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    let text = text_content(&m["content"]).to_lowercase();
    text.starts_with("error:") || text.starts_with("blocked:")
}

fn has_keep_marker(m: &Value) -> bool {
    let text = text_content(&m["content"]).to_lowercase();
    ["[[keep]]", "[keep]", "<keep>", "<!-- keep -->"].iter().any(|marker| text.contains(marker))
}

/// 前缀内所有工具调用都已有结果的最长长度。
fn closed_prefix_end(messages: &[Value]) -> usize {
    let mut open = std::collections::HashSet::new();
    let mut closed = 0;
    for (i, m) in messages.iter().enumerate() {
        open.extend(tool_calls(m).map(|(id, _, _)| id.to_string()));
        if let Some(id) = m.get("toolCallId").and_then(Value::as_str) {
            open.remove(id);
        }
        if open.is_empty() {
            closed = i + 1;
        }
    }
    closed
}

fn summary_message(digest: &str) -> Value {
    json!({
        "role": "user",
        "content": [{ "type": "text", "text": format!("{TAG_OPEN}\nSummary of earlier conversation (older messages were compacted to save context):\n{digest}\n{TAG_CLOSE}") }],
        "timestamp": now_ms(),
    })
}

fn mechanical_digest(n: usize) -> String {
    format!("{n} earlier message(s) were folded here to free context, but the automatic summary was unavailable. Ask the user if you need details from before this point.")
}

/// 出站副本合并相邻的用户消息（摘要与保留的用户原话相邻时）；规范历史与投影不变。
pub fn coalesce_user_runs(messages: Vec<Value>) -> Vec<Value> {
    fn parts(m: &Value) -> Vec<Value> {
        match &m["content"] {
            Value::String(text) => vec![json!({ "type": "text", "text": text })],
            Value::Array(parts) => parts.clone(),
            _ => Vec::new(),
        }
    }
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    for m in messages {
        if role(&m) == "user" {
            if let Some(prev) = out.last_mut().filter(|p| role(p) == "user") {
                let mut merged = parts(prev);
                merged.extend(parts(&m));
                prev["content"] = Value::Array(merged);
                continue;
            }
        }
        out.push(m);
    }
    out
}

/// 摘要器/召回用的文本转写；摘要器只看参数键名，召回保留完整参数。
fn render_transcript(messages: &[Value], verbatim: bool) -> String {
    let mut names = std::collections::HashMap::new();
    let mut out = String::new();
    for m in messages {
        match role(m) {
            "user" => out.push_str(&format!("[user]\n{}\n\n", text_content(&m["content"]))),
            "assistant" => {
                let text = text_content(&m["content"]);
                if !text.is_empty() {
                    out.push_str(&format!("[assistant]\n{text}\n"));
                }
                for (id, name, args) in tool_calls(m) {
                    names.insert(id.to_string(), name.to_string());
                    let args = if verbatim { args.to_string() } else { summarize_args(args) };
                    out.push_str(&format!("[assistant calls {name}] {args}\n"));
                }
                out.push('\n');
            }
            "toolResult" => {
                let name = m
                    .get("toolName")
                    .and_then(Value::as_str)
                    .or_else(|| m.get("toolCallId").and_then(Value::as_str).and_then(|id| names.get(id)).map(String::as_str))
                    .unwrap_or("tool");
                out.push_str(&format!("[tool {name} result]\n{}\n\n", text_content(&m["content"])));
            }
            _ => {}
        }
    }
    out
}

fn summarize_args(args: &Value) -> String {
    match args.as_object() {
        Some(map) => {
            let keys: Vec<&str> = map.keys().map(String::as_str).collect();
            format!("{{{}}} ({} keys)", keys.join(", "), map.len())
        }
        None if args.is_null() => "(no arguments)".into(),
        None => format!("({} bytes)", args.to_string().len()),
    }
}

/// 召回一条调用时带上回答它的工具结果。
fn recall_span(canonical: &[Value], pos: usize) -> Vec<Value> {
    let mut span = vec![canonical[pos].clone()];
    let mut wanted: Vec<&str> = tool_calls(&canonical[pos]).map(|(id, _, _)| id).collect();
    for m in &canonical[pos + 1..] {
        if wanted.is_empty() || role(m) != "toolResult" {
            break;
        }
        if let Some(i) = wanted.iter().position(|id| m.get("toolCallId").and_then(Value::as_str) == Some(*id)) {
            wanted.swap_remove(i);
            span.push(m.clone());
        }
    }
    span
}

fn read_only_tool(name: &str) -> bool {
    matches!(name, "read" | "polaris" | "recall" | "context_budget" | "search_context" | "acp_status" | "list_agents" | "wait_agent")
}

fn with_text(m: &Value, text: String) -> Value {
    let mut out = m.clone();
    out["content"] = json!([{ "type": "text", "text": text }]);
    out
}

/// 摘要器输入里的长工具结果改为头尾草图（只读 80/12 行，有副作用 40/40 行）。
fn sketch_tool_result(m: &Value) -> Value {
    if role(m) != "toolResult" || is_error_result(m) {
        return m.clone();
    }
    let text = text_content(&m["content"]);
    if text.len() < MIN_PRUNE_BYTES {
        return m.clone();
    }
    let name = m.get("toolName").and_then(Value::as_str).unwrap_or("tool");
    let (head, tail, head_chars, tail_chars) = if read_only_tool(name) { (80, 12, 10_000, 2_000) } else { (40, 40, 8_000, 8_000) };
    let lines: Vec<&str> = text.lines().collect();
    let sketch = if lines.len() <= head + tail {
        let h = floor_char_boundary(&text, head_chars.min(text.len() / 2));
        let t = floor_char_boundary(&text, text.len() - tail_chars.min(text.len() / 4));
        format!("[snipped tool result — {name}, {} bytes; single large line truncated]\n{}\n[... {} bytes omitted ...]\n{}", text.len(), &text[..h], t.saturating_sub(h), &text[t..])
    } else {
        format!(
            "[snipped tool result — {name}, {} bytes; showing first {head} lines and last {tail} lines]\n{}\n[... {} lines omitted ...]\n{}",
            text.len(),
            lines[..head].join("\n"),
            lines.len() - head - tail,
            lines[lines.len() - tail..].join("\n")
        )
    };
    with_text(m, sketch)
}

/// 保留的失败结果只留带失败信息的行（前 3 行、末 12 行、失败词 ±2 行，最多 60 行），其余标注省略。
fn snip_failed_result(m: &Value) -> Value {
    if !is_error_result(m) {
        return m.clone();
    }
    let text = text_content(&m["content"]);
    let snipped = snip_failure(&text);
    if snipped == text { m.clone() } else { with_text(m, snipped) }
}

fn snip_failure(content: &str) -> String {
    const MARKERS: [&str; 14] = [
        "fail", "error", "panic:", "fatal", "assert", "expected", "want:", "got:", "exit status",
        "undefined:", "cannot ", "no such", "timeout", "timed out",
    ];
    if content.contains(ELISION_MARKER) {
        return content.to_string();
    }
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.len() < 24 {
        return content.to_string();
    }
    let mut keep = vec![false; lines.len()];
    for i in (0..3).chain(lines.len() - 12..lines.len()) {
        keep[i] = true;
    }
    for (i, line) in lines.iter().enumerate() {
        if keep.iter().filter(|k| **k).count() >= 60 {
            break;
        }
        let lower = line.to_lowercase();
        if MARKERS.iter().any(|marker| lower.contains(marker)) {
            keep[i.saturating_sub(2)..=(i + 2).min(lines.len() - 1)].fill(true);
        }
    }
    if keep.iter().all(|k| *k) {
        return content.to_string();
    }
    let mut out = String::new();
    let mut omitted = 0;
    for (line, keep) in lines.iter().zip(&keep) {
        if !keep {
            omitted += 1;
            continue;
        }
        if omitted > 0 {
            out.push_str(&format!("… {omitted}{ELISION_MARKER}\n"));
            omitted = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    if omitted > 0 {
        out.push_str(&format!("… {omitted}{ELISION_MARKER}\n"));
    }
    out.trim_end_matches('\n').to_string()
}

/// 摘要必须带走的事实：改过的文件与失败过的命令。
#[derive(Default)]
struct Coverage {
    missing_changes: Vec<String>,
    missing_failures: Vec<String>,
    changes: usize,
}

impl Coverage {
    fn lost_every_change(&self) -> bool {
        self.changes > 0 && self.missing_changes.len() == self.changes
    }

    fn reason(&self) -> String {
        let mut parts = Vec::new();
        if !self.missing_changes.is_empty() {
            parts.push(format!("changed files not mentioned: {}", self.missing_changes.join(", ")));
        }
        if !self.missing_failures.is_empty() {
            parts.push(format!("failures not mentioned: {}", self.missing_failures.join(", ")));
        }
        parts.join("; ")
    }
}

fn command_signature(args: &Value) -> Option<String> {
    let command = args.get("command").and_then(Value::as_str)?.trim();
    let first = command.lines().next()?;
    let sig = first.split_whitespace().take(2).collect::<Vec<_>>().join(" ");
    (!sig.is_empty()).then_some(sig)
}

fn changed_path(name: &str, args: &Value) -> Option<String> {
    matches!(name, "edit" | "write")
        .then(|| args.get("path").and_then(Value::as_str).map(str::trim))
        .flatten()
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// 摘要必须负责的调用：成功的改文件，或失败的命令。其余交给折叠索引，两者不留缝。
fn coverage_demands(name: &str, args: &Value, failed: bool) -> bool {
    if failed { command_signature(args).is_some() } else { changed_path(name, args).is_some() }
}

/// 对区间内每个工具结果回调 (调用所在下标, 名称, 参数, 结果消息)。
fn for_each_result<'a>(region: &'a [Value], mut f: impl FnMut(usize, &'a str, &'a Value, &'a Value)) {
    let mut calls = std::collections::HashMap::new();
    for (i, m) in region.iter().enumerate() {
        for (id, name, args) in tool_calls(m) {
            calls.insert(id, (i, name, args));
        }
        if let Some(&(at, name, args)) = m.get("toolCallId").and_then(Value::as_str).and_then(|id| calls.get(id)) {
            f(at, name, args, m);
        }
    }
}

fn fold_coverage(fold: &[Value], digest: &str) -> Coverage {
    let (mut changes, mut failures) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
    for_each_result(fold, |_, name, args, result| {
        if is_error_result(result) {
            failures.extend(command_signature(args));
        } else {
            changes.extend(changed_path(name, args));
        }
    });
    let haystack = digest.to_lowercase();
    let mentions_path = |p: &String| {
        let lower = p.to_lowercase();
        let base = lower.rsplit(['/', '\\']).next().unwrap_or_default();
        haystack.contains(&lower) || (!base.is_empty() && haystack.contains(base))
    };
    Coverage {
        changes: changes.len(),
        missing_changes: changes.iter().filter(|p| !mentions_path(p)).cloned().collect(),
        missing_failures: failures.into_iter().filter(|c| !haystack.contains(&c.to_lowercase())).collect(),
    }
}

/// 摘要漏掉的改动/失败由宿主补一段清单，不再二次调用模型。
fn backstop(digest: String, coverage: &Coverage) -> String {
    if coverage.missing_changes.is_empty() && coverage.missing_failures.is_empty() {
        return digest;
    }
    let mut block = format!("{BACKSTOP_HEADING}\nThe summary above does not cover these, and the fold index omits them. The full transcript still holds them.\n");
    for (label, facts) in [("changed", &coverage.missing_changes), ("failed", &coverage.missing_failures)] {
        for fact in facts.iter().take(20) {
            block.push_str(&format!("- {label}: {fact}\n"));
        }
        if facts.len() > 20 {
            block.push_str(&format!("- ({} more {label}, not listed)\n", facts.len() - 20));
        }
    }
    format!("{}\n\n{}", digest.trim_end(), block.trim_end())
}

struct IndexEntry {
    line: String,
    /// 越小越先留：丢掉的用户原话 < 失败调用 < 命令 < 读取。
    rank: u8,
}

fn build_fold_index(region: &[Value], keep: &[bool], origin: impl Fn(usize) -> Option<usize>) -> Vec<IndexEntry> {
    let address = |i: usize| origin(i).map(|p| format!("#{p} ")).unwrap_or_default();
    let mut entries: Vec<(usize, IndexEntry)> = Vec::new();
    for (i, m) in region.iter().enumerate() {
        if role(m) == "user" && !keep[i] && !is_summary(m) {
            let flat = text_content(&m["content"]).split_whitespace().collect::<Vec<_>>().join(" ");
            let opening: String = flat.chars().take(60).collect();
            let ellipsis = if flat.chars().count() > 60 { "…" } else { "" };
            entries.push((i, IndexEntry { line: format!("- {}you  {:?}  (summary only)", address(i), format!("{opening}{ellipsis}")), rank: 0 }));
        }
    }
    for_each_result(region, |at, name, args, result| {
        let failed = is_error_result(result);
        if coverage_demands(name, args, failed) {
            return;
        }
        let (subject, mut rank) = if let Some(command) = args.get("command").and_then(Value::as_str) {
            (command.lines().next().unwrap_or_default().to_string(), 2)
        } else if let Some(path) = args.get("path").and_then(Value::as_str) {
            (path.to_string(), 3)
        } else {
            (summarize_args(args), 3)
        };
        let note = if failed {
            rank = 1;
            "  (failed)"
        } else {
            ""
        };
        entries.push((at, IndexEntry { line: format!("- {}{name}  {subject}{note}", address(at)), rank }));
    });
    entries.sort_by_key(|(i, _)| *i);
    entries.into_iter().map(|(_, e)| e).collect()
}

/// 拆出摘要里宿主写的索引段，摘要器永远看不到它（重写地址只会出错）。
fn strip_fold_index(fold: &[Value]) -> (Vec<Value>, String) {
    let mut carried = Vec::new();
    let out = fold
        .iter()
        .map(|m| {
            if !is_summary(m) {
                return m.clone();
            }
            let text = text_content(&m["content"]);
            let Some(at) = text.find(INDEX_HEADING) else { return m.clone() };
            carried.push(text[at..].trim_end_matches(TAG_CLOSE).trim().to_string());
            with_text(m, format!("{}\n{TAG_CLOSE}", text[..at].trim_end()))
        })
        .collect();
    (out, carried.join("\n"))
}

/// 新索引按排名装入预算，再接在旧索引之后；超预算从最旧的行开始丢。
fn attach_fold_index(digest: &str, prior: &str, entries: &[IndexEntry], budget: u64, tokens: impl Fn(&str) -> u64) -> String {
    let mut spent = tokens(INDEX_HEADING);
    let mut chosen = vec![false; entries.len()];
    for rank in 0..=3 {
        for (i, entry) in entries.iter().enumerate().filter(|(_, e)| e.rank == rank) {
            let cost = tokens(&entry.line);
            if spent + cost <= budget {
                spent += cost;
                chosen[i] = true;
            }
        }
    }
    let mut lines: Vec<String> = prior
        .lines()
        .map(str::trim_end)
        .filter(|l| l.starts_with("- ") && !l.starts_with("- ("))
        .map(str::to_string)
        .collect();
    lines.extend(entries.iter().zip(&chosen).filter(|(_, c)| **c).map(|(e, _)| e.line.clone()));
    let mut spent = tokens(INDEX_HEADING);
    let mut first = lines.len();
    while first > 0 {
        let cost = tokens(&lines[first - 1]);
        if spent + cost > budget {
            break;
        }
        spent += cost;
        first -= 1;
    }
    if first == lines.len() {
        return digest.to_string();
    }
    let mut section = format!("{INDEX_HEADING}\n");
    if first > 0 {
        section.push_str(&format!("- ({first} older entries dropped; recall with a query still finds them)\n"));
    }
    section.push_str(&lines[first..].join("\n"));
    format!("{}\n\n{section}", digest.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewritten_view_persists_and_later_history_still_appends() {
        let mut ctx = ContextWindow::ephemeral(4096, 0);
        let mut canonical: Vec<Value> = (0..4).map(|i| user(&format!("m{i}"))).collect();
        assert!(!ctx.rewrite_view(&canonical, |v| Some(v.to_vec())), "no change, no projection");
        assert!(!ctx.rewrite_view(&canonical, |_| None));
        assert!(ctx.rewrite_view(&canonical, |v| { let mut v = v.to_vec(); v[1] = user("trimmed"); Some(v) }));
        canonical.push(user("m4"));
        let view = ctx.view(&canonical);
        assert_eq!(view.len(), 5);
        assert_eq!((view[1].clone(), view[2].clone(), view[4].clone()), (user("trimmed"), user("m2"), user("m4")));
        assert_eq!(ctx.view_index(&canonical, 2), Some(2), "only the rewritten prefix is covered");
        // A second rewrite inside the existing projection keeps its coverage.
        assert!(ctx.rewrite_view(&canonical, |v| { let mut v = v.to_vec(); v[0] = user("t0"); Some(v) }));
        assert_eq!(ctx.view(&canonical)[..3], [user("t0"), user("trimmed"), user("m2")]);
        assert_eq!(ctx.view_index(&canonical, 2), Some(2));
    }

    fn user(text: &str) -> Value {
        json!({ "role": "user", "content": [{ "type": "text", "text": text }] })
    }

    fn call(id: &str, name: &str, args: Value) -> Value {
        json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": id, "name": name, "arguments": args }] })
    }

    fn result(id: &str, name: &str, text: &str, error: bool) -> Value {
        json!({ "role": "toolResult", "toolCallId": id, "toolName": name, "content": [{ "type": "text", "text": text }], "isError": error })
    }

    #[test]
    fn token_budget_counts_parts_and_anchors_to_actual_usage() {
        use crate::lyra::history::estimate_text_tokens;
        let text = "Hello, world! 中文代码：fn main() { println!(\"你好\"); } <|endoftext|>";
        let args = json!({ "path": "src/中文.rs", "content": text });
        let tools = json!([{ "name": "write", "description": "写代码", "parameters": {
            "type": "object", "properties": { "path": { "type": "string" }, "content": { "type": "string" } }
        }}]);
        let messages = vec![user(text), call("c1", "write", args.clone()), result("c1", "write", "完成 done", false),
            json!({ "role": "user", "content": [{ "type": "image", "data": "not-tokenized-as-base64" }] })];
        let counter = TokenCounter;
        assert_eq!(counter.count_text("Hello, world!"), 4);
        assert_eq!(counter.count_text("中文"), 2);
        assert_eq!(counter.count_text(""), 0);
        assert_eq!(estimate_text_tokens(text), counter.count_text(text));
        let mut ctx = ContextWindow::ephemeral(4096, 999);
        ctx.configure_tokens(counter.clone(), "系统 system", &tools, 512);
        let parts = ctx.token_breakdown(&messages);
        assert_eq!(parts.system, counter.count_text("系统 system"));
        assert_eq!(parts.tools, counter.count_json(&tools));
        assert_eq!(parts.messages_text, counter.count_text(text));
        assert_eq!(parts.tool_arguments, counter.count_json(&args));
        assert_eq!(parts.tool_results, counter.count_text("完成 done"));
        assert_eq!(parts.images, IMAGE_TOKENS);
        assert_eq!(parts.image_count, 1);
        assert_eq!(parts.message_count, 4);
        let raw = parts.total();
        assert_eq!(ctx.prompt_tokens(&messages), raw);
        let report: Value = serde_json::from_str(&ctx.budget_report(&messages)).unwrap();
        assert_eq!(report["uncalibratedPromptTokens"], raw);
        assert_eq!(report["estimatedTotalWithReserves"], raw + 512 + PROTOCOL_RESERVE);
        assert_eq!(report["foldTrigger"], 3328);
        assert_eq!(report["tokenizer"]["promptExact"], false);
        assert!(report["lastActualUsage"].is_null());
        // 旧账本（BPE 时代的校准）不误用；usage 中 cache 只计入 prompt 一次。
        ctx.state = serde_json::from_value(json!({ "version": 1, "calibration": 4.0, "recallSpent": 7 })).unwrap();
        assert_eq!(ctx.prompt_tokens(&messages), raw);
        ctx.record_usage(&messages, &json!({ "input": raw - 30, "cacheRead": 20, "cacheWrite": 10, "output": 8, "reasoning": 3 }));
        let report: Value = serde_json::from_str(&ctx.budget_report(&messages)).unwrap();
        assert_eq!(report["lastActualUsage"]["promptTokens"], raw);
        assert_eq!(report["lastActualUsage"]["usage"]["reasoning"], 3);
        assert_eq!(report["recall"]["spent"], 7);
        ctx.state = serde_json::from_str(&serde_json::to_string(&ctx.state).unwrap()).unwrap();
        // 实测 = 3×估算：已发送前缀直接用实测，新增消息按校准系数估算。
        ctx.calibrate(&messages, raw * 3);
        assert_eq!(ctx.prompt_tokens(&messages), raw * 3);
        let report: Value = serde_json::from_str(&ctx.budget_report(&messages)).unwrap();
        assert_eq!(report["calibrationAdjustment"], raw * 2);
        assert_eq!(report["anchoredToActualUsage"], true);
        let mut next = messages.clone();
        next.push(user("next turn"));
        let tail = ctx.token_breakdown(&next).total() - raw;
        assert_eq!(ctx.prompt_tokens(&next), raw * 3 + tail * 3);
        // 前缀被改写（折叠/去 reasoning）时锚点失效，回退为校准后的整体估算。
        assert_eq!(ctx.prompt_tokens(&next[1..]), ctx.token_breakdown(&next[1..]).total() * 3);
        // 系统提示变化只调整固定开销差额，不丢弃实测。
        ctx.configure_tokens(counter.clone(), "系统 system 加长了", &tools, 512);
        let grown = (counter.count_text("系统 system 加长了") - counter.count_text("系统 system")) * 3;
        assert_eq!(ctx.prompt_tokens(&messages), raw * 3 + grown);
        let mut screenshots = result("c1", "chrome", "page", false);
        screenshots["details"] = json!({ "images": [{ "path": "/does/not/need/to/exist.png" }, { "imageId": "repeat-without-image" }] });
        let parts = TokenCounter::default().count_messages(&[screenshots]);
        assert_eq!((parts.image_count, parts.images), (1, IMAGE_TOKENS));
        let unknown_media = |data: &str| json!({ "role": "user", "content": [{ "type": "input_audio", "data": data }] });
        let counter = TokenCounter::default();
        assert_eq!(counter.count_messages(&[unknown_media("tiny")]).total(), counter.count_messages(&[unknown_media(&"abc".repeat(10000))]).total());
        assert_eq!(counter.count_messages(&[unknown_media("data")]).unknown_part_count, 1);
        // 折叠预算与校准基线必须是同一份去 reasoning/修复配对/合并 user 后的视图。
        let canonical = vec![user("old"), json!({ "role": "assistant", "content": [
            { "type": "thinking", "thinking": "private reasoning" }, { "type": "text", "text": "answer" }
        ] }), user("next"), user("steer")];
        let mut ctx = ContextWindow::ephemeral(4096, 0);
        ctx.configure_tokens(counter, "system", &Value::Null, 512);
        ctx.set_prompt_policy(2, true);
        let sent = ctx.prompt_view(&canonical);
        assert_eq!(sent, coalesce_user_runs(History::for_prompt(&canonical, 2, true)));
        assert_eq!(ctx.token_breakdown(&sent).reasoning, 0);
        assert_eq!(sent.len(), 3);
        let raw = ctx.prompt_tokens(&sent);
        ctx.record_usage(&sent, &json!({ "input": raw - 3, "cacheRead": 2, "cacheWrite": 1, "output": 9, "reasoning": 5 }));
        let report: Value = serde_json::from_str(&ctx.budget_report(&canonical)).unwrap();
        assert_eq!(report["uncalibratedPromptTokens"], raw);
        assert_eq!(report["lastActualUsage"]["localUncalibratedPromptTokens"], raw);
        assert_eq!(report["calibrationFactor"], 1.0);
        // 系统提示变化不使校准作废；只有词表/去 reasoning 策略变化才作废。
        ctx.calibrate(&sent, raw * 2);
        ctx.configure_tokens(TokenCounter::default(), "system prompt changed: 2030-01-01 /tmp", &Value::Null, 512);
        assert_eq!(ctx.calibration(), ctx.state.calibration);
        assert!(ctx.calibration() > 1.0);
        ctx.state.calibration = 1.0;
        // 输出预留封顶为窗口的一定比例：max_output_tokens 接近窗口不应卡死自动折叠。
        for reserved in [4096 - PROTOCOL_RESERVE, 8192, u64::MAX] {
            ctx.configure_tokens(TokenCounter::default(), "system", &Value::Null, reserved);
            assert_eq!(ctx.trigger_tokens(), 4096 - 1024 - PROTOCOL_RESERVE);
            let report: Value = serde_json::from_str(&ctx.budget_report(&canonical)).unwrap();
            assert_eq!(report["inputBudgetExhausted"], false);
            assert_eq!(report["automaticCompaction"], true);
            assert_eq!(report["reservedOutputTokens"], 1024);
        }
        // 只有固定提示本身占满窗口才算耗尽。
        let exhausted = ContextWindow::ephemeral(4096, 4000);
        assert!(exhausted.input_budget_exhausted() && !exhausted.should_compact(&canonical));
    }

    #[tokio::test]
    async fn compaction_keeps_each_screenshot_streams_latest_state_and_call_pair() {
        let root = tempfile::tempdir().unwrap();
        let shot = |id: &str, tool: &str, stream: &str, group: &str| {
            let mut m = result(id,tool,"observed",false);
            m["details"] = json!({"snapshotId":id,"images":[{"imageId":stream,"frameStream":stream,"path":format!("{stream}-{group}.jpg")}]});
            m
        };
        for (session, streams) in [
            ("screens", [("jianlai","monitor-1"),("jianlai","monitor-2"),("jianlai","monitor-3")]),
            ("tabs", [("chrome","tab-a:document-0"),("chrome","tab-a:document-1"),("chrome","tab-b:document-0")]),
            ("tools", [("chrome","shared"),("webview","shared"),("jianlai","shared")]),
        ] {
            let mut canonical = vec![user("observe screenshot streams")];
            for (id,(tool,stream),group) in [
                ("oldest",streams[0],"a"),("second",streams[1],"b"),("third",streams[2],"c"),("mid",streams[0],"d"),("new",streams[0],"e"),
            ] {
                canonical.push(call(id,tool,json!({"operation":"screenshot"})));
                canonical.push(shot(id,tool,stream,group));
            }
            let mut ctx = ContextWindow::load(root.path(),session,20_000,500);
            let keep = ctx.keep_indexes(&canonical[1..7],None,&canonical[7..]);
            assert_eq!(keep,vec![false,false,true,true,true,true],"{session}: tail screenshots supersede only their own stream beyond KEEP_FRAMES and keep the others' callers");
            let filler = "context line\n".repeat(500);
            for i in 0..12 {
                canonical.push(call(&format!("r{i}"),"read",json!({"path":"notes.txt"})));
                canonical.push(result(&format!("r{i}"),"read",&filler,false));
            }
            canonical.push(user("continue"));
            let receipt = ctx.compact(&canonical,None,Trigger::Manual,|_,_|async { Ok("Continue with all screenshot streams.".into()) }).await.unwrap();
            assert_eq!(receipt.status,"applied","{session}: {receipt:?}");
            let view = ctx.prompt_view(&canonical);
            for id in ["second","third","mid","new"] {
                assert_eq!(view.iter().find(|m|m["toolCallId"] == id),canonical.iter().find(|m|m["toolCallId"] == id),"{session}: preserve the complete latest state");
                assert_eq!(view.iter().find(|m|tool_calls(m).any(|(cid,_,_)|cid == id)),canonical.iter().find(|m|tool_calls(m).any(|(cid,_,_)|cid == id)),"{session}: preserve real tool pairs, not synthetic orphan results");
            }
            assert!(!view.iter().any(|m|m["toolCallId"] == "oldest"),"{session}: superseded state is folded");
            let reloaded = ContextWindow::load(root.path(),session,20_000,500);
            assert_eq!(reloaded.prompt_view(&canonical),view,"{session}: screenshot state survives projection persistence");
        }
    }

    #[tokio::test]
    async fn folds_into_projection_with_index_recall_and_reload() {
        let root = std::env::temp_dir().join(format!("lyra-context-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // 真实 BPE 下仍超过触发线；有分词边界，避免长串重复字符的最坏性能。
        let filler = "context line\n".repeat(500);
        let mut canonical = vec![user("fix the parser")];
        canonical.push(call("e1", "edit", json!({ "path": "src/parser.rs", "edits": [] })));
        canonical.push(result("e1", "edit", "ok", false));
        let failing = (0..40).map(|i| if i == 20 { "test parse FAILED".to_string() } else { format!("line {i}") }).collect::<Vec<_>>().join("\n");
        canonical.push(call("b1", "bash", json!({ "command": "cargo test parse" })));
        canonical.push(result("b1", "bash", &failing, true));
        canonical.push(call("e2", "write", json!({ "path": "src/lexer.rs", "content": "" })));
        canonical.push(result("e2", "write", "ok", false));
        for i in 0..12 {
            if i == 6 {
                canonical.push(user("also check the lexer"));
            }
            canonical.push(call(&format!("r{i}"), "read", json!({ "path": format!("src/f{i}.rs") })));
            canonical.push(result(&format!("r{i}"), "read", &filler, false));
        }
        canonical.push(user("keep going"));
        canonical.push(json!({ "role": "assistant", "content": [{ "type": "text", "text": "done" }] }));

        let mut exhausted = ContextWindow::ephemeral(4096, 4000);
        let rejected = exhausted.compact(&canonical, None, Trigger::Manual, |_, _| async { panic!("must not summarize with no input budget") }).await.unwrap();
        assert_eq!(rejected.status, "rejected");
        // provider 已报溢出时仍要尝试折叠，不能因本地预算耗尽拒绝。
        let overflow = exhausted.compact(&canonical, None, Trigger::Overflow, |_, _| async { Err("offline".to_string()) }).await.unwrap();
        assert_ne!(overflow.reason, rejected.reason, "{overflow:?}");
        let mut ctx = ContextWindow::load(&root, "s", 20_000, 500);
        ctx.set_prompt_policy(canonical.len() - 2, true);
        assert!(ctx.should_compact(&canonical));
        let source_tokens = ctx.prompt_tokens(&ctx.prompt_view(&canonical));
        let receipt = ctx
            .compact(&canonical, Some(canonical.len() - 2), Trigger::Auto, |system, transcript| async move {
                assert!(system.contains("## Pending & next step"));
                assert!(transcript.contains("[assistant calls read] {path} (1 keys)"));
                Ok("## Goal\n- fix the parser\n## Files & code\n- src/parser.rs edited".to_string())
            })
            .await
            .unwrap();
        assert_eq!(receipt.status, "applied", "{receipt:?}");
        assert_eq!(receipt.source_tokens, source_tokens);
        assert_eq!(receipt.result_tokens, ctx.prompt_tokens(&ctx.prompt_view(&canonical)));
        let view = ctx.view(&canonical);
        assert!(view.len() < canonical.len());
        assert_eq!(view[0], canonical[0], "small first user turn stays pinned");
        let digest = text_content(&view[1]["content"]);
        assert!(digest.starts_with(TAG_OPEN) && digest.contains(INDEX_HEADING) && digest.contains("#7 read  src/f0.rs"));
        // 摘要没提改过的文件和失败的命令 → 宿主补清单；失败结果连同调用组被保留并裁剪。
        assert!(digest.contains("- changed: src/lexer.rs") && !digest.contains("- changed: src/parser.rs"));
        assert!(view.iter().any(|m| text_content(&m["content"]).contains("test parse FAILED") && text_content(&m["content"]).contains(ELISION_MARKER)));
        assert!(ctx.prompt_tokens(&view) < ctx.trigger_tokens());
        // 用户原话原样保留在视图里；相邻的用户消息出站时合并。
        assert!(view.iter().any(|m| text_content(&m["content"]) == "also check the lexer"));
        assert!(coalesce_user_runs(view.clone()).len() < view.len());

        let recalled = ctx.recall(&canonical, &[7], None).unwrap();
        assert!(recalled.contains("src/f0.rs") && recalled.contains(&filler[..100]));
        assert!(ctx.recall(&canonical, &[], Some("f3.rs")).unwrap().contains("#13"));
        assert!(ctx.recall(&canonical, &[canonical.len() - 1], None).is_err());

        // 规范历史继续追加，重新加载后投影仍有效。
        canonical.push(user("next"));
        let reloaded = ContextWindow::load(&root, "s", 20_000, 500);
        assert_eq!(reloaded.view(&canonical).len(), view.len() + 1);
        // 规范前缀变化（例如回滚）→ 投影失效，回退规范历史。
        canonical[0] = user("different");
        assert_eq!(reloaded.view(&canonical).len(), canonical.len());
        let _ = std::fs::remove_dir_all(&root);
    }
}
