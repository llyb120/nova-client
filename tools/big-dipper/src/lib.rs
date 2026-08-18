//! Big Dipper（北斗七星）：模糊符号检索库。
//!
//! 供两个宿主使用：
//!   - `src/main.rs`：独立 CLI 二进制
//!   - src-tauri 的 context-service：编译进常驻进程，通过 `big_dipper_handoff` 暴露成服务方法

pub mod gitignore;
pub mod index;
pub mod scanner;
pub mod search;

use std::path::Path;

/// 基于已构建好的 Prepared 跑一次模糊召回（供 context-service 缓存 Prepared 后复用）。
pub fn big_dipper_search_with(
    prepared: &search::Prepared,
    query: &str,
    limit: usize,
) -> search::SearchResult {
    search::big_dipper_search(prepared, query, limit, search::DEFAULT_ALIAS)
}

/// 构建检索预备（遍历 + 符号缓存 + BM25F 统计）。调用方应缓存结果复用。
pub fn prepare_root(root: &Path, use_disk: bool) -> search::Prepared {
    index::prepare_root(root, use_disk)
}

/// 模糊召回 + 交接载荷（--handoff JSON）。context-service 的 `big_dipper` 方法直接调用本函数。
pub fn big_dipper_handoff(root: &Path, query: &str, limit: usize) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!("workspace root is not a directory: {}", root.display()));
    }
    if query.trim().is_empty() {
        return Err("big_dipper 缺少 query".into());
    }
    let prepared = index::prepare_root(root, true);
    let res = search::big_dipper_search(&prepared, query, limit, search::DEFAULT_ALIAS);
    Ok(index::handoff_json(&res, root))
}

/// 完整检索（含证据/置信度），输出 --json 格式。
pub fn big_dipper_query(root: &Path, query: &str, limit: usize) -> Result<String, String> {
    if !root.is_dir() {
        return Err(format!("workspace root is not a directory: {}", root.display()));
    }
    if query.trim().is_empty() {
        return Err("big_dipper 缺少 query".into());
    }
    let prepared = index::prepare_root(root, true);
    let res = search::big_dipper_search(&prepared, query, limit, search::DEFAULT_ALIAS);
    Ok(index::to_json(&res))
}
