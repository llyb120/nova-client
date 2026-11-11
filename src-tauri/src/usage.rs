//! token 用量台账。
//!
//! 用量按轮增量累计在 `usage.json`，不再每次从现存的会话全量重算：会话随时可能被删除
//! （手动删除、自动清理、员工回收、临时会话），重算会让已经花掉的量凭空消失。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::threads::{Item, Thread};

/// 一组累计值。`input` 只算未命中缓存的新增输入（缓存读写单列，与 turn 明细同一口径），
/// 缓存命中率 = cacheRead / (input + cacheRead + cacheWrite)。
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub total: u64,
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
        self.input += prompt - cache_read - cache_write;
        self.output += output;
        self.cache_read += cache_read;
        self.cache_write += cache_write;
        self.total += total_tokens.unwrap_or(prompt + output);
    }

    /// 把另一组累计值并进来：按时间窗重算总量时要合并多天的桶。
    fn merge(&mut self, other: &UsageBucket) {
        self.turns += other.turns;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.total += other.total;
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
}

/// 落盘的累计台账。只在出现新轮次时写盘。
pub struct UsageLedger {
    path: PathBuf,
    file: UsageFile,
    /// 还没有 `usage.json`：首次启动要用现存会话跑一次全量统计。
    fresh: bool,
    dirty: bool,
}

impl UsageLedger {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("usage.json");
        let file = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<UsageFile>(&text).ok());
        Self { path, fresh: file.is_none(), file: file.unwrap_or_default(), dirty: false }
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
            self.file.total.add_turn(item);
            self.file.models.entry(model.to_string()).or_default().add_turn(item);
            if let Some(day) = local_day(*ts) {
                self.file.days.entry(day.clone()).or_default().add_turn(item);
                self.file
                    .day_models
                    .entry(day)
                    .or_default()
                    .entry(model.to_string())
                    .or_default()
                    .add_turn(item);
            }
            added = true;
        }
        if let Some(mark) = mark {
            self.file.marks.insert(thread_id.to_string(), mark);
        }
        self.dirty |= added;
        added
    }

    /// 有新轮次才写盘；失败保留内存计数，下次统计时重试。
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
        UsageStats { total, days, models }
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
            stop_reason: "end_turn".into(),
        }
    }

    fn ledger() -> UsageLedger {
        let path = std::env::temp_dir().join(format!("nova-usage-{}.json", uuid::Uuid::new_v4()));
        UsageLedger { path, file: UsageFile::default(), fresh: false, dirty: false }
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
