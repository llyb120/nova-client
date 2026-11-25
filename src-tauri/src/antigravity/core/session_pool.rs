//! Antigravity 热进程池（严格 1 活跃会话 = 1 独立 `agy acp` 子进程）：
//! - 上限 `MAX_LIVE_BINDINGS = 4`，超限时按 LRU 淘汰最久未用的**空闲**绑定；
//! - 空闲驱逐 TTL `IDLE_EVICT_SECS = 1800`（30 分钟，减少频繁冷启与重新加载上下文）；
//! - 正在运行 turn（`in_use == true`）的绑定绝不被驱逐。

use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

pub const MAX_LIVE_BINDINGS: usize = 4;
pub const IDLE_EVICT_SECS: u64 = 1800;

struct PoolEntry<T> {
    value: T,
    in_use: bool,
    last_used: Instant,
    lru_seq: u64,
}

pub struct SessionPool<T> {
    entries: HashMap<String, PoolEntry<T>>,
    max_live: usize,
    idle_ttl: Duration,
    seq: u64,
}

impl<T> Default for SessionPool<T> {
    fn default() -> Self {
        Self::new(MAX_LIVE_BINDINGS, Duration::from_secs(IDLE_EVICT_SECS))
    }
}

impl<T> SessionPool<T> {
    pub fn new(max_live: usize, idle_ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            max_live: max_live.max(1),
            idle_ttl,
            seq: 0,
        }
    }

    fn next_seq(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&T> {
        self.entries.get(key).map(|e| &e.value)
    }

    /// 命中并刷新 LRU 时间戳。
    pub fn touch(&mut self, key: &str, now: Instant) -> Option<&T> {
        let seq = self.next_seq();
        let entry = self.entries.get_mut(key)?;
        entry.last_used = now;
        entry.lru_seq = seq;
        Some(&entry.value)
    }

    pub fn mark_in_use(&mut self, key: &str, in_use: bool, now: Instant) {
        let seq = self.next_seq();
        if let Some(entry) = self.entries.get_mut(key) {
            entry.in_use = in_use;
            entry.last_used = now;
            entry.lru_seq = seq;
        }
    }

    /// 插入或替换指定会话绑定，并在超过 `max_live` 时按 LRU 淘汰最久未用的空闲绑定。
    /// 返回被替换或被淘汰的所有 `(thread_id, binding)`，供调用方异步 `session/close` + 终止进程。
    pub fn insert(&mut self, key: String, value: T, now: Instant) -> Vec<(String, T)> {
        let mut evicted = Vec::new();
        let seq = self.next_seq();
        if let Some(old) = self.entries.insert(
            key.clone(),
            PoolEntry {
                value,
                in_use: false,
                last_used: now,
                lru_seq: seq,
            },
        ) {
            evicted.push((key, old.value));
        }

        while self.entries.len() > self.max_live {
            let victim = self
                .entries
                .iter()
                .filter(|(_, e)| !e.in_use)
                .min_by_key(|(_, e)| (e.last_used, e.lru_seq))
                .map(|(k, _)| k.clone());
            let Some(victim_key) = victim else {
                break;
            };
            if let Some(removed) = self.entries.remove(&victim_key) {
                evicted.push((victim_key, removed.value));
            }
        }
        evicted
    }

    /// 按空闲 TTL 驱逐所有超过 `idle_ttl` 且未运行的绑定。
    pub fn evict_idle(&mut self, now: Instant) -> Vec<(String, T)> {
        let victims: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| !e.in_use && now.saturating_duration_since(e.last_used) >= self.idle_ttl)
            .map(|(k, _)| k.clone())
            .collect();
        let mut evicted = Vec::with_capacity(victims.len());
        for key in victims {
            if let Some(removed) = self.entries.remove(&key) {
                evicted.push((key, removed.value));
            }
        }
        evicted
    }

    pub fn remove(&mut self, key: &str) -> Option<T> {
        self.entries.remove(key).map(|e| e.value)
    }

    pub fn drain_all(&mut self) -> Vec<(String, T)> {
        self.entries
            .drain()
            .map(|(k, e)| (k, e.value))
            .collect()
    }
}
