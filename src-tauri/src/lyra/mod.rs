//! Lyra — Rust 原生自研 agent，实现 pi 系 agent 的核心机制：
//! 基础工具（read/bash/edit/write + polaris 直连 nova_tools_native）、
//! Reasonix 精简上下文（slim memory 冻结摘要 + 压力分层 + 中途重写）、
//! 缓存优化（稳定/动态系统提示分段、prompt_cache_key、service_tier、长缓存保持）、
//! provider 兼容 OpenAI Completions 与 Responses。
//!
//! 沿用原配置（~/.nova/alkaid/config.jsonc）与会话数据目录；不走 Node bridge。
//! 主运行时与借用额度运行时都直接在应用进程内以 tokio 任务运行（spawn_prompt /
//! run_oneshot），事件与控制行走 mpsc 通道；借用额度只切换数据根（不同凭证），无需进程
//! 隔离。`nova lyra` 子命令保留作命令行调试入口，stdio JSONL 协议与 alkaid-bridge
//! 完全兼容，两种载体的事件流完全一致。

mod agent;
mod bridge;
mod config;
mod edit;
mod prompt;
mod provider;
mod read;
mod reasonix;
mod tools;
mod watchdog;

pub use bridge::{run_oneshot, spawn_prompt, InProcessSession};
pub use config::set_nova_root;

/// 若 argv 命中 `lyra` 子命令则执行 stdio bridge 协议并返回 true（调用方随后直接退出）。
/// 进程内运行时不会走这里；该入口保留作命令行调试。
pub fn maybe_run() -> bool {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("lyra") {
        return false;
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(4)
        .build()
        .expect("lyra runtime");
    let code = runtime.block_on(bridge::run());
    std::process::exit(code);
}
