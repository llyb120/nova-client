//! 命令行子工具集：给数字员工的 agent 用「自带 shell」直接调用。
//!
//! 分两类：
//! - 读工具（实时直读数据目录里的存储）：`kb-search`(=`mem-search`) 语义/向量检索记忆+知识、
//!   `ledger-list` 看门锁账本（谁在做什么，防撞车）。
//! - 写工具（把一条命令追加进「收件箱」文件，由应用后台循环执行、秒级生效）：
//!   `relay`(接力/讨论/交用户决策)、`memo`(沉淀记忆)、`learn`(沉淀长期知识)、
//!   `lesson`(立试行守则)/`lesson-verify`(验证强化，满 2 次自动内化)、
//!   `forget`(清理记忆)、`done`/`blocked`(收尾)。
//!
//! 复用与主程序同一份数据目录（记忆库 employee_memory.json / 设置 settings.json / 账本 marks.json），
//! API key 留在设置文件里、不经命令行传递，避免出现在会话记录里。

use crate::employees::{append_command, bm25_rank, InboxCommand, JournalEntry, MemoryStore};
use crate::marks::{render_digest, MarkStore};
use crate::semantic;
use crate::settings::Settings;
use crate::threads::now_ms;
use std::collections::HashMap;
use std::path::PathBuf;

/// 所有已知子命令。
fn is_known(cmd: &str) -> bool {
    matches!(
        cmd,
        "mem-search"
            | "kb-search"
            | "ledger-list"
            | "relay"
            | "memo"
            | "learn"
            | "lesson"
            | "lesson-verify"
            | "forget"
            | "done"
            | "blocked"
    )
}

/// 若 argv 命中 CLI 子命令则执行并返回 true（调用方随后直接退出，不再启动 GUI）；否则 false。
pub fn maybe_run() -> bool {
    let args: Vec<String> = std::env::args().collect();
    let Some(cmd) = args.get(1).map(|s| s.as_str()) else {
        return false;
    };
    if !is_known(cmd) {
        return false;
    }
    let (flags, positional) = parse_flags(&args[2..]);
    let out = match cmd {
        "mem-search" | "kb-search" => cmd_search(&flags, &positional),
        "ledger-list" => cmd_ledger_list(&flags),
        _ => cmd_write(cmd, &flags),
    };
    println!("{out}");
    true
}

/// 极简 flag 解析：支持 `--k v`、`--k=v`；无值的 `--flag` 记为 "true"；其余为位置参数。
fn parse_flags(args: &[String]) -> (HashMap<String, String>, Vec<String>) {
    let mut flags: HashMap<String, String> = HashMap::new();
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(rest) = a.strip_prefix("--") {
            if let Some((k, v)) = rest.split_once('=') {
                flags.insert(k.to_string(), v.to_string());
            } else if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                flags.insert(rest.to_string(), args[i + 1].clone());
                i += 1;
            } else {
                flags.insert(rest.to_string(), "true".to_string());
            }
        } else {
            positional.push(a.clone());
        }
        i += 1;
    }
    (flags, positional)
}

// ===== 读工具 =====

fn cmd_search(flags: &HashMap<String, String>, positional: &[String]) -> String {
    let data_dir = flags.get("data-dir").cloned().unwrap_or_default();
    let employee = flags.get("employee").cloned().unwrap_or_default();
    let k: usize = flags
        .get("k")
        .and_then(|s| s.parse().ok())
        .unwrap_or(6)
        .clamp(1, 20);
    // --query 优先，否则把位置参数拼起来当查询词
    let query = flags
        .get("query")
        .cloned()
        .unwrap_or_else(|| positional.join(" "));
    run_search(&data_dir, &employee, query.trim(), k)
}

fn cmd_ledger_list(flags: &HashMap<String, String>) -> String {
    let data_dir = flags.get("data-dir").cloned().unwrap_or_default();
    let scope = flags.get("scope").cloned().unwrap_or_default();
    if data_dir.is_empty() || scope.is_empty() {
        return "用法：ledger-list --data-dir <dir> --scope <scope>".into();
    }
    let store = MarkStore::load(&PathBuf::from(data_dir));
    let marks = store.list(Some(&scope));
    format!("门锁账本（scope={scope}）：\n{}", render_digest(&marks))
}

fn run_search(data_dir: &str, employee: &str, query: &str, k: usize) -> String {
    if data_dir.is_empty() || employee.is_empty() {
        return "用法：kb-search --data-dir <dir> --employee <id> --query \"<关键词/问题>\"".into();
    }
    if query.is_empty() {
        return "请提供 --query \"<关键词或问题>\"".into();
    }
    let dir = PathBuf::from(data_dir);
    let mem = MemoryStore::load(&dir);
    let entries = mem.all(employee);
    if entries.is_empty() {
        return "（这名员工还没有任何历史记忆/知识）".into();
    }
    let refs: Vec<&JournalEntry> = entries.iter().collect();

    // 语义（向量）优先；服务不可用/未启用时回退 BM25 关键词检索。
    let ranked: Vec<&JournalEntry> =
        semantic_search(&dir, query, &refs, k).unwrap_or_else(|| bm25_rank(&refs, query, k));
    if ranked.is_empty() {
        return "（没有找到相关记忆）".into();
    }

    let mut out = format!("与「{}」最相关的 {} 条记忆/知识：\n", query, ranked.len());
    for e in ranked {
        let tag = if e.kind == "lesson" {
            if e.pinned {
                "守则·已内化"
            } else {
                "守则·试行"
            }
        } else if e.pinned {
            "长期知识"
        } else {
            "工作记忆"
        };
        let title = if e.task_title.trim().is_empty() {
            "(无标题)"
        } else {
            e.task_title.trim()
        };
        out.push_str(&format!(
            "\n- [{tag}] ts={} 〔{title}〕{}",
            e.ts,
            one_line(&e.summary, 400)
        ));
    }
    out
}

// ===== 写工具（追加到收件箱）=====

fn cmd_write(kind: &str, flags: &HashMap<String, String>) -> String {
    let data_dir = flags.get("data-dir").cloned().unwrap_or_default();
    if data_dir.is_empty() {
        return "缺少 --data-dir <dir>".into();
    }
    let from = flags
        .get("employee")
        .or_else(|| flags.get("from"))
        .cloned()
        .unwrap_or_default();
    if from.is_empty() {
        return "缺少 --employee <employeeId>".into();
    }
    let opt = |name: &str| flags.get(name).cloned().unwrap_or_default();
    let cmd = InboxCommand {
        kind: kind.to_string(),
        from,
        scope: opt("scope"),
        key: opt("key"),
        title: opt("title"),
        to: opt("to"),
        relay_kind: opt("kind"),
        brief: opt("brief"),
        question: opt("question"),
        category: opt("category"),
        options: flags
            .get("options")
            .map(|s| {
                s.split([';', '；', ',', '，'])
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        text: opt("text"),
        ts: flags.get("ts").and_then(|s| s.parse().ok()).unwrap_or(0),
        summary: opt("summary"),
        reason: opt("reason"),
        origin_thread_id: opt("origin-thread-id"),
        decision_source: opt("decision-source"),
        at: now_ms(),
    };

    // 各命令的必填校验，尽早给 agent 明确报错。
    let err = match kind {
        "relay" => {
            if cmd.to.is_empty() {
                Some("relay 需要 --to <同事名|self|user>")
            } else if cmd.relay_kind.is_empty() {
                Some("relay 需要 --kind <work|discuss|decision>")
            } else if cmd.key.is_empty() {
                Some("relay 需要 --key <单子唯一标识>")
            } else {
                None
            }
        }
        "memo" | "learn" => {
            if cmd.text.is_empty() {
                Some("需要 --text \"<要沉淀的内容>\"")
            } else {
                None
            }
        }
        "lesson" => {
            if cmd.text.is_empty() {
                Some("lesson 需要 --text \"当…时，就…\"（一条具体可执行的守则）")
            } else {
                None
            }
        }
        "lesson-verify" => {
            if cmd.ts == 0 {
                Some("lesson-verify 需要 --ts <守则时间戳>（可先用 kb-search 查到 ts）")
            } else {
                None
            }
        }
        "forget" => {
            if cmd.ts == 0 {
                Some("forget 需要 --ts <记忆时间戳>（可先用 kb-search 查到 ts）")
            } else {
                None
            }
        }
        "done" => {
            if cmd.key.is_empty() {
                Some("需要 --key <单子唯一标识>")
            } else if cmd.summary.trim().is_empty() {
                Some("done 需要 --summary \"<一句话结果>\"")
            } else {
                None
            }
        }
        "blocked" => {
            if cmd.key.is_empty() {
                Some("需要 --key <单子唯一标识>")
            } else {
                None
            }
        }
        _ => None,
    };
    if let Some(e) = err {
        return e.into();
    }

    match append_command(&data_dir, &cmd) {
        Ok(()) => format!("OK：已提交 {kind}，应用将在下个心跳周期内执行。"),
        Err(e) => format!("提交失败：{e}"),
    }
}

/// 语义检索：设置启用且 embedding 服务可用时按向量相似度排序取 top-k；否则 None → 回退 BM25。
/// CLI 单次调用，直接现算候选向量（记忆条数有限，够用），不动主程序的向量缓存。
fn semantic_search<'a>(
    dir: &PathBuf,
    query: &str,
    docs: &[&'a JournalEntry],
    k: usize,
) -> Option<Vec<&'a JournalEntry>> {
    let s = Settings::load(dir);
    if !s.semantic_enabled || s.embed_endpoint.trim().is_empty() || s.embed_model.trim().is_empty()
    {
        return None;
    }
    // 候选上限：避免一次 embed 过多（超出则先按时间取最近的）
    let cap = 200usize;
    let mut cand: Vec<&JournalEntry> = docs.to_vec();
    cand.sort_by(|a, b| b.ts.cmp(&a.ts));
    cand.truncate(cap);
    let texts: Vec<String> = cand
        .iter()
        .map(|d| {
            format!(
                "passage: {}",
                one_line(&format!("{} {}", d.task_title, d.summary), 512)
            )
        })
        .collect();

    let client = reqwest::Client::new();
    let endpoint = s.embed_endpoint.clone();
    let model = s.embed_model.clone();
    let key = s.embed_api_key.clone();
    let n = cand.len();
    let query = query.to_string();
    let scores: Option<Vec<f32>> = tauri::async_runtime::block_on(async move {
        let qv = semantic::embed(
            &client,
            &endpoint,
            &model,
            &key,
            &[format!("query: {query}")],
        )
        .await
        .ok()?;
        let qv = qv.into_iter().next()?;
        if qv.is_empty() {
            return None;
        }
        let dv = semantic::embed(&client, &endpoint, &model, &key, &texts)
            .await
            .ok()?;
        if dv.len() != n {
            return None;
        }
        Some(dv.iter().map(|v| semantic::cosine(&qv, v)).collect())
    });
    let scores = scores?;
    let mut idx: Vec<usize> = (0..cand.len()).collect();
    idx.sort_by(|&a, &b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Some(idx.into_iter().take(k).map(|i| cand[i]).collect())
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
