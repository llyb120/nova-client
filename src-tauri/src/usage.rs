//! token 用量台账。
//!
//! 已返回的请求用量立即增量入账；删除或撤回会话不改变已发生的用量。

mod prices;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::threads::{Item, Thread};

/// 一组累计值。`input` 只算未命中缓存的新增输入（缓存读写单列，与 turn 明细同一口径），
/// 缓存命中率 = cacheRead / (input + cacheRead + cacheWrite)。
/// 项目记忆消息和完整固定前缀均是输入的估算子集，不重复计入 `total`。
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
    /// 项目记忆消息在各次请求里的估算 token 合计（每次请求单独累计）。
    #[serde(default)]
    pub memory: u64,
    /// 上面这些记忆消息里命中缓存的估算量。
    #[serde(default)]
    pub memory_cache_read: u64,
    /// 系统提示 + 工具定义 + 项目记忆；旧口径台账没有这两列。
    #[serde(default)]
    pub memory_prefix_tokens: u64,
    #[serde(default)]
    pub memory_prefix_cache_read: u64,
    /// 美元估算；无法定价的部分单独标记，不当作免费。
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub unpriced_tokens: u64,
    #[serde(default)]
    pub unknown_requests: u64,
}

impl UsageBucket {
    /// 累加一轮用量。落盘的 inputTokens 已含缓存（normalize_usage 把缓存加回了输入），
    /// 少数直接透传 provider usage 的路径可能没含，按「含则扣掉、不含则并入」两种口径处理。
    fn add_turn(&mut self, item: &Item) {
        let Item::Turn {
            total_tokens,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
            memory_tokens,
            memory_cache_read,
            memory_prefix_tokens,
            memory_prefix_cache_read,
            request_accounted,
            ..
        } = item
        else {
            return;
        };
        let cache_read = cache_read_tokens.unwrap_or(0);
        let cache_write = cache_write_tokens.unwrap_or(0);
        let input = input_tokens.unwrap_or(0);
        let output = output_tokens.unwrap_or(0);
        let prompt = if input >= cache_read + cache_write {
            input
        } else {
            input + cache_read + cache_write
        };
        self.turns += 1;
        if !request_accounted {
            self.input += prompt - cache_read - cache_write;
            self.output += output;
            self.cache_read += cache_read;
            self.cache_write += cache_write;
            self.total += total_tokens.unwrap_or(prompt + output);
        }
        self.memory += memory_tokens.unwrap_or(0);
        self.memory_cache_read += memory_cache_read.unwrap_or(0);
        self.memory_prefix_tokens += memory_prefix_tokens.unwrap_or(0);
        self.memory_prefix_cache_read += memory_prefix_cache_read.unwrap_or(0);
    }

    /// 把另一组累计值并进来：按时间窗重算总量时要合并多天的桶。
    fn merge(&mut self, other: &UsageBucket) {
        self.turns += other.turns;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.total += other.total;
        self.memory += other.memory;
        self.memory_cache_read += other.memory_cache_read;
        self.memory_prefix_tokens += other.memory_prefix_tokens;
        self.memory_prefix_cache_read += other.memory_prefix_cache_read;
        self.cost_usd += other.cost_usd;
        self.unpriced_tokens += other.unpriced_tokens;
        self.unknown_requests += other.unknown_requests;
    }
}

/// 按天或按模型分组的一组用量，`name` 与 bucket 字段平铺在同一个对象里。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageGroup {
    name: String,
    #[serde(flatten)]
    usage: UsageBucket,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageStats {
    total: UsageBucket,
    days: Vec<UsageGroup>,
    models: Vec<UsageGroup>,
    legacy_pricing_pending: bool,
}

#[derive(Serialize, Deserialize, Default)]
struct UsageFile {
    #[serde(default)]
    total: UsageBucket,
    #[serde(default)]
    days: BTreeMap<String, UsageBucket>,
    #[serde(default)]
    models: BTreeMap<String, UsageBucket>,
    /// 按天再按模型记一份：只看某个时间窗时，按模型的分布要用同一个窗重算。
    /// 升级前的台账没有这一层，那些日子只能进「全部」（全部直接用 `models` 的累计值）。
    #[serde(default)]
    day_models: BTreeMap<String, BTreeMap<String, UsageBucket>>,
    /// 每个会话已入账的最后一轮 `(ts, id)`。会话被删除后条目故意保留：同一个 id 的会话
    /// 若再次出现（漫游回传等），不会把已算过的轮次重复累加。
    #[serde(default)]
    marks: HashMap<String, (i64, u64)>,
    /// 旧版金额与待定价快照都不复用；新口径从完整 token 台账重算一次。
    #[serde(default)]
    model_id_pricing_applied: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PricePart {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// 落盘的累计台账。新请求实时记账，轮次入账时补登记轮次和记忆统计。
pub struct UsageLedger {
    path: PathBuf,
    file: UsageFile,
    /// 还没有 `usage.json`：首次启动要用现存会话跑一次全量统计。
    fresh: bool,
    dirty: bool,
    prices: prices::Prices,
}

impl UsageLedger {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("usage.json");
        let file = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<UsageFile>(&text).ok());
        Self { path, fresh: file.is_none(), file: file.unwrap_or_default(), dirty: false, prices: prices::Prices::load(dir) }
    }

    /// 台账首次建立时用现存会话跑一次全量统计；之后只对新增轮次做增量累加。
    pub fn bootstrap(&mut self, threads: &[Thread]) {
        if !std::mem::take(&mut self.fresh) {
            return;
        }
        for thread in threads {
            self.account(thread);
        }
    }

    /// 新版首次拿到按模型 ID 的价表时，只替换金额和未定价量；不改变 token、轮次或对账水位。
    pub fn reprice_history(&mut self) {
        if self.file.model_id_pricing_applied || !self.prices.ready() { return; }
        let prices = &self.prices;
        for (model, usage) in self.file.models.iter_mut()
            .chain(self.file.day_models.values_mut().flat_map(|buckets| buckets.iter_mut()))
        {
            usage.cost_usd = 0.0;
            usage.unpriced_tokens = 0;
            Self::price(prices, usage, model, true, false, None);
        }
        let summarize = |total: &mut UsageBucket, buckets: Option<&BTreeMap<String, UsageBucket>>| {
            total.cost_usd = 0.0;
            total.unpriced_tokens = total.total;
            for usage in buckets.into_iter().flat_map(|models| models.values()) {
                total.cost_usd += usage.cost_usd;
                total.unpriced_tokens = total.unpriced_tokens.saturating_sub(usage.total.saturating_sub(usage.unpriced_tokens));
            }
        };
        summarize(&mut self.file.total, Some(&self.file.models));
        for (day, usage) in &mut self.file.days {
            summarize(usage, self.file.day_models.get(day));
        }
        self.file.model_id_pricing_applied = true;
        self.dirty = true;
    }

    fn price(prices: &prices::Prices, usage: &mut UsageBucket, model: &str, historical: bool, cache_unknown: bool, reasoning: Option<u64>) {
        if usage.total == 0 { return; }
        let known = usage.input.saturating_add(usage.output).saturating_add(usage.cache_read).saturating_add(usage.cache_write);
        match (!cache_unknown).then(|| prices.cost(model, usage, historical, reasoning)).flatten() {
            Some(cost) => {
                usage.cost_usd += cost;
                usage.unpriced_tokens += usage.total.saturating_sub(known);
            }
            None => usage.unpriced_tokens += usage.total,
        }
    }

    /// 会话标题上的费用：每一轮单独计价再相加。有一轮对不上官方价就整段不显示，避免把缺口当成 0。
    pub fn price_parts(&self, model: &str, parts: &[PricePart]) -> Option<f64> {
        if model.trim().is_empty() || !self.prices.ready() { return None; }
        let mut total = 0.0;
        let mut any = false;
        for part in parts {
            let usage = UsageBucket {
                input: part.input, output: part.output, cache_read: part.cache_read, cache_write: part.cache_write,
                total: part.input.saturating_add(part.output).saturating_add(part.cache_read).saturating_add(part.cache_write),
                ..UsageBucket::default()
            };
            if usage.total == 0 { continue; }
            total += self.prices.cost(model, &usage, true, None)?;
            any = true;
        }
        any.then_some(total).filter(|cost| cost.is_finite())
    }

    /// 把这个会话里还没入账的轮次并入台账，返回是否有新增。
    pub fn account(&mut self, thread: &Thread) -> bool {
        let model = thread.model.as_deref().unwrap_or("默认模型");
        self.account_turns(&thread.id, model, &thread.items)
    }

    /// 只并入标记之后的轮次：编辑/恢复后重跑的新轮次照常计入，已被计入的轮次即使后来
    /// 被截断也不会被重复统计。
    fn account_turns(&mut self, thread_id: &str, model: &str, items: &[Item]) -> bool {
        let seen = self.file.marks.get(thread_id).copied();
        let mut mark = seen;
        let mut added = false;
        for item in items {
            let Item::Turn { id, ts, .. } = item else { continue };
            let key = (*ts, *id);
            mark = Some(mark.map_or(key, |prev| prev.max(key)));
            if seen.is_some_and(|seen| key <= seen) {
                continue;
            }
            let mut usage = UsageBucket::default();
            usage.add_turn(item);
            if self.file.model_id_pricing_applied {
                Self::price(&self.prices, &mut usage, model, true, false, None);
            }
            self.add_bucket(*ts, model, &usage);
            added = true;
        }
        if let Some(mark) = mark {
            self.file.marks.insert(thread_id.to_string(), mark);
        }
        self.dirty |= added;
        added
    }

    fn add_bucket(&mut self, ts: i64, model: &str, usage: &UsageBucket) {
        self.file.total.merge(usage);
        self.file.models.entry(model.to_string()).or_default().merge(usage);
        if let Some(day) = local_day(ts) {
            self.file.days.entry(day.clone()).or_default().merge(usage);
            self.file.day_models.entry(day).or_default().entry(model.to_string()).or_default().merge(usage);
        }
        self.dirty = true;
    }

    fn account_request(&mut self, model: &str, value: &serde_json::Value, complete: bool) -> UsageBucket {
        let mut usage = request_bucket(value, complete);
        Self::price(&self.prices, &mut usage, model, false, value["cacheUnknown"] == true, value["reasoning"].as_u64());
        self.add_bucket(chrono::Utc::now().timestamp_millis(), model, &usage);
        self.flush();
        usage
    }

    /// 新请求实时入账，旧轮次仍供历史迁移和记忆前缀统计使用。
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let Ok(json) = serde_json::to_string(&self.file) else { return };
        if let Some(parent) = self.path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                eprintln!("[usage] 创建用量台账目录失败：{error}");
                return;
            }
        }
        // 先写临时文件再改名：中途崩溃不会留下半截台账。
        let tmp = self.path.with_extension("json.tmp");
        match fs::write(&tmp, json).and_then(|()| fs::rename(&tmp, &self.path)) {
            Ok(()) => self.dirty = false,
            Err(error) => eprintln!("[usage] 写入用量台账失败，保留内存计数：{error}"),
        }
    }

    /// 直接读累计值：`window` 为只算最近几天（含今天），None 表示全部。
    /// 总量和按模型的分布都按同一个时间窗汇总。
    pub fn stats(&self, window: Option<u32>) -> UsageStats {
        let since = window
            .and_then(|days| {
                chrono::Local::now()
                    .date_naive()
                    .checked_sub_signed(chrono::Duration::days(i64::from(days.max(1)) - 1))
            })
            .map(|day| day.format("%Y-%m-%d").to_string());
        let in_range = |day: &str| since.as_deref().is_none_or(|since| day >= since);
        let days: Vec<UsageGroup> = self
            .file
            .days
            .iter()
            .filter(|(day, _)| in_range(day))
            .map(|(name, usage)| UsageGroup { name: name.clone(), usage: *usage })
            .collect();
        // 全部用累计值：升级前的历史没有按天分模型，只有累计值算得准。
        let mut models: BTreeMap<&String, UsageBucket> = BTreeMap::new();
        if since.is_none() {
            models.extend(self.file.models.iter().map(|(name, usage)| (name, *usage)));
        } else {
            for buckets in self
                .file
                .day_models
                .iter()
                .filter(|(day, _)| in_range(day))
                .map(|(_, buckets)| buckets)
            {
                for (name, usage) in buckets {
                    models.entry(name).or_default().merge(usage);
                }
            }
        }
        let mut models: Vec<UsageGroup> = models
            .into_iter()
            .map(|(name, usage)| UsageGroup { name: name.clone(), usage })
            .collect();
        models.sort_by(|a, b| b.usage.total.cmp(&a.usage.total));
        let total = match since {
            None => self.file.total,
            Some(_) => {
                let mut total = UsageBucket::default();
                for group in &days {
                    total.merge(&group.usage);
                }
                total
            }
        };
        let legacy_pricing_pending = !self.file.model_id_pricing_applied && self.file.total.total > 0;
        UsageStats { total, days, models, legacy_pricing_pending }
    }
}

static LIVE: OnceLock<Arc<Mutex<UsageLedger>>> = OnceLock::new();

pub fn start(ledger: Arc<Mutex<UsageLedger>>) {
    if LIVE.set(ledger.clone()).is_err() { return; }
    tauri::async_runtime::spawn(async move {
        loop {
            let stale = ledger.lock().unwrap().prices.stale();
            if stale {
                match prices::fetch().await {
                    Ok(prices) => {
                        let mut ledger = ledger.lock().unwrap();
                        if let Some(dir) = ledger.path.parent() {
                            if let Err(error) = prices.save(dir) { eprintln!("[usage] 保存价格失败：{error}"); }
                        }
                        ledger.prices = prices;
                        ledger.reprice_history();
                        ledger.flush();
                    }
                    Err(error) => eprintln!("[usage] 更新模型价格失败：{error}"),
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
        }
    });
}

tokio::task_local! {
    /// 会话内的实际请求收据（含失败/取消时已收到的用量），与全局台账使用同一笔计价。
    pub(crate) static REQUEST_SETTLED: Box<dyn Fn(UsageBucket) + Send + Sync>;
}

fn request_bucket(value: &serde_json::Value, complete: bool) -> UsageBucket {
    let n = |key: &str| value[key].as_u64().unwrap_or(0);
    UsageBucket {
        input: n("input"), output: n("output"), cache_read: n("cacheRead"), cache_write: n("cacheWrite"),
        total: n("input").saturating_add(n("output")).saturating_add(n("cacheRead")).saturating_add(n("cacheWrite")),
        unknown_requests: u64::from(!complete || value["input"].as_u64().is_none() || value["output"].as_u64().is_none()
            || value["cacheUnknown"] == true || value["usageIncomplete"] == true), ..UsageBucket::default()
    }
}

/// On interruption the guard still records any usage received; absent usage remains unknown.
pub(crate) struct RequestMeter {
    ledger: Option<Arc<Mutex<UsageLedger>>>,
    model: String,
    usage: serde_json::Value,
    complete: bool,
}

impl RequestMeter {
    pub(crate) fn new(model: &crate::lyra::config::ResolvedModel) -> Self {
        Self::for_model(&model.provider, &model.id)
    }
    pub(crate) fn for_model(provider: &str, model: &str) -> Self {
        Self { ledger: LIVE.get().cloned(), model: format!("{provider}/{model}"), usage: serde_json::Value::Null, complete: false }
    }
    pub(crate) fn set_model(&mut self, id: &str) {
        if !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control) {
            if let Some((provider, _)) = self.model.split_once('/') { self.model = format!("{provider}/{id}"); }
        }
    }
    pub(crate) fn observe(&mut self, usage: &serde_json::Value) {
        if usage.is_object() { self.usage = usage.clone(); }
    }
    pub(crate) fn complete(&mut self, usage: &serde_json::Value, complete: bool) {
        self.observe(usage);
        self.complete = complete;
    }
}

impl Drop for RequestMeter {
    fn drop(&mut self) {
        let receipt = self.ledger.as_ref().map(|ledger| {
            ledger.lock().unwrap_or_else(|e| e.into_inner()).account_request(&self.model, &self.usage, self.complete)
        });
        let _ = REQUEST_SETTLED.try_with(|notify| notify(receipt.unwrap_or_else(|| {
            let mut usage = request_bucket(&self.usage, self.complete);
            usage.unpriced_tokens = usage.total;
            usage
        })));
    }
}

fn local_day(ts: i64) -> Option<String> {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ts)
        .map(|at| at.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(id: u64, ts: i64, input: u64, output: u64, cache_read: u64, cache_write: u64) -> Item {
        Item::Turn {
            id,
            ts,
            duration_ms: 0,
            total_tokens: Some(input + output),
            input_tokens: Some(input),
            context_tokens: None,
            output_tokens: Some(output),
            cache_read_tokens: Some(cache_read),
            cache_write_tokens: Some(cache_write),
            memory_tokens: None,
            memory_cache_read: None,
            memory_prefix_tokens: None,
            memory_prefix_cache_read: None,
            cost_usd: None,
            unpriced_tokens: None,
            unknown_requests: None,
            request_accounted: false,
            stop_reason: "end_turn".into(),
        }
    }

    fn ledger() -> UsageLedger {
        let path = std::env::temp_dir().join(format!("nova-usage-{}.json", uuid::Uuid::new_v4()));
        UsageLedger { path, file: UsageFile::default(), fresh: false, dirty: false, prices: prices::Prices::default() }
    }

    fn bucket_of(item: &Item) -> UsageBucket {
        let mut bucket = UsageBucket::default();
        bucket.add_turn(item);
        bucket
    }

    #[test]
    fn cache_tokens_are_split_out_of_input() {
        // 输入 100 里 70 命中缓存、20 写入缓存，剩 10 才是新增输入。
        let first = bucket_of(&turn(1, 0, 100, 30, 70, 20));
        let second = bucket_of(&turn(2, 0, 50, 10, 0, 0));
        let merged = UsageBucket {
            turns: first.turns + second.turns,
            input: first.input + second.input,
            output: first.output + second.output,
            cache_read: first.cache_read + second.cache_read,
            cache_write: first.cache_write + second.cache_write,
            total: first.total + second.total,
            ..UsageBucket::default()
        };
        assert_eq!(
            (merged.turns, merged.input, merged.output, merged.cache_read, merged.cache_write),
            (2, 60, 40, 70, 20)
        );
        assert_eq!(merged.total, 190);
    }

    #[test]
    fn input_without_cache_tokens_still_joins_the_prompt_total() {
        // 有的 provider 的 input 不含缓存 token，不能被当成「已扣过」而吞掉。
        let bucket = bucket_of(&turn(1, 0, 10, 5, 70, 20));
        assert_eq!((bucket.input, bucket.cache_read, bucket.cache_write), (10, 70, 20));
    }

    #[test]
    fn project_memory_tokens_are_a_subset_and_aggregate() {
        // 两种口径均是输入的子集，不重复进总量；旧台账的记忆消息不能冒充完整前缀。
        let memory_turn = Item::Turn {
            id: 1,
            ts: 0,
            duration_ms: 0,
            total_tokens: Some(1050),
            input_tokens: Some(1000),
            context_tokens: None,
            output_tokens: Some(50),
            cache_read_tokens: Some(700),
            cache_write_tokens: Some(0),
            memory_tokens: Some(900),
            memory_cache_read: Some(620),
            memory_prefix_tokens: Some(980),
            memory_prefix_cache_read: Some(700),
            cost_usd: None,
            unpriced_tokens: None,
            unknown_requests: None,
            request_accounted: false,
            stop_reason: "end_turn".into(),
        };
        let mut bucket = bucket_of(&memory_turn);
        assert_eq!((bucket.memory, bucket.memory_cache_read), (900, 620));
        assert_eq!((bucket.memory_prefix_tokens, bucket.memory_prefix_cache_read), (980, 700));
        assert_eq!((bucket.input, bucket.cache_read, bucket.total), (300, 700, 1050));
        let legacy: UsageBucket = serde_json::from_str(
            r#"{"turns":1,"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":10,"memory":500,"memoryCacheRead":400}"#,
        ).unwrap();
        assert_eq!((legacy.memory, legacy.memory_cache_read), (500, 400));
        assert_eq!((legacy.memory_prefix_tokens, legacy.memory_prefix_cache_read), (0, 0));
        bucket.merge(&legacy);
        assert_eq!((bucket.memory, bucket.memory_prefix_tokens, bucket.memory_prefix_cache_read), (1400, 980, 700));
    }

    #[test]
    fn project_memory_prefix_survives_turn_and_ledger_reload() {
        let dir = tempfile::tempdir().unwrap();
        let mut thread = Thread::new(".".into(), crate::threads::AgentKind::Lyra, None, None, None, false);
        thread.push_turn(1, Some(&serde_json::json!({"inputTokens":1200,"outputTokens":10,
            "totalTokens":1210,"cacheReadTokens":950,"memoryPrefixTokens":1000,"memoryPrefixCacheRead":900})), "end_turn", false);
        let mut ledger = UsageLedger::load(dir.path());
        assert!(ledger.account(&thread));
        ledger.flush();
        let restored = UsageLedger::load(dir.path());
        let total = restored.stats(None).total;
        assert_eq!((total.memory_prefix_tokens, total.memory_prefix_cache_read), (1000, 900));
        assert_eq!((total.input, total.cache_read, total.total), (250, 950, 1210));
    }

    #[test]
    fn reaccounting_skips_counted_turns_and_keeps_removed_sessions() {
        let mut ledger = ledger();
        let first = turn(1, 10, 100, 30, 70, 20);
        assert!(ledger.account_turns("t1", "m", std::slice::from_ref(&first)));
        // 打开统计面板会再对一次账：同一轮不能重复累加。
        assert!(!ledger.account_turns("t1", "m", std::slice::from_ref(&first)));
        // 会话被删掉之后就不再对账，但已入账的量留在总量里。
        assert!(ledger.account_turns("t2", "m", &[turn(1, 20, 10, 5, 0, 0)]));
        // 截断重跑：旧轮次已算过，只计新落的这一轮。
        assert!(ledger.account_turns("t1", "m", &[first, turn(1, 30, 10, 5, 0, 0)]));
        let stats = ledger.stats(None);
        assert_eq!((stats.total.turns, stats.total.total), (3, 130 + 15 + 15));
    }

    fn record_day(ledger: &mut UsageLedger, ago: i64, total: u64) {
        let day = chrono::Local::now()
            .date_naive()
            .checked_sub_signed(chrono::Duration::days(ago))
            .unwrap()
            .format("%Y-%m-%d")
            .to_string();
        let usage = UsageBucket { turns: 1, total, ..UsageBucket::default() };
        ledger.file.days.insert(day.clone(), usage);
        ledger.file.day_models.entry(day).or_default().insert("m".into(), usage);
        ledger.file.models.entry("m".into()).or_default().merge(&usage);
        ledger.file.total.merge(&usage);
    }

    #[test]
    fn historical_pricing_is_once_and_requests_are_incremental() {
        let mut ledger = ledger();
        let make_prices = |input, output| prices::Prices::from_catalog(&serde_json::json!({
            "p": {"models": {"m": {"cost": {"input":input,"output":output,"cache_read":0.5,"cache_write":1.25}}}}
        }), 1).unwrap();
        let old = turn(1, chrono::Utc::now().timestamp_millis(), 100, 30, 70, 20);
        assert!(ledger.account_turns("thread", "p/m", &[old]));
        ledger.reprice_history();
        assert!(ledger.stats(None).legacy_pricing_pending);
        ledger.prices = make_prices(1.0, 2.0);
        ledger.reprice_history();
        assert!(!ledger.stats(None).legacy_pricing_pending);
        assert!((ledger.stats(None).total.cost_usd - 0.00013).abs() < 1e-12);

        let request = serde_json::json!({"input":10,"output":5,"cacheRead":0,"cacheWrite":0});
        ledger.account_request("p/m", &request, true);
        ledger.prices = make_prices(4.0, 8.0);
        ledger.reprice_history();
        ledger.account_request("p/m", &request, true);
        let stats = ledger.stats(None);
        assert!((stats.total.cost_usd - 0.00023).abs() < 1e-12);
        assert_eq!(stats.total.total, 160);
        assert_eq!(stats.days[0].usage.total, 160);
        assert_eq!(stats.models[0].usage.total, 160);
    }

    #[test]
    fn metered_retries_and_interrupted_requests_survive_turn_reconciliation() {
        let ledger = Arc::new(Mutex::new(ledger()));
        let make = || RequestMeter { ledger: Some(ledger.clone()), model: "p/m".into(),
            usage: serde_json::Value::Null, complete: false };
        let usage = serde_json::json!({"input":10,"output":5,"cacheRead":0,"cacheWrite":0});
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let captured = receipts.clone();
        REQUEST_SETTLED.sync_scope(Box::new(move |b| captured.lock().unwrap().push(b)), || {
            { let mut retry = make(); retry.complete(&usage, false); }
            { let mut cancelled = make(); cancelled.observe(&serde_json::json!({"input":7})); }
            { let mut success = make(); success.set_model("actual"); success.complete(&usage, true); }
        });
        let reports = receipts.lock().unwrap();
        assert_eq!(reports.iter().map(|b| (b.total, b.unknown_requests)).collect::<Vec<_>>(), [(15, 1), (7, 1), (15, 0)]);
        let mut ledger = ledger.lock().unwrap();
        let mut item = turn(1, chrono::Utc::now().timestamp_millis(), 30, 10, 0, 0);
        if let Item::Turn { request_accounted, .. } = &mut item { *request_accounted = true; }
        assert!(ledger.account_turns("t", "p/m", std::slice::from_ref(&item)));
        assert!(!ledger.account_turns("t", "p/m", &[item]));
        let stats = ledger.stats(None);
        assert_eq!((stats.total.input, stats.total.output, stats.total.total), (27, 10, 37));
        assert_eq!(stats.total.unknown_requests, 2);
        assert_eq!(stats.total.unpriced_tokens, 37);
        assert_eq!(stats.total.turns, 1);
        assert_eq!(stats.models.iter().find(|m| m.name == "p/actual").unwrap().usage.total, 15);
    }

    #[test]
    fn pricing_and_request_accounting_survive_restart_without_repricing() {
        let dir = tempfile::tempdir().unwrap();
        let prices = prices::Prices::from_catalog(&serde_json::json!({"p":{"models":{
            "m":{"cost":{"input":1.0,"output":2.0}}
        }}}), 1).unwrap();
        prices.save(dir.path()).unwrap();
        let mut ledger = UsageLedger::load(dir.path());
        let old = turn(1, chrono::Utc::now().timestamp_millis(), 100, 10, 0, 0);
        ledger.account_turns("t", "p/m", std::slice::from_ref(&old));
        ledger.reprice_history();
        ledger.account_request("p/m", &serde_json::json!({"input":10,"output":5}), true);
        ledger.flush();
        drop(ledger);
        let mut restored = UsageLedger::load(dir.path());
        restored.reprice_history();
        assert!(!restored.account_turns("t", "p/m", &[old]));
        let stats = restored.stats(None);
        assert_eq!(stats.total.total, 125);
        assert!((stats.total.cost_usd - 0.00014).abs() < 1e-12);
        assert!(!stats.legacy_pricing_pending);
    }

    #[test]
    fn upgrade_replaces_old_prices_once_without_losing_history_or_marks() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = UsageLedger::load(dir.path());
        let now = chrono::Utc::now().timestamp_millis();
        let day = local_day(now).unwrap();
        let model = "中转/openai/m/variant/max";
        ledger.add_bucket(now, model, &UsageBucket {
            turns: 2, input: 1_000_000, cache_read: 500_000, output: 100_000, total: 1_600_000,
            cost_usd: 99.0, unpriced_tokens: 600_000, unknown_requests: 1, ..UsageBucket::default()
        });
        ledger.file.total.total += 12;
        ledger.file.days.get_mut(&day).unwrap().total += 12;
        ledger.file.marks.insert("deleted".into(), (now, 2));
        let mut old = serde_json::to_value(&ledger.file).unwrap();
        old["legacy_pricing_started"] = true.into();
        old["legacy_pending"] = serde_json::json!({"total":{"total":1}});
        fs::write(&ledger.path, old.to_string()).unwrap();
        fs::write(dir.path().join("model-prices.json"), r#"{"fetchedAt":1,"models":{"openai/m":{"input":99}},"indexed":true}"#).unwrap();
        let mut ledger = UsageLedger::load(dir.path());
        ledger.reprice_history();
        assert!(ledger.stats(None).legacy_pricing_pending, "wait for the new index");
        assert_eq!(ledger.stats(None).total.cost_usd, 99.0);
        // Requests received while the catalogue is loading are part of the same one-shot reprice.
        ledger.account_request("another/m", &serde_json::json!({"input":100_000,"output":0}), true);
        let catalog = |input| prices::Prices::from_catalog(&serde_json::json!({"openai":{"models":{"m":{
            "canonical_model_id":"openai/m", "cost":{"input":input,"output":2.0,"cache_read":0.2,
                "tiers":[{"tier":{"type":"context","size":272_000},"input":9.0,"output":9.0,"cache_read":9.0}]}
        }}}}), 1).unwrap();
        ledger.prices = catalog(1.0);
        ledger.reprice_history();
        ledger.flush();
        ledger.prices = catalog(10.0);
        ledger.prices.save(dir.path()).unwrap();
        let mut restored = UsageLedger::load(dir.path());
        restored.reprice_history();
        let stats = restored.stats(None);
        assert!(!stats.legacy_pricing_pending);
        for b in [stats.total, stats.days[0].usage] {
            assert!((b.cost_usd - 1.4).abs() < 1e-12);
            assert_eq!((b.total, b.unpriced_tokens, b.unknown_requests, b.turns), (1_700_012, 12, 1, 2));
        }
        assert!((stats.models[0].usage.cost_usd - 1.3).abs() < 1e-12);
        assert_eq!(restored.file.marks["deleted"], (now, 2));
    }

    #[test]
    fn known_cache_details_are_required_for_per_request_prices() {
        let mut ledger = ledger();
        ledger.prices = prices::Prices::from_catalog(&serde_json::json!({"p":{"models":{
            "m":{"cost":{"input":1.0,"output":2.0,"cache_read":0.1}}
        }}}), 1).unwrap();
        ledger.account_request("p/m", &serde_json::json!({"input":100,"output":10,"cacheUnknown":true}), true);
        let total = ledger.stats(None).total;
        assert_eq!((total.total, total.unpriced_tokens, total.unknown_requests), (110, 110, 1));
        assert_eq!(total.cost_usd, 0.0);
    }

    #[test]
    fn missing_rates_and_tiered_or_reasoning_prices_are_not_guessed() {
        let prices = prices::Prices::from_catalog(&serde_json::json!({"p":{"models":{
            "tier":{"cost":{"input":1.0,"output":2.0,"tiers":[{"tier":{"type":"context","size":100},"input":3.0,"output":4.0}]}},
            "reason":{"cost":{"input":1.0,"output":2.0,"reasoning":4.0}}
        }}}), 1).unwrap();
        let usage = UsageBucket { input: 150, output: 10, total: 160, ..UsageBucket::default() };
        assert!((prices.cost("p/tier", &usage, false, None).unwrap() - 0.00049).abs() < 1e-12);
        assert!((prices.cost("p/tier", &usage, true, None).unwrap() - 0.00017).abs() < 1e-12);
        assert!(prices.cost("p/reason", &usage, false, None).is_none());
        assert!((prices.cost("p/reason", &usage, false, Some(5)).unwrap() - 0.00018).abs() < 1e-12);
        let cached = UsageBucket { cache_read: 10, total: 10, ..UsageBucket::default() };
        assert!(prices.cost("p/reason", &cached, false, None).is_none());
    }

    #[test]
    fn prices_use_official_catalog_and_dated_variants() {
        let prices = prices::Prices::from_catalog(&serde_json::json!({
            "deepseek": {"models": {
                "deepseek-flash": {"canonical_model_id": "deepseek/deepseek-v4.1-flash-1017",
                    "cost": {"input": 0.15, "output": 0.6}},
                "deepseek-v4-pro": {"canonical_model_id": "deepseek/deepseek-v4-pro-0813",
                    "cost": {"input": 0.66, "output": 1.98}}
            }},
            "openrouter": {"models": {
                "deepseek/deepseek-v4.1-flash-1017": {"canonical_model_id": "deepseek/deepseek-v4.1-flash-1017",
                    "cost": {"input": 0.04, "output": 0.5}},
                "deepseek-v4.1-flash-1017-fast": {"canonical_model_id": "deepseek/deepseek-v4.1-flash-1017",
                    "cost": {"input": 9.0, "output": 9.0}},
                "deepseek-v4.1-flash": {"canonical_model_id": "deepseek/deepseek-v4.1-flash-1017",
                    "cost": {"input": 0.09, "output": 0.45}}
            }},
            "gateway": {"models": {"deepseek/deepseek-v4.1-flash": {"canonical_model_id": "deepseek/deepseek-v4.1-flash",
                "cost": {"input": 1.0, "output": 1.0}}}},
            "only": {"models": {"solo": {"cost": {"input": 2.0, "output": 0.0}}}},
            "a": {"models": {"dup": {"cost": {"input": 1.0, "output": 1.0}}}},
            "b": {"models": {"dup": {"cost": {"input": 3.0, "output": 3.0}}}}
        }), 1).unwrap();
        let usage = UsageBucket { input: 1_000_000, output: 1_000_000, total: 2_000_000, ..UsageBucket::default() };
        let price = |id: &str| prices.cost(id, &usage, false, None);
        for id in [
            "openrouter/deepseek/deepseek-v4.1-flash-1017",
            "custom/deepseek-v4.1-flash-1017",
            "custom/DeepSeek-V4.1-Flash-1017",
            "custom/deepseek-ai/DeepSeek-V4.1-Flash-1017",
            "custom/deepseek-v4.1-flash-1017-fast",
            "custom/deepseek-v4.1-flash-1017[1m]",
            "deepseek/deepseek-flash",
            "deepseek-v4.1-flash",
            "梦嘉/DeepSeek/DEEPSEEK-V4.1-FLASH/variant/max",
            "opendesign/deepseek-v4.1-flash-eco/variant/legend",
            "deepseek/deepseek-v4.1-flash-1017",
        ] {
            let cost = price(id).unwrap_or(-1.0);
            assert!((cost - 0.75).abs() < 1e-9, "{id} priced {cost}");
        }
        assert!((price("anywhere/deepseek-v4-pro-0813").unwrap() - 2.64).abs() < 1e-9);
        assert!((price("other/solo").unwrap() - 2.0).abs() < 1e-9);
        assert!(["a/dup", "b/dup", "dup", "custom/deepseek-v4.1-flash-1111"].into_iter().all(|id| price(id).is_none()));
    }

    #[test]
    fn session_price_sums_turns_at_the_official_rate() {
        let mut ledger = ledger();
        ledger.prices = prices::Prices::from_catalog(&serde_json::json!({
            "deepseek": {"models": {"deepseek-flash": {"canonical_model_id": "deepseek/deepseek-v4.1-flash-1017",
                "cost": {"input": 1.0, "output": 2.0}}}},
            "gw": {"models": {"deepseek-v4.1-flash-1017": {"canonical_model_id": "deepseek/deepseek-v4.1-flash-1017",
                "cost": {"input": 9.0, "output": 9.0}}}}
        }), 1).unwrap();
        let parts = [
            PricePart { input: 1_000_000, output: 0, cache_read: 0, cache_write: 0 },
            PricePart { input: 0, output: 1_000_000, cache_read: 0, cache_write: 0 },
        ];
        assert!((ledger.price_parts("gw/deepseek-v4.1-flash-1017", &parts).unwrap() - 3.0).abs() < 1e-9);
        assert!(ledger.price_parts("gw/missing-model", &parts).is_none());
    }

    #[test]
    fn window_keeps_only_recent_days_and_rebuilds_models_inside_it() {
        let mut ledger = ledger();
        record_day(&mut ledger, 0, 100);
        record_day(&mut ledger, 5, 200);
        record_day(&mut ledger, 40, 400);

        let week = ledger.stats(Some(7));
        assert_eq!((week.days.len(), week.total.total), (2, 300));
        assert_eq!((week.models.len(), week.models[0].usage.total), (1, 300));

        let all = ledger.stats(None);
        assert_eq!((all.days.len(), all.total.total), (3, 700));
        assert_eq!(all.models[0].usage.total, 700);
    }
}
