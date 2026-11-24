//! Antigravity ACP 纯 Rust 协议核心。
//!
//! 硬约束：本目录下所有模块禁止引用 `tauri` 或 `crate::AppState`，
//! 仅依赖标准库与 `serde` / `serde_json` / `tokio`，以便独立验证 crate 直接包含编译与测试。

pub mod connection;
pub mod mapper;
pub mod process;
pub mod protocol;
pub mod quota;
pub mod schema_budget;
pub mod session;
pub mod session_pool;
pub mod system_prompt;
pub mod watchdog;
