//! Google Antigravity CLI（`agy acp`）后端。
//!
//! 架构分层：
//! - `core/`：纯 Rust ACP 协议核心（零 `tauri` 依赖），可在无 GTK/WebKit 环境下独立编译与单测。
//! - `sidecar`：纯 Rust stdio MCP 桥与每会话回环 IPC（零 `tauri` 依赖）。
//! - `bridge`：主进程工具派发与 sidecar 早期入口胶水。
//! - `runtime`：薄 Tauri 胶水层，负责 `SessionPool`、`ThreadStore` 落盘与 `EV_*` 事件发射。

pub mod accounts;
pub mod bridge;
pub mod core;
#[cfg(windows)]
mod proxy;
pub mod runtime;
pub mod sidecar;
pub mod unlock;

pub use accounts::AntigravityAccountsSnapshot;
pub use core::quota::AntigravityQuotaSnapshot;
pub use runtime::AntigravityRuntime;
