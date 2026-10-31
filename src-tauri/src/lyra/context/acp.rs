//! ACP（Active Context Pruning）的 Rust 实现，机制取自 ranxianglei/billion-context 的 acp-kernel：
//! 由模型决定何时、压缩哪一段，摘要也由模型撰写；宿主只负责引用编号、区间校验、块账本、提示（nudge）
//! 与兜底截断。规范历史仍只追加；块按规范下标记账，渲染时叠加在 Lyra 旧投影（若有）之后。
//! 与原 acp-kernel 的差异（shortcut）：kernel 的 young/old 分代只是没人读取的标签，未实现；旧块的再压缩走 tier 机制（bN 作区间端点）；
//! 用户消息在块里原文保留（Lyra 一贯做法），不由模型转述。

use super::*;
use std::collections::HashSet;
use std::future::Future;

/// 末尾这么多个单元不可压缩（正在进行的工作）。
const PRESERVE_UNITS: usize = 2;
// 提示节奏取自 acp-kernel 的 decideNudge，数值同 kernel 默认；kernel 的按块数触发（tier2Trigger/tier3Trigger）默认关闭，这里不实现。
/// 模型没有声明上下文窗口时按这个值算。
pub(super) const DEFAULT_WINDOW: u64 = 200_000;
const NUDGE_MIN: f64 = 0.45;
const NUDGE_MAX: f64 = 0.75;
const GROWTH_TOKENS: u64 = 50_000;
const MIN_GROWTH_FLOOR: u64 = 20_000;
const MIN_PRESSURE_BENEFIT: u64 = 5_000;
const GROWTH_FLOOR_RATIO: f64 = 0.45;
const TIER_MASS_MULTIPLIER: f64 = 1.5;
const MAX_TIER: u8 = 3;
const EMERGENCY_RATIO: f64 = 0.92;
const EMERGENCY_TARGET: f64 = 0.75;
const MIN_RUN_TOKENS: u64 = 800;
const MAX_RUNS_SHOWN: usize = 6;
const MAX_RANGES: usize = 8;
const KEPT_USER_CHARS: usize = 1500;
const MIN_SUMMARY_CHARS: usize = 40;
/// 块（摘要 + 保留的用户原话）必须不超过被替换内容的这个比例，否则拒绝。
const MAX_BLOCK_RATIO: f64 = 0.7;
const MAX_SEARCH_HITS: usize = 20;

pub const GUIDE: &str = r#"

Context management (ACP): Nova never summarizes your history behind your back; you decide when and what to compress.
- User messages and tool results start with a ref like [m00012]. A compressed section shows up as <acp-block id="b3">.
- When a <context-budget> notice arrives (or acp_status shows pressure), call `compress` on FINISHED work: ranges[].startRef/endRef are the first/last refs of the span; put earlier block ids (bN) in a range to fold old summaries into a bigger one. Never compress work still in progress; the last few messages are protected.
- You write the summary, and it replaces the originals: keep every path, identifier, number, command + outcome, decision and its reason, error + fix, and open TODO you will still need. Be terse (bullets), never invent. User messages inside a range are kept verbatim automatically.
- `decompress` restores a block's originals, `search_context` greps summaries and the stored originals, `acp_status` shows usage and compressible ranges. Compression is cheap and reversible; waiting until the window is full is not."#;

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
pub(super) struct AcpState {
    blocks: Vec<Block>,
    next_id: u32,
    /// 被应急截断的工具结果（规范下标，升序）。
    truncated: Vec<usize>,
    /// 413 后永久省略旧图（渲染时套用 drop_old_tool_images）。
    pub(super) trim_images: bool,
    /// 上次提示时的 prompt tokens；0 = 没有待处理的提示（kernel: lastNudgeShownTokens）。
    nudge_shown: u64,
    /// 增长基线（只随观测值下降）；compress 成功后清零（kernel: lastPerMessageNudgeTokens）。
    baseline: u64,
    /// 每层级最近一次提示时的 tokens（下标 1..=3）。
    shown_by_tier: [u64; 4],
}

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase", default)]
struct Block {
    id: u32,
    /// 规范下标闭区间。
    start: usize,
    end: usize,
    hash: String,
    topic: String,
    summary: String,
    active: bool,
    mechanical: bool,
    /// 1 = 原文压缩；2/3 = 用 bN 作区间端点把下一层的摘要归并（distill / condense）。旧账本缺省按 1。
    tier: u8,
    /// 被本块并入的旧块；decompress 本块时恢复它们。
    children: Vec<u32>,
    at: u64,
}

/// 一个原子单元：用户消息（连同其后纯文本回复），或助手工具调用 + 全部结果（连同其后纯文本回复）。
struct Unit {
    start: usize,
    /// 不含。
    end: usize,
    /// 携带引用标记的消息下标：用户消息 / 首个工具结果。
    ref_at: Option<usize>,
    /// 仍有工具调用缺结果。
    open: bool,
}

enum Ref {
    Msg(usize),
    Block(u32),
}

struct Run {
    start_ref: String,
    end_ref: String,
    tokens: u64,
}

struct Group {
    first: u32,
    last: u32,
    count: usize,
    tokens: u64,
}

fn is_text_assistant(m: &Value) -> bool {
    role(m) == "assistant" && tool_calls(m).next().is_none()
}

fn units(canonical: &[Value], from: usize) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut i = from;
    while i < canonical.len() {
        let m = &canonical[i];
        if role(m) == "user" {
            let mut j = i + 1;
            while j < canonical.len() && is_text_assistant(&canonical[j]) {
                j += 1;
            }
            out.push(Unit { start: i, end: j, ref_at: Some(i), open: false });
            i = j;
        } else if role(m) == "assistant" && tool_calls(m).next().is_some() {
            let ids: HashSet<&str> = tool_calls(m).map(|c| c.0).collect();
            let mut seen = HashSet::new();
            let (mut j, mut first) = (i + 1, None);
            while j < canonical.len() && role(&canonical[j]) == "toolResult" {
                if let Some(id) = canonical[j].get("toolCallId").and_then(Value::as_str) {
                    seen.insert(id);
                }
                first.get_or_insert(j);
                j += 1;
            }
            let open = ids.iter().any(|id| !seen.contains(id));
            if !open {
                while j < canonical.len() && is_text_assistant(&canonical[j]) {
                    j += 1;
                }
            }
            out.push(Unit { start: i, end: j, ref_at: first, open });
            i = j;
        } else {
            out.push(Unit { start: i, end: i + 1, ref_at: None, open: false });
            i += 1;
        }
    }
    out
}

/// 可压缩区间的规范下标上界（不含）：末尾受保护单元与第一个未闭合单元之前。
fn unit_limit(units: &[Unit], len: usize) -> usize {
    let protect = units.len().saturating_sub(PRESERVE_UNITS);
    let k = units.iter().position(|u| u.open).map_or(protect, |o| o.min(protect));
    units.get(k).map_or(len, |u| u.start)
}

fn ref_name(i: usize) -> String {
    format!("m{i:05}")
}

fn parse_ref(s: &str) -> Option<Ref> {
    let s = s.trim().trim_matches(|c| c == '[' || c == ']');
    let split = s.char_indices().nth(1).map_or(s.len(), |(i, _)| i);
    let (head, rest) = s.split_at(split);
    match head.to_ascii_lowercase().as_str() {
        "m" => rest.parse().ok().map(Ref::Msg),
        "b" => rest.parse().ok().map(Ref::Block),
        _ => None,
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    format!("{}…", &s[..floor_char_boundary(s, n)])
}

fn tag_ref(m: &mut Value, i: usize) {
    let tag = format!("[{}] ", ref_name(i));
    match &mut m["content"] {
        Value::String(s) => *s = format!("{tag}{s}"),
        Value::Array(parts) => match parts.first_mut().filter(|p| p["type"] == "text") {
            Some(first) => {
                let text = first["text"].as_str().unwrap_or_default().to_string();
                first["text"] = json!(format!("{tag}{text}"));
            }
            None => parts.insert(0, json!({ "type": "text", "text": tag.trim_end() })),
        },
        _ => {}
    }
}

fn block_message(b: &Block, canonical: &[Value]) -> Value {
    let mut users = String::new();
    for m in &canonical[b.start..=b.end] {
        if role(m) == "user" {
            users.push_str(&format!("- {}\n", clip(&text_content(&m["content"]), KEPT_USER_CHARS)));
        }
    }
    let topic = if b.topic.is_empty() { String::new() } else { format!(" topic=\"{}\"", b.topic.replace('"', "'")) };
    let mut text = format!("<acp-block id=\"b{}\" messages=\"{}\"{topic}>\n{}\n", b.id, b.end - b.start + 1, b.summary);
    if !users.is_empty() {
        text.push_str(&format!("\nUser messages in this section (verbatim):\n{users}"));
    }
    text.push_str(&format!("</acp-block>\n(originals are stored; decompress block \"b{}\" if you need exact details)", b.id));
    json!({ "role": "user", "content": [{ "type": "text", "text": text }], "timestamp": b.at })
}

fn mechanical_summary(msgs: &[Value]) -> String {
    let mut lines = vec!["(compressed mechanically because the context was nearly full; only the outline below survives — decompress this block for details)".to_string()];
    for m in msgs {
        match role(m) {
            "assistant" => {
                let t = text_content(&m["content"]);
                if !t.is_empty() {
                    lines.push(format!("- assistant: {}", clip(&t, 200)));
                }
                for (_, name, args) in tool_calls(m) {
                    lines.push(format!("- called {name} {}", clip(&args.to_string(), 120)));
                }
            }
            "toolResult" if is_error_result(m) => {
                let name = m.get("toolName").and_then(Value::as_str).unwrap_or("tool");
                lines.push(format!("- {name} failed: {}", clip(&text_content(&m["content"]), 160)));
            }
            _ => {}
        }
    }
    lines.truncate(80);
    lines.join("\n")
}

fn snippet(text: &str, needle: &str) -> Option<String> {
    let at = text.to_lowercase().find(needle)?;
    let from = floor_char_boundary(text, at.saturating_sub(120));
    let to = floor_char_boundary(text, (at + needle.len() + 120).min(text.len()));
    Some(text[from..to].trim().replace('\n', " "))
}

impl ContextWindow {
    pub fn enable_acp(&mut self) {
        self.acp_enabled = true;
        self.set_window(self.window);
    }

    pub fn acp_enabled(&self) -> bool {
        self.acp_enabled
    }

    fn acp_from(&self, canonical: &[Value]) -> usize {
        self.valid_projection(canonical).map_or(0, |p| p.covered_count)
    }

    /// 校验通过、互不重叠的活动块（按起点升序）。
    // ponytail: 每次渲染对块覆盖区间做 sha256（O(被压缩历史)）；成为瓶颈时缓存 (len, hash)。
    fn live_blocks(&self, canonical: &[Value], from: usize) -> Vec<&Block> {
        let mut blocks: Vec<&Block> = self.state.acp.blocks.iter()
            .filter(|b| b.active && b.start >= from && b.start <= b.end && b.end < canonical.len()
                && hash_values(&canonical[b.start..=b.end]) == b.hash)
            .collect();
        blocks.sort_by_key(|b| b.start);
        let mut out: Vec<&Block> = Vec::new();
        for b in blocks {
            if out.last().map_or(true, |last| b.start > last.end) {
                out.push(b);
            }
        }
        out
    }

    /// 模型可见视图及「规范下标 → 视图下标」（被折叠的下标指向其块消息）。
    pub(super) fn acp_view(&self, canonical: &[Value]) -> (Vec<Value>, Vec<usize>) {
        let (mut out, from) = match self.valid_projection(canonical) {
            Some(p) => (p.messages.clone(), p.covered_count),
            None => (Vec::new(), 0),
        };
        let mut map = vec![0usize; canonical.len()];
        let blocks = self.live_blocks(canonical, from);
        let refs: HashSet<usize> = units(canonical, from).iter().filter_map(|u| u.ref_at).collect();
        let (mut i, mut next) = (from, 0);
        while i < canonical.len() {
            if let Some(b) = blocks.get(next).filter(|b| b.start == i) {
                map[b.start..=b.end].fill(out.len());
                out.push(block_message(b, canonical));
                i = b.end + 1;
                next += 1;
                continue;
            }
            map[i] = out.len();
            out.push(self.render_message(i, &canonical[i], refs.contains(&i)));
            i += 1;
        }
        if self.state.acp.trim_images {
            // 条数不变；与 rewrite_view 的 413 恢复共用 drop_old_tool_images。
            if let Some(trimmed) = crate::lyra::provider::drop_old_tool_images(&out) {
                out = trimmed;
            }
        }
        (out, map)
    }

    fn render_message(&self, i: usize, m: &Value, has_ref: bool) -> Value {
        let mut out = if role(m) == "toolResult" && self.state.acp.truncated.binary_search(&i).is_ok() {
            sketch_tool_result(m)
        } else {
            m.clone()
        };
        if role(m) == "assistant" {
            // 摘要已在块里，别让 compress 调用的参数再占一份。
            if let Some(parts) = out["content"].as_array_mut() {
                for p in parts.iter_mut().filter(|p| p["type"] == "toolCall" && p["name"] == "compress") {
                    p["arguments"] = json!({ "ranges": "(folded; the summary now lives in its block)" });
                }
            }
        }
        if has_ref {
            tag_ref(&mut out, i);
        }
        out
    }

    pub(super) fn acp_emergency_tokens(&self) -> u64 {
        ((self.window as f64 * EMERGENCY_RATIO) as u64).min(self.hard_ceiling())
    }

    fn acp_runs(&self, canonical: &[Value]) -> Vec<Run> {
        let from = self.acp_from(canonical);
        let units = units(canonical, from);
        let limit = unit_limit(&units, canonical.len());
        let blocks = self.live_blocks(canonical, from);
        let mut runs = Vec::new();
        let mut cur: Vec<&Unit> = Vec::new();
        let finish = |cur: &mut Vec<&Unit>, runs: &mut Vec<Run>| {
            let refd: Vec<&&Unit> = cur.iter().filter(|u| u.ref_at.is_some()).collect();
            if let (Some(first), Some(last)) = (refd.first(), refd.last()) {
                let tokens = self.messages_tokens(&canonical[first.start..last.end]);
                if tokens >= MIN_RUN_TOKENS {
                    runs.push(Run { start_ref: ref_name(first.ref_at.unwrap()), end_ref: ref_name(last.ref_at.unwrap()), tokens });
                }
            }
            cur.clear();
        };
        for u in units.iter().filter(|u| u.start < limit) {
            if u.open || blocks.iter().any(|b| b.start <= u.start && u.start <= b.end) {
                finish(&mut cur, &mut runs);
            } else {
                cur.push(u);
            }
        }
        finish(&mut cur, &mut runs);
        runs
    }

    /// 与 acp-kernel 一致的按层级提示：T1 = 可压缩原文区间，T2/T3 = 把相邻的 1/2 层块摘要精炼成更高一层。
    /// 压力带（≥ 75%）不看增长直接挑待压量最大的层级；平时要求自上次提示后有足够增长。
    pub(super) fn acp_nudge(&mut self, canonical: &[Value]) -> Option<String> {
        if self.window == 0 || self.input_budget_exhausted() {
            return None;
        }
        let used = self.prompt_tokens(&self.prompt_view(canonical));
        let usage = used as f64 / self.window as f64;
        let growth = GROWTH_TOKENS;
        let floor = MIN_GROWTH_FLOOR.max((growth as f64 * GROWTH_FLOOR_RATIO) as u64);
        let min_benefit = MIN_PRESSURE_BENEFIT.max(self.window / 100);

        let mut runs = self.acp_runs(canonical);
        let groups = [self.tier_groups(canonical, 1), self.tier_groups(canonical, 2)];
        let pending = [
            runs.iter().map(|r| r.tokens).sum::<u64>(),
            groups[0].iter().map(|g| g.tokens).sum::<u64>(),
            groups[1].iter().map(|g| g.tokens).sum::<u64>(),
        ];
        let st = &self.state.acp;
        let reference = [st.nudge_shown, st.baseline].into_iter().find(|&n| n > 0).unwrap_or(used);
        let first_sight = st.nudge_shown == 0 && st.baseline == 0 && usage >= NUDGE_MIN && pending.iter().max().copied().unwrap_or(0) >= growth;
        let growth_ready = first_sight || used.saturating_sub(reference) >= floor;
        let cadence = |tier: usize| st.shown_by_tier[tier] == 0 || used.saturating_sub(st.shown_by_tier[tier]) >= floor;
        let mass = (growth as f64 * TIER_MASS_MULTIPLIER) as u64;
        let pick: Option<usize> = if usage < NUDGE_MIN {
            None
        } else if usage >= NUDGE_MAX {
            let (best, best_tokens) = (0..3).fold((None, 0), |acc, i| if pending[i] > acc.1 { (Some(i + 1), pending[i]) } else { acc });
            best.filter(|_| best_tokens >= min_benefit)
        } else if !growth_ready {
            None
        } else if pending[0] >= growth {
            Some(1)
        } else if pending[1] >= mass && pending[1] > pending[0] && cadence(2) {
            Some(2)
        } else if pending[2] >= mass && pending[2] > pending[1] && pending[2] > pending[0] && cadence(3) {
            Some(3)
        } else {
            None
        };

        // 参考线都是低水位：只随观测值下降，增长始终从最近的谷底算起。
        let st = &mut self.state.acp;
        let before = (st.baseline, st.nudge_shown, st.shown_by_tier);
        if st.baseline == 0 || used < st.baseline { st.baseline = used; }
        if st.nudge_shown > 0 && used < st.nudge_shown { st.nudge_shown = used; }
        st.shown_by_tier.iter_mut().for_each(|s| *s = (*s).min(used));
        if let Some(tier) = pick {
            st.nudge_shown = used;
            st.shown_by_tier[tier] = used;
        }
        if before != (st.baseline, st.nudge_shown, st.shown_by_tier) {
            self.save();
        }
        let tier = pick?;

        let level = if usage >= NUDGE_MAX { format!("Context is at {:.0}% ({used}/{} tokens) and the window will overflow soon. Compress NOW, before anything else.", usage * 100.0, self.window) }
            else { format!("Context is at {:.0}% ({used}/{} tokens). Compress finished work before it grows further.", usage * 100.0, self.window) };
        let body = if tier == 1 {
            runs.sort_by(|a, b| b.tokens.cmp(&a.tokens));
            runs.truncate(MAX_RUNS_SHOWN);
            runs.sort_by(|a, b| a.start_ref.cmp(&b.start_ref));
            let list: String = runs.iter().map(|r| format!("- {}..{} (~{} tokens)\n", r.start_ref, r.end_ref, r.tokens)).collect();
            format!("Compressible ranges (finished, outside the protected recent messages):\n{list}Write a self-contained summary (paths, identifiers, decisions, results, pending TODOs).")
        } else {
            let list: String = groups[tier - 2].iter().map(|g| format!("- b{}..b{} ({} tier-{} blocks, ~{} summary tokens)\n", g.first, g.last, g.count, tier - 1, g.tokens)).collect();
            format!("Raw history is already compact; the block summaries themselves are now the bulk. Distill adjacent tier-{} blocks into one tighter tier-{tier} block by using block ids as the range endpoints:\n{list}Keep every fact you still need, drop wording and detail that no longer matters.", tier - 1)
        };
        Some(format!("<context-budget>\n{level}\n{body}\n</context-budget>"))
    }

    /// 相邻的同层级活动块组（中间隔着其它层级的块就断开，至少 2 块才有精炼价值）。
    fn tier_groups(&self, canonical: &[Value], tier: u8) -> Vec<Group> {
        let from = self.acp_from(canonical);
        let mut out = Vec::new();
        let mut cur: Vec<&Block> = Vec::new();
        let mut flush = |cur: &mut Vec<&Block>| {
            if let (true, Some(first), Some(last)) = (cur.len() >= 2, cur.first(), cur.last()) {
                out.push(Group { first: first.id, last: last.id, count: cur.len(), tokens: cur.iter().map(|b| self.text_tokens(&b.summary)).sum() });
            }
            cur.clear();
        };
        for b in self.live_blocks(canonical, from) {
            if b.tier.max(1) == tier { cur.push(b) } else { flush(&mut cur) }
        }
        flush(&mut cur);
        out
    }

    pub fn acp_status_value(&self, canonical: &[Value]) -> Value {
        let used = self.prompt_tokens(&self.prompt_view(canonical));
        let from = self.acp_from(canonical);
        let blocks: Vec<Value> = self.live_blocks(canonical, from).iter().map(|b| json!({
            "id": format!("b{}", b.id), "tier": b.tier.max(1), "messages": b.end - b.start + 1, "topic": b.topic, "mechanical": b.mechanical,
            "summaryTokens": self.text_tokens(&b.summary),
            "originalTokens": self.messages_tokens(&canonical[b.start..=b.end]),
        })).collect();
        let runs: Vec<Value> = self.acp_runs(canonical).iter()
            .map(|r| json!({ "startRef": r.start_ref, "endRef": r.end_ref, "tokens": r.tokens })).collect();
        json!({
            "contextWindow": self.window,
            "usedTokens": used,
            "usedPercent": if self.window > 0 { (used as f64 * 1000.0 / self.window as f64).round() / 10.0 } else { 0.0 },
            "nudgeAtTokens": (self.window as f64 * NUDGE_MIN) as u64,
            "emergencyTruncationAtTokens": self.acp_emergency_tokens(),
            "blocks": blocks,
            "compressibleRanges": runs,
            "truncatedToolResults": self.state.acp.truncated.len(),
        })
    }

    pub fn acp_status(&self, canonical: &[Value]) -> String {
        serde_json::to_string_pretty(&self.acp_status_value(canonical)).unwrap_or_default()
    }

    /// `compress` 工具。每个区间独立校验，成功的先落账；全部失败才报错。
    pub fn acp_compress(&mut self, canonical: &[Value], args: &Value) -> Result<String, String> {
        let ranges: Vec<&Value> = match args.get("ranges").and_then(Value::as_array) {
            Some(ranges) => ranges.iter().collect(),
            None if args.get("summary").is_some() => vec![args],
            None => return Err("compress: give ranges:[{startRef,endRef,summary,topic?}]".into()),
        };
        if ranges.is_empty() || ranges.len() > MAX_RANGES {
            return Err(format!("compress: give 1..{MAX_RANGES} ranges"));
        }
        let before = self.prompt_tokens(&self.prompt_view(canonical));
        let (mut ok, mut notes) = (0, Vec::new());
        for (n, range) in ranges.iter().enumerate() {
            match self.apply_range(canonical, range) {
                Ok(line) => { ok += 1; notes.push(line); }
                Err(error) => notes.push(format!("range {}: rejected — {error}", n + 1)),
            }
        }
        if ok == 0 {
            return Err(notes.join("\n"));
        }
        self.state.generation += 1;
        // 成功压缩后清空基线与各层节奏，让下一轮从新的低点重新开始（kernel 同步做法）。
        let acp = &mut self.state.acp;
        (acp.baseline, acp.nudge_shown, acp.shown_by_tier) = (0, 0, [0; 4]);
        let after = self.prompt_tokens(&self.prompt_view(canonical));
        self.save();
        notes.push(format!("Context now ~{after} tokens (was ~{before})."));
        Ok(notes.join("\n"))
    }

    fn apply_range(&mut self, canonical: &[Value], r: &Value) -> Result<String, String> {
        let get = |keys: &[&str]| keys.iter().find_map(|k| r.get(*k).and_then(Value::as_str)).map(str::trim).filter(|s| !s.is_empty());
        let start_ref = get(&["startRef", "start_ref", "start"]).ok_or("missing startRef")?;
        let end_ref = get(&["endRef", "end_ref", "end"]).ok_or("missing endRef")?;
        let summary = get(&["summary"]).ok_or("missing summary")?;
        if summary.chars().count() < MIN_SUMMARY_CHARS {
            return Err("summary is too short to stand in for the originals".into());
        }
        let from = self.acp_from(canonical);
        let units = units(canonical, from);
        let limit = unit_limit(&units, canonical.len());
        let bounds: Vec<(u32, usize, usize)> = self.live_blocks(canonical, from).iter().map(|b| (b.id, b.start, b.end)).collect();
        let resolve = |reference: &str, is_start: bool| -> Result<usize, String> {
            match parse_ref(reference).ok_or_else(|| format!("unrecognized ref {reference:?}"))? {
                Ref::Block(id) => bounds.iter().find(|b| b.0 == id)
                    .map(|b| if is_start { b.1 } else { b.2 })
                    .ok_or_else(|| format!("block b{id} is not active")),
                Ref::Msg(n) => units.iter().find(|u| u.ref_at == Some(n))
                    .map(|u| if is_start { u.start } else { u.end - 1 })
                    .ok_or_else(|| format!("unknown ref {}", ref_name(n))),
            }
        };
        let (mut s, mut e) = (resolve(start_ref, true)?, resolve(end_ref, false)?);
        if s > e {
            return Err("startRef is after endRef".into());
        }
        // 与活动块相交就整块并入（成为 children）。
        let mut children: Vec<u32> = Vec::new();
        loop {
            let mut grew = false;
            for b in &bounds {
                if !children.contains(&b.0) && b.1 <= e && s <= b.2 {
                    (s, e) = (s.min(b.1), e.max(b.2));
                    children.push(b.0);
                    grew = true;
                }
            }
            if !grew { break; }
        }
        if s < from {
            return Err("range reaches into the earlier legacy-folded history".into());
        }
        if e >= limit {
            return Err("range reaches the protected recent messages or an unfinished tool call; end it earlier".into());
        }
        if role(&canonical[s]) == "toolResult" || closed_prefix_end(&canonical[s..=e]) != e - s + 1 {
            return Err("range would split a tool call from its result".into());
        }
        let boundary = matches!(parse_ref(start_ref), Some(Ref::Block(_))) || matches!(parse_ref(end_ref), Some(Ref::Block(_)));
        let tier = if boundary {
            let top = self.state.acp.blocks.iter().filter(|b| children.contains(&b.id)).map(|b| b.tier.max(1)).max().unwrap_or(0);
            (top + 1).min(MAX_TIER)
        } else {
            1
        };
        let block = Block {
            id: self.state.acp.next_id + 1, start: s, end: e, hash: hash_values(&canonical[s..=e]),
            topic: get(&["topic"]).unwrap_or_default().to_string(), summary: summary.to_string(),
            active: true, children, tier, at: now_ms(), ..Block::default()
        };
        // 当前可见内容 = 原文 − 已并入旧块多占的部分（旧块以其渲染计）。
        let mut current = self.messages_tokens(&canonical[s..=e]);
        for child in self.state.acp.blocks.iter().filter(|b| block.children.contains(&b.id)) {
            let rendered = self.messages_tokens(&[block_message(child, canonical)]);
            current = current.saturating_sub(self.messages_tokens(&canonical[child.start..=child.end])).saturating_add(rendered);
        }
        let new = self.messages_tokens(&[block_message(&block, canonical)]);
        if new as f64 > current as f64 * MAX_BLOCK_RATIO {
            return Err(format!("the block (~{new} tokens incl. kept user messages) is not much smaller than the ~{current} tokens it replaces; write a denser summary or take a larger range"));
        }
        self.commit_block(block);
        Ok(format!("Compressed {} messages into b{} (~{current} → ~{new} tokens).", e - s + 1, self.state.acp.next_id))
    }

    fn commit_block(&mut self, block: Block) {
        let acp = &mut self.state.acp;
        for b in acp.blocks.iter_mut().filter(|b| block.children.contains(&b.id)) {
            b.active = false;
        }
        acp.next_id = block.id;
        acp.blocks.push(block);
    }

    /// `decompress` 工具：恢复块原文，并重新激活它并入过的旧块。
    pub fn acp_decompress(&mut self, canonical: &[Value], args: &Value) -> Result<String, String> {
        let value = ["block", "blockId", "id"].iter().find_map(|k| args.get(*k)).ok_or("decompress: give block, e.g. \"b3\"")?;
        let id = match (value.as_u64(), value.as_str().and_then(parse_ref)) {
            (Some(n), _) => n as u32,
            (_, Some(Ref::Block(id))) => id,
            _ => return Err("decompress: block must look like \"b3\"".into()),
        };
        let from = self.acp_from(canonical);
        let live = self.live_blocks(canonical, from);
        let Some(block) = live.iter().find(|b| b.id == id) else {
            return Err(format!("decompress: b{id} is not active (already restored, or merged into a larger block — decompress that one)"));
        };
        let (children, tokens) = (block.children.clone(), self.messages_tokens(&canonical[block.start..=block.end]));
        let count = block.end - block.start + 1;
        for b in self.state.acp.blocks.iter_mut() {
            if b.id == id { b.active = false; }
            if children.contains(&b.id) { b.active = true; }
        }
        self.state.generation += 1;
        self.save();
        let nested = if children.is_empty() { String::new() } else { format!(" Its earlier blocks are active again: {}.", children.iter().map(|c| format!("b{c}")).collect::<Vec<_>>().join(", ")) };
        Ok(format!("Restored b{id}: {count} messages (~{tokens} tokens) are visible again.{nested}"))
    }

    /// `search_context` 工具：在活动块的摘要与被压缩的原文里找关键词。
    pub fn acp_search(&self, canonical: &[Value], args: &Value) -> Result<String, String> {
        let needle = args.get("query").and_then(Value::as_str).map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty())
            .ok_or("search_context: give a query")?;
        let from = self.acp_from(canonical);
        let mut out = String::new();
        let mut hits = 0;
        'blocks: for b in self.live_blocks(canonical, from) {
            if let Some(s) = snippet(&format!("{} {}", b.topic, b.summary), &needle) {
                out.push_str(&format!("b{} (summary): …{s}…\n", b.id));
                hits += 1;
            }
            for (k, m) in canonical[b.start..=b.end].iter().enumerate() {
                if hits >= MAX_SEARCH_HITS {
                    out.push_str("(more matches not shown; narrow the query)\n");
                    break 'blocks;
                }
                if let Some(s) = snippet(&render_transcript(std::slice::from_ref(m), true), &needle) {
                    out.push_str(&format!("b{} (original #{}): …{s}…\n", b.id, b.start + k));
                    hits += 1;
                }
            }
        }
        if hits == 0 {
            return Err(format!("search_context: nothing in compressed blocks matches {needle:?}"));
        }
        Ok(out)
    }

    /// 兜底（取代旧折叠，仅在逼近/超出窗口时）：先把旧的长工具结果换成头尾草图，还不够就把最老的
    /// 一段已完成单元压成块。对齐 billion-context 的 preflight：块摘要优先交给 `summarize`（独立
    /// 无工具的摘要请求，失败/超时/空摘要则退回机械轮廓，即官方的 deterministicDigest 定位）。
    /// 模型主动 compress 才是常态路径。
    pub(super) async fn acp_emergency<F, Fut>(&mut self, canonical: &[Value], trigger: Trigger, summarize: F) -> Receipt
    where
        F: FnOnce(String, String) -> Fut,
        Fut: Future<Output = Result<String, String>>,
    {
        if self.window == 0 {
            return self.receipt("noop", trigger, "no context window declared");
        }
        let overflow = trigger == Trigger::Overflow;
        if self.input_budget_exhausted() && !overflow {
            return self.receipt("rejected", trigger, "fixed system prompt/tools exhaust the context window");
        }
        let used = self.prompt_tokens(&self.prompt_view(canonical));
        let mut need = used.saturating_sub((self.window as f64 * EMERGENCY_TARGET) as u64);
        if overflow {
            need = need.max(used / 4);
        }
        if need == 0 {
            return self.receipt("noop", trigger, "below target");
        }
        let from = self.acp_from(canonical);
        let units = units(canonical, from);
        let limit = unit_limit(&units, canonical.len());
        let covered: Vec<(usize, usize)> = self.live_blocks(canonical, from).iter().map(|b| (b.start, b.end)).collect();
        let is_covered = |i: usize| covered.iter().any(|c| c.0 <= i && i <= c.1);

        let mut cands: Vec<(usize, u64)> = (from..limit)
            .filter(|&i| role(&canonical[i]) == "toolResult" && !is_covered(i) && self.state.acp.truncated.binary_search(&i).is_err())
            .filter_map(|i| {
                let sketch = sketch_tool_result(&canonical[i]);
                (sketch != canonical[i]).then(|| (i, self.messages_tokens(std::slice::from_ref(&canonical[i])).saturating_sub(self.messages_tokens(&[sketch]))))
            })
            .collect();
        cands.sort_by(|a, b| b.1.cmp(&a.1));
        let mut freed = 0;
        let mut cut = Vec::new();
        for (i, saved) in cands {
            if freed >= need { break; }
            freed += saved;
            cut.push(i);
        }
        self.state.acp.truncated.extend(&cut);
        self.state.acp.truncated.sort_unstable();

        let mut folded = 0;
        let mut llm = false;
        if freed < need {
            let mut stretch: Vec<&Unit> = Vec::new();
            for u in units.iter().filter(|u| u.start < limit) {
                if u.open || is_covered(u.start) {
                    if stretch.is_empty() { continue; }
                    break;
                }
                stretch.push(u);
                if self.messages_tokens(&canonical[stretch[0].start..u.end]) >= need - freed { break; }
            }
            if let (Some(first), Some(last)) = (stretch.first(), stretch.last()) {
                let (s, e) = (first.start, last.end - 1);
                // 先让摘要模型写这段（preflight）；它不可用再用机械轮廓。
                let summary = match self.summarize_fold(&canonical[s..=e], summarize).await {
                    Ok(text) if !text.trim().is_empty() => {
                        llm = true;
                        text.trim().to_string()
                    }
                    outcome => {
                        let cause = outcome.err().unwrap_or_else(|| "empty summary".into());
                        eprintln!("lyra: emergency summary unavailable ({cause}); folded mechanically");
                        mechanical_summary(&canonical[s..=e])
                    }
                };
                let block = Block {
                    id: self.state.acp.next_id + 1, start: s, end: e, hash: hash_values(&canonical[s..=e]),
                    summary, active: true, mechanical: !llm, tier: 1, at: now_ms(), ..Block::default()
                };
                self.commit_block(block);
                folded = e - s + 1;
            }
        }
        if cut.is_empty() && folded == 0 {
            return self.receipt("rejected", trigger, "no finished tool output or older messages outside the protected tail to release");
        }
        self.state.generation += 1;
        let after = self.prompt_tokens(&self.prompt_view(canonical));
        let mut receipt = self.receipt("applied", trigger, format!("truncated {} tool result(s), compressed {folded} message(s)", cut.len()));
        receipt.source_tokens = used;
        receipt.result_tokens = after;
        receipt.saved_tokens = used.saturating_sub(after);
        receipt.folded_messages = folded;
        receipt.degraded = folded > 0 && !llm;
        self.state.last_receipt = Some(receipt.clone());
        self.save();
        receipt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(t: &str) -> Value { json!({ "role": "user", "content": [{ "type": "text", "text": t }] }) }
    fn call(id: &str, name: &str) -> Value {
        json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": id, "name": name, "arguments": { "path": "a.rs" } }] })
    }
    fn said(t: &str) -> Value { json!({ "role": "assistant", "content": [{ "type": "text", "text": t }] }) }
    fn result(id: &str, t: &str) -> Value {
        json!({ "role": "toolResult", "toolCallId": id, "toolName": "read", "content": [{ "type": "text", "text": t }], "isError": false })
    }
    fn big() -> String { "some line of output\n".repeat(400) }

    /// 0 user | 1..2 t1 | 3..4 t2 | 5..6 t3 | 7..8 t4 | 9 user
    fn convo() -> Vec<Value> {
        vec![user("fix the bug"), call("a", "read"), result("a", &big()), call("b", "read"), result("b", &big()),
            call("c", "read"), result("c", &big()), call("d", "read"), result("d", &big()), said("done reading"), user("now what")]
    }

    fn ctx(window: u64) -> ContextWindow {
        let mut c = ContextWindow::ephemeral(window, 0);
        c.enable_acp();
        c
    }

    const SUMMARY: &str = "Read a.rs three times; the bug is in parse(): off-by-one at line 42, fix pending.";

    #[test]
    fn refs_are_stable_and_compress_replaces_range_with_block() {
        let canon = convo();
        let mut c = ctx(100_000);
        let before = c.prompt_tokens(&c.prompt_view(&canon));
        let text = c.prompt_view(&canon)[2]["content"][0]["text"].as_str().unwrap().to_string();
        assert!(text.starts_with("[m00002] "), "{text}");

        let msg = c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00004", "summary": SUMMARY, "topic": "reading" }] })).unwrap();
        assert!(msg.contains("b1"), "{msg}");
        let view = c.prompt_view(&canon);
        assert!(c.prompt_tokens(&view) < before);
        let block = view.iter().find(|m| text_content(&m["content"]).contains("<acp-block id=\"b1\"")).expect("block message");
        assert!(text_content(&block["content"]).contains("off-by-one"));
        // 受保护的尾部和后面的工具对原样保留，配对完整。
        assert_eq!(closed_prefix_end(&canon[1..]), canon.len() - 1);
        assert!(view.iter().any(|m| text_content(&m["content"]).contains("[m00006]")));
    }

    #[test]
    fn rejects_protected_tail_and_tiny_savings() {
        let canon = convo();
        let mut c = ctx(100_000);
        let tail = c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00008", "summary": SUMMARY }] }));
        assert!(tail.unwrap_err().contains("protected"));
        let tiny = c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00002", "summary": "x".repeat(7000) }] }));
        assert!(tiny.unwrap_err().contains("not much smaller"));
        assert!(c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00099", "endRef": "m00099", "summary": SUMMARY }] })).is_err());
    }

    #[test]
    fn decompress_restores_and_nested_blocks_reactivate() {
        let canon = convo();
        let mut c = ctx(100_000);
        let plain = c.prompt_view(&canon).len();
        c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00002", "summary": SUMMARY }] })).unwrap();
        c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "b1", "endRef": "m00004", "summary": SUMMARY }] })).unwrap();
        assert_eq!(c.acp_view(&canon).0.len(), plain - 3); // 一个块替换 t1+t2 的 4 条，净少 3 条
        assert!(c.acp_decompress(&canon, &json!({ "block": "b1" })).is_err()); // 已并入 b2
        c.acp_decompress(&canon, &json!({ "block": "b2" })).unwrap();
        assert_eq!(c.acp_view(&canon).0.len(), plain - 1); // b1 重新生效
        c.acp_decompress(&canon, &json!({ "block": 1 })).unwrap();
        assert_eq!(c.acp_view(&canon).0.len(), plain);
    }

    #[test]
    fn changed_history_invalidates_block() {
        let mut canon = convo();
        let mut c = ctx(100_000);
        c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00004", "summary": SUMMARY }] })).unwrap();
        canon[2] = result("a", "different");
        assert_eq!(c.acp_view(&canon).0.len(), canon.len());
    }

    #[test]
    fn compress_call_arguments_are_folded_in_view() {
        let mut canon = convo();
        canon.splice(9..9, [json!({ "role": "assistant", "content": [{ "type": "toolCall", "id": "z", "name": "compress", "arguments": { "ranges": [{ "summary": "long" }] } }] }), result("z", "ok")]);
        let c = ctx(100_000);
        let view = c.acp_view(&canon).0;
        assert!(!view.iter().any(|m| m.to_string().contains("\"long\"")));
    }

    #[test]
    fn search_finds_originals_inside_blocks() {
        let mut canon = convo();
        canon[2] = result("a", &format!("{}needle-in-original\n{}", big(), big()));
        let mut c = ctx(100_000);
        c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00004", "summary": SUMMARY }] })).unwrap();
        assert!(c.acp_search(&canon, &json!({ "query": "NEEDLE-in" })).unwrap().contains("original #2"));
        assert!(c.acp_search(&canon, &json!({ "query": "absent" })).is_err());
    }

    #[test]
    fn nudge_in_pressure_band_lists_ranges() {
        let canon = convo();
        let mut c = ctx(10_000); // ~81% used: pressure band ignores the growth gate
        let note = c.budget_notice(&canon).expect("pressure nudge");
        assert!(note.contains("m00000..m00006") && note.contains("overflow soon"), "{note}");
    }

    #[test]
    fn nudge_below_pressure_waits_for_growth() {
        let huge = "some line of output\n".repeat(10_000); // ~50k tokens each
        let canon = vec![user("go"), call("a", "read"), result("a", &huge), call("b", "read"), result("b", &huge),
            call("c", "read"), result("c", &huge), call("d", "read"), result("d", &huge), said("ok"), user("next")];
        let mut c = ctx(400_000); // ~50% used: needs 50k pending and growth
        let note = c.budget_notice(&canon).expect("first-sight nudge");
        assert!(note.contains("m00000..m00006") && !note.contains("overflow soon"), "{note}");
        assert!(c.budget_notice(&canon).is_none(), "no growth since the last notice");
    }

    #[test]
    fn missing_window_defaults_to_200k_in_acp_mode() {
        let mut c = ContextWindow::ephemeral(0, 0);
        c.enable_acp();
        assert_eq!(c.window(), 200_000);
        c.set_window(0);
        assert_eq!(c.window(), 200_000);
        c.set_window(64_000);
        assert_eq!(c.window(), 64_000);
    }

    /// summarize 不可用时 emergency 回退机械轮廓（deterministicDigest 定位）。
    #[tokio::test]
    async fn emergency_truncates_then_mechanically_compresses() {
        let canon = convo();
        let mut c = ctx(3_000);
        let before = c.prompt_tokens(&c.prompt_view(&canon));
        assert!(c.should_compact(&canon));
        let receipt = c.acp_emergency(&canon, Trigger::Auto, |_sys, _t| async { Err("no summarizer".to_string()) }).await;
        assert_eq!(receipt.status, "applied", "{receipt:?}");
        assert!(receipt.degraded, "summarizer failed → mechanical fold counts as degraded");
        assert!(c.state.acp.blocks.last().is_some_and(|b| b.mechanical));
        assert!(c.prompt_tokens(&c.prompt_view(&canon)) < before / 2);
    }

    /// preflight：模型不调 compress 且截断不够时，emergency 自己发一次摘要请求写成正常块。
    #[tokio::test]
    async fn emergency_uses_llm_summary_before_folding() {
        let canon = convo();
        let mut c = ctx(3_000);
        let receipt = c.acp_emergency(&canon, Trigger::Auto, |_sys, _t| async { Ok(SUMMARY.to_string()) }).await;
        assert_eq!(receipt.status, "applied", "{receipt:?}");
        let block = c.state.acp.blocks.last().expect("a fold happened");
        assert!(!block.mechanical && block.summary.contains("off-by-one"));
        assert!(!receipt.degraded, "LLM-summarized fold is not degraded");
    }

    #[test]
    fn block_boundaries_distill_into_higher_tiers() {
        let canon = convo();
        let mut c = ctx(100_000);
        let ask = |a: &str, b: &str| json!({ "ranges": [{ "startRef": a, "endRef": b, "summary": SUMMARY }] });
        c.acp_compress(&canon, &ask("m00002", "m00002")).unwrap();
        c.acp_compress(&canon, &ask("m00004", "m00004")).unwrap();
        let tiers = |c: &ContextWindow| c.state.acp.blocks.iter().map(|b| (b.id, b.tier, b.active)).collect::<Vec<_>>();
        assert_eq!(tiers(&c), vec![(1, 1, true), (2, 1, true)]);
        let groups = c.tier_groups(&canon, 1);
        assert_eq!((groups.len(), groups[0].first, groups[0].last, groups[0].count), (1, 1, 2, 2));
        c.acp_compress(&canon, &ask("b1", "b2")).unwrap();
        assert_eq!(tiers(&c), vec![(1, 1, false), (2, 1, false), (3, 2, true)]);
        assert!(c.tier_groups(&canon, 1).is_empty());
        // 按消息引用压缩不升层；用块 id 做端点才升层，最高 3 层。
        c.acp_compress(&canon, &ask("b3", "m00006")).unwrap();
        assert_eq!(c.state.acp.blocks.last().unwrap().tier, 3);
    }

    #[test]
    fn nudge_stamps_reset_after_compress() {
        let canon = convo();
        let mut c = ctx(10_000);
        c.budget_notice(&canon).expect("first nudge");
        assert!(c.state.acp.nudge_shown > 0 && c.state.acp.shown_by_tier[1] > 0);
        c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00004", "summary": SUMMARY }] })).unwrap();
        assert_eq!((c.state.acp.baseline, c.state.acp.nudge_shown, c.state.acp.shown_by_tier), (0, 0, [0; 4]));
    }

    #[test]
    fn status_reports_blocks() {
        let canon = convo();
        let mut c = ctx(100_000);
        c.acp_compress(&canon, &json!({ "ranges": [{ "startRef": "m00002", "endRef": "m00004", "summary": SUMMARY }] })).unwrap();
        let status = c.acp_status_value(&canon);
        assert_eq!(status["blocks"][0]["id"], "b1");
    }
}
