//! big-dipper — Big Dipper（北斗七星）：独立模糊符号检索 CLI（薄壳，逻辑在 lib.rs / index.rs / search.rs / scanner.rs）
//!
//! 与 polaris（北极星，精确查询）成对：Big Dipper 模糊召回指路，polaris 精确展开。
//!
//! 用法：
//!   big-dipper "鉴权失败后的重试" [--limit 10] [--json] [--handoff] [--bench] [--root DIR]
//!   big-dipper "getIndx"     # 拼写纠错走 n-gram 通道
//!   big-dipper "AHR"         # 缩写走标识符通道
//!
//! 职责边界：只做模糊召回与排序，输出候选句柄 + 证据 + 置信度；
//! 高置信句柄交给 polaris 做精确解析（path + symbol + lines 即精确定位输入）。

use big_dipper::index;
use big_dipper::search::{big_dipper_search, DEFAULT_ALIAS};
use std::env;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let mut root = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut limit = 10usize;
    let mut json = false;
    let mut bench = false;
    let mut handoff = false;
    let mut query: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--root" => {
                i += 1;
                if i < args.len() {
                    root = PathBuf::from(&args[i]);
                }
            }
            "--limit" => {
                i += 1;
                if i < args.len() {
                    limit = args[i].parse().unwrap_or(10);
                }
            }
            "--json" => json = true,
            "--bench" => bench = true,
            "--handoff" => handoff = true,
            a if !a.starts_with("--") && query.is_none() => query = Some(a.to_string()),
            _ => {}
        }
        i += 1;
    }
    if query.is_none() && !bench {
        eprintln!("usage: big-dipper \"query\" [--limit N] [--json] [--handoff] [--bench] [--root DIR]");
        std::process::exit(2);
    }

    let prep = index::prepare_root(&root, true);

    if bench {
        let queries = [
            "getIndex",
            "getIndx",
            "scanSource",
            "鉴权重试",
            "context recall eval",
            "render message list",
            "STRIP",
        ];
        let _ = big_dipper_search(&prep, "warmup", 1, DEFAULT_ALIAS); // 预热
        for q in queries {
            let r = big_dipper_search(&prep, q, 5, DEFAULT_ALIAS);
            println!(
                "{:>8.1}ms  {:<6}  {:<24}  ->  {}",
                r.elapsed_ms,
                r.verdict,
                q,
                r.candidates.first().map(|c| c.symbol.as_str()).unwrap_or("(none)")
            );
        }
        return;
    }

    let res = big_dipper_search(&prep, query.as_deref().unwrap(), limit, DEFAULT_ALIAS);
    if handoff {
        print!("{}", index::handoff_json(&res, &root));
    } else if json {
        print!("{}", index::to_json(&res));
    } else {
        index::print_human(&res);
    }
}
